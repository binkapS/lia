---
name: deployment-readiness
description: Check whether this machine and workspace are ready to deploy — health, disk, security posture, build and tests — and produce a go/no-go checklist.
---

# Deployment readiness

Use this for "deploy", "are we ready to ship?", or before a release. It is a
checklist, not a deployment: nothing is pushed, restarted or published
without a separate, explicit request.

## Gather

1. `system_health` — is there room to build, and is the machine stable?
2. `system_diagnostics` — anything degraded that would make a failure hard to
   read?
3. `security_status` — is the gateway exposed, and are approvals sane?
4. In the workspace: read the build and test commands from the project
   (README, Makefile, package manifest, Cargo.toml). Run the tests only when
   the user asked for them or the project's own instructions say to.

Do not run a deploy command as part of a readiness check.

## Checklist

```text
DEPLOYMENT READINESS: HOLD (1 blocker, 2 warnings)

Blockers
  - tests fail: <test> — <first failing assertion>

Warnings
  - disk / is 88% full
  - gateway has no token but is loopback-only

Passing
  - build succeeds
  - machine healthy (score 91/100)
  - no uncommitted changes
```

## Rules

- A blocker is something that makes the release wrong or unsafe to ship.
- A warning is something that will make the next hour harder.
- If the user only wants the machine checked, stop after the health and
  security sections and say what was not checked.
- Report the exact command that failed and the first useful line of its
  output. Do not paste a whole log unless asked.
- Anything that changes the machine — a deploy, a restart, a migration — is
  a separate action that needs approval.
