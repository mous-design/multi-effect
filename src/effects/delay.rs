use tracing::warn;
use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

const DELAY_MIN_SAMPLES:usize = 50;
/// Stereo tape-style delay.
///
/// Each channel has its own `RingBuffer`.  Processing is independent per
/// channel, which works naturally for both stereo and dual-mono use.
///
/// Signal model per channel:
/// ```text
/// delayed  = buf.read_at(delay_samples)
/// buf.write(input + delayed * feedback)
/// output   = delayed * wet[ch]
/// ```
///
/// The dry signal is **not** added here — it is injected by the `Chain`
/// before this node (`inp = dry + prev_effect`).
pub struct Delay {
    pub key: String,
    bufs: [RingBuffer; 2],
    delay_samples: usize,
    pub feedback: f32,
    /// Per-channel output level: `[left, right]`. 0.0 = silent, 1.0 = full.
    pub wet: [f32; 2],
    pub active: bool,
    sample_rate: f32,
}

impl Delay {
    pub fn new(key: impl Into<String>, sample_rate: f32, max_delay_seconds: f32) -> Self {
        let requested = (sample_rate * max_delay_seconds) as usize;
        let min_buf = 2 * DELAY_MIN_SAMPLES;
        if requested < min_buf {
            warn!("Delay: max_delay_seconds {max_delay_seconds} too small (< {} samples), clamping to {min_buf}", min_buf);
        }
        let max_samples = requested.max(min_buf) + 1;
        let default_delay = (sample_rate * 0.5) as usize;

        Self {
            key: key.into(),
            bufs: [RingBuffer::new(max_samples), RingBuffer::new(max_samples)],
            delay_samples: default_delay.min(max_samples),
            feedback: 0.4,
            wet: [0.5; 2],
            active: true,
            sample_rate,
        }
    }

    pub fn set_time(&mut self, seconds: f32) -> Result<(), String> {
        self.delay_samples = (seconds * self.sample_rate) as usize;
        let cap = self.bufs[0].capacity();
        if self.delay_samples >= cap {
            self.delay_samples = cap - 1;
            return Err(format!("Delay time clamped to {} samples", self.delay_samples));
        }
        if self.delay_samples < DELAY_MIN_SAMPLES {
            self.delay_samples = DELAY_MIN_SAMPLES;
            return Err(format!("Delay time too short, clamped to {} samples", DELAY_MIN_SAMPLES));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn time(&self) -> f32 {
        self.delay_samples as f32 / self.sample_rate
    }
}

impl Parameterized for Delay {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active"   => { self.active = value.try_bool()?; Ok(()) }
            "time"     => self.set_time(value.try_float()?),
            "feedback" => { let (v, r) = check_bounds("Delay", "feedback", value.try_float()?, 0.0, 1.0); self.feedback = v; r }
            "wet"      => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Delay", "wet", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Delay", "wet", r, 0.0, 1.0);
                self.wet = [vl, vr];
                rl.and(rr)
            },
            _ => Err(format!("Delay: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active"   => Some(if self.active { 1.0 } else { 0.0 }),
            "time"     => Some(self.time()),
            "feedback" => Some(self.feedback),
            "wet"      => Some(self.wet[0]),
            _ => None,
        }
    }
}

impl Device for Delay {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { "delay" }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),   self.active.into());
        m.insert("time".into(),     self.time().into());
        m.insert("feedback".into(), self.feedback.into());
        m.insert("wet".into(),      serde_json::json!(self.wet));
        m
    }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        for e in eff.iter_mut() {
            for ch in 0..2 {
                let inp = e[ch];
                let delayed = self.bufs[ch].read_at(self.delay_samples);
                self.bufs[ch].write(inp + delayed * self.feedback);
                e[ch] = inp + delayed * self.wet[ch];
            }
        }
    }

    fn reset(&mut self) {
        for buf in &mut self.bufs {
            buf.clear();
        }
    }
}
