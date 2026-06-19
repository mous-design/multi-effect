use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn, debug};
use anyhow::{Result, Context, bail};

use super::{Config, ConfigPatch};
use super::preset::{PresetDef, PRESET_NONE};
use super::snapshot::{ConfigSnapshot, SnapshotState, SnapshotState::*};
use super::ChainDef;
use crate::control::{self, EventBus, NetworkControl, SerialControl, ControlMessage};
use crate::control::mapping::{ControlDef, ControllerDef, DeviceDef};
use crate::control::midi::{MidiControl, MidiOutControl};
use crate::engine::AudioHandle;
use crate::engine::device::{apply_override, aspect_value, EventAction, MetaAspect, MetaTarget,
    OverrideMap, ParamInfo, ParamKind, ParamValue};
use crate::engine::patch::{self, resolve_params_info, Chain};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub type Resp<T> = oneshot::Sender<Result<T>>;
pub type OptionResp<T> = Option<Resp<T>>;
pub type OptionRespEmpty = Option<Resp<()>>;

/// Inbound requests to the config master.
pub enum ConfigRequest {
    // -- Reads (need response) --
    GetConfig       { resp: Resp<ConfigPatch> },
    GetSnapshot     { resp: Resp<ConfigSnapshot> },
    GetDevices      { resp: Resp<HashMap<String, DeviceDef>> },
    /// Canonical (firmware-declared, pre-override) `ParamInfo` per effect
    /// type. Used by the Type-overrides editor UI as the absolute envelope:
    /// every override clamps against this.
    GetCanonical    { resp: Resp<HashMap<String, Vec<ParamInfo>>> },
    // -- Config mutations (need response for HTTP) --
    UpdateConfig      { config: ConfigPatch, confirmed: bool, source: String, resp: OptionRespEmpty },
    SwitchPreset      { slot: u8, source: String, resp: OptionRespEmpty },
    SavePreset        { slot: u8, source: String, resp: OptionRespEmpty },
    DeletePreset      { slot: u8, source: String, resp: OptionRespEmpty },
    PutDevice         { alias: String, def: DeviceDef, source: String, resp: OptionRespEmpty },
    DeleteDevice      { alias: String, source: String, resp: OptionRespEmpty },
    RenameDevice      { old_alias: String, new_alias: String, source: String, resp: OptionRespEmpty },
    UpdateControllers { controllers: Vec<ControllerDef>, source: String, resp: OptionRespEmpty },
    ApplySet          { path: String, value: ParamValue,
                        source: String, resp: OptionResp<SnapshotState> },
    /// Chain-level set (e.g. `mute_dry`). `chain_idx` indexes into
    /// `snapshot.preset.chains`. Marks the preset dirty on real change;
    /// triggers a `recompute_dry_effective_all` walk so derived state
    /// follows the new user setting.
    ApplyChainSet     { chain_idx: usize, param: String, value: ParamValue,
                        source: String, resp: OptionResp<SnapshotState> },
    ApplyCtrl         { channel_id: String, raw: f32, alias: String,
                        source: String, resp: OptionResp<SnapshotState> },
    ApplyAction       { path: String, action: EventAction, source: String, resp: OptionRespEmpty },
    ApplyReset        { source: String, resp: OptionRespEmpty },
    /// Apply an Instance bound override (the runtime "edit a param's
    /// min/max/default" path). `path` is the node key, `target` is param +
    /// aspect, `value` is the new bound value. Master computes the
    /// Type-resolved view and forwards to the audio thread.
    ApplyInfoOverride { path: String, target: MetaTarget, value: ParamValue,
                        confirmed: bool, source: String, resp: OptionResp<SnapshotState> },
    /// Reverse-map a parameter to (channel_id, raw_value) without rounding.
    /// Use for binary protocols (MIDI) that do their own integer rounding.
    ReverseMap        { path: String, value: f32, alias: String,
                        resp: Resp<Option<(String, f32)>> },
    /// Same as `ReverseMap` but the raw value is pre-rounded via the mapping's
    /// cached ctrl multiplier. Use for text protocols (serial / TCP) so the
    /// wire output has clean decimals via default `Display`.
    ReverseMapRounded { path: String, value: f32, alias: String,
                        resp: Resp<Option<(String, f32)>> },

    // -- Chain structure update --
    SetChains         { chains: Vec<ChainDef>, source: String, resp: OptionRespEmpty },

    // -- Fire-and-forget (MIDI notes) --
    ApplyControl(ControlMessage), // @todo maybe rename this to ApplyMidiControl, otherwise confusing with ApplyCtrl
    ToggleCompare { source: String, resp: OptionRespEmpty },
    /// Ask each effect to re-fire its current live state on the bus. Used
    /// when a client connects so it can catch up. All current bus
    /// subscribers receive the events; existing clients see a no-op refresh,
    /// the new one gets state-from-zero.
    RepublishLiveState { resp: OptionRespEmpty },

    // -- Internal --
    Reload { source: String, resp: OptionRespEmpty },
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
        // Boot-time Type-overrides sanitiser: clamp stale entries the file may
        // carry from an earlier session (e.g. canonical narrowed since save,
        // or an old build with wider BoundMeta). Persist if anything changed
        // so the on-disk file is cleaned up too.
        let before = self.cfg.type_overrides.clone();
        sanitize_type_overrides(&mut self.cfg.type_overrides);
        if self.cfg.type_overrides != before {
            if let Err(e) = self.cfg.persist() {
                warn!("persist after Type-overrides sanitise failed: {e}");
            }
        }

        self.spawn_initial_devices();

        // Resolve `params_info` on each node of the active preset — it isn't
        // persisted, so a freshly-loaded snapshot has empty arrays. After
        // this, every wire snapshot is a cheap clone (no recomputation) and
        // master reads (clamp, smart-round) are direct field lookups.
        self.refresh_preset_params_info();

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
        // Initialize each mix node's `dry_effective` based on current chains.
        self.recompute_dry_effective_all();

        while let Some(req) = rx.recv().await {
            self.handle(req);
        }

        // Shutdown: final save.
        self.handle_save_state();
        info!("ConfigMaster shutting down.");
    }
    fn respond<T>(resp: OptionResp<T>, value: Result<T>) {
        if let Some(resp) = resp {
            let _ = resp.send(value);
        }
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
                let _ = resp.send(Ok(self.cfg.control_devices.clone()));
            }
            ConfigRequest::GetCanonical { resp } => {
                let _ = resp.send(Ok(canonical_map()));
            }
            // Mutations
            ConfigRequest::UpdateConfig { config, confirmed, source, resp } => {
                Self::respond(resp, self.handle_update_config(config, confirmed, &source));
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
            ConfigRequest::ApplyChainSet { chain_idx, param, value, source, resp } => {
                Self::respond(resp, self.handle_apply_chain_set(chain_idx, &param, value, &source));
            },
            ConfigRequest::ApplyCtrl { channel_id, raw, alias, source, resp } => {
                Self::respond(resp, self.handle_apply_ctrl(&channel_id, raw, &alias, &source));
            },
            ConfigRequest::ApplyAction { path, action, source, resp } => {
                Self::respond(resp, self.handle_apply_action(&path, action, &source));
            },
            ConfigRequest::ApplyReset { source, resp } => {
                Self::respond(resp, self.handle_apply_reset(&source));
            },
            ConfigRequest::ApplyInfoOverride { path, target, value, confirmed, source, resp } => {
                Self::respond(resp, self.handle_apply_info_override(&path, target, value, confirmed, &source));
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
            ConfigRequest::RepublishLiveState { resp } => {
                Self::respond(resp, self.audio.push_control(ControlMessage::RepublishLiveState)
                    .map_err(|e| anyhow::anyhow!(e)));
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

    fn handle_update_config(&mut self, config: ConfigPatch, confirmed: bool, source: &str) -> Result<()> {
        // Phase 1: type_overrides. Mutate in-memory, run the bound-grow check
        // against pre-mutation snapshot. If a non-growable max grew and the
        // client hasn't acknowledged the reload, roll back and refuse with a
        // `confirm_required:` error — client re-sends with `confirmed=true`.
        let type_overrides_changed = config.type_overrides.is_some();
        let before_overrides = self.cfg.type_overrides.clone();
        let before_maxes = type_overrides_changed.then(|| self.non_growable_maxes());
        let mut grew = false;
        if let Some(mut v) = config.type_overrides {
            sanitize_type_overrides(&mut v);
            self.cfg.type_overrides = v;
            self.refresh_preset_params_info();
            if let Some(before) = &before_maxes {
                grew = Self::growth_requires_reload(before, &self.non_growable_maxes());
            }
            if grew && !confirmed {
                // Rollback in-memory state — no persist yet, no broadcasts emitted.
                info!("type_overrides: bound widens past current — confirm_required");
                self.cfg.type_overrides = before_overrides;
                self.refresh_preset_params_info();
                bail!("confirm_required: bound widens past current — reload needed");
            }
        }

        // Phase 2: other fields (none of these trigger reload).
        if let Some(v) = config.sample_rate  { self.cfg.sample_rate  = v as u32; }
        if let Some(v) = config.buffer_size  { self.cfg.buffer_size  = v as u32; }
        if let Some(v) = config.audio_device { self.cfg.audio_device = v.to_string(); }
        if let Some(v) = config.in_channels  { self.cfg.in_channels  = v as u16; }
        if let Some(v) = config.out_channels { self.cfg.out_channels = v as u16; }

        self.cfg.persist()?;
        if type_overrides_changed {
            // Source = "master": the originator's optimistic state doesn't
            // cover the per-instance params_info refresh; everyone gets it.
            self.notify_preset_loaded("master");
        }
        if grew {
            // Only reached when client supplied `confirmed = true` (else we
            // bailed in Phase 1). Audio buffers are now too small for the new
            // resolved max — schedule a process-level reload.
            info!("type_overrides grew a non-growable max — scheduling reload");
            self.schedule_reload();
        }
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
        self.refresh_preset_params_info();
        self.notify_preset_loaded(source);
        self.notify_state_changed(source);
        // Recompute per-mix `dry_effective` for the newly-loaded chains.
        self.recompute_dry_effective_all();
        info!("Loaded preset {slot} [source={source}]");
        Ok(())
    }

    fn handle_save_preset(&mut self, slot: u8, source: &str) -> Result<()> {
        let prev_indices = self.snapshot.preset_indices.clone();
        self.snapshot.set_to_slot(slot);
        self.cfg.presets.save_to_slot(self.snapshot.preset.clone());
        self.cfg.presets.active = slot;
        self.snapshot.preset_indices = self.cfg.presets.indices();
        self.cfg.persist()?;
        // Always notify: preset.index may have changed; state went to Clean.
        self.notify_preset_loaded(source);
        if self.snapshot.set_state(Clean) {
            self.notify_state_changed(source);
        }
        if self.snapshot.preset_indices != prev_indices {
            self.notify_preset_indices(source);
        }
        info!("Saved preset {slot} [source={source}]");
        Ok(())
    }

    fn handle_delete_preset(&mut self, slot: u8, source: &str) -> Result<()> {
        if !self.cfg.presets.remove_slot(slot) {
            bail!("preset not found");
        }
        self.snapshot.preset_indices = self.cfg.presets.indices();
        self.notify_preset_indices(source);

        // If this is the active preset, clear all live objects
        if slot == self.cfg.presets.active {
            self.snapshot.load_preset(PresetDef::default(), Dirty);
            self.notify_preset_loaded(source);
            self.notify_state_changed(source);
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
        self.notify_preset_loaded(source);
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed(source);
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
        self.refresh_preset_params_info();
        // Always notify ALL clients — originator included. The originator's
        // optimistic update for an added node lacks `params_info` (only master
        // knows canonical), so they need the broadcast to populate it. Use
        // `source = "master"` so they're not filtered out.
        self.notify_preset_loaded("master");
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed(source);
        }
        // New chain composition → re-evaluate per-mix `dry_effective`.
        self.recompute_dry_effective_all();
        info!("Applied PATCH ({} chains) [source={source}]", self.snapshot.preset.chains.len());
        Ok(())
    }

    fn handle_apply_set(&mut self, path: &String, value: ParamValue, source: &str) -> Result<SnapshotState> {
        debug!("SET {path} {value} [source={source}]");
        // Single validator: `apply_set` handles type check, clamp, variant
        // normalisation, and storage in one pass. Returns the actually-stored
        // value for downstream broadcast — never the raw input.
        if let Some(stored) = self.snapshot.apply_set(path, value)? {
            if self.snapshot.set_state(Dirty) {
                self.notify_state_changed(source);
            }
            // Source-rewrite rule: filter the originator only when the broadcast
            // is a faithful echo of their request. If master clamped or
            // variant-normalised, broadcast with `source = "master"` so the
            // originator also hears the authoritative value (corrects their
            // optimistic UI).
            let bcast_source = if stored == value { source.to_string() } else { "master".into() };
            let cm = ControlMessage::SetParam { path: path.clone(), value: stored, source: bcast_source };
            self.audio.push_control(cm.clone())?;
            self.bus.send(cm).ok();
            // Any SET could affect `dry_effective` — an effect's `active` flip
            // (changes the chain-needs-dry tally) or a mix node's `dry`
            // (changes the user-side input to the OR). Cheap walk; recompute
            // unconditionally and let the per-mix diff check suppress no-ops.
            self.recompute_dry_effective_all();
        }
        Ok(self.snapshot.state)
    }

    /// CTRL is intrinsically `f32` (analog hardware). Wrap as `Float`; if the
    /// target param's declared type is something else (Bool / Int), the audio
    /// thread's `set_param` rejects with a typed-mismatch warning. Hardware
    /// mappings to non-float params need explicit conversion at the mapping
    /// layer — not yet implemented.
    fn handle_apply_ctrl(&mut self, channel_id: &str, raw: f32, alias: &str, source: &str) -> Result<SnapshotState> {
        let translated = self.controller_map.get(alias)
            .and_then(|m| control::translate_ctrl(channel_id, raw, m));
        if let Some((path, value)) = translated {
            self.handle_apply_set(&path, ParamValue::Float(value), source)
        } else {
            Ok(self.snapshot.state)
        }
    }

    fn handle_apply_action(&mut self, path: &str, action: EventAction, source: &str) -> Result<()> {
        debug!("ACTION {path} {action:?} [source={source}]");
        let cm = ControlMessage::Action { path: path.to_string(), action, source: source.to_string() };
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

    fn handle_apply_info_override(
        &mut self,
        node_key: &str,
        target:   MetaTarget,
        value:    ParamValue,
        confirmed: bool,
        source:   &str,
    ) -> Result<SnapshotState> {
        debug!("SET META {node_key}.{}.{:?} = {:?} [source={source}]",
               target.param, target.aspect, value);

        // Snapshot the targeted param's aspects before applying — used after
        // resolve to diff against the new state and broadcast every aspect
        // that changed (including cascades like default auto-clamp).
        let param = target.param.clone();
        // Match the override's target entry by name — ParamMeta carries
        // value-shape aspects, ActionsGroup carries only visible/active.
        // Mirrors `apply_override`'s dispatch.
        let find_target = |i: &ParamInfo| i.name == param && match i.kind {
            ParamKind::ParamMeta { .. } | ParamKind::ActionsGroup => true,
            ParamKind::BoundMeta { .. } | ParamKind::Event { .. }  => false,
        };
        let old_aspects = self.snapshot.preset.chains.iter()
            .flat_map(|c| c.nodes.iter())
            .find(|n| n.key == node_key)
            .and_then(|n| n.params_info.iter()
                .find(|i| find_target(i))
                .map(extract_aspects))
            .unwrap_or_default();

        // Bound-grow guard: snapshot non-growable maxes + the prior per-node
        // overrides/params_info for potential rollback. If the override grows
        // a non-growable max and the client didn't acknowledge the reload,
        // restore prior state and refuse with `confirm_required:`.
        let before_maxes = self.non_growable_maxes();
        let prior_node_state = self.snapshot.preset.chains.iter()
            .flat_map(|c| c.nodes.iter())
            .find(|n| n.key == node_key)
            .map(|n| (n.overrides.clone(), n.params_info.clone()));

        // Persist override + refresh resolved view.
        let current_value = if let Some(node) = self.snapshot.preset.chains.iter_mut()
            .flat_map(|c| c.nodes.iter_mut())
            .find(|n| n.key == node_key)
        {
            node.overrides.insert(target.clone(), value);
            node.params_info = resolve_params_info(node, &self.cfg).unwrap_or_default();
            node.params.get(&param).copied()
        } else {
            None
        };

        // Grow check — bail before any broadcasts or audio writes when the
        // client hasn't acknowledged. Rollback restores the per-node state
        // captured above; nothing outside this node was touched.
        let grew = Self::growth_requires_reload(&before_maxes, &self.non_growable_maxes());
        if grew && !confirmed {
            if let (Some((prior_overrides, prior_info)), Some(node)) = (
                prior_node_state,
                self.snapshot.preset.chains.iter_mut()
                    .flat_map(|c| c.nodes.iter_mut())
                    .find(|n| n.key == node_key),
            ) {
                node.overrides = prior_overrides;
                node.params_info = prior_info;
            }
            bail!("confirm_required: bound widens past current — reload needed");
        }

        // Broadcast every aspect that changed (user's direct edit + cascades).
        let new_aspects = self.snapshot.preset.chains.iter()
            .flat_map(|c| c.nodes.iter())
            .find(|n| n.key == node_key)
            .and_then(|n| n.params_info.iter()
                .find(|i| find_target(i))
                .map(extract_aspects))
            .unwrap_or_default();

        let mut new_default: Option<ParamValue> = None;
        let mut default_changed = false;
        for (aspect, new_v) in &new_aspects {
            let old_v = old_aspects.iter().find(|(a, _)| a == aspect).map(|(_, v)| *v);
            if *aspect == MetaAspect::Default {
                new_default = Some(*new_v);
                if Some(*new_v) != old_v { default_changed = true; }
            }
            if Some(*new_v) != old_v {
                // Source-rewrite: only the user's own direct edit with an
                // unmodified value is a true echo (filter their copy). Cascade
                // aspects (default auto-clamp) and any kernel-clamped value
                // go out with `source = "master"` so all clients — originator
                // included — hear the authoritative state.
                let bcast_source = if *aspect == target.aspect && *new_v == value {
                    source.to_string()
                } else {
                    "master".into()
                };
                self.bus.send(ControlMessage::SetInfoOverride {
                    path:   node_key.to_string(),
                    target: MetaTarget { param: param.clone(), aspect: *aspect },
                    value:  *new_v,
                    source: bcast_source,
                }).ok();
            }
        }

        // Make sure the audio thread's runtime state matches the new effective
        // value of this param. Two independent cases:
        //   1. min/max shifted AND a stored override exists → re-apply via
        //      `handle_apply_set` (clamps to new bounds, broadcasts on bus,
        //      pushes to audio).
        //   2. `default` changed (cascaded or directly edited) AND the value
        //      is at-default (no stored override) → push the new default
        //      straight to audio. No bus broadcast: clients' `effectiveValue`
        //      uses `info.default` fallback, already updated via the `default`
        //      aspect PARAM broadcast above.
        if matches!(target.aspect, MetaAspect::Min | MetaAspect::Max) {
            if let Some(current) = current_value {
                let path = format!("{node_key}.{param}");
                self.handle_apply_set(&path, current, "master")?;
            }
        }
        if default_changed && current_value.is_none() {
            if let Some(default) = new_default {
                self.audio.push_control(ControlMessage::SetParam {
                    path:   format!("{node_key}.{param}"),
                    value:  default,
                    source: "master".into(),
                }).ok();
            }
        }

        // Instance bound edits are part of preset state; mark dirty.
        if self.snapshot.set_state(SnapshotState::Dirty) {
            self.notify_state_changed(source);
        }
        // Meta override on the `active` aspect changes the chain's needs-dry
        // tally for the touched node; cheap walk, suppressed when no diff.
        self.recompute_dry_effective_all();
        if grew {
            // Only reached when client supplied `confirmed = true` (else we
            // bailed before the broadcast pass). The live audio buffer was
            // sized smaller than the new resolved max — process restart will
            // rebuild the effect with the widened bound.
            info!("instance override grew a non-growable max — scheduling reload");
            self.schedule_reload();
        }
        Ok(self.snapshot.state)
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
        self.refresh_preset_params_info();
        self.notify_preset_loaded(source);
        self.notify_state_changed(source);
        self.recompute_dry_effective_all();
        Ok(())
    }

    fn handle_reload(&mut self, source: &str) -> Result<()> {
        debug!("RELOAD [source={source}]");
        self.schedule_reload();
        Ok(())
    }

    /// Schedule a process-level reload. 100 ms delay lets any in-flight
    /// response flush to the originator before the runtime drops. Used by the
    /// explicit `RELOAD` command and by the bound-grow auto-trigger.
    fn schedule_reload(&self) {
        let tx = self.reload_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = tx.send(()).await;
        });
    }

    /// Map of every node's current resolved max for `ContinuousFloat` params
    /// marked `max_growable_at_runtime: false`. These are buffer-affecting —
    /// the audio thread sized its fixed buffer from this value at
    /// construction. Comparing this map before and after a bound mutation
    /// tells us whether the live buffer is now too small.
    fn non_growable_maxes(&self) -> HashMap<(String, String), f32> {
        use crate::engine::device::ParamType;
        let mut out = HashMap::new();
        for chain in &self.snapshot.preset.chains {
            for node in &chain.nodes {
                for info in &node.params_info {
                    if !matches!(info.kind,
                        ParamKind::ParamMeta { max_growable_at_runtime: false, .. })
                    {
                        continue;
                    }
                    if let ParamType::ContinuousFloat { max, .. } = info.data_kind {
                        out.insert((node.key.clone(), info.name.to_string()), max);
                    }
                }
            }
        }
        out
    }

    /// True if any (node, param) in `after` has a strictly greater max than
    /// the matching entry in `before`. New keys (post-mutation only) are
    /// ignored — they belong to nodes/params that didn't exist before, so no
    /// pre-sized buffer is at stake.
    fn growth_requires_reload(
        before: &HashMap<(String, String), f32>,
        after:  &HashMap<(String, String), f32>,
    ) -> bool {
        after.iter().any(|(k, new_max)|
            before.get(k).is_some_and(|old_max| new_max > old_max))
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
    /// Push a PresetLoaded event on the bus with the typed preset. Each
    /// subscriber decides what to do — UI wraps with `PresetWire` and
    /// serializes; MIDI out reads `preset.index`. Originator filtered out by
    /// the outbound dispatcher.
    fn notify_preset_loaded(&self, source: &str) {
        self.bus.send(ControlMessage::PresetLoaded {
            preset: self.snapshot.preset.clone(),
            source: source.to_string(),
        }).ok();
    }

    /// Recompute `params_info` for every node in a preset. Call after preset
    /// load (so the active preset's nodes carry their resolved view) and
    /// whenever `cfg.type_overrides` changes.
    fn refresh_preset_params_info(&mut self) {
        for chain in self.snapshot.preset.chains.iter_mut() {
            for node in chain.nodes.iter_mut() {
                node.params_info = resolve_params_info(node, &self.cfg).unwrap_or_default();
            }
        }
    }

    /// Apply a chain-level set. Today the only writable chain param is
    /// `mute_dry` (bool) — the user's "I'd like analog-bypass dry on this
    /// chain" request. Resolution to `dry_effective` happens in the
    /// `recompute_dry_effective_all` walk triggered afterwards.
    fn handle_apply_chain_set(&mut self, chain_idx: usize, param: &str, value: ParamValue, source: &str) -> Result<SnapshotState> {
        debug!("SET_CHAIN {chain_idx} {param} {value:?} [source={source}]");
        let Some(chain) = self.snapshot.preset.chains.get_mut(chain_idx) else {
            bail!("SET_CHAIN: chain_idx {chain_idx} out of range");
        };
        let stored = match param {
            "mute_dry" => {
                let v = value.try_bool().map_err(|e| anyhow::anyhow!(e))?;
                if chain.mute_dry == v { return Ok(self.snapshot.state); }  // idempotent
                chain.mute_dry = v;
                ParamValue::Bool(v)
            },
            "dry_effective" => bail!("dry_effective is master-derived; cannot SET"),
            other => bail!("SET_CHAIN: unknown chain param '{other}'"),
        };
        if self.snapshot.set_state(Dirty) {
            self.notify_state_changed(source);
        }
        // Echo the user-set to other clients as PARAM_CHAIN (originator
        // filtered by source). `mute_dry` isn't pushed to audio — audio
        // only cares about `dry_effective`, which the recompute below
        // produces.
        self.bus.send(ControlMessage::SetChainParam {
            chain_idx, param: param.to_string(), value: stored, source: source.to_string(),
        }).ok();
        // Recompute `dry_effective` (this chain at least) and broadcast diffs.
        self.recompute_dry_effective_all();
        Ok(self.snapshot.state)
    }

    /// When using Effectance with an analog dry signal, we need to suppress
    /// the digital dry to avoid doubling it. Suppressing the dry signal in
    /// the final output must be done by subtracting the original dry signal
    /// from the result. This can only be done if the dry signal remains
    /// unchanged through the chain. For example: distortion will replace
    /// the complete signal, eq will change the phase. These effects need
    /// dry to flow digitally.
    ///
    /// For each chain, compute `dry_effective` and push audio + bus updates
    /// when it differs from the current value.
    ///
    /// Rule: `dry_effective = !mute_dry || any_active_needs_dry`.
    /// - `mute_dry=false` (default): digital dry always on, no override needed.
    /// - `mute_dry=true`, no active `needs_dry` effect: allow the mute,
    ///   `dry_effective=false`; `Chain::process` subtracts dry at output.
    /// - `mute_dry=true`, any active `needs_dry` effect: override the mute,
    ///   `dry_effective=true`; digital dry stays in the chain output (the
    ///   user's hardware-LED indicator stays lit, reflecting actual state).
    ///
    /// Call after: preset load, chain reshape, ApplySet (any active flag),
    /// SET_CHAIN (mute_dry), info-override (active aspect), Type-override
    /// save, COMPARE toggle.
    fn recompute_dry_effective_all(&mut self) {
        let mut updates: Vec<(usize, bool)> = Vec::new();
        for (idx, chain) in self.snapshot.preset.chains.iter().enumerate() {
            let any_active_needs_dry = chain.nodes.iter().any(|n| {
                // Effect's `active` lives in `params.active`; absent = canonical default (true).
                let active = n.params.get("active")
                    .and_then(|v| if let ParamValue::Bool(b) = v { Some(*b) } else { None })
                    .unwrap_or(true);
                active && crate::effects::registry::lookup(&n.device_type)
                    .is_some_and(|r| r.needs_dry)
            });
            let effective = !chain.mute_dry || any_active_needs_dry;
            if chain.dry_effective != effective {
                updates.push((idx, effective));
            }
        }
        for (idx, value) in updates {
            self.update_chain_dry_effective(idx, value);
        }
    }

    /// Master-driven update of a chain's `dry_effective` — writes the
    /// snapshot, pushes to audio so `Chain::process` flips its dry-subtract
    /// next buffer, and broadcasts as `LiveChainParam` (wire `LIVE_CHAIN`)
    /// for the future analog signal-relay daemon. Does **not** mark dirty:
    /// derived state, not a user edit.
    fn update_chain_dry_effective(&mut self, chain_idx: usize, value: bool) {
        if let Some(chain) = self.snapshot.preset.chains.get_mut(chain_idx) {
            chain.dry_effective = value;
        }
        let pv = ParamValue::Bool(value);
        // Push to audio so `Chain::process` uses the new dry-subtract decision.
        if let Err(e) = self.audio.push_control(ControlMessage::SetChainParam {
            chain_idx, param: "dry_effective".to_string(),
            value: pv, source: "master".to_string(),
        }) {
            warn!("chain dry_effective push to audio failed: {e}");
        }
        // Bus broadcast — LIVE_CHAIN, no dirty mark on any subscriber.
        self.bus.send(ControlMessage::LiveChainParam {
            chain_idx, param: "dry_effective".to_string(), value: pv,
        }).ok();
    }

    /// Push a StateChanged event on the bus (Clean / Dirty / Comparing).
    /// `source` filters out the originator.
    fn notify_state_changed(&self, source: &str) {
        self.bus.send(ControlMessage::StateChanged {
            state: self.snapshot.state.label().to_string(),
            source: source.to_string(),
        }).ok();
    }

    /// Push a PresetIndices event on the bus (occupied preset slots).
    /// Fired when the slot list actually changes (save to empty slot, delete).
    /// `source` filters out the originator.
    fn notify_preset_indices(&self, source: &str) {
        self.bus.send(ControlMessage::PresetIndices {
            indices: self.snapshot.preset_indices.clone(),
            source: source.to_string(),
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

/// Clamp inbound Type-overrides against canonical bounds before storage, and
/// drop entries whose post-clamp value matches canonical (sparse-storage
/// convention). Makes `cfg.type_overrides` round-trip cleanly through
/// FETCH_CONFIG / SAVE_CONFIG — the stored map is the resolved view, not the
/// user's raw input. Unknown effect types and unknown params are dropped
/// (warnings already fire from `apply_override`).
fn sanitize_type_overrides(map: &mut HashMap<String, OverrideMap>) {
    let canon = canonical_map();
    map.retain(|effect_type, om| {
        let Some(canonical) = canon.get(effect_type) else {
            warn!("type_overrides: unknown effect type '{effect_type}' — dropping");
            return false;
        };
        sanitize_one(canonical, om);
        !om.is_empty()
    });
}

/// Per-effect sanitiser: clamp every entry by replaying it through
/// `apply_override` against canonical, then re-extract the post-clamp value
/// out of `resolved`. Entries that collapse to canonical are dropped.
fn sanitize_one(canonical: &[ParamInfo], om: &mut OverrideMap) {
    let mut resolved = canonical.to_vec();
    for (target, value) in om.iter() {
        apply_override(&mut resolved, canonical, target, value);
    }
    let targets: Vec<MetaTarget> = om.keys().cloned().collect();
    for target in targets {
        // Find the entry the override touches by name — ParamMeta (whose
        // aspect field gets edited) or ActionsGroup (visible/active only).
        let Some(idx) = resolved.iter()
            .position(|i| i.name == target.param && match i.kind {
                ParamKind::ParamMeta { .. } | ParamKind::ActionsGroup => true,
                ParamKind::BoundMeta { .. } | ParamKind::Event { .. } => false,
            })
        else {
            om.remove(&target);
            continue;
        };
        let Some(clamped) = aspect_value(&resolved[idx], target.aspect) else {
            om.remove(&target);
            continue;
        };
        if aspect_value(&canonical[idx], target.aspect) == Some(clamped) {
            om.remove(&target);
        } else {
            om.insert(target, clamped);
        }
    }
}

/// Snapshot of every effect type's canonical `ParamInfo` list. Single source
/// of truth lives in each effect module's `pub static CANONICAL: [_; N]`; this
/// just gathers them into one map keyed by `device_type` for the Type-overrides
/// editor.
fn canonical_map() -> HashMap<String, Vec<ParamInfo>> {
    crate::effects::registry::REGISTRY.iter()
        .map(|r| (r.name.to_string(), r.canonical.to_vec()))
        .collect()
}

/// Extract all editable aspects of a `ParamInfo` as `(aspect, value)` pairs.
/// Used to diff before/after states around `apply_override` and broadcast
/// every aspect that actually changed (the user's direct edit plus any
/// cascades — default auto-clamp on min/max change, etc.).
fn extract_aspects(info: &ParamInfo) -> Vec<(MetaAspect, ParamValue)> {
    use crate::engine::device::ParamType;
    let mut out = vec![
        (MetaAspect::Visible, ParamValue::Bool(info.visible)),
        (MetaAspect::Active,  ParamValue::Bool(info.active)),
    ];
    match info.data_kind {
        ParamType::ContinuousFloat { min, max, default, log, .. } => {
            out.push((MetaAspect::Min,     ParamValue::Float(min)));
            out.push((MetaAspect::Max,     ParamValue::Float(max)));
            out.push((MetaAspect::Default, ParamValue::Float(default)));
            out.push((MetaAspect::Log,     ParamValue::Bool(log)));
        },
        ParamType::ContinuousInt { min, max, default, .. } => {
            out.push((MetaAspect::Min,     ParamValue::Int(min)));
            out.push((MetaAspect::Max,     ParamValue::Int(max)));
            out.push((MetaAspect::Default, ParamValue::Int(default)));
        },
        ParamType::DiscreteBool { default, .. } => {
            out.push((MetaAspect::Default, ParamValue::Bool(default)));
        },
        // DiscreteFloat / Event: no editable bound aspects today.
        _ => {},
    }
    out
}
