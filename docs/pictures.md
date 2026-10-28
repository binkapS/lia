# Pictures

A model that can see can be asked about a diagram, a screenshot, or a
photograph of a bug. Pictures go in the same message as the words, so the
assistant has both at once.

In the terminal:

```sh
lia prompt "what is wrong in this screenshot?" --image shot.png
```

In the TUI, `/image shot.png` attaches a picture, and it goes out with the
next message. Over HTTP, the pictures ride along with the message:

```json
{
  "content": "what is wrong in this screenshot?",
  "images": [{"media_type": "image/png", "data": "<base64>"}]
}
```

On Telegram, Discord and Slack, a picture that somebody sends is fetched and
passed on by itself — a message may be nothing but a picture, and it still
arrives. Pictures bigger than five megabytes are left where they are, and the
words are still answered.

Not every model can see. When the provider refuses a picture, the turn is
tried again with the picture described in words instead of sent, and a note
says so — the conversation carries on rather than stopping.
