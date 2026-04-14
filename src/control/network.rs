use std::sync::{Arc, RwLock};

use anyhow::{Result, Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::master::ConfigRequest;
use crate::control::{apply_ctrl, connection_id, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::engine::patch;

/// Text-based TCP control server.
///
/// One command per line (UTF-8).  Responds with `OK` or `ERR <reason>`.
///
/// # Commands
///
/// ```text
/// SET  <key.param> <value>     — set a single parameter, e.g. SET 04-reverb.wet 0.6
/// CTRL <channel_id> <value>    — mapped control (same as serial CTRL)
/// UPDATE <json>                — partial patch update
/// CHAINS <json>                — replace chain structure (full chain array)
/// RESET                        — reset all effect state
/// PROGRAM <0-127>              — load preset number
/// SAVE_PRESET <0-127>          — save current chains to preset slot in config.json
/// ```
///
/// All connected clients also receive outbound events from the bus:
/// `CTRL <channel_id> <raw>` for mapped params, `SET <key.param> <value>` otherwise.
///
/// Multiple clients per port are handled concurrently via `tokio::spawn`.
pub struct NetworkControl {
    alias:       String,
    host:        String,
    port:        u16,
    fallback:    bool,
    bus:         EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
    master_tx:   mpsc::Sender<ConfigRequest>,
}

impl NetworkControl {
    pub fn new(
        alias:     String,
        host:      String,
        port:      u16,
        fallback:  bool,
        bus:       EventBus,
        mappings:  Arc<RwLock<ControllerDef>>,
        master_tx: mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { alias, host, port, fallback, bus, mappings, master_tx }
    }

    pub async fn run(
        self,
        mut active_rx: watch::Receiver<bool>,
    ) -> Result<()> {
        if !*active_rx.borrow() { return Ok(()); }

        let listener = TcpListener::bind((self.host.as_str(), self.port)).await?;
        tracing::info!("Control server listening on {}:{}", self.host, self.port);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (socket, addr) = result?;
                    tracing::info!("Control connection from {addr}");

                    let bus       = self.bus.clone();
                    let mappings  = Arc::clone(&self.mappings);
                    let fallback  = self.fallback;
                    let master_tx = self.master_tx.clone();
                    let alias     = self.alias.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_client(socket, bus, mappings, fallback, master_tx, &alias).await {
                            tracing::warn!("Client {addr}: {e}");
                        }
                    });
                }
                _ = active_rx.changed() => {
                    if !*active_rx.borrow() {
                        tracing::info!("Net control on :{} deactivated", self.port);
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn handle_client(
    socket:    TcpStream,
    bus:       EventBus,
    mappings:  Arc<RwLock<ControllerDef>>,
    fallback:  bool,
    master_tx: mpsc::Sender<ConfigRequest>,
    alias:     &str,
) -> Result<()> {
    let source = connection_id(alias);
    let (reader, writer) = socket.into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    // Outbound: forward bus events to this client (mapped where possible)
    let mut bus_rx    = bus.subscribe();
    let writer_out    = Arc::clone(&writer);
    let mappings_out  = Arc::clone(&mappings);
    let source_out    = source.clone();
    tokio::spawn(async move {
        while let Ok(msg) = bus_rx.recv().await {
            // Skip messages originating from this connection
            if msg.source() == source_out { continue; }

            let line = match &msg {
                ControlMessage::SetParam { path, value, .. } => {
                    let cfg = mappings_out.read().unwrap();
                    outbound_line(path, *value, &cfg)
                }
                ControlMessage::ProgramChange { slot, .. } => format!("PROGRAM {slot}\n"),
                ControlMessage::Reset { .. }               => "RESET\n".to_string(),
                ControlMessage::NoteOn          { .. }
                | ControlMessage::NoteOff       { .. }
                | ControlMessage::Action        { .. }
                | ControlMessage::NodeEvent     { .. }
                => continue,
                ControlMessage::PresetLoaded { ref preset, state: ref s, .. }
                => format!("PRESET {}\nSTATE {s}\n", serde_json::to_string(preset).unwrap_or_default()),
                ControlMessage::StateChanged { ref state, preset_index, .. }
                => format!("STATE {state} {preset_index}\n"),
            };
            if writer_out.lock().await.write_all(line.as_bytes()).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // Inbound: read commands, respond with OK/ERR
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }

        let response = if line.starts_with("CTRL ") {
            let cfg = mappings.read().unwrap();
            apply_ctrl(&line, &cfg, fallback, &bus, &source);
            "OK\n".to_string()
        } else {
            match handle_command(&line, &bus, &master_tx, &source).await {
                Ok(())  => "OK\n".to_string(),
                Err(e)  => format!("ERR {e}\n"),
            }
        };
        writer.lock().await.write_all(response.as_bytes()).await?;
    }

    Ok(())
}

pub(crate) async fn handle_command(
    line:      &str,
    bus:       &EventBus,
    master_tx: &mpsc::Sender<ConfigRequest>,
    source:    &str,
) -> Result<()> {
    let (cmd, rest) = split_cmd(line);
    let src = source.to_string();

    match cmd {
        // ------------------------------------------------------------------
        // SET <key.param> <float>
        // ------------------------------------------------------------------
        "SET" => {
            let (path, val_str) = rest
                .split_once(' ')
                .context("usage: SET <key.param> <value>")?;
            let val_str = val_str.trim();
            if let Ok(value) = val_str.parse::<f32>() {
                bus.send(ControlMessage::SetParam { path: path.to_string(), value, source: src }).ok();
            } else {
                // Non-numeric value → action dispatch (e.g. "SET 01-looper.action rec")
                bus.send(ControlMessage::Action {
                    path:   path.to_string(),
                    action: val_str.to_string(),
                    source: src,
                }).ok();
            }
            Ok(())
        }

        // ------------------------------------------------------------------
        // UPDATE <json-object>
        // ------------------------------------------------------------------
        "UPDATE" => {
            let v: Value = serde_json::from_str(rest)?;
            let pairs = patch::flatten_update(&v);
            if pairs.is_empty() {
                bail!("no numeric values found in update object");
            }
            for (path, value) in &pairs {
                bus.send(ControlMessage::SetParam { path: path.clone(), value: *value, source: src.clone() }).ok();
            }
            Ok(())
        }

        // ------------------------------------------------------------------
        // CHAINS <json>
        // ------------------------------------------------------------------
        "CHAINS" => {
            let (tx, rx) = oneshot::channel();
            master_tx.send(ConfigRequest::SetChains {
                json: rest.to_string(),
                resp: Some(tx),
            }).await?;
            rx.await?
        }

        // ------------------------------------------------------------------
        "RESET" => {
            bus.send(ControlMessage::Reset { source: src }).ok();
            Ok(())
        }

        "PROGRAM" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("program number must be 0-127")?;
            let (tx, rx) = oneshot::channel();
            master_tx.send(ConfigRequest::SwitchPreset { slot, resp: Some(tx) })
                .await?;
            bus.send(ControlMessage::ProgramChange { slot, source: src }).ok();
            rx.await?
        }

        // ------------------------------------------------------------------
        // SAVE_PRESET <slot>
        // ------------------------------------------------------------------
        "SAVE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("slot must be 0-127")?;
            let (tx, rx) = oneshot::channel();
            master_tx.send(ConfigRequest::SavePreset { slot, resp: Some(tx) }).await?;
            rx.await?
        }

        "COMPARE" => {
            let (tx, rx) = oneshot::channel();
            master_tx.send(ConfigRequest::ToggleCompare { resp: Some(tx) })
                .await?;
            rx.await?
        }

        other => bail!("unknown command '{other}'"),
    }
}

pub(crate) fn split_cmd(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (line, ""),
    }
}
