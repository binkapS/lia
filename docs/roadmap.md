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

The goal is to help Lia move from answering requests to helping someone keep
track of their day. Build these as opt-in, connected workflows on top of the
existing agent, schedule, memory, and channel foundations.

### First: a personal inbox and task list

- Add a persistent, user-level inbox for quick capture from the terminal, web
  UI, or an allowed chat. Captures should work without deciding immediately
  whether something is a task, note, or reminder.
- Let the user review and promote inbox items into tasks, notes, or scheduled
  reminders; support due dates, priorities, completion, snooze, and archive.
- Make the list available across sessions and channels, unlike conversation-
  scoped task tracking. Provide concise commands to capture, list, complete,
  and defer items.

**Done when:** A user can capture an item from any enabled surface and later
find and update the same item from another, with state surviving restarts.

### Next: daily briefings and routines

- Offer opt-in morning and end-of-day briefings assembled from due tasks,
  reminders, scheduled work, and recent user-approved notes.
- Let users choose the time, timezone, delivery channel, length, and included
  sources; make each briefing easy to pause or reschedule.
- Support lightweight recurring routines such as a daily plan, weekly review,
  or end-of-day wrap-up, with answers saved only when the user asks.

**Done when:** A user can configure a useful briefing once, receive it at the
expected local time, and see why each item was included.

### Then: calendar awareness

- Start with an optional, read-only calendar connector (CalDAV first) that can
  show today's agenda, upcoming events, and free/busy windows.
- Add event-aware planning: flag task/calendar conflicts and suggest time
  blocks without silently changing events or making commitments.
- Support multiple calendars and explicit timezone handling; show the source
  and last-sync time for calendar data.

**Done when:** Lia can answer agenda and availability questions from a
configured calendar while remaining useful when it is disconnected.

### Later: follow-ups and connected services

- Let users explicitly mark a conversation outcome as a commitment or
  follow-up, with an owner, due date, and reminder. Ask before turning an
  inferred commitment into a persistent task.
- Add optional read-only email triage after the inbox and calendar workflows
  are established: summarize selected messages, identify requested actions,
  and create draft tasks or replies for review.
- Treat sending messages, editing calendar events, or other external writes as
  separate, explicitly approved actions; show a preview and the target before
  execution.

**Done when:** Follow-ups are traceable to their source, and external services
never receive a write action without a clear user confirmation.

### Product boundaries

- Keep personal tasks and notes local by default, with clear export and delete
  controls. Make every external connector opt-in and explain what data it can
  read or change.
- Prefer useful manual workflows before adding integrations; connectors
  should not be required for capture, task tracking, or briefings.
- Avoid unsolicited interruptions by default. Let users choose which events
  can notify them, where, and when.

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
