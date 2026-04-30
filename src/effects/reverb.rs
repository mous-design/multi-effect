use std::collections::HashMap;
use crate::engine::device::{override_float, find_param_info, check_bounds,
    ParamInfo, OverrideValue, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

// ---------------------------------------------------------------------------
// Freeverb delay tunings (samples at 44100 Hz; scaled at runtime)
// L and R use slightly different lengths for natural stereo spread.
// ---------------------------------------------------------------------------
const COMB_TUNING_L: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const COMB_TUNING_R: [usize; 8] = [1139, 1211, 1300, 1379, 1445, 1514, 1580, 1640];
const ALLPASS_TUNING_L: [usize; 4] = [556, 441, 341, 225];
const ALLPASS_TUNING_R: [usize; 4] = [579, 464, 364, 248];

pub const NAME: &str = "reverb";

fn scale_size(size: usize, sample_rate: f32) -> usize {
    ((size as f32 * sample_rate / 44100.0) as usize).max(1)
}

// ---------------------------------------------------------------------------
// Comb filter with one-pole LP damping in the feedback loop (Freeverb style)
// ---------------------------------------------------------------------------
struct CombFilter {
    buf: RingBuffer,
    feedback: f32,
    damp: f32,
    filterstore: f32,
}

impl CombFilter {
    fn new(size: usize) -> Self {
        Self { buf: RingBuffer::new(size), feedback: 0.5, damp: 0.5, filterstore: 0.0 }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let output = self.buf.read_at(self.buf.capacity());
        self.filterstore = output * (1.0 - self.damp) + self.filterstore * self.damp;
        self.buf.write(input + self.filterstore * self.feedback);
        output
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.filterstore = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Schroeder allpass filter
// ---------------------------------------------------------------------------
struct AllpassFilter {
    buf: RingBuffer,
}

impl AllpassFilter {
    fn new(size: usize) -> Self {
        Self { buf: RingBuffer::new(size) }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let buf_out = self.buf.read_at(self.buf.capacity());
        self.buf.write(input + buf_out * 0.5);
        buf_out - input
    }

    fn clear(&mut self) {
        self.buf.clear();
    }
}

// ---------------------------------------------------------------------------
// Reverb
// ---------------------------------------------------------------------------

/// Freeverb-style stereo reverb.
///
/// Architecture per channel: 8 parallel comb filters followed by 4 series
/// allpass filters.  L and R use slightly different delay lengths for natural
/// stereo spread.  A mono mix of the input feeds both channels so the reverb
/// tail decorrelates via the different delay tunings.
///
/// Output is **effect-only** (`output[ch] = reverb_signal * wet[ch]`).
/// Dry is handled externally by the `Chain`.
///
/// Parameters:
/// - `room_size` (0–1): comb feedback (maps to ~0.70–0.98)
/// - `damping`   (0–1): high-frequency absorption in feedback loop
/// - `wet`       (0–1): output level, same for both channels
/// - `wet_l` / `wet_r`: per-channel output level
pub struct Reverb {
    params_info: [ParamInfo; 4],
    key: String,
    combs: [Vec<CombFilter>; 2],
    allpasses: [Vec<AllpassFilter>; 2],
    active: bool,
    room_size: f32,
    damping: f32,
    /// Per-channel output level: `[left, right]`.
    wet: [f32; 2],
    /// `wet[ch] / num_combs` — precomputed in `update_params` so the audio
    /// loop does a single multiply instead of a multiply + divide per sample.
    wet_norm: [f32; 2],
}

fn build_params_info(param_type_props: &HashMap<String, OverrideValue>) -> [ParamInfo; 4] {
    [
        ParamInfo::new_discrete_bool("active", true, None),
        ParamInfo::new_continuous_float(
            "room_size",
            override_float(param_type_props, "reverb.room_size.min", 0.0),
            override_float(param_type_props, "reverb.room_size.max", 1.0),
            override_float(param_type_props, "reverb.room_size.default", 0.7),
            false,
            None,
        ),
        ParamInfo::new_continuous_float(
            "damping",
            override_float(param_type_props, "reverb.damping.min", 0.0),
            override_float(param_type_props, "reverb.damping.max", 1.0),
            override_float(param_type_props, "reverb.damping.default", 0.5),
            false,
            None,
        ),
        ParamInfo::new_continuous_float(
            "wet",
            override_float(param_type_props, "reverb.wet.min", 0.0),
            override_float(param_type_props, "reverb.wet.max", 1.0),
            override_float(param_type_props, "reverb.wet.default", 0.3),
            false,
            None,
        ),
    ]
}

impl Reverb {
    pub fn new(key: impl Into<String>, sample_rate: f32,
        param_type_props: &HashMap<String, OverrideValue>) -> Self {
        let params_info = build_params_info(param_type_props);
        let active = find_param_info(&params_info,"active").bool_default();
        let room_size = find_param_info(&params_info,"room_size").continuous_float_default();
        let damping = find_param_info(&params_info,"damping").continuous_float_default();
        let wet = find_param_info(&params_info,"wet").continuous_float_default();

        let build_combs = |tuning: &[usize]| {
            tuning.iter().map(|&s| CombFilter::new(scale_size(s, sample_rate))).collect()
        };
        let build_allpasses = |tuning: &[usize]| {
            tuning.iter().map(|&s| AllpassFilter::new(scale_size(s, sample_rate))).collect()
        };

        let mut r = Self {
            params_info, key: key.into(),
            combs: [build_combs(&COMB_TUNING_L), build_combs(&COMB_TUNING_R)],
            allpasses: [build_allpasses(&ALLPASS_TUNING_L), build_allpasses(&ALLPASS_TUNING_R)],
            active, room_size, damping, wet: [wet; 2], wet_norm: [0.0; 2],
        };
        r.update_params();
        r
    }

    fn update_params(&mut self) {
        let feedback = self.room_size * 0.28 + 0.70;
        let damp = self.damping * 0.4; // Freeverb scaleDamp: user 1.0 → internal 0.4
        let inv_num_combs = 1.0 / self.combs[0].len() as f32;
        for ch in 0..2 {
            for comb in &mut self.combs[ch] {
                comb.feedback = feedback;
                comb.damp = damp;
            }
            self.wet_norm[ch] = self.wet[ch] * inv_num_combs;
        }
    }
}

impl Parameterized for Reverb {
    fn get_params_info(&self) -> &[ParamInfo] {
        &self.params_info
    }

    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active" => {
                self.active = value.try_bool()?;
                Ok(())
            },
            "room_size" => {
                let info = find_param_info(self.get_params_info(), "room_size");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.room_size = v;
                self.update_params();
                r
            },
            "damping" => {
                let info = find_param_info(self.get_params_info(), "damping");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.damping = v;
                self.update_params();
                r
            },
            "wet" => {
                let info = find_param_info(self.get_params_info(), "wet");
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds(info, l, NAME);
                let (vr, rr) = check_bounds(info, r, NAME);
                self.wet = [vl, vr];
                self.update_params();
                rl.and(rr)
            },
            _ => Err(format!("{}: unknown param '{param}'", NAME)),
        }
    }
}

impl Device for Reverb {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { NAME }

    fn is_active(&self) -> bool { self.active }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        for e in eff.iter_mut() {
            let inp = *e;
            // Mono mix of current signal; L/R decorrelate via different delay tunings.
            let reverb_in = (inp[0] + inp[1]) * 0.5;

            for ch in 0..2 {
                let mut acc = 0.0_f32;
                for comb in &mut self.combs[ch] {
                    acc += comb.process(reverb_in);
                }

                let mut sig = acc;
                for ap in &mut self.allpasses[ch] {
                    sig = ap.process(sig);
                }

                e[ch] = inp[ch] + sig * self.wet_norm[ch];
            }
        }
    }

    fn reset(&mut self) {
        for ch in 0..2 {
            for c in &mut self.combs[ch] { c.clear(); }
            for a in &mut self.allpasses[ch] { a.clear(); }
        }
    }
}
