use crate::engine::device::{find_param_info, check_bounds, into_param_array,
    ParamInfo, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

pub const NAME: &str = "delay";

/// Stereo feedback delay.
///
/// Each channel has its own `RingBuffer`; processing is independent per
/// channel, so the effect works naturally for both stereo and dual-mono use.
///
/// Signal model per sample per channel:
/// ```text
/// delayed = buf.read_at(delay_samples)        // tap from `time` seconds ago
/// buf.write(inp + delayed * feedback)         // recurrence — feedback drives repeats
/// output  = inp + delayed * wet               // input passes through; wet tap is added
/// ```
///
/// Per the chain convention, `inp` already carries `dry + prior_effects`
/// (the `Chain` mixes them in before each node). The `Mix` node at the chain
/// tail subtracts the dry component so the engine emits wet-only — dry is
/// re-added in analog downstream.
pub struct Delay {
    params_info: [ParamInfo; 4],
    key: String,
    bufs: [RingBuffer; 2],
    active: bool,
    delay_samples: usize,
    feedback: f32,
    /// Output level applied equally to both channels. 0.0 = silent, 1.0 = full.
    wet: f32,
    sample_rate: f32,
}

pub static CANONICAL: [ParamInfo; 4] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("time",     0.1, 2.0, 1.0, true,  Some("s")).with_non_growable(),
    ParamInfo::new_continuous_float("feedback", 0.0, 1.0, 0.4, false, None),
    ParamInfo::new_continuous_float("wet",      0.0, 1.0, 0.5, false, None),
];

impl Delay {
    pub fn new(key: impl Into<String>, sample_rate: f32, params_info: &[ParamInfo]) -> Self {
        let params_info = into_param_array(params_info, CANONICAL, NAME);
        let feedback = find_param_info(&params_info,"feedback").continuous_float_default();
        let wet = find_param_info(&params_info,"wet").continuous_float_default();
        let active = find_param_info(&params_info,"active").bool_default();
        let info_time = find_param_info(&params_info,"time");
        let max_samples = (sample_rate * info_time.continuous_float_max()) as usize;
        let delay_samples = (sample_rate * info_time.continuous_float_default()) as usize;
        Self {
            params_info, key: key.into(),
            bufs: [RingBuffer::new(max_samples), RingBuffer::new(max_samples)],
            delay_samples,
            feedback, wet, active, sample_rate,
        }
    }
}

impl Parameterized for Delay {
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
            "time" => {
                let info = find_param_info(self.get_params_info(), "time");
                let (v, r) =
                    check_bounds(info, value.try_float()?, NAME);
                self.delay_samples = (v * self.sample_rate) as usize;
                r
            },
            "feedback" => {
                let info = find_param_info(self.get_params_info(), "feedback");
                let (v, r) =
                    check_bounds(info, value.try_float()?, NAME);
                self.feedback = v;
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

impl Device for Delay {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { NAME }

    fn is_active(&self) -> bool { self.active }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        for e in eff.iter_mut() {
            for ch in 0..2 {
                let inp = e[ch];
                let delayed = self.bufs[ch].read_at(self.delay_samples);
                self.bufs[ch].write(inp + delayed * self.feedback);
                e[ch] = inp + delayed * self.wet;
            }
        }
    }

    fn reset(&mut self) {
        for buf in &mut self.bufs {
            buf.clear();
        }
    }
}
