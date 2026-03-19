/// A fixed-size circular buffer for mono audio samples.
///
/// Used by delay, reverb (comb filters), and chorus as read memory.
/// Each `write` call stores one sample and advances the write position.
/// `read_at(offset)` reads `offset` samples *back* from the write position,
/// where offset=1 returns the most recently written sample.
///
/// Allocation only happens at construction time; the RT thread never allocates.
pub struct RingBuffer {
    buf: Box<[f32]>,
    write: usize,
}

impl RingBuffer {
    /// Create a new buffer with `capacity` samples, filled with zeros.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "RingBuffer capacity must be > 0");
        Self {
            buf: vec![0.0_f32; capacity].into_boxed_slice(),
            write: 0,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Write one sample and advance the write position by one step.
    #[inline]
    pub fn write(&mut self, sample: f32) {
        self.buf[self.write] = sample;
        self.write = (self.write + 1) % self.buf.len();
    }

    /// Read the sample that is `offset` steps before the current write position.
    /// `offset` must be in [1, capacity].
    #[inline]
    pub fn read_at(&self, offset: usize) -> f32 {
        debug_assert!(offset >= 1 && offset <= self.buf.len());
        let len = self.buf.len();
        self.buf[(self.write + len - offset) % len]
    }

    /// Read with linear interpolation for sub-sample taps (chorus/flanger).
    ///
    /// `offset` is a float; the integer part is the base position,
    /// the fractional part interpolates towards the next sample.
    #[inline]
    pub fn read_lerp(&self, offset: f32) -> f32 {
        let floor = offset.floor() as usize;
        let frac = offset - offset.floor();
        let a = self.read_at(floor.max(1));
        let b = self.read_at((floor + 1).min(self.buf.len()));
        a + frac * (b - a)
    }

    /// Set all samples to zero and reset the write position.
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }
}
