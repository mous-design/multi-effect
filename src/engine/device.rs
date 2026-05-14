use serde::{Serialize, Deserialize};
use tracing::warn;
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
///
/// `Min`/`Max`/`Default`/`Step`/`Log` describe numeric bounds; `Visible`
/// is the presentation flag (knob shown / hidden on the tile). Putting them
/// in one enum keeps the override pipeline uniform — every per-param attribute
/// flows through `apply_override` and the meta-form `SET` regardless of whether
/// it's a bound or a presentation hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum MetaAspect { Min, Max, Default, Step, Log, Visible }

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
    /// Whether the UI should render this param's knob on the tile.
    /// Defaults to `true` at construction; canonical entries can flip it
    /// via `with_hidden()`, and per-instance overrides toggle it at runtime
    /// via `SET <key>.<param>.visible <bool>` (meta form).
    pub visible: bool,
}
impl ParamInfo {
    pub const fn new_continuous_float(name: &'static str, min: f32, max: f32, default: f32,
        log: bool, unit: Option<&'static str>) -> Self {
        assert!(!log || (min > 0.0 && max > 0.0), "Can only have log with param always > 0");
        // round_multiplier left at 0.0 — `build_info` computes it from the
        // settled (post-override) min/max range.
        ParamInfo {
            name,
            data_kind: ParamType::ContinuousFloat { min, max, default, log, unit, round_multiplier: 0.0 },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
            visible: true,
        }
    }
    pub const fn new_continuous_int(name: &'static str, min: i32, max: i32, default: i32,
        unit: Option<&'static str>) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::ContinuousInt { min, max, default, unit },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
            visible: true,
        }
    }
    pub const fn new_discrete_bool(name: &'static str, default: bool,
        labels: Option<(&'static str, &'static str)>,) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::DiscreteBool { default, labels },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true },
            visible: true,
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

    /// Mark this param hidden by default in the UI. Per-preset overrides
    /// can still flip it back on via `SET <key>.<param>.visible true` (meta form).
    #[allow(dead_code)]
    pub const fn with_hidden(self) -> Self {
        Self { visible: false, ..self }
    }

    /// Tag this entry as a per-effect-type bound editor for the targeted live
    /// param (matched by `name`). `aspect` selects which bound is edited.
    #[allow(dead_code)]
    pub const fn with_kind_type_meta(self, aspect: MetaAspect) -> Self {
        Self {
            kind: ParamKind::TypeMeta { aspect },
            ..self
        }
    }

    /// Tag this entry as a per-instance bound editor for the targeted live
    /// param. Same shape as `with_kind_type_meta` but applied per-tile.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn continuous_int_min(&self) -> i32 {
        match &self.data_kind {
            ParamType::ContinuousInt { min, .. } => *min,
            _ => panic!("{}: expected ContinuousInt", self.name),
        }
    }
    #[allow(dead_code)]
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

    /// The canonical default as a `ParamValue`. Used to fill in sparse param
    /// maps at the get-or-default boundary (UI, master inspection). `Event`
    /// has no value semantics — panics; callers should never look up an
    /// Event's "value."
    pub fn default_as_param_value(&self) -> ParamValue {
        match &self.data_kind {
            ParamType::ContinuousFloat { default, .. } => ParamValue::Float(*default),
            ParamType::ContinuousInt   { default, .. } => ParamValue::Int(*default),
            ParamType::DiscreteFloat   { default, .. } => ParamValue::Float(*default),
            ParamType::DiscreteBool    { default, .. } => ParamValue::Bool(*default),
            ParamType::Event { .. } => panic!("{}: Event has no value", self.name),
        }
    }
}

/// A stereo audio frame: [left, right]
pub type Frame = [f32; 2];

/// A parameter value — the unified type for every channel that carries values:
/// runtime knob updates (`set_param`), bound overrides (`Config.type_overrides`,
/// `apply_override`), and the wire protocol's `SET <path> <value>` payloads
/// (both 2-segment values and 3-segment meta overrides).
/// Variants are kept open so integers keep their type across the
/// JSON round-trip (no `i32 → f32 → i32` coercion).
///
/// `#[serde(untagged)]` with variant order Int → Float → Bool means JSON `5`
/// deserialises as `Int(5)`, `5.5` as `Float(5.5)`, `true` as `Bool(true)`.
///
/// `try_int` ↔ `try_float` coerce between numeric variants because serde's
/// untagged round-trip is non-deterministic — `1.0` may deserialise as
/// `Int(1)` and `5` as `Int(5)`, so callers can't rely on which variant
/// arrived. `try_int` warns when a fractional `Float` is truncated.
///
/// The `Bool` boundary is strict — bools and numbers don't bridge.
/// A JSON bool always deserialises as `Bool`, never as a number, so there's
/// no determinism reason to be lenient and silent coercion would hide typos.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Int(i32),
    Float(f32),
    Bool(bool),
}

impl std::fmt::Display for ParamValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamValue::Int(v)   => write!(f, "{v}"),
            ParamValue::Float(v) => write!(f, "{v}"),
            ParamValue::Bool(b)  => write!(f, "{b}"),
        }
    }
}

impl ParamValue {
    /// Coerce to `f32`. `Int` widens cleanly. `Bool` is rejected.
    pub fn try_float(self) -> Result<f32, String> {
        match self {
            ParamValue::Float(v) => Ok(v),
            ParamValue::Int(v)   => Ok(v as f32),
            ParamValue::Bool(_)  => Err("expected float, got bool".into()),
        }
    }

    /// Coerce to `i32`. Lossless for `Int` and integral `Float`; warns and
    /// truncates when a `Float` has a fractional part. `Bool` is rejected.
    pub fn try_int(self) -> Result<i32, String> {
        match self {
            ParamValue::Int(v)   => Ok(v),
            ParamValue::Float(v) => {
                if v.fract() != 0.0 {
                    tracing::warn!("ParamValue::try_int: expected integer, got {v} with fractional part — truncating to {}", v as i32);
                }
                Ok(v as i32)
            },
            ParamValue::Bool(_)  => Err("expected int, got bool".into()),
        }
    }

    /// Strict — only accepts `Bool`. Numbers never silently become bools.
    pub fn try_bool(self) -> Result<bool, String> {
        match self {
            ParamValue::Bool(b)  => Ok(b),
            ParamValue::Float(_) => Err("expected bool, got float".into()),
            ParamValue::Int(_)   => Err("expected bool, got int".into()),
        }
    }
}

impl From<f32>  for ParamValue { fn from(v: f32)  -> Self { ParamValue::Float(v) } }
impl From<i32>  for ParamValue { fn from(v: i32)  -> Self { ParamValue::Int(v)   } }
impl From<bool> for ParamValue { fn from(v: bool) -> Self { ParamValue::Bool(v)  } }

// ---------------------------------------------------------------------------
// Override helpers
// ---------------------------------------------------------------------------

/// Structured override key. The targeted live `ParamMeta`'s `name` is `param`;
/// `aspect` selects which bound (Min/Max/Default/...). On the wire / in JSON
/// it serialises as a single string `"param.aspect"`, so the override map
/// reads as a flat object: `{"depth_ms.max": 20, "rate_hz.min": 0.5}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetaTarget {
    pub param:  String,
    pub aspect: MetaAspect,
}

impl MetaTarget {
    fn aspect_str(&self) -> &'static str {
        match self.aspect {
            MetaAspect::Min     => "min",
            MetaAspect::Max     => "max",
            MetaAspect::Default => "default",
            MetaAspect::Step    => "step",
            MetaAspect::Log     => "log",
            MetaAspect::Visible => "visible",
        }
    }

    /// Parse from the wire form `"param.aspect"`, e.g. `"depth_ms.max"`.
    pub fn parse_str(s: &str) -> Result<Self, String> {
        let (param, aspect_str) = s.rsplit_once('.')
            .ok_or_else(|| format!("expected 'param.aspect', got '{s}'"))?;
        let aspect = match aspect_str {
            "min"     => MetaAspect::Min,
            "max"     => MetaAspect::Max,
            "default" => MetaAspect::Default,
            "step"    => MetaAspect::Step,
            "log"     => MetaAspect::Log,
            "visible" => MetaAspect::Visible,
            other     => return Err(format!("unknown aspect '{other}' in '{s}'")),
        };
        Ok(MetaTarget { param: param.to_string(), aspect })
    }
}

impl serde::Serialize for MetaTarget {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}.{}", self.param, self.aspect_str()))
    }
}

impl<'de> serde::Deserialize<'de> for MetaTarget {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        let (param, aspect_str) = s.rsplit_once('.')
            .ok_or_else(|| D::Error::custom(format!("expected 'param.aspect', got '{s}'")))?;
        let aspect = match aspect_str {
            "min"     => MetaAspect::Min,
            "max"     => MetaAspect::Max,
            "default" => MetaAspect::Default,
            "step"    => MetaAspect::Step,
            "log"     => MetaAspect::Log,
            "visible" => MetaAspect::Visible,
            other     => return Err(D::Error::custom(format!("unknown aspect '{other}' in '{s}'"))),
        };
        Ok(MetaTarget { param: param.to_string(), aspect })
    }
}

/// Per-effect override map. Master pre-filters the global config map to the
/// entries for a given effect type before passing in to `build_info`.
pub type OverrideMap = HashMap<MetaTarget, ParamValue>;

/// Resolve a per-instance `params_info` array from the canonical metadata +
/// Type overrides. Type overrides narrow but cannot widen; canonical is the
/// absolute envelope.
///
/// Instance overrides (per-tile bound edits) are applied later by direct
/// mutation of the resolved `params_info` on the live instance — they don't
/// flow through `build_info`. Snapshot serialisation captures them via the
/// instance's `params_info` field.
///
/// Resolution order:
/// 1. Start from canonical.
/// 2. Apply each Type override, clamping to canonical bounds.
/// 3. Compute `round_multiplier` from the final (post-override) min/max.
pub fn build_info(
    canonical: &[ParamInfo],
    type_overrides: &OverrideMap,
) -> Vec<ParamInfo> {
    let mut resolved: Vec<ParamInfo> = canonical.to_vec();

    for (target, value) in type_overrides {
        apply_override(&mut resolved, canonical, target, value);
    }

    // Final pass: compute round_multiplier from settled live bounds.
    for info in resolved.iter_mut() {
        if let ParamType::ContinuousFloat { min, max, round_multiplier, .. } = &mut info.data_kind {
            *round_multiplier = auto_multiplier(*min, *max);
        }
    }
    resolved
}

/// Apply a single override to the targeted `ParamMeta` entry in `resolved`,
/// clamping the value to the matching entry's bounds in `clamp_ref`.
///
/// **The kernel for both build paths**: `build_info` calls this for fresh
/// construction (clamp_ref = canonical); the runtime instance-edit path will
/// call this when the user submits a bound change (clamp_ref = master-computed
/// Type-resolved view).
///
/// On return the touched entry is internally consistent — if `min` or `max`
/// shifted on a `ContinuousFloat`, `round_multiplier` is recomputed so callers
/// don't need a separate finalize step.
///
/// Warnings are emitted whenever something happens that an upstream validator
/// should have prevented (clamp fired, unknown param, type mismatch,
/// unsupported variant/aspect combination). UI and save-time validation are
/// expected to catch these before the value reaches here.
///
/// Returns `true` if any field on the touched entry was modified.
/// `resolved` and `clamp_ref` must have the same length.
pub fn apply_override(
    resolved:  &mut [ParamInfo],
    clamp_ref: &[ParamInfo],
    target:    &MetaTarget,
    value:     &ParamValue,
) -> bool {
    if resolved.len() != clamp_ref.len() {
        warn!("apply_override: resolved/clamp_ref length mismatch ({} vs {})",
              resolved.len(), clamp_ref.len());
        return false;
    }
    let Some(idx) = resolved.iter().position(|i| {
        i.name == target.param && matches!(i.kind, ParamKind::ParamMeta { .. })
    }) else {
        warn!("override targets unknown param '{}'", target.param);
        return false;
    };

    // Visible lives at the top of `ParamInfo`, independent of `data_kind`.
    // No clamping (it's a bool flag); strict bool input — numbers don't
    // silently flip visibility on or off.
    if matches!(target.aspect, MetaAspect::Visible) {
        let Ok(v) = value.try_bool() else {
            warn!("override {}.visible: expected bool", target.param);
            return false;
        };
        let changed = resolved[idx].visible != v;
        resolved[idx].visible = v;
        return changed;
    }

    let mut bound_changed = false;
    let changed = match (&mut resolved[idx].data_kind, &clamp_ref[idx].data_kind) {
        (
            ParamType::ContinuousFloat { min, max, default, log, .. },
            ParamType::ContinuousFloat { min: cmin, max: cmax, .. },
        ) => {
            // Snapshot for invariant rollback. The post-state must satisfy
            // `!log || (min > 0 && max > 0)` — same rule the construction-time
            // `assert!` enforces on canonical declarations.
            let (prev_min, prev_max, prev_default, prev_log) = (*min, *max, *default, *log);
            let changed = match target.aspect {
                MetaAspect::Min | MetaAspect::Max | MetaAspect::Default => {
                    let Ok(v_in) = value.try_float() else {
                        warn!("override {}.{:?}: expected float", target.param, target.aspect);
                        return false;
                    };
                    let v = v_in.clamp(*cmin, *cmax);
                    if v != v_in {
                        warn!("override {}.{:?}: value {v_in} out of canonical range [{cmin}, {cmax}], clamped to {v}",
                              target.param, target.aspect);
                    }
                    match target.aspect {
                        MetaAspect::Min     => { *min = v;     bound_changed = true; },
                        MetaAspect::Max     => { *max = v;     bound_changed = true; },
                        MetaAspect::Default => { *default = v; },
                        _ => unreachable!(),
                    }
                    true
                },
                MetaAspect::Log => {
                    let Ok(v) = value.try_bool() else {
                        warn!("override {}.{:?}: expected bool", target.param, target.aspect);
                        return false;
                    };
                    *log = v;
                    true
                },
                MetaAspect::Step => {
                    warn!("override {}.{:?}: Step aspect not supported by ContinuousFloat",
                          target.param, target.aspect);
                    false
                },
                MetaAspect::Visible => unreachable!("Visible aspect handled at top level"),
            };
            // Invariant check on the resulting state — single rule that covers
            // every aspect that touches log/min/max.
            if *log && (*min <= 0.0 || *max <= 0.0) {
                warn!("override {}.{:?}: log scale requires min > 0 && max > 0; rolled back",
                      target.param, target.aspect);
                *min = prev_min; *max = prev_max; *default = prev_default; *log = prev_log;
                return false;
            }
            changed
        },
        (
            ParamType::ContinuousInt { min, max, default, .. },
            ParamType::ContinuousInt { min: cmin, max: cmax, .. },
        ) => {
            let Ok(v_in) = value.try_int() else {
                warn!("override {}.{:?}: expected int", target.param, target.aspect);
                return false;
            };
            let v = v_in.clamp(*cmin, *cmax);
            if v != v_in {
                warn!("override {}.{:?}: value {v_in} out of canonical range [{cmin}, {cmax}], clamped to {v}",
                      target.param, target.aspect);
            }
            match target.aspect {
                MetaAspect::Min     => { *min = v;     true },
                MetaAspect::Max     => { *max = v;     true },
                MetaAspect::Default => { *default = v; true },
                _ => {
                    warn!("override {}.{:?}: aspect not applicable to ContinuousInt",
                          target.param, target.aspect);
                    false
                },
            }
        },
        (ParamType::DiscreteBool { default, .. }, _) => {
            if !matches!(target.aspect, MetaAspect::Default) {
                warn!("override {}.{:?}: only Default aspect supported for DiscreteBool",
                      target.param, target.aspect);
                return false;
            }
            let Ok(v) = value.try_bool() else {
                warn!("override {}.{:?}: expected bool", target.param, target.aspect);
                return false;
            };
            *default = v;
            true
        },
        _ => {
            warn!("override {}.{:?}: unsupported variant/aspect combination",
                  target.param, target.aspect);
            false
        },
    };

    // Recompute round_multiplier if min/max shifted on a ContinuousFloat,
    // so the touched entry stays internally consistent for callers that
    // don't run a separate finalize pass (i.e. runtime instance-edits).
    if bound_changed {
        if let ParamType::ContinuousFloat { min, max, round_multiplier, .. } = &mut resolved[idx].data_kind {
            *round_multiplier = auto_multiplier(*min, *max);
        }
    }

    changed
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
    /// Set a named parameter. Master validates and clamps before push, so
    /// audio implementations can store directly (use `try_float` / `try_bool`
    /// / `try_int` to extract the right variant).
    fn set_param(&mut self, param: &str, _value: ParamValue) -> Result<(), String> {
        Err(format!("unknown param '{param}'"))
    }
}

/// Allow `Box<dyn Device>` to be used where `Parameterized` is expected.
impl Parameterized for Box<dyn Device> {
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