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
