"use strict";

/* ------------------------------------------------------------------ */
/* Starting                                                            */
/* ------------------------------------------------------------------ */

async function start() {
  startTheme();
  startClock();
  startBars();
  startField();
  readToken();
  renderWelcome();
  setActivity("idle", "awaiting directive");

  /* The gateway is asked who it is. A refusal means the token is wanted,
     and the one that was given is not the right one. */
  let health = null;
  try {
    health = await api("/health");
  } catch (e) {
    if (state.token) {
      keepToken("");
      state.token = "";
    }
    if (!(await askForToken())) {
      addLine("error", "the gateway wants a token, and none was given.");
      setActivity("error", "authentication is required");
      return;
    }
    try {
      health = await api("/health");
    } catch (e2) {
      addLine("error", "the token was refused.");
      setActivity("error", "authentication failed");
      return;
    }
  }
  if (health && health.model) {
    el.model.textContent = health.model;
  }
  /* The gateway's own speech model speaks in the same voice as the TUI;
     the browser's synthesizer is the fallback when it has none. */
  state.voice.server = !!(health && health.speech);
  if (typeof initPhonon === "function") {
    initPhonon();
  }
  startNexus();
  connect();
}

start();
