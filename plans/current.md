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

## Still incomplete or unproved

- Physical lid suspend/resume, actual desktop notification/tray interaction,
  sustained daily use and the accepted specifically **two-Arch** milestone.
  An Arch-derived/Debian pair and daemon restart do not substitute for them.
- The laptop owner UID remains trusted. Attribution is to a paired body, not
  human versus agent sharing it. Tool arguments and process descendants are not
  sandboxed. The owner's [scope decision](2026-09-12-product-quality.md) limits
  remote laptop access; server development-account isolation is outside scope.
- Finite replay-receipt capacity eventually stops admission. Output pruning
  keeps receipts; it is not indefinite garbage collection. Full-state rewrites
  and large-log memory costs remain.
- Pin sync runs at pairing, before remote execution and explicitly, not as a
  continuous background replica. File/type/tree limits, retained-inode writers
  and power-loss durability have not been exhaustively proved.

**Verdict: trust the tested granted-hand scope; keep the code.** The shared
history and owner recovery objects now exist. The whole accepted v0 remains
unfinished; do not describe it as a same-UID isolation boundary or a fully
proved production system. Earlier repair and publication evidence is retained
in the linked records, including [owner-control repair](2026-09-12-owner-control-fix.md).
