use crate::control::{ControlMessage, EventBus};
use crate::engine::device::{find_param_info, find_param_meta, validate_canonical,
    EventAction, MetaAspect, ParamInfo, Device, Frame, Parameterized, ParamValue};

const LOOP_FADE_SAMPLES: usize = 8;
const LOOP_FADE_STEP: f32 = 1.0 / LOOP_FADE_SAMPLES as f32;

pub const NAME: &str = "looper";

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
/// - `buffers[0]` is pre-allocated at `init_len` samples (from `max_seconds`).
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
    key:    String,
    active: bool,

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
    wet:   f32,
    decay: f32,

    // --- Fading ---
    fade_gain:  f32,   // 0.0 = silent, 1.0 = full; applied to loop output
    fade_delta: f32,   // per-sample step: >0 = fade in, <0 = fade out, 0 = steady
    stopping:   bool,  // true while fading out before transitioning to Stop

    sample_rate: f32,
    #[allow(dead_code)]
    init_len:    usize, // initial capacity of buffers[0]

    // Event bus: set via init_bus(), used to fire NodeEvent messages.
    event_bus: Option<EventBus>,
}

pub static CANONICAL: [ParamInfo; 24] = [
    ParamInfo::new_discrete_bool("active", true, None),
    ParamInfo::new_continuous_float("wet",        0.0,   1.0,  0.5, false, None),
    ParamInfo::new_continuous_float("decay",      0.0,   1.0,  1.0, false, None),

    // ── Transport grouping anchor ─────────────────────────────────────────
    // UI-only marker for the cluster of Event buttons below. `visible`
    // controls whether the cluster shows on the tile by default; `active`
    // controls whether the group exists at all (declared-but-off can be
    // flipped on via Type-override). Hidden by default — most users drive
    // the looper from a foot controller and don't want the on-screen
    // transport row taking space until they ask for it.
    ParamInfo::new_actions_group("transport").with_hidden(),

    // ── Live read-only buffer counter + its cap ───────────────────────────
    // The read-only ParamMeta's `max` IS the cap (initial 4, configurable up
    // to 16 via meta-override on `max`). BoundMeta declares the envelope.
    // Consumed by the Undo button's badge in the tile.
    ParamInfo::new_continuous_int("buffer_cnt",   0,    4,   0,    None)
        .with_read_only(),
    ParamInfo::new_continuous_int("buffer_cnt",   0,   16,   0,    None)
        .with_kind_bound_meta(MetaAspect::Max),

    // ── Live RW playhead position ─────────────────────────────────────────
    // Reserved for the eventual scrubber widget. Inactive for now — proper
    // scrubber design pending.
    ParamInfo::new_continuous_float("pos_secs",   0.0, 300.0, 0.0, false, Some("s"))
        .with_inactive(),

    // ── Transport cells (canonical order = render order) ──────────────────
    // The tile iterates active+visible cells and splits them into two equal
    // rows. Order chosen so the default layout reads:
    //   row 1: Rec, Play, Pause, Stop
    //   row 2: Timer, Undo, Reset
    // The `duration` RO ParamMeta is the timer display (effect-published live
    // loop length). Combined verbs are declared but inactive — per-instance
    // override flips `active` on for users who want them; the effect's state
    // machine resolves them all.
    ParamInfo::new_event(EventAction::Rec).with_inactive(),
    ParamInfo::new_event(EventAction::RecPlay),
    ParamInfo::new_event(EventAction::RecPlayStop),
    // 4thd verb is where it loops to, so loops to Stop/Play
    ParamInfo::new_event(EventAction::RecPlayStopPlay), 
    ParamInfo::new_event(EventAction::Play).with_inactive(),
    ParamInfo::new_event(EventAction::PlayStop),
    ParamInfo::new_event(EventAction::PlayPause),
    ParamInfo::new_event(EventAction::RecPause),
    ParamInfo::new_event(EventAction::StopReset),
    ParamInfo::new_event(EventAction::PauseStop),
    ParamInfo::new_event(EventAction::PauseStopReset),
    ParamInfo::new_event(EventAction::Pause),
    ParamInfo::new_event(EventAction::Stop),
    ParamInfo::new_event(EventAction::Undo),
    ParamInfo::new_event(EventAction::Reset),
    // ── Live read-only duration + its cap ─────────────────────────────────
    // The read-only ParamMeta's `max` IS the cap (initial 30 s, configurable
    // up to 300 s). Non-growable: widening past construction-time max
    // requires reload (sizes the underlying buffer). BoundMeta on `max`
    // declares the override envelope.
    ParamInfo::new_continuous_float("duration",   0.0,  30.0, 0.0, false, Some("s"))
        .with_read_only().with_non_growable(),
    ParamInfo::new_continuous_float("duration",   1.0, 300.0, 0.0, false, Some("s"))
        .with_kind_bound_meta(MetaAspect::Max),
];
const _: () = validate_canonical(&CANONICAL);

pub const REGISTRATION: crate::effects::registry::EffectRegistration =
    crate::effects::registry::EffectRegistration {
        name:      NAME,
        canonical: &CANONICAL,
        factory:   |key, sr, info| Box::new(Looper::new(key, sr, info)),
    };

impl Looper {
    pub fn new(key: impl Into<String>, sample_rate: f32, params_info: &[ParamInfo]) -> Self {
        let active      = find_param_info(params_info, "active"    ).bool_default();
        let decay       = find_param_info(params_info, "decay"     ).continuous_float_default();
        let wet         = find_param_info(params_info, "wet"       ).continuous_float_default();
        // The cap for each live read-only counter is the entry's `max` —
        // configurable via meta-override (`SET <key>.<param>.max <n>`),
        // bounded by the BoundMeta envelope declared alongside it.
        // `find_param_meta` disambiguates against the BoundMeta sibling.
        let max_seconds = find_param_meta(params_info, "duration"  ).continuous_float_max();
        let max_buffers = find_param_meta(params_info, "buffer_cnt").continuous_int_max() as usize;

        let init_len = (sample_rate * max_seconds) as usize;
        // Pre-allocate buffer[0] (OS gives us lazily-zeroed pages)
        let buf0 = vec![[0.0f32; 2]; init_len];
        Self {
            key: key.into(),
            state:         LooperState::Idle,
            current_pos:   0,
            loop_len:      0,
            buffers:       vec![buf0],
            merge_buf:     Vec::new(),
            overdub_count: 0,
            active,
            decay,

            fade_gain:     1.0,
            fade_delta:    0.0,
            stopping:      false,

            max_buffers,
            wet, 
            sample_rate,
            init_len,
            event_bus:     None,
        }
    }

    /// Fire a NodeEvent on the bus (no-op if bus not set). Used for the
    /// abstract state tag (`looper-recording`, …); live values go through
    /// `fire_live` as `LiveParam` instead.
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

    /// Publish a live read-only ParamMeta value via `LiveParam`. Wire-shaped
    /// identical to a regular PARAM broadcast — the UI updates `node.<param>`
    /// the same way it does for SetParam.
    fn fire_live(&self, param: &str, value: ParamValue) {
        if let Some(bus) = &self.event_bus {
            bus.send(ControlMessage::LiveParam {
                path:  format!("{}.{param}", self.key),
                value,
            }).ok();
        }
    }

    /// Snapshot of state-derived values to publish. Loop length grows during
    /// recording; pos cycles during playback; buffer_cnt steps on
    /// overdub/merge.
    fn fire_state(&self) {
        self.fire_state_as(self.state, self.current_pos);
    }

    /// Same as `fire_state` but with a state-override and pos override — used
    /// by `do_pause` / `do_stop` to update the UI immediately during the short
    /// audio fade-out before the actual state transition completes.
    ///
    /// Publishes:
    ///   - State tag as a NodeEvent (`looper-recording`, etc.) — picked up by
    ///     any subscriber that wants to react to abstract state.
    ///   - Live values (`duration`, `pos_secs`, `buffer_cnt`) as `LiveParam`s
    ///     mapping onto the read-only ParamMeta entries of the same name.
    fn fire_state_as(&self, state: LooperState, pos: usize) {
        let duration = if self.sample_rate > 0.0 && self.loop_len > 0 {
            self.loop_len as f32 / self.sample_rate
        } else { 0.0 };
        let pos_secs = if self.sample_rate > 0.0 {
            pos as f32 / self.sample_rate
        } else { 0.0 };
        // `buffer_cnt` counts base + completed overdubs (1 = just base, no overdubs).
        let buffer_cnt = if matches!(state, LooperState::Idle) { 0 }
                         else { (1 + self.overdub_count) as i32 };

        // State tag — kebab-case, ambient broadcast for any subscriber.
        let tag = match state {
            LooperState::Idle      => "looper-idle",
            LooperState::Recording => "looper-recording",
            LooperState::Playing   => "looper-playing",
            LooperState::Overdub   => "looper-overdub",
            LooperState::Stop      => "looper-stop",
        };
        self.fire_event("state", serde_json::json!({ "tag": tag }));

        // Live read-only values.
        self.fire_live("duration",   ParamValue::Float(duration));
        self.fire_live("pos_secs",   ParamValue::Float(pos_secs));
        self.fire_live("buffer_cnt", ParamValue::Int(buffer_cnt));
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
            self.fire_state_as(LooperState::Stop, self.current_pos);
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
                self.fire_state_as(LooperState::Stop, 0);
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

    /// Resolve a combined transport verb against the current state, then
    /// dispatch the appropriate primitive. Combined verbs encode a small
    /// state-machine "what should this button do given current state".
    fn dispatch_action(&mut self, action: EventAction) {
        // PauseStopReset is the one verb whose resolution depends on
        // `current_pos` as well as state — special-case.
        if matches!(action, EventAction::PauseStopReset) {
            match self.state {
                LooperState::Idle                                                    => {},
                LooperState::Recording | LooperState::Playing | LooperState::Overdub => self.do_pause(),
                LooperState::Stop if self.current_pos > 0                            => self.do_stop(),
                LooperState::Stop                                                    => self.do_reset(),
            }
            return;
        }

        // Resolve combined → primitive (or None = no-op). Primitives map
        // directly to do_* methods at the bottom.
        let primitive: Option<EventAction> = match (action, self.state) {
            // Primitives — pass through unchanged.
            (EventAction::Rec,    _) => Some(EventAction::Rec),
            (EventAction::Play,   _) => Some(EventAction::Play),
            (EventAction::Pause,  _) => Some(EventAction::Pause),
            (EventAction::Stop,   _) => Some(EventAction::Stop),
            (EventAction::Reset,  _) => Some(EventAction::Reset),
            (EventAction::Undo,   _) => Some(EventAction::Undo),

            // RecPlay — 2-letter Rec ↔ Play cycle. From Playing fires Rec
            // (starts overdub at current pos); Overdub fires Play (commits
            // and goes back to Playing). Stop wraps to Rec.
            (EventAction::RecPlay, LooperState::Idle)      => Some(EventAction::Rec),
            (EventAction::RecPlay, LooperState::Recording) => Some(EventAction::Play),
            (EventAction::RecPlay, LooperState::Overdub)   => Some(EventAction::Play),
            (EventAction::RecPlay, LooperState::Playing)   => Some(EventAction::Rec),
            (EventAction::RecPlay, LooperState::Stop)      => Some(EventAction::Rec),

            // RecPlayStop — 3-letter cycle, Stop wraps back to Rec.
            (EventAction::RecPlayStop, LooperState::Idle)      => Some(EventAction::Rec),
            (EventAction::RecPlayStop, LooperState::Recording) => Some(EventAction::Play),
            (EventAction::RecPlayStop, LooperState::Overdub)   => Some(EventAction::Play),
            (EventAction::RecPlayStop, LooperState::Playing)   => Some(EventAction::Stop),
            (EventAction::RecPlayStop, LooperState::Stop)      => Some(EventAction::Rec),

            // RecPlayStopPlay — distinct 4-letter: Stop → Play (resume),
            // not Rec (start over).
            (EventAction::RecPlayStopPlay, LooperState::Idle)      => Some(EventAction::Rec),
            (EventAction::RecPlayStopPlay, LooperState::Recording) => Some(EventAction::Play),
            (EventAction::RecPlayStopPlay, LooperState::Overdub)   => Some(EventAction::Play),
            (EventAction::RecPlayStopPlay, LooperState::Playing)   => Some(EventAction::Stop),
            (EventAction::RecPlayStopPlay, LooperState::Stop)      => Some(EventAction::Play),

            // PlayStop — true stop (resets playhead to 0).
            (EventAction::PlayStop, LooperState::Playing) => Some(EventAction::Stop),
            (EventAction::PlayStop, LooperState::Stop)    => Some(EventAction::Play),
            (EventAction::PlayStop, _)                    => None,

            // PlayPause — soft toggle (keeps playhead).
            (EventAction::PlayPause, LooperState::Playing) => Some(EventAction::Pause),
            (EventAction::PlayPause, LooperState::Stop)    => Some(EventAction::Play),
            (EventAction::PlayPause, _)                    => None,

            // RecPause — Rec when at-rest, Pause when actively recording/playing
            // back. Press oscillates Rec/Pause once the loop is established.
            (EventAction::RecPause, LooperState::Idle)      => Some(EventAction::Rec),
            (EventAction::RecPause, LooperState::Recording) => Some(EventAction::Pause),
            (EventAction::RecPause, LooperState::Overdub)   => Some(EventAction::Pause),
            (EventAction::RecPause, LooperState::Playing)   => Some(EventAction::Rec),
            (EventAction::RecPause, LooperState::Stop)      => Some(EventAction::Rec),

            // StopReset — true Stop while active (resets playhead), Reset
            // from Stop. From Idle, Reset is harmless and serves as a hint
            // ("nothing to stop yet").
            (EventAction::StopReset, LooperState::Idle)      => Some(EventAction::Reset),
            (EventAction::StopReset, LooperState::Recording) => Some(EventAction::Stop),
            (EventAction::StopReset, LooperState::Overdub)   => Some(EventAction::Stop),
            (EventAction::StopReset, LooperState::Playing)   => Some(EventAction::Stop),
            (EventAction::StopReset, LooperState::Stop)      => Some(EventAction::Reset),

            // PauseStop
            (EventAction::PauseStop, LooperState::Idle)      => None,
            (EventAction::PauseStop, LooperState::Recording) => Some(EventAction::Pause),
            (EventAction::PauseStop, LooperState::Overdub)   => Some(EventAction::Pause),
            (EventAction::PauseStop, LooperState::Playing)   => Some(EventAction::Pause),
            (EventAction::PauseStop, LooperState::Stop)      => Some(EventAction::Stop),

            // Handled above; unreachable.
            (EventAction::PauseStopReset, _) => unreachable!(),
        };

        match primitive {
            Some(EventAction::Rec)   => self.do_rec(),
            Some(EventAction::Play)  => self.do_play(),
            Some(EventAction::Pause) => self.do_pause(),
            Some(EventAction::Stop)  => self.do_stop(),
            Some(EventAction::Reset) => self.do_reset(),
            Some(EventAction::Undo)  => self.do_undo(),
            _ => {},
        }
    }
}

// ---------------------------------------------------------------------------
// Device impl
// ---------------------------------------------------------------------------

impl Device for Looper {
    fn key(&self)       -> &str  { &self.key }
    fn type_name(&self) -> &str  { NAME }
    fn is_active(&self) -> bool  { self.active }

    fn init_bus(&mut self, bus: &crate::control::EventBus) {
        self.event_bus = Some(bus.clone());
    }

    fn republish_state(&self) {
        // Re-fire current state for any newly-joined subscribers. Existing
        // subscribers receive the same values as a no-op refresh.
        self.fire_state();
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
                        self.current_pos += 1;
                    } else {
                        // Buffer full — we've hit the configured `duration`
                        // cap. Auto-stop recording (same shape as do_stop's
                        // Recording branch). User can press Play to start
                        // looping the maxed-out recording.
                        self.loop_len    = self.buffers[0].len();
                        self.bake_fade_out(0);
                        self.init_merge_buf();
                        self.state       = LooperState::Stop;
                        self.current_pos = 0;
                        self.fire_state();
                    }
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
                                self.fire_event("loop_wrap", serde_json::json!({}));
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
                                self.fire_event("loop_wrap", serde_json::json!({}));
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
    fn set_action(&mut self, param: &str, action: EventAction) -> Result<(), String> {
        // With one Event entry per action, `param` == `action.name()`.
        // Cross-check so a malformed wire SET-with-action-mismatch errors
        // visibly instead of silently dispatching.
        if param != action.name() {
            return Err(format!("{}: action param '{param}' != verb '{}'", NAME, action.name()));
        }
        self.dispatch_action(action);
        Ok(())
    }

}

// ---------------------------------------------------------------------------
// Parameterized impl
// ---------------------------------------------------------------------------

impl Parameterized for Looper {
    fn set_param(&mut self, param: &str, value: ParamValue) -> Result<(), String> {
        // Master clamps to declared bounds and normalises variant before push;
        // audio just stores. See `ConfigMaster::clamp_to_bounds`.
        match param {
            "active" => { self.active = value.try_bool()?;  Ok(()) },
            "decay"  => { self.decay  = value.try_float()?; Ok(()) },
            "wet"    => { self.wet    = value.try_float()?; Ok(()) },
            // Maybe this isn't right. Maybe should be action play <secs>
            "pos_secs" => {
                let secs     = value.try_float()?;
                let max_secs = if self.loop_len > 0 && self.sample_rate > 0.0 {
                    self.loop_len as f32 / self.sample_rate
                } else { 0.0 };
                self.current_pos = (secs.clamp(0.0, max_secs) * self.sample_rate) as usize;
                Ok(())
            },
            _ => Err(format!("{}: unknown param '{param}'", NAME)),
        }
    }
}
