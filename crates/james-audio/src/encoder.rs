//! Opus encoding tuned for music.

use audiopus::coder::Encoder;
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};
use bytes::Bytes;
use james_core::audio::{OpusFrame, FRAME_LEN};

/// Largest possible Opus packet.
const MAX_PACKET: usize = 1275;

pub struct OpusEncoder {
    encoder: Encoder,
    buf: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(bitrate: u32) -> anyhow::Result<Self> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;
        encoder.set_signal(Signal::Music)?;
        encoder.set_complexity(10)?;
        encoder.set_vbr(true)?;
        encoder.set_vbr_constraint(false)?;
        encoder.set_bitrate(Bitrate::BitsPerSecond(bitrate as i32))?;
        Ok(Self {
            encoder,
            buf: vec![0; MAX_PACKET],
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
