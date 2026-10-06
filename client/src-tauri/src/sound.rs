//! Notification sound — a soft three-tone ascending chime with an ADSR-ish
//! envelope. Rendered as a single mono SamplesBuffer at 44.1 kHz and played
//! through the default output device on a background thread.

use std::time::Duration;

use rodio::buffer::SamplesBuffer;
use rodio::source::Source;
use rodio::{OutputStream, Sink};

const SR: u32 = 44_100;

pub fn play_notification_chirp() {
    std::thread::spawn(|| {
        let Ok((_stream, handle)) = OutputStream::try_default() else { return; };
        let Ok(sink) = Sink::try_new(&handle) else { return; };

        let samples = render_chime();
        let src = SamplesBuffer::new(1, SR, samples);
        sink.append(src);
        sink.sleep_until_end();
    });
}

/// Two pairs of notes: a soft two-note "ping" (E5 -> B5) followed by a
/// quieter echo (E5 -> B5) with reduced amplitude. Total ≈ 420 ms.
fn render_chime() -> Vec<f32> {
    let mut out = Vec::with_capacity((SR as f32 * 0.5) as usize);
    let groups: [(f32, f32, f32); 4] = [
        (659.25, 0.09, 1.0),   // E5
        (987.77, 0.14, 0.9),   // B5
        (659.25, 0.06, 0.45),  // E5 echo
        (987.77, 0.13, 0.4),   // B5 echo
    ];
    for (freq, dur, amp) in groups {
        push_tone(&mut out, freq, dur, amp);
    }
    out
}

fn push_tone(out: &mut Vec<f32>, freq: f32, dur_secs: f32, amp: f32) {
    let n = (SR as f32 * dur_secs) as usize;
    let attack = (n as f32 * 0.12).max(1.0) as usize;
    let release = (n as f32 * 0.55).max(1.0) as usize;
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let s = (2.0 * std::f32::consts::PI * freq * t).sin();
        let env = if i < attack {
            i as f32 / attack as f32
        } else if i > n.saturating_sub(release) {
            (n - i) as f32 / release as f32
        } else {
            1.0
        };
        out.push(s * env * amp * 0.22);
    }
}

/// Keeps rodio's Source trait in scope (SamplesBuffer implements it; the
/// import silences an unused-import warning when the module is trimmed).
#[allow(dead_code)]
fn _assert_source<S: Source<Item = f32>>() {}

#[allow(dead_code)]
fn _duration_hint() -> Duration { Duration::from_millis(1) }