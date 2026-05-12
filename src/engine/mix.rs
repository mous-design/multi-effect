use crate::engine::device::{find_param_info, check_bounds, into_param_array,
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
    params_info: [ParamInfo; 5],
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
 
// Hardcoded ranges, these are structural.
pub static CANONICAL: [ParamInfo; 5] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("dry",   0.0, 1.0, 1.0, false, None),
    ParamInfo::new_continuous_float("wet",   0.0, 1.0, 1.0, false, None),
    ParamInfo::new_continuous_float("gain",  0.0, 1.0, 1.0, false, None),
    ParamInfo::new_continuous_float("pan", -1.0, 1.0, 0.0, false, None),
];

impl Mix {
    pub fn new(key: impl Into<String>, params_info: &[ParamInfo]) -> Self {
        let params_info = into_param_array(params_info, CANONICAL, NAME);
        let active = find_param_info(&params_info,"active").bool_default();
        let dry = find_param_info(&params_info,"dry").continuous_float_default();
        let wet = find_param_info(&params_info,"wet").continuous_float_default();
        let gain = find_param_info(&params_info,"gain").continuous_float_default();
        let pan = find_param_info(&params_info,"pan").continuous_float_default();
        Self {
            params_info, key: key.into(),
            active, dry, wet, gain, pan,
        }
    }
}

impl Parameterized for Mix {
    fn get_params_info(&self) -> &[ParamInfo] {
        &self.params_info
    }
    fn get_params_info_mut(&mut self) -> &mut [ParamInfo] {
        &mut self.params_info
    }

    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active" => {
                self.active = value.try_bool()?;
                Ok(())
            },
            "dry"  => {
                let info = find_param_info(self.get_params_info(), "dry");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.dry = v;
                r
            },
            "wet"  => {
                let info = find_param_info(self.get_params_info(), "wet");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.wet = v;
                r
            },
            "gain" => {
                let info = find_param_info(self.get_params_info(), "gain");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.gain = v;
                r
            },
            "pan"  => {
                let info = find_param_info(self.get_params_info(), "pan");
                let (v, r) = check_bounds(info,  value.try_float()?, NAME);
                self.pan  = v;
                r
            },
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
