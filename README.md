# Clix

[![CI](https://github.com/nicholasraimbault/clix/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/nicholasraimbault/clix/actions/workflows/ci.yml)

**Your machines. The tools you grant.**

Run tools on your other machines without opening a login session.

Pair your machines, then grant the tools you want each one to make available.
You can limit a grant to particular paired machines, set an expiry, or allow
one successful run. Use the same commands yourself, in scripts, or through an
agent.

You choose each machine's name when you pair it. In the example below,
`laptop` is a machine that has `adb`, and `server` is a machine where you run
commands. After you grant `adb` on `laptop`, run this on `server`:

```sh
clix laptop adb devices
```

That runs `adb` on the machine named `laptop` and returns its output to your
normal shell. The command stays on the machine you named. You add or revoke a
**grant** on the machine that has the tool. Nothing granted means nothing runs
through Clix.

A few commands still use older words. `clix hands` lists grants, and so does
`clix grants`. In those commands a body is a paired machine. This README says
*grant* and *machine*.

**Experimental, Linux today.** Tested between a CachyOS (Arch-derived) laptop
and a Debian 13.6 server: a refused command, a single-use grant, and delivery
after the service restarted. The design is unfinished. See
[what is proved and what is left](plans/current.md).

## Try it

On each machine, install a Linux Rust toolchain, a C compiler and linker, and
a working systemd user manager. From this checkout, with Cargo's install
directory on your `PATH`:

```sh
cargo install --locked --path .
clix install
```

`clix install` starts a systemd user service that runs the binary you just
installed. Leave that binary where it is. Pairing and remote commands need
Tailscale, with the machines able to reach each other. Commands you run on a
machine itself still work with Tailscale off. Notifications and the tray need
the session D-Bus. The terminal commands also work without a desktop.

When you replace the binary, restart the service:

```sh
systemctl --user restart clix.service
```

Upgrade the `clix` command and the service together. If machines share
history, upgrade them together. Before an upgrade, back up Clix's state and
`~/src/.clix-recovery`. An older binary cannot use the newer recovery data.
Restoring a backup from before the upgrade drops receipts recorded after that
backup. Those receipts are how a repeated run is refused. See
[operation and recovery](docs/operations.md).

**Sharing `~/src` is off until you turn it on** with `clix pin on`. With pin
on for both machines, pairing and `clix pin sync` copy that directory both
ways, including deletions once both sides have the same baseline. Look through
`~/src` first. If both sides changed the same path, sync stops and names that
path. It does not merge the files, and a remote command still runs. While pin
is off, other machines cannot list, read, or write this machine's `~/src`.
Pin never copies anything inside a `.git` directory. Turning pin on trusts
every paired machine, and any agent using that machine's identity, with
write access to `~/src`. Files that get replaced are kept in
`~/src/.clix-recovery`. If pin fails while pairing, the pairing can still be
saved. Read the error and `clix status` before you try again. See
[Pin limits](#pin-limits).

The steps below use the example names from above. First confirm `adb devices`
works on the machine that has `adb`. On that machine:

```sh
clix pair --name laptop
```

Keep that command open. On the other machine, replace `PRINTED-PHRASE` with
the phrase this one displays:

```sh
clix pair 'PRINTED-PHRASE' --name server
clix status
```

Then grant the tool on the machine named `laptop`:

```sh
clix add adb --allow server --only devices
clix grants
```

From the machine named `server`:

```sh
clix laptop adb devices
clix log
```

Without `--name`, pairing uses the hostname. `clix status` shows this machine
and the names it has paired. Use those names in remote commands. On the
machine that has a tool, run that tool directly.

`clix @laptop adb devices` is the same run with the machine written out. Use
that, or `clix -- laptop adb devices`, when the machine's name is also a Clix
command. A new pairing refuses such a name. Put `--no-wait` before the tool
name, as in `clix --no-wait laptop adb devices` or
`clix laptop --no-wait adb devices`. Arguments after the tool name belong to
the tool.

## Grants and authority

A grant has to name which machines may use it. Pass `--allow <machine>` once
for each machine, `--server` when one of them is named `server`, or `--all`
for every paired machine, including ones you pair later. `clix add` with no
scope is an error.

`--once` allows one successful run. `--for 2h` sets how long the grant lasts.
`--only ARGS` allows one exact argument list. Repeat `--only` for another
list. Each value is split on spaces, and `--only ""` allows a run with no
arguments. `clix add adb --allow server --only devices` permits `adb devices`
and refuses `adb shell`, `adb pull …`, and extra options. Without `--only`,
the tool accepts any arguments.

Remove a grant on the machine that added it with `clix remove adb`. You can
pass the executable path Clix stored, including after that file is gone.
Another file with the same name is a different program, so it is not that
grant.

The granted program runs as the user who owns that machine. Clix does not
limit its arguments, files, or network, and it does not sandbox the process.
A shell or an interpreter can do anything that user can do. The log names the
paired machine that asked, not the person or agent at the keyboard.

That owning account is trusted. A process running as that user can use the
owner commands and read the owner keys. Clix limits what other machines can
run. An agent already running as that user is outside that limit. An agent
is optional. You approve a grant on the machine that has the tool.

## Waiting and requests

If the other machine is offline, the command is saved where you asked and
tried again after a restart. With the example names,
`clix --no-wait laptop adb devices` prints a job ID and returns while the job
is waiting or running. If that job has already finished, the command returns
its result.

`clix request laptop adb` asks for a grant and does not run the tool. On the
machine that has the tool, answer from a notification, the tray, or the
terminal:

```sh
clix pending
clix allow REQUEST_ID
# Or: clix deny REQUEST_ID
```

Copy the ID from `clix pending`. `clix allow` by itself permits one successful
run by the machine that asked. The same flags as `clix add` can name other
machines or set a duration. To grant every paired machine, use
`clix add --all` on that machine. Approving a request does not do that, and
it does not replace a different grant for the same tool.

**Allow once** in a notification or the tray is that same one run, for the
machine that asked. **Allow** (the tray says **Allow this machine**) keeps
the grant until you remove it, still only for the machine that asked. Changing
or removing the grant invalidates older pending IDs for that tool. An old
action fails. It does not approve some other request, and it does not replace
a newer decision.

If the service restarts in the middle of a run, the job is marked **uncertain**
and a single-use grant stays used. Look at what the command actually did
before you grant it again. Each output stream is kept up to 4 MiB. Past that,
the job reports that capture failed. `clix log` shows the jobs this machine
knows about. A machine that is disconnected may have jobs this one has not
seen yet.

Paired machines exchange a signed history. That includes local Clix jobs and
failures from before a run was accepted. Receiving history does not grant a
tool or start a run. A record is checked only when this machine is paired
with the machine that wrote it, and the command line says when history is
incomplete. Older records are labelled as an observation by the machine that
kept them, not as a new run.

```sh
clix job inspect JOB_ID
clix job output JOB_ID
clix job output JOB_ID --stderr
clix job retry JOB_ID
```

Run `clix job retry` on the machine that originally asked. It starts a new
job linked to the old one and checks the current grant. An uncertain
single-use reservation stays in place. When saved output passes its budget,
Clix deletes the bytes. The outcome, a digest of the output, and the receipt
that refuses a repeat stay.

## Pin limits

Pin copies regular files with UTF-8 paths, up to 16 MiB each. It does not
copy a symlink, a special file, or a file that replaced a directory (or the
other way around). Sync runs when the machines pair and when you run
`clix pin sync <machine>`. It does not run before a remote command, and it
is not a live copy. One scan covers at most 4096 entries and 64 directory
levels.

Replaced files and receipts stay in `~/src/.clix-recovery`. Sync stops until
you review a receipt that is still open, or a later write to a file Clix is
holding:

```sh
clix pin recovery list
clix pin recovery inspect ID
```

`clix pin recovery inspect` prints a token. A recovery decision needs that
token, and any change means you inspect again. Clix does not delete a kept
version on its own. New pin work stops at 256 MiB or 4096 recovery entries,
with room kept for the recovery records. A program outside Clix can still
grow a held file. This is not a disk quota. Pin will not replace a program
that currently has a grant. Remove the grant, sync, review the new file, then
grant it again. [Operation and recovery](docs/operations.md) is the procedure
for exporting a copy, choosing a version, and deleting one.

## Capacity

Each service starts at most four tools itself, and it caps how many network
connections and waiting clients it accepts. A command past that cap keeps its
job ID and waits. Encoded state is limited to 128 MiB, and saved output to
16 MiB, with room kept for results already accepted. Clix keeps receipts so a
repeated request is refused. When too many receipts have accumulated, new
commands stop being accepted. `clix storage status` shows the usage. These
are the current bounds. They are not a promise of unlimited history, a
sandbox, or production scale.

For development, see [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues
using [SECURITY.md](SECURITY.md).

## License

Copyright 2026 nicholasraimbault. Licensed under the
[Apache License, Version 2.0](LICENSE).
