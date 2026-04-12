use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio_serial::SerialPortBuilderExt;
use tracing::{info, warn};

use crate::config::master::ConfigRequest;
use crate::control::{apply_ctrl, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::control::network::handle_command;

/// Bidirectional USB serial control interface.
///
/// **Inbound** supports two command forms:
/// ```text
/// CTRL <channel_id> <value>    — mapped control: looked up in ControllerDef,
///                                raw value transformed to param range.
///                                Falls back to SET if `fallback = true`.
/// SET  <key.param> <value>     — direct parameter set (same as TCP)
/// ```
/// Plus: `UPDATE`, `PATCH`, `RESET`, `PROGRAM`, `SAVE_PRESET` (same as TCP).
///
/// **Outbound**: for each EventBus event, if a reverse mapping exists for the
/// parameter target, sends `CTRL <channel_id> <raw_value>`.
/// Otherwise sends `SET <key.param> <value>` (or `PROGRAM`/`RESET`).
pub struct SerialControl {
    device:    String,
    baud:      u32,
    fallback:  bool,
    bus:       EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
    master_tx: mpsc::Sender<ConfigRequest>,
}

impl SerialControl {
    pub fn new(
        device:    String,
        baud:      u32,
        fallback:  bool,
        bus:       EventBus,
        mappings:  Arc<RwLock<ControllerDef>>,
        master_tx: mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { device, baud, fallback, bus, mappings, master_tx }
    }

    pub async fn run(self, mut active_rx: watch::Receiver<bool>) -> Result<()> {
        // Destructure so fields can be reused across reconnect iterations.
        let Self { device, baud, fallback, bus, mappings, master_tx } = self;

        loop {
            // Exit immediately if deactivated.
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

            let (reader, writer) = tokio::io::split(port);
            let writer = Arc::new(tokio::sync::Mutex::new(writer));

            // Outbound: bus → serial writer (mapped where possible)
            let mut bus_rx   = bus.subscribe();
            let mappings_out = Arc::clone(&mappings);
            let writer_out   = Arc::clone(&writer);
            tokio::spawn(async move {
                while let Ok(msg) = bus_rx.recv().await {
                    let line = match &msg {
                        ControlMessage::SetParam { path, value } => {
                            let cfg = mappings_out.read().unwrap();
                            outbound_line(path, *value, &cfg)
                        }
                        ControlMessage::ProgramChange(n) => format!("PROGRAM {n}\n"),
                        ControlMessage::Reset            => "RESET\n".to_string(),
                        ControlMessage::NoteOn          { .. }
                        | ControlMessage::NoteOff       { .. }
                        | ControlMessage::Action        { .. }
                        | ControlMessage::NodeEvent     { .. }
                        | ControlMessage::Compare  => continue,
                    };
                    if writer_out.lock().await.write_all(line.as_bytes()).await.is_err() {
                        break; // port closed — outbound task ends, inbound loop will catch it too
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
                            apply_ctrl(&line, &cfg, fallback, &bus);
                        } else if let Err(e) = handle_command(&line, &bus, &master_tx).await {
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
