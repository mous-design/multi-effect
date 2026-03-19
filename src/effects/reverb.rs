use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

// ---------------------------------------------------------------------------
// Freeverb delay tunings (samples at 44100 Hz; scaled at runtime)
// L and R use slightly different lengths for natural stereo spread.
// ---------------------------------------------------------------------------
const COMB_TUNING_L: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const COMB_TUNING_R: [usize; 8] = [1139, 1211, 1300, 1379, 1445, 1514, 1580, 1640];
const ALLPASS_TUNING_L: [usize; 4] = [556, 441, 341, 225];
const ALLPASS_TUNING_R: [usize; 4] = [579, 464, 364, 248];

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
    pub key: String,
    combs: [Vec<CombFilter>; 2],
    allpasses: [Vec<AllpassFilter>; 2],
    pub room_size: f32,
    pub damping: f32,
    /// Per-channel output level: `[left, right]`.
    pub wet: [f32; 2],
    pub active: bool,
}

impl Reverb {
    pub fn new(key: impl Into<String>, sample_rate: f32) -> Self {
        let build_combs = |tuning: &[usize]| {
            tuning.iter().map(|&s| CombFilter::new(scale_size(s, sample_rate))).collect()
        };
        let build_allpasses = |tuning: &[usize]| {
            tuning.iter().map(|&s| AllpassFilter::new(scale_size(s, sample_rate))).collect()
        };

        let mut r = Self {
            key: key.into(),
            combs: [build_combs(&COMB_TUNING_L), build_combs(&COMB_TUNING_R)],
            allpasses: [build_allpasses(&ALLPASS_TUNING_L), build_allpasses(&ALLPASS_TUNING_R)],
            room_size: 0.7,
            damping: 0.5,
            wet: [0.3; 2],
            active: true,
        };
        r.update_params();
        r
    }

    fn update_params(&mut self) {
        let feedback = self.room_size * 0.28 + 0.70;
        let damp = self.damping * 0.4; // Freeverb scaleDamp: user 1.0 → internal 0.4
        for ch in 0..2 {
            for comb in &mut self.combs[ch] {
                comb.feedback = feedback;
                comb.damp = damp;
            }
        }
    }
}

impl Parameterized for Reverb {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active"    => { self.active = value.try_bool()?; Ok(()) }
            "room_size" => { let (v, r) = check_bounds("Reverb", "room_size", value.try_float()?, 0.0, 1.0); self.room_size = v; self.update_params(); r }
            "damping"   => { let (v, r) = check_bounds("Reverb", "damping",   value.try_float()?, 0.0, 1.0); self.damping   = v; self.update_params(); r }
            "wet"       => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Reverb", "wet", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Reverb", "wet", r, 0.0, 1.0);
                self.wet = [vl, vr];
                rl.and(rr)
            }
            _ => Err(format!("Reverb: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active"    => Some(if self.active { 1.0 } else { 0.0 }),
            "room_size" => Some(self.room_size),
            "damping"   => Some(self.damping),
            "wet"       => Some(self.wet[0]),
            _ => None,
        }
    }
}

impl Device for Reverb {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { "reverb" }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),    self.active.into());
        m.insert("room_size".into(), self.room_size.into());
        m.insert("damping".into(),   self.damping.into());
        m.insert("wet".into(),       serde_json::json!(self.wet));
        m
    }

    fn process(&mut self, dry: &[Frame], eff: &mut [Frame]) {
        let num_combs = self.combs[0].len() as f32;

        for (e, &d) in eff.iter_mut().zip(dry.iter()) {
            let inp = [d[0] + e[0], d[1] + e[1]];
            // Mono mix of (dry + prev_eff); L/R decorrelate via different delay tunings.
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

                e[ch] = inp[ch] * (1.0 - self.wet[ch]) + sig * self.wet[ch] / num_combs;
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
