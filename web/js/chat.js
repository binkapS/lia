"use strict";

/* ------------------------------------------------------------------ */
/* The activity rail                                                   */
/* ------------------------------------------------------------------ */

const ACTION = {
  idle: ["system idle", "awaiting directive"],
  uplink: ["uplink", "opening a channel"],
  thinking: ["cognition", "reasoning"],
  writing: ["synthesis", "transmitting a response"],
  tool: ["action", "executing a tool"],
  action: ["action", "executing a tool"],
  auth: ["authorisation", "permission requested"],
  error: ["fault", "signal interrupted"],
};

function setActivity(mode, text, tool) {
  const pair = ACTION[mode] || ACTION.thinking;
  el.activity.dataset.mode = mode;
  el.actionLabel.textContent = pair[0];
  el.actionText.textContent = text || pair[1];
  el.actionTool.textContent = tool ? "// " + String(tool).toUpperCase() : "// no active process";
}

function runningTool() {
  for (const tool of state.tools.values()) {
    if (tool.running) {
      return tool.name;
    }
  }
  return "";
}

function toolTag(name) {
  const n = String(name || "").toLowerCase();
  if (n.includes("shell") || n.includes("bash") || n.includes("command")) return "shell";
  if (n.includes("read") || n.includes("write") || n.includes("edit") || n.includes("filesystem")) return "files";
  if (n.includes("search")) return "search";
  if (n.includes("fetch") || n.includes("http")) return "network";
  if (n.includes("task") || n.includes("agent")) return "dispatch";
  if (n.includes("memory")) return "memory";
  if (n.includes("schedule") || n.includes("cron")) return "schedule";
  if (n.includes("todo") || n.includes("plan")) return "plan";
  if (n.includes("voice") || n.includes("speak") || n.includes("listen")) return "voice";
  return "tool";
}

function toolVerb(name) {
  const tag = toolTag(name);
  const verbs = {
    shell: "executing a shell sequence",
    files: "accessing local files",
    search: "querying the knowledge grid",
    network: "retrieving a remote signal",
    dispatch: "dispatching a sub-agent",
    memory: "reading long-term memory",
    schedule: "synchronising the schedule",
    plan: "updating the mission plan",
    voice: "opening the voice channel",
    tool: "running " + (name || "a tool"),
  };
  return verbs[tag] || verbs.tool;
}

function formatElapsed(seconds) {
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return String(minutes).padStart(2, "0") + ":" + rest.toFixed(1).padStart(4, "0");
}

function updateTimer() {
  if (!state.busy) {
    return;
  }
  el.actionTimer.textContent = formatElapsed((performance.now() - state.turnStart) / 1000);
}

function startTimer() {
  window.clearInterval(state.timer);
  state.turnStart = performance.now();
  el.actionTimer.textContent = "00:00.0";
  state.timer = window.setInterval(updateTimer, 100);
}

function stopTimer() {
  window.clearInterval(state.timer);
  state.timer = null;
}

function beginTurn() {
  state.busy = true;
  state.follow = true;
  document.body.classList.add("busy");
  el.activity.classList.add("busy");
  el.send.disabled = true;
  el.send.classList.add("busy");
  el.input.setAttribute("aria-busy", "true");
  el.mode.textContent = "ACTIVE";
  state.streaming = null;
  state.thinking = null;
  startTimer();
  hideWelcome();
  beginSpeechStream();
}

function endTurn() {
  state.busy = false;
  document.body.classList.remove("busy");
  el.activity.classList.remove("busy");
  el.send.disabled = false;
  el.send.classList.remove("busy");
  el.input.removeAttribute("aria-busy");
  el.mode.textContent = "STANDBY";
  stopTimer();

  for (const which of [state.streaming, state.thinking]) {
    if (!which) continue;
    /* Painting the final queued render before stripping the caret means
       the last tokens of a long thinking trace are never lost. */
    flushStreamRender(which);
    which.classList.remove("streaming");
    which.querySelectorAll(".caret").forEach((caret) => caret.remove());
    if (!which.dataset.raw) {
      which.remove();
    }
  }
  state.streaming = null;
  state.thinking = null;
  state.tools.clear();
  setActivity("idle", "awaiting directive");

  /* The answer was spoken as it arrived. When the queue drains, the
     microphone opens again if hands-free is still on. */
  finishSpeechStream(state.connected && (state.voice.handsfree || state.voice.live));
}

/* ------------------------------------------------------------------ */
/* The conversation                                                    */
/* ------------------------------------------------------------------ */

const ROLES = {
  user: { name: "OPERATOR", glyph: "❯" },
  assistant: { name: "LIA", glyph: "◆" },
  thinking: { name: "THOUGHT", glyph: "◇" },
  notice: { name: "SYSTEM", glyph: "·" },
  error: { name: "FAULT", glyph: "✗" },
  sub: { name: "SUB-AGENT", glyph: "↳" },
  tool: { name: "TOOL", glyph: "⬢" },
  auth: { name: "AUTH", glyph: "⚠" },
};

function hideWelcome() {
  const welcome = document.getElementById("welcome");
  if (welcome) {
    welcome.remove();
  }
  state.welcome = false;
}

function addLine(kind, text) {
  hideWelcome();
  const role = ROLES[kind] || ROLES.notice;
  const entry = document.createElement("div");
  entry.className = "entry " + kind;
  const roleBox = document.createElement("div");
  roleBox.className = "entry-role";
  roleBox.innerHTML =
    '<span class="role-glyph">' + role.glyph + '</span><span class="role-name">' + role.name + "</span>";

  const body = document.createElement("div");
  body.className = "entry-body";

  if (kind === "thinking") {
    /* Thoughts stay out of the way until the reader asks for them. */
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "thought-toggle";
    toggle.textContent = "Thought";
    toggle.setAttribute("aria-expanded", "false");

    const inner = document.createElement("div");
    inner.className = "body thought-body";
    inner.hidden = true;
    inner.innerHTML = markdown(text);

    toggle.addEventListener("click", () => {
      const open = inner.hidden;
      inner.hidden = !open;
      toggle.setAttribute("aria-expanded", String(open));
      entry.classList.toggle("expanded", open);
    });

    body.appendChild(toggle);
    body.appendChild(inner);
  } else {
    const inner = document.createElement("div");
    inner.className = "body";
    inner.innerHTML = markdown(text);
    body.appendChild(inner);
  }

  entry.appendChild(roleBox);
  entry.appendChild(body);
  el.transcript.appendChild(entry);
  updateCount();
  follow();
  return entry;
}

function updateCount() {
  const count = el.transcript.querySelectorAll(".entry").length;
  el.msgCount.textContent = String(count).padStart(2, "0") + " entries";
}

function addCaret(entry) {
  const body = entry.querySelector(".body");
  if (!body) return;
  body.querySelectorAll(".caret").forEach((caret) => caret.remove());
  const caret = document.createElement("span");
  caret.className = "caret";
  caret.setAttribute("aria-hidden", "true");
  const last = body.lastElementChild;
  if (last && last.tagName === "P") {
    last.appendChild(caret);
  } else {
    body.appendChild(caret);
  }
}

/* Streaming deltas used to re-render the *whole* entry with markdown on
   every token. A long thinking trace then becomes O(n²) work on the main
   thread: the UI visibly freezes while the model keeps producing, TCP
   buffers fill, and the gateway's bounded queues back up. Coalescing the
   renders to one per animation frame (and never more often than ~30fps)
   keeps the page responsive even with thousands of deltas. */
const STREAM_MIN_GAP_MS = 32;

function renderStreamEntry(entry) {
  const body = entry.querySelector(".body");
  if (!body) return;
  body.innerHTML = markdown(entry.dataset.raw || "");
  addCaret(entry);
  follow();
}

function scheduleStreamRender(entry) {
  if (!entry || entry._renderQueued) return;
  entry._renderQueued = true;
  const render = () => {
    entry._renderQueued = false;
    if (entry._lastRenderAt) {
      const gap = Date.now() - entry._lastRenderAt;
      if (gap < STREAM_MIN_GAP_MS) {
        window.setTimeout(render, STREAM_MIN_GAP_MS - gap);
        entry._renderQueued = true;
        return;
      }
    }
    entry._lastRenderAt = Date.now();
    renderStreamEntry(entry);
  };
  if (document.hidden) {
    /* requestAnimationFrame is paused for hidden tabs, which would hide
       the newest content entirely. A timer keeps the transcript fresh. */
    window.setTimeout(render, 100);
  } else {
    window.requestAnimationFrame(render);
  }
}

function flushStreamRender(entry) {
  if (!entry) return;
  entry._renderQueued = false;
  entry._lastRenderAt = Date.now();
  renderStreamEntry(entry);
}

function streamText(text) {
  if (!state.streaming) {
    state.streaming = addLine("assistant", "");
    state.streaming.classList.add("streaming");
  }
  const entry = state.streaming;
  entry.dataset.raw = (entry.dataset.raw || "") + text;
  scheduleStreamRender(entry);
}

function streamThinking(text) {
  if (!state.thinking) {
    state.thinking = addLine("thinking", "");
    state.thinking.classList.add("streaming");
  }
  const entry = state.thinking;
  entry.dataset.raw = (entry.dataset.raw || "") + text;
  scheduleStreamRender(entry);
}

/* A tool, and what came of it. */
function showTool(data) {
  hideWelcome();
  const id = data.id || "tool-" + state.tools.size + "-" + Date.now();
  const entry = document.createElement("div");
  entry.className = "entry tool-entry";
  entry.innerHTML =
    '<div class="entry-role"><span class="role-glyph">⬢</span><span class="role-name">TOOL</span></div>' +
    '<div class="entry-body"></div>';

  const panel = document.createElement("div");
  panel.className = "tool running";
  const head = document.createElement("div");
  head.className = "tool-head";
  head.innerHTML =
    '<span class="tool-name"><span class="sigil">◈</span>' +
    escape(data.name || "tool") +
    "</span>" +
    '<span class="tool-tag">' + toolTag(data.name) + "</span>" +
    '<span class="tool-state"><i class="spinner"></i><span>running</span></span>';
  panel.appendChild(head);

  if (data.input !== undefined && data.input !== null && pretty(data.input)) {
    const input = document.createElement("pre");
    input.className = "tool-input";
    input.textContent = pretty(data.input);
    panel.appendChild(input);
  }

  const status = head.querySelector(".tool-state span");
  entry.querySelector(".entry-body").appendChild(panel);
  el.transcript.appendChild(entry);
  state.tools.set(id, {
    entry: entry,
    card: panel,
    status: status,
    name: data.name || "tool",
    running: true,
  });
  setActivity("tool", toolVerb(data.name), data.name || "tool");
  updateCount();
  follow();
}

function finishTool(data) {
  const tool = state.tools.get(data.id);
  if (!tool) {
    return;
  }
  tool.running = false;
  const bad = !!data.is_error;
  tool.card.classList.remove("running");
  tool.card.classList.add(bad ? "failed" : "done");
  tool.status.parentElement.innerHTML = bad
    ? "✗<span>failed</span>"
    : "✓<span>complete</span>";

  const content = document.createElement("pre");
  content.className = "tool-output";
  content.textContent = (data.content || "").split("\n").slice(0, 12).join("\n");
  tool.card.appendChild(content);

  state.tools.delete(data.id);
  if (state.busy) {
    const active = runningTool();
    setActivity("thinking", active ? toolVerb(active) : "processing the result", active || tool.name);
  }
  follow();
}

/* A tool that is asking whether it may run. */
function showApproval(data) {
  hideWelcome();
  setActivity("auth", "a tool is waiting for a decision", data.tool || "tool");

  const entry = document.createElement("div");
  entry.className = "entry ask-entry";
  entry.innerHTML =
    '<div class="entry-role"><span class="role-glyph">⚠</span><span class="role-name">AUTH</span></div>' +
    '<div class="entry-body"></div>';
  const panel = document.createElement("div");
  panel.className = "ask";

  const head = document.createElement("div");
  head.className = "ask-head";
  head.textContent = "permission required";
  const what = document.createElement("div");
  what.className = "what";
  what.textContent = (data.tool || "a tool") + " — " + (data.summary || "may this run?");
  const args = document.createElement("div");
  args.className = "args";
  args.textContent = data.arguments || "";
  const choices = document.createElement("div");
  choices.className = "choices";

  const allow = button("allow once", "allow", () => {
    answer(data.requestId, true, false);
    done("allowed for this call");
  });
  const always = button("always allow", "allow", () => {
    answer(data.requestId, true, true);
    done("allowed for every call");
  });
  const deny = button("deny", "deny", () => {
    answer(data.requestId, false, false);
    done("denied");
  });

  function done(note) {
    choices.querySelectorAll("button").forEach((b) => (b.disabled = true));
    const answered = document.createElement("div");
    answered.className = "answered";
    answered.textContent = "› " + note;
    panel.appendChild(answered);
    if (state.busy) {
      setActivity("thinking", "resuming the task");
    } else {
      setActivity("idle", "awaiting directive");
    }
  }

  choices.appendChild(allow);
  choices.appendChild(always);
  choices.appendChild(deny);
  panel.appendChild(head);
  panel.appendChild(what);
  if (args.textContent) {
    panel.appendChild(args);
  }
  panel.appendChild(choices);
  entry.querySelector(".entry-body").appendChild(panel);
  el.transcript.appendChild(entry);
  updateCount();
  follow();
}

function button(text, kind, action) {
  const b = document.createElement("button");
  b.textContent = text;
  b.className = kind;
  b.addEventListener("click", action);
  return b;
}

/* ------------------------------------------------------------------ */
/* Scrolling                                                           */
/* ------------------------------------------------------------------ */

function follow() {
  if (state.follow) {
    el.transcript.scrollTop = el.transcript.scrollHeight;
  }
}

el.transcript.addEventListener("scroll", () => {
  const atBottom =
    el.transcript.scrollHeight - el.transcript.scrollTop - el.transcript.clientHeight < 40;
  state.follow = atBottom;
  el.newest.className = atBottom ? "" : "show";
});

el.newest.addEventListener("click", () => {
  state.follow = true;
  el.transcript.scrollTop = el.transcript.scrollHeight;
  el.newest.className = "";
});

/* ------------------------------------------------------------------ */
/* The sessions                                                        */
/* ------------------------------------------------------------------ */

function shortId(id) {
  return id && id.length > 17 ? id.slice(0, 9) + "…" + id.slice(-5) : id || "—";
}

function renderSessions() {
  el.sessions.innerHTML = "";
  el.sessionCount.textContent = String(state.sessionList.length).padStart(2, "0");

  if (!state.sessionList.length) {
    const item = document.createElement("li");
    item.className = "session-empty";
    item.textContent = "no sessions detected";
    el.sessions.appendChild(item);
    return;
  }

  state.sessionList.forEach((session, index) => {
    const item = document.createElement("li");
    const body = document.createElement("button");
    body.className = "session-item";
    if (session.id === state.sessionId) {
      body.classList.add("current");
      body.setAttribute("aria-current", "true");
    }
    const numbered = document.createElement("span");
    numbered.className = "session-index";
    numbered.textContent = String(index + 1).padStart(2, "0");
    const name = document.createElement("span");
    name.className = "session-name";
    /* A session that has never been named is shown by its address. */
    const named = session.title && session.title !== "New session";
    name.textContent = named ? session.title : shortId(session.id);
    name.title = session.id;
    const count = document.createElement("span");
    count.className = "session-count";
    count.textContent = session.message_count || 0;
    body.appendChild(numbered);
    body.appendChild(name);
    body.appendChild(count);
    body.addEventListener("click", () => openSession(session.id));
    item.appendChild(body);
    el.sessions.appendChild(item);
  });
}

async function loadSessions() {
  let sessions = [];
  try {
    const data = await request("session.list", {});
    sessions = data.sessions || [];
  } catch (e) {
    return;
  }
  state.sessionList = sessions;
  renderSessions();

  /* The newest session is the one that was being worked on. */
  if (!state.sessionId && sessions.length) {
    openSession(sessions[0].id);
  } else if (!state.sessionId) {
    renderWelcome();
  }
}

async function openSession(id) {
  state.sessionId = id;
  el.session.textContent = shortId(id);
  el.session.title = id;
  el.channelSession.textContent = "channel " + shortId(id);
  el.transcript.innerHTML = "";
  state.tools.clear();
  state.streaming = null;
  state.thinking = null;
  hideWelcome();
  renderSessions();

  let session;
  try {
    session = await api("/v1/sessions/" + encodeURIComponent(id));
  } catch (e) {
    addLine("error", "the session could not be read: " + e.message);
    return;
  }

  for (const message of session.messages || []) {
    for (const block of message.content || []) {
      if (block.type === "text") {
        addLine(message.role === "user" ? "user" : "assistant", block.text || "");
      } else if (block.type === "thinking") {
        addLine("thinking", block.text || "");
      } else if (block.type === "tool_use") {
        showHistoryTool(block);
      } else if (block.type === "tool_result") {
        finishHistoryTool(block);
      }
    }
  }
  updateCount();
  if (!el.transcript.querySelector(".entry")) {
    renderWelcome();
  }
  setActivity("idle", "awaiting directive");
  state.follow = true;
  follow();
}

function showHistoryTool(block) {
  showTool({ id: block.id, name: block.name, input: block.input });
}

function finishHistoryTool(block) {
  finishTool({
    id: block.tool_use_id,
    content: block.content || "",
    is_error: !!block.is_error,
  });
}

el.new.addEventListener("click", async () => {
  try {
    const data = await request("session.create", {});
    if (data.session) {
      state.sessionList.unshift(data.session);
      await openSession(data.session.id);
      loadSessions();
    }
  } catch (e) {
    addLine("error", "the session could not be made: " + e.message);
  }
});

