# Implemented: history, recovery and operating limits

The owner authorized this remaining work with **"do it"** after distinguishing
code gaps from physical and everyday-use testing. Work starts at
`ac1291681f8e549fe9c72425442245e37939ffd1`, on master, without cloning. The
accepted design and laptop-owner authority scope are unchanged. This document
records the implementation decisions and acceptance gates below. Completed
results are identified separately; the whole accepted v0 is not declared done.

The baseline source archive SHA-256 is
`6fd8b399f75a5c04dced75e74d674573be9c923653aa908d371e5201d9a6acaa`;
the archive and per-file manifest are retained privately outside the repository.
Previously tested source and runtime evidence remain intact.

## Implementation decisions

- Keep locally admitted jobs separate from imported history. Bind each new
  admission to an exact origin-signed invocation. Origin delivery observations
  and runner acceptance/outcome have separate signed revisions. Replicas never
  grant, execute, reserve a once grant or enter the local delivery queue.
- Relay signed history incrementally with durable cursors, epochs and bounded
  pages. Only explicitly paired authors are trusted. Show missing or stale
  history and rescan after pairing a previously unknown author. Legacy records
  are signed observations by their actual observer, clearly labelled; they
  cannot become modern execution authorization.
- Add owner commands for job inspection, saved output and an explicit retry
  with a new linked ID. Preserve the old outcome and once reservation. Add pin
  inspection, version-checked resolution, explicit disposal of reviewed
  retained versions and an explicit sync command. Keep recovery off the mesh.
- Bound active child processes, outbound attempts, accepted connection tasks,
  input sizes, state size and retained output. Preserve replay receipts; reject
  new admissions at the receipt limit instead of aging away replay protection.
  Distinguish policy capacity from a failed durable write. Keep inspection
  available when storage cannot admit work.
- Cap new pin-retention admission and preserve interrupted or changed versions.
  External writers can grow retained original inodes after displacement; Clix
  cannot enforce a filesystem quota on those writers. Do not claim otherwise.

These are conservative initial policies, not measured throughput promises.

## Acceptance gates

Adversarial history tests must cover forged roles/keys/argv/output, unknown
authors, duplicate/reordered facts, terminal immutability, cursor resets,
crashes and three-body relay with an origin offline. Importing queued or running
history must cause zero executions and zero authority changes. Legacy migration
must preserve admission, outcomes and once reservations without new provenance.

Exercise owner commands through actual CLI/daemon processes. Interrupt pin
publication and recovery at their durable boundaries; reject stale decisions
and preserve recoverable bytes. Fill only disposable isolated storage when
testing ENOSPC. Prove process admission limits and resumed work after capacity
returns. Measure the final artifact with growing history and a representative
pin tree; record bounds and remaining limits.

Run fmt, clippy with warnings denied, the full suite and release builds; repeat
native tests and relevant runtime scenarios on the actual laptop and Debian
server. Preserve identity and grants through upgrades. The owner has confirmed
the phone was disconnected during the preceding empty `adb devices` result.

Physical suspend/resume and native desktop interaction need the owner present.
Prepare a concrete check only after automated and service-upgrade checks pass.
Sustained everyday use and the original two-Arch milestone remain separate
evidence requirements. Neither elapsed development time nor a stopped daemon
substitutes for them.

## Completed evidence

The [evidence record](evidence/2026-09-12/product-completion/README.md) identifies
source archives, per-file manifests, native binaries, commands and measured
limits. The final 104-file manifest SHA-256 is
`7ae93134d6001a0f57a6b2acbdabe302b55106f37c85a2fe5a95a2e2ee5f6e21`;
archive SHA-256 is
`ba9388d53103ad5d06d5cab80f50be7f4e50d35154c2388ae13c15757221365c`.
Later documentation and evidence additions do not change the compiled source.

- Fmt and clippy with warnings denied passed. **196 tests passed natively on
  laptop and Debian, zero failed or ignored**, with release commands on both.
- Five actual two-machine fixture cases passed: pairing; local-only history;
  origin pin-conflict failure without execution; owner pin recovery; and
  denied/once/crash/retry behavior. Eight actual signatures were independently
  verified. Fixture cleanup and unchanged production state were checked.
- Both production services were upgraded with backups and all preexisting
  decoded fields preserved. The real server-to-laptop adb hand then returned
  exit 0 and matching signed history. The disconnected phone was not a failure.
- The corrected 10,000-record benchmark measured about 3.8 ms for one inspection
  and 199 ms for full log, versus 3.27 and 3.41 seconds in the intermediate
  implementation. Peak memory remains about 291 MiB; grant changes still
  rewrite about 17 MB. Tiny-file pin transfer improved from 65.4 to 2.38 seconds
  for one 1,536-file addition in a warm-cache loopback fixture.

A suspected late legacy-upgrade defect was disproved: the existing history
lookup already excludes legacy certificates from modern execution. Three
additional regression tests passed on unchanged production source. The
withdrawn claim, timing-harness limitation, stale cached Debian release and
actual implementation failures are retained in the evidence record.

## Assessment and remaining proof

Keep the implementation. Imported history and execution admission are distinct;
owner decisions and grant checks share their authority paths; durable state
updates centralize retention and failure handling. These are useful boundaries,
not evidence that every interaction has been audited. Pin/history modules are
substantial and deserve focused review when changed.

Physical sleep/resume, the native notification/tray experience, sustained daily
use and the specifically two-Arch milestone remain unproved. Physical checks were prepared but not launched. Finite replay-receipt limits stop new admission; they are not
an indefinite-retention solution. Full-state writes and peak memory remain
measured costs. Granted-tool arguments, descendants and unrestricted access
under the laptop owner's UID are not sandboxed. Pin is not continuous background
replication, and process-kill tests are not exhaustive power-loss or external
filesystem-writer proof. The accepted design remains unchanged.
