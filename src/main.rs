mod config;
mod control;
mod effects;
mod engine;
mod logging;
mod save;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, StreamConfig};
use rtrb::RingBuffer;
use tracing::{info, warn};

use config::Config;
use control::{ControlMessage, NetworkControl, SerialControl, new_event_bus};
use control::mapping::{ControllerDef, DeviceDef};
use control::midi::{MidiControl, MidiOutControl};
use engine::patch::Chain;
use engine::AudioEngine;

#[tokio::main]
async fn main() -> Result<()> {
    // --- Config + CLI overrides (initialises logging internally) ---
    let (cfg, build_cfg) = Config::from_args()?;

    // --- Determine startup chains: saved state → active preset → top-level chains ---
    let state_path = cfg.effective_state_save_path();
    let chains: Vec<Chain> = if !cfg.skip_state && state_path.exists() {
        info!("Loading saved state: {}", state_path.display());
        engine::patch::load_file(state_path.to_str().unwrap(), &build_cfg)
            .with_context(|| format!("failed to load state '{}'", state_path.display()))?
    } else {
        let json = cfg.startup_chains_json()
            .context("failed to serialize startup chains")?
            .ok_or_else(|| anyhow::anyhow!("no chains in config (no presets and no 'chains' key)"))?;
        info!("No saved state, loading chains from config");
        engine::patch::load_str(&json, &build_cfg)
            .context("failed to build startup chains")?
    };

    // Extract what we need before moving cfg into the Arc.
    let devices              = cfg.devices.clone();
    let startup_controllers  = cfg.startup_controllers();
    let state_save_interval  = cfg.state_save_interval;

    // --- Shared config (preset management, SAVE_PRESET) ---
    let cfg = Arc::new(Mutex::new(cfg));

    // --- Per-device shared ControllerDef Arcs ---
    // One Arc<RwLock<ControllerDef>> per device alias.  The HashMap itself is
    // immutable after construction; the inner Arcs are swapped on preset change.
    let controller_arcs: Arc<HashMap<String, Arc<RwLock<ControllerDef>>>> = {
        let map = devices.keys()
            .map(|alias| (alias.clone(), Arc::new(RwLock::new(ControllerDef::default()))))
            .collect();
        Arc::new(map)
    };

    // Apply startup controller mappings (from active preset).
    for ctrl_def in startup_controllers {
        if let Some(arc) = controller_arcs.get(&ctrl_def.device) {
            *arc.write().unwrap() = ctrl_def;
        } else {
            warn!("Startup: controller references unknown device '{}' — skipping", ctrl_def.device);
        }
    }

    // --- Event bus (pub/sub: TCP, MIDI, serial → subscribers) ---
    let bus = new_event_bus();

    // --- Channels ---
    let (control_tx, control_rx) = RingBuffer::<ControlMessage>::new(64);
    let (patch_tx,   patch_rx)   = RingBuffer::<Vec<Chain>>::new(4);

    let control_tx = Arc::new(Mutex::new(control_tx));
    let patch_tx   = Arc::new(Mutex::new(patch_tx));

    // Bridge task: event bus → rtrb → audio thread
    {
        let mut bridge_rx = bus.subscribe();
        let control_tx_bridge = Arc::clone(&control_tx);
        tokio::spawn(async move {
            while let Ok(msg) = bridge_rx.recv().await {
                if let Ok(mut tx) = control_tx_bridge.lock() {
                    if tx.push(msg).is_err() {
                        tracing::warn!("audio control channel full, message dropped");
                    }
                }
            }
        });
    }

    // --- Patch state for persistence ---
    let initial_json = engine::patch::chains_to_json(&chains);
    let patch_state  = Arc::new(Mutex::new(save::PatchState::new(initial_json, Some(state_path))));

    // State-updater task: event bus → PatchState (handles SET)
    {
        let mut state_rx = bus.subscribe();
        let ps_bus = Arc::clone(&patch_state);
        tokio::spawn(async move {
            while let Ok(msg) = state_rx.recv().await {
                if let ControlMessage::SetParam { path, value } = msg {
                    if let Ok(mut s) = ps_bus.lock() {
                        s.apply_set(&path, value);
                    }
                }
            }
        });
    }

    // Preset-switch task: ProgramChange → swap chains + controller mappings
    {
        let mut pc_rx           = bus.subscribe();
        let patch_tx_pc         = Arc::clone(&patch_tx);
        let cfg_pc              = Arc::clone(&cfg);
        let controller_arcs_pc  = Arc::clone(&controller_arcs);
        let build_cfg_pc        = build_cfg;
        tokio::spawn(async move {
            while let Ok(msg) = pc_rx.recv().await {
                let ControlMessage::ProgramChange(slot) = msg else { continue };

                let preset = cfg_pc.lock().unwrap().presets.get(&slot).cloned();
                let Some(preset) = preset else {
                    warn!("Preset {slot} not found");
                    continue;
                };

                // Build new chains
                let chains_val = serde_json::json!({"chains": preset.chains});
                match engine::patch::load_str(&chains_val.to_string(), &build_cfg_pc) {
                    Ok(chains) => {
                        if patch_tx_pc.lock().unwrap().push(chains).is_err() {
                            tracing::warn!("patch channel full, preset {slot} not loaded");
                        } else {
                            // Clear all device mappings, then apply new preset's controllers.
                            for arc in controller_arcs_pc.values() {
                                *arc.write().unwrap() = ControllerDef::default();
                            }
                            for ctrl_def in preset.controllers {
                                if let Some(arc) = controller_arcs_pc.get(&ctrl_def.device) {
                                    *arc.write().unwrap() = ctrl_def;
                                } else {
                                    warn!("Preset {slot}: controller references unknown device '{}' — skipping", ctrl_def.device);
                                }
                            }
                            info!("Loaded preset {slot}");
                        }
                    }
                    Err(e) => warn!("Preset {slot} build error: {e}"),
                }
            }
        });
    }

    // --- Audio engine ---
    let mut engine = AudioEngine::new(
        chains,
        build_cfg.in_channels,
        build_cfg.out_channels,
        build_cfg.sample_rate as u32,
        {
            let c = cfg.lock().unwrap();
            c.buffer_size
        },
        control_rx,
        patch_rx,
    );

    // --- CPAL: find devices ---
    let host = cpal::default_host();

    let (device_name, in_channels, out_channels, sample_rate, buffer_size) = {
        let c = cfg.lock().unwrap();
        (c.device.clone(), c.in_channels, c.out_channels, c.sample_rate, c.buffer_size)
    };

    let in_device = match device_name.as_str() {
        "default" => host.default_input_device().context("no default input device")?,
        name => host
            .input_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .with_context(|| format!("input device '{name}' not found"))?,
    };

    let out_device = match device_name.as_str() {
        "default" => host.default_output_device().context("no default output device")?,
        name => host
            .output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .with_context(|| format!("output device '{name}' not found"))?,
    };

    info!("Input:  {}", in_device.name().unwrap_or_default());
    info!("Output: {}", out_device.name().unwrap_or_default());

    // Validate device configs against what we're about to request.
    if let Ok(dc) = in_device.default_input_config() {
        if dc.sample_rate().0 != sample_rate {
            warn!("input device default sample rate is {}Hz, requesting {}Hz — stream build may fail",
                dc.sample_rate().0, sample_rate);
        }
        if in_channels > dc.channels() {
            warn!("input device supports {} channels by default, requesting {} — stream build may fail",
                dc.channels(), in_channels);
        }
    }
    if let Ok(dc) = out_device.default_output_config() {
        if dc.sample_rate().0 != sample_rate {
            warn!("output device default sample rate is {}Hz, requesting {}Hz — stream build may fail",
                dc.sample_rate().0, sample_rate);
        }
        if out_channels > dc.channels() {
            warn!("output device supports {} channels by default, requesting {} — stream build may fail",
                dc.channels(), out_channels);
        }
    }

    let in_config = StreamConfig {
        channels:    in_channels,
        sample_rate: SampleRate(sample_rate),
        buffer_size: BufferSize::Fixed(buffer_size as u32),
    };
    let out_config = StreamConfig {
        channels:    out_channels,
        sample_rate: SampleRate(sample_rate),
        buffer_size: BufferSize::Fixed(buffer_size as u32),
    };

    // Ring buffer: sized for input (in_channels × buffer_size).
    let in_ch  = in_channels  as usize;
    let out_ch = out_channels as usize;
    let ring_cap = buffer_size * in_ch * 4;
    let (mut in_tx, mut in_rx) = RingBuffer::<f32>::new(ring_cap);

    let max_in_samples  = buffer_size * in_ch;
    let max_out_samples = buffer_size * out_ch;
    let mut in_buf  = vec![0.0f32; max_in_samples];
    let mut out_buf = vec![0.0f32; max_out_samples];

    let input_stream = in_device.build_input_stream(
        &in_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            for &s in data {
                let _ = in_tx.push(s);
            }
        },
        |e| tracing::error!("Input stream error: {e}"),
        None,
    )?;

    let output_stream = out_device.build_output_stream(
        &out_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let n_out = data.len();
            let n_in  = n_out / out_ch * in_ch;
            // CoreAudio may give a larger buffer than requested; resize if needed.
            // This allocation only happens when the buffer size changes, not in steady state.
            if n_in  > in_buf.len()  { in_buf.resize(n_in,   0.0); }
            if n_out > out_buf.len() { out_buf.resize(n_out,  0.0); }
            for s in in_buf[..n_in].iter_mut() {
                *s = in_rx.pop().unwrap_or(0.0);
            }
            engine.process_block(&in_buf[..n_in], &mut out_buf[..n_out]);
            data.copy_from_slice(&out_buf[..n_out]);
        },
        |e| tracing::error!("Output stream error: {e}"),
        None,
    )?;

    input_stream.play()?;
    output_stream.play()?;
    info!("Audio running.");

    // --- Start control devices ---
    for (alias, dev_def) in &devices {
        if !dev_def.is_active() {
            info!("Device '{alias}': disabled (active: false)");
            continue;
        }

        let mappings = match controller_arcs.get(alias) {
            Some(arc) => Arc::clone(arc),
            None      => continue, // should not happen
        };

        match dev_def {
            DeviceDef::Serial { dev, baud, fallback, .. } => {
                let serial = SerialControl::new(
                    dev.clone(),
                    *baud,
                    *fallback,
                    build_cfg,
                    Arc::clone(&patch_state),
                    Arc::clone(&cfg),
                    bus.clone(),
                    mappings,
                );
                let patch_tx_serial = Arc::clone(&patch_tx);
                let alias_s = alias.clone();
                tokio::spawn(async move {
                    if let Err(e) = serial.run(patch_tx_serial).await {
                        tracing::error!("Serial '{alias_s}': {e}");
                    }
                });
            }

            DeviceDef::Net { port, fallback, .. } => {
                let net = NetworkControl::new(
                    *port,
                    *fallback,
                    build_cfg,
                    Arc::clone(&patch_state),
                    Arc::clone(&cfg),
                    bus.clone(),
                    mappings,
                );
                let patch_tx_net = Arc::clone(&patch_tx);
                let alias_s = alias.clone();
                tokio::spawn(async move {
                    if let Err(e) = net.run(patch_tx_net).await {
                        tracing::error!("Network '{alias_s}': {e}");
                    }
                });
            }

            DeviceDef::MidiIn { dev, channel, .. } => {
                let midi = MidiControl::new(dev.clone(), channel.clone(), mappings);
                midi.run(bus.clone());
            }

            DeviceDef::MidiOut { dev, channel, .. } => {
                let midi_out = MidiOutControl::new(dev.clone(), *channel, mappings);
                midi_out.run(bus.clone());
            }
        }
    }

    // --- Periodic state save task ---
    if state_save_interval > 0 {
        let ps_periodic   = Arc::clone(&patch_state);
        let interval_secs = state_save_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                let mut s = ps_periodic.lock().unwrap();
                if s.dirty {
                    if let Err(e) = s.save() {
                        tracing::warn!("periodic state save failed: {e}");
                    } else {
                        tracing::debug!("state saved");
                    }
                }
            }
        });
    }

    // Block until Ctrl-C
    tokio::signal::ctrl_c().await?;

    // Save state on shutdown
    if let Ok(mut s) = patch_state.lock() {
        if s.dirty {
            if let Err(e) = s.save() {
                tracing::warn!("shutdown state save failed: {e}");
            } else {
                tracing::info!("state saved");
            }
        }
    }

    info!("Shutting down.");

    Ok(())
}
