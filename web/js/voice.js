"use strict";

/* ------------------------------------------------------------------ */
/* Phonon: in-browser neural text-to-speech with gapless streaming     */
/* ------------------------------------------------------------------ */

/** The voices on the picker, by name. */
const VOICES = ["Harper", "Toby", "Russell", "Freya"];

// Their files in gradium/phonon-voices, by voice id (the catalog's voices.jsonl lists them all).
const VOICE_FILES = {
  Toby: "dME3IWyZBvmh1n1q",
  Russell: "_6Aslh2DxfmnRLmP",
  Freya: "GgfEkEJtxZR7gnpy",
  Harper: "4SZHfMpw-p46Ywgs",
};

// Phonon 31M (Apache-2.0), straight from Hugging Face, which allows cross-origin requests.
// Each repo is pinned to a commit, so a cached file is never served after the repo changes.
// phonon-tts keeps the files in the browser's Cache API after the first visit, so it runs offline.
// The phonon-tts library itself is not fetched at all: npm owns it, Vite bundles it, and
// the gateway serves the bundle (with its worker and wasm builds) from /assets/phonon.js.
// These pinned commits are the same ones the Rust source uses for its
// default model and default voice catalog, so the in-browser voice and the
// built-in one agree on exactly the same files.
const MODEL_URL =
  "https://huggingface.co/gradium/phonon-31m-en/resolve/8c64b55ec301728e2d1695bfea87a08a267d0c2d";
const VOICES_URL =
  "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3";

const MODEL = {
  weights: { q8: `${MODEL_URL}/model.q8.gguf` },
  tokenizer: `${MODEL_URL}/tokenizer.json`,
  config: `${MODEL_URL}/config.json`,
  voices: Object.fromEntries(
    Object.entries(VOICE_FILES).map(([name, id]) => [
      name,
      `${VOICES_URL}/voices/${id}.safetensors`,
    ])
  ),
  defaultVoice: VOICES[0],
};

let currentVoice = VOICES[0];
let phononAudioCtx = null;
let phononTTSPromise = null;
let phononTTSInstance = null;
let activeSpeaker = null;
let phononModule = null;

function getVoice() {
  return currentVoice;
}

function setVoice(name) {
  if (!name) return null;
  const match = VOICES.find(
    (v) => v.toLowerCase() === String(name).trim().toLowerCase()
  );
  if (match) {
    currentVoice = match;
    if (activeSpeaker) {
      activeSpeaker.voice = match;
    }
    return match;
  }
  return null;
}

function voiceSourceDescription() {
  if (phononReady()) {
    return "Phonon (" + getVoice() + ")";
  }
  if (serverSpeech()) {
    return "the gateway's voice";
  }
  if (speechReady()) {
    return "the browser's voice";
  }
  return "no voice";
}

function phononReady() {
  return (
    typeof WebAssembly === "object" &&
    typeof Worker === "function" &&
    typeof (window.AudioContext || window.webkitAudioContext) === "function"
  );
}

function serverSpeech() {
  return !!state.voice.server;
}

function speechReady() {
  return phononReady() || serverSpeech() || "speechSynthesis" in window;
}

function getAudioContext() {
  if (!phononAudioCtx) {
    const AudioCtx = window.AudioContext || window.webkitAudioContext;
    if (AudioCtx) {
      phononAudioCtx = new AudioCtx();
    }
  }
  return phononAudioCtx;
}

// User interactions unlock AudioContext in modern browsers
if (typeof window !== "undefined") {
  ["click", "keydown", "touchstart"].forEach((evt) => {
    window.addEventListener(
      evt,
      () => {
        if (phononAudioCtx && phononAudioCtx.state === "suspended") {
          phononAudioCtx.resume().catch(() => {});
        }
      },
      { once: true, passive: true }
    );
  });
}

/** The speech library the gateway built and serves itself. */
function getPhononModule() {
  if (!phononModule) {
    phononModule = import("/assets/phonon.js").catch((e) => {
      phononModule = null;
      throw e;
    });
  }
  return phononModule;
}

async function loadPhonon(onProgress) {
  if (phononTTSInstance) return phononTTSInstance;
  if (phononTTSPromise) return phononTTSPromise;

  phononTTSPromise = (async () => {
    await getPhononModule();
    const PhononTTS = window.PhononTTS;
    if (!PhononTTS) {
      throw new Error("phonon-tts did not install its global");
    }
    const files = new Map();
    const tts = await PhononTTS.load({
      lang: "en",
      quant: "q8",
      device: "auto",
      model: MODEL,
      voices: [MODEL.defaultVoice],
      onProgress: ({ file, loaded, total }) => {
        files.set(file, { loaded, total: total || loaded });
        let l = 0,
          t = 0;
        for (const f of files.values()) {
          l += f.loaded;
          t += f.total;
        }
        if (typeof onProgress === "function") {
          onProgress(t ? l / t : 0, l);
        }
      },
    });
    phononTTSInstance = tts;
    return tts;
  })();

  try {
    return await phononTTSPromise;
  } catch (err) {
    phononTTSPromise = null;
    throw err;
  }
}

/**
 * Speaker class adapted from gradium-ai/examples/offline-dino-game/src/voice.js
 * Plays speech as a gapless stream of scheduled PCM audio buffers with immediate
 * start on the first frame and instant cancellation fade out.
 */
class Speaker {
  /** `out` is where Lia is heard: speakers or a gain bus. */
  constructor(tts, ctx, out = ctx.destination) {
    this.tts = tts;
    this.ctx = ctx;
    this.voice = currentVoice || VOICES[0];
    this.lines = [];
    this.playhead = 0;
    this.lastFirstAudioMs = null; // from asking for a line to its first sound being scheduled
    this.bus = ctx.createGain();
    this.bus.connect(out);
    this.newEpoch();
  }

  newEpoch() {
    this.gain = this.ctx.createGain();
    this.gain.connect(this.bus);
  }

  setMuted(muted) {
    this.bus.gain.setTargetAtTime(muted ? 0 : 1, this.ctx.currentTime, 0.02);
  }

  /** Seconds of audio scheduled past now. */
  ahead() {
    return Math.max(0, this.playhead - this.ctx.currentTime);
  }

  /** The line being heard right now, if any. */
  current() {
    const t = this.ctx.currentTime;
    return (
      this.lines.find((l) => !l.dropped && l.start <= t && t < l.end) || null
    );
  }

  /** Synthesizes `text` and schedules it after everything already queued. Resolves once the
   *  whole line is scheduled (well before it finishes playing), with the line, or null if aborted
   *  before any of it was scheduled. */
  async speak(text, signal) {
    if (this.ctx && this.ctx.state === "suspended") {
      await this.ctx.resume().catch(() => {});
    }
    const line = {
      text,
      start: Infinity,
      end: 0,
      sources: [],
      gain: this.gain,
      cut: false,
      dropped: false,
    };
    this.lines.push(line);
    this.lines = this.lines.slice(-40);
    const rate = this.tts.sampleRate;
    const asked = performance.now();
    try {
      for await (const pcm of this.tts.stream(text, {
        voice: this.voice,
        signal,
      })) {
        if (signal?.aborted || line.dropped || line.cut) break;
        if (!line.sources.length) {
          this.lastFirstAudioMs = performance.now() - asked;
        }
        const at = Math.max(this.playhead, this.ctx.currentTime + 0.06);
        const src = this.play(pcm, rate, at, line.gain);
        line.sources.push(src);
        line.start = Math.min(line.start, at);
        line.end = at + pcm.length / rate;
        this.playhead = line.end;
      }
    } catch (err) {
      if (!signal?.aborted) throw err;
    }
    if (!line.sources.length) {
      line.dropped = true;
      return null;
    }
    return line;
  }

  play(pcm, rate, at, gain) {
    const buf = this.ctx.createBuffer(1, pcm.length, rate);
    buf.getChannelData(0).set(pcm);
    const src = this.ctx.createBufferSource();
    src.buffer = buf;
    src.connect(gain);
    src.start(at);
    return src;
  }

  /** Lets the line being heard finish and drops every line queued after it. */
  dropQueued() {
    const t = this.ctx.currentTime;
    for (const l of this.lines) {
      if (l.dropped || l.start <= t) continue;
      l.dropped = true;
      for (const s of l.sources) {
        try {
          s.stop();
        } catch (_) {}
      }
    }
    const kept = this.lines.filter((l) => !l.dropped && l.end > t);
    this.playhead = Math.max(t, ...kept.map((l) => l.end));
  }

  /** Fades everything out right now. */
  cut() {
    const t = this.ctx.currentTime;
    const old = this.gain;
    old.gain.setValueAtTime(old.gain.value, t);
    old.gain.linearRampToValueAtTime(0, t + 0.06);
    for (const l of this.lines) {
      if (l.dropped || l.end <= t) continue;
      if (l.start > t) {
        l.dropped = true;
      } else {
        l.cut = true;
        l.end = t + 0.06;
      }
      for (const s of l.sources) {
        try {
          s.stop(t + 0.08);
        } catch (_) {}
      }
    }
    setTimeout(() => old.disconnect(), 300);
    this.newEpoch();
    this.playhead = t;
  }

  /** Plays ready-made audio straight away, queued ahead of anything synthesized after it. */
  blurt(text, pcm) {
    const at = Math.max(this.playhead, this.ctx.currentTime + 0.02);
    const src = this.play(pcm, this.tts.sampleRate, at, this.gain);
    const line = {
      text,
      start: at,
      end: at + pcm.length / this.tts.sampleRate,
      sources: [src],
      gain: this.gain,
      cut: false,
      dropped: false,
    };
    this.lines.push(line);
    this.playhead = line.end;
    return line;
  }
}

async function getOrCreateSpeaker() {
  const ctx = getAudioContext();
  if (!ctx) {
    throw new Error("Web Audio API is not supported in this browser");
  }
  if (ctx.state === "suspended") {
    await ctx.resume().catch(() => {});
  }
  const tts = await loadPhonon();
  if (!activeSpeaker) {
    activeSpeaker = new Speaker(tts, ctx);
  }
  return activeSpeaker;
}

/* The first generation pays for the wasm pipeline spinning up: its thread
   pool, device choice and warm caches. Paying it before anyone is listening
   means the first sentence of an answer starts with the rest, not behind a
   stall of its own. */
function warmPhonon() {
  return loadPhonon()
    .then(async (tts) => {
      if (tts._warmed) {
        return;
      }
      tts._warmed = true;
      try {
        for await (const _ of tts.stream("Hello.", { voice: currentVoice })) {
          break; /* one chunk heats the pipe; the sound is discarded */
        }
      } catch (_) {}
    })
    .catch((err) => {
      console.debug("[voice] background phonon load deferred:", err.message);
    });
}

function initPhonon() {
  if (!phononReady()) return;
  warmPhonon();
}

/* ------------------------------------------------------------------ */
/* Speaking while the answer is still arriving                         */
/* ------------------------------------------------------------------ */

/* Speech is fed to the synthesiser a sentence at a time. The first words
   are audible as soon as the first sentence is complete, rather than after
   the whole (possibly very long) answer. */
function beginSpeechStream() {
  cancelSpeech();
  state.voice.speech = {
    enabled:
      state.voice.speakReplies || state.voice.handsfree || state.voice.live,
    buffer: "",
    inCode: false,
    bullet: null,
    lead: true,
    queue: [],
    speaking: false,
    finished: false,
    pending: 0,
    phononFailed: false,
    abortControllers: [],
    onDone: null,
    completionScheduled: false,
    completionTimer: null,
    abort: null,
    stopAudio: null,
  };
}

function feedSpeech(text) {
  if (!text) return;
  if (
    !(state.voice.speakReplies || state.voice.handsfree || state.voice.live)
  ) {
    return;
  }
  if (!state.voice.speech) {
    beginSpeechStream();
  }
  const speech = state.voice.speech;
  if (!speech || !speech.enabled) return;
  speech.buffer += text;
  drainSpeech(speech, false);
}

function drainSpeech(speech, force) {
  while (speech.buffer) {
    const newline = speech.buffer.indexOf("\n");
    if (newline >= 0) {
      const line = speech.buffer.slice(0, newline);
      speech.buffer = speech.buffer.slice(newline + 1);
      speakSpeechLine(speech, line);
      continue;
    }
    if (speech.inCode) {
      break;
    }

    let boundary = -1;
    for (let i = 0; i < speech.buffer.length; i++) {
      const c = speech.buffer[i];
      if (
        (c === "." || c === "!" || c === "?") &&
        (i + 1 >= speech.buffer.length || /\s/.test(speech.buffer[i + 1]))
      ) {
        boundary = i + 1;
        break;
      }
    }
    if (boundary < 0 && speech.buffer.length > 220) {
      const at = speech.buffer.lastIndexOf(" ", 200);
      if (at > 80) {
        boundary = at + 1;
      }
    }
    if (boundary > 0) {
      const chunk = speech.buffer.slice(0, boundary);
      speech.buffer = speech.buffer.slice(boundary);
      enqueueSpeech(speech, spokenLine(speech, chunk));
      continue;
    }
    if (force) {
      enqueueSpeech(speech, spokenLine(speech, speech.buffer));
      speech.buffer = "";
    }
    break;
  }
}

const BULLET = /^(?:[-*+•·]|\d{1,2}[.)])\s+(.+)$/;

function speakSpeechLine(speech, line) {
  const text = line.trim();
  if (text.startsWith("```") || text.startsWith("~~~")) {
    flushBullets(speech);
    speech.inCode = !speech.inCode;
    speech.table = false;
    /* Code is for reading; a word says where it lives. */
    if (speech.inCode) {
      enqueueSpeech(
        speech,
        "I've laid the code out on screen — open it when you're ready."
      );
    }
    return;
  }
  if (speech.inCode || !text) {
    return;
  }
  /* A markdown table's rule row is punctuation, not prose. */
  if (/^\|?[-: |]+\|?$/.test(text)) {
    return;
  }
  const bullet = text.match(BULLET);
  if (bullet) {
    /* Consecutive rows wait together; the roster speaks when the list
       ends. One line on its own gets a gentler hand. */
    speech.bullet = speech.bullet || [];
    speech.bullet.push(bullet[1]);
    speech.table = false;
    return;
  }
  flushBullets(speech);
  /* A wide table is a picture in the transcript, not a paragraph. The
     first row names it; the rest are the same picture. */
  const cells = text.split("|").filter((part) => part.trim());
  if (cells.length > 3) {
    if (!speech.table) {
      speech.table = true;
      enqueueSpeech(
        speech,
        "I have placed the detailed table overview on the console"
      );
    }
    return;
  }
  speech.table = false;
  enqueueSpeech(speech, spokenLine(speech, text));
}

/* Consecutive list rows are read as a person would: "first… then… and
   finally…", rather than one terse line after another. */
function flushBullets(speech) {
  const items = speech.bullet;
  speech.bullet = null;
  if (!items || !items.length) {
    return;
  }
  const said = items.length > 6 ? items.slice(0, 5) : items;
  const rest = items.length - said.length;
  const connectors = ["Then,", "Also,", "Next,", "And,"];
  /* "First, …" already opens like a person would, but a list that opens
     the whole reply deserves its own courtesy before the count. */
  const opened = speech.lead && said.length > 1;
  speech.lead = false;
  if (opened) {
    enqueueSpeech(speech, opener());
  }
  said.forEach((item, index) => {
    let prefix = "";
    if (said.length === 1) {
      prefix = "One more thing,";
    } else if (index === 0) {
      prefix = "First,";
    } else if (index === said.length - 1 && said.length >= 3) {
      prefix = "And finally,";
    } else {
      prefix = connectors[Math.min(index - 1, connectors.length - 1)];
    }
    enqueueSpeech(speech, prefix + " " + cleanSpeech(speakable(item)) + ".");
  });
  if (rest > 0) {
    enqueueSpeech(
      speech,
      "And there are " + rest + " more, which I've left on your screen."
    );
  }
}

/* The voice is a butler, not a terminal: it composes what it says, and
   its openers are the quiet courtesies of one. */
const OPENERS = [
  "Right away.",
  "Certainly.",
  "Of course.",
  "Very good.",
  "Very well.",
];
/* A rare honorific, in the manner of the house; mostly it is silent. */
const ADDRESS = ["sir", "", "", ""];

function opener() {
  const courtesy =
    OPENERS[Math.floor(Math.random() * OPENERS.length)].replace(/\.$/, "");
  const honorific = ADDRESS[Math.floor(Math.random() * ADDRESS.length)];
  return courtesy + (honorific ? ", " + honorific : "") + ".";
}

function spokenLine(speech, text) {
  let out = cleanSpeech(speakable(text));
  /* A line that ends in a colon is a heading; the voice drops it there. */
  out = out.replace(/[:;]$/, "");
  if (!out) {
    return "";
  }
  if (speech.lead) {
    speech.lead = false;
    if (
      !/^(okay|alright|right|sure|so|well|here|yes|no|hi|hey|hello|certainly|of course|very good|very well|on it)\b/i.test(
        out
      )
    ) {
      out = opener() + " " + out;
    }
  }
  if (!/[.!?]$/.test(out)) {
    out += ".";
  }
  return out;
}

/* Turn what the transcript shows into what the voice should say. The
   transcript keeps every detail; the ear only wants the shape of it. */
function speakable(text) {
  return String(text)
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(
      /(?:^|[\s(])((?:https?:\/\/|www\.)[^\s>)\]]+)/gi,
      " the link on your screen "
    )
    .replace(/^#{1,6}\s+/, "")
    .replace(/^\s*[>\s]*>\s/, "")
    .replace(/→|->|=>/g, " becomes ")
    .replace(/%/g, " percent")
    .replace(/&/g, " and ")
    .replace(/°C/gi, " degrees")
    .replace(/°F/gi, " degrees Fahrenheit")
    .replace(/°/g, " degrees")
    .replace(/\s*≈\s*/g, " about ")
    .replace(/\s*×\s*/g, " by ")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*MB\/s\b/gi, "$1 megabytes per second")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*KB\/s\b/gi, "$1 kilobytes per second")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*GB\/s\b/gi, "$1 gigabytes per second")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*ms\b/gi, "$1 milliseconds")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*GB\b/gi, "$1 gigabytes")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*MB\b/gi, "$1 megabytes")
    .replace(/\b(\d+(?:[.,]\d+)?)\s*KB\b/gi, "$1 kilobytes")
    .replace(/\bB\/s\b/gi, " bytes per second")
    .replace(/\s*≥\s*/g, " at least ")
    .replace(/\s*≤\s*/g, " at most ")
    .replace(/\bat\s+at\s+(least|most)\b/gi, "at $1")
    .replace(/\s*[·•–]\s*/g, ", ")
    .replace(/\s*—\s*/g, " — ")
    .replace(/\s*~/g, " about ")
    .replace(/\|/g, ", ")
    .replace(/\s*,\s*/g, ", ")
    .replace(/^[,:\s]+|[,:\s]+$/g, "")
    .replace(/\p{Extended_Pictographic}|\p{Emoji_Modifier}/gu, "")
    .replace(/\s+/g, " ")
    .trim();
}

function cleanSpeech(text) {
  return String(text)
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/[*_`#>]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function enqueueSpeech(speech, text) {
  if (!text) return;
  /* Lines arriving after the completion timer was set reverse it: what was
     scheduled to end will now have more to say. */
  if (speech.completionScheduled) {
    speech.completionScheduled = false;
    if (speech.completionTimer) {
      window.clearTimeout(speech.completionTimer);
      speech.completionTimer = null;
    }
  }
  speech.queue.push(text);
  pumpSpeech(speech);
}

const HUMAN_VOICE =
  /natural|neural|premium|enhanced|samantha|daniel|aria|jenny|libby|sonia|guy|zarvox|eva/i;

function speechVoice() {
  if (!("speechSynthesis" in window)) {
    return null;
  }
  const voices = window.speechSynthesis.getVoices();
  if (!voices.length) {
    return null;
  }
  const preferred = (navigator.language || "en-US").toLowerCase().slice(0, 2);
  const local = voices.filter((voice) =>
    String(voice.lang || "").toLowerCase().startsWith(preferred)
  );
  const pool = local.length
    ? local
    : voices.filter((voice) =>
        String(voice.lang || "").toLowerCase().startsWith("en")
      );
  return pool.find((voice) => HUMAN_VOICE.test(voice.name)) || pool[0] || null;
}

/* The batch size of one dispatch: several sentences go to the synthesiser
   together, since it splits text at sentence boundaries itself. Feeding it
   more at once lets it start on the next sentence while the previous one is
   still being heard, instead of idling between lines. */
const SPEECH_BATCH_MAX = 900;

function takeSpeechBatch(speech) {
  if (!speech.queue.length) return "";
  const parts = [speech.queue.shift()];
  let total = parts[0].length;
  while (speech.queue.length) {
    const next = speech.queue[0];
    if (total + next.length + 1 > SPEECH_BATCH_MAX) break;
    parts.push(speech.queue.shift());
    total += next.length + 1;
  }
  return parts.join(" ");
}

/* Phonon synthesis starts and is left alone: the dispatch handles the next
   queue head the moment this batch's generation begins, so batches overlap
   instead of every one waiting for the whole pipeline turn of the last. The
   worker runs one generation at a time and the speaker schedules by a shared
   playhead, so the audio still arrives in order, gapless. */
function dispatchPhononSpeech(speech, text) {
  speech.pending += 1;
  getOrCreateSpeaker()
    .then((speaker) => {
      if (speech.cancelled || state.voice.speech !== speech) {
        return;
      }
      const controller = new AbortController();
      speech.abort = controller;
      speech.abortControllers.push(controller);
      return speaker.speak(text, controller.signal);
    })
    .then(
      () => releasePhononSpeech(speech, false),
      (err) => {
        if (state.voice.speech !== speech) return;
        console.warn("[voice] phonon speaking failed, falling back:", err);
        releasePhononSpeech(speech, true);
      }
    );
}

function releasePhononSpeech(speech, failed) {
  if (speech.pending > 0) {
    speech.pending -= 1;
  }
  if (failed && !speech.phononFailed && state.voice.speech === speech) {
    /* Whatever is still scheduled keeps playing; the rest of the answer
       speaks through the fallback one line at a time. */
    speech.phononFailed = true;
  }
  /* With this batch settled, the queue may drain or the completion may be
     due; the pump knows which. */
  pumpSpeech(speech);
}

function pumpSpeech(speech) {
  if (!speech || state.voice.speech !== speech || speech.speaking) {
    return;
  }
  if (!speech.queue.length && !speech.pending) {
    if (speech.finished) {
      scheduleSpeechCompletion(speech);
    }
    return;
  }

  if (phononReady() && !speech.phononFailed) {
    while (speech.queue.length) {
      dispatchPhononSpeech(speech, takeSpeechBatch(speech));
    }
    return;
  }
  if (!speech.queue.length) {
    return;
  }

  speech.speaking = true;
  const text = speech.queue.shift();

  function next() {
    if (state.voice.speech !== speech) return;
    speech.speaking = false;
    pumpSpeech(speech);
  }

  fallbackSpeech(speech, text, next);
}

/* Fallback: Play one sentence in the gateway's server voice. */
function playServerSpeech(speech, text) {
  const controller = new AbortController();
  speech.abort = controller;
  const headers = Object.assign(
    { "content-type": "application/json" },
    apiHeaders()
  );
  return fetch("/v1/audio/speech", {
    method: "POST",
    headers,
    body: JSON.stringify({ input: text, response_format: "wav" }),
    signal: controller.signal,
  })
    .then((response) => {
      if (!response.ok) {
        if (response.status === 404 || response.status === 501) {
          state.voice.server = false;
        }
        throw new Error(
          "the gateway could not speak (" + response.status + ")"
        );
      }
      return response.blob();
    })
    .then((blob) => {
      if (speech.cancelled || state.voice.speech !== speech) {
        throw new Error("the speech was cancelled");
      }
      const url = URL.createObjectURL(blob);
      const audio = new Audio(url);
      speech.audio = audio;
      return new Promise((resolve, reject) => {
        let settled = false;
        function finish(error) {
          if (settled) return;
          settled = true;
          speech.audio = null;
          speech.stopAudio = null;
          URL.revokeObjectURL(url);
          if (error) {
            reject(error);
          } else {
            resolve();
          }
        }
        speech.stopAudio = () => finish(new Error("the speech was cancelled"));
        audio.addEventListener("ended", () => finish(), { once: true });
        audio.addEventListener(
          "error",
          () => finish(new Error("the sound could not be played")),
          { once: true }
        );
        audio.play().catch(finish);
      });
    });
}

/* Fallback: Speak a sentence in the browser's speechSynthesis. */
function browserSpeech(text) {
  return new Promise((resolve) => {
    if (!("speechSynthesis" in window)) {
      resolve();
      return;
    }
    const utterance = new SpeechSynthesisUtterance(text);
    utterance.lang = navigator.language || "en-US";
    const picked = speechVoice();
    if (picked) {
      utterance.voice = picked;
    }
    utterance.rate = 1.04;
    utterance.pitch = 1.0;
    let done = false;
    function finish() {
      if (done) return;
      done = true;
      resolve();
    }
    utterance.onend = finish;
    utterance.onerror = finish;
    window.speechSynthesis.speak(utterance);
  });
}

function fallbackSpeech(speech, text, next) {
  if (serverSpeech()) {
    playServerSpeech(speech, text).then(next, () => {
      if (speech.cancelled || state.voice.speech !== speech) return;
      browserSpeech(text).then(next, next);
    });
    return;
  }
  browserSpeech(text).then(next, next);
}

function scheduleSpeechCompletion(speech) {
  if (speech.completionScheduled || speech.pending) return;
  speech.completionScheduled = true;
  if (activeSpeaker && activeSpeaker.ahead() > 0.02) {
    const ms = Math.ceil(activeSpeaker.ahead() * 1000) + 60;
    speech.completionTimer = window.setTimeout(() => {
      if (state.voice.speech === speech) {
        completeSpeech(speech);
      }
    }, ms);
  } else {
    completeSpeech(speech);
  }
}

function completeSpeech(speech) {
  if (state.voice.speech !== speech) return;
  const done = speech.onDone;
  state.voice.speech = null;
  if (done) done();
}

function finishSpeechStream(resume) {
  const speech = state.voice.speech;
  if (!speech) {
    if (resume) {
      resumeListeningSoon();
    }
    return;
  }
  flushBullets(speech);
  drainSpeech(speech, true);
  speech.finished = true;
  speech.onDone = resume ? resumeListeningSoon : null;
  if (!speech.speaking) {
    pumpSpeech(speech);
  }
}

function resumeListeningSoon() {
  if (
    !(state.voice.handsfree || state.voice.live) ||
    !state.connected ||
    state.busy
  ) {
    return;
  }
  window.setTimeout(() => {
    if (state.voice.handsfree && state.connected && !state.busy) {
      startListening("handsfree");
    } else if (state.voice.live && state.connected && !state.busy) {
      startListening("live");
    }
  }, 250);
}

function cancelSpeech() {
  const speech = state.voice.speech;
  if (speech) {
    speech.cancelled = true;
    if (speech.completionTimer) {
      window.clearTimeout(speech.completionTimer);
      speech.completionTimer = null;
    }
    if (speech.abort) {
      try {
        speech.abort.abort();
      } catch (_) {}
    }
    for (const controller of speech.abortControllers || []) {
      try {
        controller.abort();
      } catch (_) {}
    }
    speech.abortControllers = [];
    if (speech.stopAudio) {
      try {
        speech.stopAudio();
      } catch (_) {}
    }
    state.voice.speech = null;
  }
  if (activeSpeaker) {
    try {
      activeSpeaker.cut();
    } catch (_) {}
  }
  if ("speechSynthesis" in window) {
    try {
      window.speechSynthesis.cancel();
    } catch (_) {}
  }
}

/* A line the person asked to hear immediately, outside a turn. */
function speak(text, done) {
  const clean = cleanSpeech(
    speakable(
      String(text)
        .replace(/```[\s\S]*?```/g, " code block ")
        .slice(0, 4000)
    )
  );
  if (!clean) {
    if (done) done();
    return;
  }
  cancelSpeech();
  const speech = {
    enabled: true,
    buffer: "",
    inCode: false,
    bullet: null,
    lead: false,
    queue: [clean],
    speaking: false,
    finished: true,
    pending: 0,
    phononFailed: false,
    abortControllers: [],
    onDone: done || null,
    cancelled: false,
    completionScheduled: false,
    completionTimer: null,
    abort: null,
    stopAudio: null,
  };
  state.voice.speech = speech;
  pumpSpeech(speech);
}

// Kick off background model loading
initPhonon();
