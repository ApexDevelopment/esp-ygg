//! Voice calls: codecs and the audio that moves between them and the
//! board's speaker and microphone.

pub mod mulaw;

/// Samples per second, the telephone rate every codec here uses.
pub const SAMPLE_RATE: u32 = 8000;
/// Samples in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 160;
