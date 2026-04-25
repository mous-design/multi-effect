pub mod master;
pub mod persist_fs;
pub mod preset;
pub mod snapshot;

use anyhow::{bail, anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use persist_fs::{persist,load};
use crate::{control::mapping::DeviceDef, engine::patch::ChainDef};
use preset::PresetDefs;

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
    delay_max_seconds:  Option<f32>,
    looper_max_seconds: Option<f32>,
}
impl ConfigPatch {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            sample_rate:        Some(cfg.sample_rate),
            buffer_size:        Some(cfg.buffer_size),
            audio_device:       Some(cfg.audio_device.clone()),
            in_channels:        Some(cfg.in_channels),
            out_channels:       Some(cfg.out_channels),
            delay_max_seconds:  Some(cfg.delay_max_seconds),
            looper_max_seconds: Some(cfg.looper_max_seconds),
        }
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

    /// Maximum delay time in seconds (determines delay buffer size at startup)
    #[serde(default = "Config::default_delay_max_seconds")]
    pub delay_max_seconds: f32,

    /// Maximum loop length in seconds (initial buffer size for buffer[0])
    #[serde(default = "Config::default_looper_max_seconds")]
    pub looper_max_seconds: f32,

    /// Maximum number of overdub layers (0 = limited only by memory, default 8)
    #[serde(default = "Config::default_looper_max_buffers")]
    pub looper_max_buffers: usize,

    /// Port for the HTTP/WebSocket control API (0 = disabled).
    #[serde(default = "Config::default_http_port")]
    pub http_port: u16,

    /// Startup chains (used when no preset is active).  Empty = none.
    #[serde(default)]
    pub chains: Vec<ChainDef>,

    /// Preset slots and active-preset pointer.
    #[serde(default)]
    pub presets: PresetDefs,

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
    fn default_delay_max_seconds()   -> f32    { 2.0  }
    fn default_looper_max_seconds()  -> f32    { 30.0 }
    fn default_looper_max_buffers()  -> usize  { 8    }
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
        persist(&serde_json::to_value(self)?, &self.config_path)
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
            state_save_path:      Self::default_state_save_path(),
            log_target:           Self::default_log_target(),
            state_save_interval:  Self::default_state_save_interval(),
            delay_max_seconds:    Self::default_delay_max_seconds(),
            looper_max_seconds:   Self::default_looper_max_seconds(),
            looper_max_buffers:   Self::default_looper_max_buffers(),
            http_port:            Self::default_http_port(),
            config_path:          PathBuf::new(),
        }
    }
}
