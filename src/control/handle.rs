use std::sync::{Arc, RwLock};

use anyhow::{Result, Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::io::{AsyncRead, AsyncWrite};
use crate::config::master::{ConfigRequest, snd_request};
use crate::control::{apply_ctrl, connection_id, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::engine::patch;
pub async fn handle_client<S>(
    stream: S,
    bus:       EventBus,
    mappings:  Arc<RwLock<ControllerDef>>,
    fallback:  bool,
    master_tx: mpsc::Sender<ConfigRequest>,
    alias:     &str,
) -> Result<()> 
where S: AsyncRead + AsyncWrite + Unpin + Send + 'static,

{
    let (reader, writer) = tokio::io::split(stream);
    let source = connection_id(alias);
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

        let res = if line.starts_with("CTRL ") {
            let cfg = mappings.read().unwrap();
            apply_ctrl(&line, &cfg, fallback, &bus, &source)
        } else {
            handle_command(&line, &bus, &master_tx, &source).await
        };
        let response = match res {
            Ok(())  => "OK\n".to_string(),
            Err(e)  => format!("ERR {e}\n"),
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
                snd_request(&master_tx, |tx| ConfigRequest::ApplySet {
                     path: path.to_string(), value, source: src, resp: Some(tx)
                }).await?;
            } else {
                // Non-numeric value → action dispatch (e.g. "SET 01-looper.action rec")
                bus.send(ControlMessage::Action {
                    path:   path.to_string(),
                    action: val_str.to_string(),
                    source: src,
                }).ok();
            }
        },

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
                snd_request(&master_tx, |tx| ConfigRequest::ApplySet {
                     path: path.clone(), value: *value, source: src.clone(), resp: Some(tx)
                }).await?;
            }
        },

        // ------------------------------------------------------------------
        // CHAINS <json>
        // ------------------------------------------------------------------
        "CHAINS" => {
            snd_request(&master_tx, |tx| ConfigRequest::SetChains {
                json: rest.to_string(), resp: Some(tx),
            }).await?;
        }

        // ------------------------------------------------------------------
        "RESET" => {
            bus.send(ControlMessage::Reset { source: src }).ok();
        },

        "PROGRAM" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("program number must be 0-127")?;
            snd_request(&master_tx, |tx| ConfigRequest::SwitchPreset {
                slot, resp: Some(tx),
            }).await?;
            bus.send(ControlMessage::ProgramChange { slot, source: src }).ok();
        },

        // ------------------------------------------------------------------
        // SAVE_PRESET <slot>
        // ------------------------------------------------------------------
        "SAVE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("slot must be 0..127")?;
            snd_request(&master_tx, |tx| ConfigRequest::SavePreset {
                slot, resp: Some(tx),
            }).await?;
        },

        "COMPARE" => {
            snd_request(&master_tx, |tx| ConfigRequest::ToggleCompare {
                resp: Some(tx),
            }).await?;
        },

        other => bail!("unknown command '{other}'"),
    };
    Ok(())
}

fn split_cmd(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (line, ""),
    }
}
