# History, recovery and capacity evidence

This record starts from `ac1291681f8e549fe9c72425442245e37939ffd1` on master.
The accepted design was not changed. Public machine names are pseudonyms;
private machine names, addresses, state, keys and device output stay outside
the repository. Commands below use those public names where necessary.

## Artifact identity

The final 104-file [source manifest](source-sha256.json) has SHA-256
`7ae93134d6001a0f57a6b2acbdabe302b55106f37c85a2fe5a95a2e2ee5f6e21`.
The corresponding private source archive has SHA-256
`ba9388d53103ad5d06d5cab80f50be7f4e50d35154c2388ae13c15757221365c`.
Documentation and evidence were completed after freezing that source. The
manifest describes the tested artifact, including the then-current work record;
it does not claim that later evidence files existed before their checks ran.

The laptop release SHA-256 is
`1f234ab9e5b8e6c03146b3084ceec7c36170d5d3ca542192e4b586e7522d036d`.
The Debian release SHA-256 is
`4bc43d0e1697c2e573c7e5c0eece727b4034c5ec8160ca7a0ded7efdc197fc50`.
These are native builds of the same frozen source, using Rust 1.96.0 and
1.96.1 respectively. They are not claimed to be reproducible binary builds.

The runtime and performance runs used the earlier 193-test candidate:
[103-file manifest](candidate-193-source.json) SHA-256
`ef087852b243bd8c1c924f326ac75bff6f1431323bd5f40beb9bb4e30235d197`, archive
`8fec67a7651f3cec357adc0ce026aa6d3460c277ad2785de04f16cf4938614ab`.
The final candidate adds only `tests/legacy_delivery.rs`. All 29 production
source and dependency files are identical; the rebuilt laptop release is
byte-identical to the tested artifact. See [artifact binding](artifact-binding.json).

## Native validation

[Laptop validation](laptop-validation.json) records commands, results and raw
log hashes:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

The final suite passed **196 tests, zero failed, zero ignored**, natively on
both [laptop](laptop-validation.json) and [Debian](debian-validation.json). The earlier
193-test candidate also passed natively on
[Debian](candidate-193-debian-validation.json). The suite includes actual
CLI/daemon processes, loopback mesh tests and three-body relay with the original
runner offline. Those tests do not stand in for two actual machines.

The failure-boundary tests include:

- Forged invocation, author, role and output rejection; immutable terminal
  facts; duplicate/reordered history; unknown-author rescan; explicit legacy
  provenance. Imported waiting/running records cause no execution or grants.
- Owner output, retry, pruning and recovery commands through actual processes.
  Existing bodies named `pin` and `job` remain addressable after restart using
  `clix -- BODY TOOL`.
- Concurrent once grants, runner death with preserved uncertainty and
  reservation, direct-child admission limits, bounded owner input and mesh
  admission that leaves room for reciprocal pin requests.
- Real ENOSPC in an 8 MiB private tmpfs, including failed completion and failed
  startup recovery. Owner inspection remains available and new execution stops.
  Policy-capacity refusal is tested separately from a durable write failure.
- Pin recovery interrupted by real process death at fsync boundaries; stale
  decisions, symlinks, malformed metadata and quota refusal; bounded streaming
  that detects changed versions. A process kill is not a power-cut test.

[The additional upgrade checks](legacy-upgrade-check.json) migrate a synthetic
pre-history origin state while retaining identities and receipts produced by
actual loopback pairing and tool execution. Queued and waiting work resumes;
an already completed runner result is retrieved without another effect. These
use in-process sidecars with real listeners and child tools, not actual old
binary installations or separate daemon-process crashes.

## Actual two-machine fixtures

[The laptop/Debian run](two-machine-fixtures.json) paired separate disposable
states over their distinct actual Tailscale interfaces. It verified local-only
history on the other machine, origin-side pin-conflict failure with zero runner
admission or effects, owner conflict/recovery commands, ungranted denial and one
effect from a once-granted tool. Killing and restarting the fixture runner
preserved uncertainty and its reservation; an explicit new retry stayed denied.

Eight Ed25519 signatures across three replicated records were independently
verified against the public keys stored by the actual PAKE pairing, including
the output digest against CLI bytes. Both production services, running binary
hashes and complete state hashes remained unchanged. Fixture process groups
were stopped and temporary directories removed on both machines. This run
did not upgrade production, exercise the real adb grant, suspend a machine or
establish desktop interaction.

## Measurements

[The corrected history comparison](candidate-193-history-performance.json)
uses identical synthetic fixtures at 100, 1,000 and 10,000 completed jobs and
105 measured CLI calls per binary. At 10,000 jobs, median single-job inspection
changed from about 3.27 seconds to 3.8 ms; full log from 3.41 seconds to 199 ms.
The final sidecar still reached about 291 MiB peak memory and rewrote about
17 MB for each grant mutation. These are serial CLI observations, not 10,000
actual executions, throughput or indefinite operation proof.

The baseline is an intermediate implementation, identified by
[its exact source](performance-baseline-source.json), manifest SHA-256
`631b2d8c87a2158fc80046514d4762a7c786f5e51bd68a010792028644c3bdde`, archive
`4306753a171840390f0fce099f3ea18088c8642fb1df7a5608f28073a0161a8f`, and release
`5e047ecd6f593a00eb5161aa410740e9852ba7ffa999673ed9c8dd92be79a4a6`.
It is not the previously published baseline revision.

[The pin benchmark](candidate-193-pin-performance.json) uses two real daemon
processes on loopback. The 1,536-file addition took about 2.38 seconds in the
final artifact versus 65.4 seconds in that intermediate baseline; a
[controlled TCP_NODELAY comparison](pin-nodelay-comparison.json) isolates the
socket change. Each new-file batch is one observation. Three repeated runs
give medians of 88 ms for an unchanged 2,048-file tree, 9.8 ms to list 16
retained versions and 772 ms for a verified 16 MiB export. All three exports
matched exact length and digest. Tree counts were checked; the benchmark did
not independently hash every copied tree file.

The pin run sampled about 55 MiB resident memory while the daemon's lifetime
peak was about 164 MiB, including earlier setup. All 266 sampled owner-control
requests succeeded; the slowest was about 18 ms. This is not a hard latency
ceiling. Logical retention overflow refused publication of the new file; its
sparse fixture did not fill the disk or rehash all preexisting retained data.

## Defeated premises retained

The work did not pass on its first implementation. The resulting regressions
and private failed-run artifacts were retained:

- Counting admitted and replicated payloads separately evicted useful output
  too early, while polling could rehydrate pruned bytes. Output selection and
  normalization now share one implementation and preserve local prune receipts.
- Raw encoded size was insufficient completion accounting: pruning creates
  metadata and sequence numbers grow. Capacity includes those costs, and an
  already oversized migrated state can shrink and settle admitted work.
- A failed recovery write could prevent owner inspection or show a durable
  Running result as current. Startup now keeps an owner inspection path with
  an explicit unsaved-uncertainty observation and no execution dispatch.
- Rehashing all retained data for every export chunk made export quadratic.
  One reviewed descriptor now streams with incremental verification. Read-only
  recovery work uses a snapshot of the pin root instead of holding Store while
  hashing. Mutation still holds the authority guard during publication.
- Single-job inspection built all history views, with repeated admission
  scans. It now selects the requested record before materializing output; full
  views use a temporary borrowed index while retaining the same authority checks.
- Tiny pin files incurred TCP delayed-write stalls. The shared socket setup now
  enables TCP_NODELAY; the controlled comparison keeps all other source equal.
- UUID sorting was incorrectly treated as execution chronology in a log test.
  Assertions now select actual IDs and outcomes. Display order follows local
  observations; it is not a global order shared by disconnected machines.
- Reusing a Cargo target after extracting a zero-mtime archive returned a stale
  Debian release despite a successful command. The test profile had been
  cleaned; the release profile also needed explicit cleanup. That stale result
  is not native-release proof. The corrected build visibly recompiled Clix and
  produced the distinct Debian hash above.
- The first history timing harness used Python's timed process wait, which
  polls with increasing sleeps. Its millisecond decimals implied unjustified
  precision. Those runs remain private; the final comparison uses blocking
  process wait with a separate watchdog on both baseline and final artifacts.
- A late review claimed legacy admissions would be sent through modern signed
  execution. Reading the complete lookup and running three migration cases
  disproved that claim: the existing helper already excludes legacy records.
  The unnecessary patch was removed before build or deployment. The production
  source remains unchanged and the new tests retain the actual upgrade behavior.

The numerical operating limits are admission policy, not evidence of indefinite
operation. Full-state rewrites, memory use, finite replay-receipt capacity and
arbitrary external pin writers remain explicit limits in
[operation and recovery](../../../../docs/operations.md).

## Installed services

Both [laptop](laptop-upgrade.json) and [server](server-upgrade.json) were upgraded
with private backups. Every preexisting decoded state field was preserved,
including identity, grants, jobs, requests and pin index. History migration
added its own records. Both services remained enabled and active, with verified
installed/running binary hashes and writable storage.

The [post-upgrade remote smoke](two-machine-smoke.json) ran the existing laptop
`adb devices` hand from Debian through Clix TLS: exit 0, exact output bytes,
matching saved jobs and verified runner provenance. Both machines' authority
remained unchanged. No phone was attached; that is not an execution failure.
Physical suspend, native desktop interaction and sustained use remain unproved.
