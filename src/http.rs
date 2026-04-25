use axum::{
    Router,
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::Response,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::{mpsc, watch};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::{info, warn};
use super::config::master::{ConfigRequest, snd_request};
use super::control::EventBus;
use super::control::handle::handle_client;

#[derive(Clone)]
pub struct AppState {
    pub master_tx:   mpsc::Sender<ConfigRequest>,
    pub bus:         EventBus,
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
        .route("/ws",                  get(ws_handler))
        .nest_service("/", ServeDir::new(ui_dist_path))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_ws(socket, s))
}

/// WebSocket handler: adapts the socket into an `AsyncRead + AsyncWrite` byte
/// stream and hands off to the universal `handle_client`. Same text protocol as
/// serial / TCP — one code path for all transports.
async fn handle_ws(mut socket: WebSocket, state: AppState) {
    // Initial snapshot: push the current preset + state as the UI's first frames,
    // so the UI can render immediately without waiting for the next bus event.
    if let Ok(snap) = snd_request(&state.master_tx, |tx| ConfigRequest::GetSnapshot { resp: tx }).await {
        let initial = format!("SNAPSHOT {}\n", serde_json::to_string(&snap.to_view()).unwrap_or_default());
        if socket.send(Message::Text(initial.into())).await.is_err() {
            warn!("WS client disconnected before initial snapshot");
            return;
        }
    }

    // UI has no "deactivation" concept — this channel is effectively unused.
    let (_active_tx, active_rx) = watch::channel(true);

    // Bridge WS frames to a byte stream so handle_client sees a normal duplex I/O.
    let stream = ws_adapter(socket);
    if let Err(e) = handle_client(stream, state.bus, state.master_tx, "ws", active_rx).await {
        warn!("WS: {e}");
    }
}

/// Bridge a WebSocket to an `AsyncRead + AsyncWrite` byte stream.
///
/// - Each inbound WS text frame becomes `<bytes>\n` on the read side.
/// - Each outbound `\n`-terminated line becomes one outbound text frame.
/// - `Close` ends the bridge; binary / ping / pong are ignored (axum auto-
///   responds to pings at the transport layer).
fn ws_adapter(ws: WebSocket) -> DuplexStream {
    let (ours, theirs) = tokio::io::duplex(4096);
    let (their_rd, mut their_wr) = tokio::io::split(theirs);
    let (mut sink, mut stream) = ws.split();

    tokio::spawn(async move {
        let inbound = async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if their_wr.write_all(text.as_bytes()).await.is_err() { break; }
                        if their_wr.write_all(b"\n").await.is_err() { break; }
                    },
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {} // binary / ping / pong — ignore
                }
            }
        };
        let outbound = async {
            let mut lines = BufReader::new(their_rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if sink.send(Message::Text(line.into())).await.is_err() { break; }
            }
        };
        tokio::select! {
            _ = inbound  => {}
            _ = outbound => {}
        }
    });

    ours
}

