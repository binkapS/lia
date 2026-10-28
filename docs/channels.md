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

**WhatsApp** uses the Cloud API. Meta pushes new messages to a webhook, so
there is nothing to poll, and the webhook must be reachable over HTTPS:

```sh
export WHATSAPP_TOKEN=...                 # access token
export WHATSAPP_PHONE_NUMBER_ID=...       # the sending number's id
export WHATSAPP_VERIFY_TOKEN=...          # a string you choose, echoed by Meta
export WHATSAPP_APP_SECRET=...            # checks webhook signatures
export WHATSAPP_ALLOWED_NUMBERS=15551234567   # comma separated, with country code
```

The webhook listens on `127.0.0.1:8788` at `/webhooks/whatsapp`. Point a
tunnel at it (or move it with `WHATSAPP_WEBHOOK_HOST` and
`WHATSAPP_WEBHOOK_PORT`) and give Meta the public HTTPS address and the
verify token. A delivery is checked against `WHATSAPP_APP_SECRET` when it is
set; without it the webhook still works, but anyone who knows the address
could post a message, so it is worth setting. Only numbers on the allow-list
may talk to the assistant, and an empty list means nobody can. WhatsApp text
messages are supported in both directions; status updates and media are
ignored. `WHATSAPP_API_ROOT` moves the Graph API version when Meta retires
the default one.

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

On Telegram, a private message from somebody who is not on the allow-list
is answered with a pairing code. The owner approves it in the terminal:

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
