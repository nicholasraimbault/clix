# Current work

**Implementation and automated validation completed for this pass.** The
[completion record](2026-09-12-product-completion.md) and
[exact evidence](evidence/2026-09-12/product-completion/README.md) identify what
was run; physical and sustained-use checks below remain unproved.

The [accepted design](../docs/superpowers/specs/2026-09-11-clix-design.md) is
unchanged. The prepared v0 plan is a recipe, not proof. Work stayed on master,
without cloning. The owner authorized pushing and making the repository public;
[publication history](2026-09-12-publication.md#publication) records that decision.
Public machine names `laptop` and `server` are pseudonyms.

## What exists and is proved

**The granted hand exists beyond the CLI shape.** The actual laptop (CachyOS,
Arch-derived) and server (Debian 13.6) use Clix's authenticated TLS transport and
granted-binary execution. SSH controls the server CLI; it does not execute the
laptop hand. The only standing laptop grant is `/usr/bin/adb`, restricted to
server. Server has no grants. Revoke locally with `clix remove adb`.

The latest [remote smoke](evidence/2026-09-12/product-completion/two-machine-smoke.json)
returned exit 0 from `adb devices`, with exact bytes and matching signed jobs.
The phone was disconnected. The earlier
[attached-phone result](evidence/2026-09-11/two-machine.log) remains separate
historical proof, including its failed immediate convergence assertion and
[successful follow-up](evidence/2026-09-11/log-convergence.log).

Signed shared history now includes local Clix jobs and failures before runner
admission. Imported history never schedules execution or grants authority.
Actual two-machine fixtures proved replication, conflict refusal with zero
execution, owner pin recovery and once-reservation preservation across runner
process death. Three-body relay and unknown-author rescan are loopback tests;
they are not additional physical machines.

Owner job inspection, saved output, explicit linked retry, output pruning, pin
conflict decisions and retained-version recovery are implemented. State,
retained output, connections, waiting clients and direct child admission have
explicit limits. Actual ENOSPC in isolated private tmpfs proved fail-closed
completion/startup behavior while retaining owner inspection.

**196 tests passed natively on both machines, zero failed or ignored.** Fmt,
clippy with warnings denied and release builds passed. Both installed services
are enabled and active on the tested binaries. Upgrades preserved every
preexisting decoded state field and added signed history migration. See
[operation and recovery](../docs/operations.md) for commands and upgrade limits.

At 10,000 synthetic jobs, the measured final release inspected one job in about
3.8 ms and rendered full log in 199 ms. Peak daemon memory was about 291 MiB;
grant changes still rewrote about 17 MB. These are measured costs, not production
scale or indefinite-operation guarantees.

## Review-fix pass (2026-09-24)

An outside review was checked against the code. The owner-approved fixes are
recorded in the [review-fix plan](2026-09-24-review-fixes.md) and in the design's
[amendments](../docs/superpowers/specs/2026-09-11-clix-design.md#amendments-2026-09-24):

- Grants must name their machines.
- Native Allow grants only the requester, and approvals never silently replace
  a different grant.
- A grant can be limited to exact argument lists (`--only`).
- State is written as format 2.
- Pin is opt-in, is no longer coupled to remote execution, never syncs `.git`,
  and the receiver checks writes against its own baseline.
- `clix @machine` addressing works, and new pairings refuse command names.
- Install is hardened: a hostname with dots no longer breaks startup,
  notifications and the tray are detected at runtime, and lingering is advised.
- Wording now uses machine and grant.

225 tests pass (`cargo test --locked --no-fail-fast`), fmt and clippy are clean,
and GitHub CI passed on the pushed commit. The
[two-machine evidence](evidence/2026-09-24/review-fixes/README.md) covers
disposable fixtures on the laptop and server over Tailscale. The server drove
the laptop, and all 9 checks passed:
- an argument-limited grant ran only its allowed argument list;
- an ungranted tool was refused;
- pin sync was refused while pin was off;
- `.git` was not synced.

Both installed services were then upgraded with backups, and every existing
state field was preserved. The standing `adb` grant ran from the server with
exit 0. Pin is now off on both machines, and that grant still accepts any
`adb` arguments until the owner limits it.

## Still incomplete or unproved

- Physical lid suspend/resume, actual desktop notification/tray interaction,
  sustained daily use and the accepted specifically **two-Arch** milestone.
  An Arch-derived/Debian pair and daemon restart do not substitute for them.
- The laptop owner UID remains trusted. Attribution is to a paired body, not
  human versus agent sharing it. A grant can now be limited to exact argument
  lists (`clix add --only`), but a grant without one accepts any arguments, and
  process descendants are not sandboxed. The owner's
  [scope decision](2026-09-12-product-quality.md) limits remote laptop access;
  server development-account isolation is outside scope.
- Finite replay-receipt capacity eventually stops admission. Output pruning
  keeps receipts; it is not indefinite garbage collection. Full-state rewrites
  and large-log memory costs remain.
- Pin is opt-in (`clix pin on`) and syncs at pairing and explicitly, not before
  remote execution and not as a continuous background replica. The receiver
  checks each write against its own baseline, but a paired machine that
  deliberately deviates from the protocol (commit, then write) can still
  replace a file, with the displaced version retained; session-bound commits
  are follow-up work. File/type/tree limits, retained-inode writers and
  power-loss durability have not been exhaustively proved.
- Not yet implemented from the review: protocol version negotiation, a bounded
  replay-receipt window (including keeping denied callers from filling the
  execution cap), splitting state storage, and published release binaries.

**Verdict: trust the tested granted-hand scope; keep the code.** The shared
history and owner recovery objects now exist. The whole accepted v0 remains
unfinished; do not describe it as a same-UID isolation boundary or a fully
proved production system. Earlier repair and publication evidence is retained
in the linked records, including [owner-control repair](2026-09-12-owner-control-fix.md).
