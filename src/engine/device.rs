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
    #[allow(dead_code)]
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

    /// No value shape. Used by entries whose role is non-value (currently
    /// `ParamKind::Event` button-clusters). The kind discriminator carries
    /// the meaningful payload; data_kind exists purely because the wire-flat
    /// shape requires *some* `type` tag.
    None,
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
///
/// Combined verbs (e.g. `RecPlayStopRec`) encode a state-machine choice —
/// the effect decides which primitive to fire based on its current state.
/// UI sends the combined verb; effect resolves. No state-machine
/// duplication on the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventAction {
    // ── Primitives ────────────────────────────────────────────────────
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
    /// Generic reset (effect-defined semantics).
    Reset,

    // ── Combined transport (looper) ───────────────────────────────────
    /// 2-letter cycle: Rec ↔ Play. From Playing → Rec (starts overdub at
    /// current pos); from Overdub → Play. After loop established, alternates
    /// Play / Overdub on each press.
    RecPlay,
    /// 3-letter cycle, Stop wraps back to Rec: idle→Rec, rec→Play, play→Stop,
    /// stop→Rec.
    RecPlayStop,
    /// 4-letter, distinct from `RecPlayStop`: stop→Play (resume) instead of
    /// wrapping back to Rec.
    RecPlayStopPlay,
    /// Playing → Stop (true stop — resets playhead to 0); Stop → Play.
    PlayStop,
    /// Playing → Pause (keeps playhead); Stop → Play.
    PlayPause,
    /// idle→Rec, then Recording/Overdub→Pause, Playing/Stop→Rec. Press
    /// oscillates Rec/Pause when active (was historically called `RecStop`
    /// but the active-state action is Pause).
    RecPause,
    /// any-active→pause, stop→reset.
    StopReset,
    /// any-active→pause, stop→stop (goto 0).
    PauseStop,
    /// State-dependent: pause if running, stop if stopped-at-pos, else reset.
    PauseStopReset,
}

impl EventAction {
    /// Kebab-case name — matches the wire form and is used as the
    /// `ParamInfo.name` of single-action Event entries.
    pub const fn name(self) -> &'static str {
        match self {
            EventAction::Rec               => "rec",
            EventAction::Play              => "play",
            EventAction::Stop              => "stop",
            EventAction::Pause             => "pause",
            EventAction::Undo              => "undo",
            EventAction::Reset             => "reset",
            EventAction::RecPlay           => "rec-play",
            EventAction::RecPlayStop       => "rec-play-stop",
            EventAction::RecPlayStopPlay   => "rec-play-stop-play",
            EventAction::PlayStop          => "play-stop",
            EventAction::PlayPause         => "play-pause",
            EventAction::RecPause          => "rec-pause",
            EventAction::StopReset         => "stop-reset",
            EventAction::PauseStop         => "pause-stop",
            EventAction::PauseStopReset    => "pause-stop-reset",
        }
    }
}

/// Which aspect of a targeted live `ParamMeta` a meta-entry edits.
///
/// `Min`/`Max`/`Default`/`Step`/`Log` describe numeric bounds; `Visible`
/// is the presentation flag (knob shown / hidden on the tile). Putting them
/// in one enum keeps the override pipeline uniform — every per-param attribute
/// flows through `apply_override` and the meta-form `SET` regardless of whether
/// it's a bound or a presentation hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaAspect { Min, Max, Default, Step, Log, Visible, Active }

/// Role of a `ParamInfo` entry within a canonical list.
///
/// `ParamMeta` entries are the live params themselves (knob / toggle on the
/// tile). `BoundMeta` entries declare the editable envelope for an aspect
/// (Min / Max) of a targeted `ParamMeta` — i.e. the *bounds of the bounds*.
/// Their `name` matches the targeted live param's name; `aspect` picks which
/// bound. Override values clamp to the `BoundMeta`'s range; when no
/// `BoundMeta` is declared, the ParamMeta's own range is the cap.
///
/// Only `Min` and `Max` aspects need `BoundMeta` entries:
/// - `default` is invariant-bound to the resolved `[min, max]`.
/// - `log`, `visible` are bool — no range to declare.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "tag")]
pub enum ParamKind {
    /// Live param. Rendered on the tile; the contract depends on `read_only`:
    /// - `read_only: false` — settable via SET; rendered as knob/toggle.
    /// - `read_only: true`  — effect-driven (current loop position, current
    ///   buffer count, …); `set_param` rejects writes. Effect publishes value
    ///   updates via `ControlMessage::LiveParam`. UI renders an effect-specific
    ///   display widget (timer, counter, …) bound to the same value.
    ///
    /// `max_growable_at_runtime: false` locks the max at construction (delay
    /// buffer, chorus depth) — widening `<param>.max` past construction is
    /// rejected by the resolver and surfaces a reload-required event.
    ParamMeta { max_growable_at_runtime: bool, read_only: bool },
    /// Editable envelope for one aspect of a targeted `ParamMeta`. The
    /// entry's own `min`/`max` describe the range an override of that aspect
    /// can take. Lets canonical declare a wider override envelope than the
    /// param's own range (e.g. `delay.time.max` widenable up to 60 s).
    BoundMeta { aspect: MetaAspect },
    /// Single action trigger. One ParamInfo entry per button — the entry's
    /// `name` is the action verb (`"rec"`, `"rec-play-stop-rec"`, …), the
    /// `action` field is the typed verb. UI renders one button per active
    /// Event entry. Per-button `active` / `visible` work via the standard
    /// override pipeline. Wire: `ACT <key>.<verb> <verb>`. The effect's
    /// state machine resolves combined verbs against current state.
    Event { action: EventAction },
    /// UI grouping anchor. Carries no value of its own — exists purely as a
    /// handle for `visible` / `active` overrides on a related cluster of
    /// entries (e.g. the looper's transport buttons). The tile renders the
    /// group iff `info.visible` (and exists at all iff `info.active`).
    ///
    /// Today, which entries belong to which group is hard-coded in the UI
    /// per effect. A future field on this variant may declare members
    /// explicitly when the generic tile-ordering work lands — see
    /// `todo.md → ActionsGroup: membership + custom tile ordering`. The
    /// marker stays compatible either way.
    ActionsGroup,
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
    /// `with_kind_bound_meta(aspect)`.
    pub kind: ParamKind,
    /// Whether the UI should render this param's knob on the tile.
    /// Defaults to `true` at construction; canonical entries can flip it
    /// via `with_hidden()`, and per-instance overrides toggle it at runtime
    /// via `SET <key>.<param>.visible <bool>` (meta form).
    pub visible: bool,
    /// Whether this entry is declared-active. `false` means the canonical
    /// declares it but it's hidden from all UI surfaces until an override
    /// flips it back on. Effects can declare a wide menu of optional
    /// controls (many transport variants, advanced toggles) marked inactive
    /// by default; per-instance overrides switch on the ones the user wants.
    /// Wire path: `SET <key>.<param>.active <bool>`.
    pub active: bool,
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
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true, read_only: false },
            visible: true,
            active: true,
        }
    }
    pub const fn new_continuous_int(name: &'static str, min: i32, max: i32, default: i32,
        unit: Option<&'static str>) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::ContinuousInt { min, max, default, unit },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true, read_only: false },
            visible: true,
            active: true,
        }
    }
    pub const fn new_discrete_bool(name: &'static str, default: bool,
        labels: Option<(&'static str, &'static str)>,) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::DiscreteBool { default, labels },
            kind: ParamKind::ParamMeta { max_growable_at_runtime: true, read_only: false },
            visible: true,
            active: true,
        }
    }
    /// Declare a single-action Event entry. The entry's `name` is the
    /// action verb (kebab-case, derived from `EventAction::name()`); the UI
    /// renders one button per active Event entry. Effect's `set_action`
    /// dispatches the verb; combined verbs (e.g. `RecPlayStopRec`) resolve
    /// against current state in the effect.
    pub const fn new_event(action: EventAction) -> Self {
        ParamInfo {
            name: action.name(),
            data_kind: ParamType::None,
            kind: ParamKind::Event { action },
            visible: true,
            active: true,
        }
    }

    /// Declare a UI grouping anchor — no value, no aspect, just a handle
    /// for `visible` / `active` overrides on a related cluster of entries.
    /// The looper's `"transport"` is the only example today.
    pub const fn new_actions_group(name: &'static str) -> Self {
        ParamInfo {
            name,
            data_kind: ParamType::None,
            kind: ParamKind::ActionsGroup,
            visible: true,
            active: true,
        }
    }

    // ----- Builders for orthogonal aspects (chained on top of constructors) -----

    /// Lock the max at construction time (sizes a buffer, etc.). Override
    /// attempts to grow past the construction-time max are rejected by the
    /// resolver — master surfaces a reload-required event. Only valid on
    /// `ParamMeta`; panics on the other kinds (no value-shape, or no buffer
    /// to protect).
    pub const fn with_non_growable(self) -> Self {
        match self.kind {
            ParamKind::ParamMeta { read_only, .. } => Self {
                kind: ParamKind::ParamMeta { max_growable_at_runtime: false, read_only },
                ..self
            },
            ParamKind::BoundMeta { .. } =>
                panic!("with_non_growable: not valid for BoundMeta entries"),
            ParamKind::Event { .. } =>
                panic!("with_non_growable: not valid for Event entries"),
            ParamKind::ActionsGroup =>
                panic!("with_non_growable: not valid for ActionsGroup entries"),
        }
    }

    /// Mark this ParamMeta as effect-driven (read-only from the user's POV).
    /// `set_param` should reject writes; the effect emits `LiveParam` updates
    /// for the value. UI renders a display widget instead of a knob.
    pub const fn with_read_only(self) -> Self {
        match self.kind {
            ParamKind::ParamMeta { max_growable_at_runtime, .. } => Self {
                kind: ParamKind::ParamMeta { max_growable_at_runtime, read_only: true },
                ..self
            },
            _ => panic!("with_read_only: only valid for ParamMeta entries"),
        }
    }

    /// Mark this param hidden by default in the UI. Per-preset overrides
    /// can still flip it back on via `SET <key>.<param>.visible true` (meta form).
    pub const fn with_hidden(self) -> Self {
        Self { visible: false, ..self }
    }

    /// Declare this entry as inactive by default. UI hides it everywhere
    /// (tile, override popup) until an override flips `active` back on.
    /// Used to ship a wide menu of optional controls (e.g. all transport
    /// variants for the looper) with sensible defaults visible.
    pub const fn with_inactive(self) -> Self {
        Self { active: false, ..self }
    }

    /// Tag this entry as the editable envelope for an aspect of the
    /// targeted live param (matched by `name`). `aspect` ∈ `Min` / `Max` —
    /// the entry's `min`/`max` describe the range an override of that aspect
    /// can take. Lets canonical declare a wider override envelope than the
    /// param's default knob range (e.g. delay time widenable to 60 s) or a
    /// tighter one (e.g. feedback never above 0.99 even by override).
    pub const fn with_kind_bound_meta(self, aspect: MetaAspect) -> Self {
        Self {
            kind: ParamKind::BoundMeta { aspect },
            ..self
        }
    }

    pub fn continuous_float_default(&self) -> f32 {
        match &self.data_kind {
            ParamType::ContinuousFloat { default, .. } => *default,
            _ => panic!("{}: expected ContinuousFloat", self.name),
        }
    }
    #[allow(dead_code)]
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
            ParamType::None => panic!("{}: Event entry has no value", self.name),
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
            MetaAspect::Active  => "active",
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
            "active"  => MetaAspect::Active,
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
            "active"  => MetaAspect::Active,
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

    // ActionsGroup is a UI-only marker — no value, no aspect. Only the
    // top-level `visible` / `active` flags apply; anything else is nonsense.
    // Handle here so the ParamMeta lookup below doesn't fall through with
    // an "unknown param" warning.
    if let Some(idx) = resolved.iter().position(|i|
        i.name == target.param && matches!(i.kind, ParamKind::ActionsGroup))
    {
        let Ok(v) = value.try_bool() else {
            warn!("override {}.{:?}: expected bool", target.param, target.aspect);
            return false;
        };
        match target.aspect {
            MetaAspect::Visible => {
                let changed = resolved[idx].visible != v;
                resolved[idx].visible = v;
                return changed;
            },
            MetaAspect::Active => {
                let changed = resolved[idx].active != v;
                resolved[idx].active = v;
                return changed;
            },
            _ => {
                warn!("override {}.{:?}: aspect not valid on ActionsGroup",
                      target.param, target.aspect);
                return false;
            },
        }
    }

    let Some(idx) = resolved.iter().position(|i| {
        i.name == target.param && matches!(i.kind, ParamKind::ParamMeta { .. })
    }) else {
        warn!("override targets unknown param '{}'", target.param);
        return false;
    };

    // Visible / Active live at the top of `ParamInfo`, independent of
    // `data_kind`. No clamping (bool flags); strict bool input.
    if matches!(target.aspect, MetaAspect::Visible) {
        let Ok(v) = value.try_bool() else {
            warn!("override {}.visible: expected bool", target.param);
            return false;
        };
        let changed = resolved[idx].visible != v;
        resolved[idx].visible = v;
        return changed;
    }
    if matches!(target.aspect, MetaAspect::Active) {
        let Ok(v) = value.try_bool() else {
            warn!("override {}.active: expected bool", target.param);
            return false;
        };
        let changed = resolved[idx].active != v;
        resolved[idx].active = v;
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
                    // For Min / Max: consult BoundMeta envelope if declared,
                    // else fall back to the targeted ParamMeta's range. For
                    // Default: clamp to the ParamMeta range (kernel invariant
                    // re-asserts default ∈ [min, max] below).
                    let (lo, hi) = match target.aspect {
                        MetaAspect::Min | MetaAspect::Max =>
                            bound_meta_float(clamp_ref, &target.param, target.aspect)
                                .unwrap_or((*cmin, *cmax)),
                        _ => (*cmin, *cmax),
                    };
                    let v = v_in.clamp(lo, hi);
                    if v != v_in {
                        warn!("override {}.{:?}: value {v_in} out of [{lo}, {hi}], clamped to {v}",
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
                MetaAspect::Visible | MetaAspect::Active =>
                    unreachable!("Visible/Active handled at top level"),
            };
            // Invariant: log scale requires strictly-positive bounds.
            if *log && (*min <= 0.0 || *max <= 0.0) {
                warn!("override {}.{:?}: log scale requires min > 0 && max > 0; rolled back",
                      target.param, target.aspect);
                *min = prev_min; *max = prev_max; *default = prev_default; *log = prev_log;
                return false;
            }
            // Invariant: min <= max.
            if *min > *max {
                warn!("override {}.{:?}: min ({}) > max ({}); rolled back",
                      target.param, target.aspect, *min, *max);
                *min = prev_min; *max = prev_max; *default = prev_default; *log = prev_log;
                return false;
            }
            // Invariant: default ∈ [min, max]. Direct edits must satisfy;
            // cascaded clamps (after a min/max change) auto-fix silently.
            match target.aspect {
                MetaAspect::Default => {
                    if *default < *min || *default > *max {
                        warn!("override {}.default: {} outside [{}, {}]; rolled back",
                              target.param, *default, *min, *max);
                        *min = prev_min; *max = prev_max; *default = prev_default; *log = prev_log;
                        return false;
                    }
                },
                MetaAspect::Min | MetaAspect::Max => {
                    let clamped = default.clamp(*min, *max);
                    if clamped != *default {
                        tracing::info!("override {}.{:?}: default {} out of new range [{}, {}], auto-clamped to {}",
                              target.param, target.aspect, *default, *min, *max, clamped);
                        *default = clamped;
                    }
                },
                _ => {},
            }
            changed
        },
        (
            ParamType::ContinuousInt { min, max, default, .. },
            ParamType::ContinuousInt { min: cmin, max: cmax, .. },
        ) => {
            let (prev_min, prev_max, prev_default) = (*min, *max, *default);
            let Ok(v_in) = value.try_int() else {
                warn!("override {}.{:?}: expected int", target.param, target.aspect);
                return false;
            };
            // BoundMeta envelope for Min / Max, else ParamMeta range.
            let (lo, hi) = match target.aspect {
                MetaAspect::Min | MetaAspect::Max =>
                    bound_meta_int(clamp_ref, &target.param, target.aspect)
                        .unwrap_or((*cmin, *cmax)),
                _ => (*cmin, *cmax),
            };
            let v = v_in.clamp(lo, hi);
            if v != v_in {
                warn!("override {}.{:?}: value {v_in} out of [{lo}, {hi}], clamped to {v}",
                      target.param, target.aspect);
            }
            let changed = match target.aspect {
                MetaAspect::Min     => { *min = v;     true },
                MetaAspect::Max     => { *max = v;     true },
                MetaAspect::Default => { *default = v; true },
                _ => {
                    warn!("override {}.{:?}: aspect not applicable to ContinuousInt",
                          target.param, target.aspect);
                    false
                },
            };
            // Invariant: min <= max.
            if *min > *max {
                warn!("override {}.{:?}: min ({}) > max ({}); rolled back",
                      target.param, target.aspect, *min, *max);
                *min = prev_min; *max = prev_max; *default = prev_default;
                return false;
            }
            // Invariant: default ∈ [min, max]. Direct → hard reject; cascaded → auto-clamp.
            match target.aspect {
                MetaAspect::Default => {
                    if *default < *min || *default > *max {
                        warn!("override {}.default: {} outside [{}, {}]; rolled back",
                              target.param, *default, *min, *max);
                        *min = prev_min; *max = prev_max; *default = prev_default;
                        return false;
                    }
                },
                MetaAspect::Min | MetaAspect::Max => {
                    let clamped = (*default).clamp(*min, *max);
                    if clamped != *default {
                        tracing::info!("override {}.{:?}: default {} out of new range [{}, {}], auto-clamped to {}",
                              target.param, target.aspect, *default, *min, *max, clamped);
                        *default = clamped;
                    }
                },
                _ => {},
            }
            changed
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
/// Implemented by effects, loopers, and chains. Separating this from
/// [`Device`] allows non-audio nodes to participate in the same parameter
/// system without implementing the full audio processing interface.
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

    /// Re-fire all current live state on the bus (LiveParam values + state
    /// tag). Triggered when a new client connects so it can catch up without
    /// waiting for the next state transition. Default no-op for effects with
    /// no published live state.
    fn republish_state(&self) {}

    /// Dispatch a typed action verb. Effects with an action-based interface
    /// (looper transport, delay tap, reverb freeze) match on the `EventAction`
    /// variant exhaustively — typos can't reach here (wire layer parses the
    /// string into the enum). `param` selects which Event entry on the effect
    /// the action targets (most effects have just one, named "action").
    fn set_action(&mut self, param: &str, action: EventAction) -> Result<(), String> {
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

/// Find a `ParamMeta` entry by name. Disambiguates when the canonical has
/// multiple entries sharing the same `name` (e.g. a read-only ParamMeta
/// alongside a `BoundMeta` declaring its override envelope).
pub fn find_param_meta<'a>(params_info: &'a [ParamInfo], name: &str) -> &'a ParamInfo {
    params_info.iter()
        .find(|i| i.name == name && matches!(i.kind, ParamKind::ParamMeta { .. }))
        .unwrap_or_else(|| panic!("ParamMeta entry '{name}' not found"))
}

/// `BoundMeta`-declared envelope for `(param, aspect)` as `(min, max)`.
/// `None` when no `BoundMeta` entry exists — caller falls back to the
/// targeted `ParamMeta`'s own range. Float-typed lookup; the canonical
/// validator ensures `BoundMeta` matches its target's `ParamType`.
pub fn bound_meta_float(slice: &[ParamInfo], param: &str, aspect: MetaAspect) -> Option<(f32, f32)> {
    slice.iter().find_map(|i| {
        if i.name != param { return None; }
        let ParamKind::BoundMeta { aspect: a } = i.kind else { return None; };
        if a != aspect { return None; }
        match i.data_kind {
            ParamType::ContinuousFloat { min, max, .. } => Some((min, max)),
            _ => None,
        }
    })
}

/// Same as [`bound_meta_float`] for integer params.
pub fn bound_meta_int(slice: &[ParamInfo], param: &str, aspect: MetaAspect) -> Option<(i32, i32)> {
    slice.iter().find_map(|i| {
        if i.name != param { return None; }
        let ParamKind::BoundMeta { aspect: a } = i.kind else { return None; };
        if a != aspect { return None; }
        match i.data_kind {
            ParamType::ContinuousInt { min, max, .. } => Some((min, max)),
            _ => None,
        }
    })
}

/// Current value of one aspect on a `ParamInfo`, as a `ParamValue`. Symmetric
/// to `apply_override`'s write side — reads back the same fields the kernel
/// writes. Returns `None` when the aspect doesn't apply to this entry's
/// `data_kind` (e.g. `Log` on `DiscreteBool`, `Step` on any variant — not
/// stored anywhere today).
///
/// Used by the Type-overrides sanitiser to re-extract clamped values out of
/// `resolved` after `apply_override` has run.
pub fn aspect_value(info: &ParamInfo, aspect: MetaAspect) -> Option<ParamValue> {
    // ParamMeta / BoundMeta: aspect maps directly to the corresponding field.
    if matches!(aspect, MetaAspect::Visible) {
        return Some(ParamValue::Bool(info.visible));
    }
    if matches!(aspect, MetaAspect::Active) {
        return Some(ParamValue::Bool(info.active));
    }
    match (&info.data_kind, aspect) {
        (ParamType::ContinuousFloat { min,     .. }, MetaAspect::Min)     => Some((*min)    .into()),
        (ParamType::ContinuousFloat { max,     .. }, MetaAspect::Max)     => Some((*max)    .into()),
        (ParamType::ContinuousFloat { default, .. }, MetaAspect::Default) => Some((*default).into()),
        (ParamType::ContinuousFloat { log,     .. }, MetaAspect::Log)     => Some((*log)    .into()),
        (ParamType::ContinuousInt   { min,     .. }, MetaAspect::Min)     => Some((*min)    .into()),
        (ParamType::ContinuousInt   { max,     .. }, MetaAspect::Max)     => Some((*max)    .into()),
        (ParamType::ContinuousInt   { default, .. }, MetaAspect::Default) => Some((*default).into()),
        (ParamType::DiscreteBool    { default, .. }, MetaAspect::Default) => Some((*default).into()),
        _ => None,
    }
}

/// Compile-time validator for a canonical `[ParamInfo]` array.
///
/// Run via `const _: () = validate_canonical(&CANONICAL);` after each effect's
/// canonical static — any malformed `BoundMeta` declaration becomes a
/// const-eval panic at build time:
/// - bad aspect (not Min or Max)
/// - no matching `ParamMeta` target by name
/// - type mismatch (Float BoundMeta on Int target, etc.)
///
/// Widening past canonical on a non-growable `ParamMeta` is allowed — the
/// reload-required UX gate (Type-overrides popup → confirm reload → process
/// restart) rebuilds the effect with the widened max, so audio buffers get
/// sized correctly. The "non-growable" flag is purely about *runtime* growth
/// (no reload), which the resolver still rejects.
pub const fn validate_canonical(arr: &[ParamInfo]) {
    let mut i = 0;
    while i < arr.len() {
        match arr[i].kind {
            ParamKind::BoundMeta { aspect } => {
                // Aspect must be Min or Max.
                match aspect {
                    MetaAspect::Min | MetaAspect::Max => {},
                    _ => panic!("BoundMeta aspect must be Min or Max"),
                }
                // Find matching ParamMeta with same name and compatible ParamType.
                let mut j = 0;
                let mut ok = false;
                while j < arr.len() {
                    if let ParamKind::ParamMeta { .. } = arr[j].kind {
                        if const_str_eq(arr[j].name, arr[i].name) {
                            match (&arr[j].data_kind, &arr[i].data_kind) {
                                (ParamType::ContinuousFloat { .. },
                                 ParamType::ContinuousFloat { .. }) => { ok = true; },
                                (ParamType::ContinuousInt { .. },
                                 ParamType::ContinuousInt { .. }) => { ok = true; },
                                _ => panic!("BoundMeta type does not match targeted ParamMeta"),
                            }
                            break;
                        }
                    }
                    j += 1;
                }
                if !ok {
                    panic!("BoundMeta has no matching ParamMeta target by name");
                }
            },
            ParamKind::ParamMeta { .. } => {},
            ParamKind::Event { .. } => {
                // Event entries must have `data_kind = None` — they don't
                // carry a value. The canonical constructor `new_event`
                // enforces this; this check catches manual struct-literal
                // construction (which shouldn't happen but isn't impossible).
                match arr[i].data_kind {
                    ParamType::None => {},
                    _ => panic!("Event entries must have data_kind = None"),
                }
            },
            ParamKind::ActionsGroup => {
                // Same as Event: no value-shape, no aspect. The canonical
                // constructor `new_actions_group` enforces None data_kind;
                // this check catches manual struct-literal construction.
                match arr[i].data_kind {
                    ParamType::None => {},
                    _ => panic!("ActionsGroup entries must have data_kind = None"),
                }
            },
        }
        i += 1;
    }
}

/// `const fn` byte-by-byte string equality — `str::eq` isn't const yet.
const fn const_str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() { return false; }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] { return false; }
        i += 1;
    }
    true
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