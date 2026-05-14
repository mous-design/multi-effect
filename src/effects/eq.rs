use std::f32::consts::PI;

use crate::engine::device::{find_param_info,
    ParamInfo, Device, Frame, Parameterized, ParamValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EqType {
    /// Bell/peak filter: boost or cut around a centre frequency.
    Peak,
    /// Low-shelf: boost or cut below the shelf frequency.
    LowShelf,
    /// High-shelf: boost or cut above the shelf frequency.
    HighShelf,
}

pub const NAME_MID: &str = "eq_mid";
pub const NAME_LOW: &str = "eq_low";
pub const NAME_HIGH: &str = "eq_high";


/// Second-order parametric EQ section (Transposed Direct Form II).
///
/// Coefficients computed from the Robert Bristow-Johnson Audio EQ Cookbook.
/// Separate state per channel for correct stereo operation.
///
/// Transfer function (normalised by a0):
/// ```text
/// H(z) = (b0 + b1*z⁻¹ + b2*z⁻²) / (1 + a1*z⁻¹ + a2*z⁻²)
/// ```
///
/// Chain multiple `Eq` nodes for multi-band EQ:
/// ```text
/// eq_low → eq_mid → eq_mid → eq_high
/// ```
pub struct Eq {
    key: String,
    eq_type:  EqType,
    active: bool,
    freq_hz:  f32,
    q:        f32,
    gain_db:  f32,

    // Normalised biquad coefficients (divided by a0)
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,

    // Transposed DF-II state: w[channel][0..1]
    w: [[f32; 2]; 2],

    sample_rate: f32,
}

// One canonical per EQ type — only the per-band defaults differ.
// `freq`/`q`/`gain_db` ranges are shared (overridable globally via `eq.<param>.<aspect>`).
// Low/high shelves hide `freq` by default (shelf frequencies are usually set once);
// mid-band hides `q` (most uses are gentle wide-Q boosts/cuts).
pub static CANONICAL_LOW: [ParamInfo; 4] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("freq",       20.0, 20000.0,   100.0, true,  Some("Hz")).with_hidden(),
    ParamInfo::new_continuous_float("q",           0.1,    10.0,   0.707, true,  None).with_hidden(),
    ParamInfo::new_continuous_float("gain_db",   -15.0,    15.0,     0.0, false, Some("dB")),
];
pub static CANONICAL_MID: [ParamInfo; 4] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("freq",       20.0, 20000.0,  1000.0, true,  Some("Hz")),
    ParamInfo::new_continuous_float("q",           0.1,    10.0,     1.0, true,  None).with_hidden(),
    ParamInfo::new_continuous_float("gain_db",   -15.0,    15.0,     0.0, false, Some("dB")),
];
pub static CANONICAL_HIGH: [ParamInfo; 4] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("freq",       20.0, 20000.0, 10000.0, true,  Some("Hz")).with_hidden(),
    ParamInfo::new_continuous_float("q",           0.1,    10.0,   0.707, true,  None).with_hidden(),
    ParamInfo::new_continuous_float("gain_db",   -15.0,    15.0,     0.0, false, Some("dB")),
];

impl Eq {
    pub fn new(key: impl Into<String>, eq_type: EqType, sample_rate: f32, params_info: &[ParamInfo]) -> Self {
        let active  = find_param_info(params_info, "active" ).bool_default();
        let freq_hz = find_param_info(params_info, "freq"   ).continuous_float_default();
        let q       = find_param_info(params_info, "q"      ).continuous_float_default();
        let gain_db = find_param_info(params_info, "gain_db").continuous_float_default();
        let mut eq = Self {
            key: key.into(), eq_type,
            active, freq_hz, q, gain_db,
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            w: [[0.0; 2]; 2],
            sample_rate,
        };
        eq.update_coefficients();
        eq
    }

    fn update_coefficients(&mut self) {
        let w0    = 2.0 * PI * self.freq_hz / self.sample_rate;
        let cos_w = w0.cos();
        let sin_w = w0.sin();
        let alpha = sin_w / (2.0 * self.q);
        // A = 10^(dBgain/40) — linear amplitude factor used by shelf and peak
        let a = 10.0_f32.powf(self.gain_db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match self.eq_type {
            EqType::Peak => (
                1.0 + alpha * a,
               -2.0 * cos_w,
                1.0 - alpha * a,
                1.0 + alpha / a,
               -2.0 * cos_w,
                1.0 - alpha / a,
            ),
            EqType::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w),
                    a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha),
                    (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos_w),
                    (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha,
                )
            },
            EqType::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w),
                    a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha),
                    (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos_w),
                    (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha,
                )
            }
        };

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }
}

impl Parameterized for Eq {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        let name = self.type_name();
        // Master clamps to declared bounds and normalises variant before push;
        // audio just stores. See `ConfigMaster::clamp_to_bounds`.
        match param {
            "active"  => { self.active   = value.try_bool()?;  Ok(()) },
            "freq"    => { self.freq_hz  = value.try_float()?; self.update_coefficients(); Ok(()) },
            "q"       => { self.q        = value.try_float()?; self.update_coefficients(); Ok(()) },
            "gain_db" => { self.gain_db  = value.try_float()?; self.update_coefficients(); Ok(()) },
            _ => Err(format!("{name}: unknown param '{param}'")),
        }
    }
}

impl Device for Eq {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str {
        match self.eq_type {
            EqType::Peak      => NAME_MID,
            EqType::LowShelf  => NAME_LOW,
            EqType::HighShelf => NAME_HIGH,
        }
    }

    fn is_active(&self) -> bool { self.active }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        for e in eff.iter_mut() {
            for ch in 0..2 {
                let x = e[ch];
                let y = self.b0 * x + self.w[ch][0];
                self.w[ch][0] = self.b1 * x - self.a1 * y + self.w[ch][1];
                self.w[ch][1] = self.b2 * x - self.a2 * y;
                e[ch] = y;
            }
        }
    }

    fn reset(&mut self) {
        self.w = [[0.0; 2]; 2];
    }
}
