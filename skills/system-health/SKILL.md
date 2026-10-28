---
name: system-health
description: Read and explain this machine's health — CPU, memory, disk, network, temperature, battery and top processes — without changing anything.
---

# System health

Use this when the user asks how the machine is doing, before a long build or
download, or when something feels slow. The tools behind this skill only read
the machine. Nothing here needs approval, and nothing here should change
state.

## Read it

Call `system_health` for the overview. Ask for `detail: "full"` when the user
wants the checks or the process table. The default sample is a fraction of a
second; do not make it longer than necessary.

If one number looks alarming, call `system_health` again before treating it
as a fault. A single spike is a moment, not a trend.

## Say it

Lead with the verdict, then the reading:

```text
Status: stable (score 82/100)
CPU 51% across 8 cores · memory 74% · load 0.9
Disk / is 68% full · network 1.2 MB/s down
```

Name the busiest process only when it explains the reading. Convert bytes
into the units a person reads. Do not dump the entire process table unless
the user asked for it.

## Interpret it

- **CPU** — 100% is one core. A process at 400% on an eight-core machine is
  using four cores, not four machines.
- **Load** — compare it with the core count. A load above the core count
  means work is waiting; twice the core count is a problem.
- **Memory** — 75% is busy but normal on a desktop. Above 90%, or swap in
  heavy use, means something must give.
- **Disk** — above 80% is worth mentioning; above 90% is urgent.
- **Temperature** — above 75°C is warm, above 85°C is throttling territory.
  Some machines have no readable sensor; say so instead of guessing.
- **Network** — rates are measured over the sample. Loopback is excluded
  from the throughput line because it is not the wire.
- **Battery** — below 15% while discharging is worth mentioning before heavy
  work.

If a sensor is absent, say it is not available. Do not invent a GPU or a
battery reading.

## Then what

For a specific complaint, hand off to the matching tool:

- `system_processes` when the question is "what is using it?"
- `system_network` when the question is "is it the network?"
- `system_diagnostics` when the user wants a scan and next steps.
- `security_status` when the question is about exposure or protection.

Changing anything — stopping a process, deleting files, changing a setting —
is a separate, approved action. Do not slip it into a health report.
