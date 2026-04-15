use axum::{
    Router,
    extract::{Path, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::Response,
    routing::{delete, get, post, put},
    Json,
};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot,};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::{debug, info, warn};
use crate::config::master::{ConfigRequest, snd_request};
use crate::control::{ControlMessage, EventBus};
use crate::control::mapping::{ControllerDef, DeviceDef};


pub type RespJson = Json<serde_json::Value>;

#[derive(Clone)]
pub struct AppState {
    pub master_tx:   mpsc::Sender<ConfigRequest>,
    pub bus:         EventBus,
}

impl AppState {
    /// HTTP wrapper around request() — returns (StatusCode, Json).
    async fn ask_master<T, F>(&self, build: F) -> (StatusCode, RespJson)
    where
        T: serde::Serialize,
        F: FnOnce(oneshot::Sender<Result<T>>) -> ConfigRequest,
    {
        match snd_request(&self.master_tx, build).await {
            Ok(val) => (StatusCode::OK, Json(serde_json::to_value(val).unwrap_or_default())),
            Err(e)  => {
                let full = format!("{e:#}");
                warn!("{full}");
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": full })))
            }
        }
    }
}
pub fn run(http_port: u16, master_tx: mpsc::Sender<ConfigRequest>, bus: EventBus)  {
    let http_state = AppState {
        master_tx,
        bus,
    };
    let router = router(http_state, "ui/dist");
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], http_port));
    info!("HTTP server on http://0.0.0.0:{http_port}");
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, router).await.unwrap();
    });
}

pub fn router(state: AppState, ui_dist_path: &str) -> Router {
    Router::new()
        .route("/api/state",           get(get_state))
        .route("/api/compare",         post(post_compare))
        .route("/api/config",          get(get_config))
        .route("/api/config",          post(post_config))
        .route("/api/reload",          post(post_reload))
        .route("/api/set",             post(post_set))
        .route("/api/action",          post(post_action))
        .route("/api/chains",          post(post_chains))
        .route("/api/preset/:n",       post(post_preset))
        .route("/api/preset/:n/save",  post(post_save_preset))
        .route("/api/preset/:n",       delete(delete_preset))
        .route("/api/devices",              get(get_devices))
        .route("/api/devices/:alias",       put(put_device).delete(delete_device))
        .route("/api/devices/:alias/rename", post(post_rename_device))
        .route("/api/controllers", put(put_controllers))
        .route("/ws",                  get(ws_handler))
        .nest_service("/", ServeDir::new(ui_dist_path))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Read handlers (use snapshot or master request)
// ---------------------------------------------------------------------------

async fn get_state(State(s): State<AppState>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::GetSnapshot { resp: tx }).await
}

async fn get_config(State(s): State<AppState>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::GetConfig { resp: tx }).await
}

// ---------------------------------------------------------------------------
// Config mutation
// ---------------------------------------------------------------------------

#[derive(Deserialize, serde::Serialize)]
struct ConfigBody {
    sample_rate:        Option<u32>,
    buffer_size:        Option<usize>,
    device:             Option<String>,
    in_channels:        Option<u16>,
    out_channels:       Option<u16>,
    delay_max_seconds:  Option<f32>,
}

async fn post_config(State(s): State<AppState>, Json(body): Json<ConfigBody>) -> (StatusCode, RespJson)  {
    let body_val = serde_json::to_value(&body).unwrap_or_default();
    s.ask_master(|tx| ConfigRequest::UpdateConfig { body: body_val, resp: Some(tx) }).await
}

// ---------------------------------------------------------------------------
// Set / Action / Compare — fast path via bus
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SetBody {
    path:  String,
    value: serde_json::Value,
}

async fn post_set(State(s): State<AppState>, Json(body): Json<SetBody>) -> (StatusCode, RespJson) {
    let value: f32 = match &body.value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        serde_json::Value::Bool(b)   => if *b { 1.0 } else { 0.0 },
        _ => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "Unknown value type" }))),
    };
    s.ask_master(|tx| ConfigRequest::ApplySet { path: body.path, value, source: "http".into(), resp: Some(tx) }).await
}

#[derive(Deserialize)]
struct ActionBody {
    target: String,
    action: String,
}

async fn post_action(State(s): State<AppState>, Json(body): Json<ActionBody>) -> StatusCode {
    s.bus.send(ControlMessage::Action { path: body.target, action: body.action, source: "http".into() }).ok();
    StatusCode::OK
}

async fn post_compare(State(s): State<AppState>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::ToggleCompare { resp: Some(tx) }).await
}

// ---------------------------------------------------------------------------
// Chains
// ---------------------------------------------------------------------------

async fn post_chains(State(s): State<AppState>, Json(body): RespJson) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::SetChains { json: body.to_string(), resp: Some(tx) }).await
}

// ---------------------------------------------------------------------------
// Reload
// ---------------------------------------------------------------------------

async fn post_reload(State(s): State<AppState>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::Reload { resp: Some(tx) }).await
}

// ---------------------------------------------------------------------------
// Preset switch / save / delete
// ---------------------------------------------------------------------------

async fn post_preset(State(s): State<AppState>, Path(slot): Path<u8>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::SwitchPreset { slot, resp: Some(tx) }).await
}

async fn post_save_preset(State(s): State<AppState>, Path(slot): Path<u8>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::SavePreset { slot, resp: Some(tx) }).await
}

async fn delete_preset(State(s): State<AppState>, Path(slot): Path<u8>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::DeletePreset { slot, resp: Some(tx) }).await
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_ws(socket, s))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let mut bus_rx = state.bus.subscribe();
    let master_tx = state.master_tx.clone();
    let source = crate::control::connection_id("ws");

    // Push initial snapshot so the UI can render immediately.
    if let Ok(snap) = snd_request(&state.master_tx, |tx| ConfigRequest::GetSnapshot { resp: tx }).await {
        let preset = serde_json::to_value(&snap.preset).unwrap_or_default();
        let j = serde_json::json!({
            "type": "preset",
            "preset": preset,
            "preset_indices": snap.preset_indices,
            "state": snap.state.label(),
        });
        if sink.send(Message::Text(j.to_string().into())).await.is_err() {
            warn!("WS client disconnected before initial snapshot");
            return;
        }
    }

    // Outbound: bus → WS client (NO filtering — UI must see all messages)
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = bus_rx.recv().await {
            let json: Option<serde_json::Value> = match msg {
                ControlMessage::SetParam { path, value, .. } =>
                    Some(serde_json::json!({ "type": "set", "path": path, "value": value })),
                ControlMessage::ProgramChange { .. } => None,
                ControlMessage::Reset { .. } =>
                    Some(serde_json::json!({ "type": "reset" })),
                ControlMessage::NodeEvent { key, event, data } =>
                    Some(serde_json::json!({ "type": "node_event", "key": key, "event": event, "data": data })),
                ControlMessage::PresetLoaded { preset, preset_indices, state } =>
                    Some(serde_json::json!({ "type": "preset", "preset": preset, "preset_indices": preset_indices, "state": state })),
                ControlMessage::StateChanged { state, preset_index, preset_indices } =>
                    Some(serde_json::json!({ "type": "state", "state": state, "preset_index": preset_index, "preset_indices": preset_indices })),
                _ => None,
            };
            if let Some(j) = json {
                if sink.send(Message::Text(j.to_string().into())).await.is_err() { break; }
            }
        }
    });

    // Inbound: WS client → bus
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(text) = msg {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                match v["type"].as_str() {
                    Some("set") => {
                        if let (Some(path), Some(value)) = (v["path"].as_str(), v["value"].as_f64()) {
                            debug!("WS SET {path} {value}");
                            state.bus.send(ControlMessage::SetParam {
                                path: path.to_string(), value: value as f32, source: source.clone(),
                            }).ok();
                        }
                    }
                    Some("preset") => {
                        if let Some(n) = v["n"].as_u64() {
                            let slot = n as u8;
                            master_tx.send(ConfigRequest::SwitchPreset { slot, resp: None }).await.ok();
                            state.bus.send(ControlMessage::ProgramChange { slot, source: source.clone() }).ok();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    send_task.abort();
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

async fn get_devices(State(s): State<AppState>) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::GetDevices { resp: tx }).await
}

async fn put_device(
    State(s): State<AppState>,
    Path(alias): Path<String>,
    Json(def): Json<DeviceDef>
) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::PutDevice { alias, def, resp: Some(tx) }).await
}

async fn delete_device(
    State(s): State<AppState>,
    Path(alias): Path<String>
) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::DeleteDevice { alias, resp: Some(tx) }).await
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
) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::RenameDevice {
        old_alias, new_alias: body.new_alias, def: body.def, resp: Some(tx)
    }).await
}

// ---------------------------------------------------------------------------
// Controllers
// ---------------------------------------------------------------------------

async fn put_controllers(
    State(s): State<AppState>,
    Json(body): Json<Vec<ControllerDef>>,
) -> (StatusCode, RespJson) {
    s.ask_master(|tx| ConfigRequest::UpdateControllers {
        controllers: body, resp: Some(tx) 
    }).await
}
