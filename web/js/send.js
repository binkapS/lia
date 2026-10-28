"use strict";

/* ------------------------------------------------------------------ */
/* Sending, whether typed or spoken                                    */
/* ------------------------------------------------------------------ */

async function send() {
  const text = el.input.value.trim();
  if (!text) {
    return;
  }
  if (!text.startsWith("/") && state.busy) {
    addLine("notice", "Lia is still working on the previous message.");
    return;
  }
  el.input.value = "";
  growInput();
  closePalette();
  if (text.startsWith("/")) {
    await executeCommand(text);
    return;
  }
  await deliver(text);
}

async function deliver(text) {
  if (state.busy || !text.trim()) {
    return;
  }
  if (!state.sessionId) {
    try {
      const data = await request("session.create", {});
      if (data.session) {
        state.sessionId = data.session.id;
        state.sessionList.unshift(data.session);
        el.session.textContent = shortId(state.sessionId);
        el.channelSession.textContent = "channel " + shortId(state.sessionId);
        renderSessions();
      }
    } catch (e) {
      addLine("error", "the session could not be made: " + e.message);
      return;
    }
  }

  const images = state.images.slice();
  if (images.length) {
    state.images = [];
    renderAttachments();
  }

  const shown =
    text +
    (images.length
      ? "\n\n" + images.length + (images.length === 1 ? " picture attached" : " pictures attached")
      : "");
  addLine("user", shown);
  state.follow = true;
  follow();

  /* One turn at a time: the events that come back carry no id of their
     own, so two turns at once would mix their words together. */
  beginTurn();
  setActivity("uplink", "transmitting directive");

  try {
    const data = await request("message.send", {
      content: text,
      sessionId: state.sessionId,
      images: images.map((image) => ({ media_type: image.mediaType, data: image.data })),
    });
    if (data.usage) {
      showUsage(data.usage);
    }
  } catch (e) {
    addLine("error", e.message);
    setActivity("error", "the directive was not delivered");
  } finally {
    endTurn();
    loadSessions();
  }
}

function showUsage(usage) {
  const inputTokens = Number(usage.input_tokens || 0);
  const outputTokens = Number(usage.output_tokens || 0);
  el.usage.textContent =
    "in " + inputTokens.toLocaleString() + " · out " + outputTokens.toLocaleString();
}

el.input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    if (paletteOpen()) {
      choosePalette(state.palette.index);
    } else {
      send();
    }
    return;
  }

  if (!paletteOpen()) {
    return;
  }

  if (event.key === "ArrowDown") {
    event.preventDefault();
    movePalette(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    movePalette(-1);
  } else if (event.key === "Tab") {
    event.preventDefault();
    if (state.palette.kind === "preview") {
      completePalette();
    } else {
      choosePalette(state.palette.index);
    }
  } else if (event.key === "Escape") {
    event.preventDefault();
    state.suppressPreview = true;
    closePalette();
  }
});

el.input.addEventListener("input", () => {
  state.suppressPreview = false;
  growInput();
  updateCommandPreview();
});

el.send.addEventListener("click", () => {
  if (paletteOpen()) {
    choosePalette(state.palette.index);
  } else {
    send();
  }
});

el.commandButton.addEventListener("click", () => {
  commandHelp();
  el.input.focus();
});

el.imagePicker.addEventListener("change", () => {
  addImageFiles(Array.from(el.imagePicker.files || []));
});

window.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    el.input.focus();
  }
});

