use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_ctrl()           -> [f32; 2]   { [0.0, 127.0]   }
fn default_param()          -> [f32; 2]   { [0.0, 1.0]     }
fn default_baud()           -> u32        { 115_200         }
fn default_host()           -> String     { "0.0.0.0".into() }
fn default_true()           -> bool       { true            }
fn default_channel()        -> MidiChannel { MidiChannel::Omni }
fn default_midi_out_channel() -> u8       { 1               }

// ---------------------------------------------------------------------------
// ControlDef
// ---------------------------------------------------------------------------

/// Bidirectional mapping between a controller's native range and a parameter range.
///
/// - `ctrl`:  native controller range (e.g. `[0, 127]` for MIDI CC, `[0, 1023]` for 10-bit ADC).
/// - `param`: target parameter range.
///
/// Transformation is linear and symmetric:
/// ```text
/// param_val = param[0] + (ctrl_val  - ctrl[0])  / (ctrl[1]  - ctrl[0])  * (param[1] - param[0])
/// ctrl_val  = ctrl[0]  + (param_val - param[0]) / (param[1] - param[0]) * (ctrl[1]  - ctrl[0])
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlDef {
    /// Target parameter path, e.g. `"04-delay.feedback"`.
    pub target: String,

    /// Controller native range.  Default: `[0.0, 127.0]` (7-bit MIDI).
    #[serde(default = "default_ctrl")]
    pub ctrl: [f32; 2],

    /// Parameter value range.  Default: `[0.0, 1.0]`.
    #[serde(default = "default_param")]
    pub param: [f32; 2],

    /// Decimal places in outbound `CTRL` values.
    /// `0` = integer, `1` = one decimal, etc.
    /// Default: auto — `0` when ctrl range > 100, full float otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u8>,
}

impl ControlDef {
    /// Map a raw controller value to a parameter value.
    pub fn to_param(&self, ctrl_val: f32) -> f32 {
        let t = ((ctrl_val - self.ctrl[0]) / (self.ctrl[1] - self.ctrl[0])).clamp(0.0, 1.0);
        self.param[0] + t * (self.param[1] - self.param[0])
    }

    /// Map a parameter value back to a raw controller value.
    pub fn to_ctrl(&self, param_val: f32) -> f32 {
        let t = ((param_val - self.param[0]) / (self.param[1] - self.param[0])).clamp(0.0, 1.0);
        self.ctrl[0] + t * (self.ctrl[1] - self.ctrl[0])
    }

    /// Format the raw controller value for transmission.
    ///
    /// Uses `round` if set; otherwise auto-detects:
    /// - ctrl range > 100 → integer (0 decimals)
    /// - ctrl range ≤ 100 → full float precision
    pub fn to_ctrl_str(&self, param_val: f32) -> String {
        let raw = self.to_ctrl(param_val);
        let range = (self.ctrl[1] - self.ctrl[0]).abs();
        let decimals = self.round.unwrap_or(if range > 100.0 { 0 } else { u8::MAX });
        match decimals {
            0         => format!("{}", raw.round() as i64),
            u8::MAX   => format!("{raw}"),
            n         => format!("{raw:.prec$}", prec = n as usize),
        }
    }
}

// ---------------------------------------------------------------------------
// MidiChannel
// ---------------------------------------------------------------------------

/// MIDI channel selector: a specific channel (1–16) or `"*"` for omni.
#[derive(Debug, Clone)]
pub enum MidiChannel {
    Omni,
    Number(u8),
}

impl MidiChannel {
    pub fn matches(&self, channel: u8) -> bool {
        match self {
            MidiChannel::Omni       => true,
            MidiChannel::Number(ch) => *ch == channel,
        }
    }
}

impl Serialize for MidiChannel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MidiChannel::Omni      => s.serialize_str("*"),
            MidiChannel::Number(n) => s.serialize_u8(*n),
        }
    }
}

impl<'de> Deserialize<'de> for MidiChannel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = MidiChannel;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "MIDI channel number (1–16) or \"*\" for omni")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<MidiChannel, E> {
                Ok(MidiChannel::Number(v as u8))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<MidiChannel, E> {
                Ok(MidiChannel::Number(v as u8))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<MidiChannel, E> {
                if v == "*" {
                    Ok(MidiChannel::Omni)
                } else {
                    v.parse::<u8>().map(MidiChannel::Number).map_err(serde::de::Error::custom)
                }
            }
        }
        d.deserialize_any(Visitor)
    }
}

// ---------------------------------------------------------------------------
// DeviceDef
// ---------------------------------------------------------------------------

/// Connection-level device configuration.
/// Defined once at root level under `"devices"`, referenced by alias in `ControllerDef`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DeviceDef {
    /// USB/UART serial port.
    Serial {
        dev:  String,
        #[serde(default = "default_baud")]
        baud: u32,
        #[serde(default = "default_true")]
        fallback: bool,
        #[serde(default = "default_true")]
        active: bool,
    },

    /// TCP server — accepts `CTRL`, `SET`, `UPDATE`, `PATCH`, `RESET`, `PROGRAM`.
    Net {
        #[serde(default = "default_host")]
        host: String,
        port: u16,
        #[serde(default = "default_true")]
        fallback: bool,
        #[serde(default = "default_true")]
        active: bool,
    },

    /// MIDI input: receives CC → mapped params, Program Change → preset switch.
    MidiIn {
        #[serde(default)]
        dev: Option<String>,
        #[serde(default = "default_channel")]
        channel: MidiChannel,
        #[serde(default = "default_true")]
        active: bool,
    },

    /// MIDI output: sends CC for mapped parameter changes.
    MidiOut {
        #[serde(default)]
        dev: Option<String>,
        #[serde(default = "default_midi_out_channel")]
        channel: u8,
        #[serde(default = "default_true")]
        active: bool,
    },
}

impl DeviceDef {
    pub fn is_active(&self) -> bool {
        match self {
            DeviceDef::Serial { active, .. } => *active,
            DeviceDef::Net    { active, .. } => *active,
            DeviceDef::MidiIn { active, .. } => *active,
            DeviceDef::MidiOut{ active, .. } => *active,
        }
    }
}


// ---------------------------------------------------------------------------
// ControllerDef
// ---------------------------------------------------------------------------

/// Per-preset controller binding: links a device alias to parameter mappings.
///
/// One entry per device referenced in a preset.  On a preset switch all previous
/// mappings are cleared and replaced with the new preset's `controllers` list.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ControllerDef {
    /// Alias matching a key in `Config.devices`.
    pub device: String,

    /// MIDI channel override (MidiIn only).  If absent, inherits the channel from `DeviceDef`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<MidiChannel>,

    /// key → ControlDef mapping.
    /// Keys are CC number strings (`"70"`) for MIDI, or channel IDs (`"ctrl_1"`) for serial/net.
    #[serde(default)]
    pub mappings: HashMap<String, ControlDef>,
}

impl ControllerDef {
    /// Reverse-lookup: find `(channel_id, def)` for a given parameter target (outbound mapping).
    pub fn channel_for_target(&self, target: &str) -> Option<(&str, &ControlDef)> {
        self.mappings.iter()
            .find(|(_, def)| def.target == target)
            .map(|(ch, def)| (ch.as_str(), def))
    }
}
