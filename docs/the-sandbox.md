# The sandbox

Approval asks whether a command may run. A sandbox is the other half of the
answer: it makes the command harmless rather than merely permitted.

```toml
[sandbox]
enabled = true
network = true      # false cuts the command off from the network
hide_home = false   # true hides everything in the home directory but the workspace
```

The sandbox is bubblewrap, a small program that sets up a namespace and
mounts things into it. The workspace is bound into it read-write; the rest
of the disk is read-only; `/tmp` is a scratch space of its own; and the
command gets its own processes. The network is left alone unless it is
turned off.

bubblewrap is not always there, and it does not always work — inside a
container, or where the kernel says no. So it is asked first, once, with a
harmless command, and the answer decides: if it works, the commands run
inside it; if it does not, the commands run as they did before, and a
warning says why. Nothing the assistant does fails because a sandbox was
not available.

A question that might have a passing answer is asked more than once. A
bubblewrap that is momentarily busy — being replaced, say — would
otherwise leave every later command running free for the rest of the
session, so a failure that could go away is tried again before it is
believed.
