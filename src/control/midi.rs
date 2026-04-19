use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::config::master::ConfigRequest;
use crate::control::{ControlMessage, EventBus};
use crate::control::mapping::MidiChannel;

// ---------------------------------------------------------------------------
// MidiControl  (MIDI input)
// ---------------------------------------------------------------------------

pub struct MidiControl {
    alias:       String,
    device_name: Option<String>,
    channel:     MidiChannel,
}

impl MidiControl {
    pub fn new(alias: String, device_name: Option<String>, channel: MidiChannel) -> Self {
        Self { alias, device_name, channel }
    }

    /// Channel filtering happens locally (MIDI-native). CC translation is
    /// delegated to master via `ApplyCtrl`.
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
        let channel_filter = self.channel;

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

                // Device-level channel filter (MIDI-native). Program Change is
                // accepted on all channels (it's a global event).
                let is_channel_msg = matches!(msg_type, 0xB0 | 0x90 | 0x80);
                if is_channel_msg && !channel_filter.matches(channel) { return; }

                let source = format!("{alias}-ch{channel}");

                let req = match msg_type {
                    0xB0 if msg.len() >= 3 => {
                        Some(ConfigRequest::ApplyCtrl {
                            channel_id: msg[1].to_string(),
                            raw: msg[2] as f32,
                            alias: alias.clone(),
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
                        Some(ConfigRequest::ApplyControl(cm))
                    }
                    0x80 if msg.len() >= 3 => {
                        Some(ConfigRequest::ApplyControl(ControlMessage::NoteOff { note: msg[1] }))
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
                                    // Encoding follows MIDI spec:
                                    //   CC 0..=31  → 14-bit, MSB on cc, LSB on cc+32
                                    //   CC 32..=63 → reserved for LSB, invalid as a primary CC
                                    //   CC 64..=127 → 7-bit
                                    match cc {
                                        0..=31 => {
                                            // `.round() as u16` (not bare `as u16` which truncates).
                                            let raw_u16 = raw.clamp(0.0, 16383.0).round() as u16;
                                            let msb = ((raw_u16 >> 7) & 0x7F) as u8;
                                            let lsb = (raw_u16 & 0x7F) as u8;
                                            debug!("MIDI out SET {path} {value} → CC{cc} 14-bit (MSB={msb}, LSB={lsb})");
                                            let _ = conn.send(&[0xB0 | ch_byte, cc, msb]);
                                            let _ = conn.send(&[0xB0 | ch_byte, cc + 32, lsb]);
                                        }
                                        32..=63 => {
                                            error!("MIDI out: CC{cc} is reserved for 14-bit LSB (paired with CC{}) — cannot use as primary mapping", cc - 32);
                                        }
                                        64..=127 => {
                                            let raw_u8 = raw.clamp(0.0, 127.0).round() as u8;
                                            debug!("MIDI out SET {path} {value} → CC{cc} {raw_u8}");
                                            let _ = conn.send(&[0xB0 | ch_byte, cc, raw_u8]);
                                        }
                                        _ => {
                                            error!("MIDI out: CC{cc} out of range (valid: 0..=127)");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ControlMessage::PresetLoaded { preset, .. } => {
                        if preset.index != 0 {
                            let _ = conn.send(&[0xC0 | ch_byte, preset.index]);
                        }
                    }
                    ControlMessage::Reset { .. }
                    | ControlMessage::NoteOn         { .. }
                    | ControlMessage::NoteOff        { .. }
                    | ControlMessage::Action         { .. }
                    | ControlMessage::NodeEvent      { .. }
                    | ControlMessage::StateChanged   { .. }
                                     => {}
                }
            }
        });
    }
}
