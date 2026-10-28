# Security

The assistant runs code on your machine, so the boundaries are explicit.

- **The workspace.** Every file tool resolves its path against the workspace
  root and refuses to step outside it — `..` and symlinks included.
- **Approval.** Tools that change the system ask first. The policy is
  `ask`, `allow`, or `deny`; see `[approval]` in the config. A session that
  cannot prompt (a one-shot run) denies risky tools by default rather than
  assuming yes. A chat can prompt: the assistant asks in the conversation,
  and only the person who asked it something may answer.
- **Blocked commands.** The shell tool refuses a short list of destructive
  patterns outright, whatever the approval policy says.
- **A sandbox, where there is one.** Commands can be run inside
  [bubblewrap](https://github.com/containers/bubblewrap): the workspace is
  the one place the command may write, and the rest of the disk can only
  be read. Where bubblewrap is missing, or the kernel says no, the
  commands run as they did before — the assistant keeps working either
  way. See `[sandbox]` in the config.
- **Provenance.** A remembered fact is only as trusted as where it came from.
  Something read off the network is data, never an instruction, however
  confidently it is phrased.
- **Credentials.** Keys live in the environment, not in the config file. The
  gateway binds to `127.0.0.1` and can require a bearer token.
- **Pictures.** A picture sent in a chat is fetched from the platform's own
  servers only, and no redirect is followed, so a message cannot make the
  assistant fetch something from anywhere else — nor take the bot's token
  with it.
