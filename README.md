# multi-effect

![Status: WIP](https://img.shields.io/badge/status-WIP-orange)

Real-time audio effect daemon for CPAL.

Signal path: ADC → Rust engine → DAC. The dry signal is mixed in analogue — the engine outputs wet signal only.

---

## Running

```bash
cargo run                          # loads ./config.json
cargo run -- -c my-config.json     # specific config
cargo run -- -v                    # verbose logging
cargo run -- -f                    # fresh start: ignore saved snapshot state
```

### Command-line flags

| Flag | Description |
|---|---|
| `-c <path>` / `--config <path>` | Config file (default: `config.json`) |
| `-v` / `--verbose` | Enable debug-level logging |
| `-f` / `--fresh` | Skip restoring snapshot state (start clean) |

Log level can be controlled at runtime via the `RUST_LOG` env var (e.g. `RUST_LOG=debug`).

Audio device, sample rate, buffer size, etc. are configured in `config.json` — not via CLI flags.

---

## Configuration — config.json

```json
{
  "sample_rate":         48000,
  "buffer_size":         256,
  "audio_device":        "default",
  "in_channels":         1,
  "out_channels":        2,
  "delay_max_seconds":   2.0,
  "looper_max_seconds":  30.0,
  "looper_max_buffers":  8,
  "http_port":           8080,
  "log_target":          "stderr",
  "state_save_path":     "/tmp/multi-effect-state.json",
  "state_save_interval": 300,

  "control_devices": { ... },
  "presets":         { ... },
  "chains":          [ ... ]
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `sample_rate` | u32 | 48000 | Audio sample rate in Hz |
| `buffer_size` | u32 | 256 | Frames per CPAL callback |
| `audio_device` | string | `"default"` | CPAL device name (or `"default"`) |
| `in_channels` | u16 | 2 | Physical input channels |
| `out_channels` | u16 | 2 | Physical output channels |
| `delay_max_seconds` | f32 | 2.0 | Delay buffer size at startup |
| `looper_max_seconds` | f32 | 30.0 | Looper buffer[0] size at startup |
| `looper_max_buffers` | usize | 8 | Max overdub layers |
| `http_port` | u16 | 8080 | HTTP/WebSocket port (0 = disabled) |
| `log_target` | string | `"stderr"` | `"stderr"` or `"syslog"` |
| `state_save_path` | path | `/tmp/multi-effect-state.json` | Where snapshot state is persisted |
| `state_save_interval` | u64 | 300 | Seconds between auto-saves (0 = disabled) |
| `control_devices` | map | `{}` | Control device aliases → connection config |
| `presets` | object | empty | Numbered preset slots |
| `chains` | array | `[]` | Startup chains (used when no preset is active) |

---

## Patch format — chains and nodes

A **chain** is a signal-flow unit with input/output routing and a sequence of nodes. Multiple chains run in parallel; their outputs sum.

```json
{
  "key":    "01-main",
  "input":  [1, 1],
  "output": [1, 2],
  "nodes": [
    { "key": "04-delay",  "type": "delay",  "time": 0.5, "feedback": 0.4, "wet": 0.5 },
    { "key": "06-mix",    "type": "mix",    "dry": 0.0, "wet": 1.0 }
  ]
}
```

### Channel routing
`input` / `output` are **1-based** physical channel numbers. A single integer expands to `[n, n]`; a pair `[L, R]` selects two channels. `0` means silent / no output.

```json
"input":  [1, 1]    // mono mic on ch 1 → both stereo halves
"input":  [1, 2]    // stereo
"output": [1, 2]    // stereo out
```

### Node keys
Globally unique stable IDs. Recommended format: `{index:02}-{type}` (e.g. `04-delay`). Used to address nodes over the control protocol.

---

## Effect parameters

All numeric params accept integers or floats. Out-of-range values are clamped and logged. `wet` and `dry` accept either a single float (applied to both channels) or `[L, R]`.

### delay

| Parameter  | Range        | Default | Description                       |
|------------|--------------|---------|-----------------------------------|
| `time`     | 0.0 – `delay_max_seconds` | 0.5 | Delay time in **seconds** |
| `feedback` | 0.0 – 1.0    | 0.4     | Feedback amount (0 = single echo) |
| `wet`      | 0.0 – 1.0    | 0.5     | Output level                      |
| `active`   | bool         | true    | Bypass when false                 |

### reverb

| Parameter   | Range     | Default | Description                              |
|-------------|-----------|---------|------------------------------------------|
| `room_size` | 0.0 – 1.0 | 0.5     | Room size (larger = longer decay)        |
| `damping`   | 0.0 – 1.0 | 0.5     | High-frequency damping                   |
| `wet`       | 0.0 – 1.0 | 0.33    | Output level                             |
| `active`    | bool      | true    | Bypass when false                        |

### chorus

| Parameter  | Range        | Default | Description            |
|------------|--------------|---------|------------------------|
| `rate_hz`  | 0.01 – 20.0  | 1.0     | LFO rate in Hz         |
| `depth_ms` | 0.5 – 35.0   | 5.0     | Modulation depth in ms |
| `wet`      | 0.0 – 1.0    | 0.5     | Output level           |
| `active`   | bool         | true    | Bypass when false      |

### eq (peak/bell, low-shelf, high-shelf)

Set `eq_type` to `"peak"`, `"low_shelf"`, or `"high_shelf"`.

| Parameter  | Range            | Default | Description                       |
|------------|------------------|---------|-----------------------------------|
| `freq_hz`  | 20 – Nyquist     | 1000    | Centre/shelf frequency in Hz      |
| `q`        | 0.1 – 30.0       | 1.0     | Bandwidth (higher = narrower)     |
| `gain_db`  | −24 – +24        | 0.0     | Boost (+) or cut (−) in dB        |
| `active`   | bool             | true    | Bypass when false                 |

### mix

Final output stage. Combines dry and processed signals.

| Parameter | Range     | Default | Description                                         |
|-----------|-----------|---------|-----------------------------------------------------|
| `dry`     | 0.0 – 1.0 | 1.0     | Dry signal level (use 0.0 for analogue dry mixing)  |
| `wet`     | 0.0 – 1.0 | 1.0     | Effect signal level                                 |
| `gain`    | 0.0 – 4.0 | 1.0     | Overall output level                                |
| `pan`     | −1.0 – 1.0 | 0.0    | −1 = full L, 0 = centre, +1 = full R                |

### looper

Transport-driven, controlled via actions (see `SET <key>.action <value>`):

| Action     | Effect                                                  |
|------------|---------------------------------------------------------|
| `rec`      | Start recording                                         |
| `play`     | Stop recording, start playback                          |
| `overdub`  | Toggle overdub on top of current loop                   |
| `stop`     | Stop playback (loop kept in memory)                     |
| `reset`    | Clear all layers                                        |

| Parameter          | Range     | Default | Description                                       |
|--------------------|-----------|---------|---------------------------------------------------|
| `wet`              | 0.0 – 1.0 | 1.0     | Playback level                                    |
| `decay`            | 0.0 – 1.0 | 1.0     | How much each overdub layer fades on next pass    |
| `active`           | bool      | true    | Bypass when false                                 |

---

## Control devices

Devices are configured under `control_devices` in `config.json`. Each device has an alias (the JSON key) and a typed connection config.

```json
"control_devices": {
  "serial-1":  { "type": "serial",   "dev": "/dev/ttyUSB0", "baud": 115200 },
  "tcp-net":   { "type": "net",      "port": 9000 },
  "midi-kbd":  { "type": "midi-in",  "dev": "Launchkey", "channel": 1 },
  "midi-out":  { "type": "midi-out", "channel": 1 }
}
```

| Type | Fields | Description |
|---|---|---|
| `serial` | `dev`, `baud` (default 115200), `active` | USB/UART serial line |
| `net` | `host` (default `0.0.0.0`), `port`, `active` | TCP server |
| `midi-in` | `dev` (substring match, optional), `channel` (`"*"` for omni), `active` | MIDI input |
| `midi-out` | `dev`, `channel` (1–16), `active` | MIDI output |

### Mappings (per preset)

Each preset can attach a controller mapping per device. Mappings live in `presets[N].controllers`:

```json
"controllers": [
  {
    "device": "midi-kbd",
    "mappings": {
      "7":  { "target": "06-mix.gain", "ctrl": [0, 127], "param": [0.0, 1.0] },
      "11": { "target": "04-delay.feedback", "ctrl": [0, 127], "param": [0.0, 0.95] },
      "74": { "target": "07-eq.freq_hz", "ctrl": [0, 127], "param": [200, 8000], "log": true }
    }
  }
]
```

For MIDI: keys are CC numbers as strings. For serial/net: any channel ID.

`ctrl` and `param` are `[min, max]` ranges. `log: true` enables logarithmic mapping (useful for frequencies). Reverse mapping (param → controller) is applied automatically when broadcasting outbound events.

MIDI Program Change (`0xC0`) maps to preset switching automatically.

---

## Line protocol — net / serial / WebSocket

All transports speak the same line-based text protocol. One command per line (UTF-8). One response per command.

### Inbound commands (client → server)

| Command | Reply | Description |
|---|---|---|
| `SET <path> <value>` | `OK` / `ERR` | Set a parameter (numeric value) |
| `SET <path> <action>` | `OK` / `ERR` | Dispatch an action (non-numeric value) |
| `CTRL <channel_id> <raw>` | `OK` / `ERR` | Mapped control change (translated by master) |
| `CHAINS <json>` | `OK` / `ERR` | Replace all chains (JSON array of `ChainDef`) |
| `PRESET <0–127>` | `OK` / `ERR` | Switch to preset slot |
| `SAVE_PRESET <0–127>` | `OK` / `ERR` | Save current state to preset slot |
| `DELETE_PRESET <0–127>` | `OK` / `ERR` | Delete preset slot |
| `COMPARE` | `OK` / `ERR` | Toggle compare mode (saved vs working) |
| `RESET` | `OK` / `ERR` | Reset all effect state |
| `RELOAD` | `OK` / `ERR` | Re-read config.json and restart audio |
| `FETCH_CONFIG` | `CONFIG <json>` / `ERR` | Get current audio config |
| `SAVE_CONFIG <json>` | `OK` / `ERR` | Update audio config (partial — fields are optional) |
| `FETCH_DEVICES` | `DEVICES <json>` / `ERR` | Get device list |
| `PUT_DEVICE <alias> <json>` | `OK` / `ERR` | Add/update a device |
| `DELETE_DEVICE <alias>` | `OK` / `ERR` | Remove a device |
| `SET_DEVICE_NAME <old> <new>` | `OK` / `ERR` | Rename a device alias |
| `PUT_CONTROLLERS <json>` | `OK` / `ERR` | Replace controller mappings of current preset |

### Outbound broadcasts (server → all clients)

Sent unsolicited when state changes. The originating client is filtered (no echo).

| Line | When | Description |
|---|---|---|
| `SET <path> <value>` | Any param change | Param updated (echo to all *other* clients) |
| `CTRL <ch> <raw>` | Param change with reverse mapping | Mapped form for devices that prefer it |
| `RESET` | After RESET command | Effect state cleared |
| `PRESET <preset_json>` | Preset switch / save / compare | Full preset content; `preset.index` indicates active slot |
| `STATE <state>` | State transition | `Clean` / `Dirty` / `Comparing` |
| `INDICES <json_array>` | Save to empty slot / delete | Updated list of occupied preset slots |
| `EVENT <key> <event_name> <json>` | Effect-internal event | E.g. looper status / loop wrap |

### WebSocket handshake

On WS connect, the server immediately pushes:

```
SNAPSHOT <json>
```

…containing `state`, `preset`, and `preset_indices`. Subsequent updates arrive as the broadcasts above.

### Testing with netcat

```bash
echo "SET 04-delay.time 0.8" | nc localhost 9000     # if a Net device is configured at port 9000
echo "PRESET 1"              | nc localhost 9000
echo "FETCH_CONFIG"          | nc localhost 9000
```

---

## Signal model

Each chain processes one stereo block per audio callback:

```
dry_buf  ← physical input channels (routed by chain.input)
eff_buf  ← 0.0

for each node in chain.nodes:
    node.process(dry_buf, eff_buf)
        # Each Device reads dry_buf[f] + eff_buf[f] as its input
        # and writes wet output to eff_buf[f].
        #
        # Mix node:
        #   out[ch] = dry_buf[ch] * dry[ch] + eff_buf[ch] * wet[ch]

output_channels += eff_buf              # routed by chain.output
```

Multiple chains run in parallel and their outputs are summed. The dry signal is **not** mixed by the engine in analogue-bypass mode — set `dry: 0.0` on the final mix node and let your hardware add the dry path.

---

## Architecture summary

```
┌───────────────┐                                    
│  control      │  inbound: text protocol            
│  transports   │ ────────────────►  master_tx       
│  serial / net │                       │            
│  ws / midi    │ ◄──────── bus ◄───────┤            
└───────────────┘     (broadcast)       │            
                                        ▼            
                                 ┌───────────────┐   
                                 │  ConfigMaster │   
                                 │  (sole owner  │   
                                 │  of state)    │   
                                 └─────┬─────────┘   
                                       │ rtrb        
                                       ▼ (lock-free) 
                                 ┌───────────────┐   
                                 │  Audio engine │   
                                 │  (cpal thread)│   
                                 └───────────────┘   
```

- **Master** owns all configuration and snapshot state — single writer.
- All inbound control flows through `master_tx` (mpsc).
- Master pushes to audio via lock-free SPSC ring buffers (`rtrb`).
- Master broadcasts state changes on the bus (tokio `broadcast`); all transports subscribe.
- All wire-format I/O happens at the transport layer; master speaks only typed Rust values.
