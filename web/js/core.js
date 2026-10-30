"use strict";

/* ------------------------------------------------------------------ */
/* State                                                               */
/* ------------------------------------------------------------------ */

const state = {
  token: "",
  sessionId: "",
  sessionList: [],
  socket: null,
  requests: new Map(),
  nextId: 1,
  retry: 2000,
  busy: false,
  follow: true,
  streaming: null,
  thinking: null,
  tools: new Map(),
  turnStart: 0,
  timer: null,
  welcome: false,
  reduced: window.matchMedia("(prefers-reduced-motion: reduce)").matches,
  connected: false,
  suppressPreview: false,
  palette: { items: [], index: 0, kind: null },
  images: [],
  voice: {
    // lia speaks by default. The microphone stays shut until it is asked
    // for, so the page does not listen on its own.
    speakReplies: true,
    handsfree: false,
    live: false,
    liveShare: "none",
    listening: false,
    mode: null,
    recognition: null,
    speech: null,
    /* Whether the gateway can synthesize speech. When it can, replies are
       spoken in the same voice the terminal uses; the browser's own voice
       is only a fallback. */
    server: false,
  },
};

const el = {
  model: document.getElementById("model"),
  session: document.getElementById("session"),
  channelSession: document.getElementById("channelSession"),
  dot: document.getElementById("dot"),
  linkText: document.getElementById("linkText"),
  theme: document.getElementById("theme"),
  sessions: document.getElementById("sessions"),
  sessionCount: document.getElementById("sessionCount"),
  transcript: document.getElementById("transcript"),
  input: document.getElementById("input"),
  send: document.getElementById("send"),
  usage: document.getElementById("usage"),
  msgCount: document.getElementById("msgCount"),
  newest: document.getElementById("newest"),
  new: document.getElementById("new"),
  wrap: document.getElementById("wrap"),
  activity: document.getElementById("activity"),
  actionLabel: document.getElementById("actionLabel"),
  actionTool: document.getElementById("actionTool"),
  actionText: document.getElementById("actionText"),
  actionTimer: document.getElementById("actionTimer"),
  actionBars: document.getElementById("actionBars"),
  charCount: document.getElementById("charCount"),
  mode: document.getElementById("mode"),
  clock: document.getElementById("clock"),
  gate: document.getElementById("gate"),
  gateForm: document.getElementById("gate-form"),
  gateToken: document.getElementById("gate-token"),
  gateCancel: document.getElementById("gate-cancel"),
  palette: document.getElementById("palette"),
  paletteTitle: document.getElementById("paletteTitle"),
  paletteHint: document.getElementById("paletteHint"),
  paletteList: document.getElementById("paletteList"),
  attachments: document.getElementById("attachments"),
  commandButton: document.getElementById("commandButton"),
  voiceHint: document.getElementById("voiceHint"),
  liveToggle: document.getElementById("liveToggle"),
  liveShare: document.getElementById("liveShare"),
  voiceLive: document.getElementById("voiceLive"),
  voiceLiveText: document.getElementById("voiceLiveText"),
  imagePicker: document.getElementById("imagePicker"),
};

/* ------------------------------------------------------------------ */
/* The token                                                           */
/* ------------------------------------------------------------------ */

/* The token may be in the address so a link can carry it. It is taken
   out of the address at once, and kept for the rest of the visit. */
function readToken() {
  const params = new URLSearchParams(location.search);
  let token = params.get("token") || "";
  if (token) {
    params.delete("token");
    const rest = params.toString();
    history.replaceState(null, "", location.pathname + (rest ? "?" + rest : ""));
  }
  if (!token) {
    try {
      token = sessionStorage.getItem("lia.token") || "";
    } catch (e) {
      token = "";
    }
  }
  state.token = token;
}

function keepToken(token) {
  state.token = token;
  try {
    sessionStorage.setItem("lia.token", token);
  } catch (e) {
    /* A browser that will not keep it is a browser that does not mind. */
  }
}

/* Ask for the token in the interface rather than in a browser dialog. */
function askForToken() {
  return new Promise((resolve) => {
    el.gate.classList.add("show");
    el.gateToken.value = "";
    window.setTimeout(() => el.gateToken.focus(), 30);

    function finish(given) {
      el.gate.classList.remove("show");
      el.gateForm.removeEventListener("submit", submit);
      el.gateCancel.removeEventListener("click", cancel);
      resolve(given);
    }
    function submit(event) {
      event.preventDefault();
      const value = el.gateToken.value.trim();
      if (!value) {
        return;
      }
      keepToken(value);
      finish(true);
    }
    function cancel() {
      finish(false);
    }

    el.gateForm.addEventListener("submit", submit);
    el.gateCancel.addEventListener("click", cancel);
  });
}

/* ------------------------------------------------------------------ */
/* Talking to the gateway                                              */
/* ------------------------------------------------------------------ */

function apiHeaders() {
  const headers = {};
  if (state.token) {
    headers["authorization"] = "Bearer " + state.token;
  }
  return headers;
}

async function api(path) {
  const response = await fetch(path, { headers: apiHeaders() });
  if (response.status === 401) {
    throw new Error("the token was refused");
  }
  if (!response.ok) {
    throw new Error("the gateway answered " + response.status);
  }
  return await response.json();
}

function connect() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  let url = scheme + "//" + location.host + "/v1/ws";
  if (state.token) {
    url += "?token=" + encodeURIComponent(state.token);
  }

  setLink("linking");
  const socket = new WebSocket(url);
  state.socket = socket;

  socket.addEventListener("open", () => {
    state.retry = 2000;
    setConnected(true);
    loadSessions();
  });

  socket.addEventListener("message", (event) => {
    let frame;
    try {
      frame = JSON.parse(event.data);
    } catch (e) {
      return;
    }
    handleFrame(frame);
  });

  socket.addEventListener("close", () => {
    setConnected(false);
    const wasBusy = state.busy;
    stopListening();
    for (const pending of state.requests.values()) {
      pending.reject(new Error("the link was lost"));
    }
    state.requests.clear();
    if (wasBusy) {
      endTurn();
      setActivity("error", "link lost before the answer arrived");
      addLine("error", "the link to Lia was lost. reconnecting…");
    }
    /* A hiccup, not a decision: try again, a little later each time. */
    window.setTimeout(connect, state.retry);
    state.retry = Math.min(Math.round(state.retry * 1.7), 15000);
  });

  socket.addEventListener("error", () => {
    setConnected(false);
  });
}

function setLink(text) {
  el.linkText.textContent = text;
}

function setConnected(up) {
  state.connected = up;
  el.dot.className = up ? "dot up" : "dot";
  el.dot.title = up ? "connected" : "not connected";
  el.linkText.textContent = up ? "link stable" : "link lost";
  document.body.classList.toggle("offline", !up);
}

function request(method, params) {
  return new Promise((resolve, reject) => {
    if (!state.socket || state.socket.readyState !== WebSocket.OPEN) {
      reject(new Error("not connected"));
      return;
    }
    const id = String(state.nextId++);
    state.requests.set(id, { resolve, reject });
    state.socket.send(
      JSON.stringify({ type: "req", id: id, method: method, params: params || {} })
    );
  });
}

function answer(requestId, allow, always) {
  if (!state.socket || state.socket.readyState !== WebSocket.OPEN) {
    return;
  }
  const frame = { type: allow ? "approve" : "deny", request_id: requestId };
  if (always) {
    frame.always = true;
  }
  state.socket.send(JSON.stringify(frame));
}

/* ------------------------------------------------------------------ */
/* The frames                                                          */
/* ------------------------------------------------------------------ */

function handleFrame(frame) {
  const type = frame.type;

  if (type === "hello") {
    if (frame.model) {
      el.model.textContent = frame.model;
    }
    return;
  }

  if (type === "res") {
    const pending = state.requests.get(frame.id);
    state.requests.delete(frame.id);
    if (!pending) {
      return;
    }
    if (frame.ok) {
      pending.resolve(frame.data || {});
    } else {
      pending.reject(new Error(frame.error || "the request failed"));
    }
    return;
  }

  if (type === "event") {
    handleEvent(frame.event, frame.data || {});
    return;
  }

  if (type === "approval_request") {
    showApproval(frame);
    return;
  }

  if (type === "error") {
    addLine("error", frame.message || "something went wrong");
    setActivity("error", "the gateway reported a fault");
  }
}

function handleEvent(name, data) {
  switch (name) {
    case "started":
      if (!state.busy) {
        beginTurn();
      }
      setActivity("thinking", "decomposing the request");
      break;
    case "text_delta":
      streamText(data.text || "");
      feedSpeech(data.text || "");
      if (!runningTool()) {
        setActivity("writing", "synthesising a response");
      }
      break;
    case "thinking_delta":
      streamThinking(data.text || "");
      if (!runningTool()) {
        setActivity("thinking", "reasoning across the context");
      }
      break;
    case "tool_call_started":
      showTool(data);
      break;
    case "tool_call_finished":
      finishTool(data);
      break;
    case "sub_assistant":
      addLine("sub", (data.label ? data.label + " " : "") + (data.note || ""));
      break;
    case "notice":
      addLine("notice", data.message || "");
      break;
    case "error":
      addLine("error", data.message || "");
      setActivity("error", "the operation did not complete");
      break;
    case "approval_request":
      showApproval(data);
      break;
    case "turn_complete":
      if (data.usage) {
        showUsage(data.usage);
      }
      break;
    case "finished":
      if (data.usage) {
        showUsage(data.usage);
      }
      break;
    default:
      break;
  }
}

