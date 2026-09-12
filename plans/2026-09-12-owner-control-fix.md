# Owner control repair and measured storage change

Work on `master`, starting at `8837ee655caace2dc8a16272a97a3319b98c1d35`.
The owner authorized fixing the reviewed defects. The accepted design is
unchanged. This completes the request-selection part of the
[quality proposal](2026-09-12-product-quality.md), not its broader recovery,
shared-history or operating-limit milestones.

## What was wrong

The [earlier production-function reproduction](evidence/2026-09-12/owner-control-review.json)
found that approval selected the latest request rather than the request the
owner inspected. A new arrival could change the target. Adding by executable
path and removing that same path also failed to revoke the grant.

Before changing production code, new regression tests failed on the baseline:
the owner RPC accepted approval without an ID, removal of the stored path after
file deletion returned `NotAdded`, and a grant change left three pending rows
where the regression expected only the unrelated row. These were test failures
against disposable state, not an execution or desktop demonstration.

Review also found two related problems: stale native actions could replace a
newer owner grant, and simply adding an ID field to the old `allow` protocol
would let an old daemon silently ignore the field and still approve the latest
request. Both are addressed in this change.

## Resulting behavior

- `clix pending` prints request IDs. `clix allow REQUEST_ID` and
  `clix deny REQUEST_ID` require one. Missing, invalid or stale IDs cannot select
  another request. CLI, notification and tray decisions share `request::decide`
  and commit through `Store::update`.
- Terminal approval defaults to one successful run by the requester. Native
  Allow once has that scope; native Allow remains a persistent grant for all
  paired bodies. Scope is explicit in the shared decision object. Existing
  grant validation remains in `grant::add`.
- A successful grant replacement or revocation invalidates pending requests
  for that grant's tool name, including requests using a full executable path.
  Delivery receipts remain durable, so retries cannot resurrect an old ID.
  Deny removes only the selected request. A failed change preserves pending
  requests and grants; a persistence failure latches the existing storage error.
- Removal accepts the stored executable path even after deletion. A path must
  match that stored path; another path with the same basename cannot revoke it.
  A bare tool name still selects the named grant. Relative paths are made
  absolute in the CLI's working directory before RPC, without resolving a
  symlink or requiring the executable to exist.
- Owner RPC uses distinct `allow_request` and `deny_request` operations. The
  new daemon rejects legacy approval operations. The old daemon rejects the
  new operation names. Upgrade the CLI and daemon together and restart the
  service; there is no fallback to latest-request approval.

No new mesh authority, owner role, grant representation or storage schema was
introduced. The existing mesh listener rejects both old and new owner decision
operations in the regression suite.

## Validation and artifact identity

The [source manifest](evidence/2026-09-12/owner-control-fix/source-sha256.json)
identifies all 41 source, test and Cargo files in the final tested patch. The
[local check record](evidence/2026-09-12/owner-control-fix/local-checks.json)
links the captured output from these commands on the actual laptop, Rust 1.96.0:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

All commands passed. **132 tests passed, zero failed, zero ignored.** New cases
exercise the real owner RPC, actual CLI and daemon processes across restart,
and the actual tray menu action closure after another request arrives. They
cover exact selection, missing/stale IDs, denied remote owner operations, grant
invalidation, durable delivery receipts, failed persistence, and path revocation
after deletion from a different CLI working directory. The existing execution,
identity, once-grant, retry and pin tests also passed.

The [version-skew run](evidence/2026-09-12/owner-control-fix/version-skew.json)
used an actual daemon built from the immutable baseline and an actual new CLI
against two synthetic pending requests. Targeted allow and deny both exited 1
with an unknown-operation error; the entire state hash, pending requests and
grants stayed unchanged. Its binary hashes identify that run independently of
the final release artifact. It used a private socket and disposable state;
it adds no two-machine or desktop proof.

The final laptop release binary SHA-256 is
`9200bf4d895ea5539aafcd9bc1b6f9047f4403da05453357695321ed4ad9a7a6`.

The [native Debian build](evidence/2026-09-12/owner-control-fix/debian-build.json)
verified the same 41 source hashes plus three supplementary build inputs before
and after building in a fresh private directory on the existing server. With
Rust 1.96.1, `cargo test --locked` also passed **132 tests, zero failed, zero
ignored**, and `cargo build --release --locked` passed. Its release binary
SHA-256 is `e8e5d3441c0a3c7e39df60a7ce01067e215c9a89bd4668c3bad1bf81cde605fd`.
No clone was performed. Public logs redact only identifying paths; the records
preserve raw and published hashes and identify the tested artifacts.

## Storage measurement

The [benchmark report](evidence/2026-09-12/owner-control-fix/storage-benchmark.json)
compares an immutable baseline source snapshot with the single production
change `serde_json::to_vec_pretty(self)` to `serde_json::to_vec(self)`. The final
patch uses that exact measured `store.rs` variant. `Store::update` still clones
the full state, writes a temporary file, syncs it, renames it and syncs the
directory. There is no migration or relaxed durability operation.

The experiment used disposable state on the laptop's actual btrfs filesystem
with SSD and zstd compression, five warmups and 51 measured updates per case.
Each update changed one synthetic request, with successful file/directory sync
calls and a verified reopen. Measurements were uncontended and warm-cache.

| Saved history | Pretty / compact bytes | Median update, before / after | p95, before / after |
| --- | ---: | ---: | ---: |
| 1,000 jobs, each with 1 KiB stdout and 128 B stderr | 14,750,622 / 4,230,347 | 21.51 / 11.33 ms | 25.01 / 14.18 ms |
| 100 jobs, each with 16 KiB stdout and 2 KiB stderr | 23,106,222 / 6,501,947 | 30.37 / 13.29 ms | 33.13 / 14.27 ms |

Small-state measurements stayed around 5 ms. This justifies removing pretty
formatting from internal state. It does not establish total command latency,
concurrent throughput, large-tree performance, power-loss durability or a
supported history size. The largest baseline file was about 23 MB. The report
retains the first baseline's compiler-overlap caveat and identifies the second
baseline used for these comparisons. Raw samples, harnesses, source snapshots
and binaries are privately retained under the report's hashes.

## Installed machines and actual remote execution

The [laptop upgrade record](evidence/2026-09-12/owner-control-fix/laptop-upgrade.json)
and [server upgrade record](evidence/2026-09-12/owner-control-fix/server-upgrade.json)
identify the old and new installed binaries. Before each replacement there were
no active jobs, pending requests or reserved grants. Each service was stopped,
its binary and state backed up privately, the binary replaced atomically at its
existing path, and the service started. The running and installed hashes match
their respective final releases. The enabled service units and complete decoded
states were unchanged, including owner identity, peers, grants and pin index.
On both machines, both decision commands rejected a nonexistent ID, again
leaving state unchanged. The only grant remains laptop's `adb`, restricted to
server; server has no grants.

The [actual remote smoke](evidence/2026-09-12/owner-control-fix/two-machine-smoke.json)
ran `clix laptop adb devices` from server through the existing laptop grant,
using the actual paired name. The command exited 0 and the saved job on caller
and runner agreed in ID, location, argv, status and output. SSH invoked the
server CLI; Clix's authenticated mesh carried execution to laptop.

**The attached-phone premise failed.** The smoke harness expected an attached
authorized phone, but `adb` returned `List of devices attached` with no entries.
The harness exited 1 on that assertion. A follow-up read of its retained CLI
result and both saved jobs verified successful remote execution and unchanged
authority state without rerunning the command or changing grants. This proves
the upgraded real hand still executes; it does not repeat the earlier phone
access result. The failed assertion remains in the evidence record.

## Remaining work

The hand and these owner controls are implemented objects, not CLI stubs. The
changes are fit to keep: selection and native action semantics have one shared
implementation, grant invalidation belongs to grant mutation, and path removal
uses the recorded path without guessing from the current filesystem.

The broader system still needs work. Global history is incomplete; job and pin
recovery still need ordinary owner commands. Full-state rewrites and retained
history remain unbounded. Dispatch scans, listener connections and process
concurrency have no demonstrated operating limits. Physical suspend/resume,
native desktop interaction, operational longevity and the original two-Arch
milestone remain unproved. The laptop owner UID remains trusted and granted
tools are not sandboxed. **Verdict: fix it**, with this owner-control repair
complete; do not describe the whole product as finished or generally optimized.
