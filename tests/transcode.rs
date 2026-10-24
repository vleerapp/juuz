mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use ogg::reading::PacketReader;
use opus_rs::OpusDecoder;

struct Decoded {
    channels: u8,
    pre_skip: u16,
    packets: usize,
    last_granule: u64,
    rms: f64,
}

fn inspect(bytes: &[u8]) -> Decoded {
    let mut reader = PacketReader::new(std::io::Cursor::new(bytes));
    let head = reader.read_packet().unwrap().unwrap();
    assert_eq!(&head.data[..8], b"OpusHead");
    let channels = head.data[9];
    let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]);
    let tags = reader.read_packet().unwrap().unwrap();
    assert_eq!(&tags.data[..8], b"OpusTags");

    let mut decoder = OpusDecoder::new(48000, channels as usize).unwrap();
    let mut packets = 0;
    let mut last_granule = 0;
    let mut energy = 0.0f64;
    let mut count = 0u64;
    let mut pcm = vec![0.0f32; 960 * channels as usize];
    let mut ended = false;
    while let Some(p) = reader.read_packet().unwrap() {
        packets += 1;
        last_granule = p.absgp_page();
        ended = p.last_in_stream();
        let n = decoder.decode(&p.data, 960, &mut pcm).unwrap();
        for s in &pcm[..n * channels as usize] {
            energy += f64::from(*s) * f64::from(*s);
            count += 1;
        }
    }
    assert!(ended, "stream must end with an EOS page");
    Decoded {
        channels,
        pre_skip,
        packets,
        last_granule,
        rms: (energy / count.max(1) as f64).sqrt(),
    }
}

async fn upload_wav(h: &Harness, bytes: Vec<u8>, title: &str) -> String {
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&title={title}&artist=T&album=A&track=1"),
            "x.wav",
            &bytes,
        )
        .await;
    v["juuzUploadResult"]["song"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn opus_transcode_mono_resampled() {
    let h = Harness::new();
    let id = upload_wav(&h, wav_bytes_spec(1, 2000, 8000, 1), "mono").await;
    let r = h
        .get(&format!(
            "/rest/stream?{}&id={id}&format=opus&maxBitRate=64",
            h.auth()
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(&r.body[..4], b"OggS");
    let d = inspect(&r.body);
    assert_eq!(d.channels, 1);
    assert_eq!(d.pre_skip, 312);
    assert_eq!(
        d.last_granule,
        312 + 2 * 48000,
        "exact duration after resampling"
    );
    assert_eq!(d.packets, (2 * 48000_usize).div_ceil(960));
    assert!(d.rms > 0.05, "decoded audio must not be silent: {}", d.rms);
    assert!(r.body.len() < 20_000, "{} bytes", r.body.len());

    let cached: Vec<_> = walk(&h.data.path().join("cache/transcode"));
    assert_eq!(cached.len(), 1);
    let again = h
        .get(&format!(
            "/rest/stream?{}&id={id}&format=opus&maxBitRate=64",
            h.auth()
        ))
        .await;
    assert_eq!(again.body, r.body);
    assert_eq!(walk(&h.data.path().join("cache/transcode")).len(), 1);

    let ranged = h
        .raw(
            Request::builder()
                .uri(format!(
                    "/rest/stream?{}&id={id}&format=opus&maxBitRate=64",
                    h.auth()
                ))
                .header("range", "bytes=0-9")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.body.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn opus_transcode_stereo_44100_and_48000() {
    let h = Harness::new();
    for (i, rate) in [44_100u32, 48_000].into_iter().enumerate() {
        let id = upload_wav(
            &h,
            wav_bytes_spec(10 + i as u32, 3000, rate, 2),
            &format!("st{rate}"),
        )
        .await;
        let r = h
            .get(&format!(
                "/rest/stream?{}&id={id}&format=opus&maxBitRate=128",
                h.auth()
            ))
            .await;
        let d = inspect(&r.body);
        assert_eq!(d.channels, 2);
        assert_eq!(d.last_granule, 312 + 3 * 48000, "rate {rate}");
        assert!(d.rms > 0.05, "rate {rate}: {}", d.rms);
        assert_eq!(d.packets, (3 * 48000_usize).div_ceil(960));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn other_formats_and_default_serve_the_original() {
    let h = Harness::new();
    let wav = wav_bytes_spec(3, 800, 8000, 1);
    let id = upload_wav(&h, wav.clone(), "plain").await;
    for q in [
        "",
        "&format=wma",
        "&format=raw",
        "&format=flac",
        "&maxBitRate=64",
    ] {
        let r = h
            .get(&format!("/rest/stream?{}&id={id}{q}", h.auth()))
            .await;
        assert_eq!(&r.body[..4], b"RIFF", "query {q:?}");
    }
    assert!(!h.data.path().join("cache/transcode").exists());
}

struct Mp3Info {
    rate: u32,
    channels: u16,
    seconds: f64,
    rms: f64,
}

fn inspect_mp3(bytes: &[u8]) -> Mp3Info {
    let mut decoder = rusty_mp3::Mp3Decoder::new();
    decoder.push(bytes);
    decoder.flush();
    let (mut rate, mut channels, mut frames, mut energy, mut count) = (0, 0, 0usize, 0.0f64, 0u64);
    while let Ok(frame) = decoder.next_frame() {
        rate = frame.sample_rate;
        channels = frame.channels;
        frames += frame.samples.len() / usize::from(frame.channels);
        for s in &frame.samples {
            energy += f64::from(*s) * f64::from(*s);
            count += 1;
        }
    }
    let trim = bytes
        .windows(4)
        .take(256)
        .position(|w| w == b"Info" || w == b"Xing")
        .map(|at| {
            let t = &bytes[at + 141..at + 144];
            let packed = u32::from_be_bytes([0, t[0], t[1], t[2]]) as usize;
            let (delay, padding) = (packed >> 12, packed & 0xFFF);
            let spf = if rate < 32_000 { 576 } else { 1152 };
            spf + delay + padding
        })
        .unwrap_or(0);
    Mp3Info {
        rate,
        channels,
        seconds: frames.saturating_sub(trim) as f64 / f64::from(rate.max(1)),
        rms: (energy / count.max(1) as f64).sqrt(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mp3_transcode_keeps_duration_across_sample_rates() {
    let h = Harness::new();
    for (i, (rate, ch)) in [
        (8000u32, 1u16),
        (22_050, 1),
        (44_100, 2),
        (48_000, 2),
        (88_200, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let id = upload_wav(
            &h,
            wav_bytes_spec(20 + i as u32, 2000, rate, ch),
            &format!("m{rate}"),
        )
        .await;
        let r = h
            .get(&format!(
                "/rest/stream?{}&id={id}&format=mp3&maxBitRate=128",
                h.auth()
            ))
            .await;
        assert_eq!(r.status, StatusCode::OK, "rate {rate}");
        assert_eq!(r.headers["content-type"], "audio/mpeg");
        let info = inspect_mp3(&r.body);
        let expected_rate = if rate == 88_200 { 44_100 } else { rate };
        assert_eq!(info.rate, expected_rate, "rate {rate}");
        assert_eq!(info.channels, ch);
        assert!(
            (info.seconds - 2.0).abs() < 0.01,
            "rate {rate}: {} s",
            info.seconds
        );
        assert!(
            info.rms > 0.05,
            "rate {rate}: decoded audio is silent ({})",
            info.rms
        );
    }
    let cached = walk(&h.data.path().join("cache/transcode"));
    assert_eq!(cached.len(), 5);
    assert!(cached.iter().all(|p| p.extension().unwrap() == "mp3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn mp3_bitrate_follows_max_bit_rate() {
    let h = Harness::new();
    let id = upload_wav(&h, wav_bytes_spec(30, 6000, 44_100, 2), "br").await;
    let small = h
        .get(&format!(
            "/rest/stream?{}&id={id}&format=mp3&maxBitRate=64",
            h.auth()
        ))
        .await;
    let big = h
        .get(&format!(
            "/rest/stream?{}&id={id}&format=mp3&maxBitRate=256",
            h.auth()
        ))
        .await;
    assert!(
        big.body.len() > small.body.len() * 2,
        "{} vs {}",
        big.body.len(),
        small.body.len()
    );
    let kbps = |bytes: &[u8], secs: f64| bytes.len() as f64 * 8.0 / secs / 1000.0;
    assert!(
        (kbps(&small.body, 6.0) - 64.0).abs() < 12.0,
        "{}",
        kbps(&small.body, 6.0)
    );
    assert!(
        (kbps(&big.body, 6.0) - 256.0).abs() < 30.0,
        "{}",
        kbps(&big.body, 6.0)
    );
}

fn decode_aac(bytes: Vec<u8>) -> (u32, usize, f64, f64) {
    use symphonia::core::audio::sample::Sample;
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::TrackType;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;

    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("aac");
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, Default::default(), Default::default())
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let params = track
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .clone();
    let (rate, channels) = (
        params.sample_rate.unwrap(),
        params.channels.as_ref().unwrap().count(),
    );
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .unwrap();
    let id = track.id;
    let (mut frames, mut energy, mut count) = (0usize, 0.0f64, 0u64);
    let mut samples: Vec<f32> = Vec::new();
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != id {
            continue;
        }
        if let Ok(buf) = decoder.decode(&packet) {
            samples.resize(buf.samples_interleaved(), f32::MID);
            buf.copy_to_slice_interleaved(&mut samples);
            frames += samples.len() / channels;
            for s in &samples {
                energy += f64::from(*s) * f64::from(*s);
                count += 1;
            }
        }
    }
    (
        rate,
        channels,
        frames as f64 / f64::from(rate),
        (energy / count.max(1) as f64).sqrt(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn aac_transcode_is_decodable_adts() {
    let h = Harness::new();
    for (i, (rate, ch)) in [(8000u32, 1u16), (44_100, 2), (48_000, 2), (88_200, 2)]
        .into_iter()
        .enumerate()
    {
        let id = upload_wav(
            &h,
            wav_bytes_spec(40 + i as u32, 2000, rate, ch),
            &format!("a{rate}"),
        )
        .await;
        let r = h
            .get(&format!(
                "/rest/stream?{}&id={id}&format=aac&maxBitRate=128",
                h.auth()
            ))
            .await;
        assert_eq!(r.status, StatusCode::OK, "rate {rate}");
        assert_eq!(r.headers["content-type"], "audio/aac");
        assert_eq!(r.body[0], 0xFF, "ADTS sync word");
        let (out_rate, channels, seconds, rms) = decode_aac(r.body.clone());
        let expected_rate = if rate == 88_200 { 44_100 } else { rate };
        assert_eq!(out_rate, expected_rate, "rate {rate}");
        assert_eq!(channels, usize::from(ch));
        assert!((seconds - 2.0).abs() < 0.2, "rate {rate}: {seconds} s");
        assert!(rms > 0.03, "rate {rate}: decoded audio is silent ({rms})");
    }
}
