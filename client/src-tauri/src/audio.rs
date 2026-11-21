//! Native audio-message playback via rodio.
//!
//! Why: playing audio inside the webview makes the OS volume mixer show a
//! separate "WebView" entry. Routing playback through rodio keeps all sound
//! in the app's own audio session (same as the notification chime).
//!
//! Performance model (the old first-play ~10 s stall is gone):
//!   * Commands receive only `(peer, id)` — Rust reads the payload from the
//!     local DB and base64-decodes it HERE. No ~21 MiB string crosses the
//!     Tauri IPC boundary, and the webview never touches audio bytes.
//!   * Decoded bytes are cached (LRU, 64 MiB cap) so replays / probes skip
//!     the DB read + JSON parse + base64 decode entirely.
//!   * Playback starts IMMEDIATELY. Duration comes from the container
//!     header, a caller hint, or — for headerless CBR MP3 — a BACKGROUND
//!     decode-count that emits `audio-duration` when finished. The old code
//!     decoded the entire file synchronously before the first sample.
//!
//! Events pushed to the webview:
//!   * `audio-state`    { peer, id, state, position, duration, message? }
//!   * `audio-progress` { peer, id, position, duration }
//!   * `audio-duration` { peer, id, duration }
//!
//! Only one message plays at a time. Backward seeks rebuild the source at
//! the exact offset (rodio's in-place backward seek is unreliable); forward
//! seeks use in-place `try_seek` with a rebuild fallback. Position is
//! tracked here (base + elapsed clock) rather than via `Player::get_pos()`.
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use serde_json::json;
use tauri::{AppHandle, Emitter};

/// Progress tick / command poll interval.
const TICK: Duration = Duration::from_millis(200);

/// Total decoded-bytes cache budget. Attachments are capped at 16 MiB, so
/// this holds the few most recently played/probed tracks.
const BYTES_CACHE_CAP: usize = 64 * 1024 * 1024;

/// Playback is keyed by (peer, message id) so a malicious peer reusing
/// another chat's message id can't collide with an active player.
type Key = (i64, String);

enum Cmd {
    Play {
        key: Key,
        bytes: Arc<Vec<u8>>,
        volume: f32,
        start: Option<f64>,
        duration_hint: Option<f64>,
    },
    Probe {
        key: Key,
        bytes: Arc<Vec<u8>>,
    },
    Pause {
        key: Key,
    },
    Resume {
        key: Key,
    },
    Seek {
        key: Key,
        position: f64,
    },
    Volume {
        key: Key,
        volume: f32,
    },
    Stop {
        key: Key,
    },
    StopAll,
    /// Result of a background duration scan.
    DurationReady {
        key: Key,
        duration: f64,
    },
}

struct Track {
    player: Player,
    /// Original encoded bytes, kept so a backward seek can rebuild the
    /// source at an exact offset. Shared with the bytes cache (Arc).
    bytes: Arc<Vec<u8>>,
    volume: f32,
    duration: f64,
    paused: bool,
    base_pos: f64,
    clock: Instant,
    paused_pos: f64,
}

impl Track {
    fn position(&self) -> f64 {
        if self.paused {
            return self.paused_pos;
        }
        let pos = self.base_pos + self.clock.elapsed().as_secs_f64();
        if self.duration > 0.0 {
            pos.min(self.duration)
        } else {
            pos
        }
    }
}

// ---------- decoded-bytes cache ----------

struct BytesCache {
    map: HashMap<Key, Arc<Vec<u8>>>,
    order: VecDeque<Key>,
    total: usize,
}

impl BytesCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            total: 0,
        }
    }

    fn get(&mut self, key: &Key) -> Option<Arc<Vec<u8>>> {
        let v = self.map.get(key).cloned();
        if v.is_some() {
            self.order.retain(|k| k != key);
            self.order.push_back(key.clone());
        }
        v
    }

    fn put(&mut self, key: Key, bytes: Arc<Vec<u8>>) {
        if let Some(old) = self.map.remove(&key) {
            self.total = self.total.saturating_sub(old.len());
            self.order.retain(|k| k != &key);
        }
        self.total += bytes.len();
        self.map.insert(key.clone(), bytes);
        self.order.push_back(key);
        while self.total > BYTES_CACHE_CAP && self.order.len() > 1 {
            if let Some(evict) = self.order.pop_front() {
                if let Some(b) = self.map.remove(&evict) {
                    self.total = self.total.saturating_sub(b.len());
                }
            } else {
                break;
            }
        }
    }
}

/// Handle used by Tauri commands to talk to the audio thread.
#[derive(Clone)]
pub struct AudioManager {
    tx: Sender<Cmd>,
    cache: Arc<Mutex<BytesCache>>,
}

impl AudioManager {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let thread_tx = tx.clone();
        std::thread::Builder::new()
            .name("audio-player".into())
            .spawn(move || run(app, rx, thread_tx))
            .expect("failed to spawn audio thread");
        Self {
            tx,
            cache: Arc::new(Mutex::new(BytesCache::new())),
        }
    }

    fn send(&self, cmd: Cmd) -> Result<(), String> {
        self.tx
            .send(cmd)
            .map_err(|_| "audio thread is gone".to_string())
    }

    /// Cached decoded bytes for a message, if present (skips DB + base64).
    pub fn cached_bytes(&self, peer: i64, id: &str) -> Option<Arc<Vec<u8>>> {
        self.cache.lock().ok()?.get(&(peer, id.to_string()))
    }

    pub fn cache_bytes(&self, peer: i64, id: &str, bytes: Arc<Vec<u8>>) {
        if let Ok(mut c) = self.cache.lock() {
            c.put((peer, id.to_string()), bytes);
        }
    }

    pub fn play(
        &self,
        peer: i64,
        id: String,
        bytes: Arc<Vec<u8>>,
        volume: f32,
        start: Option<f64>,
        duration_hint: Option<f64>,
    ) -> Result<(), String> {
        self.send(Cmd::Play {
            key: (peer, id),
            bytes,
            volume,
            start,
            duration_hint,
        })
    }

    pub fn probe(&self, peer: i64, id: String, bytes: Arc<Vec<u8>>) -> Result<(), String> {
        self.send(Cmd::Probe {
            key: (peer, id),
            bytes,
        })
    }

    pub fn pause(&self, peer: i64, id: String) -> Result<(), String> {
        self.send(Cmd::Pause { key: (peer, id) })
    }

    pub fn resume(&self, peer: i64, id: String) -> Result<(), String> {
        self.send(Cmd::Resume { key: (peer, id) })
    }

    pub fn seek(&self, peer: i64, id: String, position: f64) -> Result<(), String> {
        self.send(Cmd::Seek {
            key: (peer, id),
            position,
        })
    }

    pub fn set_volume(&self, peer: i64, id: String, volume: f32) -> Result<(), String> {
        self.send(Cmd::Volume {
            key: (peer, id),
            volume,
        })
    }

    pub fn stop(&self, peer: i64, id: String) -> Result<(), String> {
        self.send(Cmd::Stop { key: (peer, id) })
    }

    pub fn stop_all(&self) -> Result<(), String> {
        self.send(Cmd::StopAll)
    }
}

// ---------- events ----------

fn emit_state(
    app: &AppHandle,
    key: &Key,
    state: &str,
    position: f64,
    duration: f64,
    message: Option<&str>,
) {
    let _ = app.emit(
        "audio-state",
        json!({
            "peer": key.0, "id": key.1, "state": state,
            "position": position, "duration": duration, "message": message,
        }),
    );
}

fn emit_progress(app: &AppHandle, key: &Key, position: f64, duration: f64) {
    let _ = app.emit(
        "audio-progress",
        json!({ "peer": key.0, "id": key.1, "position": position, "duration": duration }),
    );
}

fn emit_duration(app: &AppHandle, key: &Key, duration: f64) {
    let _ = app.emit(
        "audio-duration",
        json!({ "peer": key.0, "id": key.1, "duration": duration }),
    );
}

// ---------- helpers ----------

fn sane_volume(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn sane_pos(p: f64) -> f64 {
    if p.is_finite() {
        p.max(0.0)
    } else {
        0.0
    }
}

/// Positions a FRESH decoder at `target` seconds before it is appended.
/// Prefers a container seek; otherwise decodes and discards samples.
fn seek_decoder(decoder: &mut Decoder<Cursor<Vec<u8>>>, target: f64) {
    if target <= 0.0 {
        return;
    }
    if decoder.try_seek(Duration::from_secs_f64(target)).is_ok() {
        return;
    }
    let rate = decoder.sample_rate().get() as f64;
    let channels = decoder.channels().get() as f64;
    let skip = (target * rate * channels) as usize;
    let _ = decoder.by_ref().take(skip).count();
}

/// Replaces the track's player with a fresh one decoding from `target`.
/// Reliable BACKWARD seek; preserves volume and paused state.
fn rebuild_at(out: &MixerDeviceSink, track: &mut Track, target: f64) {
    let Ok(mut decoder) = Decoder::new(Cursor::new(track.bytes.as_ref().clone())) else {
        return;
    };
    seek_decoder(&mut decoder, target);
    let player = Player::connect_new(out.mixer());
    player.set_volume(track.volume);
    if track.paused {
        player.pause();
    }
    player.append(decoder);
    track.player = player;
    track.base_pos = target;
    track.clock = Instant::now();
    track.paused_pos = target;
}

/// Full-file decode-count, ONLY ever run on a background thread. This is
/// the operation that used to block first playback for ~10 s on long MP3s.
fn spawn_duration_scan(tx: Sender<Cmd>, key: Key, bytes: Arc<Vec<u8>>) {
    std::thread::spawn(move || {
        let duration = Decoder::new(Cursor::new(bytes.as_ref().clone()))
            .ok()
            .map(|dec| {
                let channels = dec.channels().get() as f64;
                let rate = dec.sample_rate().get() as f64;
                let frames = dec.count() as f64 / channels;
                frames / rate
            })
            .unwrap_or(0.0);
        let _ = tx.send(Cmd::DurationReady { key, duration });
    });
}

// ---------- audio thread ----------

struct Runtime {
    app: AppHandle,
    tx: Sender<Cmd>,
    sink: Option<MixerDeviceSink>,
    tracks: HashMap<Key, Track>,
    /// Known durations (0 = "scanned, unknown"). Survives track stop so
    /// re-opening a chat never re-scans.
    durations: HashMap<Key, f64>,
    probing: HashSet<Key>,
}

impl Runtime {
    fn ensure_sink(&mut self, key: &Key) -> bool {
        if self.sink.is_some() {
            return true;
        }
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut s) => {
                s.log_on_drop(false);
                self.sink = Some(s);
                true
            }
            Err(e) => {
                emit_state(
                    &self.app,
                    key,
                    "error",
                    0.0,
                    0.0,
                    Some(&format!("no audio device: {e}")),
                );
                false
            }
        }
    }

    fn start_scan(&mut self, key: &Key, bytes: &Arc<Vec<u8>>) {
        if self.probing.insert(key.clone()) {
            spawn_duration_scan(self.tx.clone(), key.clone(), bytes.clone());
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play {
                key,
                bytes,
                volume,
                start,
                duration_hint,
            } => {
                // Restart semantics: drop any previous player for this message.
                self.tracks.remove(&key);

                let mut decoder = match Decoder::new(Cursor::new(bytes.as_ref().clone())) {
                    Ok(d) => d,
                    Err(e) => {
                        emit_state(
                            &self.app,
                            &key,
                            "error",
                            0.0,
                            0.0,
                            Some(&format!("unsupported or corrupt audio: {e}")),
                        );
                        return;
                    }
                };

                // Duration: cache > caller hint > container header. NEVER
                // block playback on a full-file scan — background it.
                //
                // BORROW ORDER: this block must run BEFORE `self.sink` is
                // borrowed below. `start_scan` takes `&mut self` (whole
                // self), which would conflict with the immutable borrow
                // held by `out` until `out.mixer()` (E0502). Field-only
                // accesses afterwards are fine: they're disjoint fields.
                let duration = match self.durations.get(&key) {
                    Some(d) => *d,
                    None => {
                        let hint = duration_hint.filter(|h| h.is_finite() && *h > 0.0);
                        let d = hint
                            .or_else(|| decoder.total_duration().map(|d| d.as_secs_f64()))
                            .unwrap_or(0.0);
                        if d > 0.0 {
                            self.durations.insert(key.clone(), d);
                        } else {
                            self.start_scan(&key, &bytes);
                        }
                        d
                    }
                };

                if !self.ensure_sink(&key) {
                    return;
                }
                let out = self.sink.as_ref().expect("sink just opened");

                let mut start = start.map(sane_pos).unwrap_or(0.0);
                if duration > 0.0 {
                    start = start.min(duration);
                }
                seek_decoder(&mut decoder, start);

                let volume = sane_volume(volume);
                let player = Player::connect_new(out.mixer());
                player.set_volume(volume);
                player.append(decoder);

                // Only one message audible at a time.
                for (k, t) in self.tracks.iter_mut() {
                    if !t.paused {
                        let pos = t.position();
                        t.player.pause();
                        t.paused = true;
                        t.paused_pos = pos;
                        emit_state(&self.app, k, "paused", pos, t.duration, None);
                    }
                }

                self.tracks.insert(
                    key.clone(),
                    Track {
                        player,
                        bytes,
                        volume,
                        duration,
                        paused: false,
                        base_pos: start,
                        clock: Instant::now(),
                        paused_pos: start,
                    },
                );
                emit_state(&self.app, &key, "playing", start, duration, None);
            }

            Cmd::Probe { key, bytes } => {
                if let Some(d) = self.durations.get(&key) {
                    emit_duration(&self.app, &key, *d);
                    return;
                }
                match Decoder::new(Cursor::new(bytes.as_ref().clone())) {
                    Ok(dec) => {
                        if let Some(td) = dec.total_duration() {
                            let d = td.as_secs_f64();
                            self.durations.insert(key.clone(), d);
                            emit_duration(&self.app, &key, d);
                        } else {
                            self.start_scan(&key, &bytes);
                        }
                    }
                    Err(_) => {
                        self.durations.insert(key.clone(), 0.0);
                        emit_duration(&self.app, &key, 0.0);
                    }
                }
            }

            Cmd::Pause { key } => {
                if let Some(t) = self.tracks.get_mut(&key) {
                    if !t.paused {
                        let pos = t.position();
                        t.player.pause();
                        t.paused = true;
                        t.paused_pos = pos;
                        emit_state(&self.app, &key, "paused", pos, t.duration, None);
                    }
                }
            }

            Cmd::Resume { key } => {
                if let Some(t) = self.tracks.get_mut(&key) {
                    if t.paused {
                        t.player.play();
                        t.paused = false;
                        t.base_pos = t.paused_pos;
                        t.clock = Instant::now();
                        emit_state(&self.app, &key, "playing", t.base_pos, t.duration, None);
                    }
                }
            }

            Cmd::Seek { key, position } => {
                let Some(t) = self.tracks.get_mut(&key) else {
                    return;
                };
                let target = sane_pos(position).min(t.duration.max(0.0));
                if target < t.position() {
                    if let Some(out) = self.sink.as_ref() {
                        rebuild_at(out, t, target);
                    }
                } else if t.player.try_seek(Duration::from_secs_f64(target)).is_ok() {
                    t.base_pos = target;
                    t.clock = Instant::now();
                    t.paused_pos = target;
                } else if let Some(out) = self.sink.as_ref() {
                    rebuild_at(out, t, target);
                }
                emit_progress(&self.app, &key, t.position(), t.duration);
            }

            Cmd::Volume { key, volume } => {
                if let Some(t) = self.tracks.get_mut(&key) {
                    t.volume = sane_volume(volume);
                    t.player.set_volume(t.volume);
                }
            }

            Cmd::Stop { key } => {
                if let Some(t) = self.tracks.remove(&key) {
                    emit_state(&self.app, &key, "stopped", 0.0, t.duration, None);
                }
            }

            Cmd::StopAll => {
                for (key, t) in self.tracks.drain() {
                    emit_state(&self.app, &key, "stopped", 0.0, t.duration, None);
                }
            }

            Cmd::DurationReady { key, duration } => {
                self.probing.remove(&key);
                self.durations.insert(key.clone(), duration);
                if let Some(t) = self.tracks.get_mut(&key) {
                    if duration > 0.0 {
                        t.duration = duration;
                        emit_progress(&self.app, &key, t.position(), duration);
                    }
                }
                emit_duration(&self.app, &key, duration);
            }
        }
    }

    /// Emits progress for playing tracks and reaps finished ones.
    fn tick(&mut self) {
        let mut ended = Vec::new();
        for (key, t) in self.tracks.iter() {
            if t.paused {
                continue;
            }
            if t.player.empty() {
                ended.push(key.clone());
                continue;
            }
            emit_progress(&self.app, key, t.position(), t.duration);
        }
        for key in ended {
            if let Some(t) = self.tracks.remove(&key) {
                emit_state(&self.app, &key, "ended", t.duration, t.duration, None);
            }
        }
    }
}

fn run(app: AppHandle, rx: Receiver<Cmd>, tx: Sender<Cmd>) {
    let mut rt = Runtime {
        app,
        tx,
        sink: None,
        tracks: HashMap::new(),
        durations: HashMap::new(),
        probing: HashSet::new(),
    };
    loop {
        match rx.recv_timeout(TICK) {
            Ok(cmd) => rt.handle(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Drain bursts (seek/volume drags) so controls feel instant.
        while let Ok(cmd) = rx.try_recv() {
            rt.handle(cmd);
        }
        rt.tick();
    }
}
