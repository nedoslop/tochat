// Native audio-message playback controller.
//
// Playback AND metadata probing happen in Rust via rodio (src-tauri/src/
// audio.rs). The webview never creates a media element and never ships
// audio bytes over IPC — commands take only (peer, id); Rust reads the
// payload from the local DB, decodes it, caches the bytes, and starts
// playback immediately. Duration for headerless files arrives later via
// `audio-duration` (background scan), never blocking first play.
//
// This module keeps per-message UI state (volume/duration survive chat
// switches and message-list re-renders) and paints the custom player.
import { invoke, listen } from "./api.js";
import { state } from "./state.js";
import { toast, formatDuration, cssEscape, parseFilePayload } from "./utils.js";

// "peer:id" -> { volume, duration, position, playing, active, probing, seekGuard }
const players = new Map();
const keyOf = (peer, id) => `${peer}:${id}`;

function ensureState(peer, id) {
  const k = keyOf(peer, id);
  let st = players.get(k);
  if (!st) {
    st = {
      volume: 1,
      duration: 0,
      position: 0,
      playing: false,
      active: false,
      probing: false,
      seekGuard: null,
    };
    players.set(k, st);
  }
  return st;
}

// ---------- commands ----------

async function play(peer, id, start = null) {
  const st = ensureState(peer, id);
  try {
    await invoke("audio_play", {
      peer,
      id,
      volume: st.volume,
      position: start,
      // Hint from a previous probe: lets Rust skip its own duration work.
      durationHint: st.duration > 0 ? st.duration : null,
    });
  } catch (e) {
    toast("Audio error: " + e);
  }
}

export async function toggleAudio(peer, id, dataUrl) {
  if (!/^data:/i.test(dataUrl)) {
    return toast("Cannot play audio — encryption key mismatch?");
  }
  const st = ensureState(peer, id);
  try {
    if (st.active && st.playing) await invoke("audio_pause", { peer, id });
    else if (st.active) await invoke("audio_resume", { peer, id });
    else await play(peer, id);
  } catch (e) {
    toast("Audio error: " + e);
  }
}

async function seekOrStart(peer, id, dataUrl, pct) {
  if (!/^data:/i.test(dataUrl)) {
    return toast("Cannot play audio — encryption key mismatch?");
  }
  const st = ensureState(peer, id);
  if (!st.duration) return; // unknown until probe / background scan finishes
  const target = pct * st.duration;

  if (st.active) {
    st.position = target;
    st.seekGuard = { target, until: Date.now() + 350 };
    refreshDom(peer, id);
    try {
      await invoke("audio_seek", { peer, id, position: target });
    } catch (e) {
      st.seekGuard = null;
      toast("Audio error: " + e);
    }
  } else {
    await play(peer, id, target);
  }
}

async function setVolume(peer, id, v) {
  const st = ensureState(peer, id);
  st.volume = v;
  if (st.active) {
    try {
      await invoke("audio_set_volume", { peer, id, volume: v });
    } catch (_) {
      /* player may have just ended */
    }
  }
}

/** Stops one message's playback (e.g. the message was deleted). */
export async function stopAudio(peer, id) {
  players.delete(keyOf(peer, id));
  try {
    await invoke("audio_stop", { peer, id });
  } catch (_) {}
}

/**
 * Stops everything (chat switch, logout…) but KEEPS volume + duration per
 * message, so re-opening a chat never re-probes. (The old `players.clear()`
 * forced a fresh metadata load of every audio message on every chat open.)
 */
export async function stopAllAudio() {
  for (const st of players.values()) {
    st.active = false;
    st.playing = false;
    st.position = 0;
    st.seekGuard = null;
  }
  try {
    await invoke("audio_stop_all");
  } catch (_) {}
}

// ---------- DOM sync ----------

function paint(st, btn, fill, time) {
  if (btn) {
    btn.textContent = st.playing ? "⏸" : "▶";
    btn.setAttribute("aria-label", st.playing ? "Pause" : "Play");
  }
  if (fill) {
    const pct =
      st.duration > 0
        ? Math.max(0, Math.min(100, (st.position / st.duration) * 100))
        : 0;
    fill.style.width = pct + "%";
  }
  if (time) {
    time.textContent = formatDuration(st.active ? st.position : st.duration);
  }
}

function refreshDom(peer, id) {
  if (state.currentPeer !== peer) return;
  const root = document.querySelector(
    `#messages .msg[data-id="${cssEscape(id)}"] .audio-player`,
  );
  if (!root) return;
  const st = players.get(keyOf(peer, id));
  if (!st) return;
  paint(
    st,
    root.querySelector(".audio-play-btn"),
    root.querySelector(".audio-progress-fill"),
    root.querySelector(".audio-time"),
  );
}

// ---------- events ----------

export async function setupAudioEvents() {
  await listen("audio-duration", (e) => {
    const { peer, id, duration } = e.payload;
    const st = players.get(keyOf(peer, id));
    if (!st) return;
    st.probing = false;
    if (typeof duration === "number" && duration > 0 && !st.duration) {
      st.duration = duration;
    }
    refreshDom(peer, id);
  });

  await listen("audio-state", (e) => {
    const { peer, id, state: s, position, duration, message } = e.payload;
    const st = players.get(keyOf(peer, id));
    if (s === "error") {
      if (st) {
        st.active = false;
        st.playing = false;
        st.probing = false;
        st.seekGuard = null;
      }
      refreshDom(peer, id);
      if (message) toast("Audio: " + message);
      return;
    }
    if (!st) return; // stale event after stopAudio()
    st.seekGuard = null; // state events are authoritative
    if (typeof duration === "number" && duration > 0) st.duration = duration;
    if (typeof position === "number") st.position = position;
    switch (s) {
      case "playing":
        st.active = true;
        st.playing = true;
        break;
      case "paused":
        st.playing = false;
        break;
      case "ended":
      case "stopped":
        st.active = false;
        st.playing = false;
        st.position = 0;
        break;
    }
    refreshDom(peer, id);
  });

  await listen("audio-progress", (e) => {
    const { peer, id, position, duration } = e.payload;
    const st = players.get(keyOf(peer, id));
    if (!st || !st.active) return;
    if (st.seekGuard) {
      if (Date.now() > st.seekGuard.until) {
        st.seekGuard = null;
      } else if (position < st.seekGuard.target - 0.3) {
        // Tick emitted before the seek was processed — ignore it.
        return;
      }
    }
    st.position = position;
    if (duration > 0) st.duration = duration;
    refreshDom(peer, id);
  });
}

// ---------- player element ----------

/** Builds the custom audio player UI for an audio message. */
export function buildAudioPlayer(m, peer) {
  const info = parseFilePayload(m.payload);
  const dataUrl = info.data;
  const st = ensureState(peer, m.id);

  const wrap = document.createElement("div");
  wrap.className = "audio-player";

  const playBtn = document.createElement("button");
  playBtn.type = "button";
  playBtn.className = "audio-play-btn";

  const track = document.createElement("div");
  track.className = "audio-track";
  const fill = document.createElement("div");
  fill.className = "audio-progress-fill";
  track.appendChild(fill);

  const time = document.createElement("span");
  time.className = "audio-time";

  const vol = document.createElement("input");
  vol.type = "range";
  vol.min = "0";
  vol.max = "1";
  vol.step = "0.01";
  vol.value = String(st.volume);
  vol.className = "audio-volume";
  vol.title = "Volume";
  vol.setAttribute("aria-label", "Volume");

  paint(st, playBtn, fill, time);

  playBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    void toggleAudio(peer, m.id, dataUrl);
  });

  track.addEventListener("click", (e) => {
    e.stopPropagation();
    const rect = track.getBoundingClientRect();
    const pct = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    void seekOrStart(peer, m.id, dataUrl, pct);
  });

  vol.addEventListener("input", () => {
    void setVolume(peer, m.id, Number(vol.value));
  });

  // Duration probe runs in RUST (header read; background scan for
  // headerless files). No webview <audio> element is ever created — the
  // old probe loaded the entire multi-MiB data URL into the media
  // pipeline, which was a big part of the first-play stall.
  if (!st.duration && !st.probing && /^data:audio\//i.test(dataUrl)) {
    st.probing = true;
    invoke("audio_probe", { peer, id: m.id }).catch(() => {
      st.probing = false;
    });
  }

  wrap.appendChild(playBtn);
  wrap.appendChild(track);
  wrap.appendChild(time);
  wrap.appendChild(vol);
  return wrap;
}