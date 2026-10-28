"use strict";

/* ------------------------------------------------------------------ */
/* The Nexus deck                                                      */
/* ------------------------------------------------------------------ */

const nx = {
  clockDate: document.getElementById("clockDate"),
  statOnline: document.getElementById("statOnline"),
  statSecure: document.getElementById("statSecure"),
  statSecureChip: document.getElementById("statSecureChip"),
  statLoad: document.getElementById("statLoad"),
  statNet: document.getElementById("statNet"),
  healthRing: document.getElementById("healthRing"),
  healthScore: document.getElementById("healthScore"),
  healthStatus: document.getElementById("healthStatus"),
  healthUptime: document.getElementById("healthUptime"),
  healthTemp: document.getElementById("healthTemp"),
  tileCpu: document.getElementById("tileCpu"),
  tileMemory: document.getElementById("tileMemory"),
  tileNetwork: document.getElementById("tileNetwork"),
  tileDisk: document.getElementById("tileDisk"),
  tileGpu: document.getElementById("tileGpu"),
  tilePower: document.getElementById("tilePower"),
  sparkCpu: document.getElementById("sparkCpu"),
  sparkMemory: document.getElementById("sparkMemory"),
  sparkNetwork: document.getElementById("sparkNetwork"),
  sparkDisk: document.getElementById("sparkDisk"),
  sparkGpu: document.getElementById("sparkGpu"),
  sparkPower: document.getElementById("sparkPower"),
  processList: document.getElementById("processList"),
  hostName: document.getElementById("hostName"),
  kernel: document.getElementById("kernel"),
  metricTools: document.getElementById("metricTools"),
  metricSkills: document.getElementById("metricSkills"),
  metricSessions: document.getElementById("metricSessions"),
  metricLatency: document.getElementById("metricLatency"),
  orbWrap: document.getElementById("orbWrap"),
  orbState: document.getElementById("orbState"),
  orbSub: document.getElementById("orbSub"),
  orbTabs: document.getElementById("orbTabs"),
  orbReadouts: [1, 2, 3, 4, 5, 6, 7, 8].map((n) =>
    document.getElementById("orbPct" + n)
  ),
  sessionButton: document.getElementById("sessionButton"),
  latticeButton: document.getElementById("latticeButton"),
  commandDock: document.getElementById("commandDock"),
  configDock: document.getElementById("configDock"),
  systemDock: document.getElementById("systemDock"),
  sessionDrawer: document.getElementById("sessionDrawer"),
  latticeDrawer: document.getElementById("latticeDrawer"),
  drawerScrim: document.getElementById("drawerScrim"),
  drawerClose: document.getElementById("drawerClose"),
  latticeClose: document.getElementById("latticeClose"),
  toolList: document.getElementById("toolList"),
  skillList: document.getElementById("skillList"),
  checkList: document.getElementById("checkList"),
  latticeStatus: document.getElementById("latticeStatus"),
  voiceOrb: document.getElementById("voiceOrb"),
  voiceState: document.getElementById("voiceState"),
  configPanel: document.getElementById("configPanel"),
  configForm: document.getElementById("configForm"),
  configPath: document.getElementById("configPath"),
  configStatus: document.getElementById("configStatus"),
  configClose: document.getElementById("configClose"),
  configCancel: document.getElementById("configCancel"),
  configButton: document.getElementById("configButton"),
  cfgProvider: document.getElementById("cfgProvider"),
  cfgModel: document.getElementById("cfgModel"),
  cfgTemperature: document.getElementById("cfgTemperature"),
  cfgMaxOutput: document.getElementById("cfgMaxOutput"),
  cfgMaxIterations: document.getElementById("cfgMaxIterations"),
  cfgApproval: document.getElementById("cfgApproval"),
  cfgTheme: document.getElementById("cfgTheme"),
  cfgWakeWord: document.getElementById("cfgWakeWord"),
  cfgLanguage: document.getElementById("cfgLanguage"),
  cfgTools: document.getElementById("cfgTools"),
  cfgSkills: document.getElementById("cfgSkills"),
  cfgVoice: document.getElementById("cfgVoice"),
};

const SYSTEM = {
  latest: null,
  busy: false,
  failures: 0,
  orbMode: 0,
  series: { cpu: [], memory: [], network: [], disk: [], gpu: [], power: [] },
};

const ORB_MODES = [
  { name: "CPU", label: "CPU CORE" },
  { name: "MEMORY", label: "MEMORY" },
  { name: "NETWORK", label: "NETWORK" },
  { name: "DISK", label: "DISK I/O" },
];

function humanBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return Math.round(value) + " B";
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let scaled = value / 1024;
  let unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return scaled.toFixed(scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2) + " " + units[unit];
}

function humanRate(bytes) {
  return humanBytes(bytes) + "/s";
}

function humanDuration(seconds) {
  const total = Math.max(0, Math.floor(Number(seconds) || 0));
  const days = Math.floor(total / 86400);
  const hours = Math.floor((total % 86400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (days) return days + "d " + hours + "h";
  if (hours) return hours + "h " + minutes + "m";
  if (minutes) return minutes + "m";
  return total + "s";
}

function pct(value) {
  return Math.round(Math.max(0, Math.min(100, Number(value) || 0))) + "%";
}

function worstDisk(disks) {
  if (!Array.isArray(disks) || !disks.length) return null;
  return disks.reduce((worst, disk) =>
    !worst || Number(disk.percent) > Number(worst.percent) ? disk : worst
  , null);
}

function sparkPoints(values, scaleMax) {
  const max =
    scaleMax === "auto" ? Math.max(1, ...values) : Math.max(1, Number(scaleMax) || 100);
  const width = 100;
  const top = 3;
  const bottom = 22;
  const span = Math.max(2, values.length);
  return values
    .map((value, index) => {
      const x = (index / (span - 1)) * width;
      const ratio = Math.max(0, Math.min(1, (Number(value) || 0) / max));
      const y = bottom - ratio * (bottom - top);
      return x.toFixed(1) + "," + y.toFixed(1);
    })
    .join(" ");
}

function pushSeries(name, value) {
  const series = SYSTEM.series[name];
  if (!series) return;
  series.push(Number(value) || 0);
  if (series.length > 24) series.shift();
}

function drawSpark(name, node) {
  if (!node) return;
  const series = SYSTEM.series[name];
  if (!series.length) return;
  node.setAttribute(
    "points",
    sparkPoints(series, name === "network" ? "auto" : 100)
  );
}

function healthKey(status) {
  const value = String(status || "").toLowerCase();
  if (value === "critical") return "critical";
  if (value === "degraded") return "degraded";
  return "ok";
}

function renderProcesses(processes) {
  nx.processList.innerHTML = "";
  const list = (processes || []).slice(0, 6);
  if (!list.length) {
    const item = document.createElement("li");
    const name = document.createElement("span");
    name.className = "proc-name";
    name.textContent = "no processes visible";
    item.appendChild(name);
    nx.processList.appendChild(item);
    return;
  }
  for (const process of list) {
    const item = document.createElement("li");
    const name = document.createElement("span");
    name.className = "proc-name";
    name.textContent = process.name || "pid " + process.pid;
    name.title = (process.name || "") + " · pid " + process.pid;
    const cpu = document.createElement("span");
    cpu.className = "proc-cpu";
    cpu.textContent = (Number(process.cpu_percent) || 0).toFixed(1) + "%";
    const memory = document.createElement("span");
    memory.className = "proc-mem";
    memory.textContent = humanBytes(process.memory_bytes);
    item.appendChild(name);
    item.appendChild(cpu);
    item.appendChild(memory);
    nx.processList.appendChild(item);
  }
}

function renderSystem(status, latency) {
  SYSTEM.latest = status;
  const system = status.system || {};
  const gateway = status.gateway || {};
  const health = system.health || {};
  const cpu = system.cpu || {};
  const memory = system.memory || {};
  const network = system.network || {};
  const load = system.load || {};
  const disk = worstDisk(system.disks);
  const diskPercent = disk ? Number(disk.percent) || 0 : 0;

  pushSeries("cpu", cpu.usage_percent);
  pushSeries("memory", memory.percent);
  pushSeries(
    "network",
    (Number(network.rx_bytes_per_second) || 0) + (Number(network.tx_bytes_per_second) || 0)
  );
  pushSeries("disk", diskPercent);
  pushSeries("gpu", system.gpu ? system.gpu.usage_percent : 0);
  pushSeries("power", system.battery ? system.battery.percent : 100);
  drawSpark("cpu", nx.sparkCpu);
  drawSpark("memory", nx.sparkMemory);
  drawSpark("network", nx.sparkNetwork);
  drawSpark("disk", nx.sparkDisk);
  drawSpark("gpu", nx.sparkGpu);
  drawSpark("power", nx.sparkPower);

  const score = Math.max(0, Math.min(100, Number(health.score) || 0));
  nx.healthScore.textContent = String(Math.round(score));
  nx.healthRing.style.setProperty("--score", (score * 3.6).toFixed(1) + "deg");
  nx.healthRing.dataset.status = healthKey(health.status);
  nx.healthStatus.textContent = String(health.status || "unknown").toUpperCase();
  nx.healthUptime.textContent = humanDuration(system.uptime_seconds);
  nx.healthTemp.textContent =
    cpu.temperature_c === null || cpu.temperature_c === undefined
      ? "n/a"
      : Math.round(Number(cpu.temperature_c)) + "°C";

  nx.tileCpu.textContent = pct(cpu.usage_percent);
  nx.tileMemory.textContent = pct(memory.percent);
  nx.tileNetwork.textContent = humanRate(
    (Number(network.rx_bytes_per_second) || 0) + (Number(network.tx_bytes_per_second) || 0)
  );
  nx.tileDisk.textContent = disk ? pct(diskPercent) : "n/a";
  nx.tileGpu.textContent =
    system.gpu && system.gpu.usage_percent !== null && system.gpu.usage_percent !== undefined
      ? pct(system.gpu.usage_percent)
      : "n/a";
  nx.tilePower.textContent = system.battery
    ? pct(system.battery.percent)
    : "AC";

  nx.statOnline.textContent = "ONLINE";
  nx.statSecure.textContent = gateway.secure ? "SEALED" : "OPEN";
  nx.statSecureChip.classList.toggle("secure", !!gateway.secure);
  nx.statSecureChip.classList.toggle("warn", !gateway.secure);
  nx.statLoad.textContent = (Number(load.one) || 0).toFixed(2);
  nx.statNet.textContent = humanRate(
    (Number(network.rx_bytes_per_second) || 0) + (Number(network.tx_bytes_per_second) || 0)
  );

  nx.hostName.textContent = (system.host && system.host.hostname) || "local";
  nx.kernel.textContent = (system.host && system.host.kernel) || "unknown";
  nx.metricTools.textContent = String(gateway.tools ?? "--");
  nx.metricSkills.textContent = String(gateway.skills ?? "--");
  nx.metricSessions.textContent = String(gateway.sessions ?? "--");
  nx.metricLatency.textContent =
    latency === undefined || latency === null ? "--" : Math.max(1, Math.round(latency)) + "ms";

  renderProcesses(system.processes);
  renderOrb();
}

function orbReadouts(system) {
  const cpu = system.cpu || {};
  const memory = system.memory || {};
  const network = system.network || {};
  const load = system.load || {};
  const host = system.host || {};
  const disk = worstDisk(system.disks);
  const interfaces = Array.isArray(network.interfaces) ? network.interfaces : [];
  const busiest = interfaces.reduce(
    (top, entry) =>
      !top ||
      (Number(entry.rx_bytes_per_second) || 0) + (Number(entry.tx_bytes_per_second) || 0) >
        (Number(top.rx_bytes_per_second) || 0) + (Number(top.tx_bytes_per_second) || 0)
        ? entry
        : top,
    null
  );

  switch (ORB_MODES[SYSTEM.orbMode].name) {
    case "MEMORY":
      return [
        "USED " + pct(memory.percent),
        "USED " + humanBytes(memory.used_bytes),
        "FREE " + humanBytes(memory.available_bytes),
        "TOTAL " + humanBytes(memory.total_bytes),
        "SWAP " + pct(memory.swap_percent),
        "SWAP " + humanBytes(memory.swap_used_bytes),
        "SWAP TOTAL " + humanBytes(memory.swap_total_bytes),
        "SWAP FREE " + humanBytes((memory.swap_total_bytes || 0) - (memory.swap_used_bytes || 0)),
      ];
    case "NETWORK":
      return [
        "RX " + humanRate(network.rx_bytes_per_second),
        "TX " + humanRate(network.tx_bytes_per_second),
        "RX TOTAL " + humanBytes(network.rx_bytes_total),
        "TX TOTAL " + humanBytes(network.tx_bytes_total),
        "IFACES " + interfaces.length,
        busiest ? busiest.name : "IFACE —",
        busiest ? "TOP RX " + humanRate(busiest.rx_bytes_per_second) : "TOP RX —",
        busiest ? "TOP TX " + humanRate(busiest.tx_bytes_per_second) : "TOP TX —",
      ];
    case "DISK":
      return disk
        ? [
            "USED " + pct(disk.percent),
            "USED " + humanBytes(disk.used_bytes),
            "FREE " + humanBytes(disk.available_bytes),
            "TOTAL " + humanBytes(disk.total_bytes),
            "MOUNT " + disk.mount,
            "FS " + (disk.filesystem || "—"),
            "DISKS " + (system.disks || []).length,
            system.battery
              ? "BAT " + Math.round(Number(system.battery.percent) || 0) + "%"
              : "BAT n/a",
          ]
        : ["DISK —", "DISK —", "DISK —", "DISK —", "DISK —", "DISK —", "DISK —", "DISK —"];
    default:
      return [
        "CPU " + pct(cpu.usage_percent),
        "CORES " + (cpu.cores ?? "—"),
        "LOAD1 " + (Number(load.one) || 0).toFixed(2),
        "LOAD5 " + (Number(load.five) || 0).toFixed(2),
        "LOAD15 " + (Number(load.fifteen) || 0).toFixed(2),
        cpu.temperature_c === null || cpu.temperature_c === undefined
          ? "TEMP n/a"
          : "TEMP " + Math.round(Number(cpu.temperature_c)) + "°C",
        "PROCS " + ((system.processes || []).length || "—"),
        "ARCH " + (host.arch || "—"),
      ];
  }
}

function orbSubtitle(system) {
  const cpu = system.cpu || {};
  const memory = system.memory || {};
  const network = system.network || {};
  const disk = worstDisk(system.disks);
  switch (ORB_MODES[SYSTEM.orbMode].name) {
    case "MEMORY":
      return humanBytes(memory.used_bytes) + " of " + humanBytes(memory.total_bytes) + " in use";
    case "NETWORK":
      return (
        ((network.interfaces || []).length || 0) +
        " interfaces · " +
        humanRate(
          (Number(network.rx_bytes_per_second) || 0) +
            (Number(network.tx_bytes_per_second) || 0)
        )
      );
    case "DISK":
      return disk ? disk.mount + " · " + pct(disk.percent) + " used" : "no mounted filesystem";
    default:
      return (
        (cpu.cores || "—") +
        " cores · " +
        (cpu.model ? String(cpu.model).slice(0, 42) : "processor model unavailable")
      );
  }
}

function renderOrb() {
  const status = SYSTEM.latest;
  const mode = ORB_MODES[SYSTEM.orbMode];
  nx.orbState.textContent = status ? mode.label : "STANDBY";
  if (!status) {
    nx.orbSub.textContent = "awaiting telemetry";
    nx.orbReadouts.forEach((node) => {
      if (node) node.textContent = "--";
    });
    return;
  }
  const system = status.system || {};
  nx.orbSub.textContent = orbSubtitle(system);
  const values = orbReadouts(system);
  nx.orbReadouts.forEach((node, index) => {
    if (node) node.textContent = values[index] || "—";
  });
  if (nx.orbTabs) {
    nx.orbTabs.querySelectorAll("span").forEach((tab, index) => {
      tab.classList.toggle("active", index === SYSTEM.orbMode);
    });
  }
}

async function pollSystem() {
  if (SYSTEM.busy) return;
  SYSTEM.busy = true;
  const started = performance.now();
  try {
    const status = await api("/v1/system");
    renderSystem(status, performance.now() - started);
    SYSTEM.failures = 0;
  } catch (e) {
    SYSTEM.failures += 1;
    if (SYSTEM.failures === 1) {
      nx.statOnline.textContent = "OFFLINE";
      nx.healthStatus.textContent = "telemetry lost";
      nx.healthRing.dataset.status = "degraded";
      nx.orbSub.textContent = "telemetry unavailable";
    }
  } finally {
    SYSTEM.busy = false;
  }
}

function startSystem() {
  pollSystem();
  window.setInterval(pollSystem, 2500);
}

/* Drawers ---------------------------------------------------------- */

function openDrawer(drawer) {
  closeDrawers();
  nx.drawerScrim.hidden = false;
  drawer.hidden = false;
}

function closeDrawers() {
  nx.sessionDrawer.hidden = true;
  nx.latticeDrawer.hidden = true;
  nx.drawerScrim.hidden = true;
  document.body.classList.remove("system-drawer-open");
}

function fillLatticeList(list, entries, formatter) {
  list.innerHTML = "";
  if (!entries.length) {
    const item = document.createElement("li");
    item.textContent = "nothing found";
    list.appendChild(item);
    return;
  }
  for (const entry of entries) {
    const item = document.createElement("li");
    formatter(item, entry);
    list.appendChild(item);
  }
}

function renderChecks() {
  const status = SYSTEM.latest;
  const checks = status && status.system && status.system.health
    ? status.system.health.checks || []
    : [];
  nx.checkList.innerHTML = "";
  if (!checks.length) {
    const item = document.createElement("li");
    item.textContent = status ? "no checks reported" : "awaiting telemetry";
    nx.checkList.appendChild(item);
    return;
  }
  for (const check of checks) {
    const item = document.createElement("li");
    item.className = healthKey(check.status === "warn" ? "degraded" : check.status);
    const name = document.createElement("b");
    name.textContent = check.name || "check";
    const detail = document.createElement("small");
    detail.textContent = (check.status || "") + " — " + (check.detail || "");
    item.appendChild(name);
    item.appendChild(detail);
    nx.checkList.appendChild(item);
  }
}

async function loadLattice() {
  nx.latticeStatus.textContent = "scanning";
  nx.toolList.innerHTML = "";
  nx.skillList.innerHTML = "";
  const [tools, skills] = await Promise.allSettled([
    api("/v1/tools"),
    api("/v1/skills"),
  ]);

  let toolCount = 0;
  let skillCount = 0;

  if (tools.status === "fulfilled") {
    const names = tools.value.tools || [];
    toolCount = names.length;
    fillLatticeList(nx.toolList, names, (item, name) => {
      const label = document.createElement("b");
      label.textContent = name;
      item.appendChild(label);
    });
  } else {
    fillLatticeList(nx.toolList, ["tools unavailable"], (item, text) => {
      item.textContent = text;
    });
  }

  if (skills.status === "fulfilled") {
    const list = skills.value.skills || [];
    skillCount = list.length;
    fillLatticeList(nx.skillList, list, (item, skill) => {
      const label = document.createElement("b");
      label.textContent = skill.name;
      const note = document.createElement("small");
      note.textContent = (skill.description || "no description") + " · " + (skill.source || "unknown");
      item.appendChild(label);
      item.appendChild(note);
    });
  } else {
    fillLatticeList(nx.skillList, ["skills unavailable"], (item, text) => {
      item.textContent = text;
    });
  }

  nx.latticeStatus.textContent = toolCount + " tools · " + skillCount + " skills";
  renderChecks();
}

/* Configuration ---------------------------------------------------- */

function setConfigValue(node, value) {
  if (!node) return;
  if (node.type === "checkbox") {
    node.checked = String(value).toLowerCase() === "on";
  } else {
    node.value = value === undefined || value === null ? "" : String(value);
  }
}

function applyConfig(data) {
  const settings = data.config || {};
  nx.configPath.textContent = data.path || "configuration file";
  setConfigValue(nx.cfgProvider, settings.provider);
  setConfigValue(nx.cfgModel, settings.model);
  setConfigValue(nx.cfgTemperature, settings.temperature);
  setConfigValue(nx.cfgMaxOutput, settings.max_output_tokens);
  setConfigValue(nx.cfgMaxIterations, settings.max_iterations);
  setConfigValue(nx.cfgApproval, settings.approval);
  setConfigValue(nx.cfgTheme, settings.theme);
  setConfigValue(nx.cfgWakeWord, settings.wake_word);
  setConfigValue(nx.cfgLanguage, settings.language);
  setConfigValue(nx.cfgTools, settings.tools);
  setConfigValue(nx.cfgSkills, settings.skills);
  setConfigValue(nx.cfgVoice, settings.voice);
}

async function loadConfig() {
  nx.configStatus.textContent = "reading configuration…";
  try {
    const data = await api("/v1/config");
    applyConfig(data);
    nx.configStatus.textContent =
      "loaded · saved changes take effect after the gateway restarts";
  } catch (e) {
    nx.configStatus.textContent = "the configuration could not be read: " + e.message;
  }
}

function collectConfig() {
  return {
    provider: nx.cfgProvider.value,
    model: nx.cfgModel.value.trim(),
    temperature: nx.cfgTemperature.value.trim() || "default",
    max_output_tokens: nx.cfgMaxOutput.value.trim(),
    max_iterations: nx.cfgMaxIterations.value.trim(),
    approval: nx.cfgApproval.value,
    theme: nx.cfgTheme.value,
    wake_word: nx.cfgWakeWord.value.trim(),
    language: nx.cfgLanguage.value.trim(),
    tools: nx.cfgTools.checked ? "on" : "off",
    skills: nx.cfgSkills.checked ? "on" : "off",
    voice: nx.cfgVoice.checked ? "on" : "off",
  };
}

async function saveConfig(event) {
  event.preventDefault();
  nx.configStatus.textContent = "saving configuration…";
  try {
    const response = await fetch("/v1/config", {
      method: "PATCH",
      headers: Object.assign({ "content-type": "application/json" }, apiHeaders()),
      body: JSON.stringify(collectConfig()),
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(data.error || "the gateway answered " + response.status);
    }
    applyConfig(data);
    nx.configStatus.textContent =
      "saved · restart the gateway to apply provider, model, tool, skill and voice changes";
  } catch (e) {
    nx.configStatus.textContent = "the configuration was not saved: " + e.message;
  }
}

function openConfig() {
  nx.configPanel.hidden = false;
  loadConfig();
  window.setTimeout(() => nx.cfgProvider.focus(), 30);
}

/* Voice ------------------------------------------------------------ */

function syncVoiceState() {
  if (!nx.voiceState) return;
  const label = state.voice.listening
    ? "listening"
    : state.voice.handsfree
      ? "handsfree"
      : state.voice.live
        ? "live"
        : state.voice.speakReplies
        ? "voice ready"
        : "voice idle";
  nx.voiceState.textContent = label;
  nx.voiceOrb.classList.toggle("listening", state.voice.listening);
}

/* Wiring ----------------------------------------------------------- */

function startNexus() {
  nx.sessionButton.addEventListener("click", () => {
    if (nx.sessionDrawer.hidden) {
      openDrawer(nx.sessionDrawer);
    } else {
      closeDrawers();
    }
  });
  nx.latticeButton.addEventListener("click", () => {
    if (nx.latticeDrawer.hidden) {
      openDrawer(nx.latticeDrawer);
      loadLattice();
    } else {
      closeDrawers();
    }
  });
  nx.systemDock.addEventListener("click", () => {
    if (window.matchMedia("(max-width: 880px)").matches) {
      const open = !document.body.classList.contains("system-drawer-open");
      closeDrawers();
      if (open) {
        document.body.classList.add("system-drawer-open");
        nx.drawerScrim.hidden = false;
      }
    } else {
      const panel = document.querySelector(".system-panel");
      if (panel) panel.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  });
  nx.commandDock.addEventListener("click", () => {
    el.input.focus();
    commandHelp();
  });
  nx.configDock.addEventListener("click", openConfig);
  nx.configButton.addEventListener("click", openConfig);
  nx.drawerClose.addEventListener("click", closeDrawers);
  nx.latticeClose.addEventListener("click", closeDrawers);
  nx.drawerScrim.addEventListener("click", closeDrawers);

  nx.configForm.addEventListener("submit", saveConfig);
  nx.configClose.addEventListener("click", () => {
    nx.configPanel.hidden = true;
  });
  nx.configCancel.addEventListener("click", () => {
    nx.configPanel.hidden = true;
  });

  nx.orbWrap.addEventListener("click", () => {
    SYSTEM.orbMode = (SYSTEM.orbMode + 1) % ORB_MODES.length;
    renderOrb();
  });
  nx.orbWrap.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      SYSTEM.orbMode = (SYSTEM.orbMode + 1) % ORB_MODES.length;
      renderOrb();
    }
  });
  nx.orbTabs.querySelectorAll("span").forEach((tab, index) => {
    tab.addEventListener("click", () => {
      SYSTEM.orbMode = index;
      renderOrb();
    });
  });

  document.querySelectorAll(".quick-command").forEach((button) => {
    button.addEventListener("click", () => {
      const prompt = button.dataset.prompt || "";
      if (!prompt) return;
      el.input.value = prompt;
      growInput();
      send();
    });
  });

  nx.voiceOrb.addEventListener("click", () => {
    if (state.voice.listening) {
      stopListening();
      return;
    }
    startListening("send");
  });

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    if (!nx.configPanel.hidden) {
      nx.configPanel.hidden = true;
      return;
    }
    if (!nx.sessionDrawer.hidden || !nx.latticeDrawer.hidden || document.body.classList.contains("system-drawer-open")) {
      closeDrawers();
    }
  });

  syncVoiceState();
  startSystem();
}



