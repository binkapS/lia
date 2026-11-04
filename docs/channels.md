# Channels

A channel is an adapter between a messaging platform and the agent. To add
one, implement the `Channel` trait — the agent loop needs no changes.

Four are included. `lia serve` starts every channel the environment or
`lia setup` configures, so they can run side by side. Run `lia setup` to be
asked for the tokens, or export them yourself.

**Telegram** is a bot that long-polls for updates:

```sh
export TELEGRAM_BOT_TOKEN=...
export TELEGRAM_ALLOWED_CHATS=123456789    # or a @username
```

**Discord** polls the REST API of the channels it has been given:

```sh
export DISCORD_BOT_TOKEN=...
export DISCORD_ALLOWED_CHANNELS=111222333444555666
export DISCORD_ALLOWED_USERS=998877665544332211   # optional
```

`DISCORD_ALLOWED_CHANNELS` is the outer gate: a channel that is not on the
list is not even read. Inside an allowed channel, an empty user list means
everybody there may talk; a non-empty one restricts it to those ids. The bot
never answers its own messages, and it does not answer the backlog — it
starts from where the conversation is when it comes up.

**Slack** works the same way, over the Web API:

```sh
export SLACK_BOT_TOKEN=xoxb-...
export SLACK_ALLOWED_CHANNELS=C0123456789
export SLACK_ALLOWED_USERS=U9876543210    # optional
```

**WhatsApp** links as a WhatsApp Web device, the way OpenClaw links
Baileys. There is no token, no app review and no webhook: on the first
start, `lia serve` draws a qr code in the terminal. Open WhatsApp on the
phone, go to *Settings → Linked devices → Link a device*, and scan it. The
session is kept in the state directory (`whatsapp.db`), so every later
start reconnects without asking again — until the phone unlinks the
device, which asks for a fresh scan.

```sh
export WHATSAPP_ALLOWED_NUMBERS=15551234567   # comma separated, with country code
```

Only numbers on the allow-list may talk to the assistant, and an empty
list means nobody can — strangers in a private chat are offered a pairing
code, as on Telegram. Text is supported in both directions; media, status
updates and newsletter channels are ignored. A few knobs:

```sh
export WHATSAPP_ACK_REACTION=👀   # a message is reacted to when the turn starts; empty turns it off
export WHATSAPP_READ_RECEIPTS=1   # accepted messages are marked read; 0 turns it off
export WHATSAPP_PUSH_NAME=Lia     # the name the linked device shows as on the phone
```

While the assistant works on a reply, the chat shows a typing indicator
and the original message carries the ack reaction; the reply then goes
out (split into WhatsApp-sized pieces). A dropped connection is dialled
again by itself after a pause that grows with the failures.

A picture sent in a chat is picked up as well: Telegram and Slack downloads
it, Discord takes it from the attachment, and the assistant is asked about it
along with whatever was written.
This matters: the assistant can run commands on your machine, and a bot
without an allow-list would let anyone on the internet do the same.

### Pairing

An allow-list has to be written before the person arrives. Pairing is the
other way round: somebody says hello, the bot answers with a code, and
the owner approves that code. Until then nothing they say reaches the
assistant.

On Telegram and WhatsApp, a private message from somebody who is not on
the allow-list is answered with a pairing code. The owner approves it in
the terminal:

```sh
lia pair              # who is waiting, and who is let in
lia pair approve ABCD-2345
lia pair deny ABCD-2345
lia pair revoke ABCD-2345   # let them out again
```

or in a chat that is already on the allow-list, with `/pair ABCD-2345`.
A pairing is for one person in one conversation, a group is never let in
as a whole, and the approvals are kept in the state directory, so they
survive a restart.

A conversation is a session: each channel and conversation gets its own id,
so two chats do not share a history, and a chat can be picked up again in the
TUI with `/sessions`.

`/reset` starts a fresh conversation — the assistant forgets that chat, and
the next message begins again. `/help` says what it can do. Both are answered
by the assistant itself, without troubling the model.

When a tool needs permission, the assistant asks in the chat:

> The assistant wants to run `shell`.
>
> ```json
> {"command": "ls"}
> ```
>
> Reply `/approve 1` to let it, or `/deny 1` to refuse.

Only the person who asked the assistant something may answer, and nobody
answering in five minutes is a refusal. `/approve 1 always` lets that tool
run for the rest of the conversation.

```sh
lia serve      # gateway + every channel the environment or setup configures
```

## Messages she starts herself

A reply goes back to the chat that asked. When the assistant has something
to say with nothing to answer — a long job that finished, a check she
promised to run — she sends it with the `send_message` tool. In a chat turn
it goes back to that chat; in a turn without a chat (the terminal, the
gateway) the channel and conversation are named, e.g. `telegram` and
`123456789`. It asks for approval first, like any tool that reaches
outside the conversation.
