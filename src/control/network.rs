use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use rtrb::Producer;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::config::{BuildConfig, Config};
use crate::control::{apply_ctrl, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::engine::patch::{self, Chain};
use crate::save::PatchState;

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
/// PATCH  <json>                — swap to a new patch (full chain array)
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
    host:        String,
    port:        u16,
    fallback:    bool,
    build_cfg:   BuildConfig,
    patch_state: Arc<Mutex<PatchState>>,
    cfg:         Arc<Mutex<Config>>,
    bus:         EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
}

impl NetworkControl {
    pub fn new(
        host:        String,
        port:        u16,
        fallback:    bool,
        build_cfg:   BuildConfig,
        patch_state: Arc<Mutex<PatchState>>,
        cfg:         Arc<Mutex<Config>>,
        bus:         EventBus,
        mappings:    Arc<RwLock<ControllerDef>>,
    ) -> Self {
        Self { host, port, fallback, build_cfg, patch_state, cfg, bus, mappings }
    }

    pub async fn run(
        self,
        patch_tx: Arc<Mutex<Producer<Vec<Chain>>>>,
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

                    let patch_tx    = Arc::clone(&patch_tx);
                    let build_cfg   = self.build_cfg;
                    let patch_state = Arc::clone(&self.patch_state);
                    let cfg         = Arc::clone(&self.cfg);
                    let bus         = self.bus.clone();
                    let mappings    = Arc::clone(&self.mappings);
                    let fallback    = self.fallback;

                    tokio::spawn(async move {
                        if let Err(e) = handle_client(socket, bus, patch_tx, build_cfg, patch_state, cfg, mappings, fallback).await {
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
    socket:      TcpStream,
    bus:         EventBus,
    patch_tx:    Arc<Mutex<Producer<Vec<Chain>>>>,
    build_cfg:   BuildConfig,
    patch_state: Arc<Mutex<PatchState>>,
    cfg:         Arc<Mutex<Config>>,
    mappings:    Arc<RwLock<ControllerDef>>,
    fallback:    bool,
) -> Result<()> {
    let (reader, writer) = socket.into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    // Outbound: forward bus events to this client (mapped where possible)
    let mut bus_rx    = bus.subscribe();
    let writer_out    = Arc::clone(&writer);
    let mappings_out  = Arc::clone(&mappings);
    tokio::spawn(async move {
        while let Ok(msg) = bus_rx.recv().await {
            let line = match &msg {
                ControlMessage::SetParam { path, value } => {
                    let cfg = mappings_out.read().unwrap();
                    outbound_line(path, *value, &cfg)
                }
                ControlMessage::ProgramChange(n) => format!("PROGRAM {n}\n"),
                ControlMessage::Reset            => "RESET\n".to_string(),
                ControlMessage::NoteOn       { .. }
                | ControlMessage::NoteOff    { .. }
                | ControlMessage::Action     { .. }
                | ControlMessage::NodeEvent { .. } => continue,
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
            apply_ctrl(&line, &cfg, fallback, &bus);
            "OK\n".to_string()
        } else {
            match handle_command(&line, &bus, &patch_tx, &build_cfg, &patch_state, &cfg) {
                Ok(())  => "OK\n".to_string(),
                Err(e)  => format!("ERR {e}\n"),
            }
        };
        writer.lock().await.write_all(response.as_bytes()).await?;
    }

    Ok(())
}

pub(crate) fn handle_command(
    line:        &str,
    bus:         &EventBus,
    patch_tx:    &Arc<Mutex<Producer<Vec<Chain>>>>,
    build_cfg:   &BuildConfig,
    patch_state: &Arc<Mutex<PatchState>>,
    cfg:         &Arc<Mutex<Config>>,
) -> Result<(), String> {
    let (cmd, rest) = split_cmd(line);

    match cmd {
        // ------------------------------------------------------------------
        // SET <key.param> <float>
        // ------------------------------------------------------------------
        "SET" => {
            let (path, val_str) = rest
                .split_once(' ')
                .ok_or("usage: SET <key.param> <value>")?;
            let val_str = val_str.trim();
            if let Ok(value) = val_str.parse::<f32>() {
                bus.send(ControlMessage::SetParam { path: path.to_string(), value }).ok();
            } else {
                // Non-numeric value → action dispatch (e.g. "SET 01-looper.action rec")
                bus.send(ControlMessage::Action {
                    path:   path.to_string(),
                    action: val_str.to_string(),
                }).ok();
            }
            Ok(())
        }

        // ------------------------------------------------------------------
        // UPDATE <json-object>
        // ------------------------------------------------------------------
        "UPDATE" => {
            let v: Value = serde_json::from_str(rest)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            let pairs = patch::flatten_update(&v);
            if pairs.is_empty() {
                return Err("no numeric values found in update object".into());
            }
            for (path, value) in &pairs {
                bus.send(ControlMessage::SetParam { path: path.clone(), value: *value }).ok();
            }
            Ok(())
        }

        // ------------------------------------------------------------------
        // PATCH <json-object>   — too large for broadcast; direct path kept
        // ------------------------------------------------------------------
        "PATCH" => {
            let json_value: Value = serde_json::from_str(rest)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            let chains = patch::load_str(rest, build_cfg)
                .map_err(|e| format!("patch build error: {e}"))?;
            patch_tx
                .lock().map_err(|_| "lock error")?
                .push(chains).map_err(|_| "patch channel full")?;
            if let Ok(mut s) = patch_state.lock() {
                s.apply_patch(json_value);
            }
            Ok(())
        }

        // ------------------------------------------------------------------
        "RESET" => {
            bus.send(ControlMessage::Reset).ok();
            Ok(())
        }

        "PROGRAM" => {
            let p: u8 = rest.trim()
                .parse()
                .map_err(|_| "program number must be 0-127")?;
            bus.send(ControlMessage::ProgramChange(p)).ok();
            Ok(())
        }

        // ------------------------------------------------------------------
        // SAVE_PRESET <slot>  — persist current chains to preset slot
        // ------------------------------------------------------------------
        "SAVE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .map_err(|_| "slot must be 0-127")?;
            let chains_val = patch_state
                .lock().map_err(|_| "lock error")?
                .json["chains"].clone();
            cfg.lock().map_err(|_| "lock error")?
                .save_preset(slot, chains_val)
                .map_err(|e| format!("save error: {e}"))?;
            Ok(())
        }

        other => Err(format!("unknown command '{other}'")),
    }
}

pub(crate) fn split_cmd(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (line, ""),
    }
}
