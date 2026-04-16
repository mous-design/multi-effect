use anyhow::{Result, Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use crate::config::master::{ConfigRequest, snd_request};
use super::{connection_id, ControlMessage, EventBus};
use crate::engine::patch;

/// Run a full-duplex control session on `stream` until one of:
/// - the peer disconnects (inbound EOF or a write error);
/// - an inbound read or ack-write fails;
/// - `active_rx` flips to `false` (device deactivation).
///
/// Inbound and outbound communicate through an `ack` channel: inbound sends
/// OK/ERR strings, outbound owns the writer exclusively and flushes both bus
/// events and acks — no shared writer, no Mutex.
///
/// All mapping work (inbound CTRL translation, outbound reverse mapping) is
/// delegated to master via `master_tx`
pub async fn handle_client<S>(
    stream:        S,
    bus:           EventBus,
    fallback:      bool,
    master_tx:     mpsc::Sender<ConfigRequest>,
    alias:         &str,
    mut active_rx: watch::Receiver<bool>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let source = connection_id(alias);
    let mut bus_rx = bus.subscribe();
    let (ack_tx, mut ack_rx) = mpsc::channel::<String>(8);

    // Outbound: sole owner of the writer.
    // Selects over bus events and ack responses from inbound.
    let outbound = async {
        loop {
            tokio::select! {
                msg = bus_rx.recv() => {
                    let msg = match msg {
                        Ok(m)  => m,
                        Err(_) => break,  // bus closed
                    };
                    if msg.source() == source { continue; }

                    let line = match &msg {
                        ControlMessage::SetParam { path, value, .. } => {
                            match snd_request(&master_tx, |tx| ConfigRequest::ReverseMap {
                                path: path.clone(), value: *value, alias: alias.to_string(), resp: tx,
                            }).await {
                                Ok(Some((ch, raw))) => format!("CTRL {ch} {raw}\n"),
                                _ => format!("SET {path} {value:.4}\n"),
                            }
                        }
                        ControlMessage::ProgramChange { slot, .. } => format!("PROGRAM {slot}\n"),
                        ControlMessage::Reset { .. }               => "RESET\n".to_string(),
                        ControlMessage::NoteOn      { .. }
                        | ControlMessage::NoteOff   { .. }
                        | ControlMessage::Action    { .. }
                        | ControlMessage::NodeEvent { .. }
                        => continue,
                        ControlMessage::PresetLoaded { ref preset, state: ref s, .. }
                        => format!("PRESET {}\nSTATE {s}\n", serde_json::to_string(preset).unwrap_or_default()),
                        ControlMessage::StateChanged { ref state, preset_index, .. }
                        => format!("STATE {state} {preset_index}\n"),
                    };
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break; // peer disconnected
                    }
                }
                ack = ack_rx.recv() => {
                    match ack {
                        Some(line) => {
                            if writer.write_all(line.as_bytes()).await.is_err() {
                                break; // peer disconnected
                            }
                        }
                        None => break,  // inbound dropped ack_tx
                    }
                }
            }
        }
    };

    // Inbound: reads commands, sends OK/ERR acks through the channel.
    // All commands are routed through master_tx — no direct bus access.
    let inbound = async {
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() { continue; }

            let res = if line.starts_with("CTRL ") {
                handle_ctrl(&line, &master_tx, alias, fallback, &source).await
            } else {
                handle_command(&line, &master_tx, &source).await
            };
            let response = match res {
                Ok(())  => "OK\n".to_string(),
                Err(e)  => format!("ERR {e}\n"),
            };
            // If outbound is gone (writer dead), stop reading too.
            if ack_tx.send(response).await.is_err() { break; }
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        _ = outbound            => {}
        r = inbound             => r?,
        _ = active_rx.changed() => {}
    }
    Ok(())
}

/// Parse and forward a CTRL line to master for mapping translation.
async fn handle_ctrl(
    line:      &str,
    master_tx: &mpsc::Sender<ConfigRequest>,
    alias:     &str,
    fallback:  bool,
    source:    &str,
) -> Result<()> {
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        bail!("Malformed CTRL command: {line}");
    }
    let channel_id = parts[1];
    let raw: f32 = parts[2].trim().parse()
        .with_context(|| format!("CTRL value not a number: {}", parts[2]))?;

    snd_request(master_tx, |tx| ConfigRequest::ApplyCtrl {
        channel_id: channel_id.to_string(),
        raw,
        alias: alias.to_string(),
        fallback,
        midi_channel: None,
        source: source.to_string(),
        resp: Some(tx),
    }).await
}

pub(crate) async fn handle_command(
    line:      &str,
    master_tx: &mpsc::Sender<ConfigRequest>,
    source:    &str,
) -> Result<()> {
    let (cmd, rest) = split_cmd(line);
    let src = source.to_string();

    match cmd {
        // ------------------------------------------------------------------
        // SET <key.param> <value>
        // ------------------------------------------------------------------
        "SET" => {
            let (path, val_str) = rest
                .split_once(' ')
                .context("usage: SET <key.param> <value>")?;
            let val_str = val_str.trim();
            if let Ok(value) = val_str.parse::<f32>() {
                snd_request(master_tx, |tx| ConfigRequest::ApplySet {
                     path: path.to_string(), value, source: src, resp: Some(tx)
                }).await?;
            } else {
                // Non-numeric value → action dispatch (e.g. "SET 01-looper.action rec")
                snd_request(master_tx, |tx| ConfigRequest::ApplyAction {
                    path: path.to_string(), action: val_str.to_string(), source: src, resp: Some(tx)
                }).await?;
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
                snd_request(master_tx, |tx| ConfigRequest::ApplySet {
                     path: path.clone(), value: *value, source: src.clone(), resp: Some(tx)
                }).await?;
            }
        },

        // ------------------------------------------------------------------
        // CHAINS <json>
        // ------------------------------------------------------------------
        "CHAINS" => {
            snd_request(master_tx, |tx| ConfigRequest::SetChains {
                json: rest.to_string(), resp: Some(tx),
            }).await?;
        }

        // ------------------------------------------------------------------
        "RESET" => {
            snd_request(master_tx, |tx| ConfigRequest::ApplyReset {
                source: src, resp: Some(tx),
            }).await?;
        },

        "PROGRAM" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("program number must be 0-127")?;
            snd_request(master_tx, |tx| ConfigRequest::SwitchPreset {
                slot, resp: Some(tx),
            }).await?;
        },

        // ------------------------------------------------------------------
        // SAVE_PRESET <slot>
        // ------------------------------------------------------------------
        "SAVE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("slot must be 0..127")?;
            snd_request(master_tx, |tx| ConfigRequest::SavePreset {
                slot, resp: Some(tx),
            }).await?;
        },

        "COMPARE" => {
            snd_request(master_tx, |tx| ConfigRequest::ToggleCompare {
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
