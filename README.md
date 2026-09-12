# Clix

Run owner-granted tools across your machines, from your normal shell. The owner
and an agent use the same commands. A **hand** is a granted tool on a named
machine: `clix laptop adb devices` runs that machine's `adb`.

The owner adds grants on the machine that has the tool. Nothing granted means
nothing runs through Clix. Pairing does not give the other machine a login.

**Experimental, Linux today.** The hand has been exercised on a real CachyOS
(Arch-derived) laptop and Debian 13.6 server, including denial, single-use grants
and delivery after daemon restarts. That is one demonstrated workflow; Clix is
not limited to laptops. The full accepted v0 is unfinished. See
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

## Grants and authority

Without `--allow`, an added tool is available to every paired machine.
`--once` permits one successful run; `--for 2h` limits the grant's lifetime.
Revoke a grant on the machine that added it with `clix remove adb`.

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
handles requests on the target through native notifications or `clix pending`,
`clix allow` (default: once), and `clix deny`.

A restarted runner reports an interrupted job as **uncertain** and retains its
single-use reservation. Inspect the actual command's effects before replacing
that grant. Output is captured as bytes, up to 4 MiB per stream; exceeding the
limit reports capture failure. `clix log` shows locally known jobs; it is not
yet the complete shared history promised by the design.

## Current pin limits

Pin supports regular files with UTF-8 paths, at most 16 MiB per file. Symlinks,
special files, and file/directory replacement are unsupported. Sync runs at
pairing and before remote execution, not continuously in the background.

Displaced files and receipts remain in `~/src/.clix-recovery`. A pending receipt
or later edit to a retained inode stops further sync until the owner reviews it.
There is no owner recovery command or retention policy yet. Pin will not replace
an actively granted executable; remove its grant, sync and review the replacement,
then add it again. See the [repair record](plans/2026-09-11-repair.md) for the
current manual recovery procedure.

For development, see [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues
using [SECURITY.md](SECURITY.md).

## License

Copyright 2026 nicholasraimbault. Licensed under the
[Apache License, Version 2.0](LICENSE).
