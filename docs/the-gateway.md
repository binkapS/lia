# The gateway

The gateway is a local daemon that owns the agent. The TUI, the CLI, and
messaging channels all talk to the same one, so a conversation started in one
place can be continued in another.

```sh
lia gateway --port 8787
lia serve                    # gateway + the channels the environment or setup configures
```

### HTTP

| Route | Purpose |
|---|---|
| `GET /health` | liveness, model, session count, whether speech is available |
| `GET /` | the interface, in a browser |
| `GET /assets/{file}` | one stylesheet or script of the interface |
| `GET /v1/models` | models the provider offers |
| `GET /v1/tools` | tools the assistant can call |
| `GET /v1/skills` | skills that were found |
| `GET /v1/system` | a snapshot of the machine and the gateway |
| `GET /v1/config` | the current configuration |
| `PATCH /v1/config` | change a few settings at runtime |
| `GET /v1/audio/voices` | voices the built-in speech model offers |
| `POST /v1/audio/speech` | synthesize speech, as wav or pcm |
| `GET /v1/sessions` | list sessions |
| `POST /v1/sessions` | create a session |
| `GET /v1/sessions/{id}` | fetch one session |
| `DELETE /v1/sessions/{id}` | delete a session |
| `POST /v1/sessions/{id}/messages` | send a message, get the reply |

```sh
curl -s localhost:8787/health

SID=$(curl -s -XPOST localhost:8787/v1/sessions \
  -H 'content-type: application/json' -d '{}' | jq -r .id)

curl -s -XPOST "localhost:8787/v1/sessions/$SID/messages" \
  -H 'content-type: application/json' \
  -d '{"content":"list the files here"}' | jq -r .reply

curl -s localhost:8787/v1/system | jq -r .machine.cpu

# The built-in voice, in the shape the OpenAI speech endpoint uses.
curl -s -XPOST localhost:8787/v1/audio/speech \
  -H 'content-type: application/json' \
  -d '{"input":"Hello from lia.","voice":"Juniper"}' -o speech.wav
```

The machine is readable to the assistant too: `system_health`,
`system_processes`, `system_network`, `system_diagnostics` and
`security_status` read the same snapshot the page draws from.

### WebSocket

`ws://localhost:8787/v1/ws` speaks newline-delimited JSON, in either of two
shapes.

The first is a frame in, a frame out. The client sends
`{"type":"send","content":"..."}`; the gateway replies with `event` frames
carrying the turn's progress, then a `done` frame.

The second is an envelope, for a client that wants one answer per request
and an id to match it by:

```json
{"type":"req","id":"1","method":"message.send","params":{"content":"hello"}}
```

The gateway answers when the turn is over:

```json
{"type":"res","id":"1","ok":true,"data":{"sessionId":"...","reply":"..."}}
```

and pushes what happens meanwhile as `{"type":"event","event":"text_delta",
"data":{"text":"..."}}` frames. A request that failed is answered with
`"ok":false` and the reason in `error`.

| Method | Parameters | Answers |
|---|---|---|
| `message.send` | `content`, `sessionId`, `images` | the reply, the session, the usage |
| `message.approve` | `requestId`, `always` | — |
| `message.deny` | `requestId` | — |
| `session.list` | — | the sessions |
| `session.get` | `sessionId` | the session |
| `session.create` | — | the new session |
| `health` | — | what the gateway is |

Both spellings of a name are accepted, so `sessionId` and `session_id` both
work.

### The page

The gateway serves an interface of its own, in the browser:

```sh
lia gateway --port 8787
# the page is at http://127.0.0.1:8787/
```

The page and its stylesheet and scripts are held inside the binary. Nothing
is fetched from anywhere, so it works on a machine with no network, and the
conversation never goes near a third party.

The page is drawn as a deck. A top bar carries the link, the model, the
session and the clock; a SYSTEM MONITOR on the left shows the health of the
machine — CPU, memory, network, disk, GPU and power, with the busiest
processes beneath; the core in the middle is the conversation, wrapped
around an orb that pulses with what the machine is doing; the COMMAND
CONSOLE on the right holds the quick commands and the composer; and the
dock at the bottom opens the microphone and the drawers. The sessions live
in a drawer, so the deck stays quiet during a run.

The source is split across `web/`: `index.html` is the markup, `css/` the
stylesheets, `js/` the behaviour, one file per part of the page. The
gateway serves them under `/assets/`, and everything still ships inside
the binary.

The sessions live in a drawer — the ones the gateway already keeps.
Opening a session reads its history from the disk, so the whole
conversation is there and not only the words that arrive while the page is
open. The newest session is opened when the page loads.

The transcript follows the newest words and stops when you scroll back to
read something older, and a button says where the newest words are. A
tool that wants permission appears in the transcript with its arguments
and three buttons: allow, allow always, deny.

The composer understands the same slash commands as the terminal, adapted
to what a browser can do: `/help`, `/clear`, `/sessions`, `/model`,
`/tools`, `/skills`, `/image`, `/listen`, `/dictate`, `/speak`, `/voice`,
`/handsfree`, and `/quit`. Typing `/` opens a filtered command palette;
`↑` and `↓` move, `tab` completes, `enter` runs, and `esc` closes. The
same index can be opened with the `/ commands` button below the composer.

`/image` opens a file picker and attaches up to four pictures to the next
message. `/listen` and `/dictate` use the browser's speech recognition;
`/speak` and `/voice` use the gateway's configured voice — Phonon, piper,
or another engine that writes a wav — so the page speaks with the same
voice as the terminal, and fall back to the browser's speech synthesis
when the gateway has none. `/handsfree on` listens for "lia", speaks the
answer, and opens the microphone again after the voice finishes. The
listening commands need a browser with speech recognition (Chrome and Edge
have it); spoken replies need either the gateway's synthesizer or a browser
with speech synthesis.

What is spoken is not what the transcript shows. The transcript keeps
every detail — lists, tables, code — but the voice is composed like a
good butler's: it opens with a quiet courtesy ("Right away.", "Of
course, sir."), list rows are read as "first… then… and finally…", code
and wide tables are pointed at with a phrase rather than recited,
symbols and units become the words they stand for (42 percent, 71
degrees, 120 milliseconds), and a bare link is noticed rather than
spelled out. The transcript says everything the voice only gestures at.
The gateway reports whether it can synthesize speech in `/health`, and
the page uses that voice when it can; otherwise it falls back to the
browser's own synthesizer and keeps the most human voice it finds for the
whole visit.

The page is served to anyone who can reach the gateway: it holds no
secrets, it is only a drawing. Everything it draws needs the token, and
the page asks for it. The token can be given in the address, which is
then scrubbed from it:

```
http://127.0.0.1:8787/?token=the-token
```

With no token in the address, the page asks for one and keeps it for the
visit. A browser cannot put a header on a WebSocket, so the socket
carries the token in its own address; every other request carries it in
the `Authorization` header.
