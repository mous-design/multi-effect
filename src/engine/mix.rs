use crate::engine::device::{find_param_info,
    ParamInfo, Device, Frame, Parameterized, ParamValue};

pub const NAME: &str = "mix";

/// Final output stage: scales the accumulated effect signal and compensates
/// for dry bleed in analogue-bypass setups.
///
/// Formula per channel (`d` is the dry input signal, `e` is the accumulated
/// effect signal arriving from the prior node):
///
/// ```text
/// out[ch] = ((e[ch] - d[ch]) * wet + d[ch] * dry) * gain * pan_factor[ch]
/// ```
///
/// - `wet`  (0–1):    output level of the pure effect signal (eff minus dry). Default: `1.0`.
/// - `dry`  (0–1):    output level of the original dry signal. `1.0` = full dry (digital mode),
///   `0.0` = no dry output (analogue-bypass mode, hardware adds dry). Default: `1.0`.
/// - `gain` (0–1):    overall output level (post-pan). Default: `1.0`.
/// - `pan`  (-1..+1): -1.0 = full left, 0.0 = centre, +1.0 = full right. Default: `0.0`.
pub struct Mix {
    key: String,
    /// Gain applied to the dry signal (both channels equally).
    dry: f32,
    /// Gain applied to the accumulated effect signal (both channels equally).
    wet: f32,
    /// Overall output level (0.0 = silence, 1.0 = unity). Default: 1.0.
    gain: f32,
    /// Pan: -1.0 = full left, 0.0 = centre, +1.0 = full right. Default: 0.0.
    pan: f32,
    active: bool,
}
 
// Hardcoded ranges, these are structural. `dry`/`wet` hidden by default —
// they only matter when reconfiguring an effect's analogue-bypass behaviour.
pub static CANONICAL: [ParamInfo; 5] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("dry",   0.0, 1.0, 1.0, false, None),
    ParamInfo::new_continuous_float("wet",   0.0, 1.0, 1.0, false, None),
    ParamInfo::new_continuous_float("gain",  0.0, 1.0, 1.0, false, None).with_hidden(),
    ParamInfo::new_continuous_float("pan", -1.0, 1.0, 0.0, false, None).with_hidden(),
];

impl Mix {
    pub fn new(key: impl Into<String>, params_info: &[ParamInfo]) -> Self {
        let active = find_param_info(params_info, "active").bool_default();
        let dry    = find_param_info(params_info, "dry"   ).continuous_float_default();
        let wet    = find_param_info(params_info, "wet"   ).continuous_float_default();
        let gain   = find_param_info(params_info, "gain"  ).continuous_float_default();
        let pan    = find_param_info(params_info, "pan"   ).continuous_float_default();
        Self {
            key: key.into(),
            active, dry, wet, gain, pan,
        }
    }
}

impl Parameterized for Mix {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        // Master clamps to declared bounds and normalises variant before push;
        // audio just stores. See `ConfigMaster::clamp_to_bounds`.
        match param {
            "active" => { self.active = value.try_bool()?;  Ok(()) },
            "dry"    => { self.dry    = value.try_float()?; Ok(()) },
            "wet"    => { self.wet    = value.try_float()?; Ok(()) },
            "gain"   => { self.gain   = value.try_float()?; Ok(()) },
            "pan"    => { self.pan    = value.try_float()?; Ok(()) },
            _ => Err(format!("{}: unknown param '{param}'", NAME)),
        }
    }
}

impl Device for Mix {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { NAME }

    fn is_active(&self) -> bool { self.active }

    fn process(&mut self, dry: &[Frame], eff: &mut [Frame]) {
        // Pan: the louder side stays at 1.0, the quieter side fades to 0.
        let pan_l = (1.0 - self.pan).min(1.0);
        let pan_r = (1.0 + self.pan).min(1.0);
        for (e, &d) in eff.iter_mut().zip(dry.iter()) {
            e[0] = ((e[0] - d[0]) * self.wet + d[0] * self.dry) * self.gain * pan_l;
            e[1] = ((e[1] - d[1]) * self.wet + d[1] * self.dry) * self.gain * pan_r;
        }
    }
    fn reset(&mut self) {}
}
