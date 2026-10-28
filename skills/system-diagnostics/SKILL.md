---
name: system-diagnostics
description: Run a structured diagnostic scan of this machine and turn the readings into findings, severity and concrete next steps.
---

# System diagnostics

Use this for "run diagnostics", "scan the system", "analyze the machine" or
when the user reports slowness without naming a cause. The scan is read-only.

## Scan

1. Call `system_diagnostics` for the scored checks and findings.
2. Call `system_health` with `detail: "full"` when you need the process table
   or the individual checks in front of you.
3. Call `system_processes` with `sort: "cpu"` and `sort: "memory"` when the
   findings point at load or memory.
4. Call `system_network` when throughput or connectivity is part of the
   complaint.
5. Call `security_status` only when the user asked about security, or when a
   finding is about exposure rather than performance.

Take the readings close together. Do not report a CPU spike from one sample
and a memory spike from another without saying they were separate moments.

## Report

Use this shape:

```text
DIAGNOSTICS: degraded (score 61/100)
Findings
  1. memory — 91% in use; the largest consumer is <process> (<size>)
  2. disk — / is 92% full
Healthy
  CPU, load, network, temperature

Next steps
  1. <the smallest action that addresses finding 1>
  2. <the smallest action that addresses finding 2>
```

Order findings by severity, then by how much they explain the symptom. Say
which checks were healthy as well as which were not: "nothing else is wrong"
is useful.

A single reading is a moment. If a finding is marginal, say what would make
it certain — another sample, a longer run, or a specific log.

## Boundaries

- Do not kill, restart or renice a process as part of a scan. Propose the
  command and let the user approve it.
- Do not delete files or clear caches to raise a score.
- Do not describe a warning as a failure. A busy machine is not a broken one.
- If the machine is healthy, say so plainly and stop. Do not manufacture work.
