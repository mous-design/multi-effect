use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use tracing::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::effects::{chorus, delay, eq, harmonizer, reverb,looper};
use crate::effects::eq::EqType;
use super::device::{Device, Frame, MetaTarget, ParamInfo, Parameterized, ParamValue};
use super::mix;

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
/// { "key": "09-mix",    "type": "mix",    "dry": 1.0, "wet": 0.8,
///   "overrides": { "wet.max": 0.5, "dry.visible": false } }
/// ```
///
/// `params` carries the live values (the flattened scalar fields), typed.
/// Replayed via `set_param` at construction. **Sparse by convention** — an
/// absent key means "still at the canonical default"; readers resolve absence
/// against `params_info[name].default_as_param_value()`.
///
/// `overrides` carries per-instance metadata edits (bound narrowing,
/// visibility toggles). Keys are `"param.aspect"` strings (see `MetaTarget`).
/// Replayed via `set_info_override` after construction. Absent / empty for
/// nodes that haven't been edited.
///
/// `params_info` is the resolved per-instance ParamInfo array (canonical →
/// Type overrides → Instance overrides applied). Computed by master at preset
/// load and refreshed when canonical / Type overrides / Instance overrides
/// change. Always serialised — it's derived data, but having it inline lets
/// the wire shape match the in-memory shape (no per-level borrow wrappers).
/// The few hundred extra bytes per preset are cheap; if eeprom storage ever
/// matters, a custom save handler can prune it then.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeDef {
    pub key: String,

    #[serde(rename = "type")]
    pub device_type: String,

    /// Per-instance metadata overrides (bounds, visibility, …). Empty by default.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub overrides: HashMap<MetaTarget, ParamValue>,

    /// Live param values — typed, sparse. Absent key = canonical default.
    #[serde(flatten)]
    pub params: HashMap<String, ParamValue>,

    /// Resolved metadata. Write-only — serialized to disk and wire for clients,
    /// but discarded on deserialize (`ParamInfo` is `&'static str` and can't be
    /// re-hydrated; master refreshes from canonical after every load). The
    /// `deserialize_with = "discard"` form consumes the value so the flattened
    /// `params` map doesn't try to absorb the key.
    #[serde(default, deserialize_with = "discard_params_info")]
    pub params_info: Vec<ParamInfo>,
}

fn discard_params_info<'de, D>(deserializer: D) -> Result<Vec<ParamInfo>, D::Error>
where D: serde::Deserializer<'de>,
{
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(Vec::new())
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

    /// Apply an Instance bound override to the node identified by `key`.
    /// Returns `Ok(true)` if a field changed, `Ok(false)` if the override
    /// was a no-op, `Err` if no node with that key exists.
    pub fn set_info_override(
        &mut self,
        key:       &str,
        target:    &crate::engine::device::MetaTarget,
        value:     &crate::engine::device::ParamValue,
        clamp_ref: &[crate::engine::device::ParamInfo],
    ) -> Result<bool, String> {
        for node in &mut self.nodes {
            if node.key() == key {
                return Ok(node.set_info_override(target, value, clamp_ref));
            }
        }
        Err(format!("no node with key '{key}'"))
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
    params: &HashMap<String, ParamValue>,
    node_key: &str,
) -> Result<()> {
    for (k, v) in params {
        if let Err(e) = target.set_param(k, *v) { warn!("node '{node_key}': {e}"); }
    }
    Ok(())
}

/// Resolve a node's full Instance view: canonical → Type overrides → Instance
/// overrides applied. Master stores this on each `NodeDef.params_info` at
/// preset load and refreshes it on override changes. Used for wire output
/// (UI rendering) and for master-side reads.
///
/// Returns `None` if the effect type is unknown. Distinct from the
/// Type-resolved view used by `build_node` for audio buffer sizing.
pub fn resolve_params_info(def: &NodeDef, cfg: &Config) -> Option<Vec<ParamInfo>> {
    use crate::engine::device::{build_info, OverrideMap, apply_override};
    let canonical = canonical_for(&def.device_type)?;
    let empty = OverrideMap::new();
    let t = cfg.type_overrides.get(&def.device_type).unwrap_or(&empty);
    let type_resolved = build_info(canonical, t);
    let mut resolved = type_resolved.clone();
    for (target, value) in &def.overrides {
        apply_override(&mut resolved, &type_resolved, target, value);
    }
    Some(resolved)
}

fn build_node(def: &NodeDef, cfg: &Config) -> Result<Box<dyn Device>> {
    use crate::engine::device::{build_info, OverrideMap};
    let sr = cfg.sample_rate as f32;
    // Type-resolved is the clamp_ref / construction base. Instance overrides
    // are replayed on top so they can shrink within Type; the audio thread's
    // buffers (chorus depth, delay time, looper init) size from Type's max
    // so a runtime Instance widen-back stays within the buffer.
    let empty = OverrideMap::new();
    let t = cfg.type_overrides.get(&def.device_type).unwrap_or(&empty);
    let canonical = canonical_for(&def.device_type)
        .ok_or_else(|| anyhow::anyhow!("unknown device type: '{}'", def.device_type))?;
    let type_resolved = build_info(canonical, t);

    let mut device: Box<dyn Device> = match def.device_type.as_str() {
        mix::NAME        => Box::new(mix::Mix::new(&def.key, &type_resolved)),
        looper::NAME     => Box::new(looper::Looper::new(&def.key, sr, &type_resolved)),
        delay::NAME      => Box::new(delay::Delay::new(&def.key, sr, &type_resolved)),
        reverb::NAME     => Box::new(reverb::Reverb::new(&def.key, sr, &type_resolved)),
        chorus::NAME     => Box::new(chorus::Chorus::new(&def.key, sr, &type_resolved)),
        harmonizer::NAME => Box::new(harmonizer::Harmonizer::new(&def.key, sr, &type_resolved)),
        eq::NAME_MID     => Box::new(eq::Eq::new(&def.key, EqType::Peak,      sr, &type_resolved)),
        eq::NAME_LOW     => Box::new(eq::Eq::new(&def.key, EqType::LowShelf,  sr, &type_resolved)),
        eq::NAME_HIGH    => Box::new(eq::Eq::new(&def.key, EqType::HighShelf, sr, &type_resolved)),
        other            => bail!("unknown device type: '{other}'"),
    };
    // Replay saved Instance overrides on top of the Type-resolved base; same
    // `apply_override` kernel master uses at runtime.
    for (target, value) in &def.overrides {
        device.set_info_override(target, value, &type_resolved);
    }
    apply_params(&mut device, &def.params, &def.key)?;
    debug!("  Node '{}' ({})", def.key, def.device_type);
    Ok(device)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Look up the canonical `ParamInfo` array for an effect type, by string name.
/// Used by master to compute Type-resolved bounds on the fly when the user
/// submits an Instance bound edit (3-segment `SET <key>.<param>.<aspect> <value>`).
pub fn canonical_for(effect_type: &str) -> Option<&'static [crate::engine::device::ParamInfo]> {
    match effect_type {
        mix::NAME        => Some(&mix::CANONICAL),
        looper::NAME     => Some(&looper::CANONICAL),
        delay::NAME      => Some(&delay::CANONICAL),
        reverb::NAME     => Some(&reverb::CANONICAL),
        chorus::NAME     => Some(&chorus::CANONICAL),
        harmonizer::NAME => Some(&harmonizer::CANONICAL),
        eq::NAME_MID     => Some(&eq::CANONICAL_MID),
        eq::NAME_LOW     => Some(&eq::CANONICAL_LOW),
        eq::NAME_HIGH    => Some(&eq::CANONICAL_HIGH),
        _                => None,
    }
}

fn validate_eq_order(nodes: &[NodeDef]) -> Result<()> {
    const EQ_TYPES: &[&str] = &["eq_mid", "eq_low", "eq_high"];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodedef_preserves_floats() {
        let json = r#"{"key":"01-delay","type":"delay","wet":0.65,"feedback":0.616,"time":0.626,"active":true}"#;
        let node: NodeDef = serde_json::from_str(json).unwrap();

        let serialized = serde_json::to_value(&node).unwrap();
        let node2: NodeDef = serde_json::from_value(serialized).unwrap();

        let wet = node2.params.get("wet").unwrap().try_float().unwrap();
        assert!((wet - 0.65).abs() < 0.01, "wet should be ~0.65, got {wet}");
    }
}
