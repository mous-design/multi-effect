use std::f32::consts::TAU;

use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

/// LFO-driven modulation delay (chorus / flanger).
///
/// Each channel has its own ring buffer and LFO phase.  L and R are
/// initialised 90° apart so the effect widens the stereo image naturally.
///
/// Signal model per sample per channel:
/// ```text
/// buf.write(input)
/// offset     = depth_samples + sin(lfo_phase) * depth_samples * 0.5
/// wet_sample = lerp(buf, offset)
/// output     = wet_sample * wet[ch]
/// lfo_phase += 2π * rate_hz / sample_rate
/// ```
///
/// Output is **effect-only** — dry is handled by the `Chain`.
pub struct Chorus {
    pub key: String,
    bufs: [RingBuffer; 2],
    pub rate_hz: f32,
    pub depth_ms: f32,
    /// Per-channel output level: `[left, right]`. 0.0 = silent, 1.0 = full.
    pub wet: [f32; 2],
    pub active: bool,
    lfo_phase: [f32; 2],
    sample_rate: f32,
}

impl Chorus {
    pub fn new(key: impl Into<String>, sample_rate: f32) -> Self {
        let max_samples = (sample_rate * 0.040) as usize + 2; // 40 ms + guard
        Self {
            key: key.into(),
            bufs: [RingBuffer::new(max_samples), RingBuffer::new(max_samples)],
            rate_hz: 1.0,
            depth_ms: 8.0,
            wet: [0.5; 2],
            active: true,
            lfo_phase: [0.0, TAU / 4.0], // 90° spread between L and R
            sample_rate,
        }
    }
}

impl Parameterized for Chorus {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active"   => { self.active = value.try_bool()?; Ok(()) }
            "rate_hz"  => { let (v, r) = check_bounds("Chorus", "rate_hz",  value.try_float()?, 0.01, 20.0); self.rate_hz  = v; r }
            "depth_ms" => { let (v, r) = check_bounds("Chorus", "depth_ms", value.try_float()?, 0.5,  35.0); self.depth_ms = v; r }
            "wet"      => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Chorus", "wet", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Chorus", "wet", r, 0.0, 1.0);
                self.wet = [vl, vr];
                rl.and(rr)
            }
            _ => Err(format!("Chorus: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active"   => Some(if self.active { 1.0 } else { 0.0 }),
            "rate_hz"  => Some(self.rate_hz),
            "depth_ms" => Some(self.depth_ms),
            "wet"      => Some(self.wet[0]),
            _ => None,
        }
    }
}

impl Device for Chorus {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { "chorus" }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),   self.active.into());
        m.insert("rate_hz".into(),  self.rate_hz.into());
        m.insert("depth_ms".into(), self.depth_ms.into());
        m.insert("wet".into(),      serde_json::json!(self.wet));
        m
    }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        let phase_inc = self.rate_hz * TAU / self.sample_rate;
        let center = self.depth_ms * self.sample_rate / 1000.0;
        let mod_depth = center * 0.5;
        let cap = self.bufs[0].capacity() as f32;

        for e in eff.iter_mut() {
            for ch in 0..2 {
                let inp = e[ch];
                self.bufs[ch].write(inp);

                let lfo = self.lfo_phase[ch].sin();
                let offset = (center + lfo * mod_depth).clamp(1.0, cap - 1.0);

                e[ch] = inp + self.bufs[ch].read_lerp(offset) * self.wet[ch];

                self.lfo_phase[ch] += phase_inc;
                if self.lfo_phase[ch] >= TAU {
                    self.lfo_phase[ch] -= TAU;
                }
            }
        }
    }

    fn reset(&mut self) {
        for buf in &mut self.bufs {
            buf.clear();
        }
        self.lfo_phase = [0.0, TAU / 4.0];
    }
}
