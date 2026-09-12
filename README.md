# Clix

[![CI](https://github.com/nicholasraimbault/clix/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/nicholasraimbault/clix/actions/workflows/ci.yml)

**Your machines. The tools you grant.**

Run tools on your other machines without opening a login session.

Pair your machines, then grant the tools you want each one to make available.
You can limit a grant to particular paired machines, set an expiry, or allow
one successful run. Use the same commands yourself, in scripts, or through an
agent.

For example, after pairing and granting `adb` on the laptop, run this from the server:

```sh
clix laptop adb devices
```

That runs the laptop's `adb` and returns its output to your normal shell.
The command stays on the machine you named. Clix calls a tool grant a **hand**;
you add or revoke it on the machine that has the tool. Nothing granted means
nothing runs through Clix.

**Experimental, Linux today.** Tested on a real CachyOS (Arch-derived) laptop
and Debian 13.6 server, including denial, single-use grants and delivery after
daemon restarts. The full v0 design is unfinished. See
[current proof and remaining work](plans/current.md).

## Try it

On each machine, use a Linux Rust toolchain, a C compiler/linker, and a working
systemd user manager. Build from this checkout, with Cargo's install directory
on your `PATH`:

```sh
cargo install --locked --path .
clix install
```

`clix install` creates and starts a systemd user service pointing at the binary
you ran; keep that binary in place. Both machines need Tailscale running and
reachable over their tailnet for pairing and remote work. Local owner controls
remain available when Tailscale is off. Desktop notifications and the tray use
the session D-Bus; the terminal commands also work headlessly.

After upgrading the binary, restart the service with
`systemctl --user restart clix.service`. Upgrade the CLI and daemon together;
upgrade paired machines together for shared history. Back up Clix's state and
`~/src/.clix-recovery` before upgrading. Older binaries cannot safely operate on
the new recovery phases; restoring a pre-upgrade backup also loses later replay
receipts. See [operation and recovery](docs/operations.md).

**Before pairing:** Clix automatically pins `~/src` on both machines. Pairing and
each remote execution synchronize that directory in both directions, including
deletions after a shared baseline. Review its contents first. Conflicts stop
sync and remote execution; there is no automatic merge. Existing grants without
`--allow` also apply to newly paired machines. A pin failure after pairing can
leave the pairing saved; read the error and `clix status` before retrying.
See the pin limits below.

For example, first confirm `adb devices` works locally on the laptop. To use
that installed tool from a server, start on the laptop:

```sh
clix pair --name laptop
```

Keep that command open. On the server, replace `PRINTED-PHRASE` with the phrase
the laptop displays:

```sh
clix pair 'PRINTED-PHRASE' --name server
clix status
```

Then grant the tool locally on the laptop:

```sh
clix add adb --allow server
clix hands
```

From the server:

```sh
clix laptop adb devices
clix log
```

These are example names. Without `--name`, pairing uses the hostname;
`clix status` shows this machine and its paired names. Use the actual target
name in remote commands. On this machine, run tools normally.
If a body's name matches a Clix command, use `clix -- BODY TOOL ARG…`.
This explicit form preserves tool arguments, including `--no-wait`; place
Clix's own `--no-wait` before `--` when needed.

## Grants and authority

Without `--allow`, an added tool is available to every paired machine.
`--once` permits one successful run; `--for 2h` limits the grant's lifetime.
Revoke a grant on the machine that added it with `clix remove adb`.
You can also pass its stored executable path, even after that file is deleted.
A different executable with the same basename is not a matching path.

A grant exposes the binary's capabilities under the owner's account. Clix does
not constrain its arguments or sandbox its subprocesses, files or network access.
Granting a shell, interpreter or another tool that runs arbitrary code can give
broad account access. Attribution identifies the paired machine, not the
individual human or agent using it.

The local owner account is trusted. A process with unrestricted access as that
Unix user can use owner controls and read owner keys. Clix restricts remote
tool access; it does not isolate an agent already running as the local owner.
An agent is optional, and approval happens on the machine granting the tool.

## Waiting and requests

Offline jobs and explicit requests are saved on the caller and retried after
restart. `clix --no-wait laptop adb devices` prints the job ID while the job is
waiting or running; an already-completed command returns its result.
`clix request laptop adb` asks for permission without executing. The owner
handles requests on the target through native notifications, the tray, or the
terminal:

```sh
clix pending
clix allow REQUEST_ID
# Or: clix deny REQUEST_ID
```

Copy the ID printed by `clix pending`. Terminal approval defaults to one
successful run by the requester. Grant flags select permitted paired machines
and set a duration. Native **Allow once** has the same requester scope;
native **Allow** grants all paired machines until revoked. A grant change or
revocation invalidates older pending IDs for that tool. Stale actions fail
without selecting another request or overwriting the newer owner decision.

A restarted runner reports an interrupted job as **uncertain** and retains its
single-use reservation. Inspect the actual command's effects before replacing
that grant. Output is captured as bytes, up to 4 MiB per stream; exceeding the
limit reports capture failure. `clix log` shows locally known jobs; it is not
an instantaneous view of disconnected machines. Signed history converges among
paired machines, including local Clix jobs and failures before execution.
Imported history never grants permission or schedules a job. Authors must be
explicitly paired to verify their records; the CLI reports incomplete history.
Older records are labelled as observations by the machine that retained them.

Inspect a job, retrieve its saved bytes, or explicitly create a new attempt:

```sh
clix job inspect JOB_ID
clix job output JOB_ID
clix job output JOB_ID --stderr
clix job retry JOB_ID
```

A retry is available on the original caller. It gets a new ID linked to the
old attempt and checks the current grant. It preserves any uncertain once
reservation. Saved output has a retention budget; outcomes, output digests and
replay receipts remain after output is pruned.

## Current pin limits

Pin supports regular files with UTF-8 paths, at most 16 MiB per file. Symlinks,
special files, and file/directory replacement are unsupported. Sync runs at
pairing, before remote execution, and when you run `clix pin sync BODY`.
It is not continuous background replication. A scan supports up to 4096 entries
and 64 levels of directories.

Displaced files and receipts remain in `~/src/.clix-recovery`. A pending receipt
or later edit to a retained inode stops further sync until the owner reviews it.
Use `clix pin recovery list` and `clix pin recovery inspect ID` to review
retained versions. Recovery decisions require the token from that inspection;
changed versions require a fresh decision. Nothing retained is automatically
deleted. New pin work stops at the recovery admission budget of 256 MiB or
4096 entries, with reserved space for recovery metadata. External writers can
still grow retained inodes; this is not a filesystem quota. Pin will not replace
an actively granted executable; remove its grant, sync and review the replacement,
then add it again. See [operation and recovery](docs/operations.md) for export,
resolution, disposal and the remaining operating limits.

Clix admits at most four directly launched tools per sidecar and bounds network
and waiting-client concurrency. Busy jobs remain queued under the same ID.
State has a 128 MiB encoded limit and a 16 MiB retained-output budget, with
space reserved for admitted outcomes. Execution and request receipts are kept
to reject replays; their count limits eventually stop new admissions.
`clix storage status` shows usage. This is an initial bounded operating policy,
not a claim of indefinite retention, process sandboxing or proved production scale.

For development, see [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues
using [SECURITY.md](SECURITY.md).

## License

Copyright 2026 nicholasraimbault. Licensed under the
[Apache License, Version 2.0](LICENSE).
