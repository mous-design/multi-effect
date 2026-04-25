use std::f32::consts::PI;

use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EqType {
    /// Bell/peak filter: boost or cut around a centre frequency.
    Peak,
    /// Low-shelf: boost or cut below the shelf frequency.
    LowShelf,
    /// High-shelf: boost or cut above the shelf frequency.
    HighShelf,
}

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
/// eq_low → eq_param → eq_param → eq_high
/// ```
pub struct Eq {
    pub key: String,
    pub eq_type:  EqType,
    pub freq_hz:  f32,
    pub q:        f32,
    pub gain_db:  f32,

    // Normalised biquad coefficients (divided by a0)
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,

    // Transposed DF-II state: w[channel][0..1]
    w: [[f32; 2]; 2],

    pub active: bool,
    sample_rate: f32,
}

impl Eq {
    pub fn new(key: impl Into<String>, eq_type: EqType, sample_rate: f32) -> Self {
        let (freq_hz, q) = match eq_type {
            EqType::Peak     => (1000.0, 1.0),
            EqType::LowShelf =>  (100.0, 0.707),
            EqType::HighShelf => (10000.0, 0.707),
        };
        let mut eq = Self {
            key: key.into(),
            eq_type,
            freq_hz,
            q,
            gain_db: 0.0,
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            w: [[0.0; 2]; 2],
            active: true,
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
        let name = match self.eq_type {
            EqType::Peak      => "EqParam",
            EqType::LowShelf  => "EqLow",
            EqType::HighShelf => "EqHigh",
        };
        match param {
            "active"  => { self.active = value.try_bool()?; Ok(()) }
            "freq"    => { let (v, r) = check_bounds(name, "freq",    value.try_float()?, 20.0, self.sample_rate * 0.499); self.freq_hz = v; self.update_coefficients(); r }
            "q"       => { let (v, r) = check_bounds(name, "q",       value.try_float()?, 0.1,  30.0); self.q       = v; self.update_coefficients(); r }
            "gain_db" => { let (v, r) = check_bounds(name, "gain_db", value.try_float()?, -24.0, 24.0); self.gain_db = v; self.update_coefficients(); r }
            _ => Err(format!("{name}: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active"  => Some(if self.active { 1.0 } else { 0.0 }),
            "freq"    => Some(self.freq_hz),
            "q"       => Some(self.q),
            "gain_db" => Some(self.gain_db),
            _ => None,
        }
    }
}

impl Device for Eq {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str {
        match self.eq_type {
            EqType::Peak      => "eq_param",
            EqType::LowShelf  => "eq_low",
            EqType::HighShelf => "eq_high",
        }
    }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),  self.active.into());
        m.insert("freq".into(),    self.freq_hz.into());
        if self.eq_type == EqType::Peak {
            m.insert("q".into(), self.q.into());
        }
        m.insert("gain_db".into(), self.gain_db.into());
        m
    }

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
