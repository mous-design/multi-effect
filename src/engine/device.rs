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
// Bounds helper
// ---------------------------------------------------------------------------

/// Clamp `value` to `[min, max]`.
///
/// Returns `(clamped, Ok(()))` when in range, or `(clamped, Err(message))` when
/// the value was out of range.  Callers should always assign the returned value
/// so the effect continues to work even when the input was invalid.
pub fn check_bounds(prefix: &str, param: &str, value: f32, min: f32, max: f32) -> (f32, Result<(), String>) {
    let clamped = value.clamp(min, max);
    if clamped != value {
        (clamped, Err(format!("{prefix}: '{param}' value {value} out of range [{min}, {max}], clamped to {clamped}")))
    } else {
        (clamped, Ok(()))
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
    /// Set a named parameter.
    ///
    /// Parameters that accept stereo values (e.g. `wet`) take either a
    /// `Float` (applied to both channels) or a `Stereo` pair.
    /// Scalar-only parameters return an error when given a `Stereo` value.
    fn set_param(&mut self, param: &str, _value: ParamValue) -> Result<(), String> {
        Err(format!("unknown param '{param}'"))
    }

    /// Read a named parameter (returns the left-channel value for stereo params).
    #[allow(dead_code)]
    fn get_param(&self, param: &str) -> Option<f32> {
        let _ = param;
        None
    }

    /// Dispatch a named action string (e.g. "rec", "play", "rec-play-stop-rec").
    /// Only meaningful for nodes that have an action-based interface (e.g. Looper).
    fn set_action(&mut self, param: &str, action: &str) -> Result<(), String> {
        let _ = (param, action);
        Err(format!("unknown action '{param}'"))
    }
}

/// Allow `Box<dyn Device>` to be used where `Parameterized` is expected.
impl Parameterized for Box<dyn Device> {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        (**self).set_param(param, value)
    }
    fn get_param(&self, param: &str) -> Option<f32> {
        (**self).get_param(param)
    }
    fn set_action(&mut self, param: &str, action: &str) -> Result<(), String> {
        (**self).set_action(param, action)
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
    #[allow(dead_code)]
    fn type_name(&self) -> &str;

    /// Current parameter values as a JSON map (for state serialisation).
    #[allow(dead_code)]
    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    /// Whether this node is active. Inactive nodes are skipped entirely in
    /// `Chain::process` — no CPU cost beyond the branch check.
    fn is_active(&self) -> bool { true }

    /// MIDI Control Change
    fn on_cc(&mut self, controller: u8, value: u8) {
        let _ = (controller, value);
    }

    /// MIDI Program Change → switch preset
    fn on_program_change(&mut self, program: u8) {
        let _ = program;
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
}
