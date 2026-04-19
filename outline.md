# Basic design
• main: cli/config, logging, bus, audio engine, master, http.
• AudioEngine: owns both rtrbs internally, exposes push_patch() and push_control()
• Master: owns config, snapshot, controller_map. All interfaces talk to it via master_tx with oneshot responses. Broadcasts changes on bus.
• Bus: outbound only — hardware controllers listen for deltas
• No watch channel — Devices gets full state via master_tx request, deltas via bus

# Chains/controllers

## Chains
### Where chains live

`AudioEngine.chains: Vec<Chain>` — that's the one and only owner of the live DSP chain. It lives on the audio thread.

### How a new patch gets in

1. ConfigMaster builds a new `Vec<Chain>` and calls `audio.push_patch(chains)`
2. On every audio block, `AudioEngine::install_pending_patch()` pops from internal `patch_rx`
3. It takes the **latest** one (drains the buffer, keeps only the last), calls `prepare()` on it, then:

```rust
self.chains = new_chains;
```

The old `Vec<Chain>` is simply **dropped** — Rust's ownership handles cleanup. The old effects, their delay buffers, looper memory — all freed automatically when the old vec goes out of scope. No explicit "clear" needed.

### How SET reaches audio

ConfigMaster calls `audio.push_control(msg)` which pushes into the internal `control_tx` ring buffer. The audio engine polls `control_rx` each block and calls `set_param` on the matching device. No chain rebuild — just a parameter tweak on the existing live objects.

### Summary: two channels, two purposes

| Channel | Type | Purpose |
|---|---|---|
| `control_rx` | `Consumer<ControlMessage>` | Parameter tweaks (SET, NoteOn, Reset) — cheap, no allocation |
| `patch_rx` | `Consumer<Vec<Chain>>` | Full chain swap (PATCH, preset switch) — new graph replaces old |

Both are lock-free `rtrb` ring buffers owned internally by AudioEngine. ConfigMaster uses `audio.push_control()` and `audio.push_patch()`. The audio thread never blocks. The old chain isn't "cleared" — it's replaced and dropped. That's the Rust way.

## Controllers

### 1. Build (startup, ConfigMaster)

- One `Arc<RwLock<ControllerDef>>` is created per device alias, initially empty (`ControllerDef::default()`)
- The active preset's controllers are written into the matching Arcs
- The `controller_map` is owned by ConfigMaster

### 2. Shared with device tasks

When ConfigMaster spawns a device task (serial/MIDI/network), it passes `Arc::clone(controller_map[alias])` to that task. So the device task and the master both point to the **same** `ControllerDef`.

- **Device task reads** it on every incoming message (to translate CTRL → SET)
- **ConfigMaster writes** it on preset switch (`clear_controllers` + `apply_controllers`)

### 3. Preset switch flow

```
clear_controllers()   → every Arc gets ControllerDef::default() (empty mappings)
apply_controllers()   → for each ControllerDef in the new preset,
                         write it into the matching Arc
```

The device tasks don't restart — they just see new mappings next time they read the Arc.

### 4. Where they're persisted

In `cfg.presets.items[n].controllers: Vec<ControllerDef>` — saved to disk as part of the preset. The Arcs are runtime-only shared state.

### In short

```
Config (disk)                Runtime (shared)              Device tasks
PresetDef.controllers  →  Arc<RwLock<ControllerDef>>  ←  read on every message
                          ↑ written by ConfigMaster
                            on preset switch
```

## Control devices
### Changes
Looking at the current code, device changes *are* supported — `handle_put_device`, `handle_delete_device`, `handle_rename_device` all exist. But they don't recreate the Arc. They work differently:

- **Put (new/update)**: If the device becomes active, a new Arc is inserted into `controller_map` and a new task is spawned with it
- **Delete**: The `watch::Sender` sends `false` to shut down the task, device removed from config
- **Rename**: Config-level rename, no task restart

So the Arcs are **not** recreated on reload or device changes. They're created once at startup (or when a new device is added via `handle_put_device`), and the device task keeps its reference forever. Only the *contents* get swapped on preset switch.

For property changes on an existing device (like changing baud rate or port) — that's actually a gap right now. The task would need to be stopped and respawned to pick up new connection settings. The `device_active` watch channel gives you the mechanism (send `false`, then respawn), but I don't think `handle_put_device` does that for an already-active device whose properties changed. It only handles the active→inactive and inactive→active transitions.

@todo look at this

### init the controls
`control.rs` makes sense — it already owns `ControlMessage` and `EventBus`. It's the natural home for the controller runtime setup.

ConfigMaster builds the controller_map internally (via `build_controller_map()` in `control.rs`). On preset switch, `clear_controllers` + `apply_controllers` update the Arcs in place.

@todo implement


### Preset change
• controller must be able to (up/down, by nr) — todo?

## MIDI mappings
Globale channel change propagate to all presets

## Validatie of all free input fields.
Todo

### implementation details looper
#### Allocation/freeing
Allocation can be done at each iteration. After initial record, loop_len is known, so every new buffer can be initialized at this size. buffer[0] can be initialized at looper_init_buf_secs, system-setting in config.json, no ui needed. Defaults to 20 secs.

Freeing is a good question. I guess allocating and zeroing a buffer slot could be done without a drop-out? Then I would say free them on reset. If not, maybe a smarter way should be choosen.

#### memory management
I'd like a better discission making for the buffer-logic. We will use a background-process that will get meminfo into two AtomicUsize's (total and free).
I'd like the logic to be: is there enough mem left (after this allocation) for yet another min_seconds_free seconds of audio, given the current samplerate and bit depth? If not, dont do an extra layer, but merge instead.
We might also have an extra cleanup: if free mem gets under a certain threshold AND layer count is above a certain threshold, we might want to merge multiple layers, or merge the bottom layers while playing: 
- in OVERDUB, we might merge nog 2 but 3 layers.
- in PLAY, we might merge the bottom 2 layers and swap on at-end.
These thresholds would be compile-time contst.
I guess this would get us a more mature memory managemend, right?

#### Memory Indicators
If we'd ask the OS for mem usage, as a bonus, I can imagine we put a memory-free indicator in the ui. If we ask the OS about the mem, it might be so we also get the total mem? Then we could make a vu-meter-kind of indicator for mem wich depicts the percentage. This is apart from the looper, so can be in the header somewhere.

#### Fading - later iterations
For later iterations we might fix this otherwise: pre-record/post-record. Post record is quite doable: let the recording continue for X ms, but don't count that for loop_len. On at-end, continue to add a fading part of that piece. Of course we'd have to watch out for CPU, because we'd have to do this for every layer. Or - better - merge this fade one time with the layer itself: just merge a couple of samples from the end directly to the start of that layer, making it continues on its own. That is actually a great idea, would require a one-time fix at the at-end event for just that layer.
Having a pre-record is more difficult. For OVERDUB this is doable: just record the input constantly, but set your pointer at the moment the user starts 'record'. That would only fail if near the start. But then we might fade in with the end-samples. Since the current record buffer is constantly dummy-recording, it would loop itself on at-end. The data would still be there in that same buffer. We would only have to maintain a flag that indicates whether this is the case or not.
Pre-record on entering RECORD phase, would be much more difficult: we would have to maintian a small ringbuffer, of fade_in_samples, and when the user hits record, copy those samples to the buffer, start recording after it, and have the start-pointer point to fade_in_samples. Of course, not starting on 0 anymore would complicate all logic a little bit.

#### effects - not for this iteration.
Later I'd like effects such as reverse, fade, different playback speed (half/double/based on midi note-on).

#### Multi-looper - not for this iteration.
I want to support multiple loopers like so:
• parallel: the loopers must share loop_len, which is set by the first looper recording. Each looper can be started/stopped separately. Playing recodring is sync, so current_pos is also shared.
• serial: the loopers play after each other. If a looper is playing, starting a looper will wait with playing until the currently playing is triggering 'at-end'. If a looper is in RECORD state, wait until that looper is done recording.

Audio-input must be the same for all loopers, all wet-outputs must ben added. In both modes.
This asks for a parallel-chain. This is a sub-container that can hold parallel-compatible effects (looper only for now). Or maybe better, this container is specialized for looper, so it can hold specific state, like shared loop_len/current_pos and so on. We might then create such a container automatically if loopers are added in sequense in the chain. In that case they don't even show up in the json, it is just a container under the hood. Or maybe just add them always. 
I think it makes sense that such a container then holds the mode, and acturally we should add an third mode:
• independant
• parallel
• serial

### LV2 + CLAP plugin hosting

#### Background
VST/LV2/CLAP plugins are shared dynamic libraries (`.so`/`.dylib`). The host loads them in-process and calls exported C functions directly — no IPC. Each format has a similar core: activate, process, set/get param.

**Why LV2 + CLAP:**
- LV2: Linux/ARM native, huge open source ecosystem (`apt install lsp-plugins-lv2`), almost all free. Primary format for Raspberry Pi.
- CLAP: newest open C API, excellent Rust support (`clack` crate), growing Mac/Linux ecosystem with Apple Silicon binaries. Used for Mac development/testing.
- VST2/3 avoided: legal issues (VST2), C++ COM ABI complexity (VST3), mostly x86 binaries.

#### Architecture

**New `ChainNode` variant:**
```rust
ChainNode::Plugin(Box<dyn AudioPlugin>)
```

**Abstraction trait (hides format differences):**
```rust
trait AudioPlugin {
    fn activate(&mut self, sample_rate: f32, max_block_size: usize);
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]);
    fn param_info(&self) -> Vec<ParamInfo>;
    fn set_param(&mut self, id: u32, value: f32);
    fn get_param(&self, id: u32) -> f32;
}
```

`Lv2Plugin` and `ClapPlugin` each implement this. `Chain` doesn't know which format.

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

#### Refactor built-in effects to use ParamInfo
Add `param_info() -> Vec<ParamInfo>` to the `Device` trait. Built-in effects implement it. This allows the frontend to use one generic rendering path for both built-in effects and plugins.

#### Plugin config in JSON (patch)
```json
{ "key": "01-compressor", "type": "plugin", "format": "lv2", "uri": "http://lsp-plug.in/plugins/lv2/comp_mono", "params": { "0": 0.5 } }
{ "key": "02-reverb",     "type": "plugin", "format": "clap", "path": "/path/to/plugin.clap", "id": "com.example.reverb", "params": {} }
```

#### Built-in effects strategy
Keep existing built-in effects (reverb, delay, chorus, EQ) as a "batteries included" baseline — users can get started without any plugin dependencies. Plugins are an optional upgrade path. May remove weakest built-ins in a later iteration once plugin hosting is mature.

Looper stays custom — it has stateful transport logic (rec/play/pause/stop/overdub) that doesn't fit the ParamInfo model.

#### Frontend / UI

**Generic param rendering:** `EffectTile` becomes fully data-driven. Backend sends `param_info` list via HTTP; frontend renders:
- Float → `Knob` (existing component), with log scale flag
- Boolean → `Toggle`
- Stepped/enum → `Select` or stepped knob
- Units shown as label under knob

**Looper tile stays custom** — transport buttons (rec/play/pause/stop) are actions, not scalar params. State-dependent (available actions change based on looper state). Rendered as a bespoke component, not via ParamInfo.

**No generic "Action" system needed** — transport controls are Looper-only, keep them hardcoded in the Looper tile.

#### Implementation order
1. Add `param_info()` to `Device` trait; implement for all built-in effects
2. Refactor frontend `EffectTile` to render from dynamic param list
3. Add `AudioPlugin` trait + `ParamInfo` struct
4. Implement `ClapPlugin` (CLAP hosting via `clack` crate)
5. Add `Plugin` variant to `ChainNode`, wire into `Chain::process`
6. Add plugin loader to HTTP API (scan, instantiate, expose param_info)
7. Frontend: render plugin tiles same as built-in effect tiles
8. Implement `Lv2Plugin` (LV2 hosting via `lv2` crate or direct `lilv` bindings)

#### Rust crates to investigate
- `clack` — CLAP hosting in Rust (most mature)
- `lv2` crate or `lilv-sys` — LV2 host bindings
- Consider `lilv` (C library) via FFI for LV2 metadata/discovery

#### Select of plugin
Still to discuss with Paul: how is the plugin-select populated and how are we going to group them practically? Devide into source: 'native', 'clap' and 'lv2'. Perhaps first select source, than the plugin? Or, if that would render to a very long list, maybe even have one extra selectbox to then select the type of plugin?


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