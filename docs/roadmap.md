# Lia roadmap and implementation plan

This document converts the working roadmap (pasted 2026-10-27) into an actionable plan.

## 1. High priority

### 1.1 Context handling
- Compaction provider-aware model selection (Provider::list_models())
- Per-tool token budgets + truncation helpers
- Overflow recovery surfaces Notice, not fatal, with retry telemetry
- Integration tests for compaction/overflow/large tool output

### 1.2 Filesystem
- copy_path
- move_path
- patch (structured unified diff)
- format_file
- Consistent sandbox enforcement + clear blocked-path errors

### 1.3 Search / web
- grep_codebase
- find_symbols
- read_url_chunked
- cached web search
- retry/backoff + Source metadata

### 1.4 Shell
- allowlist/denylist config + timeout metadata
- structured JSON-ish output mode
- persistent working-directory state

### 1.5 Memory
- memory_upsert, memory_import, memory_export
- vector search when configured
- confidence/conflict detection

### 1.6 Voice
- voice model selection + STT/TTS health checks
- streaming transcription events + interruption support

### 1.7 Task delegation / scheduling / gateway / channels

## 2. Skills roadmap

Built-in skills: project-setup, release-checklist, incident-triage, migration-assistant, performance-profile, security-hardening, context-compaction-doctor.

Platform: versioning, dependencies, test harness, telemetry, caching, marketplace/import.

## 3. Documentation cleanup

Keep README short. Remove/consolidate: kept-on-2026-10-22.md, checking-it-over.md, the-plan.md, another-assistant.md, intro.md.
Add troubleshooting.md, tool-reference.md, skill-authoring.md, architecture diagrams.

## 4. Quality and reliability

Error taxonomy. Integration/property/fuzz tests. Benchmarks. Metrics/logging.

## Suggested first milestone

1. 1.1.3 Overflow recovery as Notice + telemetry
2. 1.2 Filesystem copy/move/patch
3. 3.3 docs/troubleshooting.md

Recommended order: 1.1 -> 1.2 -> 1.3, docs in parallel.
