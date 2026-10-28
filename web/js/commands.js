"use strict";

/* ------------------------------------------------------------------ */
/* Commands, both typed and chosen from the palette                    */
/* ------------------------------------------------------------------ */

const COMMANDS = [
  {
    name: "/help",
    args: "",
    description: "show the command index",
    run: commandHelp,
  },
  {
    name: "/clear",
    args: "",
    description: "start a fresh conversation",
    run: commandClear,
  },
  {
    name: "/sessions",
    args: "",
    description: "switch to a saved session",
    run: commandSessions,
  },
  {
    name: "/model",
    args: "",
    description: "show the gateway model",
    run: commandModel,
  },
  {
    name: "/tools",
    args: "",
    description: "list the tools this assistant can call",
    run: commandTools,
  },
  {
    name: "/skills",
    args: "",
    description: "list the skills that were found",
    run: commandSkills,
  },
  {
    name: "/image",
    args: "",
    description: "attach pictures to the next message",
    run: commandImage,
  },
  {
    name: "/listen",
    args: "",
    description: "speak once and send the words",
    run: commandListen,
  },
  {
    name: "/dictate",
    args: "",
    description: "speak once and put the words in the box",
    run: commandDictate,
  },
  {
    name: "/speak",
    args: "<words>",
    description: "say a line out loud",
    run: commandSpeak,
  },
  {
    name: "/voice",
    args: "on|off|<name>",
    description: "show or set whether replies are spoken, or pick a voice",
    run: commandVoice,
  },
  {
    name: "/handsfree",
    args: "on|off",
    description: "listen, answer aloud, and listen again",
    run: commandHandsfree,
  },
  {
    name: "/live",
    args: "on|off",
    description: "talk over the answer, with the microphone staying open",
    run: commandLive,
  },
  {
    name: "/quit",
    args: "",
    description: "close the console window",
    run: commandQuit,
  },
];

function paletteOpen() {
  return !el.palette.hidden;
}

function openPalette(kind, title, hint, items) {
  state.palette = { kind: kind, title: title, hint: hint, items: items, index: 0 };
  el.paletteTitle.textContent = title;
  el.paletteHint.textContent = hint;
  el.palette.hidden = false;
  renderPalette();
}

function renderPalette() {
  el.paletteList.innerHTML = "";
  const items = state.palette.items || [];
  if (!items.length) {
    const empty = document.createElement("div");
    empty.className = "palette-empty";
    empty.textContent = "no matches";
    el.paletteList.appendChild(empty);
    return;
  }
  items.forEach((item, index) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "palette-item" + (index === state.palette.index ? " active" : "");
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", index === state.palette.index ? "true" : "false");
    const name = document.createElement("span");
    name.className = "palette-name";
    name.textContent = item.label;
    const desc = document.createElement("span");
    desc.className = "palette-desc";
    desc.textContent = item.desc || "";
    row.appendChild(name);
    row.appendChild(desc);
    row.addEventListener("click", () => choosePalette(index));
    el.paletteList.appendChild(row);
  });
}

function closePalette() {
  state.palette = { items: [], index: 0, kind: null };
  el.palette.hidden = true;
  el.paletteList.innerHTML = "";
}

function movePalette(delta) {
  const count = state.palette.items.length;
  if (!count) return;
  state.palette.index = (state.palette.index + delta + count) % count;
  renderPalette();
  const active = el.paletteList.querySelector(".palette-item.active");
  if (active) {
    active.scrollIntoView({ block: "nearest" });
  }
}

function choosePalette(index) {
  const item = state.palette.items[index];
  if (!item) return;
  closePalette();
  if (typeof item.action === "function") {
    item.action();
  }
}

function completePalette() {
  const item = state.palette.items[state.palette.index];
  if (!item) return;
  el.input.value = item.complete || item.label;
  growInput();
  closePalette();
  state.suppressPreview = true;
  el.input.focus();
}

function commandPreviewItems(text) {
  const query = text.toLowerCase();
  return COMMANDS.filter((command) => command.name.startsWith(query)).map((command) => ({
    label: command.name,
    desc: command.description + (command.args ? "  " + command.args : ""),
    complete: command.name + " ",
    action: () => executeCommand(command.name),
  }));
}

function updateCommandPreview() {
  if (state.palette.kind && state.palette.kind !== "preview") {
    closePalette();
  }
  if (state.suppressPreview) {
    if (state.palette.kind === "preview") closePalette();
    return;
  }
  const text = el.input.value;
  if (!text.startsWith("/") || /\s/.test(text)) {
    if (state.palette.kind === "preview") closePalette();
    return;
  }
  const items = commandPreviewItems(text);
  if (!items.length) {
    if (state.palette.kind === "preview") closePalette();
    return;
  }
  if (state.palette.kind === "preview") {
    state.palette.items = items;
    state.palette.index = Math.min(state.palette.index, items.length - 1);
    renderPalette();
  } else {
    openPalette(
      "preview",
      "Commands",
      "↑↓ select · tab complete · enter run · esc close",
      items
    );
  }
}

async function executeCommand(text) {
  closePalette();
  el.input.value = "";
  growInput();
  const trimmed = String(text || "").trim();
  const space = trimmed.search(/\s/);
  const name = (space < 0 ? trimmed : trimmed.slice(0, space)).toLowerCase();
  const arg = space < 0 ? "" : trimmed.slice(space + 1).trim();
  const command = COMMANDS.find((candidate) => candidate.name === name);
  if (!command) {
    addLine("notice", "unknown command “" + name + "” — type /help to see the index.");
    return;
  }
  try {
    await command.run(arg);
  } catch (e) {
    addLine("error", "command failed: " + e.message);
  }
}

function commandHelp() {
  openPalette(
    "help",
    "Command index",
    "↑↓ select · enter insert · esc close",
    COMMANDS.map((command) => ({
      label: command.name,
      desc: command.description + (command.args ? "  " + command.args : ""),
      complete: command.name + " ",
      action: () => {
        el.input.value = command.name + " ";
        growInput();
        el.input.focus();
      },
    }))
  );
}

async function commandClear() {
  if (state.busy) {
    addLine("notice", "Lia is still working. /clear will be available when the turn is done.");
    return;
  }
  try {
    const data = await request("session.create", {});
    if (!data.session) {
      return;
    }
    state.sessionId = data.session.id;
    state.sessionList.unshift(data.session);
    state.streaming = null;
    state.thinking = null;
    state.tools.clear();
    el.transcript.innerHTML = "";
    el.session.textContent = shortId(state.sessionId);
    el.channelSession.textContent = "channel " + shortId(state.sessionId);
    setActivity("idle", "awaiting directive");
    renderSessions();
    renderWelcome();
    el.input.focus();
  } catch (e) {
    addLine("error", "a fresh session could not be made: " + e.message);
  }
}

function commandSessions() {
  if (!state.sessionList.length) {
    addLine("notice", "no sessions have been recorded yet.");
    return;
  }
  openPalette(
    "sessions",
    "Session index",
    "↑↓ select · enter open · esc close",
    state.sessionList.map((session) => ({
      label: session.title && session.title !== "New session" ? session.title : shortId(session.id),
      desc: (session.message_count || 0) + " messages · " + session.id,
      action: () => openSession(session.id),
    }))
  );
}

function commandModel(arg) {
  const model = el.model.textContent || "unknown";
  if (arg) {
    addLine(
      "notice",
      "the model is chosen by the gateway configuration, so it cannot be changed from this page. current model: " +
        model
    );
  } else {
    addLine("notice", "model: " + model + " · provider configured by the gateway");
  }
}

async function commandTools() {
  try {
    const data = await api("/v1/tools");
    const tools = data.tools || [];
    if (!tools.length) {
      addLine("notice", "no tools are available to this gateway.");
      return;
    }
    addLine(
      "notice",
      "**Available tools**\n" + tools.map((name) => "- `" + name + "`").join("\n")
    );
  } catch (e) {
    addLine("error", "the tools could not be read: " + e.message);
  }
}

async function commandSkills() {
  try {
    const data = await api("/v1/skills");
    const skills = data.skills || [];
    if (!skills.length) {
      addLine("notice", "no skills were found for this gateway.");
      return;
    }
    addLine(
      "notice",
      "**Skills found**\n" +
        skills
          .map(
            (skill) =>
              "- **" +
              skill.name +
              "** — " +
              (skill.description || "no description") +
              " _(" +
              (skill.source || "unknown") +
              ")_"
          )
          .join("\n")
    );
  } catch (e) {
    addLine("error", "the skills could not be read: " + e.message);
  }
}

function commandImage(arg) {
  if (arg) {
    addLine(
      "notice",
      "the browser cannot open a path from here. /image opens a file picker instead."
    );
  }
  el.imagePicker.value = "";
  el.imagePicker.click();
}

function commandListen() {
  startListening("send");
}

function commandDictate() {
  startListening("dictate");
}

function commandSpeak(arg) {
  if (!arg) {
    addLine("notice", "what should be said? /speak <words>");
    return;
  }
  if (!speechReady()) {
    addLine(
      "error",
      "nothing here can speak: the gateway has no speech model, and this browser does not provide speech synthesis."
    );
    return;
  }
  speak(arg);
}

function commandVoice(arg) {
  const raw = (arg || "").trim();
  const value = raw.toLowerCase();
  const picked = typeof setVoice === "function" ? setVoice(raw) : null;
  if (picked) {
    state.voice.speakReplies = true;
    addLine(
      "notice",
      "voice set to " + picked + " (" + voiceSourceDescription() + ")."
    );
    updateVoiceHint();
    return;
  }
  if (value === "on") {
    if (!speechReady()) {
      addLine(
        "error",
        "nothing here can speak: the gateway has no speech model, and this browser does not provide speech synthesis."
      );
      return;
    }
    state.voice.speakReplies = true;
    addLine(
      "notice",
      "Lia's replies will be spoken aloud, in " + voiceSourceDescription() + "."
    );
  } else if (value === "off") {
    state.voice.speakReplies = false;
    cancelSpeech();
    addLine("notice", "Lia's replies will be silent.");
  } else {
    const heard = [
      state.voice.speakReplies ? "spoken replies on" : "spoken replies off",
      state.voice.handsfree ? "hands-free on" : "hands-free off",
      typeof voiceSourceDescription === "function"
        ? voiceSourceDescription()
        : serverSpeech()
          ? "gateway voice"
          : speechReady()
            ? "browser voice"
            : "no voice",
    ];
    const choices =
      typeof VOICES !== "undefined" && VOICES.length
        ? "on|off|" + VOICES.map((v) => v.toLowerCase()).join("|")
        : "on|off";
    addLine("notice", "voice: " + heard.join(" · ") + " — /voice " + choices);
  }
  updateVoiceHint();
}

function commandHandsfree(arg) {
  const value = (arg || "").toLowerCase();
  if (value === "on") {
    if (!speechRecognition()) {
      addLine("error", "this browser does not provide speech recognition. try Chrome or Edge.");
      return;
    }
    state.voice.handsfree = true;
    addLine(
      "notice",
      "hands-free is on — say “lia” first, and the answer will be spoken before the microphone opens again."
    );
    updateVoiceHint();
    if (!state.busy) {
      startListening("handsfree");
    }
  } else if (value === "off") {
    state.voice.handsfree = false;
    stopListening();
    cancelSpeech();
    addLine("notice", "hands-free is off.");
    updateVoiceHint();
  } else {
    addLine(
      "notice",
      "hands-free is " + (state.voice.handsfree ? "on" : "off") + " — /handsfree on|off"
    );
  }
}

function commandLive(arg) {
  const value = (arg || "").toLowerCase();
  if (value === "on") {
    if (!speechRecognition()) {
      addLine("error", "this browser does not provide speech recognition. try Chrome or Edge.");
      return;
    }
    state.voice.live = true;
    state.voice.handsfree = false;
    updateVoiceHint();
    addLine(
      "notice",
      "live is on — the microphone stays open, and talking over the answer stops it."
    );
    if (!state.busy && state.connected) {
      startListening("live");
    }
  } else if (value === "off") {
    state.voice.live = false;
    stopListening();
    cancelSpeech();
    addLine("notice", "live is off.");
    updateVoiceHint();
  } else {
    addLine(
      "notice",
      "live is " + (state.voice.live ? "on" : "off") + " — /live on|off"
    );
  }
}

function commandQuit() {
  addLine("notice", "close the browser tab to leave the console. the gateway keeps running.");
}

function speechRecognition() {
  return window.SpeechRecognition || window.webkitSpeechRecognition || null;
}

function updateVoiceHint() {
  const bits = [];
  if (state.voice.listening) {
    bits.push("listening");
  } else if (state.voice.handsfree) {
    bits.push("handsfree");
  }
  if (state.voice.speakReplies) {
    bits.push("voice");
  }
  if (!bits.length) {
    el.voiceHint.hidden = true;
    el.voiceHint.textContent = "";
    el.voiceHint.classList.remove("listening");
    syncVoiceState();
    return;
  }
  el.voiceHint.hidden = false;
  el.voiceHint.textContent = "◆ " + bits.join(" · ");
  el.voiceHint.classList.toggle("listening", state.voice.listening);
  syncVoiceState();
}

function startListening(mode) {
  const Recognition = speechRecognition();
  if (!Recognition) {
    addLine("error", "this browser does not provide speech recognition. try Chrome or Edge.");
    return false;
  }
  if (state.voice.recognition) {
    try {
      state.voice.recognition.onend = null;
      state.voice.recognition.stop();
    } catch (e) {
      /* it was already closing */
    }
    state.voice.recognition = null;
  }
  state.voice.listening = false;
  if (state.busy && mode !== "dictate") {
    addLine("notice", "Lia is still working. the microphone will wait for the next turn.");
    return false;
  }
  /* Speaking while listening would feed Lia her own voice back. */
  cancelSpeech();

  const recognition = new Recognition();
  state.voice.recognition = recognition;
  state.voice.mode = mode;
  state.voice.listening = true;
  updateVoiceHint();
  showVoiceLive(
    mode === "handsfree" ? "listening — say “lia” to begin" : "listening"
  );

  let handled = false;
  let lastError = "";
  recognition.lang = navigator.language || "en-US";
  recognition.continuous = false;
  recognition.interimResults = true;

  recognition.onresult = (event) => {
    let finalText = "";
    let interimText = "";
    for (let i = event.resultIndex; i < event.results.length; i++) {
      const transcript = event.results[i][0].transcript || "";
      if (event.results[i].isFinal) {
        finalText += transcript;
      } else {
        interimText += transcript;
      }
    }
    const heard = (finalText || interimText).trim();
    if (heard) {
      showVoiceLive(formatHeard(heard));
    }
    if (finalText && !handled) {
      handled = true;
      hideVoiceLive();
      if (mode === "live") {
        cancelSpeech();
      }
      handleVoiceText(finalText.trim(), mode);
    }
  };

  recognition.onerror = (event) => {
    lastError = event.error || "";
    state.voice.listening = false;
    hideVoiceLive();
    updateVoiceHint();
    if (lastError !== "aborted" && lastError !== "no-speech") {
      addLine("error", "the microphone: " + lastError);
    }
  };

  recognition.onend = () => {
    state.voice.listening = false;
    hideVoiceLive();
    updateVoiceHint();
    if (
      (state.voice.handsfree || state.voice.live) &&
      !handled &&
      !state.busy &&
      state.connected &&
      lastError !== "not-allowed" &&
      lastError !== "service-not-allowed"
    ) {
      window.setTimeout(() => {
        if (state.voice.handsfree && !state.busy) {
          startListening("handsfree");
        } else if (state.voice.live && !state.busy) {
          startListening("live");
        }
      }, 700);
    }
  };

  try {
    recognition.start();
    if (mode !== "handsfree") {
      addLine("notice", "microphone open — speak now.");
    }
    return true;
  } catch (e) {
    state.voice.listening = false;
    hideVoiceLive();
    updateVoiceHint();
    addLine("error", "the microphone could not open: " + e.message);
    return false;
  }
}

function showVoiceLive(text) {
  if (!text) {
    hideVoiceLive();
    return;
  }
  el.voiceLive.hidden = false;
  el.voiceLiveText.textContent = text;
}

function hideVoiceLive() {
  el.voiceLive.hidden = true;
  el.voiceLiveText.textContent = "";
}

function formatHeard(text) {
  const wake = /^(hey\s+)?lia[\s,;:.!-]*/i;
  if (wake.test(text)) {
    const rest = text.replace(wake, "").trim();
    return rest ? "command // " + rest : "lia heard — listening";
  }
  return "hearing // " + text;
}

function stopListening() {
  if (state.voice.recognition) {
    try {
      state.voice.recognition.abort();
    } catch (e) {
      /* it was already closed */
    }
  }
  state.voice.recognition = null;
  state.voice.listening = false;
  hideVoiceLive();
  updateVoiceHint();
}

function handleVoiceText(text, mode) {
  if (!text) return;
  if (mode === "live") {
    deliver(text);
    if (state.voice.live && state.connected && !state.busy) {
      window.setTimeout(() => startListening("live"), 700);
    }
    return;
  }
  hideVoiceLive();
  if (mode === "dictate") {
    el.input.value = (el.input.value.trim() ? el.input.value.trim() + " " : "") + text;
    growInput();
    el.input.focus();
    addLine("notice", "dictated: " + text);
    return;
  }

  let spoken = text;
  if (mode === "handsfree") {
    const wake = /^(hey\s+)?lia[\s,;:.!-]*/i;
    if (!wake.test(spoken)) {
      addLine("notice", "heard “" + spoken + "” — say “lia” first in hands-free mode.");
      if (state.voice.handsfree && state.connected && !state.busy) {
        window.setTimeout(() => startListening("handsfree"), 500);
      }
      return;
    }
    spoken = spoken.replace(wake, "").trim();
    if (!spoken) {
      if (state.voice.handsfree && state.connected && !state.busy) {
        window.setTimeout(() => startListening("handsfree"), 500);
      }
      return;
    }
  }
  deliver(spoken);
}

