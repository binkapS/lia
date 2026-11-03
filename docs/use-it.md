# Use it

```sh
lia                      # interactive terminal interface
lia setup                # store API keys and bot tokens
lia prompt "fix the tests"   # one prompt, print the answer
lia prompt "what is this?" --image shot.png   # with a picture
echo "explain this" | lia pipe
lia tools                # what the assistant can call
lia models               # the models the provider offers
lia sessions             # past conversations
lia skills               # the skills it can load
lia mcp                  # the MCP servers, and what they offer
lia schedule             # the work promised for later
lia inbox capture "call the dentist" --kind task --priority high
lia inbox list           # open and completed records
lia inbox complete ID
lia inbox defer ID --until 2026-11-04T09:00:00+01:00
lia inbox archive ID
lia inbox delete ID
lia agenda --timezone Europe/Paris   # today's events and due tasks/reminders
```

`lia agenda` always shows open tasks and reminders due today. It adds a
read-only CalDAV calendar when `LIA_CALDAV_URL`, `LIA_CALDAV_USER` and
`LIA_CALDAV_PASSWORD` are set (in the environment or `lia setup`); the
optional `LIA_CALDAV_CALENDAR` names it in the output. Calendar text is shown
as data, and Lia never writes to the calendar.

## Mail (read-only)

```sh
export LIA_JMAP_SESSION_URL=https://api.fastmail.com/jmap/session
export LIA_JMAP_TOKEN=...        # a JMAP API token; or set both with `lia setup`
lia mail read MESSAGE_ID ...     # subject and body of the messages you name
lia mail draft MESSAGE_ID --title "Reply to Sam" --text "..." [--reply]
lia mail send N --to sam@example.com   # shows the email, asks y/N first
lia mail drafts
lia mail delete-draft N
lia mail forget                  # delete every local draft
```

Lia reads only the message IDs you name, over https with your token, and never
moves, flags or deletes mail. Message text is untrusted content. Drafts are
local files; the only way to send one is `lia mail send`, which needs an
interactive terminal, prints the exact recipient, subject and body, and sends
only after you answer `y`. The agent cannot send mail.

## Personal tools for the agent

Set `LIA_PERSONAL_TOOLS=1` to give the agent `inbox_list`, `inbox_capture`
(items are marked assistant-suggested), `agenda` and `mail_read` (asks for
approval on every call). A chat session is not an identity, so every session
that can reach the agent then acts for you: enable this only when every allowed
chat is yours. Off by default.

Reminders surface in `lia agenda` and in routine briefings
(`lia routine create`). To also have `lia serve` push each reminder when it
falls due, set `LIA_REMINDER_CHANNEL` and `LIA_REMINDER_CONVERSATION` to one
chat. Each reminder is pushed once; changing its due time or reopening it
arms it again. Without both variables nothing is pushed.

```sh
lia pair                 # who is waiting to be let in
lia consolidate          # sort the notes out
lia memory               # what it keeps between conversations
lia listen               # record the microphone and print the words
lia speak "hello"        # say a line out loud
lia doctor               # check that everything is in place
```

Inside the TUI:

| Key | Action |
|---|---|
| `ctrl-c` | quit |
| `enter` | send the message |
| `ctrl-j` | a new line in the message |
| `↑` / `↓` | move through the lines, then through the history |
| `ctrl-p` / `ctrl-n` | the history, whatever the cursor is doing |
| `ctrl-←` / `ctrl-→` | one word |
| `home` / `end` | the ends of the line (`ctrl`: of the whole message) |
| `ctrl-u` / `ctrl-k` / `ctrl-w` | cut to the line, to the line's end, to the word |
| `F1` | the help overlay |
| `F2` | open the microphone, and close it again |
| `page up` / `page down` | scroll the transcript |
| `esc` | close a popup, or throw a recording away |
| `y` / `n` / `a` | approve, refuse, or always allow the waiting tool |

Slash commands: `/help`, `/clear`, `/sessions`, `/model`, `/tools`, `/skills`,
`/image`, `/listen`, `/dictate`, `/speak`, `/voice`, `/handsfree`, `/live`,
`/quit`.

Typing `/` opens a command preview above the message box. `↑` and `↓` move
through the matches, `tab` completes the highlighted name, `enter` runs it,
and `esc` puts the preview away until the message changes again.

`/image shot.png` attaches a picture to the next message you send.

`/sessions` opens the list of saved conversations: `↑` and `↓` move through
it, `enter` opens the chosen one, and the transcript it had is shown again.
