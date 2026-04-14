use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio_serial::SerialPortBuilderExt;
use tracing::{info, warn};

use crate::config::master::ConfigRequest;
use crate::control::{apply_ctrl, connection_id, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::control::network::handle_command;

pub struct SerialControl {
    alias:     String,
    device:    String,
    baud:      u32,
    fallback:  bool,
    bus:       EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
    master_tx: mpsc::Sender<ConfigRequest>,
}

impl SerialControl {
    pub fn new(
        alias:     String,
        device:    String,
        baud:      u32,
        fallback:  bool,
        bus:       EventBus,
        mappings:  Arc<RwLock<ControllerDef>>,
        master_tx: mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { alias, device, baud, fallback, bus, mappings, master_tx }
    }

    pub async fn run(self, mut active_rx: watch::Receiver<bool>) -> Result<()> {
        let Self { alias, device, baud, fallback, bus, mappings, master_tx } = self;

        loop {
            if !*active_rx.borrow() { return Ok(()); }

            // Open port — retry until available (handles cold-start and hot-plug).
            let port = loop {
                match tokio_serial::new(&device, baud).open_native_async() {
                    Ok(p)  => break p,
                    Err(e) => {
                        tracing::debug!("Serial '{device}': {e} — retrying in 5s");
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                            _ = active_rx.changed() => {}
                        }
                        if !*active_rx.borrow() { return Ok(()); }
                    }
                }
            };
            info!("Serial '{device}': connected");

            // Unique source ID per connection (reconnects get a new ID)
            let source = connection_id(&alias);

            let (reader, writer) = tokio::io::split(port);
            let writer = Arc::new(tokio::sync::Mutex::new(writer));

            // Outbound: bus → serial writer (mapped where possible)
            let mut bus_rx   = bus.subscribe();
            let mappings_out = Arc::clone(&mappings);
            let writer_out   = Arc::clone(&writer);
            let source_out   = source.clone();
            tokio::spawn(async move {
                while let Ok(msg) = bus_rx.recv().await {
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
                        break;
                    }
                }
            });

            // Inbound: serial port lines → handle commands → publish to bus
            let mut lines = BufReader::new(reader).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() { continue; }
                        if line.starts_with("CTRL ") {
                            let cfg = mappings.read().unwrap();
                            apply_ctrl(&line, &cfg, fallback, &bus, &source);
                        } else if let Err(e) = handle_command(&line, &bus, &master_tx, &source).await {
                            warn!("Serial command error: {e}");
                        }
                    }
                    Ok(None) => {
                        warn!("Serial '{device}': disconnected — reconnecting in 5s");
                        break;
                    }
                    Err(e) => {
                        warn!("Serial '{device}': read error: {e} — reconnecting in 5s");
                        break;
                    }
                }
            }

            // Reconnect delay
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                _ = active_rx.changed() => {}
            }
        }
    }
}
