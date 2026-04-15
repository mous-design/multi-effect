pub mod mapping;
pub mod handle;
pub mod midi;
pub mod network;
pub mod serial;

use anyhow::{Result, Context, bail};
pub use network::NetworkControl;
pub use serial::SerialControl;

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;
use mapping::ControllerDef;
use tracing::debug;

// ---------------------------------------------------------------------------
// Connection ID generator
// ---------------------------------------------------------------------------

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a unique source ID for a connection.
/// Format: `{sanitized_alias}-{counter}`, e.g. `net-1-4`, `serial-1-1`.
pub fn connection_id(alias: &str) -> String {
    let sanitized: String = alias.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    format!("{sanitized}-{id}")
}

// ---------------------------------------------------------------------------
// ControlMessage
// ---------------------------------------------------------------------------

/// Messages published on the event bus and forwarded to the audio thread.
/// Variants that originate from external controllers carry a `source` field
/// so that outbound tasks can skip echoing messages back to the sender.
#[derive(Debug, Clone)]
pub enum ControlMessage {
    SetParam { path: String, value: f32, source: String },
    ProgramChange { slot: u8, source: String },
    Reset { source: String },
    Action { path: String, action: String, source: String },

    // System events — no source needed, no echo risk.
    NoteOn  { note: u8, velocity: u8 },
    NoteOff { note: u8 },
    NodeEvent { key: String, event: String, data: serde_json::Value },
    PresetLoaded { preset: serde_json::Value, preset_indices: Vec<u8>, state: String },
    StateChanged { state: String, preset_index: u8, preset_indices: Vec<u8> },
}

impl ControlMessage {
    /// Returns the source identifier for messages that carry one, empty string otherwise.
    pub fn source(&self) -> &str {
        match self {
            Self::SetParam { source, .. }
            | Self::ProgramChange { source, .. }
            | Self::Reset { source, .. }
            | Self::Action { source, .. } => source,
            _ => "",
        }
    }
}

/// Broadcast channel used as the central pub/sub event bus.
/// All control sources (TCP, MIDI, serial) publish here.
/// All interested parties subscribe via `EventBus::subscribe()`.
pub type EventBus = broadcast::Sender<ControlMessage>;

/// Create a new event bus. The returned sender can be cloned for multiple publishers.
pub fn new_event_bus() -> EventBus {
    broadcast::channel::<ControlMessage>(256).0
}

// ---------------------------------------------------------------------------
// Shared CTRL helpers (used by serial and net)
// ---------------------------------------------------------------------------

/// Process a `CTRL <channel_id> <raw_value>` line:
/// - If the channel ID is in `mappings`, convert and publish `SetParam`.
/// - If not found and `fallback` is true, publish `SetParam { path: channel_id, value: raw }`.
/// - If not found and `fallback` is false, silently ignore.
pub(crate) fn apply_ctrl(line: &str, mappings: &ControllerDef, fallback: bool, bus: &EventBus, source: &str) -> Result<()> {
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        bail!("Malformed CTRL command: {line}");
    }
    let channel_id = parts[1];
    let raw: f32 = parts[2].trim().parse()
        .with_context(|| format!("CTRL value not a number: {}", parts[2]) )?;

    if let Some(def) = mappings.mappings.get(channel_id) {
        let value = def.to_param(raw);
        debug!("CTRL {channel_id} {raw} → SET {} {value:.4} [source={source}]", def.target);
        bus.send(ControlMessage::SetParam { path: def.target.clone(), value, source: source.to_string() }).ok();
    } else if fallback {
        debug!("CTRL {channel_id} {raw} → SET {channel_id} {raw} (fallback) [source={source}]");
        bus.send(ControlMessage::SetParam { path: channel_id.to_string(), value: raw, source: source.to_string() }).ok();
    }
    // else: dedicated controller mode — unknown channels are silently ignored
    Ok(())
}

/// Build the outbound line for a `SetParam` event:
/// - If a reverse mapping exists: `CTRL <channel_id> <raw_value>\n`
/// - Otherwise:                   `SET <path> <value>\n`
pub(crate) fn outbound_line(path: &str, value: f32, mappings: &ControllerDef) -> String {
    if let Some((ch, def)) = mappings.channel_for_target(path) {
        format!("CTRL {ch} {}\n", def.to_ctrl_str(value))
    } else {
        format!("SET {path} {value:.4}\n")
    }
}
