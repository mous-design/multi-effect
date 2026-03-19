use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use axum::{
    Router,
    extract::{Path, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Json,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::debug;

use crate::config::{BuildConfig, Config};
use crate::control::{ControlMessage, EventBus};
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
}

pub fn router(state: AppState, ui_dist_path: &str) -> Router {
    Router::new()
        .route("/api/state",           get(get_state))
        .route("/api/config",          get(get_config))
        .route("/api/config",          post(post_config))
        .route("/api/reload",          post(post_reload))
        .route("/api/presets",         get(get_presets))
        .route("/api/set",             post(post_set))
        .route("/api/patch",           post(post_patch))
        .route("/api/preset/:n",       post(post_preset))
        .route("/api/preset/:n/save",  post(post_save_preset))
        .route("/ws",                  get(ws_handler))
        .nest_service("/", ServeDir::new(ui_dist_path.to_string()))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn get_state(State(s): State<AppState>) -> Json<serde_json::Value> {
    let ps = s.patch_state.lock().unwrap();
    Json(ps.json.clone())
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
    sample_rate:  Option<u32>,
    buffer_size:  Option<usize>,
    device:       Option<String>,
    in_channels:  Option<u16>,
    out_channels: Option<u16>,
}

async fn post_config(State(s): State<AppState>, Json(body): Json<ConfigBody>) -> StatusCode {
    let mut cfg = s.cfg.lock().unwrap();
    if let Some(v) = body.sample_rate  { cfg.sample_rate  = v; }
    if let Some(v) = body.buffer_size  { cfg.buffer_size  = v; }
    if let Some(v) = body.device       { cfg.device       = v; }
    if let Some(v) = body.in_channels  { cfg.in_channels  = v; }
    if let Some(v) = body.out_channels { cfg.out_channels = v; }
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

async fn post_set(State(s): State<AppState>, Json(body): Json<SetBody>) -> StatusCode {
    let value: f32 = match &body.value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        serde_json::Value::Bool(b)   => if *b { 1.0 } else { 0.0 },
        _ => return StatusCode::BAD_REQUEST,
    };
    s.bus.send(ControlMessage::SetParam { path: body.path, value }).ok();
    StatusCode::OK
}

async fn post_patch(State(s): State<AppState>, Json(body): Json<serde_json::Value>) -> StatusCode {
    match crate::engine::patch::load_str(&body.to_string(), &s.build_cfg) {
        Err(e) => {
            tracing::warn!("PATCH error: {e}");
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Ok(chains) => {
            let json = crate::engine::patch::chains_to_json(&chains);
            if s.patch_tx.lock().unwrap().push(chains).is_err() {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            s.patch_state.lock().unwrap().apply_patch(json);
            StatusCode::OK
        }
    }
}

async fn post_reload(State(s): State<AppState>) -> StatusCode {
    s.reload_notify.notify_one();
    StatusCode::OK
}

async fn post_preset(State(s): State<AppState>, Path(n): Path<u8>) -> StatusCode {
    {
        let mut cfg = s.cfg.lock().unwrap();
        if !cfg.presets.contains_key(&n) {
            return StatusCode::NOT_FOUND;
        }
        cfg.active_preset = n;
        if let Err(e) = cfg.save_to_disk() {
            tracing::warn!("save active_preset failed: {e}");
        }
    }
    s.bus.send(ControlMessage::ProgramChange(n)).ok();
    StatusCode::OK
}

async fn post_save_preset(State(s): State<AppState>, Path(n): Path<u8>) -> StatusCode {
    let chains = {
        let ps = s.patch_state.lock().unwrap();
        ps.json["chains"].clone()
    };
    let mut cfg = s.cfg.lock().unwrap();
    cfg.active_preset = n;
    if let Err(e) = cfg.save_preset(n, chains) {
        tracing::warn!("save_preset {n} failed: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

async fn ws_handler(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_ws(socket, s))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let mut bus_rx = state.bus.subscribe();

    let send_task = tokio::spawn(async move {
        while let Ok(msg) = bus_rx.recv().await {
            let json: Option<serde_json::Value> = match msg {
                ControlMessage::SetParam { path, value } =>
                    Some(serde_json::json!({ "type": "set", "path": path, "value": value })),
                ControlMessage::ProgramChange(n) =>
                    Some(serde_json::json!({ "type": "preset", "n": n })),
                ControlMessage::Reset =>
                    Some(serde_json::json!({ "type": "reset" })),
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
