use serde::{Serialize, Deserialize};
use tracing::{warn};
use anyhow::{Result, anyhow};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Param metadata (ParamInfo / ParamType)
// ---------------------------------------------------------------------------
//
// Each effect declares its parameter list via `get_params_info()` (one entry per
// settable param). The frontend uses this to render generic tiles without
// knowing about each effect type, and the plugin host uses it to expose
// plugin parameters uniformly.
//
// The shape is a discriminated union: kind-specific data lives inside the
// `ParamType` variant, so each parameter only carries fields that actually
// apply to it. Adding a new variant requires updating the renderer; the
// type system enforces exhaustiveness.

/// What kind of control a parameter is, plus the metadata that's only
/// meaningful for that kind.
///
/// Variant names describe the *data shape* (continuous range vs discrete
/// labeled set) and *value type*.
///
/// `Copy` so `ParamInfo` is `Copy` and `const fn` builders work without
/// invoking a destructor — the canonical lives in flash, all variant
/// payloads are either scalar or `&'static [_]`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "type")]
pub enum ParamType {
    /// Continuous numeric value within `[min, max]`.
    ContinuousFloat {
        min:     f32,
        max:     f32,
        default: f32,
        /// Display unit shown alongside the value (e.g. `"Hz"`, `"dB"`, `"ms"`).
        /// `None` for unitless params.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit:    Option<&'static str>,
        /// Logarithmic interpolation across the range. Useful for frequencies.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        log:     bool,
        #[serde(skip)]
        round_multiplier: f32,
    },
    /// Continuous integer value within `[min, max]`.
    ContinuousInt {
        min:     i32,
        max:     i32,
        default: i32,
        /// Display unit shown alongside the value (e.g. `"Hz"`, `"dB"`, `"ms"`).
        /// `None` for unitless params.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit:    Option<&'static str>,
    },
    /// Discrete choice from a fixed set of float values. Each option has a
    /// label and the numeric value sent to the device on selection.
    DiscreteFloat {
        options: &'static [DiscreteFloatOption],
        default: f32,
    },

    /// Two-state boolean control. Most renders use the parameter's own name
    /// as the label; `labels` overrides with a distinct off/on pair (e.g.
    /// `Some(("Manual".into(), "Auto".into()))` — order is `(false, true)`).
    DiscreteBool {
        default: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        labels:  Option<(&'static str, &'static str)>,
    },

    /// Momentary action endpoint — no current value, no default. The param
    /// has a closed list of action verbs it can dispatch; pressing one fires
    /// `set_action(name, action)`. Single-button events use a one-element
    /// `actions` list (no labels: actions carry no per-instance data, so the
    /// overhead is just the enum byte).
    Event {
        actions: &'static [EventAction],
    },
}

/// One option in a `DiscreteFloat` parameter.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DiscreteFloatOption {
    /// Label shown in the UI (e.g. `"eq-low-pass"`). Should be translated
    /// to a human readable label in the UI.
    pub label: &'static str,
    /// Numeric value sent to the device when this option is selected.
    pub value: f32,
}

/// Action verbs an `Event` parameter can dispatch. Closed vocabulary —
/// curated across the effect library; not every effect uses every variant.
/// Wire form is kebab-case (e.g. `Rec` → `"rec"`); UIs label by combining
/// `ParamInfo.name` with the action (translation happens at the UI layer).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventAction {
    /// Looper: start recording.
    Rec,
    /// Looper: start/resume playback.
    Play,
    /// Looper: stop transport.
    Stop,
    /// Looper: pause transport.
    Pause,
    /// Looper: undo last overdub.
    Undo,
    /// Looper: clear the loop and reset state.
    Clear,
    /// Generic reset (effect-defined semantics).
    Reset,
    /// Tap-tempo input (delay, looper, etc.).
    Tap,
    /// Freeze / hold (reverb sustain, delay infinite repeat).
    Freeze,
}

/// Which aspect of a targeted live `ParamMeta` a meta-entry edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum MetaAspect { Min, Max, Default, Step, Log }

/// Role of a `ParamInfo` entry within a canonical list.
///
/// `ParamMeta` entries are live params (knob / toggle on the tile).
/// `TypeMeta` / `InstanceMeta` entries are override-form descriptors —
/// their `name` matches the targeted live `ParamMeta`'s name; `aspect`
/// disambiguates which bound (Min / Max / Default / ...) the entry edits.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "tag")]
pub enum ParamKind {
    /// Live param. `max_growable_at_runtime: false` means the max is locked
    /// at construction (delay buffer, chorus depth, looper init buf). Override
    /// attempts to grow it past the construction-time max are rejected by the
    /// resolver — master surfaces a reload-required event to the UI.
    ParamMeta { max_growable_at_runtime: bool },
    /// Per-effect-type bound editor — appears in the global config form.
    TypeMeta { aspect: MetaAspect },
    /// Per-instance bound editor — appears in the tile settings tab.
    InstanceMeta { aspect: MetaAspect },
}

/// Metadata describing one parameter of an effect.
///
/// Universal fields only — kind-specific data (range, options, etc.) lives
/// inside `ParamType`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParamInfo {
    /// Short machine-friendly name, matches the `set_param` / `set_action`
    /// key for built-in effects (e.g. `"wet"`, `"room_size"`, `"action"`).
    /// Not required to be unique within an effect's param list —
    /// multiple entries may share a name to target the same wire endpoint
    /// with different render styles (e.g. a knob view and a slider view of
    /// the same underlying value).
    pub name:  &'static str,
    /// What kind of control this is, with the data needed to render it.
    /// `#[serde(flatten)]` lifts the variant's fields (and its `"type"` tag)
    /// up into the `ParamInfo` JSON object — wire shape stays flat.
    #[serde(flatten)]
    pub data_kind: ParamType,
    /// Role of this entry — live param vs override-form descriptor.
    /// Defaults to `ParamMeta { max_growable_at_runtime: true }` at
    /// construction; override-form entries are tagged via
    /// `with_kind_type_meta(aspect)` / `with_kind_instance_meta(aspect)`.
    pub kind: ParamKind,
}
impl ParamInfo {
    pub const fn new_continuous_float(name: &'static str, min: f32, max: f32, default: f32,
        log: bool, unit: Option<&'static str>) -> Self {
        // round_multiplier left at 0.0 — `build_info` computes it from the
        // settled (post-override) min/max range.
        ParamInfo {
            name,
            data_kind: ParamType::ContinuousFloat { min, max, default, log, unit, round_multiplier: 0.0 },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
        }
    }
    pub const fn new_continuous_int(name: &'static str, min: i32, max: i32, default: i32,
        unit: Option<&'static str>) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::ContinuousInt { min, max, default, unit },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
        }
    }
    pub const fn new_discrete_bool(name: &'static str, default: bool,
        labels: Option<(&'static str, &'static str)>,) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::DiscreteBool { default, labels },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
        }
    }

    // ----- Builders for orthogonal aspects (chained on top of constructors) -----

    /// Lock the live param's max at construction time (sizes a buffer, etc.).
    /// Override attempts to grow `<param>.max` past construction-time max are
    /// rejected by the resolver — master surfaces a reload-required event.
    /// Only valid on `ParamMeta` entries; panics otherwise (compile error in
    /// const contexts).
    pub const fn with_non_growable(self) -> Self {
        match self.kind {
            ParamKind::ParamMeta { .. } => Self {
                kind: ParamKind::ParamMeta { max_growable_at_runtime: false },
                ..self
            },
            _ => panic!("with_non_growable: only valid for ParamMeta entries"),
        }
    }

    /// Tag this entry as a per-effect-type bound editor for the targeted live
    /// param (matched by `name`). `aspect` selects which bound is edited.
    pub const fn with_kind_type_meta(self, aspect: MetaAspect) -> Self {
        Self {
            kind: ParamKind::TypeMeta { aspect },
            ..self
        }
    }

    /// Tag this entry as a per-instance bound editor for the targeted live
    /// param. Same shape as `with_kind_type_meta` but applied per-tile.
    pub const fn with_kind_instance_meta(self, aspect: MetaAspect) -> Self {
        Self {
            kind: ParamKind::InstanceMeta { aspect },
            ..self
        }
    }

    pub fn continuous_float_default(&self) -> f32 {
        match &self.data_kind {
            ParamType::ContinuousFloat { default, .. } => *default,
            _ => panic!("{}: expected ContinuousFloat", self.name),
        }
    }
    pub fn continuous_float_min(&self) -> f32 {
        match &self.data_kind {
            ParamType::ContinuousFloat { min, .. } => *min,
            _ => panic!("{}: expected ContinuousFloat", self.name),
        }
    }
    pub fn continuous_float_max(&self) -> f32 {
        match &self.data_kind {
            ParamType::ContinuousFloat { max, .. } => *max,
            _ => panic!("{}: expected ContinuousFloat", self.name),
        }
    }
    pub fn continuous_int_default(&self) -> i32 {
        match &self.data_kind {
            ParamType::ContinuousInt { default, .. } => *default,
            _ => panic!("{}: expected ContinuousInt", self.name),
        }
    }
    pub fn continuous_int_min(&self) -> i32 {
        match &self.data_kind {
            ParamType::ContinuousInt { min, .. } => *min,
            _ => panic!("{}: expected ContinuousInt", self.name),
        }
    }
    pub fn continuous_int_max(&self) -> i32 {
        match &self.data_kind {
            ParamType::ContinuousInt { max, .. } => *max,
            _ => panic!("{}: expected ContinuousInt", self.name),
        }
    }
    pub fn bool_default(&self) -> bool {
        match &self.data_kind {
            ParamType::DiscreteBool { default, .. } => *default,
            _ => panic!("{}: expected DiscreteBool", self.name),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum OverrideValue { Float(f32), Int(i32), Bool(bool) }
impl OverrideValue {
    pub fn try_float(&self) -> Result<f32> {
        match self {
            OverrideValue::Float(v) => Ok(*v),
            _ => Err(anyhow!("expected a float value")),
        }
    }
    pub fn try_int(&self) -> Result<i32> {
        match self {
            OverrideValue::Int(v) => Ok(*v),
            _ => Err(anyhow!("expected a int value")),
        }
    }
    pub fn try_bool(&self) -> Result<bool> {
        match self {
            OverrideValue::Bool(v) => Ok(*v),
            _ => Err(anyhow!("expected a bool value")),
        }
    }
}

/// A stereo audio frame: [left, right]
pub type Frame = [f32; 2];

/// A parameter value: either a scalar float or a per-channel stereo pair.
///
/// Effects that have a `wet` parameter accept both variants —
/// `Float(x)` is treated as `Stereo([x, x])` via [`ParamValue::try_stereo`].
/// Scalar-only parameters (e.g. `feedback`, `time`) call [`ParamValue::try_float`]
/// and return an error if a stereo pair is passed.
#[derive(Debug, Clone, Copy)]
pub enum ParamValue {
    Float(f32),
    Stereo([f32; 2]),
    Bool(bool),
}

impl std::fmt::Display for ParamValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamValue::Float(v)      => write!(f, "{v}"),
            ParamValue::Stereo([l,r]) => write!(f, "[{l},{r}]"),
            ParamValue::Bool(b)       => write!(f, "{b}"),
        }
    }
}

impl ParamValue {
    /// Convert to `[left, right]`, or an error if the variant cannot be represented as stereo.
    ///
    /// `Float(x)` is promoted to `[x, x]`.
    pub fn try_stereo(self) -> Result<[f32; 2], String> {
        match self {
            ParamValue::Float(v)    => Ok([v, v]),
            ParamValue::Stereo(arr) => Ok(arr),
            ParamValue::Bool(b)     => { let v = if b { 1.0 } else { 0.0 }; Ok([v, v]) }
        }
    }

    /// Return the scalar value, or an error if the variant cannot be represented as a single float.
    pub fn try_float(self) -> Result<f32, String> {
        match self {
            ParamValue::Float(v)   => Ok(v),
            ParamValue::Stereo(_)  => Err("expected a scalar value, got a stereo pair".into()),
            ParamValue::Bool(b)    => Ok(if b { 1.0 } else { 0.0 }),
        }
    }

    /// Return a bool. `Float` is accepted: 0.0 = false, anything else = true (for TCP compat).
    pub fn try_bool(self) -> Result<bool, String> {
        match self {
            ParamValue::Bool(b)   => Ok(b),
            ParamValue::Float(v)  => Ok(v > 0.5),
            ParamValue::Stereo(_) => Err("expected a bool, got a stereo pair".into()),
        }
    }
}

impl From<f32> for ParamValue {
    fn from(v: f32) -> Self { ParamValue::Float(v) }
}

impl From<[f32; 2]> for ParamValue {
    fn from(arr: [f32; 2]) -> Self { ParamValue::Stereo(arr) }
}

// ---------------------------------------------------------------------------
// Override helpers
// ---------------------------------------------------------------------------

/// Structured override key — replaces the legacy stringly-keyed
/// `"effect.param.aspect"` composite. The targeted live `ParamMeta`'s
/// `name` is `param`; `aspect` selects which bound (Min/Max/Default/...).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetaTarget {
    pub param:  String,
    pub aspect: MetaAspect,
}

/// Per-effect override map. Master pre-filters the global config map to the
/// entries for a given effect type before passing in to `Effect::new`.
pub type OverrideMap = HashMap<MetaTarget, OverrideValue>;

/// Resolve a per-instance `params_info` array from the canonical metadata
/// + type-3 + type-2 overrides. Each layer narrows; canonical is the
/// absolute envelope.
///
/// Resolution order (each step clamps to the previous step's result):
/// 1. Start with each `ParamMeta` entry from canonical (cloned).
/// 2. **TODO**: apply each `TypeMeta` override, clamped to canonical bounds.
/// 3. **TODO**: apply each `InstanceMeta` override, clamped to type-3-resolved.
/// 4. **TODO**: if an applied max would exceed construction-time max AND
///    `max_growable_at_runtime == false`, reject and signal reload-required.
/// 5. Compute `round_multiplier` on every `ContinuousFloat` from the final
///    (post-override) min/max range.
pub fn build_info<const N: usize>(
    canonical: &[ParamInfo; N],
    _type_overrides: &OverrideMap,
    _instance_overrides: &OverrideMap,
) -> [ParamInfo; N] {
    let mut resolved = canonical.clone();
    // Final pass: compute round_multiplier from settled live bounds.
    for info in resolved.iter_mut() {
        if let ParamType::ContinuousFloat { min, max, round_multiplier, .. } = &mut info.data_kind {
            *round_multiplier = auto_multiplier(*min, *max);
        }
    }
    resolved
}

pub fn override_float(map: &HashMap<String, OverrideValue>, key: &str, default: f32) -> f32 {
    let Some(v) = map.get(key) else { return default; };
    match v.try_float() {
        Ok(x)  => x,
        Err(e) => {
            warn!("invalid override {key}: {e}; using default {default}");
            default
        }
    }
}
pub fn override_int(map: &HashMap<String, OverrideValue>, key: &str, default: i32) -> i32 {
    let Some(v) = map.get(key) else { return default; };
    match v.try_int() {
        Ok(x)  => x,
        Err(e) => {
            warn!("invalid override {key}: {e}; using default {default}");
            default
        }
    }
}
pub fn override_bool(map: &HashMap<String, OverrideValue>, key: &str, default: bool) -> bool {
    let Some(v) = map.get(key) else { return default; };
    match v.try_bool() {
        Ok(x)  => x,
        Err(e) => {
            warn!("invalid override {key}: {e}; using default {default}");
            default
        }
    }
}

// ---------------------------------------------------------------------------
// Parameterized trait
// ---------------------------------------------------------------------------

/// Named parameter access.
///
/// Implemented by effects, mix nodes, loopers, and chains.
/// Separating this from [`Device`] allows non-audio nodes (like `MixNode`)
/// to participate in the same parameter system without implementing the full
/// audio processing interface.
pub trait Parameterized {
    /// Get all parameters of the effect.
    fn get_params_info(&self) -> &[ParamInfo];

    /// Set a named parameter.
    ///
    /// Parameters that accept stereo values (e.g. `wet`) take either a
    /// `Float` (applied to both channels) or a `Stereo` pair.
    /// Scalar-only parameters return an error when given a `Stereo` value.
    fn set_param(&mut self, param: &str, _value: ParamValue) -> Result<(), String> {
        Err(format!("unknown param '{param}'"))
    }
}

/// Allow `Box<dyn Device>` to be used where `Parameterized` is expected.
impl Parameterized for Box<dyn Device> {
    fn get_params_info(&self) -> &[ParamInfo] {
        (**self).get_params_info()
    }
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        (**self).set_param(param, value)
    }
}

// ---------------------------------------------------------------------------
// Device trait
// ---------------------------------------------------------------------------

/// Central abstraction for every audio-processing node in the signal graph.
///
/// Effects output **only the wet signal** — no dry component.
/// Dry is added externally by the `Chain` before each node.
///
/// **Realtime safety**: `process` is called from the audio thread.
/// No allocations, no locks, no panics.
pub trait Device: Parameterized + Send + Sync {
    /// Process one block.
    ///
    /// `dry`  – the original (unprocessed) input for this chain.
    /// `eff`  – on entry: accumulated effect signal from previous nodes (`prev_eff`);
    ///          on exit:  new effect output for this node.
    ///
    /// Each device computes `inp = dry + eff` per frame as needed, then
    /// overwrites `eff` with its output.  `dry.len() == eff.len()` is guaranteed.
    fn process(&mut self, dry: &[Frame], eff: &mut [Frame]);

    /// Reset internal state (e.g. delay lines, filter state).
    fn reset(&mut self);

    /// Return the node key assigned at build time.
    fn key(&self) -> &str;

    /// The node type string used in patch JSON (e.g. "delay", "reverb").
    fn type_name(&self) -> &str;

    /// Whether this node is active. Inactive nodes are skipped entirely in
    /// `Chain::process` — no CPU cost beyond the branch check.
    fn is_active(&self) -> bool { true }

    /// MIDI Control Change
    fn on_cc(&mut self, controller: u8, value: u8) {
        let _ = (controller, value);
    }

    /// MIDI Note On
    fn on_note_on(&mut self, note: u8, velocity: u8) {
        let _ = (note, velocity);
    }

    /// MIDI Note Off
    fn on_note_off(&mut self, note: u8) {
        let _ = note;
    }

    /// Called once after the node is built and added to a chain.
    /// Nodes that want to emit events (e.g. Looper) store the bus here.
    fn init_bus(&mut self, _bus: &crate::control::EventBus) {}

    /// Dispatch a named action string (e.g. "rec", "play", "rec-play-stop-rec").
    /// Only meaningful for nodes that have an action-based interface (e.g. Looper).
    fn set_action(&mut self, param: &str, action: &str) -> Result<(), String> {
        let _ = (param, action);
        Err(format!("unknown action '{param}'"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn find_param_info<'a>(params_info: &'a [ParamInfo], name: &str) -> &'a ParamInfo {
    params_info.iter().find(|i| i.name == name).unwrap() // unwrap is cool here, since param_info is hard-coded.
}

/// Clamp `value` to `[min, max]` taken from ParamInfo.
///
/// Returns `(clamped, Ok(()))` when in range, or `(clamped, Err(message))` when
/// the value was out of range.  Callers should always assign the returned value
/// so the effect continues to work even when the input was invalid.
pub fn check_bounds(info: &ParamInfo, value: f32, device_name: &str) -> (f32, Result<(), String>) {
    let min = info.continuous_float_min();
    let max = info.continuous_float_max();
    let clamped = value.clamp(min, max);
    if clamped != value {
        (clamped, Err(format!("{device_name}.{}: value {value} out of range [{min}, {max}], clamped to {clamped}", info.name)))
    } else {
        (clamped, Ok(()))
    }
}

/// System-wide target resolution for outbound rounding. Roughly equivalent to
/// 14-bit precision (16384). Picking 10000 gives slightly friendlier decimal
/// counts on common ranges (e.g. 2 decimals on [0,127] instead of 3).
const OUTBOUND_RESOLUTION: f32 = 10000.0;

/// Compute the smallest power-of-10 multiplier `m` such that rounding a value
/// to the nearest `1/m` preserves at least `OUTBOUND_RESOLUTION` steps across
/// the given range.
pub fn auto_multiplier(min: f32, max: f32) -> f32 {
    let range = max - min; // @todo this may be not correct
    let r = range.abs();
    if r <= 0.0 { return 1.0; }
    let d = (OUTBOUND_RESOLUTION / r).log10().ceil().clamp(0.0, 10.0);
    10_f32.powi(d as i32)
}