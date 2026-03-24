use std::f32::consts::TAU;

use tracing::debug;

use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum simultaneous harmonized voices.
const MAX_VOICES: usize = 8;

/// Input ring buffer length in milliseconds.
const INPUT_BUF_MS: f32 = 200.0;

/// Default grain size in milliseconds (OLA window).
const DEFAULT_GRAIN_MS: f32 = 50.0;

/// Note-off fade time in milliseconds.
const RELEASE_MS: f32 = 50.0;

// ---------------------------------------------------------------------------
// Voice
// ---------------------------------------------------------------------------

struct Voice {
    note:         u8,
    ratio:        f32,    // playback speed = 2^(semitones/12)
    velocity:     f32,    // 0.0 – 1.0
    sustaining:   bool,   // true = note held, false = releasing
    release:      f32,    // 1.0 → 0.0 during note-off fade
    release_step: f32,    // per-sample decrement
    grain_phase:  usize,  // position within grain cycle (0..grain_size)
    read_a:       f32,    // fractional read position for grain A
    read_b:       f32,    // fractional read position for grain B (offset by half grain)
}

// ---------------------------------------------------------------------------
// Free helpers (avoid borrow conflicts inside process)
// ---------------------------------------------------------------------------

#[inline]
fn lerp_frame(buf: &[Frame], pos: f32, n: usize) -> Frame {
    let i0 = pos as usize % n;
    let i1 = (i0 + 1) % n;
    let t  = pos.fract();
    let a  = buf[i0];
    let b  = buf[i1];
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

// ---------------------------------------------------------------------------
// Harmonizer
// ---------------------------------------------------------------------------

/// Real-time OLA pitch-shifter / harmonizer.
///
/// Records a continuous ring buffer of the input signal and plays back
/// one voice per held MIDI note, each at a different pitch ratio relative
/// to the configured `root` note.  Up to [`MAX_VOICES`] simultaneous voices.
///
/// # Signal model
/// ```text
/// inp = dry + prev_eff          (per Chain convention)
/// input ring buffer ← inp       (continuous capture)
/// for each active voice:
///     mix += OLA_read(ring_buf, voice.ratio) * velocity * envelope
/// eff = prev_eff + mix * wet
/// ```
///
/// # Parameters
/// | Param    | Range    | Default | Description                          |
/// |----------|----------|---------|--------------------------------------|
/// | `active` | bool     | true    | bypass the effect entirely           |
/// | `wet`    | 0.0–1.0  | 1.0     | wet level (stereo)                   |
/// | `root`   | 0–127    | 57      | MIDI note that maps to ratio 1:1 (A3)|
///
/// # MIDI
/// Note On → start voice.  Note Off → begin 50 ms fade-out.
pub struct Harmonizer {
    pub key:     String,
    pub active:  bool,
    pub wet:     [f32; 2],
    /// MIDI note that plays back at 1:1 speed (unshifted).
    pub root:      u8,
    /// Velocity sensitivity: 0.0 = ignore velocity (fixed volume), 1.0 = full sensitivity.
    pub vel_sense: f32,

    input_buf:   Vec<Frame>,
    write_pos:   usize,
    buf_size:    usize,
    grain_size:  usize,
    half_grain:  usize,
    hann_table:  Vec<f32>,

    sample_rate: f32,
    voices:      Vec<Voice>,
    debug_tick:  u32,   // rate-limiter for in-process debug logging
}

impl Harmonizer {
    pub fn new(key: impl Into<String>, sample_rate: f32) -> Self {
        let buf_size   = (sample_rate * INPUT_BUF_MS / 1000.0) as usize;
        // Grain size in samples (no power-of-two rounding — must stay well below buf_size/2)
        let grain_size = ((sample_rate * DEFAULT_GRAIN_MS / 1000.0) as usize).max(64);
        let half_grain = grain_size / 2;
        let hann_table = (0..grain_size)
            .map(|i| 0.5 * (1.0 - (TAU * i as f32 / grain_size as f32).cos()))
            .collect();

        Self {
            key:         key.into(),
            active:      true,
            wet:         [1.0; 2],
            root:      57, // A3
            vel_sense: 0.0,
            input_buf:   vec![[0.0; 2]; buf_size],
            write_pos:   0,
            buf_size,
            grain_size,
            half_grain,
            hann_table,
            sample_rate,
            voices:      Vec::with_capacity(MAX_VOICES),
            debug_tick:  0,
        }
    }

    fn add_voice(&mut self, note: u8, velocity: u8) {
        // Retrigger if same note already active
        if let Some(v) = self.voices.iter_mut().find(|v| v.note == note) {
            v.velocity   = velocity as f32 / 127.0;
            v.sustaining = true;
            v.release    = 1.0;
            return;
        }
        // Steal oldest slot when at capacity
        if self.voices.len() >= MAX_VOICES {
            if let Some(idx) = self.voices.iter().position(|v| !v.sustaining && v.release <= 0.0) {
                self.voices.remove(idx);
            } else {
                self.voices.remove(0);
            }
        }
        let semitones    = note as f32 - self.root as f32;
        let ratio        = 2.0_f32.powf(semitones / 12.0);
        let release_step = 1.0 / (RELEASE_MS / 1000.0 * self.sample_rate).max(1.0);
        let n            = self.buf_size;
        let behind       = (self.grain_size * 2).min(n.saturating_sub(1));
        let read_start   = (self.write_pos + n - behind) % n;

        self.voices.push(Voice {
            note,
            ratio,
            velocity:     velocity as f32 / 127.0,
            sustaining:   true,
            release:      1.0,
            release_step,
            grain_phase:  0,
            read_a:       read_start as f32,
            read_b:       ((read_start + self.half_grain) % n) as f32,
        });
    }
}

// ---------------------------------------------------------------------------
// Parameterized
// ---------------------------------------------------------------------------

impl Parameterized for Harmonizer {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active" => { self.active = value.try_bool()?; Ok(()) }
            "wet" => {
                let [l, r] = value.try_stereo()?;
                let (vl, rl) = check_bounds("Harmonizer", "wet", l, 0.0, 1.0);
                let (vr, rr) = check_bounds("Harmonizer", "wet", r, 0.0, 1.0);
                self.wet = [vl, vr];
                rl.and(rr)
            }
            "root" => {
                let (v, r) = check_bounds("Harmonizer", "root", value.try_float()?, 0.0, 127.0);
                self.root = v as u8;
                r
            }
            "vel_sense" => {
                let (v, r) = check_bounds("Harmonizer", "vel_sense", value.try_float()?, 0.0, 1.0);
                self.vel_sense = v;
                r
            }
            _ => Err(format!("Harmonizer: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active" => Some(if self.active { 1.0 } else { 0.0 }),
            "wet"       => Some(self.wet[0]),
            "root"      => Some(self.root as f32),
            "vel_sense" => Some(self.vel_sense),
            _           => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

impl Device for Harmonizer {
    fn key(&self)       -> &str  { &self.key }
    fn type_name(&self) -> &str  { "harmonizer" }
    fn is_active(&self) -> bool  { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(), self.active.into());
        m.insert("wet".into(),    serde_json::json!(self.wet));
        m.insert("root".into(),      serde_json::json!(self.root));
        m.insert("vel_sense".into(), serde_json::json!(self.vel_sense));
        m
    }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        let gs   = self.grain_size;
        let half = self.half_grain;
        let n    = self.buf_size;

        for i in 0..eff.len() {
            let prev_eff = eff[i];
            let inp      = prev_eff;
            self.input_buf[self.write_pos] = inp;
            self.write_pos = (self.write_pos + 1) % n;
            let wp = self.write_pos;

            let mut mix = [0.0f32; 2];

            for vi in 0..self.voices.len() {
                // --- Read all voice state (short-lived borrows) ---
                let phase      = self.voices[vi].grain_phase;
                let b_phase    = (phase + half) % gs;
                let ratio      = self.voices[vi].ratio;
                let raw_vel    = self.voices[vi].velocity;
        let vel        = self.vel_sense * raw_vel + (1.0 - self.vel_sense);
                let rel        = self.voices[vi].release;
                let sust       = self.voices[vi].sustaining;
                let rstep      = self.voices[vi].release_step;
                let ra         = self.voices[vi].read_a;
                let rb         = self.voices[vi].read_b;

                // --- Hann window + interpolated samples ---
                let w_a = self.hann_table[phase];
                let w_b = self.hann_table[b_phase];
                let s_a = lerp_frame(&self.input_buf, ra, n);
                let s_b = lerp_frame(&self.input_buf, rb, n);

                let amp = vel * rel;
                mix[0] += (s_a[0] * w_a + s_b[0] * w_b) * amp;
                mix[1] += (s_a[1] * w_a + s_b[1] * w_b) * amp;

                // --- Advance read positions ---
                let mut new_ra = ra + ratio;
                let mut new_rb = rb + ratio;
                if new_ra >= n as f32 { new_ra -= n as f32; }
                if new_rb >= n as f32 { new_rb -= n as f32; }

                // --- Drift correction at grain boundaries (window = 0 → inaudible) ---
                // At phase == 0:    grain A window is 0 → safe to snap read_a
                // At phase == half: grain B window is 0 → safe to snap read_b
                if phase == 0 {
                    let behind = (wp + n - new_ra as usize) % n;
                    if behind < gs || behind > n - gs {
                        new_ra = ((wp + n - gs * 2) % n) as f32;
                    }
                }
                if phase == half {
                    let behind = (wp + n - new_rb as usize) % n;
                    if behind < gs || behind > n - gs {
                        new_rb = ((wp + n - gs * 2 + half) % n) as f32;
                    }
                }

                // --- Advance grain phase ---
                let new_phase = (phase + 1) % gs;

                // --- Release envelope ---
                let new_rel = if sust { rel } else { (rel - rstep).max(0.0) };

                // --- Write back ---
                self.voices[vi].read_a      = new_ra;
                self.voices[vi].read_b      = new_rb;
                self.voices[vi].grain_phase = new_phase;
                self.voices[vi].release     = new_rel;
            }

            // Remove fully silent voices
            self.voices.retain(|v| v.sustaining || v.release > 0.0);

            // Additive blend: wet controls how much harmony is added on top of inp
            eff[i] = [
                inp[0] + mix[0] * self.wet[0],
                inp[1] + mix[1] * self.wet[1],
            ];

            // Rate-limited debug: log once per second when voices are active
            if !self.voices.is_empty() {
                self.debug_tick += 1;
                if self.debug_tick >= self.sample_rate as u32 {
                    self.debug_tick = 0;
                    debug!("Harmonizer '{}': {} voice(s) | inp=[{:.4},{:.4}] mix=[{:.4},{:.4}] wet={:.2}",
                        self.key, self.voices.len(), inp[0], inp[1], mix[0], mix[1], self.wet[0]);
                }
            } else {
                self.debug_tick = 0;
            }
        }
    }

    fn reset(&mut self) {
        self.voices.clear();
        self.write_pos = 0;
        self.input_buf.fill([0.0; 2]);
    }

    fn on_note_on(&mut self, note: u8, velocity: u8) {
        // MIDI spec: note-on with velocity 0 = note-off
        if velocity == 0 { self.on_note_off(note); return; }
        let semitones = note as f32 - self.root as f32;
        let ratio     = 2.0_f32.powf(semitones / 12.0);
        debug!("Harmonizer '{}': note-on {} vel={} root={} ratio={:.3} voices={}/{}",
            self.key, note, velocity, self.root, ratio, self.voices.len() + 1, MAX_VOICES);
        self.add_voice(note, velocity);
    }

    fn on_note_off(&mut self, note: u8) {
        let found = self.voices.iter().any(|v| v.note == note);
        if found {
            debug!("Harmonizer '{}': note-off {}", self.key, note);
        }
        for v in &mut self.voices {
            if v.note == note {
                v.sustaining = false;
            }
        }
    }
}
