// Tiny notification sound player. Uses WebAudio so we don't ship an asset.

let audioCtx = null;

function getCtx() {
    if (audioCtx) return audioCtx;
    try {
        audioCtx = new (window.AudioContext || window.webkitAudioContext)();
    } catch (_) {
        audioCtx = null;
    }
    return audioCtx;
}

/** Plays a short two-tone chirp. Silently no-ops if WebAudio is unavailable. */
export function playNotificationSound() {
    const ctx = getCtx();
    if (!ctx) return;
    if (ctx.state === "suspended") ctx.resume().catch(() => {});

    const now = ctx.currentTime;
    const tones = [
        { f: 880, t: 0.0, d: 0.09 },
        { f: 1320, t: 0.1, d: 0.12 },
    ];
    for (const { f, t, d } of tones) {
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.type = "sine";
        osc.frequency.value = f;
        gain.gain.setValueAtTime(0.0001, now + t);
        gain.gain.exponentialRampToValueAtTime(0.18, now + t + 0.01);
        gain.gain.exponentialRampToValueAtTime(0.0001, now + t + d);
        osc.connect(gain).connect(ctx.destination);
        osc.start(now + t);
        osc.stop(now + t + d + 0.02);
    }
}