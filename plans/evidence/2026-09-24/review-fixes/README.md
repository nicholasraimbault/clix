# Review-fix pass: two-machine evidence

This record covers the [review-fix pass](../../../2026-09-24-review-fixes.md) at
commit `caafedfafdb74a47a1352ee53eaa751813d5b253` on master. Public machine names
are the usual pseudonyms, `laptop` and `server`. Private hostnames, tailnet
addresses, state and keys stay outside the repository.

## Artifacts

| | Laptop (CachyOS, Arch-derived) | Server (Debian 13) |
|---|---|---|
| Toolchain | rustc/cargo 1.96.0 | cargo 1.96.1 |
| Build | `cargo build --release --locked` | fresh clone of the pushed commit, same command |
| Release SHA-256 | `99f6b225f931ea71a061ed965289dd21a2564101aac40e3948b238c7213a9aa7` | `57a388d4cb31635511ed9157e515b52bdc6a0cb020dac8250b73f097cde54385` |

These are native builds of the same commit. They are not claimed to be
reproducible binary builds.

- **Local suite:** `cargo test --locked --no-fail-fast` passed 225 tests with no
  failures, twice. `cargo fmt --check` and `cargo clippy --all-targets` were clean.
- **GitHub CI:** run `35978677733` on the pushed commit completed with
  `success`. It ran formatting, lint, the full suite in isolated namespaces and
  a release build.

## Two-machine fixtures

The runs used a disposable daemon on each machine. Each had its own state,
socket and pin directories and its own fixture port, bound to that machine's
real Tailscale interface. Neither ran over loopback. The laptop acted as runner,
holding the grants and tools. The server acted as origin, running
`clix @laptop …` just as the server agent does. Both fixtures paired with a
phrase: the laptop listened and the server joined. They were named `laptopfix`
and `serverfix`, and the commands and messages below are quoted exactly.

| # | Behavior across machines | Result |
|---|---|---|
| T1 | Laptop runs `clix add tool.sh --allow serverfix --only ok`; `clix grants` shows `tool.sh  → serverfix  only: ok` | shown |
| T1 | Server runs `clix @laptopfix tool.sh ok` | exit 0; the tool ran on the laptop |
| T1 | Server runs `clix @laptopfix tool.sh evil` | exit 1: `tool.sh is granted only with these arguments: ok` |
| T1 | The tool's own record of executions on the laptop | exactly one, `ok` |
| T2 | Server runs `clix @laptopfix bash -c true` (not granted) | exit 1: `bash is not added on laptopfix` |
| T3 | Server runs `clix pin sync laptopfix` while pin is off | exit 1: `pin is off on serverfix; enable it there with clix pin on` |
| T4 | Both machines run `clix pin on`, then server runs `clix pin sync laptopfix` | synced; `repo/work.txt` copied to the server |
| T4 | Same sync, with `repo/.git/config` present on the laptop | not copied |
| — | Production daemons during the run (PID and full state SHA-256 on both machines, before and after) | unchanged |

The result was 9 passed and 0 failed. Afterwards, both fixture daemons had
stopped and both fixture directories were removed, which was checked on each
machine.

## Not established by this record

- **Native notification scopes.** Native "Allow once" and "Allow" decisions
  depend on desktop interaction with the owner present. Unit and loopback tests
  cover the scope mapping.
- **Receiver-side pin write check.** A crafted direct write from a peer is
  covered only by the loopback test
  `receiver_rejects_a_pin_put_that_diverges_from_its_baseline`. The known
  commit-then-write limit is described in the plan.
- **Installed services.** Upgrading the installed services is recorded
  separately.
- **Everything else.** Physical suspend/resume, sustained daily use and the
  specifically two-Arch milestone.
