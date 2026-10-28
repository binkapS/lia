"use strict";

/* ------------------------------------------------------------------ */
/* The zero state                                                      */
/* ------------------------------------------------------------------ */

const WELCOME = `
<div class="zero" id="welcome">
  <div class="zero-core" aria-hidden="true">
    <span class="ring one"></span>
    <span class="ring two"></span>
    <span class="ring three"></span>
    <span class="core-diamond"></span>
  </div>
  <div class="zero-kicker">LIA // online</div>
  <h1>What should we <em>build</em>?</h1>
  <p>Local-first intelligence with a view of this machine, its files, and the network beyond. Ask for an investigation, a change, or a plan.</p>
  <div class="quick-row">
    <button class="quick" data-prompt="What can you do in this workspace?">capability scan</button>
    <button class="quick" data-prompt="Read the README and give me the short version.">read the readme</button>
    <button class="quick" data-prompt="Check the repository status and suggest the next step.">inspect the repo</button>
  </div>
</div>`;

function renderWelcome() {
  el.transcript.innerHTML = WELCOME;
  state.welcome = true;
  state.follow = true;
  updateCount();
}

el.transcript.addEventListener("click", (event) => {
  const quick = event.target.closest(".quick");
  if (!quick) {
    return;
  }
  el.input.value = quick.dataset.prompt || "";
  growInput();
  el.input.focus();
});

