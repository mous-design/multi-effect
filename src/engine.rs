pub mod device;
pub mod patch;
pub mod ring_buffer;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, StreamConfig};
use tracing::{info, warn};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::control::ControlMessage;
use crate::engine::device::ParamValue;
use crate::engine::patch::Chain;

// ---------------------------------------------------------------------------
// AudioHandle — the "remote control" for the audio engine.
// Lives on the master/tokio side; pushes into lock-free ring buffers.
// ---------------------------------------------------------------------------

pub struct AudioHandle {
    control_tx: Producer<ControlMessage>,
    patch_tx:   Producer<Vec<Chain>>,
}


impl AudioHandle {
    pub fn push_control(&mut self, msg: ControlMessage) -> Result<(), rtrb::PushError<ControlMessage>> {
        self.control_tx.push(msg)
    }
    pub fn push_patch(&mut self, chains: Vec<Chain>) -> Result<(), rtrb::PushError<Vec<Chain>>> {
        self.patch_tx.push(chains)
    }

}
pub struct AudioStreams {
    input_stream: cpal::Stream,
    output_stream: cpal::Stream,
}
impl AudioStreams {
    pub fn play(&self) -> Result<()> {
        self.input_stream.play()?;
        self.output_stream.play()?;
        info!("Audio running.");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AudioEngine — the real-time side. Moves into the CPAL callback.
// ---------------------------------------------------------------------------

/// The central audio processing unit.
///
/// Owns a list of `Chain`s (the full signal graph) and processes
/// interleaved audio data in blocks.  Called from the CPAL callback;
/// everything here is realtime-safe (no allocations, no locks).
pub struct AudioEngine {
    pub chains: Vec<Chain>,
    /// Interleaved channel count of the input buffer (from ADC).
    pub in_channels: u16,
    /// Interleaved channel count of the output buffer (to DAC).
    pub out_channels: u16,
    #[allow(dead_code)]
    pub sample_rate: u32,
    pub buffer_size: u32,

    /// Receives parameter updates from the control thread (lock-free).
    control_rx: Consumer<ControlMessage>,

    /// Receives full patch swaps.
    patch_rx: Consumer<Vec<Chain>>,
}

impl AudioEngine {
    /// Create the engine and its handle. The handle goes to the master,
    /// the engine moves into the CPAL callback.
    pub fn build(
        in_channels: u16,
        out_channels: u16,
        sample_rate: u32,
        buffer_size: u32,
        device_name: String,
    ) -> Result<(AudioHandle, AudioStreams)> {
    // // needs to be wired into the CPAL callback below.
    // let device_name        = cfg.audio_device.clone();

        let (control_tx, control_rx) = RingBuffer::<ControlMessage>::new(64);
        let (patch_tx, patch_rx)     = RingBuffer::<Vec<Chain>>::new(4);

        let engine = Self {
            chains: Vec::new(), in_channels, out_channels, sample_rate, buffer_size,
            control_rx, patch_rx,
        };

        let (input_stream, output_stream) = engine.connect(device_name)?;
        Ok((
            AudioHandle { control_tx, patch_tx },
            AudioStreams { input_stream, output_stream },
        ))
    }


    fn connect(mut self, device_name: String) -> Result<(cpal::Stream, cpal::Stream)> {
        // --- CPAL: find devices ---
        let host = cpal::default_host();

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

        // Validate device configs.
        if let Ok(dc) = in_device.default_input_config() {
            if dc.sample_rate().0 != self.sample_rate {
                warn!("input device default sample rate is {}Hz, requesting {}Hz — stream build may fail",
                    dc.sample_rate().0, self.sample_rate);
            }
            if self.in_channels > dc.channels() {
                warn!("input device supports {} channels by default, requesting {} — stream build may fail",
                    dc.channels(), self.in_channels);
            }
        }
        if let Ok(dc) = out_device.default_output_config() {
            if dc.sample_rate().0 != self.sample_rate {
                warn!("output device default sample rate is {}Hz, requesting {}Hz — stream build may fail",
                    dc.sample_rate().0, self.sample_rate);
            }
            if self.out_channels as u16 > dc.channels() {
                warn!("output device supports {} channels by default, requesting {} — stream build may fail",
                    dc.channels(), self.out_channels);
            }
        }

        let in_config = StreamConfig {
            channels:    self.in_channels,
            sample_rate: SampleRate(self.sample_rate),
            buffer_size: BufferSize::Fixed(self.buffer_size),
        };
        let out_config = StreamConfig {
            channels:    self.out_channels,
            sample_rate: SampleRate(self.sample_rate),
            buffer_size: BufferSize::Fixed(self.buffer_size),
        };

        let in_ch  = self.in_channels  as usize;
        let out_ch = self.out_channels as usize;
        let buf_size = self.buffer_size as usize;
        let ring_cap = buf_size * in_ch * 4;
        let (mut in_tx, mut in_rx) = RingBuffer::<f32>::new(ring_cap);

        let max_in_samples  = buf_size * in_ch;
        let max_out_samples = buf_size * out_ch;
        let mut in_buf  = vec![0.0f32; max_in_samples];
        let mut out_buf = vec![0.0f32; max_out_samples];

        let input_stream = in_device.build_input_stream(
            &in_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for &s in data { let _ = in_tx.push(s); }
            },
            |e| tracing::error!("Input stream error: {e}"),
            None,
        )?;

        let output_stream = out_device.build_output_stream(
            &out_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let n_out = data.len();
                let n_in  = n_out / out_ch * in_ch;
                if n_in  > in_buf.len()  { in_buf.resize(n_in,   0.0); }
                if n_out > out_buf.len() { out_buf.resize(n_out,  0.0); }
                for s in in_buf[..n_in].iter_mut() { *s = in_rx.pop().unwrap_or(0.0); }
                self.process_block(&in_buf[..n_in], &mut out_buf[..n_out]);
                data.copy_from_slice(&out_buf[..n_out]);
            },
            |e| tracing::error!("Output stream error: {e}"),
            None,
        )?;

        Ok((input_stream, output_stream))
    }

    /// Process one audio block.  Called from the CPAL callback.
    ///
    /// `input`  — interleaved f32 slice: `block_size × in_channels` samples.
    /// `output` — interleaved f32 slice: `block_size × out_channels` samples.
    ///
    /// 1. Install any pending patch swap.
    /// 2. Drain incoming control messages.
    /// 3. Clear output, then run each chain.
    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        self.install_pending_patch();
        self.drain_control();

        for s in output.iter_mut() { *s = 0.0; }

        let block_size = output.len() / (self.out_channels as usize);
        for chain in &mut self.chains {
            chain.process(block_size, self.in_channels, self.out_channels, input, output);
        }

    }

    fn install_pending_patch(&mut self) {
        let mut latest: Option<Vec<Chain>> = None;
        while let Ok(p) = self.patch_rx.pop() {
            latest = Some(p);
        }
        if let Some(mut new_chains) = latest {
            for chain in &mut new_chains {
                chain.prepare(self.buffer_size as usize);
            }
            self.chains = new_chains;
        }
    }

    fn drain_control(&mut self) {
        while let Ok(msg) = self.control_rx.pop() {
            match msg {
                ControlMessage::SetParam { path, value } => {
                    let handled = self.chains.iter_mut().any(|c| c.set_param(&path, ParamValue::Float(value)).is_ok());
                    if !handled {
                        warn!("SET '{path}': unknown parameter");
                    }
                }
                ControlMessage::ProgramChange(p) => {
                    for chain in &mut self.chains { chain.on_program_change(p); }
                }
                ControlMessage::Reset => {
                    for chain in &mut self.chains { chain.reset(); }
                }
                ControlMessage::NoteOn { note, velocity } => {
                    for chain in &mut self.chains { chain.on_note_on(note, velocity); }
                }
                ControlMessage::NoteOff { note } => {
                    for chain in &mut self.chains { chain.on_note_off(note); }
                }
                ControlMessage::Action { path, action } => {
                    let handled = self.chains.iter_mut().any(|c| c.dispatch_action(&path, &action).is_ok());
                    if !handled {
                        warn!("ACTION '{path}' '{action}': no handler");
                    }
                }
                // NodeEvent is fired directly by nodes (e.g. Looper) via their stored EventBus.
                ControlMessage::NodeEvent { .. }
                | ControlMessage::Compare => {}
            }
        }
    }
}
