use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};

// ---------------------------------------------------------------------------
// Note voice (fractional playback of loop at a given pitch ratio)
// ---------------------------------------------------------------------------

/// Simple resampled playback voice triggered by MIDI note-on.
/// Pitch and playback speed change together (no time-stretching).
struct NoteVoice {
    note:         u8,
    frac_pos:     f32,    // fractional position in loop buffer
    ratio:        f32,    // playback speed = 2^((note - root) / 12)
    velocity:     f32,
    sustaining:   bool,
    release:      f32,
    release_step: f32,
}

/// Record, play back, and overdub an audio loop.
///
/// # Signal model
///
/// ```text
/// inp = dry + prev_eff   (provided by Chain)
///
/// Idle:
///   output = prev_eff                          prev_eff passes through unchanged
///
/// Recording:
///   buf[pos] = inp;  pos++                     capture dry + prev_eff
///   output = prev_eff                          prev_eff passes through unchanged
///
/// Playing:
///   loop = buf[pos % loop_len];  pos++
///   output = prev_eff + loop * loop_gain       prev_eff + loop
///
/// Overdub:
///   new_loop = buf[pos] * overdub_feedback + inp   mix into loop
///   buf[pos] = new_loop;  pos++
///   output = prev_eff + new_loop * loop_gain       prev_eff + updated loop
/// ```
///
/// The accumulated effect signal (`prev_eff`) always passes through so the
/// live chain signal is preserved.  The dry signal is only included in the
/// recording (and thus in loop playback after the fact), not in the live
/// pass-through.  Because `inp = dry + prev_eff`, pressing record while
/// playing captures everything up to this point in the chain.
///
/// # Parameters
///
/// | Param              | Range   | Default | Description                           |
/// |--------------------|---------|---------|---------------------------------------|
/// | `record`           | 0/1     | —       | 1 = start recording, 0 = stop → play  |
/// | `stop`             | any     | —       | stop playback, go idle                |
/// | `overdub`          | 0/1     | —       | 1 = enter overdub, 0 = back to play   |
/// | `loop_gain`        | 0–∞     | 1.0     | output gain on loop playback          |
/// | `overdub_feedback` | 0.0–1.0 | 1.0     | loop decay per overdub pass           |
pub struct Looper {
    pub key: String,
    buf: Vec<Frame>,
    state: LooperState,
    pos: usize,
    loop_len: usize,

    pub loop_gain: f32,
    /// Multiplier on existing loop content when mixing in new audio during overdub.
    /// 1.0 = infinite sustain, < 1.0 = gradual decay per pass.
    pub overdub_feedback: f32,
    pub active: bool,

    /// MIDI note that plays the loop at 1:1 speed. Default: 60 (C4).
    pub root: u8,
    sample_rate: f32,
    note_voices: Vec<NoteVoice>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperState {
    Idle,
    Recording,
    Playing,
    Overdub,
}

impl Looper {
    pub fn new(key: impl Into<String>, sample_rate: f32, max_seconds: f32) -> Self {
        let capacity = (sample_rate * max_seconds) as usize;
        Self {
            key: key.into(),
            buf: vec![[0.0; 2]; capacity],
            state: LooperState::Idle,
            pos: 0,
            loop_len: 0,
            loop_gain: 1.0,
            overdub_feedback: 1.0,
            active: true,
            root: 60,  // C4
            sample_rate,
            note_voices: Vec::new(),
        }
    }
    #[allow(dead_code)]
    pub fn state(&self) -> LooperState {
        self.state
    }

    pub fn start_recording(&mut self) {
        self.pos = 0;
        self.loop_len = 0;
        self.state = LooperState::Recording;
    }

    pub fn stop_recording(&mut self) {
        self.loop_len = self.pos;
        self.pos = 0;
        self.state = LooperState::Playing;
    }

    pub fn stop(&mut self) {
        self.state = LooperState::Idle;
        self.pos = 0;
    }
    
    #[allow(dead_code)]
    pub fn toggle_overdub(&mut self) {
        match self.state {
            LooperState::Playing => self.state = LooperState::Overdub,
            LooperState::Overdub => self.state = LooperState::Playing,
            _ => {}
        }
    }

}

impl Device for Looper {
    fn key(&self) -> &str { &self.key }

    fn type_name(&self) -> &str { "looper" }

    fn is_active(&self) -> bool { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),           self.active.into());
        m.insert("loop_gain".into(),        self.loop_gain.into());
        m.insert("overdub_feedback".into(), self.overdub_feedback.into());
        m.insert("root".into(),             serde_json::json!(self.root));
        m
    }

    fn process(&mut self, dry: &[Frame], eff: &mut [Frame]) {
        for i in 0..dry.len().min(eff.len()) {
            let prev_eff = eff[i];
            let inp = [dry[i][0] + prev_eff[0], dry[i][1] + prev_eff[1]];

            match self.state {
                LooperState::Idle => {
                    // prev_eff passes through unchanged.
                }

                LooperState::Recording => {
                    if self.pos < self.buf.len() {
                        self.buf[self.pos] = inp;
                        self.pos += 1;
                    }
                    // prev_eff passes through unchanged.
                }

                LooperState::Playing => {
                    if self.loop_len > 0 {
                        let loop_frame = self.buf[self.pos];
                        self.pos = (self.pos + 1) % self.loop_len;
                        eff[i] = [
                            prev_eff[0] + loop_frame[0] * self.loop_gain,
                            prev_eff[1] + loop_frame[1] * self.loop_gain,
                        ];
                    }
                }

                LooperState::Overdub => {
                    if self.loop_len > 0 {
                        let p = self.pos;
                        let new_loop = [
                            self.buf[p][0] * self.overdub_feedback + inp[0],
                            self.buf[p][1] * self.overdub_feedback + inp[1],
                        ];
                        self.buf[p] = new_loop;
                        self.pos = (self.pos + 1) % self.loop_len;
                        eff[i] = [
                            prev_eff[0] + new_loop[0] * self.loop_gain,
                            prev_eff[1] + new_loop[1] * self.loop_gain,
                        ];
                    }
                }
            }

            // Note voices: fractional resampled playback of the loop.
            // Pitch and duration both scale with ratio (simple resampling, no OLA).
            if self.loop_len > 0 {
                for vi in 0..self.note_voices.len() {
                    let fp   = self.note_voices[vi].frac_pos;
                    let vel  = self.note_voices[vi].velocity;
                    let rel  = self.note_voices[vi].release;
                    let rat  = self.note_voices[vi].ratio;
                    let sust = self.note_voices[vi].sustaining;
                    let rstep = self.note_voices[vi].release_step;

                    // Interpolated sample from loop buffer
                    let i0 = fp as usize % self.loop_len;
                    let i1 = (i0 + 1) % self.loop_len;
                    let t  = fp.fract();
                    let s0 = self.buf[i0];
                    let s1 = self.buf[i1];
                    let sample = [
                        s0[0] + (s1[0] - s0[0]) * t,
                        s0[1] + (s1[1] - s0[1]) * t,
                    ];

                    let amp = vel * rel * self.loop_gain;
                    eff[i][0] += sample[0] * amp;
                    eff[i][1] += sample[1] * amp;

                    // Advance fractional position, wrap within loop
                    let new_fp = fp + rat;
                    self.note_voices[vi].frac_pos = if new_fp >= self.loop_len as f32 {
                        new_fp % self.loop_len as f32
                    } else {
                        new_fp
                    };

                    // Release envelope
                    if !sust {
                        self.note_voices[vi].release = (rel - rstep).max(0.0);
                    }
                }
                self.note_voices.retain(|v| v.sustaining || v.release > 0.0);
            }
        }
    }

    fn reset(&mut self) {
        self.state = LooperState::Idle;
        self.pos = 0;
        self.loop_len = 0;
        self.buf.fill([0.0; 2]);
        self.note_voices.clear();
    }

    fn on_note_on(&mut self, note: u8, velocity: u8) {
        if velocity == 0 { self.on_note_off(note); return; }
        // Retrigger if same note already active
        if let Some(v) = self.note_voices.iter_mut().find(|v| v.note == note) {
            v.velocity   = velocity as f32 / 127.0;
            v.sustaining = true;
            v.release    = 1.0;
            v.frac_pos   = 0.0;
            return;
        }
        // Max 4 simultaneous note voices on the looper
        if self.note_voices.len() >= 4 {
            self.note_voices.remove(0);
        }
        let semitones    = note as f32 - self.root as f32;
        let ratio        = 2.0_f32.powf(semitones / 12.0);
        let release_step = 1.0 / (50.0_f32 / 1000.0 * self.sample_rate).max(1.0);
        self.note_voices.push(NoteVoice {
            note,
            frac_pos:     0.0,
            ratio,
            velocity:     velocity as f32 / 127.0,
            sustaining:   true,
            release:      1.0,
            release_step,
        });
    }

    fn on_note_off(&mut self, note: u8) {
        for v in &mut self.note_voices {
            if v.note == note { v.sustaining = false; }
        }
    }
}

impl Parameterized for Looper {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        if param == "active" { self.active = value.try_bool()?; return Ok(()); }
        let f = value.try_float()?;
        match param {
            "record"  if f > 0.5 => { self.start_recording(); Ok(()) }
            "record"             => { self.stop_recording();  Ok(()) }
            "stop"               => { self.stop();            Ok(()) }
            "overdub" if f > 0.5 => { self.state = LooperState::Overdub; Ok(()) }
            "overdub"            => {
                if self.state == LooperState::Overdub {
                    self.state = LooperState::Playing;
                }
                Ok(())
            }
            "loop_gain"        => { let (v, r) = check_bounds("Looper", "loop_gain", f, 0.0, 4.0); self.loop_gain = v; r }
            "overdub_feedback" => { let (v, r) = check_bounds("Looper", "overdub_feedback", f, 0.0, 1.0); self.overdub_feedback = v; r }
            "root"             => { let (v, r) = check_bounds("Looper", "root", f, 0.0, 127.0); self.root = v as u8; r }
            _ => Err(format!("Looper: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active"           => Some(if self.active { 1.0 } else { 0.0 }),
            "loop_gain"        => Some(self.loop_gain),
            "overdub_feedback" => Some(self.overdub_feedback),
            "root"             => Some(self.root as f32),
            _ => None,
        }
    }
}
