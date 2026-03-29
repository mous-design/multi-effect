pub mod mapping;
pub mod midi;
pub mod network;
pub mod serial;

pub use network::NetworkControl;
pub use serial::SerialControl;

use mapping::ControllerDef;
use tracing::{debug, warn};

/// Messages published on the event bus and forwarded to the audio thread.
#[derive(Debug, Clone)]
pub enum ControlMessage {
    /// Set a named parameter on a device in the signal graph.
    /// `path` has the form `"effect.param"`, e.g. `"delay.feedback"`.
    SetParam { path: String, value: f32 },

    /// MIDI Program Change → load preset number `p`.
    ProgramChange(u8),

    /// Reset all internal state (equivalent to switching to the same patch).
    Reset,

    /// MIDI Note On — forwarded to all Device nodes in all chains.
    NoteOn  { note: u8, velocity: u8 },

    /// MIDI Note Off — forwarded to all Device nodes in all chains.
    NoteOff { note: u8 },

    /// Dispatch a named action string to a device parameter.
    /// `path` has the form `"01-looper.action"` or just `"01-looper"`.
    /// Used for looper combined actions: "rec", "play", "stop", "reset",
    /// "rec-play-stop-rec", etc.
    Action { path: String, action: String },

    /// A generic event emitted by a node (e.g. Looper state change, loop wrap).
    /// Consumed by the WebSocket layer to push to the UI.
    /// Never forwarded to the audio thread.
    NodeEvent {
        /// The originating node's key, e.g. `"05-looper"`.
        key: String,
        /// Event name, e.g. `"looper_state"` or `"loop_wrap"`.
        event: String,
        /// Event payload as a JSON object.
        data: serde_json::Value,
    },

    /// Toggle compare mode: swap between dirty state and saved preset.
    /// Sent by foot pedal (serial/net `COMPARE` command) or the UI.
    Compare,

    /// Notification broadcast to WS clients when compare mode changes.
    CompareChanged {
        chains:      serde_json::Value,
        is_dirty:    bool,
        is_comparing: bool,
    },
}

/// Broadcast channel used as the central pub/sub event bus.
/// All control sources (TCP, MIDI, serial) publish here.
/// All interested parties subscribe via `EventBus::subscribe()`.
pub type EventBus = tokio::sync::broadcast::Sender<ControlMessage>;

/// Create a new event bus. The returned sender can be cloned for multiple publishers.
pub fn new_event_bus() -> EventBus {
    tokio::sync::broadcast::channel::<ControlMessage>(256).0
}

// ---------------------------------------------------------------------------
// Shared CTRL helpers (used by serial and net)
// ---------------------------------------------------------------------------

/// Process a `CTRL <channel_id> <raw_value>` line:
/// - If the channel ID is in `mappings`, convert and publish `SetParam`.
/// - If not found and `fallback` is true, publish `SetParam { path: channel_id, value: raw }`.
/// - If not found and `fallback` is false, silently ignore.
pub(crate) fn apply_ctrl(line: &str, mappings: &ControllerDef, fallback: bool, bus: &EventBus) {
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        warn!("Malformed CTRL command: {line}");
        return;
    }
    let channel_id = parts[1];
    let raw: f32 = match parts[2].trim().parse() {
        Ok(v)  => v,
        Err(_) => { warn!("CTRL value not a number: {}", parts[2]); return; }
    };

    if let Some(def) = mappings.mappings.get(channel_id) {
        let value = def.to_param(raw);
        debug!("CTRL {channel_id} {raw} → SET {} {value:.4}", def.target);
        bus.send(ControlMessage::SetParam { path: def.target.clone(), value }).ok();
    } else if fallback {
        debug!("CTRL {channel_id} {raw} → SET {channel_id} {raw} (fallback)");
        bus.send(ControlMessage::SetParam { path: channel_id.to_string(), value: raw }).ok();
    }
    // else: dedicated controller mode — unknown channels are silently ignored
}

/// Build the outbound line for a `SetParam` event:
/// - If a reverse mapping exists: `CTRL <channel_id> <raw_value>\n`
/// - Otherwise:                   `SET <path> <value>\n`
pub(crate) fn outbound_line(path: &str, value: f32, mappings: &ControllerDef) -> String {
    if let Some((ch, def)) = mappings.channel_for_target(path) {
        format!("CTRL {ch} {}\n", def.to_ctrl_str(value))
    } else {
        format!("SET {path} {value}\n")
    }
}
