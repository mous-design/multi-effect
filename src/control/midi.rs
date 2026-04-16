use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::config::master::ConfigRequest;
use crate::control::{ControlMessage, EventBus};

// ---------------------------------------------------------------------------
// MidiControl  (MIDI input)
// ---------------------------------------------------------------------------

pub struct MidiControl {
    alias:       String,
    device_name: Option<String>,
}

impl MidiControl {
    pub fn new(alias: String, device_name: Option<String>) -> Self {
        Self { alias, device_name }
    }

    /// All CC translation and channel filtering is delegated to master via
    /// `ApplyCtrl` (with `midi_channel` set for filtering) and `ApplyControl`.
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

        let alias = self.alias;

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

                let source = format!("{alias}-ch{channel}");

                // Route everything through master — master handles channel filtering
                // and CC translation via the device's ControllerDef.
                let req = match msg_type {
                    0xB0 if msg.len() >= 3 => {
                        Some(ConfigRequest::ApplyCtrl {
                            channel_id: msg[1].to_string(),
                            raw: msg[2] as f32,
                            alias: alias.clone(),
                            fallback: false,
                            midi_channel: Some(channel),
                            source,
                            resp: None,
                        })
                    }
                    0xC0 if msg.len() >= 2 => {
                        Some(ConfigRequest::SwitchPreset { slot: msg[1], resp: None })
                    }
                    0x90 if msg.len() >= 3 => {
                        let cm = if msg[2] > 0 {
                            ControlMessage::NoteOn { note: msg[1], velocity: msg[2] }
                        } else {
                            ControlMessage::NoteOff { note: msg[1] }
                        };
                        Some(ConfigRequest::ApplyControl {
                            msg: cm, midi_channel: Some(channel), alias: alias.clone(),
                        })
                    }
                    0x80 if msg.len() >= 3 => {
                        Some(ConfigRequest::ApplyControl {
                            msg: ControlMessage::NoteOff { note: msg[1] },
                            midi_channel: Some(channel), alias: alias.clone(),
                        })
                    }
                    _ => None,
                };
                if let Some(r) = req {
                    if master_tx.blocking_send(r).is_err() {
                        warn!("MIDI: master channel closed");
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
    alias:       String,
    master_tx:   mpsc::Sender<ConfigRequest>,
}

impl MidiOutControl {
    pub fn new(
        device_name: Option<String>,
        out_channel: u8,
        alias:       String,
        master_tx:   mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { device_name, out_channel, alias, master_tx }
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
            let master_tx = self.master_tx;
            let alias = self.alias;

            loop {
                let msg = match bus_rx.blocking_recv() {
                    Ok(m)  => m,
                    Err(_) => break,
                };

                match msg {
                    ControlMessage::SetParam { ref path, value, .. } => {
                        // Ask master for reverse mapping (blocking round-trip).
                        let (tx, rx) = oneshot::channel();
                        if master_tx.blocking_send(ConfigRequest::ReverseMap {
                            path: path.clone(), value, alias: alias.clone(), resp: tx,
                        }).is_ok() {
                            if let Ok(Ok(Some((cc_str, raw)))) = rx.blocking_recv() {
                                if let Ok(cc) = cc_str.parse::<u8>() {
                                    let raw_byte = raw.clamp(0.0, 127.0) as u8;
                                    debug!("MIDI out SET {path} {value:.4} → CC{cc} {raw_byte}");
                                    let _ = conn.send(&[0xB0 | ch_byte, cc, raw_byte]);
                                }
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
