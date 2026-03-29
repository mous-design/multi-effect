use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{watch, Notify};
use axum::{
    Router,
    extract::{Path, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::Response,
    routing::{delete, get, post, put},
    Json,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::debug;

use crate::config::{BuildConfig, Config};
use crate::control::mapping::{ControllerDef, DeviceDef};
use crate::control::{ControlMessage, EventBus, NetworkControl, SerialControl};
use crate::control::midi::{MidiControl, MidiOutControl};
use crate::engine::patch::Chain;
use crate::save::PatchState;

#[derive(Clone)]
pub struct AppState {
    pub patch_state:    Arc<Mutex<PatchState>>,
    pub patch_tx:       Arc<Mutex<rtrb::Producer<Vec<Chain>>>>,
    pub build_cfg:      BuildConfig,
    pub bus:            EventBus,
    pub cfg:            Arc<Mutex<Config>>,
    pub reload_notify:  Arc<Notify>,
    /// Per-device active-state watch senders. Send `false` to stop the device task.
    /// Wrapped in Mutex so new senders can be added when a device is (re-)activated.
    pub device_active:   Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    /// Per-device controller-mapping Arcs (shared with preset-switch task).
    pub controller_arcs: Arc<HashMap<String, Arc<RwLock<ControllerDef>>>>,
    /// Live state snapshot from the audio engine, updated every ~100 blocks.
    pub live_state:      Arc<std::sync::Mutex<serde_json::Value>>,
    /// True when params have been changed since the last save.  Survives browser reloads.
    pub preset_dirty:     Arc<AtomicBool>,
    /// Snapshot of dirty chains while in compare mode.
    pub compare_snapshot: Arc<Mutex<Option<serde_json::Value>>>,
    /// True while the user is listening to the saved preset for comparison.
    pub is_comparing:     Arc<AtomicBool>,
}

pub fn router(state: AppState, ui_dist_path: &str) -> Router {
    Router::new()
        .route("/api/state",           get(get_state))
        .route("/api/compare",         post(post_compare))
        .route("/api/config",          get(get_config))
        .route("/api/config",          post(post_config))
        .route("/api/reload",          post(post_reload))
        .route("/api/presets",         get(get_presets))
        .route("/api/set",             post(post_set))
        .route("/api/action",          post(post_action))
        .route("/api/patch",           post(post_patch))
        .route("/api/preset/:n",       post(post_preset))
        .route("/api/preset/:n/save",  post(post_save_preset))
        .route("/api/preset/:n",       delete(delete_preset))
        .route("/api/devices",              get(get_devices))
        .route("/api/devices/:alias",       put(put_device).delete(delete_device))
        .route("/api/devices/:alias/rename", post(post_rename_device))
        .route("/api/preset/:n/controllers", get(get_controllers).put(put_controllers))
        .route("/ws",                  get(ws_handler))
        .nest_service("/", ServeDir::new(ui_dist_path.to_string()))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn get_state(State(s): State<AppState>) -> Json<serde_json::Value> {
    // Prefer live_state — it contains current runtime values (looper state, pos, etc.).
    // Fall back to patch_state only if live_state is still null (startup edge case).
    let mut json = {
        let live = s.live_state.lock().unwrap();
        if !live.is_null() { live.clone() } else { s.patch_state.lock().unwrap().json.clone() }
    };
    json["is_dirty"]    = serde_json::json!(s.preset_dirty.load(Ordering::Relaxed));
    json["is_comparing"] = serde_json::json!(s.is_comparing.load(Ordering::Relaxed));
    Json(json)
}

async fn get_config(State(s): State<AppState>) -> Json<serde_json::Value> {
    let cfg = s.cfg.lock().unwrap();
    Json(serde_json::json!({
        "in_channels":        s.build_cfg.in_channels,
        "out_channels":       s.build_cfg.out_channels,
        "sample_rate":        s.build_cfg.sample_rate,
        "buffer_size":        cfg.buffer_size,
        "device":             cfg.device,
        "delay_max_seconds":  cfg.delay_max_seconds,
        "looper_max_seconds": cfg.looper_max_seconds,
    }))
}

async fn get_presets(State(s): State<AppState>) -> Json<serde_json::Value> {
    let cfg = s.cfg.lock().unwrap();
    let presets: Vec<u8> = cfg.presets.keys().cloned().collect();
    let active = cfg.active_preset;
    Json(serde_json::json!({ "presets": presets, "active": active }))
}

#[derive(Deserialize)]
struct ConfigBody {
    sample_rate:        Option<u32>,
    buffer_size:        Option<usize>,
    device:             Option<String>,
    in_channels:        Option<u16>,
    out_channels:       Option<u16>,
    delay_max_seconds:  Option<f32>,
}

async fn post_config(State(s): State<AppState>, Json(body): Json<ConfigBody>) -> StatusCode {
    let mut cfg = s.cfg.lock().unwrap();
    if let Some(v) = body.sample_rate       { cfg.sample_rate       = v; }
    if let Some(v) = body.buffer_size       { cfg.buffer_size       = v; }
    if let Some(v) = body.device            { cfg.device            = v; }
    if let Some(v) = body.in_channels       { cfg.in_channels       = v; }
    if let Some(v) = body.out_channels      { cfg.out_channels      = v; }
    if let Some(v) = body.delay_max_seconds { cfg.delay_max_seconds = v; }
    if let Err(e) = cfg.save_to_disk() {
        tracing::warn!("save config failed: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct SetBody {
    path:  String,
    value: serde_json::Value,
}

async fn post_compare(State(s): State<AppState>) -> StatusCode {
    s.bus.send(ControlMessage::Compare).ok();
    StatusCode::OK
}

async fn post_set(State(s): State<AppState>, Json(body): Json<SetBody>) -> StatusCode {
    let value: f32 = match &body.value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        serde_json::Value::Bool(b)   => if *b { 1.0 } else { 0.0 },
        _ => return StatusCode::BAD_REQUEST,
    };
    // Any edit while comparing exits compare mode (edit goes on top of current state).
    if s.is_comparing.swap(false, Ordering::Relaxed) {
        s.compare_snapshot.lock().unwrap().take();
        let chains = s.patch_state.lock().unwrap().json["chains"].clone();
        s.bus.send(ControlMessage::CompareChanged { chains, is_dirty: true, is_comparing: false }).ok();
    }
    s.preset_dirty.store(true, Ordering::Relaxed);
    s.bus.send(ControlMessage::SetParam { path: body.path, value }).ok();
    StatusCode::OK
}

#[derive(Deserialize)]
struct ActionBody {
    target: String,
    action: String,
}

async fn post_action(State(s): State<AppState>, Json(body): Json<ActionBody>) -> StatusCode {
    s.preset_dirty.store(true, Ordering::Relaxed);
    s.bus.send(ControlMessage::Action { path: body.target, action: body.action }).ok();
    StatusCode::OK
}

async fn post_patch(State(s): State<AppState>, Json(body): Json<serde_json::Value>) -> StatusCode {
    let build_cfg = {
        let cfg = s.cfg.lock().unwrap();
        crate::config::BuildConfig { delay_max_seconds: cfg.delay_max_seconds, ..s.build_cfg }
    };
    match crate::engine::patch::load_str(&body.to_string(), &build_cfg) {
        Err(e) => {
            tracing::warn!("PATCH error: {e}");
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Ok(mut chains) => {
            for chain in &mut chains { chain.init_bus(&s.bus); }
            let json = crate::engine::patch::chains_to_json(&chains);
            if s.patch_tx.lock().unwrap().push(chains).is_err() {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            s.patch_state.lock().unwrap().apply_patch(json);
            // Any structural edit while comparing exits compare mode.
            if s.is_comparing.swap(false, Ordering::Relaxed) {
                s.compare_snapshot.lock().unwrap().take();
                let c = s.patch_state.lock().unwrap().json["chains"].clone();
                s.bus.send(ControlMessage::CompareChanged { chains: c, is_dirty: true, is_comparing: false }).ok();
            }
            s.preset_dirty.store(true, Ordering::Relaxed);
            StatusCode::OK
        }
    }
}

async fn post_reload(State(s): State<AppState>) -> StatusCode {
    s.reload_notify.notify_one();
    StatusCode::OK
}

async fn post_preset(State(s): State<AppState>, Path(n): Path<u8>) -> StatusCode {
    // Update patch_state synchronously so GET /api/state immediately returns the new preset,
    // even before the async preset-switch task (triggered by ProgramChange) completes.
    let chains_json = {
        let mut cfg = s.cfg.lock().unwrap();
        if !cfg.presets.contains_key(&n) {
            return StatusCode::NOT_FOUND;
        }
        cfg.active_preset = n;
        if let Err(e) = cfg.save_to_disk() {
            tracing::warn!("save active_preset failed: {e}");
        }
        serde_json::json!({ "chains": cfg.presets[&n].chains })
    };
    s.patch_state.lock().unwrap().apply_patch(chains_json);
    s.preset_dirty.store(false, Ordering::Relaxed);
    // Preset switch always cancels compare mode.
    s.is_comparing.store(false, Ordering::Relaxed);
    s.compare_snapshot.lock().unwrap().take();
    s.bus.send(ControlMessage::ProgramChange(n)).ok();
    StatusCode::OK
}

async fn post_save_preset(State(s): State<AppState>, Path(n): Path<u8>) -> StatusCode {
    // Prefer live_state (reflects audio-thread reality); fall back to patch_state.
    // Strip transient looper fields — they cannot be restored from JSON.
    let mut chains = {
        let live = s.live_state.lock().unwrap();
        if !live.is_null() {
            live["chains"].clone()
        } else {
            s.patch_state.lock().unwrap().json["chains"].clone()
        }
    };
    // Saving while comparing: just discard the dirty snapshot and stay clean.
    // Current state IS the saved preset, no disk write needed.
    if s.is_comparing.swap(false, Ordering::Relaxed) {
        s.compare_snapshot.lock().unwrap().take();
        s.preset_dirty.store(false, Ordering::Relaxed);
        let chains = s.patch_state.lock().unwrap().json["chains"].clone();
        s.bus.send(ControlMessage::CompareChanged { chains, is_dirty: false, is_comparing: false }).ok();
        return StatusCode::OK;
    }
    crate::save::strip_looper_transient(&mut chains);
    let mut cfg = s.cfg.lock().unwrap();
    cfg.active_preset = n;
    if let Err(e) = cfg.save_preset(n, chains) {
        tracing::warn!("save_preset {n} failed: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    s.preset_dirty.store(false, Ordering::Relaxed);
    StatusCode::OK
}

async fn delete_preset(State(s): State<AppState>, Path(n): Path<u8>) -> StatusCode {
    let mut cfg = s.cfg.lock().unwrap();
    if !cfg.presets.contains_key(&n) { return StatusCode::NOT_FOUND; }
    cfg.presets.remove(&n);
    if cfg.save_to_disk().is_err() { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK }
}

async fn ws_handler(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_ws(socket, s))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let mut bus_rx = state.bus.subscribe();
    let state_send = state.clone(); // captured by send_task; state itself used in receive loop

    let send_task = tokio::spawn(async move {
        while let Ok(msg) = bus_rx.recv().await {
            let json: Option<serde_json::Value> = match msg {
                ControlMessage::SetParam { path, value } =>
                    Some(serde_json::json!({ "type": "set", "path": path, "value": value })),
                ControlMessage::ProgramChange(n) => {
                    // Include the new preset's chains (already updated synchronously in
                    // post_preset) so the client can apply state without a fetchState round trip.
                    let ps    = state_send.patch_state.lock().unwrap();
                    let dirty = state_send.preset_dirty.load(Ordering::Relaxed);
                    Some(serde_json::json!({
                        "type":     "preset",
                        "n":        n,
                        "chains":   ps.json["chains"],
                        "is_dirty": dirty,
                    }))
                }
                ControlMessage::Reset =>
                    Some(serde_json::json!({ "type": "reset" })),
                ControlMessage::NodeEvent { key, event, data } =>
                    Some(serde_json::json!({ "type": "node_event", "key": key, "event": event, "data": data })),
                ControlMessage::CompareChanged { chains, is_dirty, is_comparing } =>
                    Some(serde_json::json!({ "type": "compare", "chains": chains, "is_dirty": is_dirty, "is_comparing": is_comparing })),
                _ => None,
            };
            if let Some(j) = json {
                if sink.send(Message::Text(j.to_string().into())).await.is_err() { break; }
            }
        }
    });

    // Receive from client (SET commands)
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(text) = msg {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                match v["type"].as_str() {
                    Some("set") => {
                        if let (Some(path), Some(value)) = (v["path"].as_str(), v["value"].as_f64()) {
                            debug!("WS SET {path} {value}");
                            state.bus.send(ControlMessage::SetParam {
                                path: path.to_string(), value: value as f32,
                            }).ok();
                        }
                    }
                    Some("preset") => {
                        if let Some(n) = v["n"].as_u64() {
                            state.bus.send(ControlMessage::ProgramChange(n as u8)).ok();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    send_task.abort();
}

async fn get_devices(State(s): State<AppState>) -> Json<serde_json::Value> {
    let cfg = s.cfg.lock().unwrap();
    Json(serde_json::to_value(&cfg.devices).unwrap_or_default())
}

async fn put_device(
    State(s): State<AppState>,
    Path(alias): Path<String>,
    Json(def): Json<DeviceDef>,
) -> StatusCode {
    let was_active = s.cfg.lock().unwrap().devices.get(&alias).map(|d| d.is_active()).unwrap_or(false);
    let is_active  = def.is_active();
    {
        let mut cfg = s.cfg.lock().unwrap();
        cfg.devices.insert(alias.clone(), def.clone());
        if cfg.save_to_disk().is_err() { return StatusCode::INTERNAL_SERVER_ERROR; }
    }
    if !is_active {
        // Signal the running task to stop.
        if let Some(tx) = s.device_active.lock().unwrap().get(&alias) {
            let _ = tx.send(false);
        }
    } else if !was_active {
        // Device just activated: spawn a new task with a fresh watch channel.
        let (tx, rx) = watch::channel(true);
        s.device_active.lock().unwrap().insert(alias.clone(), tx);
        let mappings = s.controller_arcs.get(&alias)
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::new(RwLock::new(ControllerDef::default())));
        spawn_device_task(alias, def, rx, mappings, &s);
    }
    StatusCode::OK
}

fn spawn_device_task(
    alias:    String,
    def:      DeviceDef,
    active_rx: watch::Receiver<bool>,
    mappings: Arc<RwLock<ControllerDef>>,
    s:        &AppState,
) {
    let build_cfg   = s.build_cfg;
    let patch_state = Arc::clone(&s.patch_state);
    let cfg         = Arc::clone(&s.cfg);
    let bus         = s.bus.clone();
    let patch_tx    = Arc::clone(&s.patch_tx);

    match def {
        DeviceDef::Serial { dev, baud, fallback, .. } => {
            let serial = SerialControl::new(dev, baud, fallback, build_cfg, patch_state, cfg, bus, mappings);
            tokio::spawn(async move {
                if let Err(e) = serial.run(patch_tx, active_rx).await {
                    tracing::error!("Serial '{alias}': {e}");
                }
            });
        }
        DeviceDef::Net { host, port, fallback, .. } => {
            let net = NetworkControl::new(host, port, fallback, build_cfg, patch_state, cfg, bus, mappings);
            tokio::spawn(async move {
                if let Err(e) = net.run(patch_tx, active_rx).await {
                    tracing::error!("Net '{alias}': {e}");
                }
            });
        }
        DeviceDef::MidiIn { dev, channel, .. } => {
            let midi = MidiControl::new(dev, channel, mappings);
            midi.run(bus);
        }
        DeviceDef::MidiOut { dev, channel, .. } => {
            let midi_out = MidiOutControl::new(dev, channel, mappings);
            midi_out.run(bus);
        }
    }
}

async fn delete_device(
    State(s): State<AppState>,
    Path(alias): Path<String>,
) -> StatusCode {
    let mut cfg = s.cfg.lock().unwrap();
    cfg.devices.remove(&alias);
    // Remove all controller mappings that reference this device from every preset.
    for preset in cfg.presets.values_mut() {
        preset.controllers.retain(|c| c.device != alias);
    }
    if cfg.save_to_disk().is_err() { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK }
}

#[derive(Deserialize)]
struct RenameBody {
    new_alias: String,
    def: DeviceDef,
}

async fn post_rename_device(
    State(s): State<AppState>,
    Path(old_alias): Path<String>,
    Json(body): Json<RenameBody>,
) -> StatusCode {
    let new_alias = body.new_alias;
    if old_alias == new_alias {
        // Just update the def in place
        let mut cfg = s.cfg.lock().unwrap();
        cfg.devices.insert(old_alias, body.def);
        return if cfg.save_to_disk().is_err() { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK };
    }
    let mut cfg = s.cfg.lock().unwrap();
    if !cfg.devices.contains_key(&old_alias) { return StatusCode::NOT_FOUND; }
    // Move device def
    cfg.devices.remove(&old_alias);
    cfg.devices.insert(new_alias.clone(), body.def);
    // Update all controller references in every preset
    for preset in cfg.presets.values_mut() {
        for ctrl in preset.controllers.iter_mut() {
            if ctrl.device == old_alias {
                ctrl.device = new_alias.clone();
            }
        }
    }
    if cfg.save_to_disk().is_err() { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK }
}

async fn get_controllers(
    State(s): State<AppState>,
    Path(n): Path<u8>,
) -> (StatusCode, Json<serde_json::Value>) {
    let cfg = s.cfg.lock().unwrap();
    match cfg.presets.get(&n) {
        Some(preset) => (StatusCode::OK, Json(serde_json::to_value(&preset.controllers).unwrap_or(serde_json::json!([])))),
        None         => (StatusCode::NOT_FOUND, Json(serde_json::json!([]))),
    }
}

async fn put_controllers(
    State(s): State<AppState>,
    Path(n): Path<u8>,
    Json(body): Json<Vec<ControllerDef>>,
) -> StatusCode {
    let mut cfg = s.cfg.lock().unwrap();
    let preset = cfg.presets.entry(n).or_default();
    preset.controllers = body;
    if cfg.save_to_disk().is_err() { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK }
}
