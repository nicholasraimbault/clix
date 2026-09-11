# Clix

Clix is safer, easier access to your devices for you or your agent. Not a
login. The cloud is the Linux user. Devices are hands and cache.

Before acting, read [current work](plans/current.md); read its linked milestone
when relevant.

## Values

- The owner is the authority. Grants are added on that box, by the owner.
- A hand is a granted tool, not a login and not a leftover session.
- Fail closed. Nothing added means nothing runs through Clix.
- Location stays honest. Named body, or this machine.
- Owner operations remain available with no agent. Agents use the same
  commands, attributable, through the same grants.
- Build clear, coherent systems grounded in observed reality.

## Architecture

Preserve the [accepted design](docs/superpowers/specs/2026-09-11-clix-design.md).
Read it before work that changes or depends on the system design. Live
implementation and proof state belong in [`plans/current.md`](plans/current.md).

## Work

Act autonomously within the request while preserving the values. Changes to
the values or accepted design, and destructive or irreversible actions
beyond the request, require an explicit project decision.

Keep one writer per worktree; use independent review only when it can
materially affect a decision; remove task worktrees and branches after
integration or discard.

Ground claims in the exact revision, artifact, command or running system.
Surface a defeated premise rather than force the intended result. A stub,
ignored test, localhost stand-in for two boxes, SSH as exec, or a silent
fail-open is not the result.

Use Git history only to trace an earlier decision.
