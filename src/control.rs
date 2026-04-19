pub mod mapping;
pub mod handle;
pub mod midi;
pub mod network;
pub mod serial;

pub use network::NetworkControl;
pub use serial::SerialControl;

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;
use mapping::ControllerDef;
use tracing::debug;

use crate::config::preset::PresetDef;

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
    Reset { source: String },
    Action { path: String, action: String, source: String },

    // System events — no source needed, no echo risk.
    NoteOn  { note: u8, velocity: u8 },
    NoteOff { note: u8 },
    NodeEvent { key: String, event: String, data: serde_json::Value },
    PresetLoaded { preset: PresetDef, preset_indices: Vec<u8>, state: String },
    StateChanged { state: String, preset_index: u8, preset_indices: Vec<u8> },
}

impl ControlMessage {
    /// Returns the source identifier for messages that carry one, empty string otherwise.
    pub fn source(&self) -> &str {
        match self {
            Self::SetParam { source, .. }
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
// CTRL translation helper (called by master)
// ---------------------------------------------------------------------------

/// Translate a CTRL channel/value pair using the device's mappings.
///
/// Returns `Some((path, value))` if the channel is mapped, `None` otherwise.
/// Unmapped channels are silently ignored — clients that want direct parameter
/// access should use `SET <path> <value>`.
pub(crate) fn translate_ctrl(channel_id: &str, raw: f32, mappings: &ControllerDef) -> Option<(String, f32)> {
    let def = mappings.mappings.get(channel_id)?;
    let value = def.to_param(raw);
    debug!("CTRL {channel_id} {raw} → SET {} {}", def.target, def.smart_round_target(value));
    Some((def.target.clone(), value))
}
