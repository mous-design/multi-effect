use anyhow::{Result, Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use super::mapping::{ControllerDef, DeviceDef};
use crate::engine::patch::ChainDef;
use crate::config::master::{ConfigRequest, snd_request};
use crate::config::ConfigPatch;
use super::{connection_id, ControlMessage, EventBus};

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
                            match snd_request(&master_tx, |tx| ConfigRequest::ReverseMapRounded {
                                path: path.clone(), value: *value, alias: alias.to_string(), resp: tx,
                            }).await {
                                // `raw` is pre-smart-rounded by master. Default Display prints
                                // the shortest round-trippable form (e.g. "63.5" not "63.5000").
                                Ok(Some((ch, raw))) => format!("CTRL {ch} {raw}\n"),
                                // No reverse mapping: emit the raw param value. Once `get_params`
                                // exists we can smart-round-target using the param's known range.
                                _ => format!("SET {path} {value}\n"),
                            }
                        },
                        ControlMessage::Reset { .. } => "RESET\n".to_string(),
                        ControlMessage::PresetLoaded { ref preset, .. } =>
                            format!("PRESET {}\n", serde_json::to_string(preset).unwrap_or_default()),
                        ControlMessage::StateChanged { ref state, .. } =>
                            format!("STATE {state}\n"),
                        ControlMessage::PresetIndices { ref indices, .. } =>
                            format!("INDICES {}\n", serde_json::to_string(indices).unwrap_or_default()),
                        ControlMessage::NodeEvent { ref key, ref event, ref data } =>
                            format!("EVENT {key} {event} {}\n", serde_json::to_string(data).unwrap_or_default()),
                        _ => continue,
                    };
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break; // peer disconnected
                    }
                },
                ack = ack_rx.recv() => {
                    match ack {
                        Some(line) => {
                            if writer.write_all(line.as_bytes()).await.is_err() {
                                break; // peer disconnected
                            }
                        },
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
                handle_ctrl(&line, &master_tx, alias, &source).await
            } else {
                handle_command(&line, &master_tx, &source).await
            };
            let response = match res {
                Ok(line)  => line,
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
    source:    &str,
) -> Result<String> {
    let (_, rest) = line.split_once(' ').context("malformed CTRL")?;
    let (channel_id, raw_str) = rest.split_once(' ').context("malformed CTRL")?;
    let raw: f32 = raw_str.trim().parse()
        .with_context(|| format!("CTRL value not a number: {raw_str}"))?;

    snd_request(master_tx, |tx| ConfigRequest::ApplyCtrl {
        channel_id: channel_id.to_string(),
        raw,
        alias: alias.to_string(),
        source: source.to_string(),
        resp: Some(tx),
    }).await?;
    Ok("OK\n".into())
}

async fn handle_command(
    line:      &str,
    master_tx: &mpsc::Sender<ConfigRequest>,
    source:    &str,
) -> Result<String> {
    let (cmd, rest) = split_cmd(line);
    let source = source.to_string();

    let res = match cmd {
        "FETCH_CONFIG" => {
            let config = snd_request(master_tx, |tx| ConfigRequest::GetConfig {resp: tx}).await?;
            format!("CONFIG {}\n", serde_json::to_string(&config)?)
        },
        "SAVE_CONFIG" => {
            let config: ConfigPatch = serde_json::from_str(rest)?;
            snd_request(master_tx, |tx| ConfigRequest::UpdateConfig {
                resp: Some(tx), source, config,
            }).await?;
            "OK\n".into()
        },
        "SET" => {
            let (path, val_str) = rest
                .split_once(' ')
                .context("usage: SET <key.param> <value>")?;
            let val_str = val_str.trim();
            if let Ok(value) = val_str.parse::<f32>() {
                snd_request(master_tx, |tx| ConfigRequest::ApplySet {
                     path: path.to_string(), value, source, resp: Some(tx)
                }).await?;
            } else {
                // Non-numeric value → action dispatch (e.g. "SET 01-looper.action rec")
                snd_request(master_tx, |tx| ConfigRequest::ApplyAction {
                    path: path.to_string(), action: val_str.to_string(), source, resp: Some(tx)
                }).await?;
            }
            "OK\n".into()
        },
        "CHAINS" => {
            let chains: Vec<ChainDef> = serde_json::from_str(rest)?;
            snd_request(master_tx, |tx| ConfigRequest::SetChains {
                source, chains, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("program number must be 0-127")?;
            snd_request(master_tx, |tx| ConfigRequest::SwitchPreset {
                source: source.clone(), slot, resp: Some(tx),
            }).await?;
            // Return the new snapshot inline — originator is filtered out of the
            // PresetLoaded broadcast, so this is how they get the new content.
            let snap = snd_request(master_tx, |tx| ConfigRequest::GetSnapshot { resp: tx }).await?;
            format!("SNAPSHOT {}\n", serde_json::to_string(&snap.to_view())?)
        },
        "SAVE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("slot must be 0..127")?;
            snd_request(master_tx, |tx| ConfigRequest::SavePreset {
                source, slot, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "DELETE_PRESET" => {
            let slot: u8 = rest.trim()
                .parse()
                .context("slot must be 0..127")?;
            snd_request(master_tx, |tx| ConfigRequest::DeletePreset {
                source, slot, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },        
        "COMPARE" => {
            snd_request(master_tx, |tx| ConfigRequest::ToggleCompare {
                source: source.clone(), resp: Some(tx),
            }).await?;
            // Return the new snapshot inline — originator is filtered out of the
            // PresetLoaded broadcast.
            let snap = snd_request(master_tx, |tx| ConfigRequest::GetSnapshot { resp: tx }).await?;
            format!("SNAPSHOT {}\n", serde_json::to_string(&snap.to_view())?)
        },
        "PUT_CONTROLLERS" => {
            let controllers: Vec<ControllerDef> = serde_json::from_str(rest)?;
            snd_request(master_tx, |tx| ConfigRequest::UpdateControllers {
                resp: Some(tx), source, controllers,
            }).await?;
            "OK\n".into()
        },
        "FETCH_DEVICES" => {
            let devices = snd_request(master_tx, |tx| ConfigRequest::GetDevices {resp: tx}).await?;
            format!("DEVICES {}\n", serde_json::to_string(&devices)?)
        },
        "PUT_DEVICE" => {
            let (alias, val_str) = rest
                .split_once(' ')
                .context("usage: PUT_DEVICE <alias> <def>")?;
            let alias = alias.trim().to_string();
            let def: DeviceDef = serde_json::from_str(val_str)?;
            snd_request(master_tx, |tx| ConfigRequest::PutDevice {
                alias, def, source, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "DELETE_DEVICE" => {
            let alias = rest.trim().to_string();
            snd_request(master_tx, |tx| ConfigRequest::DeleteDevice {
                alias, source, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "SET_DEVICE_NAME" => {
            let (old_alias, new_alias) = rest
                .split_once(' ')
                .context("usage: SET_DEVICE_NAME <old_alias> <new_alias>")?;
            let old_alias = old_alias.trim().to_string();
            let new_alias = new_alias.trim().to_string();
            snd_request(master_tx, |tx| ConfigRequest::RenameDevice {
                old_alias, new_alias, source, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "RELOAD" => {
            snd_request(master_tx, |tx| ConfigRequest::Reload {
                source, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        "RESET" => {
            snd_request(master_tx, |tx| ConfigRequest::ApplyReset {
                source, resp: Some(tx),
            }).await?;
            "OK\n".into()
        },
        other => bail!("unknown command '{other}'"),
    };
    Ok(res)
}

fn split_cmd(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (line, ""),
    }
}
