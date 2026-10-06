//! Notification sound.
//!
//! Synthesizes a two-note bell-like chime (C6 → E6) with four harmonic
//! partials and an exponential decay envelope, then plays it through the
//! default output device on a background thread.
//!
//! API target: rodio 0.22.x. Compared to 0.21, the crate was renamed:
//!   * `OutputStream`         → `MixerDeviceSink`
//!   * `OutputStreamBuilder`  → `DeviceSinkBuilder`
//!   * `open_default_stream`  → `open_default_sink`
//!   * `Sink`                 → `Player`
//!   * `Sink::connect_new`    → `Player::connect_new`
//!   * `SamplesBuffer::new` now takes `NonZero<u16>` / `NonZero<u32>`
//!     (aliased as `ChannelCount` / `SampleRate`); the `nz!` macro builds
//!     them from integer literals.

use std::num::NonZero;

use rodio::buffer::SamplesBuffer;
use rodio::{nz, ChannelCount, DeviceSinkBuilder, Player, SampleRate};

const SR: u32 = 44_100;
const PI: f32 = std::f32::consts::PI;

/// Plays the notification chime. Fire-and-forget; silently no-ops if no
/// audio device is available (e.g. headless CI).
pub fn play_notification_chirp() {
    std::thread::spawn(|| {
        // OS-sink handle to the default physical audio device.
        let Ok(handle) = DeviceSinkBuilder::open_default_sink() else {
            return;
        };

        // A Player is the renamed Sink: it queues sources and plays them
        // sequentially. It borrows the mixer, not the handle, so `handle`
        // just has to stay alive until we're done.
        let player = Player::connect_new(handle.mixer());

        let samples = render_chime();
        let channels: ChannelCount = nz!(1);
        let rate: SampleRate = NonZero::new(SR).unwrap();
        let src = SamplesBuffer::new(channels, rate, samples);

        player.append(src);
        player.sleep_until_end();

        // Dropping the OS-sink stops all playback. By now the chime has
        // finished, so this is safe.
        drop(handle);
    });
}

/// Total duration ≈ 580 ms.
fn render_chime() -> Vec<f32> {
    let n1 = (SR as f32 * 0.35) as usize;
    let offset = (SR as f32 * 0.13) as usize;
    let n2 = (SR as f32 * 0.45) as usize;
    let mut out = vec![0.0f32; offset + n2];

    // First note: C6 (1046.5 Hz).
    add_bell(&mut out[..n1], 1046.5, 0.45);

    // Second note: E6 (1318.5 Hz) — a major third up.
    add_bell(&mut out[offset..offset + n2], 1318.5, 0.40);

    // Soft clip so loud sections don't distort.
    for s in &mut out {
        *s = s.clamp(-1.0, 1.0);
    }
    out
}

/// Adds a bell-like tone into `buf` (additively, so notes can overlap).
fn add_bell(buf: &mut [f32], freq: f32, amp: f32) {
    let n = buf.len();
    let attack_n = ((SR as f32 * 0.004) as usize).max(1);

    // (partial ratio, amplitude, decay speed)
    let partials: [(f32, f32, f32); 4] = [
        (1.00, 0.65, 6.0),
        (2.00, 0.22, 8.0),
        (2.76, 0.10, 15.0), // inharmonic bell partial
        (3.00, 0.06, 12.0),
    ];

    for i in 0..n {
        let t = i as f32 / SR as f32;
        let attack = if i < attack_n {
            i as f32 / attack_n as f32
        } else {
            1.0
        };

        let mut s = 0.0f32;
        for (ratio, pamp, decay) in partials {
            let env = attack * (-t * decay).exp();
            s += (2.0 * PI * freq * ratio * t).sin() * env * pamp;
        }
        buf[i] += s * amp;
    }
}