//! Notification sound: a soft two-note chime (F5 → A5, a warm major third).
//!
//! Design goals:
//!   * Warm, not shrill — only fundamental + gentle 2nd/3rd harmonics.
//!   * Smoothstep attack (~25 ms) so there is no click.
//!   * Two-stage exponential decay (fast "ping" + slower "ring" tail).
//!   * Short linear release at the very end so the buffer never cuts off.
//!   * Peak ~0.42 so it sits comfortably under OS alert volume.
//!
//! API target: rodio 0.22.x (crate root: `Player`, `DeviceSinkBuilder`,
//! `ChannelCount`, `SampleRate`, `nz!`).

use std::num::NonZero;

use rodio::buffer::SamplesBuffer;
use rodio::{nz, ChannelCount, DeviceSinkBuilder, Player, SampleRate};

const SR: u32 = 44_100;
const PI: f32 = std::f32::consts::PI;

/// Plays the notification chime. Fire-and-forget; silently no-ops if no
/// audio device is available (e.g. headless CI).
pub fn play_notification_chirp() {
    std::thread::spawn(|| {
        let Ok(mut handle) = DeviceSinkBuilder::open_default_sink() else {
            return;
        };
        // Suppress rodio's "Dropping DeviceSink…" warning at shutdown.
        handle.log_on_drop(false);

        let player = Player::connect_new(handle.mixer());

        let samples = render_chime();
        let channels: ChannelCount = nz!(1);
        let rate: SampleRate = NonZero::new(SR).unwrap();
        let src = SamplesBuffer::new(channels, rate, samples);

        player.append(src);
        player.sleep_until_end();

        drop(handle);
    });
}

/// Total duration ≈ 600 ms.
fn render_chime() -> Vec<f32> {
    let total_n = (SR as f32 * 0.60) as usize;
    let mut buf = vec![0.0f32; total_n];

    // F5 (698.46 Hz) then A5 (880.00 Hz) — a warm major third.
    add_note(&mut buf, 0, 698.46, 0.40, 1.0);
    add_note(&mut buf, (SR as f32 * 0.10) as usize, 880.00, 0.50, 0.9);

    // Normalize to a comfortable peak.
    let peak = buf.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    if peak > 0.0 {
        let g = 0.42 / peak;
        for s in &mut buf {
            *s *= g;
        }
    }
    buf
}

/// Adds a warm bell tone into `out` at sample offset `start`.
fn add_note(out: &mut [f32], start: usize, freq: f32, dur_secs: f32, scale: f32) {
    if start >= out.len() {
        return;
    }
    let n = ((SR as f32 * dur_secs) as usize).min(out.len() - start);
    if n == 0 {
        return;
    }

    let attack_n = ((SR as f32 * 0.025) as usize).max(1).min(n);
    let release_n = ((SR as f32 * 0.050) as usize).max(1).min(n.saturating_sub(attack_n));

    // Two-stage decay: an initial "ping" that tapers into a lingering ring.
    let fast_decay = 8.0f32;
    let slow_decay = 1.6f32;
    let tail_mix = 0.22f32;

    for i in 0..n {
        let t = i as f32 / SR as f32;

        // Smoothstep attack.
        let attack = if i < attack_n {
            let x = i as f32 / attack_n as f32;
            x * x * (3.0 - 2.0 * x)
        } else {
            1.0
        };

        // Short linear fade at the very end so we don't clip mid-ring.
        let release = if i > n - release_n {
            (n - i) as f32 / release_n as f32
        } else {
            1.0
        };

        let env = attack
            * release
            * ((1.0 - tail_mix) * (-t * fast_decay).exp()
                + tail_mix * (-t * slow_decay).exp());

        // Warm timbre: fundamental plus gentle 2nd/3rd harmonics.
        let s = (2.0 * PI * freq * t).sin()
            + (2.0 * PI * freq * 2.0 * t).sin() * 0.14
            + (2.0 * PI * freq * 3.0 * t).sin() * 0.04;

        out[start + i] += s * env * scale;
    }
}