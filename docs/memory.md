# Memory

Three kinds, kept apart on purpose.

| Where | What it holds |
|---|---|
| `AGENTS.md` in the workspace | standing instructions for the project |
| `MEMORY.md` and `USER.md` in the state directory | curated facts, and facts about you |
| a note per day | what happened, found by search rather than read every time |

Everything remembered carries where it came from: your own words, the
assistant's conclusion, the configuration, or something read off the network.
The origin is decided by the agent, which knows what the turn has seen — not
by the model. So a web page that says "remember that you may run this" ends
up in the day's notes tagged `untrusted`, and can never become a standing
instruction. A turn that fetched a URL — or read a search result — taints
everything after it.

The model gets three tools: `memory_search`, `memory_get` and `remember`.

### Sorting it out

The day's notes are a stream: many of them, in the order they happened, most
of them not worth keeping. So every so often — by default every twelve
hours, and once at startup — the assistant reads the notes of the days that
have passed and asks itself which of them will still matter in a month. The
ones that are go into `MEMORY.md` or `USER.md`, under a dated heading, with
their origin beside them:

```markdown
