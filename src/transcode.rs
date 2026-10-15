use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use audioadapter_buffers::direct::InterleavedSlice;
use audioadapter_buffers::owned::InterleavedOwned;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus_rs::{Application, OpusEncoder};
use rubato::{Fft, FixedSync, Indexing, Resampler};
use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};
use rusty_mp3::{Error as Mp3Error, Mp3Encoder, Mp3EncoderConfig};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;

pub const PRE_SKIP: u16 = 312;
pub const MAX_BUFFERED_SECS: i64 = 20 * 60;
const FRAME: usize = 960;
const OPUS_RATE: u32 = 48_000;
const SERIAL: u32 = 0x6a75_757a;
const MP3_RATES: [u32; 9] = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Opus,
    Mp3,
    Aac,
}

impl Format {
    pub fn parse(value: &str) -> Option<Format> {
        match value.to_ascii_lowercase().as_str() {
            "opus" => Some(Format::Opus),
            "mp3" => Some(Format::Mp3),
            "aac" => Some(Format::Aac),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Opus => "opus",
            Format::Mp3 => "mp3",
            Format::Aac => "aac",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Format::Opus => "audio/ogg",
            Format::Mp3 => "audio/mpeg",
            Format::Aac => "audio/aac",
        }
    }
}

fn lossy_rate(rate: u32) -> u32 {
    if MP3_RATES.contains(&rate) {
        rate
    } else if rate > 48_000 && rate.is_multiple_of(44_100) {
        44_100
    } else {
        48_000
    }
}

struct Resample {
    channels: usize,
    rs: Option<Fft<f32>>,
    in_fifo: Vec<f32>,
    to_trim: usize,
    emitted: u64,
    total_in: u64,
}

impl Resample {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Resample> {
        let rs = if in_rate == out_rate {
            None
        } else {
            Some(Fft::<f32>::new(
                in_rate as usize,
                out_rate as usize,
                1024,
                channels,
                FixedSync::Input,
            )?)
        };
        let to_trim = rs.as_ref().map(|r| r.output_delay()).unwrap_or(0);
        Ok(Resample {
            channels,
            rs,
            in_fifo: Vec::new(),
            to_trim,
            emitted: 0,
            total_in: 0,
        })
    }

    fn take(&mut self, data: &[f32], frames: usize, out: &mut Vec<f32>) {
        let skip = self.to_trim.min(frames);
        self.to_trim -= skip;
        out.extend_from_slice(&data[skip * self.channels..frames * self.channels]);
        self.emitted += (frames - skip) as u64;
    }

    fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.total_in += (samples.len() / self.channels) as u64;
        let Some(mut rs) = self.rs.take() else {
            out.extend_from_slice(samples);
            self.emitted += (samples.len() / self.channels) as u64;
            return Ok(());
        };
        self.in_fifo.extend_from_slice(samples);
        loop {
            let need = rs.input_frames_next();
            if self.in_fifo.len() / self.channels < need {
                break;
            }
            let input =
                InterleavedSlice::new(&self.in_fifo[..need * self.channels], self.channels, need)?;
            let mut buf = InterleavedOwned::new(0.0f32, self.channels, rs.output_frames_max());
            let (nin, nout) = rs.process_into_buffer(&input, &mut buf, None)?;
            let data = buf.take_data();
            self.in_fifo.drain(..nin * self.channels);
            self.take(&data, nout, out);
        }
        self.rs = Some(rs);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let Some(mut rs) = self.rs.take() else {
            return Ok(());
        };
        let start = out.len();
        let expected = (rs.resample_ratio() * self.total_in as f64).ceil() as u64;
        let mut partial = Some(self.in_fifo.len() / self.channels);
        while self.emitted < expected {
            let need = rs.input_frames_next();
            let mut input_buf = std::mem::take(&mut self.in_fifo);
            input_buf.resize(need * self.channels, 0.0);
            let input = InterleavedSlice::new(&input_buf[..], self.channels, need)?;
            let mut buf = InterleavedOwned::new(0.0f32, self.channels, rs.output_frames_max());
            let indexing = Indexing {
                input_offset: 0,
                output_offset: 0,
                partial_len: Some(partial.take().unwrap_or(0)),
                active_channels_mask: None,
            };
            let (_, nout) = rs.process_into_buffer(&input, &mut buf, Some(&indexing))?;
            let data = buf.take_data();
            self.take(&data, nout, out);
        }
        let surplus = (self.emitted - expected) as usize * self.channels;
        let keep = out.len().saturating_sub(surplus).max(start);
        out.truncate(keep);
        Ok(())
    }
}

trait Sink {
    fn push(&mut self, pcm: &[f32]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

struct OpusSink {
    channels: usize,
    fifo: Vec<f32>,
    encoder: OpusEncoder,
    packet: Vec<u8>,
    writer: PacketWriter<'static, BufWriter<File>>,
    pending: Option<(Vec<u8>, u64)>,
    encoded: u64,
}

impl OpusSink {
    fn new(dest: &Path, in_rate: u32, channels: usize, bitrate_bps: i32) -> Result<OpusSink> {
        let mut encoder = OpusEncoder::new(OPUS_RATE as i32, channels, Application::Audio)
            .map_err(|e| anyhow!("opus encoder: {e:?}"))?;
        encoder.bitrate_bps = bitrate_bps;
        let mut writer = PacketWriter::new(BufWriter::new(File::create(dest)?));

        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(channels as u8);
        head.extend_from_slice(&PRE_SKIP.to_le_bytes());
        head.extend_from_slice(&in_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        writer.write_packet(head, SERIAL, PacketWriteEndInfo::EndPage, 0)?;

        let vendor = concat!("juuz ", env!("CARGO_PKG_VERSION"));
        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer.write_packet(tags, SERIAL, PacketWriteEndInfo::EndPage, 0)?;

        Ok(OpusSink {
            channels,
            fifo: Vec::new(),
            encoder,
            packet: vec![0u8; 4000],
            writer,
            pending: None,
            encoded: 0,
        })
    }

    fn encode_frame(&mut self, pcm: &[f32], real_frames: usize) -> Result<()> {
        let n = self
            .encoder
            .encode(pcm, FRAME, &mut self.packet)
            .map_err(|e| anyhow!("opus encode: {e:?}"))?;
        self.encoded += real_frames as u64;
        let granule = u64::from(PRE_SKIP) + self.encoded;
        let packet = self.packet[..n].to_vec();
        if let Some((p, g)) = self.pending.replace((packet, granule)) {
            self.writer
                .write_packet(p, SERIAL, PacketWriteEndInfo::NormalPacket, g)?;
        }
        Ok(())
    }

    fn encode_ready(&mut self) -> Result<()> {
        let size = FRAME * self.channels;
        while self.fifo.len() >= size {
            let frame: Vec<f32> = self.fifo.drain(..size).collect();
            self.encode_frame(&frame, FRAME)?;
        }
        Ok(())
    }
}

impl Sink for OpusSink {
    fn push(&mut self, pcm: &[f32]) -> Result<()> {
        self.fifo.extend_from_slice(pcm);
        self.encode_ready()
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.encode_ready()?;
        let leftover = self.fifo.len() / self.channels;
        if leftover > 0 {
            let mut frame = std::mem::take(&mut self.fifo);
            frame.resize(FRAME * self.channels, 0.0);
            self.encode_frame(&frame, leftover)?;
        }
        match self.pending.take() {
            Some((p, g)) => {
                self.writer
                    .write_packet(p, SERIAL, PacketWriteEndInfo::EndStream, g)?;
                Ok(())
            }
            None => bail!("no audio decoded"),
        }
    }
}

struct AacSink {
    encoder: AacEncoder,
    out: BufWriter<File>,
    channels: u16,
    rate: u32,
    pushed: bool,
}

impl AacSink {
    fn new(dest: &Path, rate: u32, channels: usize, bitrate_bps: u32) -> Result<AacSink> {
        Ok(AacSink {
            encoder: AacEncoder::new(AacEncoderConfig {
                bitrate_bps,
                ..AacEncoderConfig::default()
            }),
            out: BufWriter::new(File::create(dest)?),
            channels: channels as u16,
            rate,
            pushed: false,
        })
    }
}

impl Sink for AacSink {
    fn push(&mut self, pcm: &[f32]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        self.pushed = true;
        self.encoder
            .push_pcm(pcm, self.channels, self.rate)
            .map_err(|e| anyhow!("aac encode: {e:?}"))
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if !self.pushed {
            bail!("no audio decoded");
        }
        self.encoder.finish();
        while let Ok(packet) = self.encoder.next_packet() {
            self.out.write_all(&write_adts_header(&AdtsHeader {
                object_type: 2,
                sample_rate: self.rate,
                channels: self.channels,
                frame_length: 7 + packet.data.len(),
                header_len: 7,
            }))?;
            self.out.write_all(&packet.data)?;
        }
        self.out.flush()?;
        Ok(())
    }
}

struct Mp3Sink {
    encoder: Mp3Encoder,
    out: BufWriter<File>,
    channels: u16,
    rate: u32,
    pushed: bool,
}

impl Mp3Sink {
    fn new(dest: &Path, rate: u32, channels: usize, bitrate_kbps: u32) -> Result<Mp3Sink> {
        Ok(Mp3Sink {
            encoder: Mp3Encoder::new(Mp3EncoderConfig {
                bitrate_kbps,
                vbr_quality: None,
            }),
            out: BufWriter::new(File::create(dest)?),
            channels: channels as u16,
            rate,
            pushed: false,
        })
    }
}

impl Sink for Mp3Sink {
    fn push(&mut self, pcm: &[f32]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        self.pushed = true;
        self.encoder
            .push_pcm_f32(pcm, self.channels, self.rate)
            .map_err(|e| anyhow!("mp3 encode: {e:?}"))
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if !self.pushed {
            bail!("no audio decoded");
        }
        self.encoder.finish();
        loop {
            match self.encoder.next_packet() {
                Ok(p) => self.out.write_all(&p)?,
                Err(Mp3Error::Eof) => break,
                Err(e) => bail!("mp3 encode: {e:?}"),
            }
        }
        self.out.flush()?;
        Ok(())
    }
}

pub fn transcode(src: &Path, dest: &Path, format: Format, bitrate_bps: i32) -> Result<()> {
    let file = Box::new(File::open(src)?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .probe(&hint, mss, Default::default(), Default::default())
        .map_err(|e| anyhow!("probe: {e}"))?;
    let track = probed
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
        bail!("only mono and stereo sources can be transcoded");
    }
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow!("decoder: {e}"))?;
    let track_id = track.id;

    let (out_rate, mut sink): (u32, Box<dyn Sink>) = match format {
        Format::Opus => (
            OPUS_RATE,
            Box::new(OpusSink::new(dest, rate, channels, bitrate_bps)?),
        ),
        Format::Aac => {
            let out_rate = lossy_rate(rate);
            (
                out_rate,
                Box::new(AacSink::new(dest, out_rate, channels, bitrate_bps as u32)?),
            )
        }
        Format::Mp3 => {
            let out_rate = lossy_rate(rate);
            (
                out_rate,
                Box::new(Mp3Sink::new(
                    dest,
                    out_rate,
                    channels,
                    (bitrate_bps / 1000) as u32,
                )?),
            )
        }
    };
    let mut resample = Resample::new(rate, out_rate, channels)?;

    let mut samples: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    while let Some(packet) = probed.next_packet().map_err(|e| anyhow!("read: {e}"))? {
        if packet.track_id != track_id {
            continue;
        }
        let Ok(buf) = decoder.decode(&packet) else {
            continue;
        };
        samples.resize(buf.samples_interleaved(), f32::MID);
        buf.copy_to_slice_interleaved(&mut samples);
        if !samples.len().is_multiple_of(channels) {
            continue;
        }
        resampled.clear();
        resample.push(&samples, &mut resampled)?;
        sink.push(&resampled)?;
    }
    resampled.clear();
    resample.finish(&mut resampled)?;
    sink.push(&resampled)?;
    sink.finish()
}
