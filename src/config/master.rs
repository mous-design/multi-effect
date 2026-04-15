use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn, debug};
use anyhow::{Result, Context, bail};

use super::Config;
use super::preset::PresetDef;
use super::snapshot::{ConfigSnapshot, SnapshotState::*};
use super::ChainDef;
use super::preset::PRESET_NONE;
use crate::control;
use crate::control::mapping::{ControllerDef, DeviceDef};
use crate::control::{EventBus, NetworkControl, SerialControl, ControlMessage};
use crate::control::midi::{MidiControl, MidiOutControl};
use crate::engine::AudioHandle;
use crate::engine::patch::{self, Chain};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub type Resp<T> = oneshot::Sender<T>;
pub type OptionResp = Option<oneshot::Sender<Result<()>>>;
/// Inbound requests to the config master.
pub enum ConfigRequest {
    // -- Reads (need response) --
    GetConfig       { resp: Resp<Result<Value>> },
    GetSnapshot { resp: Resp<Result<ConfigSnapshot>> },
    GetDevices      { resp: Resp<Result<Value>> },
    // -- Config mutations (need response for HTTP) --
    UpdateConfig      { body: Value, resp: OptionResp },
    SwitchPreset      { slot: u8, resp: OptionResp },
    SavePreset        { slot: u8, resp: OptionResp },
    DeletePreset      { slot: u8, resp: OptionResp },
    PutDevice         { alias: String, def: DeviceDef, resp: OptionResp },
    DeleteDevice      { alias: String, resp: OptionResp },
    RenameDevice      { old_alias: String, new_alias: String, def: DeviceDef, resp: OptionResp },
    UpdateControllers { controllers: Vec<ControllerDef>, resp: OptionResp },
    ApplySet          { path: String, value: f32, source: String, resp: OptionResp },

    // -- Chain structure update --
    SetChains         { json: String, resp: OptionResp },

    // -- Fire-and-forget --
    ApplyControl(ControlMessage),
    ToggleCompare { resp: OptionResp },

    // -- Internal --
    Reload { resp: OptionResp },
    SaveState,
}

// ---------------------------------------------------------------------------
// ConfigMaster
// ---------------------------------------------------------------------------

pub struct ConfigMaster {
    /// The full persisted config (control devices, presets, audio settings). Owned exclusively by the master — single writer.
    cfg:              Config,
    /// Live state of the currently loaded preset (the JSON that reflects what the audio engine is actually running). Updated on patch/set/switch.
    snapshot:     ConfigSnapshot,
    /// Handle to push data into the audio engine (lock-free ring buffers).
    audio: AudioHandle,
    /// Per-device-alias mapping definitions. Shared with device tasks.
    controller_map:   HashMap<String, Arc<RwLock<ControllerDef>>>,
    /// Per-device kill switch. Send false → device task shuts down gracefully.
    device_active:    HashMap<String, watch::Sender<bool>>,
    /// Broadcast channel (tokio::broadcast) for notifying all listeners (MIDI out, serial, network, WS) of control messages.
    bus:              EventBus,
    /// Self-handle — so device-spawn helpers can give new tasks a way to send requests back to the master (e.g. serial sending ApplySet).
    master_tx:        mpsc::Sender<ConfigRequest>,
}

/// Create the master, spawn it on the tokio runtime, return the channels.
pub fn spawn(cfg: Config, audio: AudioHandle) -> Result<(mpsc::Sender<ConfigRequest>, control::EventBus)> {
    let snapshot = ConfigSnapshot::restore_or_build(&cfg)?;

    // --- Event bus ---
    let bus = control::new_event_bus();

    let (master_tx, req_rx) = mpsc::channel(256);

    let controller_map: HashMap<String, Arc<RwLock<ControllerDef>>> = cfg.control_devices.keys()
        .map(|alias| (alias.clone(), Arc::new(RwLock::new(ControllerDef::default()))))
        .collect();
    let device_active = HashMap::new();
    let ret_tx = master_tx.clone();
    let ret_bus = bus.clone();

    let master = ConfigMaster {
        cfg,
        snapshot,
        audio,
        controller_map,
        device_active,
        bus,
        master_tx,
    };

    tokio::spawn(master.run(req_rx));
    Ok((ret_tx, ret_bus))
}

impl ConfigMaster {
    // -----------------------------------------------------------------------
    // Main loop
    // -----------------------------------------------------------------------

    async fn run(mut self, mut rx: mpsc::Receiver<ConfigRequest>) {
        self.spawn_initial_devices();

        // Push initial preset to audio engine.
        match self.build_chains(&self.snapshot.preset.chains.clone()) {
            Ok(chains) => {
                if let Err(e) = self.audio.push_patch(chains) {
                    warn!("Initial patch push failed: {e}");
                }
                self.apply_controllers(&self.snapshot.preset.controllers.clone());
                info!("Loaded initial preset {}", self.snapshot.preset.index);
            }
            Err(e) => warn!("Initial chain build failed: {e}"),
        }

        while let Some(req) = rx.recv().await {
            self.handle(req);
        }

        // Shutdown: final save.
        self.handle_save_state();
        info!("ConfigMaster shutting down.");
    }
    fn respond(resp: OptionResp, value: Result<()>) {
        if let Some(resp) = resp { let _ = resp.send(value); }
    }

    fn handle(&mut self, req: ConfigRequest) {
        match req {
            // Reads
            ConfigRequest::GetConfig { resp } => {
                let _ = resp.send(self.read_config());
            }
            ConfigRequest::GetSnapshot { resp } => {
                let _ = resp.send(Ok(self.snapshot.clone()));
            }
            ConfigRequest::GetDevices { resp } => {
                let snd = serde_json::to_value(&self.cfg.control_devices)
                    .unwrap_or(Value::Array(vec![]));
                let _ = resp.send(Ok(snd));
            }
            // Mutations
            ConfigRequest::UpdateConfig { body, resp } => {
                Self::respond(resp, self.handle_update_config(&body));
            }
            ConfigRequest::SwitchPreset { slot, resp } => {
                Self::respond(resp, self.handle_switch_preset(slot));
            }
            ConfigRequest::SavePreset { slot, resp } => {
                Self::respond(resp, self.handle_save_preset(slot));
            }
            ConfigRequest::DeletePreset { slot, resp } => {
                Self::respond(resp, self.handle_delete_preset(slot));
            }
            ConfigRequest::PutDevice { alias, def, resp } => {
                Self::respond(resp, self.handle_put_device(alias, def));
            }
            ConfigRequest::DeleteDevice { alias, resp } => {
                Self::respond(resp, self.handle_delete_device(&alias));
            }
            ConfigRequest::RenameDevice { old_alias, new_alias, def, resp } => {
                Self::respond(resp, self.handle_rename_device(&old_alias, &new_alias, def));
            }
            ConfigRequest::UpdateControllers { controllers, resp } => {
                Self::respond(resp, self.handle_update_controllers(controllers));
            }
            ConfigRequest::SetChains { json, resp } => {
                Self::respond(resp, self.handle_set_chains(&json));
            }
            ConfigRequest::ApplySet { path, value, source, resp } => {
                Self::respond(resp, self.handle_apply_set(&path, value, &source));
            }
            ConfigRequest::ApplyControl(_msg) => {
                // @todo: forward to audio + bus
            }
            ConfigRequest::ToggleCompare { resp } => {
                Self::respond(resp,self.handle_toggle_compare());
            }
            ConfigRequest::Reload { resp }=> {
                Self::respond(resp,self.handle_reload());
            }
            ConfigRequest::SaveState => {
                self.handle_save_state();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Read handlers
    // -----------------------------------------------------------------------

    fn read_config(&self) -> Result<Value> {
        Ok(serde_json::json!({
            "in_channels":        self.cfg.in_channels,
            "out_channels":       self.cfg.out_channels,
            "sample_rate":        self.cfg.sample_rate,
            "buffer_size":        self.cfg.buffer_size,
            "audio_device":       self.cfg.audio_device,
            "delay_max_seconds":  self.cfg.delay_max_seconds,
            "looper_max_seconds": self.cfg.looper_max_seconds,
        }))
    }

    // -----------------------------------------------------------------------
    // Mutation handlers
    // -----------------------------------------------------------------------

    fn handle_update_config(&mut self, body: &Value) -> Result<()> {
        if let Some(v) = body["sample_rate"].as_u64()       { self.cfg.sample_rate       = v as u32; }
        if let Some(v) = body["buffer_size"].as_u64()       { self.cfg.buffer_size       = v as u32; }
        if let Some(v) = body["audio_device"].as_str()       { self.cfg.audio_device      = v.to_string(); }
        if let Some(v) = body["in_channels"].as_u64()       { self.cfg.in_channels       = v as u16; }
        if let Some(v) = body["out_channels"].as_u64()      { self.cfg.out_channels      = v as u16; }
        if let Some(v) = body["delay_max_seconds"].as_f64() { self.cfg.delay_max_seconds = v as f32; }
        self.cfg.persist()?;
        Ok(())
    }

    fn handle_switch_preset(&mut self, slot: u8) -> Result<()> {
        let preset = self.cfg.presets.get(slot)
            .with_context(|| format!("preset {slot} not found"))?.clone();

        let chains = self.build_chains(&preset.chains)?;
        self.audio.push_patch(chains)
            .context("patch channel full, preset not loaded")?;

        // @todo, must propagate to bus! This line is from http, should happen here
        // s.bus.send(ControlMessage::ProgramChange(n)).ok();

        self.clear_controllers();
        self.apply_controllers(&preset.controllers);
        self.snapshot.load_preset(preset, Clean);
        self.cfg.presets.active = slot;
        self.notify_preset_loaded();
        info!("Loaded preset {slot}");
        Ok(())
    }

    fn handle_save_preset(&mut self, slot: u8) -> Result<()> {
        self.snapshot.set_to_slot(slot);
        self.cfg.presets.save_to_slot(self.snapshot.preset.clone());
        self.cfg.presets.active = slot;
        self.snapshot.preset_indices = self.cfg.presets.indices();
        self.cfg.persist()?;
        if self.snapshot.set_state(Clean) {
            self.notify_state_changed();
        }
        Ok(())
    }

    fn handle_delete_preset(&mut self, slot: u8) -> Result<()> {
        if !self.cfg.presets.remove_slot(slot) {
            bail!("preset not found");
        }
        self.snapshot.preset_indices = self.cfg.presets.indices();

        // If this is the active preset, clear all live objects
        if slot == self.cfg.presets.active {
            self.snapshot.load_preset(PresetDef::default(), Dirty);
            self.notify_preset_loaded();
            self.clear_controllers();
            self.cfg.presets.active = PRESET_NONE;
            if self.audio.push_patch(Vec::new()).is_err() {
                bail!("patch channel full, preset {slot} not loaded");
            }
        }

        self.cfg.persist()?;
        Ok(())
    }

    // @todo check
    fn handle_put_device(&mut self, alias: String, def: DeviceDef) -> Result<()> {
        let was_active = self.cfg.control_devices.get(&alias).map(|d| d.is_active()).unwrap_or(false);
        let is_active = def.is_active();

        self.cfg.control_devices.insert(alias.clone(), def.clone());
        self.cfg.persist()?;

        if !is_active {
            if let Some(tx) = self.device_active.get(&alias) {
                let _ = tx.send(false);
            }
        } else if !was_active {
            // Ensure controller arc exists.
            if !self.controller_map.contains_key(&alias) {
                self.controller_map.insert(
                    alias.clone(),
                    Arc::new(RwLock::new(ControllerDef::default())),
                );
            }
            let mappings = Arc::clone(self.controller_map.get(&alias).unwrap());
            let (tx, rx) = watch::channel(true);
            self.device_active.insert(alias.clone(), tx);
            self.spawn_device_task(&alias, &def, rx, mappings);
        }
        Ok(())
    }

    // @todo check
    fn handle_delete_device(&mut self, alias: &str) -> Result<()> {
        self.cfg.control_devices.remove(alias);
        self.cfg.presets.remove_device(alias);
        if let Some(tx) = self.device_active.get(alias) {
            let _ = tx.send(false);
        }
        self.cfg.persist()?;
        Ok(())
    }

    // @todo check
    fn handle_rename_device(&mut self, old: &str, new: &str, def: DeviceDef) -> Result<()> {
        if old == new {
            self.cfg.control_devices.insert(old.to_string(), def);
            self.cfg.persist()?;
            return Ok(());
        }
        if !self.cfg.control_devices.contains_key(old) {
            bail!("device not found");
        }
        self.cfg.control_devices.remove(old);
        self.cfg.control_devices.insert(new.to_string(), def);
        self.cfg.presets.rename_device(old, new);
        self.cfg.persist()?;
        Ok(())
    }

    fn handle_update_controllers(&mut self, controllers: Vec<ControllerDef>) -> Result<()> {
        self.snapshot.preset.controllers = controllers;
        self.clear_controllers();
        self.apply_controllers(&self.snapshot.preset.controllers);
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed();
        }
        Ok(())
    }

    /// Replace chains in the current preset (add/delete/reorder nodes or chains).
    fn handle_set_chains(&mut self, json: &str) -> Result<()> {
        let body: serde_json::Value = serde_json::from_str(json)?;
        let chain_defs: Vec<ChainDef> = serde_json::from_value(
            body.get("chains").cloned().unwrap_or_default()
        )?;

        let chains = self.build_chains(&chain_defs)?;
        self.audio.push_patch(chains)
            .context("patch channel full, patch not applied")?;
        self.snapshot.preset.chains = chain_defs;
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed();
        }
        info!("Applied PATCH ({} chains)", self.snapshot.preset.chains.len());
        Ok(())
    }

    fn handle_apply_set(&mut self, path: &String, value: f32, source: &str) -> Result<()> {
        debug!("SET {path} {value:.4} [source={source}]");
        if self.snapshot.apply_set(&path, value)? {
            self.notify_state_changed();
        }
        let cm = ControlMessage::SetParam { path: path.clone(), value, source: source.to_string() };
        self.audio.push_control(cm.clone())?;
        self.bus.send(cm).ok();
        Ok(())
    }

    fn handle_toggle_compare(&mut self) -> Result<()> {
        if self.snapshot.toggle_compare(&self.cfg.presets).is_none() {
            return Ok(());
        }
        let chains = self.build_chains(&self.snapshot.preset.chains)?;
        self.audio.push_patch(chains)
            .context("patch channel full, preset not loaded")?;
        self.clear_controllers();
        self.apply_controllers(&self.snapshot.preset.controllers);
        self.notify_preset_loaded();
        Ok(())
    }

    // @tode check this!
    fn handle_reload(&mut self) -> Result<()> {
        let new_cfg = Config::load(self.cfg.config_path.clone())?;

        // Warn about changes that require restart.
        if new_cfg.audio_device != self.cfg.audio_device  { warn!("reload: 'audio_device' changed — restart required"); }
        if new_cfg.sample_rate  != self.cfg.sample_rate  { warn!("reload: 'sample_rate' changed — restart required"); }
        if new_cfg.buffer_size  != self.cfg.buffer_size  { warn!("reload: 'buffer_size' changed — restart required"); }
        if new_cfg.in_channels  != self.cfg.in_channels  { warn!("reload: 'in_channels' changed — restart required"); }
        if new_cfg.out_channels != self.cfg.out_channels { warn!("reload: 'out_channels' changed — restart required"); }
        if new_cfg.http_port    != self.cfg.http_port    { warn!("reload: 'http_port' changed — restart required"); }

        // Save current state before swapping.
        
        if let Err(e) = self.snapshot.persist_state(&self.cfg.state_save_path) {
            warn!("reload: state save failed: {e}");
        }

        // let controllers = new_cfg.presets.active_entry()
        //     .map(|p| p.controllers.clone())
        //     .unwrap_or_default();
        // let state_path = new_cfg.state_save_path;
        // let old_config_path = self.cfg.config_path.clone();
        // self.cfg = new_cfg;
        // self.cfg.config_path = old_config_path;

        // // Build chains from new config, overlaying saved state.
        // let structure_json = self.cfg.startup_chains_json()
        //     .unwrap_or(None)
        //     .unwrap_or_else(|| r#"{"chains":[]}"#.into());

        // let merged = if !state_path.as_os_str().is_empty() && state_path.exists() {
        //     info!("reload: overlaying saved params from {}", state_path.display());
        //     match std::fs::read_to_string(&state_path)
        //         .ok()
        //         .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        //     {
        //         Some(saved) => {
        //             let structure: Value = serde_json::from_str(&structure_json).unwrap_or_default();
        //             save::merge_state_params(structure, &saved).to_string()
        //         }
        //         None => structure_json,
        //     }
        // } else {
        //     structure_json
        // };

        // match patch::load_str(&merged, &self.cfg) {
        //     Ok(mut chains) => {
        //         for chain in &mut chains { chain.init_bus(&self.bus); }
        //         let json = patch::chains_to_json(&chains);
        //         if self.audio.push_patch(chains).is_err() {
        //             warn!("reload: patch channel full");
        //         } else {
        //             self.preset_state.apply_patch(json);
        //             self.clear_controllers();
        //             self.apply_controllers(&controllers);
        //             info!("reload: done.");
        //         }
        //     }
        //     Err(e) => warn!("reload: chain build error: {e}"),
        // }
        Ok(())
    }

    fn handle_save_state(&mut self) {        
        match self.snapshot.persist_state(&self.cfg.state_save_path) {
            Ok(true) => debug!("state saved"),
            Ok(false) => {},
            Err(e) => warn!("state save failed: {e}"),
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------
    /// Push a PresetLoaded message on the bus (full preset + state).
    fn notify_preset_loaded(&self) {
        self.bus.send(ControlMessage::PresetLoaded {
            preset: serde_json::to_value(&self.snapshot.preset).unwrap_or_default(),
            preset_indices: self.snapshot.preset_indices.clone(),
            state: self.snapshot.state.label().to_string(),
        }).ok();
    }

    /// Push a StateChanged message on the bus (metadata only, no preset payload).
    fn notify_state_changed(&self) {
        self.bus.send(ControlMessage::StateChanged {
            state: self.snapshot.state.label().to_string(),
            preset_index: self.snapshot.preset.index,
            preset_indices: self.snapshot.preset_indices.clone(),
        }).ok();
    }

    fn build_chains(&self, chains: &Vec<ChainDef>) -> Result<Vec<Chain>> {
        let mut chains = patch::load_patch_def(chains, &self.cfg)
            .context("Chain error")?;

        for chain in &mut chains { chain.init_bus(&self.bus); }
        Ok(chains)
    }

    fn clear_controllers(&self) {
        for arc in self.controller_map.values() {
            *arc.write().unwrap() = ControllerDef::default();
        }
    }

    fn apply_controllers(&self, controllers: &[ControllerDef]) {
        for ctrl_def in controllers {
            if let Some(arc) = self.controller_map.get(&ctrl_def.device) {
                *arc.write().unwrap() = ctrl_def.clone();
            } else {
                warn!("controller references unknown device '{}' — skipping", ctrl_def.device);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Device spawning
    // -----------------------------------------------------------------------

    fn spawn_initial_devices(&mut self) {
        let devices: Vec<(String, DeviceDef)> = self.cfg.control_devices.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (ref alias, ref def) in devices {
            let is_active = def.is_active();
            let (tx, rx) = watch::channel(is_active);
            self.device_active.insert(alias.clone(), tx);

            if !is_active {
                info!("Device '{alias}': disabled (active: false)");
                continue;
            }

            let mappings = match self.controller_map.get(alias.as_str()) {
                Some(arc) => Arc::clone(arc),
                None      => continue,
            };

            self.spawn_device_task(alias, def, rx, mappings);
        }
    }

    fn spawn_device_task(
        &self,
        alias:    &str,
        def:      &DeviceDef,
        active_rx: watch::Receiver<bool>,
        mappings: Arc<RwLock<ControllerDef>>,
    ) {
        let bus = self.bus.clone();
        let master_tx = self.master_tx.clone();

        match def {
            DeviceDef::Serial { dev, baud, fallback, .. } => {
                let serial = SerialControl::new(
                    alias.to_string(), dev.clone(), *baud, *fallback, bus, mappings, master_tx,
                );
                let alias = alias.to_string();
                tokio::spawn(async move {
                    if let Err(e) = serial.run(active_rx).await {
                        tracing::error!("Serial '{alias}': {e}");
                    }
                });
            }
            DeviceDef::Net { host, port, fallback, .. } => {
                let net = NetworkControl::new(
                    alias.to_string(), host.clone(), *port, *fallback, bus.clone(), mappings, master_tx,
                );
                let alias = alias.to_string();
                tokio::spawn(async move {
                    if let Err(e) = net.run(active_rx).await {
                        tracing::error!("Network '{alias}': {e}");
                    }
                });
            }
            DeviceDef::MidiIn { dev, channel, .. } => {
                let midi = MidiControl::new(alias.to_string(), dev.clone(), channel.clone(), mappings);
                midi.run(master_tx);
            }
            DeviceDef::MidiOut { dev, channel, .. } => {
                let midi_out = MidiOutControl::new(dev.clone(), *channel, mappings);
                midi_out.run(bus);
            }
        }
    }
}


// ---------------------------------------------------------------------------
// Public access
// ---------------------------------------------------------------------------

/// Send a request to the master and return the result directly.
pub async fn snd_request<T, F>(master_tx: &mpsc::Sender<ConfigRequest>, build: F) -> Result<T>
where
    F: FnOnce(oneshot::Sender<Result<T>>) -> ConfigRequest,
{
    let (tx, rx) = oneshot::channel();
    master_tx.send(build(tx)).await?;
    rx.await?
}
