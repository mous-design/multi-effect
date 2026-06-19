//! Central effect registry — single enumeration site for every DSP node
//! shipped with the firmware.
//!
//! Each effect module declares a `pub const REGISTRATION: EffectRegistration`
//! (or several, for variant-bearing effects like the EQ). This file gathers
//! them into one `REGISTRY` slice that `master` and `patch` consult instead
//! of pattern-matching on the effect type string at each call site.
//!
//! Adding a new effect:
//!   1. Add `pub const REGISTRATION` (or `REG_*`) to the effect module.
//!   2. Append one line to `REGISTRY` below.
//!
//! That's it — `canonical_for`, `canonical_map`, and `build_node` all read
//! from `REGISTRY`, so nothing else needs updating.

use crate::engine::device::{Device, ParamInfo};

use crate::effects::{chorus, delay, eq, harmonizer, looper, reverb};

/// Per-effect registration entry. Co-locates name, canonical metadata, and
/// constructor so all three travel together. The factory takes the universal
/// triple `(key, sample_rate, params_info)`; effects that don't need
/// `sample_rate` (e.g. `Mix`) just ignore it in the closure.
pub struct EffectRegistration {
    pub name:      &'static str,
    pub canonical: &'static [ParamInfo],
    pub factory:   fn(key: &str, sample_rate: f32, params_info: &[ParamInfo]) -> Box<dyn Device>,
    /// Type-level: instances of this effect process dry (replace, add to, or
    /// subtract from it). Master combines with the per-instance `active` flag
    /// to compute each chain's `dry_effective` — when any active node in a
    /// chain has `needs_dry: true`, the chain's mix node force-overrides its
    /// user-set `dry` to on (and the future analog signal-relay closes).
    /// Time-shift effects (delay / chorus / reverb / looper / harmonizer)
    /// generate their own output and don't pull dry on — they leave it
    /// `false`. EQ / exciter / distortion / phaser / flanger consume dry and
    /// set `true`.
    pub needs_dry: bool,
}

/// All shipped effects. Order isn't load-bearing — `lookup` and consumers
/// iterate the slice; UI sorts alphabetically.
pub static REGISTRY: &[&EffectRegistration] = &[
    &looper::REGISTRATION,
    &delay::REGISTRATION,
    &reverb::REGISTRATION,
    &chorus::REGISTRATION,
    &harmonizer::REGISTRATION,
    &eq::REG_MID,
    &eq::REG_LOW,
    &eq::REG_HIGH,
];

/// Find a registration by effect-type name. Returns `None` for unknown
/// types — callers decide whether to error or skip.
pub fn lookup(name: &str) -> Option<&'static EffectRegistration> {
    REGISTRY.iter().copied().find(|r| r.name == name)
}
