use std::f32::consts::TAU;

use crate::engine::device::{find_param_info, check_bounds, into_param_array,
    ParamInfo, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

pub const NAME: &str = "chorus";

/// LFO-driven modulation delay (chorus / flanger).
///
/// Each channel has its own ring buffer and LFO phase. L and R are
/// initialised 90° apart so the effect widens the stereo image naturally.
///
/// Signal model per sample per channel:
/// ```text
/// buf.write(input)
/// center     = depth_ms * sample_rate / 1000      // delay-line midpoint, in samples
/// offset     = center * (1.0 + sin(lfo_phase))    // 0 .. 2*center
/// wet_sample = lerp(buf, offset)
/// output     = inp + wet_sample * wet             // input passes through; wet tap is added
/// lfo_phase += 2π * rate_hz / sample_rate
/// ```
///
/// Per the chain convention, `inp` already carries `dry + prior_effects`;
/// the `Mix` node at the chain tail subtracts the dry component so the engine
/// output is wet-only — dry is re-added in analog downstream.
pub struct Chorus {
    params_info: [ParamInfo; 4],
    key: String,
    bufs: [RingBuffer; 2],
    active: bool,
    rate_hz: f32,
    depth_ms: f32,
    /// Output level applied equally to both channels. 0.0 = silent, 1.0 = full.
    wet: f32,
    sample_rate: f32,
    lfo_phase: [f32; 2],
}
pub static CANONICAL: [ParamInfo; 4] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("rate_hz",  0.1,  10.0, 1.0, true,  Some("Hz")),
    ParamInfo::new_continuous_float("depth_ms", 0.1,  30.0, 8.0, true, Some("ms")).with_non_growable(),
    ParamInfo::new_continuous_float("wet",      0.0,  1.0,  0.5, false, None),
];

impl Chorus {
    pub fn new(key: impl Into<String>, sample_rate: f32, params_info: &[ParamInfo]) -> Self {
        let params_info = into_param_array(params_info, CANONICAL, NAME);
        let active = find_param_info(&params_info,"active").bool_default();
        let rate_hz = find_param_info(&params_info,"rate_hz").continuous_float_default();
        let wet = find_param_info(&params_info,"wet").continuous_float_default();
        let info_depth = find_param_info(&params_info,"depth_ms");
        let depth_ms = info_depth.continuous_float_default();
        // Add 2, because 1 for the sin() is going from -1.0 to 1.0 (not 0.99999), and 1 for the linear interpolation 
        let max_samples = (2.0 * sample_rate * info_depth.continuous_float_max() / 1000.0) as usize + 2;
        Self {
            params_info, key: key.into(),
            bufs: [RingBuffer::new(max_samples), RingBuffer::new(max_samples)],
            active, rate_hz, depth_ms, wet, sample_rate,
            lfo_phase: [0.0, TAU / 4.0], // 90° spread between L and R   
        }
    }
}

impl Parameterized for Chorus {
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
            "rate_hz"  => {
                let info = find_param_info(self.get_params_info(), "rate_hz");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.rate_hz  = v;
                r
            },
            "depth_ms" => {
                let info = find_param_info(self.get_params_info(), "depth_ms");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.depth_ms = v;
                r
            },
            "wet" => {
                let info = find_param_info(self.get_params_info(), "wet");
                let (v, r) = check_bounds(info, value.try_float()?, NAME);
                self.wet = v;
                r
            },
            _ => Err(format!("{}: unknown param '{param}'", NAME)),
        }
    }
}

impl Device for Chorus {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { NAME }

    fn is_active(&self) -> bool { self.active }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        let phase_inc = self.rate_hz * TAU / self.sample_rate;
        let center = self.depth_ms * self.sample_rate / 1000.0;

        for e in eff.iter_mut() {
            for ch in 0..2 {
                let inp = e[ch];
                self.bufs[ch].write(inp);

                // goes from -1.0 .. 1.0
                let lfo = self.lfo_phase[ch].sin();
                // goes from 0 .. cap - 2
                let offset = center * (1.0 + lfo);

                // Read the float offset with linear interpolation
                e[ch] = inp + self.bufs[ch].read_lerp(offset) * self.wet;

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
