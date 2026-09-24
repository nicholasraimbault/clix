# Review fixes plan

## Implementation status (2026-09-24)

The owner approved every owner decision below ("all you found") and asked for
the work on master. Packets 1, 2, 3, 4, 6, 7, 10, 11 and 12 are implemented, in
commits `ef95714`..`6ffc963` on top of `3d21163`.
`cargo test --locked --no-fail-fast` passes 225 tests with no failures, and
clippy and fmt are clean. The accepted design records the implemented
amendments.

- **Packet 6** was narrowed to the capacity constants and a boundary test. Keeping
  denials from filling the execution cap needs the receipt-retirement work,
  because a denied signed admission already has a certificate and history entry.
- **Packet 10** exposed a limit. The receiver now checks each write against its
  own baseline. But `pin_commit` records the receiver's current tree as the new
  baseline without binding it to the session's writes. So a paired machine that
  deliberately deviates from the sync protocol can still replace a file. The
  displaced version is kept in `.clix-recovery`. Closing this needs
  session-bound commits, which is follow-up work. Pin is now off by default, and
  enabling it trusts paired machines with write access to `~/src`.
- **Not implemented:** packet 5 (version handshake), packet 8 (receipt window and
  denial handling), packet 9 (storage split) and packet 13 (release binaries).
  Each changes a signed wire format, migrates stored state, or needs a published
  release. Proving each needs mixed-version two-machine runs or a real install.
  The design keeps its original text for these.
- **Proof:** the [two-machine evidence](evidence/2026-09-24/review-fixes/README.md)
  covers two things:
  - disposable fixtures over Tailscale, where all 9 checks passed;
  - the upgrade of both installed services, with backups, where all state
    was preserved and the production `adb` smoke test exited 0.

---

An outside reviewer read only `README.md` and `plans/current.md` (not the code)
and flagged eight suspected issues plus two later items. This document checks
each against the code at the current revision and lays out a fix plan. The
sections below are the plan as written before implementation.

- Revision: `3d21163503ec48faf3e9742ed6028410d3dd5aac`, master, clean tree.
- Baseline: `cargo test --locked` = 196 passed, 0 failed, 0 ignored (log in the
  session scratchpad `baseline-tests.log`).
- The accepted design (`docs/superpowers/specs/2026-09-11-clix-design.md`) and
  `plans/current.md` are **not** modified by this plan. Several proposed fixes
  *would* change the design text or the AGENTS.md values; those are called out
  and deferred to the owner decisions below.
- Real machines available for two-machine proof: the laptop (CachyOS,
  Arch-derived, this machine) and the server (Debian 13.6). Loopback
  two-daemon runs are required but are **not** a substitute where behavior
  crosses machines (design:165).

Effort scale: **S** ≈ under a day; **M** ≈ one to three days; **L** ≈ more than
three days or needing a new signed protocol and two-machine proof.

---

## 1. Summary table

| # | Issue (reviewer) | Status | Severity | Design change | Effort |
|---|---|---|---|---|---|
| 1 | Grant defaults are broad (no `--allow` = all bodies incl. future; native "Allow" = all paired) | **Confirmed** | High | Partly (CLI default = design:97; native Allow scope is unspecified) | S–M |
| 2 | Arguments can't be constrained (`adb` grant ⇒ `adb shell`, `adb pull/push`); no per-grant arg allowlist | **Confirmed**, deliberately deferred (design:127 "Later") | High | No (design already lists arg limits as Later) | M |
| 3 | Replay receipts eventually block all new work; wants expiry + pruning | **Confirmed, worse than stated** | High (availability) | Partly (expiring undelivered work bounds "wait is wait", design:159) | L |
| 4 | State rewritten in full every change (17 MB, 291 MiB @ 10k); wants SQLite / append log | **Confirmed; SQLite premise only partly right** | Medium | No | M–L |
| 5 | Machine names collide with subcommands; wants `clix @laptop adb devices` | **Partly confirmed** | Low–Medium | Partly (`@body` additive; refusing colliding names is a behavior change) | S |
| 6 | Vocabulary is mixed (grant/hand, machine/body) | **Partly confirmed** | Low | Yes (terms are in AGENTS.md values) — owner decision | S |
| 7 | No release binaries; wants prebuilt releases + one-line installer before announcing | **Confirmed** | Medium (blocks public announcement) | No (design:120 "One package") | M |
| 8 | `~/src` syncs automatically on pairing, both ways, incl. deletions; wants opt-in and separate from tool grants | **Confirmed; intentional per design (:41,:58,:120)** | High | Partly (opt-in pin is a design change; stopping sync-before-exec and receiver enforcement are not) | M |
| L1 | Peer version negotiation (today all machines must upgrade together) | **Confirmed** (docs/operations.md:173) | Medium | No | M |
| L2 | macOS support | **Confirmed gap** (Linux-only syscalls, systemd, tray) | Low (out of v0 scope) | No (design is "Two Arch machines") | L |

### Extra issues found in the same areas (not in the reviewer's list)

| # | Issue | Status | Severity | Design change | Effort |
|---|---|---|---|---|---|
| E1 | Approving a request **replaces the whole grant object**, silently dropping an existing allow-list / expiry / schedule or revoking another body | **Confirmed** | High | No | S–M |
| E2 | `clix hands` prints only tool names, hiding scope/expiry/schedule; native "Allow" notification button hides its scope | **Confirmed** | Medium | No | S |
| E3 | Pin "honest" conflict rule is enforced **only on the initiator**; a paired peer can `pin_put` past the receiver's baseline; `pin_*` ops take **no grant check** | **Confirmed** | High | No | M |
| E4 | Installed laptop service starts before the graphical session, so notifications and tray are silently disabled (requests only visible via `clix pending`) | **Confirmed** | Medium | No | S |
| E5 | An FQDN in `/etc/hostname` (contains `.`) makes the daemon exit at startup and systemd loops on restart; `clix pair --name` can't fix it because it needs the daemon | **Confirmed** | High (headless machines) | No | S |
| E6 | `After=network-online.target` is ineffective in a user manager; no linger check/advice for a headless server | **Confirmed** | Low | No | S |
| E7 | Legacy unsigned `exec` op still accepted (`mesh.rs:368`) | **Confirmed** | Low (matters for L1) | No | S |
| E8 | Limit literals duplicated (`storage.rs` prepare vs status) and untested at the 10k/30k caps | **Confirmed** | Low | No | S |
| E9 | Stale/false comment `cli.rs:411` ("The same command namespace is enforced when choosing a body name") — it is not enforced | **Confirmed** | Low | No | S |

---

## 2. Owner decisions needed

Each is a yes/no question with a recommendation. Items marked **(design)** would
change `docs/superpowers/specs/2026-09-11-clix-design.md`; items marked
**(values)** would change AGENTS.md wording. Those require an explicit project
decision per AGENTS.md before implementation.

1. **CLI scope default** — Should `clix add <tool>` require an explicit
   `--allow <body>` / `--all` instead of defaulting to every paired body
   (including machines paired later)? **Recommend: yes.** *(design: contradicts
   design:97 "with no extra flags: every paired body … All boxes, all the
   time.")*
2. **Native "Allow" scope** — Should the native/tray "Allow" grant only the
   requesting machine (persisting, not once), rather than all paired bodies?
   **Recommend: yes.** *(Not design text — design:82 lists "Allow" without a
   scope; current AllPaired is an implementation choice in
   owner-control-fix:33-35.)*
3. **Approval never silently widens/replaces** (E1) — Should approving a request
   for a tool that already has a grant be refused (directing the owner to the
   terminal) rather than replacing the existing grant's allow-list/expiry/
   schedule? **Recommend: yes** (refuse + explain; no design change).
4. **Argument allowlist** (issue 2) — Pull the design's "Later" arg limits into
   the next release as an optional field on the one Grant object? **Recommend:
   yes.** *(No design change — design:127 already lists this as Later; design:151
   requires it stay on the single grant object.)*
5. **Bounded receipt window** (issue 3) — Allow a bounded replay-receipt window
   (e.g. 30 days) after which undelivered jobs expire and their receipts are
   pruned? **Recommend: yes.** *(design: bounds "wait is wait" at design:159 —
   currently a job waits indefinitely until the grant expires; this adds a
   second expiry. Also revises docs/operations.md:156-158 "Do not delete
   receipts.")*
6. **Opt-in pin** (issue 8) — Make `~/src` pin opt-in rather than on by default?
   **Recommend: yes.** *(design: contradicts design:58 "Defaults on. `~/src`
   pins." and design:120 "One package, pair, defaults.")*
7. **Stop sync-before-exec** (issue 8) — Stop running a pin sync before every
   remote exec (keep it explicit and at pairing only)? **Recommend: yes.** *(Not
   design text — comes from the v0 recipe plan:935, which current.md:9 calls "a
   recipe, not proof.")*
8. **Pin write authority** (E3) — Enforce the honest-conflict baseline on the
   **receiver**, add write exclusions (e.g. `.git/hooks`), and make pin direction
   per-pin (push/pull/both)? **Recommend: yes** (receiver enforcement is a
   straight bug fix; exclusions/direction are additive).
9. **`@BODY` addressing + refuse colliding names** (issue 5) — Add a `clix @body
   <cmd>` form, and refuse *new* pairings whose name matches a subcommand?
   **Recommend: yes** (`@body` additive; the pairing refusal is a behavior change
   — existing command-named bodies keep working via `clix -- BODY`).
10. **Vocabulary** (issue 6) — Standardize user-facing text on "machine" and
    "grant", define "body"/"hand" once, and add `clix grants` as an alias of
    `clix hands`? **Recommend: yes.** *(values: "hand" and "body" appear in the
    AGENTS.md Values section, so this is an owner decision.)*
11. **Distribution + version handshake** (issue 7 / L1) — Publish static
    prebuilt binaries with checksums/signatures and a verifying installer, and
    require a version handshake before a public announcement? **Recommend:
    yes** (no design change; design:120 already wants "One package").
12. **Storage strategy** (issue 4) — Defer SQLite/journal until growth is bounded
    (issue 3) and outputs are split out, then re-measure and decide? **Recommend:
    yes** (product-quality.md:99-101 already says "Choose … from those
    requirements and measurements").

---

## 3. Confirmed-issue sections

Order: security defaults → reliability → usability → distribution. Each section
gives the proposed change, migration/compatibility with already-paired machines
on the previous version, tests to add, the proof required (real two-machine
where behavior crosses machines), and risks + rollback.

### Shared constraints (apply to every section)

- **Signed canonical bytes must not change.** History signatures cover
  `serde_json` bytes under domain `Clix history v1\0` (`history.rs:21`,
  `bytes()` at `history.rs:169`). Any storage/encoding change (issue 4) must keep
  the exact signed input bytes identical; re-encode for storage only, never for
  the signing path. Old invocation signatures must stay valid (issue 3, L1).
- **`format_version` fail-open-on-downgrade.** `Store::open` refuses
  `format_version > 1` (`store.rs:104-108`) but `update()` hard-sets it to `1`
  (`store.rs:211`), and neither `Store` nor `Grant` uses
  `#[serde(deny_unknown_fields)]`, so an **older** binary silently drops any new
  grant field and fails **open**. Every change that adds an authority-bearing
  field (issues 2, 8; E1 partly) must bump `format_version` to `2` in `update()`
  so an older binary refuses the state rather than misreading it. This is the
  compatibility gate; it means paired machines must upgrade the receiver first.

---

## Security defaults

### S-1 — Grant scope default (issue 1) + native Allow scope (decision 2)

**Confirmed.** Evidence:
- `grant::add` sets `allow_from: None` when `allow` is empty
  (`grant.rs:77-81`); `check_at` skips the body check when `allow_from` is `None`
  (`grant.rs:155-163`), so `None` = every body, including bodies paired later.
- Native "Allow" maps to `Scope::AllPaired`, `once=false` (`request.rs:150-155`),
  which `decide` turns into an empty allow list ⇒ `allow_from: None`
  (`request.rs:186`, `grant.rs:77-81`). Unit test `request.rs:230-231` asserts
  the resulting `allow_from == None`.
- The OS notification button is labelled just "Allow" with no scope
  (`notify.rs:65`); the tray is explicit ("Allow for all paired machines",
  `tray.rs:172`).

**Proposed change:**
- CLI: require `--allow <body>` (repeatable) or an explicit `--all` for
  `clix add`. Empty + no `--all` ⇒ usage error naming the choice. Keep `--server`
  as sugar for `--allow server` (design:151).
- Native/tray "Allow" ⇒ `Scope::Requester`, persisting (`once=false`), i.e.
  grant the requesting body only (decision 2). Relabel the notification button
  "Allow this machine"; tray label to match.
- These are gated on decisions 1 and 2 because #1 changes design:97.

**Migration / previous-version compatibility:** No stored-format change (still an
`Option<Vec<BodyId>>`). Existing `allow_from: None` grants keep meaning
"all bodies" — the change is only to how *new* grants are created, so an upgraded
machine does not silently narrow a grant the owner already made. Document that
owners re-issue any pre-upgrade all-bodies grant they no longer want global. A
previous-version peer is unaffected (this is local owner-side policy, not a mesh
change).

**Tests to add:**
- `grant::add` with empty allow and no `--all` returns a usage error; with
  `--all` yields `allow_from: None`; with `--allow server` yields
  `Some([server])` (unit).
- `Decision::from_action("allow")` yields `Scope::Requester`, `once=false`;
  update `request.rs:204-245` to assert `allow_from == Some([server])` instead of
  `None`.
- Process test: pair two loopback bodies, `clix add true` (no flags) fails;
  `clix add true --all` then `clix <peer> true` succeeds; a *later*-paired third
  body is denied unless re-granted.

**Proof required:** Loopback for the CLI/notification logic. Real two-machine
(laptop+server): grant `--allow server` only, confirm a hypothetical third
machine (or the laptop itself as `from`) is denied; confirm the existing
standing `/usr/bin/adb`→server grant still evaluates correctly after upgrade.

**Risks & rollback:** Risk — owners relying on the "all boxes" default get a
usage error; mitigate with a clear message and `--all`. Rollback: revert the CLI
arg parsing and the `from_action` mapping; no persisted state changed.

### S-2 — Argument allowlist (issue 2, decision 4)

**Confirmed; deliberately deferred** (design:127 lists "Arg limits" under
Later; product-completion.md:111 states arguments are not sandboxed). Evidence:
- `exec::command`/`validate_argv` check only `argv[0]` against the grant
  (`exec.rs:18-30`); `argv[1..]` is passed through, environment and cwd inherited.
- With an authorized device, an `adb` grant is a laptop file read/write path:
  the installed `adb` (android-tools) exposes `pull REMOTE… LOCAL` (writes any
  laptop path), `push LOCAL… REMOTE` (reads any laptop file), `forward`/`reverse`
  (laptop sockets), `-H`/`-P`, and `shell`. So "cannot run anything else on the
  laptop" (design:122) holds for the *binary* but not for its *effects*.

**Proposed change:** Add one optional field to the single `Grant` object
(design:151 forbids a second allow-list, so this must live on the same object),
e.g. `args: Option<ArgPolicy>` where `ArgPolicy` is a small enum:
- an ordered list of allowed literal args and/or a bounded set of `--flag`
  prefixes, evaluated in `grant::check`/a new `exec` pre-check;
- default `None` = today's behavior (unconstrained), so existing grants are
  unchanged.
CLI: `clix add adb --arg devices` (repeatable), or `--args-file`. Evaluate
before spawning; a disallowed arg ⇒ `Denied` with a reason and a request row,
same path as a missing grant.

**Migration / previous-version compatibility:** Adding an authority-bearing
field ⇒ **bump `format_version` to 2** in `store.rs:211` (see Shared
constraints), because an older binary would drop `args` and fail **open**
(execute the tool with unconstrained args). Receivers must upgrade first. A
grant with `args: None` serializes/deserializes identically to today for the
value, but the version bump forces the older binary to refuse the whole state
rather than misread the grant. Document: upgrade the machine that *holds* the
grant (the runner) before relying on arg limits.

**Tests to add:**
- Unit: `check`/exec pre-check allows a grant with `args=[devices]` for
  `adb devices`, denies `adb shell`, `adb pull …`, `adb -H …`.
- Unit: `args: None` grant still allows any argv (back-compat).
- Serde: a state written at version 2 with an `args` policy is refused by a
  simulated `format_version > 1` open (mirrors `store.rs:104`).
- Process: `clix add echo --arg hello`; `clix <peer> echo hello` runs,
  `clix <peer> echo world` is denied and creates a pending request.

**Proof required:** Real two-machine. On the laptop, replace the standing
unconstrained `adb`→server grant with an arg-limited one
(`clix add adb --arg devices`), then from the **server** run `clix laptop adb
devices` (exit 0, exact bytes) and `clix laptop adb shell` / `clix laptop adb
pull /etc/hostname .` (both denied). Loopback cannot prove the cross-machine
grant evaluation for the real installed services.

**Risks & rollback:** Risk — an arg policy too strict breaks a legitimate call
(fails closed, acceptable). Risk — a policy that tries to enumerate "safe" adb
usage is easy to get wrong; keep it a literal/prefix allowlist, not a blocklist.
Rollback: grants with `args: None` behave exactly as today; revert requires a
state written back at version 1 (an older binary cannot read a version-2 state,
so rollback means restoring the pre-upgrade backup per docs/operations.md
Upgrades).

### S-3 — Approval never silently replaces a grant (E1, decision 3)

**Confirmed.** `request::decide` calls `grant::add` (`request.rs:189`), which
does `store.grants.retain(|g| g.tool != grant.tool); store.grants.push(...)`
(`grant.rs:91-92`) — a full replace. So approving a fresh request for a tool
that already has a narrow/expiring/scheduled grant **overwrites** it: a native
"Allow" after an `--allow server --until …` grant drops the allow-list and the
expiry (demonstrated by `request.rs:230-231`, where the second approval sets
`once=false, allow_from=None`).

**Proposed change:** In `decide`, if a grant for `req.tool` already exists and
the decision would change its scope/time/schedule, **refuse** with a message
pointing to the terminal (`clix add …`/`clix remove …`), rather than replacing.
An identical re-grant (same fields) is a no-op success. Owner terminal `clix add`
still replaces intentionally (that is the explicit owner path). This is a bug
fix, not a design change.

**Migration / previous-version compatibility:** Local-only logic; no stored
format change, no mesh change; previous-version peers unaffected.

**Tests to add:**
- Unit: with an existing `--allow a` grant, approving a request from `b` for the
  same tool is refused and leaves the original grant intact.
- Unit: approving a request whose grant does not yet exist still creates it.
- Unit: an identical re-approval is an idempotent success.
- Process: notification/tray/terminal all share `request::decide` and all refuse
  the widening path (extends owner-control-fix behavior).

**Proof required:** Loopback is sufficient (single-machine owner logic), plus a
real two-machine check that approving from the server side path does not silently
change a laptop grant (reuses the S-1 fixture).

**Risks & rollback:** Risk — an owner who *wanted* to widen via the notification
now must use the terminal; acceptable and safer, message must say so. Rollback:
revert the `decide` guard.

### S-4 — Pin write authority (E3, issue 8 write half, decision 8)

**Confirmed.** Evidence:
- `plan()` (the honest-conflict check, design:161) runs **only on the
  initiator** (`pin.rs:509`); the receiver's `rpc_put_async`/`rpc_put`
  (`pin.rs:790`,`805`) apply the caller's expected/new/content with no check
  against the receiver's own baseline.
- `pin_list`/`pin_get`/`pin_put`/`pin_commit` are dispatched with **no grant
  check** (`mesh.rs:442-445`) — any paired peer can read the full manifest and
  write files.
- `valid_path` excludes only `.clix-recovery` (`pin.rs:81`), so `.git/hooks/*`
  and similar are writable; `apply_locked` applies the sender's mode
  (`pin.rs:361-366`,`:414`), so an executable bit propagates.
  (Session demos confirmed a peer creating `src/proj/.git/hooks/pre-commit`
  mode `-rwxr-xr-x` and overwriting a file past the honest planner via direct
  `pin_put`.)

**Proposed change:**
- Enforce the honest-conflict baseline on the **receiver**: `rpc_put`/`rpc_commit`
  must recompute against the receiver's `pin_index` and refuse when both sides
  changed since last sync (the same rule as `plan()`), returning
  `PinConflict`.
- Add a write-exclusion set (at least `.git/hooks`, configurable) enforced in
  `valid_path` for *incoming* writes.
- Add per-pin direction (push/pull/both) so a machine can host a pull-only tree
  (decision 8).
- Optionally require a pin grant/opt-in before accepting `pin_*` (ties to
  decision 6 opt-in).

**Migration / previous-version compatibility:** The receiver-side check is
compatible with an old initiator (the old initiator still runs its own `plan()`;
the receiver adds a second gate). Direction/exclusion metadata is new per-pin
config — if stored in `Store`, bump `format_version` to 2 (Shared constraints).
A previous-version peer that lacks the receiver check is exactly the current
vulnerable state, so **upgrade receivers first**; document it.

**Tests to add:**
- `tests/pin.rs`: receiver refuses a `pin_put` that conflicts with its baseline
  (mirror of `conflict_blocks_mesh_exec` at `pin.rs:156` but on the write path).
- Incoming write to `.git/hooks/pre-commit` is refused.
- Pull-only pin rejects an incoming push.
- Executable-mode propagation stays only where allowed (extends `pin.rs:311`).

**Proof required:** **Real two-machine** (behavior crosses machines; design:161).
On laptop+server with a shared baseline, have each side edit the same path, then
attempt the direct `pin_put` from the server: the laptop receiver must refuse
with a named-path conflict (not overwrite). Loopback is explicitly not a
substitute here.

**Risks & rollback:** Risk — a stricter receiver refuses a sync an owner
expected to succeed; the message names the path (design:161). Rollback: revert
the receiver check; direction/exclusion default to today's behavior when absent.

---

## Reliability

### R-1 — Replay-receipt retirement (issue 3, decision 5)

**Confirmed, worse than the reviewer stated.** Evidence:
- Every job (caller and runner) and every *denied* attempt pushes a permanent
  `Job` row: `submit_inner` records a `Denied` job (`exec.rs:132-142`) and, on
  denial, also `request::upsert` (`exec.rs:130`). `caller`-side rows are appended
  in `job::append_retry` (`job.rs:40`).
- The 10,000-row `execution receipt` cap and the 30,000 `history record` cap are
  in `storage::prepare` (`storage.rs:166-172`), and `count_limit`
  (`storage.rs:158-163`) **refuses new admissions** at the cap. So a
  least-privileged caller who can only ever be *denied* can still permanently
  exhaust the target's cap. (Session demo: 3 denied `bash` attempts left 3
  admitted job records + 3 history records + 1 pending row on each side.)
- Imported peer history counts toward the same 30,000 cap that gates *local*
  admission (`history.rs` ingest → `prepare`), coupling a peer's log growth to
  local availability.
- docs/operations.md:156-158 currently says receipt retirement "needs a
  separately proved protocol" and "Do not delete receipts"; product-
  completion.md:32-33 chose to refuse at the limit rather than age out receipts.
  So this is an intentional interim policy, and a real fix needs a new protocol.

**Proposed change (new signed protocol — decision 5):**
- Add a signed `issued_at` to a **new invocation version** (v2) while keeping v1
  signatures valid (Shared constraints). Timestamps are advisory for retirement,
  not for authorization.
- Persist a per-origin **retirement watermark**; the origin expires *undelivered*
  jobs after the window (e.g. 30 days), turning them terminal, and receipts older
  than the watermark are pruned once their job is terminal and replicated.
- Handle **denials** separately: short-lived denial rows with their own (smaller,
  faster) cap and rate limiting, so a denied caller cannot fill the execution cap.
- Decouple imported history from the local-admission cap: count local admissions
  and imported history against separate budgets so a peer's log cannot block
  local exec.
- Add history retention/pruning bounded by the same watermark.

**Migration / previous-version compatibility:** v2 invocations carry `issued_at`;
v1 have none and are treated as "no window" (never auto-expired) so old signed
records stay valid and are never silently discarded. Bump `format_version` to 2
for the watermark/denial fields. A previous-version peer sending v1 invocations
still works (no fallback to unsigned exec — docs/operations.md:177-178); it just
does not get windowed retirement. Upgrade both ends to get the benefit.

**Tests to add:**
- Adversarial: a forged/absent `issued_at` cannot authorize; v1 signatures still
  verify (extend existing history adversarial tests).
- A denied caller hitting the denial cap does not consume the execution-receipt
  cap and cannot block a granted caller.
- Undelivered job past the window becomes terminal; its receipt is pruned only
  after it is terminal and replicated; a late duplicate delivery is still
  rejected (no re-run) until pruning, and after pruning the ID cannot resurrect
  a completed effect.
- Imported history at the 30,000 budget does not refuse a local admission.
- **A test that actually reaches the caps** (none exists today —
  `storage_tests.rs:105` only exercises the 1,024 pending cap).

**Proof required:** Real two-machine for the cross-machine parts: server issues
work to a sleeping laptop, the window elapses, the job retires as terminal on
both sides, and a later delivery does not re-run. Loopback for the denial-cap and
budget-separation logic.

**Risks & rollback:** Risk — expiring undelivered jobs weakens "wait is wait"
(design:159); mitigate by making the window long and owner-configurable, and by
surfacing expiry as an explicit terminal state, not a silent drop (decision 5
authorizes the bound). Risk — clock skew across machines; use the origin's clock
for its own watermark, treat `issued_at` as advisory. Rollback: without the
window, receipts persist as today; a v2 state cannot be read by a v1 binary, so
rollback = restore backup.

### R-2 — Storage growth / full-state rewrite (issue 4, decision 12)

**Confirmed; the reviewer's SQLite premise is only partly right.** Evidence:
- `Store::update` clones the whole state, re-encodes all of it as JSON, writes a
  temp file, fsyncs, renames, fsyncs the dir, under a std `Mutex`
  (`store.rs:206-222`, `save_encoded` at `store.rs:155`). `history::sync_peer`
  does **one full `s.update` per 16-row page** (`history.rs:1101`,
  PAGE_ROWS=16 at `history.rs:22`), so importing a peer's log is O(rewrites).
- Measured (candidate-193-history-performance.json): at 10,000 jobs,
  `state_bytes` ≈ 17,020,849; grant add median ≈ 67.7 ms, grant remove ≈
  58.5 ms; peak RSS ≈ 297,972 KiB — matching plans/current.md:48-51.
- **History metadata dominates**, not job rows: per session measurement a job
  row ≈ 190 B but a history entry ≈ 1,911 B (certificate 643 incl. signature
  221; origin fact 576; runner fact 620), and byte arrays stored as JSON number
  arrays cost ≈ 908 of those 1,911 B (a 32-byte key = 129 B vs 64 as hex). So
  SQLite alone would not shrink the dominant cost; the encoding and the
  full-rewrite cadence are the real drivers. product-quality.md:99-101 says to
  choose storage from measurements.

**Proposed change (staged, decision 12):**
1. **Bound growth first** (depends on R-1): without a bound, any storage backend
   fills.
2. **Split outputs into content-addressed blob files** keyed by hash; the state
   holds references, not JSON number arrays. Removes the ~908 B/entry blowup and
   the biggest rewrite cost.
3. **Split the small authority state** (identity, grants, peers) from the large
   append-mostly data (jobs, history) so a grant change does not rewrite history.
4. **Re-measure**, then decide SQLite vs an append-only journal (do not pick
   now).

**Migration / previous-version compatibility:** This changes the on-disk layout
⇒ **`format_version` 2** and a one-time in-place migration on first open
(decode v1, write v2 blobs + split files), with the pre-upgrade backup as
rollback (docs/operations.md Upgrades). **Signed canonical bytes are unchanged**
(Shared constraints): blobs store the same bytes; the signing path still
serializes the same structs. A previous-version binary cannot read v2 — upgrade
is one-way per machine; document it. No mesh wire change (history pages still
carry the same signed structs).

**Tests to add:**
- Round-trip: a v1 state migrates to v2 and back-decodes to identical logical
  content; signatures still verify byte-for-byte.
- A grant mutation rewrites only the authority file, not the history/blob files
  (assert file mtimes/sizes).
- Blob GC removes only unreferenced blobs; pruned-output receipts still reject
  replays.
- Re-run the 100/1,000/10,000-job benchmark harness and record new
  state_bytes/latency/RSS.

**Proof required:** Loopback for the migration and the rewrite-scope assertions.
Real two-machine for the upgrade path: upgrade the installed laptop and server
services with backups (as product-completion did), confirm identity/grants/jobs/
pin index preserved and the standing `adb` grant still works. History
replication between a v2 laptop and v2 server must converge with signatures
verified.

**Risks & rollback:** Risk — migration bug corrupts state; mitigate with the
mandatory backup and a decode-verify-before-replace step. Risk — blob GC races
with pruning; keep GC inside `Store::update`'s durable boundary. Rollback:
restore the backup (a v1 binary cannot read v2).

### R-3 — Installed service notifications/tray disabled (E4)

**Confirmed.** `notify::enabled()` requires `DISPLAY`/`WAYLAND_DISPLAY` in the
process env (`notify.rs:50-55`); `tray_enabled()` requires a display
(`tray.rs:41-51`). The unit is `WantedBy=default.target` (`systemd/clix.service`)
and starts at user-manager start, before the graphical session, so neither var is
present in the daemon env and requests appear only via `clix pending` — contrary
to design:163 "a real OS notification (or `clix pending` if there is no
display)". (Session inspection: installed service active before the Wayland
session; daemon env had `DBUS_SESSION_BUS_ADDRESS`/`XDG_RUNTIME_DIR` but no
display vars.)

**Proposed change:** Order the unit after the graphical session
(`PartOf=graphical-session.target`/`After=graphical-session.target` and
`WantedBy=graphical-session.target`), **or** detect the session at runtime by
importing the systemd user environment (`systemctl --user import-environment` at
login, or query the display via the login session) and re-check `enabled()` when
a request arrives rather than only at process start. Prefer runtime re-check so a
headless server still runs and a desktop gets notifications when the session
comes up.

**Migration / previous-version compatibility:** Unit-file and local change only;
no state or mesh impact. Reinstall rewrites the unit (`install.rs` uses
`current_exe`).

**Tests to add:** Unit — `enabled()` re-evaluates per request (inject env);
process — a request delivered after a simulated display appears is notified.
(Actual desktop interaction stays an owner-present manual check, per
current.md:55.)

**Proof required:** Owner-present manual check on the real laptop (notification
actually shown/actioned) — this is the "actual desktop notification/tray
interaction" already listed as unproved in current.md:55. Loopback cannot prove
it.

**Risks & rollback:** Low. Rollback: restore the previous unit and reinstall.

### R-4 — FQDN hostname stops the daemon (E5)

**Confirmed.** New-store `body_name` defaults to `hostname()` which returns
`/etc/hostname` verbatim (`pair.rs:70-76`); `validate_name` rejects `.`
(`pair.rs:78-91`); `serve_inner` calls `validate_store` at startup and the daemon
exits if invalid (`daemon.rs:95`). With `Restart=on-failure` the unit loops, and
`clix pair --name` cannot fix it because it needs the daemon. (Session demo in a
private namespace with `/etc/hostname=devbox.example.com`: daemon exit 1, saved
`body_name` "devbox.example.com".)

**Proposed change:** Sanitize the default in `hostname()` — take the first label
before `.`, replace disallowed chars, truncate to 63, lowercase — so a
first-boot FQDN yields a valid name. Keep `validate_name` strict for
owner-supplied names. Optionally, if a saved `body_name` is invalid at startup,
fall back to a sanitized name and log, rather than exiting (so an already-broken
machine self-heals).

**Migration / previous-version compatibility:** Affects only default naming and
startup; no format change. An already-saved invalid name is repaired on next
start (or via a documented one-shot). Previous-version peers reference this
machine by its stored `body_name`; if startup repair changes the name, the owner
must re-pair or the peer must update the name — document this and prefer
repairing *before* first pairing.

**Tests to add:** Unit — `hostname()` sanitizes `devbox.example.com` →
`devbox`; empty/all-invalid → `clix`. Process — a store created with an FQDN
`/etc/hostname` starts the daemon successfully.

**Proof required:** Loopback/process is sufficient (single-machine startup). A
real headless-server check is nice-to-have but not required (no cross-machine
behavior).

**Risks & rollback:** Risk — silently changing a name that a peer already knows;
mitigate by repairing only when the name is *invalid* (would otherwise crash),
and documenting. Rollback: revert `hostname()`.

### R-5 — Sync-before-exec failure coupling (issue 8 exec half, decision 7)

**Confirmed.** A pin sync runs before every remote exec on both sides:
`job::dispatch_one` calls `pin::sync_with_peer` before the exec RPC
(`job.rs:352`), and the runner side syncs in `mesh` `exec_v2`/`job_get_v2`
(`mesh.rs:340`) and legacy `exec` (`mesh.rs:401`). Because the pin scan fails
past 4,096 entries / depth 64 / on symlinks (`pin.rs:274-293`), a real `~/src`
of any size makes **every remote exec fail**. Production is safe today only
because the real laptop `~/src` has 0 entries; `~/Projects` has 1,391,475
entries. Source of the behavior is the v0 recipe plan:935 ("before mesh exec …
sync"), which current.md:9 calls a recipe, not the design.

**Proposed change (decision 7):** Stop syncing before every exec. Keep pin sync
at pairing and on explicit `clix pin sync BODY` only (design:41 pins `~/src`;
nothing in the design requires a pre-exec sync). Remove the calls at
`job.rs:352`, `mesh.rs:340`, `mesh.rs:401`.

**Migration / previous-version compatibility:** Mixed-version risk — if a v1 peer
still triggers a pre-exec sync and the v2 peer no longer does, exec still
succeeds (the sync was an add-on, not required for exec). No state format change.
Document that pins are now explicit.

**Tests to add:** Process — remote exec succeeds when the pin tree exceeds the
scan limits (today it fails); `clix pin sync` still works explicitly. Extend the
existing `pin.rs` mesh-exec tests.

**Proof required:** Real two-machine — populate the laptop `~/src` beyond 4,096
entries, then run `clix laptop adb devices` from the server: it must succeed
(today it would fail the pre-exec scan). Loopback can show the decoupling but not
the real installed-service interaction.

**Risks & rollback:** Risk — users who relied on implicit pre-exec sync see
staler pins; mitigate with docs and the explicit command. Rollback: re-add the
three calls.

### R-6 — Limit-literal duplication and cap tests (E8, E7 note)

**Confirmed.** The cap literals in `storage::prepare` (`storage.rs:166-172`,
then 1024/1024/64/256) are duplicated as literals in `storage::status`
(`storage.rs:305-313`); no test reaches the 10,000/30,000 caps (only
`storage_tests.rs:105`, the 1,024 pending cap). Separately, the legacy unsigned
`exec` op is still accepted (`mesh.rs:368`), which matters for L1.

**Proposed change:** Hoist the caps to named consts (single source of truth) used
by both `prepare` and `status`; add tests at the caps (folded into R-1's cap
tests). Decide on the legacy `exec` op under L1 (keep for one version behind the
handshake, then remove).

**Migration / previous-version compatibility:** No behavior change (same
numbers); pure refactor + tests. No format change.

**Tests to add:** A `prepare` test that admits exactly to each cap and refuses
the next; assert `status` reports the same numbers as `prepare` enforces.

**Proof required:** Loopback/unit only (no cross-machine behavior).

**Risks & rollback:** Trivial. Rollback: revert the refactor.

---

## Usability

### U-1 — `@BODY` addressing + colliding names (issue 5, E9, decision 9)

**Partly confirmed.** Command output: `clix status adb devices` → `error:
unexpected argument 'adb'` (exit 2); `clix laptop adb devices` routes to exec.
The `--` form exists and is tested (`cli.rs:231-253`, `tests/process.rs:577`,
docs/operations.md:6-7). But the comment `cli.rs:411` ("The same command
namespace is enforced when choosing a body name") is **false** — nothing stops a
new subcommand shadowing an existing body name, and it sits above `validate_days`
(misplaced).

**Proposed change:** Add an explicit `clix @body <cmd>` form (additive, never
ambiguous) alongside the existing `clix -- body <cmd>`. Refuse *new* pairings
whose name matches a subcommand (decision 9) — existing command-named bodies keep
working via `clix -- BODY` (design/back-compat, tested at
`tests/process.rs:577`). Fix/remove the false comment at `cli.rs:411`.

**Migration / previous-version compatibility:** `@body` is additive; `--` stays.
Refusing new colliding names does not touch existing stored names. No format
change; no mesh change.

**Tests to add:** `clix @laptop adb devices` parses to an exec on `laptop`;
pairing a body named `status` is refused with a clear message; `clix -- status
adb` still works for a pre-existing body named `status`.

**Proof required:** Loopback/process is sufficient (CLI parsing). No cross-machine
behavior.

**Risks & rollback:** Low. Rollback: drop the `@` alias and the pairing check.

### U-2 — Vocabulary (issue 6, decision 10)

**Partly confirmed.** Counts (README): hand 2×, grant 18×, body 3×, machine 22×;
docs/operations.md: grants 7×, body 7×, machines 9×. Errors say "body"
(`error.rs`, `grant.rs:74`, `pair.rs:88`); pairing/job text says "machine"
(`pair.rs`, `local.rs`, `notify.rs:59`). Tray shows "Hands" (`tray.rs:145`) next
to "…all paired machines" (`tray.rs:172`). README defines "hand" at :21 but uses
"body" undefined at :94. So the surface is inconsistent, but "hand"/"body" are in
the AGENTS.md Values — **owner decision (values)**.

**Proposed change (decision 10):** Standardize user-facing strings on "machine"
and "grant"; define "body" and "hand" once in the README glossary and use them
consistently thereafter; add `clix grants` as an alias of `clix hands`
(`local.rs:171`,`276`) so both work. Update AGENTS.md values wording only with an
explicit owner decision.

**Migration / previous-version compatibility:** User-facing text and a CLI alias;
no format or mesh change. Keep `clix hands` working (alias, not rename).

**Tests to add:** `clix grants` behaves identically to `clix hands`; a smoke
check that error strings use the chosen term. (E2's scope display, below, is
folded here: `clix hands`/`grants` must print scope/expiry/schedule, not just the
tool name — `local.rs:862-870` currently prints only `tool`.)

**Proof required:** Loopback/unit only.

**Risks & rollback:** Low. Rollback: drop the alias; revert strings.

### U-3 — `clix hands` scope visibility (E2)

**Confirmed** (folded into U-2 for shipping but tracked separately). `emit_rpc`
for `Cmd::Hands` prints only `tool` (`local.rs:862-870`); the RPC returns the
full grant, so scope/expiry/schedule are available but hidden. The native "Allow"
button label hides scope (`notify.rs:65`; fixed in S-1).

**Proposed change:** Print `tool`, `allow_from` (or "all machines"), `once`,
`until`, `schedule`, and (after S-2) any arg policy, one grant per line.

**Tests/proof/risks:** Unit on the formatter; loopback process test; trivial
rollback.

---

## Distribution

### D-1 — Release binaries + one-line installer (issue 7, decision 11)

**Confirmed.** CI builds a release binary but publishes nothing; `git tag -l` is
empty; `Cargo.toml` is 0.1.0; SECURITY.md says no stable release; the README
requires a Rust toolchain, a C compiler and `cargo install --locked --path .`.
No design change (design:120 "One package").

**Proposed change:**
- Build static musl binaries for `x86_64` (and `aarch64` if wanted) in CI on tag.
- Publish per-release checksums and signatures/attestations (e.g. minisign or
  Sigstore) as release assets.
- A one-line installer that downloads, **verifies checksum + signature**, installs
  to `~/.local/bin`, and runs `clix install`.
- Set a real version in `Cargo.toml` and tag it.

**Migration / previous-version compatibility:** Distribution only; no state or
mesh change. The installer must not clobber an existing service without the
documented stop+backup (docs/operations.md Upgrades). Pairs with D-2 so a new
binary can talk to an old one safely.

**Tests to add:** CI job asserts the built binary is static (`ldd` shows "not a
dynamic executable"), the checksum matches, and the signature verifies; a smoke
test runs the downloaded binary's `--version`.

**Proof required:** Real two-machine — install the released binary via the
one-line installer on the server (Debian) and confirm it pairs with and executes
against the laptop. Loopback cannot prove the musl static binary runs on a clean
Debian.

**Risks & rollback:** Risk — an unverified installer is a supply-chain hole;
verification is mandatory before any announcement (decision 11). Rollback: unlist
the release; the `cargo install` path still works.

### D-2 — Version handshake (L1, decision 11, E7)

**Confirmed.** docs/operations.md:173 requires upgrading all machines together;
there is no negotiation. Today the mesh distinguishes ops by name (`exec` vs
`exec_v2`, `job_get` vs `job_get_v2`) and enforces ALPN `clix/1`
(`transport.rs:33`). New signed exec does **not** fall back to unsigned
(docs/operations.md:177-178), which is correct and must stay.

**Proposed change:** Add a `hello` op (or bump ALPN to `clix/2`) that exchanges
version + supported ops before other RPCs, so a newer peer can (a) refuse clearly
against an unsupported older peer with an actionable message, and (b) know
whether to send v1 or v2 invocations — **without** ever falling back to unsigned
exec. Deprecate then remove the legacy unsigned `exec` op (`mesh.rs:368`, E7)
once the handshake is in.

**Migration / previous-version compatibility:** The handshake itself must be
back-compatible for one version: a v2 peer detects a v1 peer (no `hello`/old
ALPN) and either speaks v1 or refuses with guidance — never silently downgrades
security. Keep ALPN `clix/1` acceptance for one release, add `clix/2`, then drop
`clix/1`. This is the mechanism that lets every other format bump (issues 2, 3,
4, 8) be rolled out receiver-first.

**Tests to add:** A v2 peer against a simulated v1 peer negotiates or refuses
with a clear message and never uses unsigned exec; ALPN mismatch is rejected
(extends `transport.rs` ALPN tests).

**Proof required:** **Real two-machine, mixed-version** — run the previous
released binary on one machine and the new one on the other and confirm the
documented behavior (works, or refuses with guidance; never unsigned). This is
the core of L1 and cannot be shown on a single version or loopback alone.

**Risks & rollback:** Risk — a botched handshake bricks cross-version exec;
mitigate with the one-release overlap window. Rollback: keep accepting `clix/1`
until the handshake is proven.

### Later item L2 — macOS

Out of v0 scope (design is "Two Arch machines"). A future port needs
replacements for `openat2`+`RESOLVE_BENEATH`/`NO_SYMLINKS` and
`renameat2`+`EXCHANGE`/`NOREPLACE` in pin (`pin.rs`), the systemd install
(`install.rs`, `systemd/clix.service`), the ksni tray (`tray.rs`),
`/etc/hostname` (`pair.rs:70`) and `/dev/urandom`. Effort L. Not planned here
beyond recording the surface.

---

## 4. Suggested order of work (small shippable packets)

Each packet ends with an independently testable, shippable deliverable. Ordered
so safety-relevant, low-risk, no-migration fixes land first, and every
format-bumping change lands after the version handshake (D-2) so it can roll out
receiver-first.

1. **Docs & visibility** (E2/E9/U-3, part of S-1): `clix hands`/`grants` prints
   scope; relabel the native "Allow" button; fix the false `cli.rs:411` comment.
   No migration. *(S)*
2. **Install fixes** (R-3, R-4, E6): graphical-session ordering / runtime
   display re-check; sanitize FQDN hostname; add linger check + headless advice;
   drop ineffective `After=network-online.target`. Unit-file + local only. *(S)*
3. **Approval semantics** (S-3/E1): `decide` refuses silent widening/replacement.
   Local only. *(S–M)*
4. **CLI scope default** (S-1, decision 1/2): require `--allow`/`--all`; native
   Allow ⇒ requester. Local policy; no migration. *(S–M)*
5. **Version handshake** (D-2/L1, decision 11): `hello`/ALPN `clix/2`, one-release
   overlap. Enables receiver-first rollout of everything below. *(M)*
6. **Denial handling + cap consts/tests** (R-6 + denial half of R-1): named cap
   consts, denial rate-limit/cap, tests that reach the caps. *(S–M)*
7. **Argument allowlist** (S-2, decision 4): `args` on the Grant, `format_version`
   2. First format bump — ships after the handshake. *(M)*
8. **Receipt retirement protocol** (rest of R-1, decision 5): signed `issued_at`
   v2 invocation, per-origin watermark, imported-history budget split. Format
   bump; two-machine proof. *(L)*
9. **Storage split** (R-2, decision 12): blob outputs, split authority vs
   history, re-measure. Format bump; migration + backup. *(M–L)*
10. **Pin receiver enforcement + decouple from exec + opt-in** (S-4/R-5, issue 8,
    decisions 6/7/8): receiver-side conflict check, exclusions, per-pin direction,
    remove pre-exec sync, make pin opt-in. Two-machine proof (design:161). *(M)*
11. **`@body` + refuse colliding names** (U-1, decision 9). Additive parsing. *(S)*
12. **Vocabulary** (U-2, decision 10): standardize strings, `clix grants` alias,
    README glossary; AGENTS.md wording only on owner decision. *(S)*
13. **Release binaries** (D-1, decision 11): musl builds, checksums/signatures,
    verifying installer, real version + tag. Ships after the handshake so the
    installed binary negotiates. *(M)*

Packets 1–4, 6, 11, 12 have no state-format migration and can ship in any order
among themselves. Packets 7–10 and 13 depend on the version handshake (5).
