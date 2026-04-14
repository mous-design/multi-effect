use std::sync::{Arc, RwLock};

use tracing::{debug, error, info, warn};

use tokio::sync::mpsc;

use crate::config::master::ConfigRequest;
use crate::control::{ControlMessage, EventBus};
use crate::control::mapping::{ControllerDef, MidiChannel};

// ---------------------------------------------------------------------------
// MidiControl  (MIDI input)
// ---------------------------------------------------------------------------

pub struct MidiControl {
    alias:           String,
    device_name:     Option<String>,
    default_channel: MidiChannel,
    pub mappings:    Arc<RwLock<ControllerDef>>,
}

impl MidiControl {
    pub fn new(
        alias:           String,
        device_name:     Option<String>,
        default_channel: MidiChannel,
        mappings:        Arc<RwLock<ControllerDef>>,
    ) -> Self {
        Self { alias, device_name, default_channel, mappings }
    }

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
        let alias           = self.alias;

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

                // Source ID: alias-ch{channel} (ch0 for omni)
                let source = format!("{alias}-ch{channel}");

                let ctrl_msg = match msg_type {
                    0xB0 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) { return; }

                        let cc_str = msg[1].to_string();
                        if let Some(def) = cfg.mappings.get(&cc_str) {
                            let mapped = def.to_param(msg[2] as f32);
                            debug!("MIDI ch{channel} CC{} {} → SET {} {mapped:.4}", msg[1], msg[2], def.target);
                            Some(ControlMessage::SetParam {
                                path:  def.target.clone(),
                                value: mapped,
                                source: source.clone(),
                            })
                        } else {
                            None
                        }
                    }
                    0xC0 if msg.len() >= 2 => {
                        debug!("MIDI ch{channel} Program Change {}", msg[1]);
                        Some(ControlMessage::ProgramChange { slot: msg[1], source: source.clone() })
                    }
                    0x90 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) { return; }
                        if msg[2] > 0 {
                            Some(ControlMessage::NoteOn { note: msg[1], velocity: msg[2] })
                        } else {
                            Some(ControlMessage::NoteOff { note: msg[1] })
                        }
                    }
                    0x80 if msg.len() >= 3 => {
                        let cfg    = mappings.read().unwrap();
                        let filter = cfg.channel.as_ref().unwrap_or(&default_channel);
                        if !filter.matches(channel) { return; }
                        Some(ControlMessage::NoteOff { note: msg[1] })
                    }
                    _ => None,
                };

                let req = match ctrl_msg {
                    Some(ControlMessage::SetParam { path, value, source }) =>
                        Some(ConfigRequest::ApplySet { path, value, source, resp: None }),
                    Some(ControlMessage::ProgramChange { slot, .. }) =>
                        Some(ConfigRequest::SwitchPreset { slot, resp: None }),
                    Some(ControlMessage::NoteOn { note, velocity }) =>
                        Some(ConfigRequest::ApplyControl(ControlMessage::NoteOn { note, velocity })),
                    Some(ControlMessage::NoteOff { note }) =>
                        Some(ConfigRequest::ApplyControl(ControlMessage::NoteOff { note })),
                    _ => None,
                };
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
                    let _conn = conn;
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

pub struct MidiOutControl {
    device_name: Option<String>,
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

            let ch_byte = (self.out_channel.saturating_sub(1)) & 0x0F;
            let mappings = self.mappings;

            loop {
                let msg = match bus_rx.blocking_recv() {
                    Ok(m)  => m,
                    Err(_) => break,
                };

                match msg {
                    ControlMessage::SetParam { ref path, value, .. } => {
                        let cfg = mappings.read().unwrap();
                        if let Some((cc_str, def)) = cfg.channel_for_target(path) {
                            if let Ok(cc) = cc_str.parse::<u8>() {
                                let raw = def.to_ctrl(value).clamp(0.0, 127.0) as u8;
                                debug!("MIDI out SET {path} {value:.4} → CC{cc} {raw}");
                                let _ = conn.send(&[0xB0 | ch_byte, cc, raw]);
                            }
                        }
                    }
                    ControlMessage::ProgramChange { slot, .. } => {
                        let _ = conn.send(&[0xC0 | ch_byte, slot]);
                    }
                    ControlMessage::Reset { .. }
                    | ControlMessage::NoteOn         { .. }
                    | ControlMessage::NoteOff        { .. }
                    | ControlMessage::Action         { .. }
                    | ControlMessage::NodeEvent      { .. }
                    | ControlMessage::PresetLoaded   { .. }
                    | ControlMessage::StateChanged   { .. }
                                     => {}
                }
            }
        });
    }
}
