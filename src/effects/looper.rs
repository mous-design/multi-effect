use crate::control::{ControlMessage, EventBus};
use crate::engine::device::{check_bounds, Device, Frame, Parameterized, ParamValue};
use tracing::warn;

const LOOP_FADE_SAMPLES: usize = 8;
const LOOP_FADE_STEP: f32 = 1.0 / LOOP_FADE_SAMPLES as f32;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperState {
    Idle,
    Recording,
    Playing,
    Overdub,
    Stop,
}

// ---------------------------------------------------------------------------
// Looper
// ---------------------------------------------------------------------------

/// Multi-layer looper with undo-capable overdub buffer stack.
///
/// # Signal model
///
/// ```text
/// inp = dry + prev_eff   (provided by Chain)
///
/// Idle / Stop:
///   output = prev_eff                        prev_eff passes through unchanged
///
/// Recording:
///   buffers[0][pos] = inp                    capture input
///   output = prev_eff                        pass-through during recording
///
/// Playing:
///   loop = playback_frame(pos)               decay-weighted sum of all layers
///   output = prev_eff + loop * wet
///
/// Overdub:
///   buffers[rec_layer][pos] = inp            record into new layer
///   loop = playback_frame(pos)               play all layers EXCEPT rec_layer
///   output = prev_eff + loop * wet
///   merge_buf[pos] = buf[0][pos] + decay * buf[1][pos]  (always maintained)
/// ```
///
/// # Layer terminology
///
/// - `buffers[0]` = base recording (always present in non-Idle state).
/// - `buffers[1..=overdub_count]` = completed overdub layers.
/// - `buffers[1+overdub_count]` = currently recording (only during Overdub state).
/// - `overdub_count` counts only overdub layers; 0 means only the base recording exists.
///
/// # Playback formula
///
/// Flat sum of all layers: `out = buf[0][pos] + buf[1][pos] + ...`
///
/// # Decay
///
/// Only active during Overdub.  Each pass through the loop, every sample in every
/// active buffer (including the currently-recording one) is multiplied by `decay`
/// in-place.  After N overdub passes, amplitude is `decay^N`.
/// Skipped entirely when `decay == 1.0`.
///
/// # Buffer management
///
/// - `buffers[0]` is pre-allocated at `init_len` samples (from `looper_max_seconds`).
/// - Additional overdub buffers are allocated on demand at `loop_len` each.
/// - `max_buffers = 0` means unlimited (memory only limit).
/// - When the stack is full: merge `buffers[0] + buffers[1]` → `buffers[0]`
///   using the pre-computed `merge_buf` (swapped in, O(1)), then shift the stack down.
///   The freed slot becomes the new recording buffer.
/// - All buffers are zero-filled on allocation (OS lazy-zeroed pages — no dropout risk).
///
/// # Parameters
///
/// | Param   | Range   | Default | Description                                  |
/// |---------|---------|---------|----------------------------------------------|
/// | `wet`   | 0.0–4.0 | 1.0     | Output gain for loop playback                |
/// | `decay` | 0.0–1.0 | 1.0     | Per-sample decay applied each loop pass      |
/// | `action`| string  | —       | See actions — dispatched via set_action      |
pub struct Looper {
    pub key:    String,
    pub active: bool,

    // --- State machine ---
    state:       LooperState,
    current_pos: usize,
    loop_len:    usize,   // 0 = not yet set

    // --- Buffer stack ---
    buffers:      Vec<Vec<Frame>>,  // [0] = base recording, [1..] = overdub layers
    merge_buf:    Vec<Frame>,       // always = buf[0] + decay * buf[1] (when overdub_count >= 1)
    overdub_count: usize,           // number of completed overdub layers (NOT counting base)
    max_buffers:  usize,            // max total layers incl. base; 0 = unlimited

    // --- Parameters ---
    pub wet:   f32,
    pub decay: f32,

    // --- Fading ---
    fade_gain:  f32,   // 0.0 = silent, 1.0 = full; applied to loop output
    fade_delta: f32,   // per-sample step: >0 = fade in, <0 = fade out, 0 = steady
    stopping:   bool,  // true while fading out before transitioning to Stop

    sample_rate: f32,
    #[allow(dead_code)]
    init_len:    usize,  // initial capacity of buffers[0]

    // Event bus: set via init_bus(), used to fire NodeEvent messages.
    event_bus: Option<EventBus>,
}

impl Looper {
    pub fn new(key: impl Into<String>, sample_rate: f32, max_seconds: f32, max_buffers: usize) -> Self {
        let init_len = (sample_rate * max_seconds) as usize;
        let sample_rate_stored = sample_rate;
        // Pre-allocate buffer[0] (OS gives us lazily-zeroed pages)
        let buf0 = vec![[0.0f32; 2]; init_len];
        Self {
            key:           key.into(),
            active:        true,
            state:         LooperState::Idle,
            current_pos:   0,
            loop_len:      0,
            buffers:       vec![buf0],
            merge_buf:     Vec::new(),
            overdub_count: 0,
            max_buffers,
            wet:           1.0,
            decay:         1.0,
            fade_gain:     1.0,
            fade_delta:    0.0,
            stopping:      false,
            sample_rate:   sample_rate_stored,
            init_len,
            event_bus:     None,
        }
    }

    /// Fire a NodeEvent on the bus (no-op if bus not set).
    fn fire_event(&self, event: &str, data: serde_json::Value) {
        tracing::debug!(key = %self.key, event, data = %data, "looper event");
        if let Some(bus) = &self.event_bus {
            bus.send(ControlMessage::NodeEvent {
                key:   self.key.clone(),
                event: event.to_string(),
                data,
            }).ok();
        }
    }

    /// Fire a `looper_state` event reflecting the current state.
    fn fire_state(&self) {
        let loop_ms = if self.sample_rate > 0.0 && self.loop_len > 0 {
            (self.loop_len as f32 / self.sample_rate * 1000.0) as u32
        } else { 0 };
        let pos_ms = if self.sample_rate > 0.0 {
            (self.current_pos as f32 / self.sample_rate * 1000.0) as u32
        } else { 0 };
        self.fire_event("looper_state", serde_json::json!({
            "state":         format!("{:?}", self.state),
            "loop_ms":       loop_ms,
            "pos_ms":        pos_ms,
            "overdub_count": self.overdub_count,
        }));
    }

    fn start_fade_in(&mut self) {
        self.fade_gain  = 0.0;
        self.fade_delta = LOOP_FADE_STEP;
    }

    fn start_fade_out(&mut self) {
        self.fade_gain  = 1.0;
        self.fade_delta = -LOOP_FADE_STEP;
        self.stopping   = true;
    }

    /// Advance fade_gain by one sample and return the gain to apply this sample.
    fn advance_fade(&mut self) -> f32 {
        let g = self.fade_gain;
        self.fade_gain = (self.fade_gain + self.fade_delta).clamp(0.0, 1.0);
        if self.fade_gain >= 1.0 || self.fade_gain <= 0.0 {
            self.fade_delta = 0.0;
        }
        g
    }

    /// Bake a fade-out into the last LOOP_FADE_SAMPLES of a recorded buffer.
    /// Mirrors the recording fade-in: last sample ends at 1/LOOP_FADE_SAMPLES,
    /// matching the fade-in level at position 0, minimising the loop boundary jump.
    fn bake_fade_out(&mut self, layer_idx: usize) {
        let len  = self.loop_len;
        let fade = LOOP_FADE_SAMPLES.min(len / 2);
        let buf  = &mut self.buffers[layer_idx];
        for k in 0..fade {
            let pos  = len - fade + k;
            let gain = (fade - k) as f32 * LOOP_FADE_STEP;
            if pos < buf.len() {
                buf[pos][0] *= gain;
                buf[pos][1] *= gain;
            }
        }
    }

    #[allow(dead_code)]
    pub fn state(&self) -> LooperState { self.state }

    // -----------------------------------------------------------------------
    // Primitive actions
    // -----------------------------------------------------------------------

    fn do_rec(&mut self) {
        match self.state {
            LooperState::Idle => {
                // First recording — re-use pre-allocated buffers[0]
                self.overdub_count = 0;
                self.loop_len      = 0;
                self.current_pos   = 0;
                self.state         = LooperState::Recording;
            },
            LooperState::Stop | LooperState::Recording => {
                // Start overdubbing from the beginning of the loop
                if self.loop_len == 0 {
                    self.loop_len = self.current_pos;
                }
                self.current_pos = 0;
                self.try_start_overdub();
                self.start_fade_in();
            },
            LooperState::Playing => {
                // Start overdubbing from current position
                self.try_start_overdub();
            },
            LooperState::Overdub => {
                // Commit current layer and start a new one.
                // If the stack is full, try_start_overdub merges the two oldest layers.
                self.finish_overdub_layer();
                self.try_start_overdub();
            }
        }
        if matches!(self.state, LooperState::Recording | LooperState::Overdub) {
            self.fire_state();
        }
    }

    fn do_play(&mut self) {
        match self.state {
            LooperState::Recording => {
                self.loop_len      = self.current_pos.max(1);
                self.bake_fade_out(0);  // bake fade into last 8 samples of recording
                self.overdub_count = 0; // base layer exists, no overdubs yet
                self.current_pos   = 0;
                self.state         = LooperState::Playing;
                self.init_merge_buf();
                // No output fade-in: buffer already near-silent at pos 0 (baked fade-in)
            },
            LooperState::Stop => {
                if self.loop_len > 0 {
                    self.state = LooperState::Playing;
                    self.start_fade_in();
                }
            },
            LooperState::Overdub => {
                self.finish_overdub_layer();
                self.state = LooperState::Playing;
                // No fade — playback was already running during overdub
            },
            LooperState::Idle | LooperState::Playing => {}
        }
        if self.state == LooperState::Playing {
            self.fire_state();
        }
    }

    /// Pause: stop playback/recording at the current position (position is preserved).
    fn do_pause(&mut self) {
        match self.state {
            LooperState::Recording => {
                self.loop_len = self.current_pos.max(1);
                self.bake_fade_out(0);
                self.init_merge_buf();
                self.state = LooperState::Stop;
            },
            LooperState::Playing | LooperState::Overdub => {
                // Fade out; state transitions to Stop when fade completes in process().
                // Notify the UI immediately so it shows Stop without waiting for the fade.
                self.start_fade_out();
            },
            LooperState::Idle | LooperState::Stop => {}
        }
        if matches!(self.state, LooperState::Stop) {
            self.fire_state();
        } else if self.stopping {
            self.fire_event("looper_state", serde_json::json!({
                "state":         "Stop",
                "loop_ms":       self.loop_ms(),
                "pos_ms":        self.pos_ms(),
                "overdub_count": self.overdub_count,
            }));
        }
    }

    /// Stop: halt playback/recording (if not already stopped) and reset position to 0.
    fn do_stop(&mut self) {
        match self.state {
            LooperState::Idle => return,
            LooperState::Stop => {
                self.current_pos = 0;
                self.fire_state();
            },
            LooperState::Recording => {
                self.loop_len    = self.current_pos.max(1);
                self.bake_fade_out(0);
                self.init_merge_buf();
                self.state       = LooperState::Stop;
                self.current_pos = 0;
                self.fire_state();
            },
            LooperState::Playing | LooperState::Overdub => {
                // Start fade-out; reset pos to 0 immediately (8-sample fade is inaudible).
                self.start_fade_out();
                self.current_pos = 0;
                self.fire_event("looper_state", serde_json::json!({
                    "state":         "Stop",
                    "loop_ms":       self.loop_ms(),
                    "pos_ms":        0u32,
                    "overdub_count": self.overdub_count,
                }));
            }
        }
    }

    fn do_reset(&mut self) {
        self.state         = LooperState::Idle;
        self.current_pos   = 0;
        self.loop_len      = 0;
        self.overdub_count = 0;
        self.stopping      = false;
        self.fade_gain     = 1.0;
        self.fade_delta    = 0.0;
        // Free all overdub buffers; keep buffers[0] but clear it
        self.buffers.truncate(1);
        if let Some(b) = self.buffers.first_mut() {
            b.fill([0.0; 2]);
        }
        self.merge_buf.clear();
        self.fire_state();
    }

    /// Undo: remove the most recent overdub layer.
    /// During Overdub: cancels the in-progress recording, transitions to Playing.
    /// During Playing/Stop: removes the last completed overdub layer.
    fn do_undo(&mut self) {
        match self.state {
            LooperState::Idle | LooperState::Recording => return,
            LooperState::Overdub => {
                // Cancel in-progress overdub: zero-fill the recording buffer, back to Playing.
                let rec = 1 + self.overdub_count;
                if rec < self.buffers.len() {
                    self.buffers[rec].fill([0.0f32; 2]);
                }
                self.stopping   = false;
                self.fade_gain  = 1.0;
                self.fade_delta = 0.0;
                self.state      = LooperState::Playing;
            },
            LooperState::Playing | LooperState::Stop => {
                if self.overdub_count > 0 {
                    // Zero-fill the last completed overdub layer and remove it.
                    if self.overdub_count < self.buffers.len() {
                        self.buffers[self.overdub_count].fill([0.0f32; 2]);
                    }
                    self.overdub_count -= 1;
                    self.init_merge_buf();
                }
                // If overdub_count == 0: nothing to undo; button should be disabled.
            }
        }
        self.fire_state();
    }

    // Small helpers to avoid repeating the ms conversion formula.
    fn loop_ms(&self) -> u32 {
        if self.sample_rate > 0.0 && self.loop_len > 0 {
            (self.loop_len as f32 / self.sample_rate * 1000.0) as u32
        } else { 0 }
    }
    fn pos_ms(&self) -> u32 {
        if self.sample_rate > 0.0 {
            (self.current_pos as f32 / self.sample_rate * 1000.0) as u32
        } else { 0 }
    }

    // -----------------------------------------------------------------------
    // Overdub helpers
    // -----------------------------------------------------------------------

    fn try_start_overdub(&mut self) {
        // New recording layer index = 1 + overdub_count (base at 0, overdubs at 1..)
        let need   = 1 + self.overdub_count;
        let at_max = self.max_buffers > 0 && need >= self.max_buffers;
        if at_max {
            self.do_merge(); // collapses oldest two layers; decrements overdub_count
        } else {
            self.try_alloc_layer(need);
            // Ensure merge_buf is sized so the per-sample maintenance in process() runs.
            // Necessary when entering overdub from Recording state (loop_len just set,
            // init_merge_buf not yet called on this path).
            self.init_merge_buf();
        }
        self.state = LooperState::Overdub;
    }

    /// Try to allocate buffers[idx] at loop_len size. Returns true on success.
    fn try_alloc_layer(&mut self, idx: usize) -> bool {
        if idx < self.buffers.len() {
            self.buffers[idx] = vec![[0.0f32; 2]; self.loop_len];
            return true;
        }
        self.buffers.push(vec![[0.0f32; 2]; self.loop_len]);
        true
    }

    fn finish_overdub_layer(&mut self) {
        // Bake fade-out into the layer just recorded, then promote it.
        self.bake_fade_out(1 + self.overdub_count);
        self.overdub_count += 1;
        self.init_merge_buf();
    }

    /// Merge buffers[0] + decay*buffers[1] into buffers[0], shift the remaining
    /// layers down, and prepare a fresh recording slot at buffers[1+overdub_count].
    /// Called when the layer stack is full. Decrements overdub_count because two
    /// layers are collapsed into one — the recording slot stays within bounds.
    fn do_merge(&mut self) {
        if self.overdub_count < 1 { return; }

        let total = 1 + self.overdub_count; // index of the needed recording slot

        // Ensure buffers[total] exists; needed as a temporary cell for the rotation.
        while self.buffers.len() <= total {
            self.buffers.push(vec![[0.0f32; 2]; self.loop_len]);
        }

        // Merge buffers[0] + decay*buffers[1] → buffers[0], using pre-computed merge_buf.
        if self.merge_buf.len() == self.loop_len {
            std::mem::swap(&mut self.merge_buf, &mut self.buffers[0]);
        } else {
            // merge_buf not ready (e.g. only one overdub layer): compute inline.
            for i in 0..self.loop_len.min(self.buffers[0].len()).min(self.buffers[1].len()) {
                let b0 = self.buffers[0][i];
                let b1 = self.buffers[1][i];
                self.buffers[0][i] = [b0[0] + b1[0], b0[1] + b1[1]];
            }
        }

        // Rotate buffers[1..=total] left: old overdub1 ends up at buffers[total].
        // buffers[total-1] (= new recording slot after overdub_count decrement) becomes
        // the zero-filled placeholder we pushed above.
        self.buffers[1..=total].rotate_left(1);

        // Zero-fill buffers[total] (was old overdub1's buffer, now unused after rotation).
        self.buffers[total].fill([0.0f32; 2]);

        // Two layers collapsed into one: decrement so recording slot stays in bounds.
        self.overdub_count -= 1;

        // Pre-compute merge_buf = new buf[0] + decay * new buf[1].
        // This keeps merge_buf valid even if the next merge happens immediately
        // (e.g. user presses Rec twice in quick succession with no audio samples in between).
        // The per-sample maintenance in the process loop will keep it updated thereafter.
        let n = self.loop_len.min(self.buffers[0].len()).min(self.buffers[1].len());
        let mut new_merge = vec![[0.0f32; 2]; self.loop_len];
        for i in 0..n {
            new_merge[i] = [
                self.buffers[0][i][0] + self.buffers[1][i][0],
                self.buffers[0][i][1] + self.buffers[1][i][1],
            ];
        }
        self.merge_buf = new_merge;
    }

    fn init_merge_buf(&mut self) {
        if self.loop_len > 0 && self.merge_buf.len() != self.loop_len {
            self.merge_buf = vec![[0.0f32; 2]; self.loop_len];
        }
    }

    // -----------------------------------------------------------------------
    // Playback helper
    // -----------------------------------------------------------------------

    /// Flat sum of all playback layers at position `pos`.
    fn playback_frame(&self, pos: usize, layer_count: usize) -> Frame {
        let mut out = [0.0f32; 2];
        for i in 0..layer_count {
            if pos < self.buffers[i].len() {
                let b = self.buffers[i][pos];
                out = [out[0] + b[0], out[1] + b[1]];
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // Combined action dispatch
    // -----------------------------------------------------------------------

    fn dispatch_combined(&mut self, action: &str) {
        // pause-stop-reset needs dynamic branching on current_pos, handled specially.
        if action == "pause-stop-reset" {
            match self.state {
                LooperState::Idle                                  => {}
                LooperState::Recording |
                LooperState::Playing   |
                LooperState::Overdub                               => self.do_pause(),
                LooperState::Stop if self.current_pos > 0          => self.do_stop(),
                LooperState::Stop                                  => self.do_reset(),
            }
            return;
        }

        let primitive = match (action, self.state) {
            // rec-play-stop-rec
            ("rec-play-stop-rec", LooperState::Idle)      => Some("rec"),
            ("rec-play-stop-rec", LooperState::Recording) => Some("play"),
            ("rec-play-stop-rec", LooperState::Overdub)   => Some("play"),
            ("rec-play-stop-rec", LooperState::Playing)   => Some("pause"),
            ("rec-play-stop-rec", LooperState::Stop)      => Some("rec"),

            // rec-play-stop-play
            ("rec-play-stop-play", LooperState::Idle)      => Some("rec"),
            ("rec-play-stop-play", LooperState::Recording) => Some("play"),
            ("rec-play-stop-play", LooperState::Overdub)   => Some("play"),
            ("rec-play-stop-play", LooperState::Playing)   => Some("pause"),
            ("rec-play-stop-play", LooperState::Stop)      => Some("play"),

            // rec-play-rec-rec
            ("rec-play-rec-rec", LooperState::Idle)      => Some("rec"),
            ("rec-play-rec-rec", LooperState::Recording) => Some("play"),
            ("rec-play-rec-rec", LooperState::Overdub)   => Some("play"),
            ("rec-play-rec-rec", LooperState::Playing)   => Some("rec"),
            ("rec-play-rec-rec", LooperState::Stop)      => Some("rec"),

            // rec-play-rec-play
            ("rec-play-rec-play", LooperState::Idle)      => Some("rec"),
            ("rec-play-rec-play", LooperState::Recording) => Some("play"),
            ("rec-play-rec-play", LooperState::Overdub)   => Some("play"),
            ("rec-play-rec-play", LooperState::Playing)   => Some("rec"),
            ("rec-play-rec-play", LooperState::Stop)      => Some("rec"),

            // play-stop (now pauses instead of stopping at pos)
            ("play-stop", LooperState::Playing) => Some("pause"),
            ("play-stop", LooperState::Stop)    => Some("play"),
            ("play-stop", _)                    => None,

            // rec-stop (now pauses instead of stopping at pos)
            ("rec-stop", LooperState::Idle)      => Some("rec"),
            ("rec-stop", LooperState::Recording) => Some("pause"),
            ("rec-stop", LooperState::Overdub)   => Some("pause"),
            ("rec-stop", LooperState::Playing)   => Some("rec"),
            ("rec-stop", LooperState::Stop)      => Some("rec"),

            // stop-reset (now pauses instead of stopping at pos)
            ("stop-reset", LooperState::Idle)      => None,
            ("stop-reset", LooperState::Recording) => Some("pause"),
            ("stop-reset", LooperState::Overdub)   => Some("pause"),
            ("stop-reset", LooperState::Playing)   => Some("pause"),
            ("stop-reset", LooperState::Stop)      => Some("reset"),

            // pause-stop: pause → then stop (goto pos 0)
            ("pause-stop", LooperState::Idle)      => None,
            ("pause-stop", LooperState::Recording) => Some("pause"),
            ("pause-stop", LooperState::Overdub)   => Some("pause"),
            ("pause-stop", LooperState::Playing)   => Some("pause"),
            ("pause-stop", LooperState::Stop)      => Some("stop"),

            _ => {
                warn!("Looper: unknown action '{action}'");
                None
            }
        };
        if let Some(p) = primitive {
            self.dispatch_primitive(p);
        }
    }

    fn dispatch_primitive(&mut self, action: &str) {
        match action {
            "rec"   => self.do_rec(),
            "play"  => self.do_play(),
            "pause" => self.do_pause(),
            "stop"  => self.do_stop(),
            "reset" => self.do_reset(),
            "undo"  => self.do_undo(),
            other   => warn!("Looper: unknown primitive action '{other}'"),
        }
    }
}

// ---------------------------------------------------------------------------
// Device impl
// ---------------------------------------------------------------------------

impl Device for Looper {
    fn key(&self)       -> &str  { &self.key }
    fn type_name(&self) -> &str  { "looper" }
    fn is_active(&self) -> bool  { self.active }

    fn to_params(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("active".into(),        self.active.into());
        m.insert("wet".into(),           self.wet.into());
        m.insert("decay".into(),         self.decay.into());
        m.insert("state".into(),         format!("{:?}", self.state).into());
        m.insert("overdub_count".into(), (self.overdub_count as f64).into());
        m.insert("max_buffers".into(),   (self.max_buffers as f64).into());
        let loop_secs = if self.sample_rate > 0.0 && self.loop_len > 0 {
            self.loop_len as f32 / self.sample_rate
        } else { 0.0 };
        m.insert("loop_secs".into(), (loop_secs as f64).into());
        let pos_secs = if self.sample_rate > 0.0 {
            self.current_pos as f32 / self.sample_rate
        } else { 0.0 };
        m.insert("pos_secs".into(), (pos_secs as f64).into());
        m
    }

    fn init_bus(&mut self, bus: &crate::control::EventBus) {
        self.event_bus = Some(bus.clone());
    }

    fn process(&mut self, _dry: &[Frame], eff: &mut [Frame]) {
        let n = eff.len();
        for i in 0..n {
            let prev_eff = eff[i];
            let inp = prev_eff;

            match self.state {
                LooperState::Idle | LooperState::Stop => {
                    // prev_eff passes through unchanged
                }

                LooperState::Recording => {
                    if self.current_pos < self.buffers[0].len() {
                        // Bake fade-in: first LOOP_FADE_SAMPLES get a gain ramp 1/8..8/8
                        let gain = if self.current_pos < LOOP_FADE_SAMPLES {
                            (self.current_pos + 1) as f32 * LOOP_FADE_STEP
                        } else {
                            1.0
                        };
                        self.buffers[0][self.current_pos] = [inp[0] * gain, inp[1] * gain];
                    }
                    self.current_pos += 1;
                    // prev_eff passes through during recording
                }

                LooperState::Playing => {
                    if self.loop_len > 0 {
                        let pos        = self.current_pos;
                        let total      = 1 + self.overdub_count;
                        let loop_out   = self.playback_frame(pos, total);
                        let gain       = self.advance_fade();
                        eff[i] = [
                            prev_eff[0] + loop_out[0] * self.wet * gain,
                            prev_eff[1] + loop_out[1] * self.wet * gain,
                        ];
                        if self.stopping && self.fade_gain <= 0.0 {
                            self.state    = LooperState::Stop;
                            self.stopping = false;
                        } else {
                            self.current_pos += 1;
                            if self.current_pos >= self.loop_len {
                                self.current_pos = 0;
                                self.fire_event("loop_wrap", serde_json::json!({
                                    "loop_ms": self.loop_ms()
                                }));
                            }
                        }
                    }
                }

                LooperState::Overdub => {
                    if self.loop_len > 0 {
                        let pos = self.current_pos;
                        let rec = 1 + self.overdub_count; // recording into buffers[rec]

                        // Apply decay in-place to all layers including the recording buffer.
                        // Do this BEFORE reading prev_rec so previous iterations of this
                        // overdub also decay each pass.
                        if self.decay < 1.0 {
                            for i in 0..self.buffers.len().min(rec + 1) {
                                if pos < self.buffers[i].len() {
                                    self.buffers[i][pos][0] *= self.decay;
                                    self.buffers[i][pos][1] *= self.decay;
                                }
                            }
                        }

                        // Read current overdub buffer BEFORE writing so we hear previous
                        // iterations of this recording session in the playback mix.
                        let prev_rec = if rec < self.buffers.len() && pos < self.buffers[rec].len() {
                            self.buffers[rec][pos]
                        } else {
                            [0.0; 2]
                        };

                        // Accumulate into current overdub buffer (additive across loop iterations).
                        // Fade-in ramp on first LOOP_FADE_SAMPLES to avoid a click at entry.
                        if rec < self.buffers.len() && pos < self.buffers[rec].len() {
                            let gain = if pos < LOOP_FADE_SAMPLES {
                                (pos + 1) as f32 * LOOP_FADE_STEP
                            } else {
                                1.0
                            };
                            self.buffers[rec][pos][0] += inp[0] * gain;
                            self.buffers[rec][pos][1] += inp[1] * gain;
                        }

                        // Always maintain merge_buf = buf[0] + buf[1].
                        // Must run even at overdub_count == 0 (first overdub): buf[1] is
                        // being accumulated and merge_buf needs to be ready for do_merge.
                        if pos < self.merge_buf.len()
                            && pos < self.buffers[0].len() && pos < self.buffers[1].len()
                        {
                            let b0 = self.buffers[0][pos];
                            let b1 = self.buffers[1][pos];
                            self.merge_buf[pos] = [b0[0] + b1[0], b0[1] + b1[1]];
                        }

                        // Playback: all completed layers + previous iterations of current overdub.
                        let total    = 1 + self.overdub_count;
                        let base_out = self.playback_frame(pos, total);
                        let loop_out = [base_out[0] + prev_rec[0], base_out[1] + prev_rec[1]];
                        let gain     = self.advance_fade();
                        eff[i] = [
                            prev_eff[0] + loop_out[0] * self.wet * gain,
                            prev_eff[1] + loop_out[1] * self.wet * gain,
                        ];

                        if self.stopping && self.fade_gain <= 0.0 {
                            // Promote partial overdub layer (silence at tail), then stop
                            self.finish_overdub_layer();
                            self.state    = LooperState::Stop;
                            self.stopping = false;
                            self.fire_state(); // update UI with final overdub_count
                        } else {
                            self.current_pos += 1;
                            if self.current_pos >= self.loop_len {
                                // at-end: keep accumulating into the same buffer.
                                // Layer is only committed when the user presses Rec again.
                                self.current_pos = 0;
                                self.fire_event("loop_wrap", serde_json::json!({
                                    "loop_ms": self.loop_ms()
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    fn reset(&mut self) {
        self.do_reset();
    }
}

// ---------------------------------------------------------------------------
// Parameterized impl
// ---------------------------------------------------------------------------

impl Parameterized for Looper {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        match param {
            "active"   => { self.active = value.try_bool()?; Ok(()) }
            "wet"      => { let (v, r) = check_bounds("Looper", "wet",   value.try_float()?, 0.0, 4.0); self.wet   = v; r }
            "decay"    => { let (v, r) = check_bounds("Looper", "decay", value.try_float()?, 0.0, 1.0); self.decay = v; r }
            "pos_secs" => {
                let secs     = value.try_float()?;
                let max_secs = if self.loop_len > 0 && self.sample_rate > 0.0 {
                    self.loop_len as f32 / self.sample_rate
                } else { 0.0 };
                self.current_pos = (secs.clamp(0.0, max_secs) * self.sample_rate) as usize;
                Ok(())
            },
            _ => Err(format!("Looper: unknown param '{param}'")),
        }
    }

    fn get_param(&self, param: &str) -> Option<f32> {
        match param {
            "active" => Some(if self.active { 1.0 } else { 0.0 }),
            "wet"    => Some(self.wet),
            "decay"  => Some(self.decay),
            _ => None,
        }
    }

    fn set_action(&mut self, param: &str, action: &str) -> Result<(), String> {
        if param != "action" {
            return Err(format!("Looper: unknown action param '{param}'"));
        }
        match action {
            "rec" | "play" | "pause" | "stop" | "reset" | "undo" => self.dispatch_primitive(action),
            _ => self.dispatch_combined(action),
        }
        Ok(())
    }

}
