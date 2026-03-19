pub mod device;
pub mod patch;
pub mod ring_buffer;

use tracing::warn;

use crate::control::ControlMessage;
use crate::engine::device::ParamValue;
use crate::engine::patch::Chain;
use rtrb::Consumer;

/// The central audio processing unit.
///
/// Owns a list of `Chain`s (the full signal graph) and processes
/// interleaved audio data in blocks.  Called from the CPAL callback;
/// everything here is realtime-safe (no allocations, no locks).
pub struct AudioEngine {
    pub chains: Vec<Chain>,
    /// Interleaved channel count of the input buffer (from ADC).
    pub in_channels: usize,
    /// Interleaved channel count of the output buffer (to DAC).
    pub out_channels: usize,
    #[allow(dead_code)]
    pub sample_rate: u32,
    pub buffer_size: usize,

    /// Receives parameter updates from the control thread (lock-free).
    control_rx: Consumer<ControlMessage>,

    /// Receives full patch swaps.
    patch_rx: Consumer<Vec<Chain>>,
}

impl AudioEngine {
    pub fn new(
        mut chains: Vec<Chain>,
        in_channels: usize,
        out_channels: usize,
        sample_rate: u32,
        buffer_size: usize,
        control_rx: Consumer<ControlMessage>,
        patch_rx: Consumer<Vec<Chain>>,
    ) -> Self {
        for chain in &mut chains {
            chain.prepare(buffer_size);
        }
        Self { chains, in_channels, out_channels, sample_rate, buffer_size, control_rx, patch_rx }
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

        let block_size = output.len() / self.out_channels;
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
                chain.prepare(self.buffer_size);
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
            }
        }
    }
}
