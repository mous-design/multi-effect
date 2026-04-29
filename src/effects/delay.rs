use std::collections::HashMap;
use crate::engine::device::{override_float, find_param_info, check_bounds,
    ParamInfo, OverrideValue, Device, Frame, Parameterized, ParamValue};
use crate::engine::ring_buffer::RingBuffer;

pub const NAME: &str = "delay";

/// Stereo feedback delay.
///
/// Each channel has its own `RingBuffer`; processing is independent per
/// channel, so the effect works naturally for both stereo and dual-mono use.
///
/// Signal model per sample per channel:
/// ```text
/// delayed = buf.read_at(delay_samples)         // tap from `time` seconds ago
/// buf.write(inp + delayed * feedback)          // recurrence — feedback drives repeats
/// output  = inp + delayed * wet[ch]            // input passes through; wet tap is added
/// ```
///
/// Per the chain convention, `inp` already carries `dry + prior_effects`
/// (the `Chain` mixes them in before each node). The `Mix` node at the chain
/// tail subtracts the dry component so the engine emits wet-only — dry is
/// re-added in analog downstream.
pub struct Delay {
    params_info: [ParamInfo; 4],
    pub key: String,
    bufs: [RingBuffer; 2],
    pub active: bool,
    delay_samples: usize,
    pub feedback: f32,
    /// Per-channel output level: `[left, right]`. 0.0 = silent, 1.0 = full.
    pub wet: [f32; 2],
    sample_rate: f32,
}
fn build_params_info(param_type_props: &HashMap<String, OverrideValue>) -> [ParamInfo; 4] {
    [
        ParamInfo::new_discrete_bool("active", true, None),
        ParamInfo::new_continuous_float(
            "time",
            override_float(param_type_props, "delay.time.min", 0.1),
            override_float(param_type_props, "delay.time.max", 2.0),
            override_float(param_type_props, "delay.time.default", 1.0),
            true, // @todo see how this works.
            None,
            Some("s"),
        ),
        ParamInfo::new_continuous_float(
            "feedback",
            override_float(param_type_props, "delay.feedback.min", 0.0),
            override_float(param_type_props, "delay.feedback.max", 1.0),
            override_float(param_type_props, "delay.feedback.default", 0.4),
            false,
            None,
            None,
        ),
        ParamInfo::new_continuous_float(
            "wet",
            override_float(param_type_props, "delay.wet.min", 0.0),
            override_float(param_type_props, "delay.wet.max", 1.0),
            override_float(param_type_props, "delay.wet.default", 0.5),
            false,
            None,
            None,
        ),
    ]
}
impl Delay {
    pub fn new(key: impl Into<String>, sample_rate: f32, param_type_props: &HashMap<String, OverrideValue>) -> Self {
        let params_info = build_params_info(param_type_props);
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
            feedback, wet: [wet; 2], active, sample_rate,
        }
    }
}

impl Parameterized for Delay {
     fn get_params_info(&self) -> &[ParamInfo] {
        &self.params_info
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
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds(info, l, NAME);
                let (vr, rr) = check_bounds(info, r, NAME);
                self.wet = [vl, vr];
                rl.and(rr)
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
