# Preset change
• controller must be able to (up/down, by nr) — todo?

# MIDI mappings
Globale channel change propagate to all presets

# Validatie of all free input fields.
Todo

# implementation details looper
## Allocation/freeing
Allocation can be done at each iteration. After initial record, loop_len is known, so every new buffer can be initialized at this size. buffer[0] can be initialized at looper_init_buf_secs, system-setting in config.json, no ui needed. Defaults to 20 secs.

Freeing is a good question. I guess allocating and zeroing a buffer slot could be done without a drop-out? Then I would say free them on reset. If not, maybe a smarter way should be choosen.

## memory management
I'd like a better discission making for the buffer-logic. We will use a background-process that will get meminfo into two AtomicUsize's (total and free).
I'd like the logic to be: is there enough mem left (after this allocation) for yet another min_seconds_free seconds of audio, given the current samplerate and bit depth? If not, dont do an extra layer, but merge instead.
We might also have an extra cleanup: if free mem gets under a certain threshold AND layer count is above a certain threshold, we might want to merge multiple layers, or merge the bottom layers while playing: 
- in OVERDUB, we might merge nog 2 but 3 layers.
- in PLAY, we might merge the bottom 2 layers and swap on at-end.
These thresholds would be compile-time contst.
I guess this would get us a more mature memory managemend, right?

## Memory Indicators
If we'd ask the OS for mem usage, as a bonus, I can imagine we put a memory-free indicator in the ui. If we ask the OS about the mem, it might be so we also get the total mem? Then we could make a vu-meter-kind of indicator for mem wich depicts the percentage. This is apart from the looper, so can be in the header somewhere.

## Fading - later iterations
For later iterations we might fix this otherwise: pre-record/post-record. Post record is quite doable: let the recording continue for X ms, but don't count that for loop_len. On at-end, continue to add a fading part of that piece. Of course we'd have to watch out for CPU, because we'd have to do this for every layer. Or - better - merge this fade one time with the layer itself: just merge a couple of samples from the end directly to the start of that layer, making it continues on its own. That is actually a great idea, would require a one-time fix at the at-end event for just that layer.
Having a pre-record is more difficult. For OVERDUB this is doable: just record the input constantly, but set your pointer at the moment the user starts 'record'. That would only fail if near the start. But then we might fade in with the end-samples. Since the current record buffer is constantly dummy-recording, it would loop itself on at-end. The data would still be there in that same buffer. We would only have to maintain a flag that indicates whether this is the case or not.
Pre-record on entering RECORD phase, would be much more difficult: we would have to maintian a small ringbuffer, of fade_in_samples, and when the user hits record, copy those samples to the buffer, start recording after it, and have the start-pointer point to fade_in_samples. Of course, not starting on 0 anymore would complicate all logic a little bit.

## effects - not for this iteration.
Later I'd like effects such as reverse, fade, different playback speed (half/double/based on midi note-on).

## Multi-looper - not for this iteration.
UPDATE - I want multiple loopers in different chains to be able to synchronize! Means share loon_len, or event-driven.
I want to support multiple loopers like so:
• parallel: the loopers must share loop_len, which is set by the first looper recording. Each looper can be started/stopped separately. Playing recodring is sync, so current_pos is also shared.
• serial: the loopers play after each other. If a looper is playing, starting a looper will wait with playing until the currently playing is triggering 'at-end'. If a looper is in RECORD state, wait until that looper is done recording.

Audio-input must be the same for all loopers, all wet-outputs must ben added. In both modes.
This asks for a parallel-chain. This is a sub-container that can hold parallel-compatible effects (looper only for now). Or maybe better, this container is specialized for looper, so it can hold specific state, like shared loop_len/current_pos and so on. We might then create such a container automatically if loopers are added in sequense in the chain. In that case they don't even show up in the json, it is just a container under the hood. Or maybe just add them always. 
I think it makes sense that such a container then holds the mode, and acturally we should add an third mode:
• independant
• parallel
• serial

# Abstract param info
```rust
trait Device
    // ...
    fn param_info(&self) -> Vec<ParamInfo>;
    // ...
}
```

**`ParamInfo` struct (shared with built-in effects too — see below):**
```rust
struct ParamInfo {
    id: u32,
    name: String,
    min: f32,
    max: f32,
    default: f32,
    logarithmic: bool,
    stepped: bool,
    boolean: bool,
    unit: Option<String>,   // "Hz", "dB", "ms"
    group: Option<String>,  // for visual grouping
}
```
Looper stays custom — it has stateful transport logic (rec/play/pause/stop/overdub) that doesn't fit the ParamInfo model.

## Frontend / UI

**Generic param rendering:** `EffectTile` becomes fully data-driven. Backend sends `param_info` list via HTTP; frontend renders:
- Float → `Knob` (existing component), with log scale flag
- Boolean → `Toggle`
- Stepped/enum → `Select` or stepped knob
- Units shown as label under knob

**Looper tile stays custom** — transport buttons (rec/play/pause/stop) are actions, not scalar params. State-dependent (available actions change based on looper state). Rendered as a bespoke component, not via ParamInfo.

# Skinning
## css
Find some way to select a skin. Best I think is config.json. That would select a list of css'es. 
_variables.scss should be a startingpoint.
## Position and actual control
Most controls can be simply skinned. But the active switch might be a problem. 
For web, a toggle in the header is fine. But if you want to mimic a foot-padel, 
you'd want a footswitch-kind of look. Active indicater would then be a led, switch a stateless toggle.

# to test
Serial reconnect (the active_rx + select! lifecycle changes)
CTRL knob sweep over TCP/serial (now round-trips through master)
MIDI CC in + MIDI CC out (both completely rewired)
Preset switch (controller mappings now owned by master, no Arc sync)
Reload logic
14-bit MIDI CC out (CCs 0–31)
PRESET / STATE / INDICES / EVENT broadcasts reach UI correctly

# porting embedded
Choose RTIC or Embassy 
• RTIC deploys NVIC, effectively hardware task scheduling - better for this domain
• embassy maybe is simpler for USB, and such.

# acitve switch should fade
When hitting the foot switch, wet should fade out, then true bypass kicks in.
Fade time depends (maybe) on the type of effect.

# 2 distinct classes of effect
Some effects demand digital dry. These are the effects that do something with dry's
phase, like eq, phaser, flanger (?), exciter. Or they completely replace dry, like 
distortion, eq, tremolo, exciter.
Others can have analog dry. These are all effects that can (or must) have some 
lantency. These include chorus, reverb, delay, looper.
