//! Notification sound. Generates a short two-tone chirp procedurally and
//! plays it through the default audio device. Spawns a thread so it never
//! blocks the async runtime.

use std::time::Duration;

use rodio::source::{SineWave, Source};
use rodio::{OutputStream, Sink};

/// Plays the notification chirp. Fire-and-forget: silently no-ops if no
/// audio device is available (e.g. headless CI).
pub fn play_notification_chirp() {
    std::thread::spawn(|| {
        let Ok((_stream, handle)) = OutputStream::try_default() else {
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            return;
        };
        let t1 = SineWave::new(880.0)
            .take_duration(Duration::from_millis(90))
            .amplify(0.18);
        let t2 = SineWave::new(1320.0)
            .take_duration(Duration::from_millis(130))
            .amplify(0.18);
        sink.append(t1);
        sink.append(t2);
        sink.sleep_until_end();
    });
}