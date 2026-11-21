//! Native audio-message playback via rodio.
//!
//! Why: playing audio inside the webview makes the OS volume mixer show a
//! separate "WebView" entry. Routing playback through rodio keeps all sound
//! in the app's own audio session (same as the notification chime).
//!
//! Architecture: a single dedicated std thread owns the output sink (cpal
//! streams are !Send, so the sink must live and die on one thread) and every
//! active `Player`. Commands arrive over an unbounded std channel; state and
//! progress are pushed to the webview as Tauri events:
//!
//!   * `audio-state`    { peer, id, state, position, duration, message? }
//!       state: "playing" | "paused" | "ended" | "stopped" | "error"
//!   * `audio-progress` { peer, id, position, duration }
//!
//! Only one message plays at a time: starting playback auto-pauses the rest.
//!
//! Seeking: rodio's in-place BACKWARD seek is unreliable — the sink can keep
//! playing from (and reporting) the pre-seek position, while forward seeks
//! work. So:
//!   * backward seeks rebuild the source at the exact offset (fresh decoder,
//!     seeked before `append`);
//!   * forward seeks use in-place `try_seek`, falling back to a rebuild;
//!   * playback position is tracked HERE (base + elapsed clock, frozen while
//!     paused) instead of `Player::get_pos()`, which does not reliably rewind.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use serde_json::json;
use tauri::{AppHandle, Emitter};

/// Progress tick / command poll interval.
const TICK: Duration = Duration::from_millis(200);

/// Playback is keyed by (peer, message id) so a malicious peer reusing
/// another chat's message id can't collide with an active player.
type Key = (i64, String);

enum Cmd {
    Play {
        key: Key,
        bytes: Vec<u8>,
        volume: f32,
        start: Option<f64>,
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
}

struct Track {
    player: Player,
    /// Original encoded bytes, kept so a backward seek can rebuild the
    /// source at an exact offset. Freed when the track ends or is stopped.
    bytes: Vec<u8>,
    volume: f32,
    duration: f64,
    paused: bool,
    /// Position at the last (re)start / seek…
    base_pos: f64,
    /// …counted forward from this instant while playing.
    clock: Instant,
    /// Frozen position while paused.
    paused_pos: f64,
}

impl Track {
    /// Current playback position in seconds. Self-managed on purpose:
    /// rodio's `get_pos()` does not reliably move backwards after a seek.
    /// The wall clock matches real-time playback within the output buffer
    /// latency (tens of ms) — far below the 200 ms UI tick.
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

/// Handle used by Tauri commands to talk to the audio thread.
#[derive(Clone)]
pub struct AudioManager {
    tx: Sender<Cmd>,
}

impl AudioManager {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = channel::<Cmd>();
        std::thread::Builder::new()
            .name("audio-player".into())
            .spawn(move || run(app, rx))
            .expect("failed to spawn audio thread");
        Self { tx }
    }

    fn send(&self, cmd: Cmd) -> Result<(), String> {
        self.tx
            .send(cmd)
            .map_err(|_| "audio thread is gone".to_string())
    }

    pub fn play(
        &self,
        peer: i64,
        id: String,
        bytes: Vec<u8>,
        volume: f32,
        start: Option<f64>,
    ) -> Result<(), String> {
        self.send(Cmd::Play {
            key: (peer, id),
            bytes,
            volume,
            start,
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
            "peer": key.0,
            "id": key.1,
            "state": state,
            "position": position,
            "duration": duration,
            "message": message,
        }),
    );
}

fn emit_progress(app: &AppHandle, key: &Key, position: f64, duration: f64) {
    let _ = app.emit(
        "audio-progress",
        json!({
            "peer": key.0,
            "id": key.1,
            "position": position,
            "duration": duration,
        }),
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

/// Total duration in seconds. Most containers report it directly; for the
/// rest (e.g. headerless CBR MP3) we decode once more just to count samples
/// — O(1) memory, and only on the fallback path.
fn duration_secs(dec: &Decoder<Cursor<Vec<u8>>>, bytes: &[u8]) -> f64 {
    if let Some(d) = dec.total_duration() {
        return d.as_secs_f64();
    }
    if let Ok(d2) = Decoder::new(Cursor::new(bytes.to_vec())) {
        let frames = d2.count() as f64 / dec.channels().get() as f64;
        return frames / dec.sample_rate().get() as f64;
    }
    0.0
}

/// Positions a FRESH decoder at `target` seconds before it is appended to a
/// player. Prefers a container seek; if the format can't seek, decodes and
/// discards samples up to the target (rare fallback, O(target) CPU).
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
/// This is the reliable way to seek BACKWARDS: rodio's in-place backward
/// seek can leave the sink playing from the old position. Preserves volume
/// and paused state; dropping the old player stops its output.
fn rebuild_at(out: &MixerDeviceSink, track: &mut Track, target: f64) {
    let Ok(mut decoder) = Decoder::new(Cursor::new(track.bytes.clone())) else {
        // Same bytes decoded fine before; if that ever changes, keep the
        // old player rather than killing playback.
        return;
    };
    seek_decoder(&mut decoder, target);

    let player = Player::connect_new(out.mixer());
    player.set_volume(track.volume);
    if track.paused {
        // Pause before appending so the rebuilt source stays silent.
        player.pause();
    }
    player.append(decoder);

    track.player = player;
    track.base_pos = target;
    track.clock = Instant::now();
    track.paused_pos = target;
}

// ---------- audio thread ----------

fn run(app: AppHandle, rx: Receiver<Cmd>) {
    // Opened lazily on first playback; kept alive afterwards so we don't
    // pay device-open latency on every message.
    let mut sink: Option<MixerDeviceSink> = None;
    let mut tracks: HashMap<Key, Track> = HashMap::new();

    loop {
        match rx.recv_timeout(TICK) {
            Ok(cmd) => handle_cmd(&app, &mut sink, &mut tracks, cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Drain bursts (seek/volume drags) so controls feel instant.
        while let Ok(cmd) = rx.try_recv() {
            handle_cmd(&app, &mut sink, &mut tracks, cmd);
        }
        tick(&app, &mut tracks);
    }
    // Sender(s) dropped => app shutting down. Dropping the sink and players
    // stops everything; log_on_drop(false) keeps rodio quiet about it.
}

fn handle_cmd(
    app: &AppHandle,
    sink: &mut Option<MixerDeviceSink>,
    tracks: &mut HashMap<Key, Track>,
    cmd: Cmd,
) {
    match cmd {
        Cmd::Play {
            key,
            bytes,
            volume,
            start,
        } => {
            // Restart semantics: drop any previous player for this message.
            tracks.remove(&key);

            if sink.is_none() {
                match DeviceSinkBuilder::open_default_sink() {
                    Ok(mut s) => {
                        s.log_on_drop(false);
                        *sink = Some(s);
                    }
                    Err(e) => {
                        emit_state(
                            app,
                            &key,
                            "error",
                            0.0,
                            0.0,
                            Some(&format!("no audio device: {e}")),
                        );
                        return;
                    }
                }
            }
            let out = sink.as_ref().expect("sink just opened");

            // Keep `bytes` for backward-seek rebuilds; the decoder gets its
            // own copy.
            let mut decoder = match Decoder::new(Cursor::new(bytes.clone())) {
                Ok(d) => d,
                Err(e) => {
                    emit_state(
                        app,
                        &key,
                        "error",
                        0.0,
                        0.0,
                        Some(&format!("unsupported or corrupt audio: {e}")),
                    );
                    return;
                }
            };
            let duration = duration_secs(&decoder, &bytes);
            let start = start.map(sane_pos).unwrap_or(0.0).min(duration);
            // Seek the fresh decoder BEFORE appending — reliable in both
            // directions, unlike seeking a live sink backwards.
            seek_decoder(&mut decoder, start);

            let volume = sane_volume(volume);
            let player = Player::connect_new(out.mixer());
            player.set_volume(volume);
            player.append(decoder);

            // Only one message audible at a time: pause everything else.
            for (k, t) in tracks.iter_mut() {
                if !t.paused {
                    let pos = t.position();
                    t.player.pause();
                    t.paused = true;
                    t.paused_pos = pos;
                    emit_state(app, k, "paused", pos, t.duration, None);
                }
            }

            tracks.insert(
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
            emit_state(app, &key, "playing", start, duration, None);
        }

        Cmd::Pause { key } => {
            if let Some(t) = tracks.get_mut(&key) {
                if !t.paused {
                    let pos = t.position();
                    t.player.pause();
                    t.paused = true;
                    t.paused_pos = pos;
                    emit_state(app, &key, "paused", pos, t.duration, None);
                }
            }
        }

        Cmd::Resume { key } => {
            if let Some(t) = tracks.get_mut(&key) {
                if t.paused {
                    t.player.play();
                    t.paused = false;
                    t.base_pos = t.paused_pos;
                    t.clock = Instant::now();
                    emit_state(app, &key, "playing", t.base_pos, t.duration, None);
                }
            }
        }

        Cmd::Seek { key, position } => {
            let Some(t) = tracks.get_mut(&key) else {
                return;
            };
            let target = sane_pos(position).min(t.duration.max(0.0));
            if target < t.position() {
                // BACKWARD: rebuild the source at the target. In-place
                // backward seeks in rodio can leave the sink playing from
                // (and reporting) the old position.
                if let Some(out) = sink.as_ref() {
                    rebuild_at(out, t, target);
                }
            } else if t.player.try_seek(Duration::from_secs_f64(target)).is_ok() {
                // FORWARD: in-place seek works; re-base our own clock.
                t.base_pos = target;
                t.clock = Instant::now();
                t.paused_pos = target;
            } else if let Some(out) = sink.as_ref() {
                // In-place seek unsupported for this source: rebuild.
                rebuild_at(out, t, target);
            }
            emit_progress(app, &key, t.position(), t.duration);
        }

        Cmd::Volume { key, volume } => {
            if let Some(t) = tracks.get_mut(&key) {
                t.volume = sane_volume(volume);
                t.player.set_volume(t.volume);
            }
        }

        Cmd::Stop { key } => {
            if let Some(t) = tracks.remove(&key) {
                emit_state(app, &key, "stopped", 0.0, t.duration, None);
            }
        }

        Cmd::StopAll => {
            for (key, t) in tracks.drain() {
                emit_state(app, &key, "stopped", 0.0, t.duration, None);
            }
        }
    }
}

/// Emits progress for playing tracks and reaps finished ones.
fn tick(app: &AppHandle, tracks: &mut HashMap<Key, Track>) {
    let mut ended = Vec::new();
    for (key, t) in tracks.iter() {
        if t.paused {
            continue;
        }
        if t.player.empty() {
            ended.push(key.clone());
            continue;
        }
        emit_progress(app, key, t.position(), t.duration);
    }
    for key in ended {
        if let Some(t) = tracks.remove(&key) {
            emit_state(app, &key, "ended", t.duration, t.duration, None);
        }
    }
}
