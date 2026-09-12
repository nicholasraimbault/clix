# Current work

The [accepted design](../docs/superpowers/specs/2026-09-11-clix-design.md) is unchanged.
The [prepared v0 plan](../docs/superpowers/plans/2026-09-11-clix-v0.md) remains a
recipe, not proof. The [repair record](2026-09-11-repair.md) identifies the tested
artifacts, commands, implementation changes and remaining limits.

Public records use `laptop` and `server` as pseudonyms for the actual tested
machines. See the [evidence provenance note](evidence/2026-09-11/README.md).
The [publication preparation](2026-09-12-publication.md) records later fixture
and documentation changes; it adds no new two-machine or product-completion proof.

[Next work](2026-09-12-product-quality.md) records the owner's 2026-09-12 scope
decision: **"just limit on laptop"**. The agent uses its normal development
account on server; Clix restricts remote access to laptop through laptop-owner
grants. Local agent isolation on server is outside scope. Proposed milestones
and acceptance tests are not completed proof. The
[owner-control repair](2026-09-12-owner-control-fix.md) now completes explicit
request selection and path revocation, and records a measured storage change;
it does not complete the broader quality proposal.

This is the owner's deployment scenario, not a restriction of Clix to laptops.

Work is on `master`. The original runtime work started at
`6ee358297524a510bdb9e019d5b07b2054d3baeb` before the owner-authorized history
sanitization. The repaired core is now committed as `42290b4`; publication edits
and their checks are recorded separately. The owner subsequently authorized
pushing the sanitized history to GitHub and replacing the repository to resolve
retained identifying objects, then making the replacement public on 2026-09-12.
The original repository remains a separate private archive. The
[publication record](2026-09-12-publication.md#publication) captures the visibility
change, verified object separation and enabled private vulnerability reporting.
No clone was performed.
The measured runtime build source is
identified by [per-file SHA-256](evidence/2026-09-11/source-sha256.json) and archive
SHA-256 `37a99e7ae439042ac4c1b83cd04d04a7e6e34e41b8e54302dc23b76749bf29ea`.

**The hand exists, beyond the CLI shape.** On 2026-09-11, the actual laptop
(CachyOS, Arch-derived) and `server` (Debian 13.6) sidecars paired over Tailscale.
From `server`, `clix laptop adb devices` executed the laptop's `/usr/bin/adb` and
returned its attached phone. With no grant it was denied; `bash` was denied;
a successful once grant disappeared. Execution uses Clix's mutually
authenticated TLS connection and the granted binary. SSH was used to install
and invoke the server-side CLI, not to execute the laptop hand.

[The two-machine run](evidence/2026-09-11/two-machine.log) stopped the laptop
sidecar, queued a job and permission request on Debian, restarted the Debian
sidecar, and brought the laptop sidecar back. Both deliveries resumed. Its
first immediate log-equality assertion failed because the runner had completed
before the origin's next poll; [the follow-up](evidence/2026-09-11/log-convergence.log)
verified all five jobs converged in identity, argv, status and output. This is
eventual convergence, not an atomic shared log.

[Default pin checks on both machines](evidence/2026-09-11/pin-two-machine.log)
proved bidirectional copying, conflict refusal with both edits retained,
explicit resolution, and deletion propagation for a disposable text file.
[The final installed build](evidence/2026-09-11/final-runtime.log) was restarted
and again ran the real hand. Both services were enabled and active at that check. The only
standing grant is `/usr/bin/adb` on `laptop`, restricted to `server`; Debian has no
hands. Revoke it locally with `clix remove adb`.

`cargo test --locked` passed **125 tests, zero failures, zero ignored**, natively
on both [CachyOS](evidence/2026-09-11/cachyos-tests.log) and
[Debian](evidence/2026-09-11/debian-tests.log). Local clippy passed with warnings
denied. Tests include actual daemon crashes, queued delivery after origin
restart, uncertain runner recovery, concurrent once grants, exact bytes and
exit status, authenticated peer rejection, pin conflicts and retained writes.
[A private D-Bus recorder](evidence/2026-09-11/notifications.log) verified pending
replay and Allow once through production notify-rust; this is not desktop UX
proof. [An unavailable Tailscale command](evidence/2026-09-11/owner-offline.log)
verified local status/add/hands/remove still work.

The [owner-control repair](2026-09-12-owner-control-fix.md), based on
`8837ee655caace2dc8a16272a97a3319b98c1d35`, fixes the two defects reproduced in the
[disposable-state review](evidence/2026-09-12/owner-control-review.json): approval
now requires a specific pending ID, and revocation accepts the stored executable
path even after deletion. Grant changes invalidate stale request actions while
retaining delivery receipts. Terminal, notification and tray actions use one
decision implementation. Actual mixed-version CLI/daemon checks reject the new
operations without changing state. **132 tests passed natively on each of the
laptop and Debian server**, with release builds on both and fmt/clippy on the
laptop; the new record identifies the exact source and test hashes separately
from the original 125-test runtime evidence above. Both services now run their
repaired binaries, with their entire decoded states unchanged across upgrade.
The [new remote smoke](evidence/2026-09-12/owner-control-fix/two-machine-smoke.json)
executed laptop's `adb devices` from server with exit 0 and matching saved jobs.
Its attached-phone assertion failed: `adb` listed no devices. This proves real
remote execution after upgrade, but does not repeat the earlier attached-phone
result. The new storage measurement supports compact JSON formatting; it does
not prove supported scale or bounded resource use.

**Still incomplete or unproved:**

- The laptop owner account remains trusted. The same-UID limitation still
  exists, but isolating an agent from its server development account is outside
  the owner's chosen scope. Do not claim protection against an agent already
  given unrestricted laptop-owner access by another route. Attribution is to a
  paired body, not an individual human versus agent sharing that body. Grants
  expose the binary's capabilities; tool argument restrictions and process
  sandboxing are not implemented.
- A global log from any box is incomplete. Accepted remote runs are durable on
  caller and runner. Local-only jobs and failures before delivery (including
  origin-side pin conflicts) are not replicated to every body.
- Physical lid sleep/resume, the native desktop notification/tray experience,
  and the specifically **two-Arch** milestone have not been proved. Stopping a
  daemon on an Arch-derived/Debian pair does not substitute for those results.
- Pin has the documented file/type/size limits; sync occurs at pairing and before
  remote execution, not as a continuous background replica. Arbitrary concurrent
  filesystem writers and crash durability have not been exhaustively proved.
- Job/output inspection and pin recovery still need ordinary owner commands.
- State/log storage and retained versions grow without compaction. Full-state
  rewrites, dispatch scans, connections and process concurrency lack proved
  operating limits. Large-tree performance, disk exhaustion and operational
  longevity remain unproved.

**Verdict: fix it.** Keep the repaired hand, identity, grant and durable delivery
core. It now has native two-machine evidence. Do not call the whole accepted
system finished or trust it as an isolation boundary for a same-UID agent.
