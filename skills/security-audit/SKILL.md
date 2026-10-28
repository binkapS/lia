---
name: security-audit
description: Review this Lia installation's exposure — gateway token and bind, approval policy, shell sandbox, extra paths and configuration permissions — without printing secrets.
---

# Security audit

Use this when the user says "secure", "audit security", "is this exposed?" or
asks how the gateway and tools are protected. The audit reads configuration
and permissions. It does not print secrets and does not change anything.

## Check

Call `security_status`. It reports:

- whether the gateway has a token, and whether it is bound beyond localhost;
- the approval policy for risky tools;
- whether the shell sandbox is enabled, and whether it allows the network;
- how many paths outside the workspace are allowed;
- whether tools and MCP servers are enabled;
- the configuration file's permissions, when they can be read.

Pair it with `system_health` only when the concern is resource exhaustion
rather than exposure.

## Judge

Treat these as critical:

- a gateway with no token, bound to a non-loopback address;
- a configuration file that other users can read while it carries secrets.

Treat these as warnings:

- no token on a loopback-only gateway (local, but any local process can try);
- approval set to `allow` for risky tools;
- the sandbox off;
- extra paths outside the workspace;
- configuration readable by group or other.

Treat these as healthy:

- a token on a gateway that is reachable;
- approval `ask` or `deny`;
- a confined shell;
- configuration mode `0600` or `0400`.

## Report

```text
SECURITY: review suggested (2 warnings, 0 critical)
  ✓ authentication — gateway has a token
  ! sandbox — shell commands run directly when approved
  ! config — ~/.config/lia/config.toml is mode 0644

Recommended
  1. Enable the sandbox in [sandbox] if commands do not need the whole disk.
  2. chmod 600 ~/.config/lia/config.toml
```

Give the smallest change that removes the risk. Never print a token, key or
secret value, even if the user asks for the "full" configuration. Do not
change permissions or settings without an explicit request and approval.

This is a configuration review, not a penetration test. Say that when the
user asks for a guarantee.
