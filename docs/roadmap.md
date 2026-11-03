# Lia roadmap

This roadmap is a set of proposed improvements, not a release schedule. It is
based on the current code and documentation; priorities should be revisited as
users report problems and maintainers choose what to take on.

## What is already here

Lia already has a local-first agent, persistent sessions and memory, provider
retry/fallback, workspace-scoped file tools, command approval and optional
sandboxing, MCP, scheduled work, voice, a web UI, and adapters for several chat
platforms. The roadmap focuses on making these capabilities more dependable,
observable, and easier to extend rather than re-listing them as future work.

## New features: make Lia useful every day

The goal is to help Lia keep track of a person's day, not just answer the
latest message. These are proposed implementation packages, not work already
assigned to permanent maintainers. The four feature tracks were separately
planned by subagents for inbox/tasks, daily routines, calendar, and
follow-ups/integrations; their findings are consolidated below.

### Phase 1: personal inbox and tasks

**F1 — User-scoped records and durable store**

Planning track: inbox and tasks.

- Define a versioned record model for inbox items, tasks, notes, and reminders:
  stable ID, kind, text, state, priority, due/snooze time, timestamps, source
  session/message, and provenance.
- Add a state-directory-backed store with safe writes, explicit update/delete,
  deterministic listing, and migration behavior. Keep the existing
  conversation-scoped todo list compatible.
- Establish the owner/user scope before exposing records across channels.
  Session and channel IDs are not proof of a shared user identity.

**Depends on:** None.

**Complete when:** Records survive restart and schema changes fail safely;
the same authorized owner can access them across sessions without exposing
them to another user.

**F2 — Capture and task lifecycle**

Planning track: inbox and tasks.

- Add capture, list, detail, promote, complete, defer/snooze, archive, and
  delete operations with concise CLI and chat commands.
- Add a small web inbox/task view and authenticated gateway operations.
- Inject the shared store/service into agent tools, CLI, web, and channel
  command handling. Do not let model-supplied IDs select another user's data.
- Keep uncertain captures in the inbox; require user confirmation before
  inferred commitments or promotions become persistent tasks.

**Depends on:** F1.

**Complete when:** An item captured from any enabled surface can be found and
updated from another authorized surface, and ordinary conversation does not
silently create persistent items.

**F3 — Reminder lifecycle and delivery**

Planning track: inbox and tasks.

- Connect reminders to the existing scheduler rather than treating every
  reminder as an unrelated agent job.
- Define timezone, pause/reschedule, missed-run, retry, and duplicate-delivery
  behavior; expose delivery status and target.
- Require explicit notification configuration and route only to the selected
  authorized conversation/channel.

**Depends on:** F1, F2, and the timezone/DST policy established in F4.

**Complete when:** Reminders survive restart, can be snoozed or rescheduled,
and delivery failures or retries are visible and do not silently lose work.

### Phase 2: daily routines and briefings

**F4 — Timezone-aware routine scheduling**

Planning track: daily routines.

- Add an explicit IANA timezone to routines and reminders; define behavior for
  daylight-saving gaps and ambiguous times.
- Support pause, resume, reschedule, and missed-run policy without duplicate
  entries. Preserve existing scheduled-job persistence compatibility.
- Allow the user to select one delivery target; never broadcast by default.

**Depends on:** None for the scheduling design; F3 and F5 use this policy.

**Complete when:** Next-run times are stable across host timezones and DST
transitions, and invalid timezone settings are reported clearly.

**F5 — Briefing source collector and templates**

Planning track: daily routines.

- Build a deterministic, read-only collector for due tasks/reminders,
  scheduled work, and explicitly selected user-approved notes.
- Support morning plan, end-of-day wrap-up, and weekly review templates.
- Apply source filters and length bounds before generation; attribute every
  included item so the user can see why it appeared.
- Do not save generated text to memory automatically. Define deliberate
  behavior when sources are empty or unavailable.

**Depends on:** F1/F2 query APIs and F4's source/time definitions.

**Complete when:** Only configured sources are included, output is bounded and
traceable, and partial source failures are visible.

**F6 — Routine setup and delivery**

Planning track: daily routines.

- Provide opt-in setup and management for time, timezone, source selection,
  length, delivery channel, and pause/resume.
- Reuse the scheduler and channel delivery architecture; persist status,
  last-run, and next-run information.
- Record delivery outcomes and define retry/catch-up behavior after restart.

**Depends on:** F4 and F5.

**Complete when:** A configured briefing arrives at its expected local time,
is delivered only to its chosen target, and can be inspected, paused, or
rescheduled.

### Phase 3: calendar awareness

**F7 — Calendar event and time model**

Planning track: calendar.

- Define normalized event fields for source/calendar, UID, title,
  start/end, all-day status, timezone, and recurrence.
- Distinguish instants from floating local times and all-day dates; bound
  recurrence expansion to requested windows.
- Document timezone precedence and how malformed or missing timezone data is
  handled.

**Depends on:** F4's timezone/DST policy.

**Complete when:** The same events produce consistent results regardless of
  host timezone, including all-day and DST boundary cases.

**F8 — Optional, read-only CalDAV connector**

Planning track: calendar.

- Start with one explicitly configured CalDAV account/calendar and read-only
  discovery, event retrieval, agenda, and free/busy queries.
- Keep disabled by default; store credentials through the existing secrets
  mechanism, not TOML or tool arguments. Bound requests and cache policy.
- Treat event text as untrusted data, never instructions. Do not write calendar
  data into general memory.

**Depends on:** F7.

**Complete when:** A local mock CalDAV server verifies agenda retrieval,
disabled/offline behavior is non-fatal, and no create/update/delete operation
is exposed.

**F9 — Agenda-aware planning**

Planning track: calendar.

- Add source-labelled agenda answers and advisory task/calendar conflict
  detection.
- Suggest time blocks only; do not make commitments or change events.
- Show source and last-sync status and remain useful when disconnected.

**Depends on:** F2/F3 and F8.

**Complete when:** The user can inspect today's agenda and conflicts with
explicit timezone/source context, without Lia modifying calendar data.

### Phase 4: follow-ups and connected services

**F10 — Confirmed commitments and follow-up lifecycle**

Planning track: follow-ups and integrations.

- Create a user-scoped follow-up record linked to the source session/message,
  with owner, due date/timezone, status, provenance, and audit timestamps.
- Add review, list/detail, complete, defer, archive, and optional reminder
  flows. Make inferred commitments proposals until the user confirms them.
- Preserve existing conversation-scoped todos; do not silently promote them.

**Depends on:** F1/F2 and F4.

**Complete when:** Confirmed follow-ups are discoverable across authorized
surfaces, source-traceable, and never created solely from model inference.

**F11 — Read-only email triage and local drafts**

Planning track: follow-ups and integrations.

- Add an optional connector with narrow, explicitly selected mailbox scope.
- Summarize selected messages and propose follow-ups or replies, linked to
  source message IDs. Treat email bodies as untrusted input.
- Keep proposed tasks and reply text as editable local drafts; provide
  disconnect, retention, and delete controls.

**Depends on:** F10 and an explicit connector consent/configuration model.

**Complete when:** The connector is disabled by default, reads only the
  approved scope, and cannot send email or turn a proposal into a commitment
  without user confirmation.

**F12 — Separately approved external actions (optional later phase)**

Planning track: follow-ups and integrations.

- Treat sending email or changing calendar events as separate mutating
  capabilities, never as an extension of read-only triage.
- Require a final per-action preview of account, recipient/target, exact
  content/change, and side effects. Default to deny in headless operation.
- Bind approval to the exact reviewed payload; reject stale drafts and log
  the decision and outcome without logging sensitive content.

**Depends on:** F8/F11 and approval UX that can show and bind the exact
operation.

**Complete when:** No external write occurs without clear, per-action user
approval; prior connector consent or standing tool approval is not treated as
consent to send or modify.

### Implementation status

- Document tools (`document_read`, `document_create`, `document_edit`) handle PDF and DOCX; see `docs/tool-reference.md`.
Implemented as library modules with tests: F1 inbox store (`src/inbox.rs`, CLI `lia inbox`, authenticated `/v1/inbox` routes and web panel), F4 timezone schedules, F5 briefings, F6 routines (`lia routine`, gateway tick loop), F7 calendar model, F8 read-only CalDAV client, F9 agenda helpers, F10 follow-ups, F11 read-only JMAP mail client (`lia mail`) and draft store, F12 approval gate.

Due reminders are pushed by `lia serve` to one opted-in chat. `lia agenda` now wires the calendar (CalDAV via environment secrets), agenda and inbox due items; follow-ups are covered by `lia inbox` defer/complete/archive.

Also delivered: `lia mail send` (the F12 gate: exact preview, interactive `y`, one-time approval; JMAP is the only write path), and opt-in agent tools (`LIA_PERSONAL_TOOLS=1`: inbox, agenda, approval-gated mail read; none can write mail or calendars).

Deliberately not done: agent-initiated sending or calendar writes, and per-person identity across chat channels (the personal tools act for the single local owner, so they are opt-in).

### Delivery order and product boundaries

Recommended order: **F1 → F2 → F4 → F3 → F5 → F6 → F7 → F8 → F9 → F10 → F11
→ F12**. F4 may be designed alongside F1/F2, but timezone behavior must be
settled before reminders, routines, or calendar work ships. F7/F8 can proceed
in parallel with F5/F6 after the timezone policy is stable; F9 still depends
on both tasks/reminders and calendar reads.

- Keep personal tasks and notes local by default, with clear export and delete
  controls. Every external connector is opt-in and must explain what data it
  reads, caches, or changes.
- Keep capture, task tracking, and briefings useful without any external
  connector. Avoid unsolicited interruptions; users choose which events
  notify them, where, and when.
- Never infer cross-channel identity from a session ID. Enforce owner scope
  before adding shared personal records.
- Treat network-derived email and calendar text as untrusted data. Keep
  credentials out of prompts, logs, URLs, and tool arguments.
- Read-only connector approval is not consent for external writes. Require a
  separate, exact-payload confirmation for every external mutation.

## Near term: make changes safer to ship

- **Automated checks:** Add a CI workflow for formatting, linting, Rust tests,
  and the web asset build. Replace the placeholder npm test script with a
  meaningful check or remove it until a frontend test runner is introduced.
- **Behavioral coverage:** Add focused tests for provider streaming and
  cancellation, tool approval across interactive and one-shot sessions,
  schedule persistence/restart behavior, and gateway authentication and
  session boundaries.
- **Consistent failures:** Expand the error taxonomy into stable, user-facing
  categories. Preserve useful provider/tool context in logs while keeping
  secrets out of errors, and document which failures are retryable.
- **Operational guidance:** Improve troubleshooting with actionable checks
  for configuration, provider connectivity, sandbox availability, voice
  dependencies, and channel pairing.

**Done when:** Contributors can run the same checks locally and in CI, and
important failure paths have regression tests and actionable diagnostics.

## Next: improve day-to-day reliability

- **Provider behavior:** Make model and capability selection explicit across
  providers; expose consistent retry/fallback status and test behavior when a
  provider fails mid-stream or returns an unsupported feature.
- **Context management:** Make compaction decisions and token limits visible,
  bound large tool results consistently, and recover from context overflow
  without losing the user's turn.
- **Scheduling:** Make job lifecycle and delivery outcomes inspectable, and
  define restart, retry, and duplicate-delivery behavior for scheduled work.
- **Gateway and channels:** Unify lifecycle, health reporting, and delivery
  error handling across the HTTP/WebSocket gateway and channel adapters.
  Document and test reconnect and shutdown behavior.
- **Web experience:** Add automated coverage for critical chat flows and
  accessible keyboard operation; report connection and tool-approval states
  clearly.

**Done when:** Common transient failures are diagnosable and recover cleanly,
without silently dropping turns or scheduled deliveries.

## Later: make extensions and memory scale

- **Memory quality:** Add explicit duplicate/conflict handling and controls
  for reviewing, correcting, or removing remembered facts. Consider optional
  semantic retrieval behind a configurable local or remote embedding
  provider.
- **Skills lifecycle:** Define validation and compatibility rules for skills,
  then add dependency metadata and a safe import/export workflow. Keep
  third-party skills reviewable before enabling them.
- **Performance and resource use:** Establish representative benchmarks for
  context compaction, search, memory retrieval, and large tool output. Use
  results to guide caching and resource limits rather than adding speculative
  optimization.
- **Diagnostics:** Provide an opt-in, privacy-conscious diagnostics view for
  provider health, tool outcomes, schedule delivery, and resource usage.
  Keep local operation useful without telemetry.

**Done when:** Larger skill collections and long-running use remain manageable,
and optional diagnostics disclose what is collected and where it goes.

## Ongoing expectations

- Keep workspace boundaries, approvals, and secret handling consistent as
  tools and integrations are added.
- Prefer opt-in behavior for network services, telemetry, and semantic
  providers; document data flow and retention.
- Update the relevant user and contributor documentation alongside behavior
  changes.
- Add focused regression tests for bug fixes and avoid expanding dependencies
  without a clear benefit.
