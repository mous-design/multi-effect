use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use rtrb::Producer;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_serial::SerialPortBuilderExt;
use tracing::{info, warn};

use crate::config::{BuildConfig, Config};
use crate::control::{apply_ctrl, outbound_line, ControlMessage, EventBus};
use crate::control::mapping::ControllerDef;
use crate::control::network::handle_command;
use crate::engine::patch::Chain;
use crate::save::PatchState;

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
    device:      String,
    baud:        u32,
    fallback:    bool,
    build_cfg:   BuildConfig,
    patch_state: Arc<Mutex<PatchState>>,
    cfg:         Arc<Mutex<Config>>,
    bus:         EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
}

impl SerialControl {
    pub fn new(
        device:      String,
        baud:        u32,
        fallback:    bool,
        build_cfg:   BuildConfig,
        patch_state: Arc<Mutex<PatchState>>,
        cfg:         Arc<Mutex<Config>>,
        bus:         EventBus,
        mappings:    Arc<RwLock<ControllerDef>>,
    ) -> Self {
        Self { device, baud, fallback, build_cfg, patch_state, cfg, bus, mappings }
    }

    pub async fn run(self, patch_tx: Arc<Mutex<Producer<Vec<Chain>>>>) -> Result<()> {
        // Destructure so fields can be reused across reconnect iterations.
        let Self { device, baud, fallback, build_cfg, patch_state, cfg, bus, mappings } = self;

        loop {
            // Open port — retry until available (handles cold-start and hot-plug).
            let port = loop {
                match tokio_serial::new(&device, baud).open_native_async() {
                    Ok(p)  => break p,
                    Err(e) => {
                        tracing::debug!("Serial '{device}': {e} — retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            };

            info!("Serial control on {device} at {baud} baud");

            let (reader, mut writer) = tokio::io::split(port);

            // Outbound: bus events → serial port
            let mut bus_rx   = bus.subscribe();
            let mappings_out = Arc::clone(&mappings);
            tokio::spawn(async move {
                while let Ok(msg) = bus_rx.recv().await {
                    let line = match &msg {
                        ControlMessage::SetParam { path, value } => {
                            let cfg = mappings_out.read().unwrap();
                            outbound_line(path, *value, &cfg)
                        }
                        ControlMessage::ProgramChange(n) => format!("PROGRAM {n}\n"),
                        ControlMessage::Reset            => "RESET\n".to_string(),
                        ControlMessage::NoteOn  { .. }
                        | ControlMessage::NoteOff { .. } => continue,
                    };
                    if writer.write_all(line.as_bytes()).await.is_err() {
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
                        } else if let Err(e) = handle_command(&line, &bus, &patch_tx, &build_cfg, &patch_state, &cfg) {
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

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}
