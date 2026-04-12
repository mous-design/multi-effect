use std::sync::{Arc, RwLock};

use tracing::{debug, error, info, warn};

use tokio::sync::mpsc;

use crate::config::master::ConfigRequest;
use crate::control::{ControlMessage, EventBus};
use crate::control::mapping::{ControllerDef, MidiChannel};

// ---------------------------------------------------------------------------
// MidiControl  (MIDI input)
// ---------------------------------------------------------------------------

/// Opens a MIDI input port and forwards CC / Program Change events to the master.
///
/// CC events are looked up in the active `ControllerDef` mappings.  Unknown CC numbers
/// are silently ignored (MIDI has no text fallback).
///
/// The `ControllerDef` is held behind an `Arc<RwLock>` so it can be swapped at runtime
/// when a new preset is loaded.  The channel filter in `ControllerDef.channel` overrides
/// the device-level default for the duration of the preset.
pub struct MidiControl {
    device_name:     Option<String>,
    /// Channel filter from `DeviceDef::MidiIn` — used when the preset doesn't override it.
    default_channel: MidiChannel,
    pub mappings:    Arc<RwLock<ControllerDef>>,
}

impl MidiControl {
    pub fn new(
        device_name:     Option<String>,
        default_channel: MidiChannel,
        mappings:        Arc<RwLock<ControllerDef>>,
    ) -> Self {
        Self { device_name, default_channel, mappings }
    }

    /// Open the MIDI input port and start forwarding messages to the master.
    /// Spawns a background thread; returns immediately.
    pub fn run(self, master_tx: mpsc::Sender<ConfigRequest>) {
        let midi_in = match midir::MidiInput::new("multi-effect") {
            Ok(m)  => m,
            Err(e) => { error!("MIDI init error: {e}"); return; }
        };

        let ports = midi_in.ports();
        if ports.is_empty() {
            warn!("No MIDI input ports found — MIDI disabled");
            return;
        }

        debug!("Available MIDI input ports:");
        for (i, p) in ports.iter().enumerate() {
            debug!("  [{i}] {}", midi_in.port_name(p).unwrap_or_else(|_| "<?>".into()));
        }

        let port = if let Some(ref name) = self.device_name {
            let found = ports.iter().find(|p| {
                midi_in.port_name(p)
                    .map(|n| n.contains(name.as_str()))
                    .unwrap_or(false)
            });
            if found.is_none() {
                warn!("MIDI input port '{name}' not found, using first available");
            }
            found.unwrap_or(&ports[0])
        } else {
            &ports[0]
        };

        let port_name = midi_in.port_name(port).unwrap_or_default();
        info!("MIDI in: opening port '{port_name}'");

        let default_channel = self.default_channel;
        let mappings        = Arc::clone(&self.mappings);

        let conn = midi_in.connect(
            port,
            "multi-effect-midi-in",
            move |_stamp, msg, _| {
                if msg.is_empty() { return; }
                let status   = msg[0];
                let msg_type = status & 0xF0;
                let channel  = (status & 0x0F) + 1; // 1-based

                match (msg_type, msg.len()) {
                    (0xB0, 3) => debug!("MIDI ch{channel} CC{}  val={}", msg[1], msg[2]),
                    (0xC0, 2) => debug!("MIDI ch{channel} PC   program={}", msg[1]),
                    (0x90, 3) => debug!("MIDI ch{channel} NoteOn  note={} vel={}", msg[1], msg[2]),
                    (0x80, 3) => debug!("MIDI ch{channel} NoteOff note={} vel={}", msg[1], msg[2]),
                    (0xE0, 3) => debug!("MIDI ch{channel} PitchBend lsb={} msb={}", msg[1], msg[2]),
                    (0xD0, 2) => debug!("MIDI ch{channel} ChannelPressure val={}", msg[1]),
                    (0xA0, 3) => debug!("MIDI ch{channel} PolyPressure note={} val={}", msg[1], msg[2]),
                    _         => debug!("MIDI raw: {:02X?}", msg),
                }

                let ctrl_msg = match msg_type {
                    0xB0 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        // Use preset channel override if present, otherwise device default.
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) { return; }

                        let cc_str = msg[1].to_string();
                        if let Some(def) = cfg.mappings.get(&cc_str) {
                            let mapped = def.to_param(msg[2] as f32);
                            debug!("MIDI ch{channel} CC{} {} → SET {} {mapped:.4}", msg[1], msg[2], def.target);
                            Some(ControlMessage::SetParam {
                                path:  def.target.clone(),
                                value: mapped,
                            })
                        } else {
                            None // MIDI: no fallback for unmapped CC
                        }
                    }
                    0xC0 if msg.len() >= 2 => {
                        debug!("MIDI ch{channel} Program Change {}", msg[1]);
                        Some(ControlMessage::ProgramChange(msg[1]))
                    }
                    // Note On (velocity 0 = note off per MIDI spec)
                    0x90 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) {
                            debug!("MIDI ch{channel} NoteOn note={} vel={} — filtered out", msg[1], msg[2]);
                            return;
                        }
                        if msg[2] > 0 {
                            debug!("MIDI ch{channel} NoteOn note={} vel={} → forwarded", msg[1], msg[2]);
                            Some(ControlMessage::NoteOn { note: msg[1], velocity: msg[2] })
                        } else {
                            debug!("MIDI ch{channel} NoteOn note={} vel=0 → NoteOff", msg[1]);
                            Some(ControlMessage::NoteOff { note: msg[1] })
                        }
                    }
                    // Note Off
                    0x80 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) {
                            debug!("MIDI ch{channel} NoteOff note={} — filtered out", msg[1]);
                            return;
                        }
                        debug!("MIDI ch{channel} NoteOff note={} → forwarded", msg[1]);
                        Some(ControlMessage::NoteOff { note: msg[1] })
                    }
                    _ => None,
                };
                
                let req = match ctrl_msg {
                    Some(ControlMessage::SetParam { path, value }) =>
                        Some(ConfigRequest::ApplySet { path, value, resp: None }),
                    Some(ControlMessage::ProgramChange(p)) =>
                        Some(ConfigRequest::SwitchPreset { slot: p, resp: None }),
                    Some(ControlMessage::NoteOn { note, velocity }) =>
                        Some(ConfigRequest::ApplyControl(ControlMessage::NoteOn { note, velocity })),
                    Some(ControlMessage::NoteOff { note }) =>
                        Some(ConfigRequest::ApplyControl(ControlMessage::NoteOff { note })),
                    _ => None,
                };
                // If system locks up on many notes, try_send is the alternative.
                if let Some(r) = req {
                    if master_tx.blocking_send(r).is_err() {
                        warn!("MIDI: master channel closed");
                        return;
                    }
                }
            },
            (),
        );

        match conn {
            Ok(conn) => {
                std::thread::spawn(move || {
                    let _conn = conn; // keep alive
                    std::thread::park();
                });
            }
            Err(e) => error!("MIDI connect error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// MidiOutControl  (MIDI output)
// ---------------------------------------------------------------------------

/// Subscribes to the event bus and sends MIDI CC messages for mapped parameter changes.
///
/// - `SetParam` with a matching mapping → CC on the configured output channel.
/// - `ProgramChange`                    → MIDI Program Change on the output channel.
/// - `Reset`                            → ignored (no MIDI equivalent).
/// - Unmapped `SetParam`                → silently ignored.
pub struct MidiOutControl {
    device_name: Option<String>,
    /// 1-based MIDI channel to send on (from `DeviceDef::MidiOut.channel`).
    out_channel: u8,
    pub mappings: Arc<RwLock<ControllerDef>>,
}

impl MidiOutControl {
    pub fn new(
        device_name: Option<String>,
        out_channel: u8,
        mappings:    Arc<RwLock<ControllerDef>>,
    ) -> Self {
        Self { device_name, out_channel, mappings }
    }

    /// Open the MIDI output port and start consuming bus events.
    /// Spawns a background thread; returns immediately.
    pub fn run(self, bus: EventBus) {
        let mut bus_rx = bus.subscribe();
        std::thread::spawn(move || {
            let midi_out = match midir::MidiOutput::new("multi-effect-out") {
                Ok(m)  => m,
                Err(e) => { error!("MIDI out init error: {e}"); return; }
            };

            let ports = midi_out.ports();
            if ports.is_empty() {
                warn!("No MIDI output ports found — MIDI out disabled");
                return;
            }

            debug!("Available MIDI output ports:");
            for (i, p) in ports.iter().enumerate() {
                debug!("  [{i}] {}", midi_out.port_name(p).unwrap_or_else(|_| "<?>".into()));
            }

            let port = if let Some(ref name) = self.device_name {
                let found = ports.iter().find(|p| {
                    midi_out.port_name(p)
                        .map(|n| n.contains(name.as_str()))
                        .unwrap_or(false)
                });
                if found.is_none() {
                    warn!("MIDI out port '{name}' not found, using first available");
                }
                found.unwrap_or(&ports[0])
            } else {
                &ports[0]
            };

            let port_name = midi_out.port_name(port).unwrap_or_default();
            info!("MIDI out: opening port '{port_name}'");

            let mut conn = match midi_out.connect(port, "multi-effect-midi-out") {
                Ok(c)  => c,
                Err(e) => { error!("MIDI out connect error: {e}"); return; }
            };

            // channel byte: 0-based for MIDI status byte
            let ch_byte = (self.out_channel.saturating_sub(1)) & 0x0F;
            let mappings = self.mappings;

            loop {
                let msg = match bus_rx.blocking_recv() {
                    Ok(m)  => m,
                    Err(_) => break, // bus closed
                };

                match msg {
                    ControlMessage::SetParam { ref path, value } => {
                        let cfg = mappings.read().unwrap();
                        if let Some((cc_str, def)) = cfg.channel_for_target(path) {
                            if let Ok(cc) = cc_str.parse::<u8>() {
                                let raw = def.to_ctrl(value).clamp(0.0, 127.0) as u8;
                                debug!("MIDI out SET {path} {value:.4} → CC{cc} {raw}");
                                let _ = conn.send(&[0xB0 | ch_byte, cc, raw]);
                            }
                        }
                        // else: silently ignore unmapped params
                    }
                    ControlMessage::ProgramChange(p) => {
                        let _ = conn.send(&[0xC0 | ch_byte, p]);
                    }
                    ControlMessage::Reset
                    | ControlMessage::NoteOn         { .. }
                    | ControlMessage::NoteOff        { .. }
                    | ControlMessage::Action         { .. }
                    | ControlMessage::NodeEvent      { .. }
                    | ControlMessage::Compare
                                     => {} // not forwarded to MIDI out
                }
            }
        });
    }
}
