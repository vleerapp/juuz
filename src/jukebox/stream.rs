use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::output::{Position, State};
use crate::pcm::{CHANNELS, PcmSource, RATE};

pub const CHUNK_MS: u64 = 20;
pub const CHUNK_FRAMES: usize = (RATE as u64 * CHUNK_MS / 1000) as usize;
const LATE_US: i64 = 200_000;

pub fn unix_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

pub fn now_us() -> i64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_micros() as i64 + 1_000_000
}

pub fn pcm_bytes(samples: &[f32], frames: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * CHANNELS * 2);
    for i in 0..frames * CHANNELS {
        let s = samples.get(i).copied().unwrap_or(0.0).clamp(-1.0, 1.0);
        out.extend_from_slice(&((s * 32767.0).round() as i16).to_le_bytes());
    }
    out
}

pub trait Sink: Send + Sync {
    fn push(&self, due_us: i64, pcm: &[u8]);
    fn flush(&self) {}
}

pub type SharedSink = Arc<dyn Sink>;

pub struct Fanout(pub Vec<SharedSink>);

impl Sink for Fanout {
    fn push(&self, due_us: i64, pcm: &[u8]) {
        for sink in &self.0 {
            sink.push(due_us, pcm);
        }
    }

    fn flush(&self) {
        match self.0.as_slice() {
            [] => {}
            [only] => only.flush(),
            many => std::thread::scope(|scope| {
                for sink in many {
                    scope.spawn(|| sink.flush());
                }
            }),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Idle,
    Playing,
    Paused,
    Ended,
}

struct Engine {
    mode: Mode,
    song: Option<String>,
    path: Option<PathBuf>,
    source: Option<PcmSource>,
    generation: u64,
    start_secs: u64,
    frames: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub unix_us: i64,
    pub pos: f64,
}

struct Shared {
    engine: Mutex<Engine>,
    clock: Mutex<Option<(u64, Clock)>>,
    wake: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Stream {
    shared: Arc<Shared>,
    lead_ms: u64,
    sink: SharedSink,
}

impl Stream {
    pub fn spawn(name: &str, lead_ms: u64, sink: SharedSink) -> Result<Stream> {
        let shared = Arc::new(Shared {
            engine: Mutex::new(Engine {
                mode: Mode::Idle,
                song: None,
                path: None,
                source: None,
                generation: 0,
                start_secs: 0,
                frames: 0,
            }),
            clock: Mutex::new(None),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = shared.clone();
        let emit = sink.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || run(worker, emit))?;
        Ok(Stream {
            shared,
            lead_ms,
            sink,
        })
    }

    pub fn play(&self, song: Option<String>, path: PathBuf, offset: u64) -> Result<()> {
        let source = PcmSource::open(&path, offset)?;
        let mut e = self.shared.lock();
        let interrupts = matches!(e.mode, Mode::Playing | Mode::Paused);
        if song.is_some() {
            e.song = song;
        }
        e.path = Some(path);
        e.source = Some(source);
        e.generation += 1;
        e.start_secs = offset;
        e.frames = 0;
        e.mode = Mode::Playing;
        self.shared.wake.notify_all();
        drop(e);
        if interrupts {
            self.sink.flush();
        }
        Ok(())
    }

    pub fn resume(&self) {
        let mut e = self.shared.lock();
        if e.mode == Mode::Paused {
            e.mode = Mode::Playing;
            self.shared.wake.notify_all();
        }
    }

    pub fn pause(&self) {
        let mut e = self.shared.lock();
        if e.mode == Mode::Playing {
            e.mode = Mode::Paused;
        }
    }

    pub fn stop(&self) {
        {
            let mut e = self.shared.lock();
            e.mode = Mode::Idle;
            e.source = None;
            e.song = None;
            e.generation += 1;
        }
        self.sink.flush();
    }

    pub fn seek(&self, secs: u64) -> Result<()> {
        let (song, path) = {
            let e = self.shared.lock();
            (e.song.clone(), e.path.clone())
        };
        let path = path.ok_or_else(|| anyhow!("nothing is loaded"))?;
        self.play(song, path, secs)
    }

    pub fn state(&self) -> State {
        match self.shared.lock().mode {
            Mode::Playing => State::Playing,
            Mode::Paused => State::Paused,
            Mode::Idle | Mode::Ended => State::Stopped,
        }
    }

    pub fn clock(&self) -> Option<Clock> {
        let (generation, playing) = {
            let e = self.shared.lock();
            (e.generation, e.mode == Mode::Playing)
        };
        if !playing {
            return None;
        }
        let held = *self.shared.clock.lock().unwrap_or_else(|e| e.into_inner());
        held.filter(|(g, _)| *g == generation).map(|(_, c)| c)
    }

    pub fn position(&self) -> Position {
        let e = self.shared.lock();
        let sent = e.start_secs * 1000 + e.frames * 1000 / u64::from(RATE);
        let heard = sent.saturating_sub(self.lead_ms);
        Position {
            song_id: e.song.clone(),
            seconds: Some(heard / 1000),
        }
    }

    pub fn close(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}

fn run(shared: Arc<Shared>, sink: SharedSink) {
    let mut timeline: Option<(i64, u64)> = None;
    let mut samples: Vec<f32> = Vec::with_capacity(CHUNK_FRAMES * CHANNELS);
    loop {
        let (mut source, generation) = {
            let mut e = shared.lock();
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    return;
                }
                if e.mode == Mode::Playing && e.source.is_some() {
                    break;
                }
                timeline = None;
                let (guard, _) = shared
                    .wake
                    .wait_timeout(e, Duration::from_millis(250))
                    .unwrap_or_else(|p| p.into_inner());
                e = guard;
            }
            (e.source.take().expect("source present"), e.generation)
        };
        samples.clear();
        let read = source.read(CHUNK_FRAMES, &mut samples);
        let (frames, finished) = match read {
            Ok(n) => (n, n < CHUNK_FRAMES && source.finished()),
            Err(e) => {
                tracing::warn!("stream decode failed: {e:#}");
                (0, true)
            }
        };
        {
            let mut e = shared.lock();
            if e.generation != generation || e.mode != Mode::Playing {
                timeline = None;
                continue;
            }
            e.source = Some(source);
        }
        if frames > 0 {
            let (t0, n) = timeline.get_or_insert((now_us(), 0));
            let mut due = *t0 + (*n as i64) * (CHUNK_MS as i64) * 1000;
            if now_us() > due + LATE_US {
                *t0 = now_us();
                *n = 0;
                due = *t0;
            }
            let wait = due - now_us();
            if *n == 0 {
                let pos = {
                    let e = shared.lock();
                    e.start_secs as f64 + e.frames as f64 / f64::from(RATE)
                };
                let clock = Clock {
                    unix_us: unix_us() + wait,
                    pos,
                };
                *shared.clock.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, clock));
            }
            if wait > 0 {
                std::thread::sleep(Duration::from_micros(wait as u64));
            }
            sink.push(due, &pcm_bytes(&samples, CHUNK_FRAMES));
            *n += 1;
            shared.lock().frames += frames as u64;
        }
        if finished {
            let mut e = shared.lock();
            if e.generation == generation {
                e.mode = Mode::Ended;
                e.source = None;
            }
            timeline = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_clamped_little_endian_i16() {
        let b = pcm_bytes(&[0.0, 1.0, -1.0, 2.0], 2);
        assert_eq!(b, vec![0, 0, 255, 127, 1, 128, 255, 127]);
    }
}
