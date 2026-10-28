"use strict";

/* ------------------------------------------------------------------ */
/* Saying something                                                    */
/* ------------------------------------------------------------------ */

function growInput() {
  el.input.style.height = "auto";
  el.input.style.height = Math.min(el.input.scrollHeight, Math.round(window.innerHeight * 0.3)) + "px";
  el.charCount.textContent = String(el.input.value.length).padStart(3, "0");
}

