pub mod master;
pub mod persist_fs;
pub mod preset;
pub mod snapshot;

use anyhow::{bail, anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use persist_fs::{persist, load, strip_derived};
use super::control::mapping::DeviceDef;
use super::engine::patch::ChainDef;
use preset::PresetDefs;

// ---------------------------------------------------------------------------
// Boundary traits
//
// Two boundaries, two filters, one shape each. Implementations decide what's
// relevant for *their* boundary on a per-type basis. Callers don't reason
// about what's filtered — they just serialize the returned `Value`.
// ---------------------------------------------------------------------------

/// "This type can be written to disk." Returns the filtered JSON Value that
/// goes through `persist()`. Implementations strip derived data the type
/// can recompute.
pub trait ToPersistable {
    fn to_persistable(&self) -> Result<serde_json::Value>;
}

/// "This type can be sent over the wire." Returns the filtered JSON Value a
/// client receives. Implementations strip internal state clients don't need.
pub trait ToWire {
    fn to_wire(&self) -> Result<serde_json::Value>;
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------
/// Lightweight config object: only carries real scalar values, meant to send 
/// over transport channels. All properties are Options, so the transport can use 
/// this struct for sending just the properties it wants.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    sample_rate:        Option<u32>,
    buffer_size:        Option<u32>,
    audio_device:       Option<String>,
    in_channels:        Option<u16>,
    out_channels:       Option<u16>,
}
impl ConfigPatch {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            sample_rate:        Some(cfg.sample_rate),
            buffer_size:        Some(cfg.buffer_size),
            audio_device:       Some(cfg.audio_device.clone()),
            in_channels:        Some(cfg.in_channels),
            out_channels:       Some(cfg.out_channels),
        }
    }
}

impl ToWire for ConfigPatch {
    /// Wire shape: full patch. Every field is wire-meaningful; nothing to
    /// filter today. Trait impl exists so a future filter (e.g. hide
    /// `audio_device` from read-only clients) is a one-impl change.
    fn to_wire(&self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// Complete config object. Holds all ConfigPatch plus presets, default chains
/// and some other values that are internal only
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Sample rate in Hz (e.g. 48000)
    pub sample_rate: u32,

    /// Samples per processing block
    pub buffer_size: u32,

    /// Audio device name ("default" or CPAL device name)
    pub audio_device: String,

    /// Number of physical input channels (e.g. 1 for mono mic, 2 for stereo)
    pub in_channels: u16,

    /// Number of physical output channels (e.g. 2 for stereo)
    pub out_channels: u16,

    /// Control device definitions.  Keys are user-defined aliases.
    /// Supported types: "serial", "net", "midi-in", "midi-out".
    #[serde(default)]
    pub control_devices: HashMap<String, DeviceDef>,

    /// Port for the HTTP/WebSocket control API (0 = disabled).
    #[serde(default = "Config::default_http_port")]
    pub http_port: u16,

    /// Startup chains (used when no preset is active).  Empty = none.
    #[serde(default)]
    pub chains: Vec<ChainDef>,

    /// Preset slots and active-preset pointer.
    #[serde(default)]
    pub presets: PresetDefs,

    /// Per-effect-type bound overrides, keyed by effect type
    /// (`"chorus"`, `"delay"`, ...). Each value is a flat
    /// `{"param.aspect": value}` map; see `engine::device::MetaTarget`.
    /// Empty / absent = no Type overrides; effects use canonical defaults.
    #[serde(default)]
    pub type_overrides: HashMap<String, crate::engine::device::OverrideMap>,

    /// Log target: "stderr" (default) or "syslog"
    #[serde(default = "Config::default_log_target")]
    pub log_target: String,

    /// Path where runtime state is saved/loaded.
    #[serde(default = "Config::default_state_save_path")]
    pub state_save_path: PathBuf,

    /// Seconds between automatic state saves (0 = disabled).
    #[serde(default = "Config::default_state_save_interval")]
    pub state_save_interval: u64,

    #[serde(skip)]
    /// Path of the loaded config file — set at runtime, not persisted.
    pub config_path: PathBuf,
}

impl Config {
    fn default_log_target()          -> String { "stderr".into() }
    fn default_state_save_interval() -> u64    { 300 }
    fn default_state_save_path()     -> PathBuf { PathBuf::from("/tmp/multi-effect-state.json") }
    fn default_http_port()           -> u16    { 8080 }

    pub fn from_args() -> Result<(Self, bool, bool)> {
        let mut config_path_str = "config.json".into();
        let mut verbose = false;
        let mut skip_state = false;
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-c" | "--config" => {  
                    config_path_str = it.next().ok_or_else(|| anyhow!("missing value after -c"))?;
                },
                "-v" | "--verbose" => { 
                    verbose = true;
                },
                "-f" | "--fresh"  => {
                    skip_state = true;
                },
                _ => bail!("Unknown option {arg}"),
            }
        }
        let cfg = Config::load(PathBuf::from(config_path_str))?;
        Ok((cfg, verbose, skip_state))
    }

    /// Atomically write the full config to `config_path`.
    pub fn persist(&self) -> Result<()> {
        persist(&self.to_persistable()?, &self.config_path)
    }

    pub fn load(config_path: PathBuf) -> Result<Self> {
        let v = load(&config_path)?;
        let mut cfg: Config = serde_json::from_value(v)?;
        cfg.config_path = config_path;
        Ok(cfg)
    }

    // -----------------------------------------------------------------------
    // Startup helpers
    // -----------------------------------------------------------------------

    // /// Get a clone of the chain of the current preset, fallback to top-level chains from 
    // /// config, which might be empty.
    // pub fn startup_chains_def(&self) -> Vec<ChainDef> {
    //     self.presets.active_entry()
    //         .map(|p| p.chains.clone())
    //         .unwrap_or_else(|| self.chains.clone())
    // }

    // /// Get a clone of controller bindings of the current preset, or empty.
    // pub fn startup_controller_defs(&self) -> Vec<ControllerDef> {
    //     self.presets.active_entry()
    //         .map(|p| p.controllers.clone())
    //         .unwrap_or_default()
    // }
}

impl ToPersistable for Config {
    fn to_persistable(&self) -> Result<serde_json::Value> {
        let mut v = serde_json::to_value(self)?;
        strip_derived(&mut v);
        Ok(v)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_rate:          48_000,
            buffer_size:          256,
            audio_device:         "default".into(),
            in_channels:          2,
            out_channels:         2,
            control_devices:      HashMap::new(),
            chains:               Vec::new(),
            presets:              PresetDefs::default(),
            type_overrides:       HashMap::new(),
            state_save_path:      Self::default_state_save_path(),
            log_target:           Self::default_log_target(),
            state_save_interval:  Self::default_state_save_interval(),
            http_port:            Self::default_http_port(),
            config_path:          PathBuf::new(),
        }
    }
}
