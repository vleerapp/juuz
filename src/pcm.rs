use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

use crate::transcode::Resample;

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

pub struct PcmSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    channels: usize,
    resample: Resample,
    skip: u64,
    out: VecDeque<f32>,
    scratch: Vec<f32>,
    resampled: Vec<f32>,
    done: bool,
}

impl PcmSource {
    pub fn open(path: &Path, offset_secs: u64) -> Result<PcmSource> {
        let file = Box::new(File::open(path)?);
        let mss = MediaSourceStream::new(file, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, Default::default(), Default::default())
            .map_err(|e| anyhow!("probe: {e}"))?;
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("no audio track"))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| anyhow!("no audio parameters"))?;
        let rate = params
            .sample_rate
            .ok_or_else(|| anyhow!("unknown sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .ok_or_else(|| anyhow!("unknown channel layout"))?;
        if channels == 0 || channels > 2 {
            bail!("only mono and stereo sources can be played");
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| anyhow!("decoder: {e}"))?;
        let track_id = track.id;
        Ok(PcmSource {
            reader,
            decoder,
            track_id,
            channels,
            resample: Resample::new(rate, RATE, channels)?,
            skip: offset_secs * u64::from(rate),
            out: VecDeque::new(),
            scratch: Vec::new(),
            resampled: Vec::new(),
            done: false,
        })
    }

    fn push(&mut self, mut samples: &[f32]) -> Result<()> {
        if self.skip > 0 {
            let frames = (samples.len() / self.channels) as u64;
            let drop = self.skip.min(frames);
            self.skip -= drop;
            samples = &samples[drop as usize * self.channels..];
            if samples.is_empty() {
                return Ok(());
            }
        }
        self.resampled.clear();
        self.resample.push(samples, &mut self.resampled)?;
        self.queue();
        Ok(())
    }

    fn queue(&mut self) {
        if self.channels == 1 {
            for s in &self.resampled {
                self.out.push_back(*s);
                self.out.push_back(*s);
            }
        } else {
            self.out.extend(self.resampled.iter().copied());
        }
    }

    fn fill(&mut self) -> Result<()> {
        while !self.done {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.done = true;
                    self.resampled.clear();
                    self.resample.finish(&mut self.resampled)?;
                    self.queue();
                    return Ok(());
                }
                Err(e) => return Err(anyhow!("read: {e}")),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let Ok(buf) = self.decoder.decode(&packet) else {
                continue;
            };
            let mut samples = std::mem::take(&mut self.scratch);
            samples.resize(buf.samples_interleaved(), f32::MID);
            buf.copy_to_slice_interleaved(&mut samples);
            if samples.len().is_multiple_of(self.channels) {
                self.push(&samples)?;
            }
            self.scratch = samples;
            if !self.out.is_empty() {
                return Ok(());
            }
        }
        Ok(())
    }

    pub fn read(&mut self, frames: usize, into: &mut Vec<f32>) -> Result<usize> {
        let want = frames * CHANNELS;
        while self.out.len() < want && !self.done {
            self.fill()?;
        }
        let take = self.out.len().min(want);
        into.extend(self.out.drain(..take));
        Ok(take / CHANNELS)
    }

    pub fn finished(&self) -> bool {
        self.done && self.out.is_empty()
    }
}
