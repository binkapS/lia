# Skills

A skill is a written procedure the assistant loads when a task needs one. It
is a directory with a `SKILL.md` file: some frontmatter saying what it is for,
and the instructions themselves.

```markdown
---
name: release
description: how to cut and publish a release
---

1. Check the changelog.
2. Bump the version.
3. Tag and push.
```

The catalogue of skills goes into the system prompt, so the assistant knows
what it can load without being told; the instructions themselves are read
only when the task calls for one. Skills are looked for in, in order:

```
<workspace>/skills
<workspace>/.agents/skills
~/.agents/skills
<state dir>/skills
```

The first root wins, so a project can shadow a personal skill by giving it
the same name. `extra_skill_dirs` adds more.
