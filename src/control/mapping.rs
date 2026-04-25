use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_ctrl()           -> [f32; 2]   { [0.0, 127.0]   }
fn default_param()          -> [f32; 2]   { [0.0, 1.0]     }
fn default_baud()           -> u32        { 115_200         }
fn default_host()           -> String     { "0.0.0.0".into() }
fn default_true()           -> bool       { true            }
fn default_channel()        -> MidiChannel { MidiChannel::Omni }
fn default_midi_out_channel() -> u8       { 1               }

/// System-wide target resolution for outbound rounding. Roughly equivalent to
/// 14-bit precision (16384). Picking 10000 gives slightly friendlier decimal
/// counts on common ranges (e.g. 2 decimals on [0,127] instead of 3).
const OUTBOUND_RESOLUTION: f32 = 10000.0;

/// Compute the smallest power-of-10 multiplier `m` such that rounding a value
/// to the nearest `1/m` preserves at least `OUTBOUND_RESOLUTION` steps across
/// the given range.
fn auto_multiplier(range: f32) -> f32 {
    let r = range.abs();
    if r <= 0.0 { return 1.0; }
    let d = (OUTBOUND_RESOLUTION / r).log10().ceil().clamp(0.0, 10.0);
    10_f32.powi(d as i32)
}

/// Round a value to the nearest `1/multiplier`.
fn smart_round(value: f32, multiplier: f32) -> f32 {
    (value * multiplier).round() / multiplier
}

// ---------------------------------------------------------------------------
// ControlDef
// ---------------------------------------------------------------------------

/// Bidirectional mapping between a controller's native range and a parameter range.
///
/// - `ctrl`:  native controller range (e.g. `[0, 127]` for MIDI CC, `[0, 1023]` for 10-bit ADC).
/// - `param`: target parameter range.
/// - `log`:   when `true`, use logarithmic interpolation: `param = param[0] * (param[1]/param[0])^t`.
///            Useful for frequency parameters where musical spacing is logarithmic.
///
/// Linear transformation (default):
/// ```text
/// param_val = param[0] + t * (param[1] - param[0])   where t = (ctrl_val - ctrl[0]) / (ctrl[1] - ctrl[0])
/// ```
/// Logarithmic transformation (`log: true`):
/// ```text
/// param_val = param[0] * (param[1] / param[0]) ^ t
/// ```
///
/// ## Construction
///
/// `ControlDef` can only be constructed via:
/// - `serde::Deserialize` (goes through [`ControlDefRaw`] and runs [`Self::new`]).
/// - [`Self::new`] — the single public constructor. Initializes cached multipliers.
/// - `Clone` of an existing (already-initialized) instance.
///
/// Struct-literal construction from outside this module is impossible because
/// the `ctrl_mult` / `target_mult` fields are private; this guarantees every
/// live `ControlDef` has valid cached multipliers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "ControlDefRaw")]
pub struct ControlDef {
    /// Target parameter path, e.g. `"04-delay.feedback"`.
    pub target: String,

    /// Controller native range.  Default: `[0.0, 127.0]` (7-bit MIDI).
    pub ctrl: [f32; 2],

    /// Parameter value range.  Default: `[0.0, 1.0]`.
    pub param: [f32; 2],

    /// Use logarithmic interpolation.  Default: `false`.
    /// Both `param` values must be positive (non-zero) when enabled.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub log: bool,

    // --- cached, private (so outside code cannot skip init via struct literal) ---

    /// Precomputed rounding multiplier for ctrl-space values.
    ///
    /// Stored as `10^d` where `d = ceil(log10(OUTBOUND_RESOLUTION / ctrl_range))`.
    /// Used by [`smart_round_ctrl`] to round outbound wire values to a decimal-aligned
    /// step that preserves the system's target resolution without cluttering output
    /// with meaningless precision.
    #[serde(skip)]
    ctrl_mult: f32,

    /// Precomputed rounding multiplier for target (param-space) values.
    ///
    /// Same formula as `ctrl_mult`, but based on the `param` range. Used by
    /// [`smart_round_target`] when emitting param values as text (e.g. `SET` lines
    /// for paths where no reverse mapping exists but the param range is known).
    #[serde(skip)]
    target_mult: f32,
}

/// Wire-format shadow of [`ControlDef`] used only for deserialization.
///
/// `ControlDef` routes deserialization here via `#[serde(from = "ControlDefRaw")]`,
/// which then runs [`ControlDef::new`] to populate the cached multipliers.
/// Serialization goes directly through the derived impl on `ControlDef`, with
/// the private fields skipped — no parallel path needed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlDefRaw {
    target: String,
    #[serde(default = "default_ctrl")]
    ctrl: [f32; 2],
    #[serde(default = "default_param")]
    param: [f32; 2],
    #[serde(default)]
    log: bool,
}

impl From<ControlDefRaw> for ControlDef {
    fn from(r: ControlDefRaw) -> Self {
        ControlDef::new(r.target, r.ctrl, r.param, r.log)
    }
}

impl ControlDef {
    /// The single public constructor. Computes and caches the rounding multipliers
    /// from the given ranges. All other construction paths (serde, clone) funnel
    /// through here.
    pub fn new(target: String, ctrl: [f32; 2], param: [f32; 2], log: bool) -> Self {
        Self {
            target, ctrl, param, log,
            ctrl_mult:   auto_multiplier(ctrl[1]  - ctrl[0]),
            target_mult: auto_multiplier(param[1] - param[0]),
        }
    }

    /// Map a raw controller value to a parameter value.
    pub fn to_param(&self, ctrl_val: f32) -> f32 {
        let t = ((ctrl_val - self.ctrl[0]) / (self.ctrl[1] - self.ctrl[0])).clamp(0.0, 1.0);
        if self.log && self.param[0] > 0.0 && self.param[1] > 0.0 {
            self.param[0] * (self.param[1] / self.param[0]).powf(t)
        } else {
            self.param[0] + t * (self.param[1] - self.param[0])
        }
    }

    /// Map a parameter value back to a raw controller value.
    pub fn to_ctrl(&self, param_val: f32) -> f32 {
        let t = if self.log && self.param[0] > 0.0 && self.param[1] > 0.0 {
            (param_val.max(self.param[0]) / self.param[0]).ln()
                / (self.param[1] / self.param[0]).ln()
        } else {
            (param_val - self.param[0]) / (self.param[1] - self.param[0])
        };
        self.ctrl[0] + t.clamp(0.0, 1.0) * (self.ctrl[1] - self.ctrl[0])
    }

    /// Round a ctrl-space value to the mapping's outbound resolution.
    ///
    /// Call this on any f32 that's about to become text on the wire (CTRL frames).
    /// The result has just enough decimal precision to preserve the system's
    /// target resolution across this mapping's `ctrl` range — nothing more,
    /// nothing less. No log/exp per call; uses the cached multiplier.
    ///
    /// Example: with `ctrl = [0, 127]` and `OUTBOUND_RESOLUTION = 10000`, the
    /// multiplier is 100 (2 decimals). `smart_round_ctrl(63.4871)` → `63.49`.
    pub fn smart_round_ctrl(&self, value: f32) -> f32 {
        smart_round(value, self.ctrl_mult)
    }

    /// Round a target (param-space) value to the mapping's outbound resolution.
    ///
    /// Same idea as [`smart_round_ctrl`] but based on the `param` range.
    /// Useful when a parameter change needs to be emitted as text on a device
    /// that can understand the full param-space (e.g. SET frames) rather than
    /// the controller's native units.
    pub fn smart_round_target(&self, value: f32) -> f32 {
        smart_round(value, self.target_mult)
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
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DeviceDef {
    /// USB/UART serial port.
    Serial {
        dev:  String,
        #[serde(default = "default_baud")]
        baud: u32,
        #[serde(default = "default_true")]
        active: bool,
    },

    /// TCP server — accepts `CTRL`, `SET`, `CHAINS`, `RESET`, `PRESET`, `SAVE_PRESET`.
    Net {
        #[serde(default = "default_host")]
        host: String,
        port: u16,
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
///
/// Note: MIDI channel filtering is a device-level concern (see `DeviceDef::MidiIn.channel`),
/// not a per-preset override.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ControllerDef {
    /// Alias matching a key in `Config.control_devices`.
    pub device: String,

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
