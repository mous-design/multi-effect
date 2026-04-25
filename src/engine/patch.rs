use anyhow::{bail, Context, Result};
use tracing::{debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::effects::{Chorus, Delay, Eq, Harmonizer, Reverb};
use crate::effects::eq::EqType;
use crate::effects::looper::Looper;
use super::device::{check_bounds, Device, Frame, Parameterized, ParamValue};

// ---------------------------------------------------------------------------
// Custom deserializers
// ---------------------------------------------------------------------------

fn deserialize_channel_pair<'de, D>(deserializer: D) -> Result<[u8; 2], D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ChannelSpec { Single(u8), Pair([u8; 2]) }

    let spec = ChannelSpec::deserialize(deserializer)?;
    Ok(match spec {
        ChannelSpec::Single(n) => [n, n],
        ChannelSpec::Pair(p)   => p,
    })
}

// ---------------------------------------------------------------------------
// JSON definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ChainDef {
    #[serde(deserialize_with = "deserialize_channel_pair")]
    pub input: [u8; 2],

    #[serde(deserialize_with = "deserialize_channel_pair")]
    pub output: [u8; 2],

    pub nodes: Vec<NodeDef>,
}

/// One node in the chain as it appears in JSON.
///
/// ```json
/// { "key": "04-reverb", "type": "reverb", "room_size": 0.7, "wet": 0.3 }
/// { "key": "09-mix",    "type": "mix",    "dry": 1.0, "wet": [0.8, 0.6] }
/// ```
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeDef {
    pub key: String,

    #[serde(rename = "type")]
    pub device_type: String,

    #[serde(flatten)]
    pub params: serde_json::Map<String, Value>,
}

// ---------------------------------------------------------------------------
// Runtime types
// ---------------------------------------------------------------------------

/// Final output stage: scales the accumulated effect signal and compensates
/// for dry bleed in analogue-bypass setups.
///
/// Formula: `out[ch] = (eff[ch] - dry[ch]) * wet[ch] + dry[ch] * dry_param[ch]`
///
/// - `wet`: output level of the pure effect signal (eff minus dry).  Default: `1.0`.
/// - `dry`: output level of the original dry signal.  `1.0` = full dry (digital mode),
///   `0.0` = no dry output (analogue-bypass mode, hardware adds dry).  Default: `1.0`.
/// - `gain`: overall output level (post-pan).  Default: `1.0`.
/// - `pan`:  -1.0 = full left, 0.0 = centre, +1.0 = full right.  Default: `0.0`.
pub struct Mix {
    pub key: String,
    /// Per-channel gain applied to the dry signal
    pub dry: [f32; 2],
    /// Per-channel gain applied to the accumulated effect signal
    pub wet: [f32; 2],
    /// Overall output level (0.0 = silence, 1.0 = unity). Default: 1.0.
    pub gain: f32,
    /// Pan: -1.0 = full left, 0.0 = centre, +1.0 = full right. Default: 0.0.
    pub pan: f32,
    pub active: bool,
}

impl Mix {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), dry: [1.0; 2], wet: [1.0; 2], gain: 1.0, pan: 0.0, active: true }
    }
}

impl Parameterized for Mix {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active" => { self.active = value.try_bool()?; Ok(()) }
            "dry"  => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Mix", "dry", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Mix", "dry", r, 0.0, 1.0);
                self.dry = [vl, vr]; rl.and(rr)
            },
            "wet"  => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Mix", "wet", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Mix", "wet", r, 0.0, 1.0);
                self.wet = [vl, vr]; rl.and(rr)
            },
            "gain" => { let (v, r) = check_bounds("Mix", "gain", value.try_float()?, 0.0, 4.0); self.gain = v; r }
            "pan"  => { let (v, r) = check_bounds("Mix", "pan",  value.try_float()?, -1.0, 1.0); self.pan  = v; r }
            _ => Err(format!("Mix: unknown param '{param}'")),
        }
    }
}

impl Device for Mix {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { "mix" }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(), self.active.into());
        m.insert("dry".into(),  serde_json::json!(self.dry));
        m.insert("wet".into(),  serde_json::json!(self.wet));
        m.insert("gain".into(), serde_json::json!(self.gain));
        m.insert("pan".into(),  serde_json::json!(self.pan));
        m
    }

    fn process(&mut self, dry: &[Frame], eff: &mut [Frame]) {
        // Pan: the louder side stays at 1.0, the quieter side fades to 0.
        let pan_l = (1.0 - self.pan).min(1.0);
        let pan_r = (1.0 + self.pan).min(1.0);
        for (e, &d) in eff.iter_mut().zip(dry.iter()) {
            e[0] = ((e[0] - d[0]) * self.wet[0] + d[0] * self.dry[0]) * self.gain * pan_l;
            e[1] = ((e[1] - d[1]) * self.wet[1] + d[1] * self.dry[1]) * self.gain * pan_r;
        }
    }
    fn reset(&mut self) {}
}

// ---------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------

/// A named processing chain with physical I/O routing and pre-allocated RT buffers.
///
/// Signal model per block:
/// ```text
/// dry_buf  ← physical input channels
/// eff_buf  ← 0.0 initially
///
/// for each node:
///     node.process(dry_buf, eff_buf)   // node reads dry+eff, writes new eff
///
/// output_channels += eff_buf
/// ```
pub struct Chain {
    pub input: [u8; 2],
    pub output: [u8; 2],
    pub nodes: Vec<Box<dyn Device>>,
    dry_buf: Vec<Frame>,
    eff_buf: Vec<Frame>,
}

impl Chain {
    pub fn new(
        input: [u8; 2],
        output: [u8; 2],
        nodes: Vec<Box<dyn Device>>,
    ) -> Self {
        Self {
            input, output, nodes,
            dry_buf: Vec::new(),
            eff_buf: Vec::new(),
        }
    }

    pub fn prepare(&mut self, block_size: usize) {
        self.dry_buf.resize(block_size, [0.0; 2]);
        self.eff_buf.resize(block_size, [0.0; 2]);
    }

    pub fn process(
        &mut self,
        block_size: usize,
        in_channels: u16,
        out_channels: u16,
        input: &[f32],
        output: &mut [f32],
    ) {
        let in_chan = in_channels as usize;
        let out_chan = out_channels as usize;
        if block_size > self.dry_buf.len() {
            self.prepare(block_size);
        }

        // 0 = none (silent input / no output); channels are otherwise 1-based.
        let read_ch = |ch: u8, frame: usize| -> f32 {
            if ch > 0 { input[frame * in_chan + (ch as usize - 1)] } else { 0.0 }
        };

        for f in 0..block_size {
            self.dry_buf[f] = [read_ch(self.input[0], f), read_ch(self.input[1], f)];
            self.eff_buf[f] = self.dry_buf[f];
        }

        // Destructure for disjoint field borrows.
        let (nodes, dry_buf, eff_buf) =
            (&mut self.nodes, &self.dry_buf, &mut self.eff_buf);

        for node in nodes.iter_mut() {
            if node.is_active() {
                node.process(&dry_buf[..block_size], &mut eff_buf[..block_size]);
            }
        }

        for f in 0..block_size {
            if self.output[0] > 0 { output[f * out_chan + (self.output[0] - 1) as usize] += self.eff_buf[f][0]; }
            if self.output[1] > 0 { output[f * out_chan + (self.output[1] - 1) as usize] += self.eff_buf[f][1]; }
        }
    }

    pub fn reset(&mut self) {
        for node in &mut self.nodes {
            node.reset();
        }
    }

    pub fn init_bus(&mut self, bus: &crate::control::EventBus) {
        for node in &mut self.nodes {
            node.init_bus(bus);
        }
    }

    pub fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        // Key-prefix routing: "04-reverb.wet" → node "04-reverb", param "wet"
        if let Some((key, rest)) = param.split_once('.') {
            for node in &mut self.nodes {
                if node.key() == key {
                    if let Err(e) = node.set_param(rest, value) {
                        warn!("{e}")
                    }
                    return Ok(());
                }
            }
        }
        Err(format!("no node handles '{param}'"))
    }

    pub fn dispatch_action(&mut self, path: &str, action: &str) -> Result<(), String> {
        // Key-prefix routing: "01-looper.action" → node "01-looper", param "action"
        if let Some((key, param)) = path.split_once('.') {
            for node in &mut self.nodes {
                if node.key() == key {
                    match node.set_action(param, action) {
                        Ok(())  => { debug!("ACTION {path} {action}"); return Ok(()); }
                        Err(e)  => { warn!("{e}"); return Ok(()); }
                    }
                }
            }
        }
        // @todo I think this is not needed. Remove after some tests.
        // Fallback: try every node
        // for node in &mut self.nodes {
        //     match node.set_action(path, action) {
        //         Ok(()) => { debug!("ACTION {path} {action}"); return Ok(()); }
        //         Err(e) if !e.contains("unknown action") => { warn!("{e}"); return Ok(()); }
        //         Err(_) => {}
        //     }
        // }
        Err(format!("no node handles action '{path}'"))
    }

    #[allow(dead_code)]
    pub fn on_cc(&mut self, controller: u8, value: u8) {
        for node in &mut self.nodes {
            node.on_cc(controller, value);
        }
    }

    pub fn on_note_on(&mut self, note: u8, velocity: u8) {
        for node in &mut self.nodes {
            node.on_note_on(note, velocity);
        }
    }

    pub fn on_note_off(&mut self, note: u8) {
        for node in &mut self.nodes {
            node.on_note_off(note);
        }
    }

    /// Serialise this chain to a JSON value matching the patch file format.
    #[allow(dead_code)]
    pub fn to_json(&self) -> serde_json::Value {
        let nodes: Vec<serde_json::Value> = self.nodes.iter().map(|d| {
            let mut obj = d.to_params();
            obj.insert("key".into(),  d.key().into());
            obj.insert("type".into(), d.type_name().into());
            serde_json::Value::Object(obj)
        }).collect();
        serde_json::json!({
            "input":  self.input,
            "output": self.output,
            "nodes":  nodes,
        })
    }
}

// /// Serialise a slice of chains to the top-level patch JSON format.
// pub fn chains_to_json(chains: &[Chain]) -> serde_json::Value {
//     serde_json::json!({ "chains": chains.iter().map(Chain::to_json).collect::<Vec<_>>() })
// }

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub fn build_chain(idx: usize, def: &ChainDef, cfg: &Config) -> Result<Chain> {
    for &ch in &def.input {
        if ch > 0 && ch as usize > cfg.in_channels as usize {
            bail!("Chain {idx}: input channel {} out of range (in_channels={})", ch, cfg.in_channels);
        }
    }
    for &ch in &def.output {
        if ch > 0 && ch as usize > cfg.out_channels as usize {
            bail!("Chain {idx}: output channel {} out of range (out_channels={})", ch, cfg.out_channels);
        }
    }

    validate_eq_order(&def.nodes)
        .with_context(|| format!("Chain {idx}"))?;

    let nodes: Result<Vec<Box<dyn Device>>> =
        def.nodes.iter().map(|n| build_node(n, cfg)).collect();
    let chain = Chain::new(def.input, def.output, nodes?);
    debug!(
        "Chain {idx}: input=[{},{}] output=[{},{}], {} node(s)",
        chain.input[0], chain.input[1],
        chain.output[0], chain.output[1],
        chain.nodes.len()
    );
    Ok(chain)
}

fn apply_params<P: Parameterized>(
    target: &mut P,
    params: &serde_json::Map<String, serde_json::Value>,
    node_key: &str,
) -> Result<()> {
    for (k, v) in params {
        if matches!(k.as_str(), "key" | "type") { continue; }
        // Skip values that cannot be mapped to a ParamValue (e.g. state strings like "Idle").
        // These are read-only runtime fields that live_state may include but are not settable.
        let pv = match parse_param_value(v) {
            Ok(pv) => pv,
            Err(_)  => { continue; }
        };
        if let Err(e) = target.set_param(k, pv) { warn!("node '{node_key}': {e}"); }
    }
    Ok(())
}

fn build_node(def: &NodeDef, cfg: &Config) -> Result<Box<dyn Device>> {
    let sr = cfg.sample_rate as f32;
    let mut device: Box<dyn Device> = match def.device_type.as_str() {
        "mix"        => Box::new(Mix::new(&def.key)),
        "looper"     => Box::new(Looper::new(&def.key, sr, cfg.looper_max_seconds, cfg.looper_max_buffers)),
        "delay"      => Box::new(Delay::new(&def.key, sr, cfg.delay_max_seconds)),
        "reverb"     => Box::new(Reverb::new(&def.key, sr)),
        "chorus"     => Box::new(Chorus::new(&def.key, sr)),
        "harmonizer" => Box::new(Harmonizer::new(&def.key, sr)),
        "eq_param" => Box::new(Eq::new(&def.key, EqType::Peak,      sr)),
        "eq_low"   => Box::new(Eq::new(&def.key, EqType::LowShelf,  sr)),
        "eq_high"  => Box::new(Eq::new(&def.key, EqType::HighShelf, sr)),
        other      => bail!("unknown device type: '{other}'"),
    };
    apply_params(&mut device, &def.params, &def.key)?;
    debug!("  Node '{}' ({})", def.key, def.device_type);
    Ok(device)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn validate_eq_order(nodes: &[NodeDef]) -> Result<()> {
    const EQ_TYPES: &[&str] = &["eq_param", "eq_low", "eq_high"];

    for (mix_pos, mix_node) in nodes.iter().enumerate()
        .filter(|(_, n)| n.device_type == "mix")
    {
        if let Some((_, eq_node)) = nodes.iter().enumerate()
            .find(|(pos, n)| *pos < mix_pos && EQ_TYPES.contains(&n.device_type.as_str()))
        {
            bail!(
                "EQ must be placed after Mix (analogue bypass phase issue). \
                 Move '{}' after '{}'.",
                eq_node.key, mix_node.key
            );
        }
    }
    Ok(())
}

pub fn load_patch_def(defs: &Vec<ChainDef>, cfg: &Config) -> Result<Vec<Chain>> {
    defs.iter().enumerate().map(|(i, c)| build_chain(i, c, cfg)).collect()
}

// pub fn load_str(json: &str, cfg: &Config) -> Result<Vec<Chain>> {
//     let def: Vec<ChainDef> = serde_json::from_str(json).context("load_str: patch JSON parse error")?;
//     load_patch_def(&def, cfg)
// }

// pub fn load_value(json: Value, cfg: &Config) -> Result<Vec<Chain>> {
//     let def: Vec<ChainDef> = serde_json::from_value(json).context("load_value: deserializing ChainDefs")?;
//     load_patch_def(&def, cfg)
// }

// pub fn load_from_config(cfg: &Config, with_state: bool) -> Result<Vec<Chain>> {
//     // --- Determine startup chains: config structure + saved params overlay ---
//     let state_path = cfg.state_save_path;
//     let chains = cfg.startup_chains_def();
    
//     let merged = if with_state && state_path.exists() {
//         match std::fs::read_to_string(&state_path) {
//             Err(e) => {
//                 warn!("Could not read state file ({}): {e}", state_path.display());
//                 chains
//             }
//             Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
//                 Err(e) => {
//                     warn!("State file contains invalid JSON, ignoring: {e}");
//                     chains
//                 }
//                 Ok(saved) => {
//                     info!("Loading saved state (params overlay): {}", state_path.display());
//                     // merge_state_params(chains, &saved)
//                     // @todo make some logic here...
//                     chains
//                 }
//             }
//         }
//     } else {
//         info!("No saved state, loading chains from config");
//         chains
//     };
//     load_patch_def(merged, cfg)
// }

/// Use `structure` (from config/preset) as the authoritative chain/node layout,
/// but overlay saved param values from `saved` for any node key that still exists.
/// This ensures structural changes in config are always respected while preserving knob positions.
// pub fn merge_state_params(chains: Vec<ChainDef>, saved: &Value) -> Vec<ChainDef> {
//     // Collect all saved nodes by key into a flat map.
//     let mut saved_nodes: HashMap<&str, &Value> = HashMap::new();
//     if let Some(chains) = saved["chains"].as_array() {
//         for chain in chains {
//             if let Some(nodes) = chain["nodes"].as_array() {
//                 for node in nodes {
//                     if let Some(key) = node["key"].as_str() {
//                         saved_nodes.insert(key, node);
//                     }
//                 }
//             }
//         }
//     }

//     let mut result = chains;
//     if let Some(chains) = result["chains"].as_array_mut() {
//         for chain in chains {
//             if let Some(nodes) = chain["nodes"].as_array_mut() {
//                 for node in nodes {
//                     let key = match node["key"].as_str() {
//                         Some(k) => k.to_string(),
//                         None    => continue,
//                     };
//                     if let Some(saved_node) = saved_nodes.get(key.as_str()) {
//                         if let (Some(n), Some(s)) = (node.as_object_mut(), saved_node.as_object()) {
//                             for (k, v) in s {
//                                 if k != "key" && k != "type" {
//                                     n.insert(k.clone(), v.clone());
//                                 }
//                             }
//                         }
//                     }
//                 }
//             }
//         }
//     }
//     result
// }

/// Parse a JSON value into a `ParamValue`.
///
/// - `number` or `bool`  → `ParamValue::Float`
/// - `[number, number]`  → `ParamValue::Stereo`
/// - anything else       → `Err`
fn parse_param_value(v: &Value) -> Result<ParamValue> {
    match v {
        Value::Number(n) => Ok(ParamValue::Float(
            n.as_f64().ok_or_else(|| anyhow::anyhow!("invalid number: {v}"))? as f32,
        )),
        Value::Bool(b) => Ok(ParamValue::Bool(*b)),
        Value::Array(arr) if arr.len() == 2 => Ok(ParamValue::Stereo([
            arr[0].as_f64().ok_or_else(|| anyhow::anyhow!("array element 0 is not a number"))? as f32,
            arr[1].as_f64().ok_or_else(|| anyhow::anyhow!("array element 1 is not a number"))? as f32,
        ])),
        Value::Array(arr) => bail!("expected [number, number], got array with {} elements", arr.len()),
        _ => bail!("expected number or [number, number], got: {v}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodedef_preserves_floats() {
        let json = r#"{"key":"01-delay","type":"delay","wet":0.65,"feedback":0.616,"time":0.626,"active":true}"#;
        let node: NodeDef = serde_json::from_str(json).unwrap();
        eprintln!("Direct parse: {:?}", node.params);

        let serialized = serde_json::to_value(&node).unwrap();
        eprintln!("Serialized:   {serialized}");

        let node2: NodeDef = serde_json::from_value(serialized).unwrap();
        eprintln!("Round-trip:   {:?}", node2.params);

        let wet = node2.params.get("wet").unwrap().as_f64().unwrap();
        assert!((wet - 0.65).abs() < 0.01, "wet should be ~0.65, got {wet}");
    }

    #[test]
    fn full_config_preserves_floats() {
        use crate::config::Config;

        // Method 1: load → from_value (current path)
        let cfg1 = Config::load(std::path::PathBuf::from("config.json")).unwrap();
        let p1 = cfg1.presets.get(1).unwrap();
        let delay = p1.chains[0].nodes.iter().find(|n| n.key == "01-delay").unwrap();
        let wet_from_value = delay.params.get("wet").unwrap().as_f64().unwrap();
        eprintln!("from_value: wet = {wet_from_value}");

        // Method 2: from_str directly
        let json_str = std::fs::read_to_string("config.json").unwrap();
        let cfg2: Config = serde_json::from_str(&json_str).unwrap();
        let p2 = cfg2.presets.get(1).unwrap();
        let delay2 = p2.chains[0].nodes.iter().find(|n| n.key == "01-delay").unwrap();
        let wet_from_str = delay2.params.get("wet").unwrap().as_f64().unwrap();
        eprintln!("from_str:   wet = {wet_from_str}");

        assert!((wet_from_str - 0.65).abs() < 0.01, "from_str: wet should be ~0.65, got {wet_from_str}");
        assert!((wet_from_value - 0.65).abs() < 0.01, "from_value: wet should be ~0.65, got {wet_from_value}");
    }
}
