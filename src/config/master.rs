use std::collections::HashMap;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn, debug};
use anyhow::{Result, Context, bail};

use super::{Config, ConfigPatch};
use super::preset::PresetDef;
use super::snapshot::{ConfigSnapshot, SnapshotState::*};
use super::ChainDef;
use super::preset::PRESET_NONE;
use crate::control;
use crate::control::mapping::{ControlDef, ControllerDef, DeviceDef};
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
    GetConfig       { resp: Resp<Result<ConfigPatch>> },
    GetSnapshot     { resp: Resp<Result<ConfigSnapshot>> },
    GetDevices      { resp: Resp<Result<Value>> },
    // -- Config mutations (need response for HTTP) --
    UpdateConfig      { config: ConfigPatch, source: String, resp: OptionResp },
    SwitchPreset      { slot: u8, source: String, resp: OptionResp },
    SavePreset        { slot: u8, source: String, resp: OptionResp },
    DeletePreset      { slot: u8, source: String, resp: OptionResp },
    PutDevice         { alias: String, def: DeviceDef, source: String, resp: OptionResp },
    DeleteDevice      { alias: String, source: String, resp: OptionResp },
    RenameDevice      { old_alias: String, new_alias: String, source: String, resp: OptionResp },
    UpdateControllers { controllers: Vec<ControllerDef>, source: String, resp: OptionResp },
    ApplySet          { path: String, value: f32, source: String, resp: OptionResp },
    ApplyCtrl         { channel_id: String, raw: f32, alias: String,
                        source: String, resp: OptionResp },
    ApplyAction       { path: String, action: String, source: String, resp: OptionResp },
    ApplyReset        { source: String, resp: OptionResp },
    /// Reverse-map a parameter to (channel_id, raw_value) without rounding.
    /// Use for binary protocols (MIDI) that do their own integer rounding.
    ReverseMap        { path: String, value: f32, alias: String,
                        resp: Resp<Result<Option<(String, f32)>>> },
    /// Same as `ReverseMap` but the raw value is pre-rounded via the mapping's
    /// cached ctrl multiplier. Use for text protocols (serial / TCP) so the
    /// wire output has clean decimals via default `Display`.
    ReverseMapRounded { path: String, value: f32, alias: String,
                        resp: Resp<Result<Option<(String, f32)>>> },

    // -- Chain structure update --
    SetChains         { chains: Vec<ChainDef>, source: String, resp: OptionResp },

    // -- Fire-and-forget (MIDI notes) --
    ApplyControl(ControlMessage), // @todo maybe rename this to ApplyMidiControl, otherwise confusing with ApplyCtrl
    ToggleCompare { source: String, resp: OptionResp },

    // -- Internal --
    Reload { source: String, resp: OptionResp },
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
    /// Per-device-alias mapping definitions.
    controller_map:   HashMap<String, ControllerDef>,
    /// Per-device kill switch. Send false → device task shuts down gracefully.
    device_active:    HashMap<String, watch::Sender<bool>>,
    /// Broadcast channel (tokio::broadcast) for notifying all listeners (MIDI out, serial, network, WS) of control messages.
    bus:              EventBus,
    /// Self-handle — so device-spawn helpers can give new tasks a way to send requests back to the master (e.g. serial sending ApplySet).
    master_tx:        mpsc::Sender<ConfigRequest>,
    /// Handle reload (from http)
    reload_tx:        mpsc::Sender<()>,
}

/// Create the master, spawn it on the tokio runtime, return the channels.
pub fn spawn(cfg: Config, audio: AudioHandle, reload_tx: mpsc::Sender<()>) -> Result<(mpsc::Sender<ConfigRequest>, control::EventBus)> {
    let snapshot = ConfigSnapshot::restore_or_build(&cfg)?;

    // --- Event bus ---
    let bus = control::new_event_bus();

    let (master_tx, req_rx) = mpsc::channel(256);

    let controller_map: HashMap<String, ControllerDef> = cfg.control_devices.keys()
        .map(|alias| (alias.clone(), ControllerDef::default()))
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
        reload_tx,
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
            },
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
                let _ = resp.send(Ok(ConfigPatch::from_config(&self.cfg)));
            },
            ConfigRequest::GetSnapshot { resp } => {
                let _ = resp.send(Ok(self.snapshot.clone()));
            },
            ConfigRequest::GetDevices { resp } => {
                let snd = serde_json::to_value(&self.cfg.control_devices)
                    .unwrap_or(Value::Array(vec![]));
                let _ = resp.send(Ok(snd));
            }
            // Mutations
            ConfigRequest::UpdateConfig { config, source, resp } => {
                Self::respond(resp, self.handle_update_config(config, &source));
            },
            ConfigRequest::SwitchPreset { slot, source, resp } => {
                Self::respond(resp, self.handle_switch_preset(slot, &source));
            },
            ConfigRequest::SavePreset { slot, source, resp } => {
                Self::respond(resp, self.handle_save_preset(slot, &source));
            },
            ConfigRequest::DeletePreset { slot, source, resp } => {
                Self::respond(resp, self.handle_delete_preset(slot, &source));
            },
            ConfigRequest::PutDevice { alias, def, source, resp } => {
                Self::respond(resp, self.handle_put_device(alias, def, &source));
            },
            ConfigRequest::DeleteDevice { alias, source, resp } => {
                Self::respond(resp, self.handle_delete_device(&alias, &source));
            },
            ConfigRequest::RenameDevice { old_alias, new_alias, source, resp } => {
                Self::respond(resp, self.handle_rename_device(&old_alias, &new_alias, &source));
            },
            ConfigRequest::UpdateControllers { controllers, source, resp } => {
                Self::respond(resp, self.handle_update_controllers(controllers, &source));
            },
            ConfigRequest::SetChains { chains, source, resp } => {
                Self::respond(resp, self.handle_set_chains(chains, &source));
            },
            ConfigRequest::ApplySet { path, value, source, resp } => {
                Self::respond(resp, self.handle_apply_set(&path, value, &source));
            },
            ConfigRequest::ApplyCtrl { channel_id, raw, alias, source, resp } => {
                Self::respond(resp, self.handle_apply_ctrl(&channel_id, raw, &alias, &source));
            },
            ConfigRequest::ApplyAction { path, action, source, resp } => {
                Self::respond(resp, self.handle_apply_action(&path, &action, &source));
            },
            ConfigRequest::ApplyReset { source, resp } => {
                Self::respond(resp, self.handle_apply_reset(&source));
            },
            ConfigRequest::ReverseMap { path, value, alias, resp } => {
                let result = self.lookup_reverse(&path, &alias)
                    .map(|(ch, def)| (ch, def.to_ctrl(value)));
                let _ = resp.send(Ok(result));
            },
            ConfigRequest::ReverseMapRounded { path, value, alias, resp } => {
                let result = self.lookup_reverse(&path, &alias)
                    .map(|(ch, def)| (ch, def.smart_round_ctrl(def.to_ctrl(value))));
                let _ = resp.send(Ok(result));
            },
            ConfigRequest::ApplyControl(msg) => {
                self.handle_apply_control(msg);
            },
            ConfigRequest::ToggleCompare { source, resp } => {
                Self::respond(resp,self.handle_toggle_compare(&source));
            },
            ConfigRequest::Reload { source, resp }=> {
                Self::respond(resp,self.handle_reload(&source));
            },
            ConfigRequest::SaveState => {
                self.handle_save_state();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mutation handlers
    // -----------------------------------------------------------------------

    fn handle_update_config(&mut self, config: ConfigPatch, source: &str) -> Result<()> {
        if let Some(v) = config.sample_rate       { self.cfg.sample_rate       = v as u32; }
        if let Some(v) = config.buffer_size       { self.cfg.buffer_size       = v as u32; }
        if let Some(v) = config.audio_device   { self.cfg.audio_device      = v.to_string(); }
        if let Some(v) = config.in_channels       { self.cfg.in_channels       = v as u16; }
        if let Some(v) = config.out_channels      { self.cfg.out_channels      = v as u16; }
        if let Some(v) = config.delay_max_seconds { self.cfg.delay_max_seconds = v as f32; }
        self.cfg.persist()?;
        info!("Updated config [source={source}]");
        Ok(())
    }

    fn handle_switch_preset(&mut self, slot: u8, source: &str) -> Result<()> {
        let preset = self.cfg.presets.get(slot)
            .with_context(|| format!("preset {slot} not found"))?.clone();

        let chains = self.build_chains(&preset.chains)?;
        self.audio.push_patch(chains)
            .context("patch channel full, preset not loaded")?;

        self.clear_controllers();
        self.apply_controllers(&preset.controllers);
        self.snapshot.load_preset(preset, Clean);
        self.cfg.presets.active = slot;
        self.notify_preset_loaded();
        info!("Loaded preset {slot} [source={source}]");
        Ok(())
    }

    fn handle_save_preset(&mut self, slot: u8, source: &str) -> Result<()> {
        self.snapshot.set_to_slot(slot);
        self.cfg.presets.save_to_slot(self.snapshot.preset.clone());
        self.cfg.presets.active = slot;
        self.snapshot.preset_indices = self.cfg.presets.indices();
        self.cfg.persist()?;
        if self.snapshot.set_state(Clean) {
            self.notify_state_changed();
        }
        info!("Saved preset {slot} [source={source}]");
        Ok(())
    }

    fn handle_delete_preset(&mut self, slot: u8, source: &str) -> Result<()> {
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
        info!("Deleted preset {slot} [source={source}]");
        Ok(())
    }

    fn handle_put_device(&mut self, alias: String, def: DeviceDef, source: &str) -> Result<()> {
        let was_active = self.cfg.control_devices.get(&alias).map(|d| d.is_active()).unwrap_or(false);
        let is_active = def.is_active();

        self.cfg.control_devices.insert(alias.clone(), def.clone());
        self.cfg.persist()?;

        if !is_active {
            if let Some(tx) = self.device_active.get(&alias) {
                let _ = tx.send(false);
            }
        } else if !was_active {
            // Ensure controller entry exists.
            if !self.controller_map.contains_key(&alias) {
                self.controller_map.insert(alias.clone(), ControllerDef::default());
            }
            let (tx, rx) = watch::channel(true);
            self.device_active.insert(alias.clone(), tx);
            self.spawn_device_task(&alias, &def, rx);
        }
        info!("Updated device {alias} [source={source}]");
        Ok(())
    }

    fn handle_delete_device(&mut self, alias: &str, source: &str) -> Result<()> {
        self.cfg.control_devices.remove(alias);
        self.cfg.presets.remove_device(alias);
        if let Some(tx) = self.device_active.get(alias) {
            let _ = tx.send(false);
        }
        self.cfg.persist()?;
        info!("Deleted device {alias} [source={source}]");
        Ok(())
    }

    fn handle_rename_device(&mut self, old: &str, new: &str, source: &str) -> Result<()> {
        if old == new { return Ok(()); }

        if self.cfg.control_devices.contains_key(new) {
            bail!("Device '{new}' already exists");
        }
        let def = self.cfg.control_devices.remove(old)
            .with_context(|| format!("Device '{old}' not found"))?;
        self.cfg.control_devices.insert(new.to_string(), def);

        // Keep sibling maps in sync.
        if let Some(ctrl) = self.controller_map.remove(old) {
            self.controller_map.insert(new.to_string(), ctrl);
        }
        if let Some(tx) = self.device_active.remove(old) {
            self.device_active.insert(new.to_string(), tx);
        }

        // All references in the controllers of the presets must be renamed.
        self.cfg.presets.rename_device(old, new);
        self.cfg.persist()?;
        info!("Renamed device {old} to {new} [source={source}]");
        Ok(())
    }

    fn handle_update_controllers(&mut self, controllers: Vec<ControllerDef>, source: &str) -> Result<()> {
        self.snapshot.preset.controllers = controllers;
        self.clear_controllers();
        self.apply_controllers(&self.snapshot.preset.controllers.clone());
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed();
        }
        info!("Updated controllers [source={source}]");
        Ok(())
    }

    /// Replace chains in the current preset (add/delete/reorder nodes or chains).
    fn handle_set_chains(&mut self, chain_defs: Vec<ChainDef>, source: &str) -> Result<()> {
        let chains = self.build_chains(&chain_defs)?;
        self.audio.push_patch(chains)
            .context("patch channel full, patch not applied")?;
        self.snapshot.preset.chains = chain_defs;
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed();
        }
        info!("Applied PATCH ({} chains) [source={source}]", self.snapshot.preset.chains.len());
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

    fn handle_apply_ctrl(&mut self, channel_id: &str, raw: f32, alias: &str, source: &str) -> Result<()> {
        let translated = self.controller_map.get(alias)
            .and_then(|m| control::translate_ctrl(channel_id, raw, m));
        if let Some((path, value)) = translated {
            self.handle_apply_set(&path, value, source)?;
        }
        Ok(())
    }

    fn handle_apply_action(&mut self, path: &str, action: &str, source: &str) -> Result<()> {
        debug!("ACTION {path} {action} [source={source}]");
        let cm = ControlMessage::Action { path: path.to_string(), action: action.to_string(), source: source.to_string() };
        self.audio.push_control(cm.clone())?;
        self.bus.send(cm).ok();
        Ok(())
    }

    fn handle_apply_reset(&mut self, source: &str) -> Result<()> {
        debug!("RESET [source={source}]");
        let cm = ControlMessage::Reset { source: source.to_string() };
        self.audio.push_control(cm.clone())?;
        self.bus.send(cm).ok();
        Ok(())
    }

    /// Find the reverse mapping for `path` on device `alias`: the channel_id
    /// the target maps to (if any) and the `ControlDef` that does the math.
    /// Callers decide whether to use `to_ctrl` alone or with `smart_round_ctrl`.
    fn lookup_reverse(&self, path: &str, alias: &str) -> Option<(String, &ControlDef)> {
        let mappings = self.controller_map.get(alias)?;
        let (ch, def) = mappings.channel_for_target(path)?;
        Some((ch.to_string(), def))
    }

    /// Forward a fire-and-forget ControlMessage to audio + bus (MIDI NoteOn/NoteOff).
    /// Channel filtering is done at the device level before we get here.
    fn handle_apply_control(&mut self, msg: ControlMessage) {
        if let Err(e) = self.audio.push_control(msg.clone()) {
            warn!("ApplyControl: audio push failed: {e}");
        }
        self.bus.send(msg).ok();
    }

    fn handle_toggle_compare(&mut self, source: &str) -> Result<()> {
        debug!("COMPARE [source={source}]");
        if self.snapshot.toggle_compare(&self.cfg.presets).is_none() {
            return Ok(());
        }
        let chains = self.build_chains(&self.snapshot.preset.chains)?;
        self.audio.push_patch(chains)
            .context("patch channel full, preset not loaded")?;
        self.clear_controllers();
        self.apply_controllers(&self.snapshot.preset.controllers.clone());
        self.notify_preset_loaded();
        Ok(())
    }

    fn handle_reload(&mut self, source: &str) -> Result<()> {
        debug!("RELOAD [source={source}]");
        let tx = self.reload_tx.clone(); 
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = tx.send(()).await;
        });
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
            preset: self.snapshot.preset.clone(),
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

    fn clear_controllers(&mut self) {
        for def in self.controller_map.values_mut() {
            *def = ControllerDef::default();
        }
    }

    fn apply_controllers(&mut self, controllers: &[ControllerDef]) {
        for ctrl_def in controllers {
            if let Some(entry) = self.controller_map.get_mut(&ctrl_def.device) {
                *entry = ctrl_def.clone();
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

            self.spawn_device_task(alias, def, rx);
        }
    }

    fn spawn_device_task(
        &self,
        alias:    &str,
        def:      &DeviceDef,
        active_rx: watch::Receiver<bool>,
    ) {
        let bus = self.bus.clone();
        let master_tx = self.master_tx.clone();

        match def {
            DeviceDef::Serial { dev, baud, .. } => {
                let serial = SerialControl::new(
                    alias.to_string(), dev.clone(), *baud, bus, master_tx,
                );
                let alias = alias.to_string();
                tokio::spawn(async move {
                    if let Err(e) = serial.run(active_rx).await {
                        tracing::error!("Serial '{alias}': {e}");
                    }
                });
            },
            DeviceDef::Net { host, port, .. } => {
                let net = NetworkControl::new(
                    alias.to_string(), host.clone(), *port, bus.clone(), master_tx,
                );
                let alias = alias.to_string();
                tokio::spawn(async move {
                    if let Err(e) = net.run(active_rx).await {
                        tracing::error!("Network '{alias}': {e}");
                    }
                });
            },
            DeviceDef::MidiIn { dev, channel, .. } => {
                let midi = MidiControl::new(alias.to_string(), dev.clone(), channel.clone());
                midi.run(master_tx);
            },
            DeviceDef::MidiOut { dev, channel, .. } => {
                let midi_out = MidiOutControl::new(dev.clone(), *channel, alias.to_string(), master_tx);
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
