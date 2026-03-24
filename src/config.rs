use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use crate::control::mapping::{ControllerDef, DeviceDef};
use crate::logging;

// ---------------------------------------------------------------------------
// BuildConfig
// ---------------------------------------------------------------------------

/// Build-time parameters derived from `Config`, passed to the patch factory.
#[derive(Clone, Copy)]
pub struct BuildConfig {
    pub sample_rate:         f32,
    pub in_channels:         usize,
    pub out_channels:        usize,
    pub delay_max_seconds:   f32,
    pub looper_max_seconds:  f32,
    /// Maximum number of overdub layers (0 = limited only by memory).
    pub looper_max_buffers:  usize,
}

impl From<&Config> for BuildConfig {
    fn from(cfg: &Config) -> Self {
        Self {
            sample_rate:        cfg.sample_rate as f32,
            in_channels:        cfg.in_channels  as usize,
            out_channels:       cfg.out_channels as usize,
            delay_max_seconds:  cfg.delay_max_seconds,
            looper_max_seconds: cfg.looper_max_seconds,
            looper_max_buffers: cfg.looper_max_buffers,
        }
    }
}

// ---------------------------------------------------------------------------
// PresetDef
// ---------------------------------------------------------------------------

/// One numbered preset slot: a signal chain definition + controller mappings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresetDef {
    /// Chain array in the same format as a top-level `"chains"` value.
    #[serde(default)]
    pub chains: Vec<Value>,

    /// Controller bindings active while this preset is loaded.
    /// Each entry references a device alias from `Config.devices` and supplies
    /// the key → target mappings for that device.
    #[serde(default)]
    pub controllers: Vec<ControllerDef>,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Sample rate in Hz (e.g. 48000)
    pub sample_rate: u32,

    /// Samples per processing block
    pub buffer_size: usize,

    /// Audio device name ("default" or CPAL device name)
    pub device: String,

    /// Number of physical input channels (e.g. 1 for mono mic, 2 for stereo)
    pub in_channels: u16,

    /// Number of physical output channels (e.g. 2 for stereo)
    pub out_channels: u16,

    /// Control device definitions.  Keys are user-defined aliases.
    /// Supported types: "serial", "net", "midi-in", "midi-out".
    #[serde(default)]
    pub devices: HashMap<String, DeviceDef>,

    /// Maximum delay time in seconds (determines delay buffer size at startup)
    #[serde(default = "Config::default_delay_max_seconds")]
    pub delay_max_seconds: f32,

    /// Maximum loop length in seconds (initial buffer size for buffer[0])
    #[serde(default = "Config::default_looper_max_seconds")]
    pub looper_max_seconds: f32,

    /// Maximum number of overdub layers (0 = limited only by memory, default 8)
    #[serde(default = "Config::default_looper_max_buffers")]
    pub looper_max_buffers: usize,

    /// Startup chains (used when no preset is active).  Empty = none.
    #[serde(default)]
    pub chains: Vec<Value>,

    /// Named preset slots.  Keys are preset numbers (0–127).
    #[serde(default)]
    pub presets: BTreeMap<u8, PresetDef>,

    /// Preset to load at startup when no state file exists.
    /// 0 = not set (use first preset, or fall back to top-level `chains`).
    #[serde(default)]
    pub active_preset: u8,

    /// Path where runtime state is saved/loaded.
    #[serde(default = "Config::default_state_save_path")]
    pub state_save_path: String,

    /// Log target: "stderr" (default) or "syslog"
    #[serde(default = "Config::default_log_target")]
    pub log_target: String,

    /// Seconds between automatic state saves (0 = disabled).
    #[serde(default = "Config::default_state_save_interval")]
    pub state_save_interval: u64,

    /// Enable debug logging for this crate (equivalent to -v on the CLI).
    #[serde(default)]
    pub verbose: bool,

    /// Port for the HTTP/WebSocket control API (0 = disabled).
    #[serde(default = "Config::default_http_port")]
    pub http_port: u16,

    /// Path of the loaded config file — set at runtime, not persisted.
    #[serde(skip)]
    pub config_path: String,

    /// When true, skip loading the state file and start fresh from config.
    /// Set by the `-f` / `--fresh` CLI flag.
    #[serde(skip)]
    pub skip_state: bool,
}

impl Config {
    fn default_delay_max_seconds()   -> f32    { 2.0  }
    fn default_looper_max_seconds()  -> f32    { 30.0 }
    fn default_looper_max_buffers()  -> usize  { 8    }
    fn default_log_target()          -> String { "stderr".into() }
    fn default_state_save_interval() -> u64    { 300 }
    fn default_state_save_path()     -> String { "/tmp/multi-effect-state.json".into() }
    fn default_http_port()           -> u16    { 8080 }

    pub fn load(path: &str) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config file '{path}'"))?;
        match Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("") {
            "json" => {
                let mut v: Value = serde_json::from_str(&text).context("config JSON parse error")?;
                coerce_whole_floats(&mut v);
                serde_json::from_value(v).context("config JSON parse error")
            }
            other => bail!("unsupported config format '.{other}' (use .json)"),
        }
    }

    // -----------------------------------------------------------------------
    // Startup helpers
    // -----------------------------------------------------------------------

    /// Chains JSON string to pass to `patch::load_str` at startup.
    /// Prefers active preset, then first preset, then top-level `chains`.
    pub fn startup_chains_json(&self) -> Result<Option<String>> {
        if let Some(preset) = self.active_preset_entry() {
            let arr = Value::Array(preset.chains.clone());
            return Ok(Some(format!(
                r#"{{"chains":{}}}"#,
                serde_json::to_string(&arr).context("chains serialize")?
            )));
        }
        self.chains_as_json()
    }

    /// Controller bindings to activate at startup (from active preset).
    pub fn startup_controllers(&self) -> Vec<ControllerDef> {
        self.active_preset_entry()
            .map(|p| p.controllers.clone())
            .unwrap_or_default()
    }

    pub fn active_preset_entry(&self) -> Option<&PresetDef> {
        if self.active_preset != 0 {
            self.presets.get(&self.active_preset)
        } else {
            None
        }
        .or_else(|| self.presets.values().next())
    }

    /// Serialize the top-level `chains` array to `{"chains":[...]}`.
    pub fn chains_as_json(&self) -> Result<Option<String>> {
        if self.chains.is_empty() { return Ok(None); }
        let s = serde_json::to_string(&self.chains).context("chains serialize")?;
        Ok(Some(format!(r#"{{"chains":{s}}}"#)))
    }

    pub fn effective_state_save_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.state_save_path)
    }

    // -----------------------------------------------------------------------
    // Preset management
    // -----------------------------------------------------------------------

    /// Update a preset slot with the given chains array and write to disk.
    /// `chains` must be a `Value::Array` of chain objects (as in PatchState.json["chains"]).
    pub fn save_preset(&mut self, slot: u8, chains: Value) -> Result<()> {
        let preset = self.presets.entry(slot).or_default();
        preset.chains = chains.as_array().cloned().unwrap_or_default();
        self.save_to_disk()
    }

    /// Atomically write the full config to `config_path`.
    pub fn save_to_disk(&self) -> Result<()> {
        if self.config_path.is_empty() { return Ok(()); }
        let path = Path::new(&self.config_path);
        let tmp  = path.with_extension("json.tmp");
        let mut v = serde_json::to_value(self).context("config serialize")?;
        crate::save::round_floats(&mut v);
        let text = serde_json::to_string_pretty(&v).context("config serialize")?;
        fs::write(&tmp, &text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Startup entry point
    // -----------------------------------------------------------------------

    /// Load config from the path given by `-c` (default `config.json`),
    /// apply any `--key value` overrides, then return Config + BuildConfig.
    pub fn from_args() -> Result<(Self, BuildConfig)> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let config_path = find_arg(&args, "-c").unwrap_or_else(|| "config.json".into());

        let mut cfg = Self::load(&config_path)
            .with_context(|| format!("failed to load config '{config_path}'"))?;

        cfg.config_path = config_path;

        apply_overrides(&mut cfg, &args)
            .context("invalid command-line argument")?;

        logging::init(&cfg.log_target, cfg.verbose)
            .context("failed to initialise logging")?;

        let build_cfg = BuildConfig::from(&cfg);
        Ok((cfg, build_cfg))
    }
}

/// Convert float JSON numbers that are whole numbers back to integers (e.g. 1.0 → 1).
/// Repairs config files that were incorrectly serialized with integer fields as floats.
fn coerce_whole_floats(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f >= 0.0 && f < u64::MAX as f64 {
                    *n = serde_json::Number::from(f as u64);
                }
            }
        }
        Value::Number(_) => {}
        Value::Array(arr) => arr.iter_mut().for_each(coerce_whole_floats),
        Value::Object(obj) => obj.values_mut().for_each(coerce_whole_floats),
        _ => {}
    }
}

fn find_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find_map(|w| if w[0] == flag { Some(w[1].clone()) } else { None })
}

fn apply_overrides(cfg: &mut Config, args: &[String]) -> Result<()> {
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        if flag == "-c" { i += 2; continue; }

        // Boolean flags (no value)
        if flag == "-v" { cfg.verbose    = true; i += 1; continue; }
        if flag == "-f" || flag == "--fresh" { cfg.skip_state = true; i += 1; continue; }

        let (key, val) = if let Some((k, v)) = flag.strip_prefix("--").and_then(|s| s.split_once('=').map(|(k,v)| (k.to_owned(), v.to_owned()))) {
            (k, v)
        } else if flag.starts_with("--") {
            let key = flag.trim_start_matches('-').to_owned();
            if i + 1 >= args.len() {
                anyhow::bail!("flag '{flag}' requires a value");
            }
            i += 1;
            (key, args[i].clone())
        } else {
            i += 1;
            continue;
        };

        match key.as_str() {
            "sample-rate"         => cfg.sample_rate         = val.parse().with_context(|| format!("--sample-rate: '{val}'"))?,
            "buffer-size"         => cfg.buffer_size         = val.parse().with_context(|| format!("--buffer-size: '{val}'"))?,
            "device"              => cfg.device              = val,
            "in-channels"         => cfg.in_channels         = val.parse().with_context(|| format!("--in-channels: '{val}'"))?,
            "out-channels"        => cfg.out_channels        = val.parse().with_context(|| format!("--out-channels: '{val}'"))?,
            "delay-max-seconds"   => cfg.delay_max_seconds   = val.parse().with_context(|| format!("--delay-max-seconds: '{val}'"))?,
            "looper-max-seconds"  => cfg.looper_max_seconds  = val.parse().with_context(|| format!("--looper-max-seconds: '{val}'"))?,
            "log-target"          => cfg.log_target          = val,
            "state-save-interval" => cfg.state_save_interval = val.parse().with_context(|| format!("--state-save-interval: '{val}'"))?,
            "state-save-path"     => cfg.state_save_path     = val,
            other => anyhow::bail!("unknown flag '--{other}'"),
        }
        i += 1;
    }
    Ok(())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_rate:          48_000,
            buffer_size:          256,
            device:               "default".into(),
            in_channels:          2,
            out_channels:         2,
            devices:              HashMap::new(),
            chains:               Vec::new(),
            presets:              BTreeMap::new(),
            active_preset:        0,
            state_save_path:      Self::default_state_save_path(),
            log_target:           Self::default_log_target(),
            state_save_interval:  Self::default_state_save_interval(),
            verbose:              false,
            delay_max_seconds:    Self::default_delay_max_seconds(),
            looper_max_seconds:   Self::default_looper_max_seconds(),
            looper_max_buffers:   Self::default_looper_max_buffers(),
            config_path:          String::new(),
            skip_state:           false,
            http_port:            Self::default_http_port(),
        }
    }
}
