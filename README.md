# multi-effect

![Status: WIP](https://img.shields.io/badge/status-WIP-orange)

Real-time audio effect daemon for CPAL.

Signal path: ADC → Rust engine → DAC. The dry signal is mixed in analogue — the engine outputs wet signal only.

---

## Running

```bash
cargo run                                        # loads config.yml
cargo run -- -c my-config.yml                   # specific config (YAML or JSON)
cargo run -- -p patch.yml                       # override startup patch
cargo run -- --device "USB Audio" --sample-rate 44100   # CLI overrides
RUST_LOG=debug cargo run                        # debug logging to stderr
cargo run -- --log-target syslog                # log to syslog (/var/log/syslog)
```

---

## Configuration — config.yml

Device settings and the startup patch live in one file.

```yaml
sample_rate:         48000
buffer_size:         256
device:              default        # or e.g. "USB Audio Interface"
in_channels:         1              # physical input channels
out_channels:        2              # physical output channels
control_port:        9000
delay_max_seconds:   2.0            # delay buffer size at startup
looper_max_seconds:  30.0           # looper buffer size at startup
# midi_device: "USB MIDI Interface"

chains:
  - key: "01-main"
    input:  [1, 1]   # physical input channels (1-based)
    output: [1, 2]   # physical output channels
    nodes:
      - key: "04-delay"
        type: delay
        time_ms:  500
        feedback: 0.35
        wet:      1.0
      - key: "06-mix"
        type: mix
        dry: 0.0
        wet: 1.0
```

Both `.yml` / `.yaml` and `.json` are accepted.

### Command-line overrides

Any config field can be overridden at startup. `-c` selects the config file;
all other flags overwrite the loaded config.

| Flag                    | Config field          | Example                         |
|-------------------------|-----------------------|---------------------------------|
| `-c <path>`             | —                     | `-c /etc/multi-effect.yml`      |
| `-p <path>`             | `patch`               | `-p my-patch.yml`               |
| `--sample-rate <n>`     | `sample_rate`         | `--sample-rate 44100`           |
| `--buffer-size <n>`     | `buffer_size`         | `--buffer-size 512`             |
| `--device <name>`       | `device`              | `--device "USB Audio"`          |
| `--in-channels <n>`     | `in_channels`         | `--in-channels 1`               |
| `--out-channels <n>`    | `out_channels`        | `--out-channels 2`              |
| `--control-port <n>`    | `control_port`        | `--control-port 9001`           |
| `--midi-device <name>`  | `midi_device`         | `--midi-device "USB MIDI"`      |
| `--delay-max-seconds <f>`  | `delay_max_seconds`  | `--delay-max-seconds 4.0`      |
| `--looper-max-seconds <f>` | `looper_max_seconds` | `--looper-max-seconds 60.0`   |
| `--log-target <target>` | —                     | `--log-target syslog`           |

`--log-target` accepts `stderr` (default) or `syslog`.
Log level is controlled by the `RUST_LOG` environment variable (e.g. `RUST_LOG=debug`).

---

## Patch format

Patches can be embedded in `config.yml` (under `chains:`) or loaded as a standalone file with `-p`.
YAML is preferred for hand-editing — it supports comments and you can comment out nodes.

### Node keys

Keys are **globally unique**, stable IDs. Recommended format: `{index:02}-{type}` (e.g. `04-delay`).
Gaps in numbering are fine. Keys are used to address nodes over the control protocol.

### input / output routing

`input` and `output` are 1-based physical channel numbers. A single integer sets both channels to the same physical channel.

```yaml
input: [1, 1]    # mono mic (ch 1) → L and R
input: [1, 2]    # stereo interface
output: [1, 2]   # stereo out
```

---

## Effect parameters

All numeric parameters accept integers or floats.
Out-of-range values are clamped and logged as a warning.
`wet` and `dry` accept either a single float (applied to both channels) or `[left, right]`.

### delay

| Parameter  | Range        | Default | Description                       |
|------------|--------------|---------|-----------------------------------|
| `time_ms`  | ~1 – 2000    | 500     | Delay time in ms                  |
| `feedback` | 0.0 – 1.0    | 0.4     | Feedback amount (0 = single echo) |
| `wet`      | 0.0 – 1.0    | 0.5     | Output level                      |

### reverb

| Parameter   | Range     | Default | Description                              |
|-------------|-----------|---------|------------------------------------------|
| `room_size` | 0.0 – 1.0 | 0.5     | Room size (larger = longer decay)        |
| `damping`   | 0.0 – 1.0 | 0.5     | High-frequency damping (1 = most damped) |
| `wet`       | 0.0 – 1.0 | 0.33    | Output level                             |

### chorus

| Parameter  | Range        | Default | Description            |
|------------|--------------|---------|------------------------|
| `rate_hz`  | 0.01 – 20.0  | 1.0     | LFO rate in Hz         |
| `depth_ms` | 0.5 – 35.0   | 5.0     | Modulation depth in ms |
| `wet`      | 0.0 – 1.0    | 0.5     | Output level           |

### eq\_param — parametric peak/bell

| Parameter  | Range            | Default | Description                       |
|------------|------------------|---------|-----------------------------------|
| `freq_hz`  | 20 – Nyquist     | 1000    | Centre frequency in Hz            |
| `q`        | 0.1 – 30.0       | 1.0     | Bandwidth (higher = narrower)     |
| `gain_db`  | −24 – +24        | 0.0     | Boost (+) or cut (−) in dB        |

### eq\_low — low-shelf

| Parameter  | Range            | Default | Description                       |
|------------|------------------|---------|-----------------------------------|
| `freq_hz`  | 20 – Nyquist     | 100     | Shelf frequency in Hz             |
| `q`        | 0.1 – 30.0       | 0.707   | Shelf slope                       |
| `gain_db`  | −24 – +24        | 0.0     | Boost (+) or cut (−) in dB        |

### eq\_high — high-shelf

| Parameter  | Range            | Default | Description                       |
|------------|------------------|---------|-----------------------------------|
| `freq_hz`  | 20 – Nyquist     | 10000   | Shelf frequency in Hz             |
| `q`        | 0.1 – 30.0       | 0.707   | Shelf slope                       |
| `gain_db`  | −24 – +24        | 0.0     | Boost (+) or cut (−) in dB        |

For a 3-band EQ, chain `eq_low → eq_param → eq_high`.

### mix

Controls how much dry and processed signal reaches the output. Typically placed last in the chain.

| Parameter | Range     | Default | Description                                         |
|-----------|-----------|---------|-----------------------------------------------------|
| `dry`     | 0.0 – ... | 0.0     | Dry (unprocessed) gain — use 0.0 if mixing analogue |
| `wet`     | 0.0 – ... | 0.0     | Processed signal gain                               |

### looper

| Parameter          | Range     | Default | Description                                              |
|--------------------|-----------|---------|----------------------------------------------------------|
| `record`           | 0 / 1     | —       | 1 = start recording, 0 = stop and start playback         |
| `stop`             | any       | —       | Stop playback, keep loop in memory                       |
| `overdub`          | 0 / 1     | —       | 1 = enter overdub mode, 0 = return to playback           |
| `loop_gain`        | 0.0 – ... | 1.0     | Playback level of the loop                               |
| `overdub_feedback` | 0.0 – 1.0 | 0.9     | How much of the previous loop survives each overdub pass |

---

## MIDI

MIDI input is configured under `midi:` in `config.yml`.  Each block targets one channel (or all channels with `"*"`) and maps CC numbers to parameter paths.

```yaml
# midi_device: "USB MIDI Interface"   # omit → first available port

midi:
  - channel: 1          # or "*" for omni
    controls:
      1:                # CC 1 — Modulation Wheel
        target: "05-chorus.wet"
      7:                # CC 7 — Channel Volume
        target: "04-delay.wet"
      11:               # CC 11 — Expression
        target: "04-delay.feedback"
        max: 0.95       # keep below self-oscillation
      74:               # CC 74 — Filter Cutoff
        target: "07-eq_param.freq_hz"
        min: 200.0
        max: 8000.0
```

- `min` / `max` default to `0.0` / `1.0`; the CC value is linearly mapped to that range.
- Program Change messages are forwarded as `PROGRAM` events to all chains.
- With `RUST_LOG=multi_effect=debug`, **every incoming MIDI message is logged** — useful when you don't know what your device sends.

---

## TCP control protocol — port 9000

One command per line (UTF-8). Response is `OK` or `ERR <reason>`.

### Commands

#### `SET <key.param> <value>`
Set a single parameter on a named node.

```
SET 04-delay.time_ms 800
SET 04-delay.feedback 0.4
SET 06-mix.wet 0.8
```

#### `UPDATE <json>`
Set multiple parameters in one call. Nested JSON object: `{ "node-key": { "param": value } }`.

```
UPDATE {"04-delay":{"time_ms":800,"feedback":0.4},"03-reverb":{"wet":0.3}}
```

#### `PATCH <json>`
Hot-swap to a completely new patch (full chain array in JSON). Takes effect at the next audio block.

```
PATCH {"chains":[{"key":"01-main","input":[1,1],"output":[1,2],"nodes":[...]}]}
```

#### `RESET`
Reset all effect state (clear delay buffers, reverb tails, etc.).

```
RESET
```

#### `PROGRAM <0-127>`
Send a MIDI program change event to all chains.

```
PROGRAM 5
```

### Testing with netcat

```bash
echo "SET 04-delay.time_ms 500" | nc localhost 9000
echo "UPDATE {\"04-delay\":{\"wet\":0.5}}" | nc localhost 9000
```

---

## Signal model

Each chain processes one stereo block per audio callback:

```
dry_buf  ← physical input channels (routed by chain.input)
eff_buf  ← 0.0

for each node:
    node.process(dry_buf, eff_buf)
        # each Device reads dry_buf[f] + eff_buf[f] as its input
        # and writes its wet output back to eff_buf[f]
        #
        # Mix node:
        #   eff_buf[ch] = dry_buf[ch] * dry[ch] + eff_buf[ch] * wet[ch]

output_channels += eff_buf              # routed by chain.output
```

Multiple chains run in parallel and their outputs are summed.
