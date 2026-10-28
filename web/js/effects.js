"use strict";

/* ------------------------------------------------------------------ */
/* The clock, the bars, and the field                                  */
/* ------------------------------------------------------------------ */

function startClock() {
  function tick() {
    const now = new Date();
    el.clock.textContent = now.toLocaleTimeString([], { hour12: false });
    if (nx.clockDate) {
      nx.clockDate.textContent = now
        .toLocaleDateString([], { weekday: "short", day: "2-digit", month: "short" })
        .toUpperCase();
    }
  }
  tick();
  window.setInterval(tick, 1000);
}

function startBars() {
  for (let i = 0; i < 26; i++) {
    const bar = document.createElement("span");
    const height = 0.22 + ((i * 7) % 11) / 13;
    bar.style.setProperty("--i", String(i));
    bar.style.setProperty("--h", height.toFixed(2));
    el.actionBars.appendChild(bar);
  }
}

let starColour = "rgba(150,235,255,.7)";
const pointer = { x: 0, y: 0, tx: 0, ty: 0 };
let stars = [];
let fieldCanvas = null;
let fieldContext = null;
let fieldSize = { width: 0, height: 0, dpr: 1 };

function readStarColour() {
  const value = getComputedStyle(document.documentElement).getPropertyValue("--star").trim();
  if (value) {
    starColour = value;
  }
}

function resizeField() {
  if (!fieldCanvas) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  fieldSize.width = window.innerWidth;
  fieldSize.height = window.innerHeight;
  fieldSize.dpr = dpr;
  fieldCanvas.width = Math.round(fieldSize.width * dpr);
  fieldCanvas.height = Math.round(fieldSize.height * dpr);
  fieldContext.setTransform(dpr, 0, 0, dpr, 0, 0);
  const count = Math.max(38, Math.min(112, Math.round(fieldSize.width / 15)));
  stars = Array.from({ length: count }, () => ({
    x: Math.random() * fieldSize.width,
    y: Math.random() * fieldSize.height,
    z: 0.25 + Math.random() * 0.75,
    r: 0.4 + Math.random() * 1.35,
    phase: Math.random() * Math.PI * 2,
    drift: 0.004 + Math.random() * 0.014,
  }));
  if (state.reduced) {
    drawField(performance.now());
  }
}

function drawField(now) {
  if (!fieldContext) return;
  const ctx = fieldContext;
  ctx.clearRect(0, 0, fieldSize.width, fieldSize.height);
  pointer.x += (pointer.tx - pointer.x) * 0.04;
  pointer.y += (pointer.ty - pointer.y) * 0.04;

  for (const star of stars) {
    const twinkle = 0.35 + 0.65 * (0.5 + 0.5 * Math.sin(now * star.drift + star.phase));
    const x = (star.x + pointer.x * star.z * 16 + now * star.drift * 0.18) % fieldSize.width;
    const y = (star.y + pointer.y * star.z * 12 + now * star.drift * 0.06) % fieldSize.height;
    ctx.globalAlpha = twinkle * star.z * 0.8;
    ctx.fillStyle = starColour;
    ctx.beginPath();
    ctx.arc(x, y, star.r * star.z, 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.globalAlpha = 1;
}

function startField() {
  fieldCanvas = document.getElementById("field");
  fieldContext = fieldCanvas.getContext("2d");
  readStarColour();
  resizeField();
  window.addEventListener("resize", resizeField);
  window.addEventListener("pointermove", (event) => {
    pointer.tx = (event.clientX / window.innerWidth - 0.5) * 2;
    pointer.ty = (event.clientY / window.innerHeight - 0.5) * 2;
  });
  if (state.reduced) {
    drawField(performance.now());
    return;
  }
  function loop(now) {
    if (!document.hidden) {
      drawField(now);
    }
    window.requestAnimationFrame(loop);
  }
  window.requestAnimationFrame(loop);
}

/* ------------------------------------------------------------------ */
/* Theme                                                               */
/* ------------------------------------------------------------------ */

el.theme.addEventListener("click", () => {
  const light = document.documentElement.dataset.theme === "light";
  setTheme(light ? "dark" : "light");
});

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  document
    .querySelector('meta[name="theme-color"]')
    .setAttribute("content", theme === "light" ? "#e7f0f4" : "#05080e");
  try {
    localStorage.setItem("lia.theme", theme);
  } catch (e) {
    /* A browser that will not keep it is a browser that does not mind. */
  }
  readStarColour();
  if (state.reduced) {
    drawField(performance.now());
  }
}

function startTheme() {
  let theme = "";
  try {
    theme = localStorage.getItem("lia.theme") || "";
  } catch (e) {
    theme = "";
  }
  /* The command deck is dark by default. A person who chose the light
     deck keeps it; a new visitor gets the night watch. */
  setTheme(theme || "dark");
}

