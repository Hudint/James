//! Opus encoding tuned for music.

use audiopus::coder::Encoder;
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};
use bytes::Bytes;
use james_core::audio::{OpusFrame, FRAME_LEN};

/// Largest possible Opus packet.
pub const MAX_PACKET: usize = 1275;

pub struct OpusEncoder {
    encoder: Encoder,
    buf: Vec<u8>,
}

impl OpusEncoder {
    /// `max_frame_bytes` caps every encoded frame (platforms drop larger voice packets, e.g.
    /// TeamSpeak above 500 bytes); libopus lowers the quality of a frame that would not fit.
    pub fn new(bitrate: u32, max_frame_bytes: usize) -> anyhow::Result<Self> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;
        encoder.set_signal(Signal::Music)?;
        encoder.set_complexity(10)?;
        encoder.set_vbr(true)?;
        // Constrained VBR: frames stay close to the average size, so the cap is rarely hit.
        encoder.set_vbr_constraint(true)?;
        encoder.set_bitrate(Bitrate::BitsPerSecond(bitrate as i32))?;
        Ok(Self {
            encoder,
            buf: vec![0; max_frame_bytes.clamp(16, MAX_PACKET)],
        })
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> anyhow::Result<()> {
        self.encoder.set_bitrate(Bitrate::BitsPerSecond(bitrate as i32))?;
        Ok(())
    }

    pub fn encode(&mut self, frame: &[f32; FRAME_LEN]) -> anyhow::Result<OpusFrame> {
        let len = self.encoder.encode_float(frame, &mut self.buf)?;
        Ok(OpusFrame {
            data: Bytes::copy_from_slice(&self.buf[..len]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_never_exceed_the_cap() {
        // White noise is the hardest signal; at 192 kbit/s frames average ~480 bytes.
        let mut encoder = OpusEncoder::new(192_000, 484).unwrap();
        let mut seed = 0x2545_f491_u32;
        let mut largest = 0;
        for _ in 0..100 {
            let mut frame = [0f32; FRAME_LEN];
            for sample in frame.iter_mut() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *sample = (seed as f32 / u32::MAX as f32 - 0.5) * 1.6;
            }
            largest = largest.max(encoder.encode(&frame).unwrap().data.len());
        }
        assert!(largest <= 484, "{largest}");
        assert!(largest > 300, "the bitrate is used ({largest} bytes)");
    }
}
