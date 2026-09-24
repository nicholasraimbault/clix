# Operation and recovery

These commands use this machine's owner socket. Run them as its owner; an agent
uses the same commands and needs the same authority. Peer history and tool
execution do not expose owner recovery operations over the mesh.
Use `clix @BODY TOOL ARG…` (or `clix -- BODY TOOL ARG…`) to address a body whose
name matches an owner command. New pairings refuse such names; adding a command
does not rename or invalidate an existing body.

## Jobs and saved history

`clix log` shows the history available here. Each invocation has a stable ID.
Origin delivery observations and runner outcomes have separate signatures;
history can arrive through another paired machine while its author is offline.
Every author must be explicitly paired here. Pairing one relay does not grant
its unpaired contacts authority. Unknown authors and unavailable peers leave
history incomplete, and pairing a missing author triggers a rescan.

Synchronization reports what was received through a peer's observed sequence.
It cannot establish that an offline machine has no newer work. There is no
global wall-clock order or instantaneous agreement. Displayed rows follow local
history changes, so order can differ between machines. Pre-upgrade jobs retain
their actual observer's provenance; migration does not invent a runner result.
Learning about a job never inserts it into local execution admission.

```sh
clix job inspect JOB_ID
clix job output JOB_ID > saved-output
clix job output JOB_ID --stderr > saved-errors
```

Output is written as bytes. A missing payload is an error, with its recorded
length and digest still available through inspection. Waiting and running jobs
have no saved terminal output yet. A local completion that could not be saved
is labelled as unsaved uncertainty, separately from the last durable facts.

An **uncertain** job may already have had effects. Clix preserves its ID and any
once reservation. Inspect the actual tool's effects before deciding to retry:

```sh
clix job retry JOB_ID
```

Run this on the original caller. It prints a new linked job ID, preserves the
old outcome and checks the current grant when execution is attempted. It does
not clear a reservation or restore a consumed grant. Any new grant decision
happens on the machine owning that tool.

## Pin conflicts

Pin is opt-in per machine and off by default. Turn it on with `clix pin on`
(and off with `clix pin off`); `clix status` shows the current state. While pin
is off, pairing does not synchronize `~/src`, `clix pin sync` is refused, and
peers cannot list, read or write this machine's pin tree. An upgraded machine
starts with pin off; run `clix pin on` on both machines to resume syncing.

With pin on for both machines, synchronization is bidirectional, including
deletion after a shared baseline. It runs at pairing and explicitly; it no
longer runs before remote execution, so a conflict never blocks a remote run:

```sh
clix pin sync BODY
clix pin conflicts BODY
```

The receiver enforces the same honest-conflict rule as the initiator: a peer's
write is accepted only when this machine's recorded baseline matches what the
peer expected, so a local edit made since the last sync cannot be overwritten.
Pin never synchronizes `.git/hooks`.

A failed sync exits nonzero. Conflict inspection reads the named peer now;
an unreachable peer is an error, not a cached decision. To adopt the inspected
peer version on **this machine**, copy its path and token:

```sh
clix pin take-peer BODY PATH --token TOKEN
clix pin sync BODY
```

The token binds the inspected versions, baseline and peer identity. Changes
require fresh inspection. Displaced local data is retained, and the operation
cannot force an owner decision or rewrite the baseline on the peer. To choose
the opposite version, inspect and act as owner on that other machine.

## Retained and interrupted pin work

`~/src/.clix-recovery` contains receipts and displaced original inodes. An
interrupted publication or a later write through an old open descriptor stops
sync until reviewed. Inspect the actual versions instead of editing receipts:

```sh
clix pin recovery list
clix pin recovery inspect ID
clix pin recovery export ID retained --token TOKEN > saved-version
clix pin recovery export ID live --token TOKEN > live-version
clix pin recovery export ID receipt --token TOKEN > receipt-copy
```

Listing is paginated. If `next_cursor` is present, continue with
`clix pin recovery list --after CURSOR`, even if a page has no entries. Storage
usage covers the directory; listed problems describe that page. An unreferenced
artifact has no inferred original path.

Export verifies one inspected version, streams bounded chunks, and exits
nonzero if it changes. A failed export can have written a partial destination;
only a successful exit establishes a complete verified export. File metadata
and hashes detect observed changes, not a transaction with arbitrary external
writers.

Choose one action using the latest inspection token:

```sh
clix pin recovery resolve ID keep-live --token TOKEN
clix pin recovery resolve ID restore-retained --token TOKEN
```

`keep-live` acknowledges the inspected versions without deleting either.
`restore-retained` restores the saved bytes and retains the displaced live
version in a linked receipt. New edits require another inspection. Restoring
an oversized version remains subject to the 16 MiB pin-file limit.

To permanently dispose of reviewed retained data, inspect again and run:

```sh
clix pin recovery discard ID --token TOKEN
```

Pending publication must be resolved first. Disposal is journalled and can be
resumed after interruption. It preserves live pins. Retained data is never
automatically evicted to admit new pin work. Process-kill recovery tests do not
establish behavior under an actual power cut or every storage device's failure
mode.

## Capacity and storage failure

```sh
clix storage status
clix storage prune --keep-output-jobs 20
```

Pruning removes terminal output payloads in local observation order. It keeps
outcomes, digests, identities, grants, reservations and delivery receipts.
`clix storage prune` keeps no terminal output. Active jobs are not selected,
and later replication does not undo a local pruning decision.

Initial policy per sidecar:

| Resource | Limit |
| --- | --- |
| Directly launched tools | 4 |
| Shared outbound attempts | 8 |
| Mesh handlers / new exec admissions | 16 / 4 |
| Owner handlers / waiting exec clients | 32 / 8 |
| Command arguments | 256, 64 KiB total |
| Captured output | 4 MiB per stream |
| Retained job output | 16 MiB total |
| Encoded state | 128 MiB, including reserved completion capacity |
| Local execution receipts | 10,000 |
| History records | 30,000 |
| Request delivery receipts | 10,000 |
| Pending / outbound requests | 1,024 each |
| Paired machines / grants | 64 / 256 |
| Pin file / tree | 16 MiB / 4,096 entries, depth 64 |
| Pin recovery admission | 256 MiB / 4,096 entries, with recovery reserve |

These bounds constrain Clix's own admission and storage. They are not throughput
promises, a memory quota, a cap on tool descendants, or a filesystem quota on
external writers. Queue capacity also depends on space reserved for outcomes.
New work is refused at a receipt limit; output pruning does not erase those
receipts or make their IDs reusable. Indefinite operation would need a separately
proved receipt-retirement protocol. Do not delete receipts to bypass the limit.

Policy capacity does not poison healthy storage. A failed durable write does:
Clix stops mutations and tells the owner to repair storage and restart. If an
interrupted-job recovery cannot be saved on startup, the owner socket remains
available for inspection and remote work stays disabled. Free space outside
Clix or repair the filesystem before restarting. Existing files larger than the
supported state limit are preserved and refused at load; they need a separate
offline migration, not truncation.

## Install and headless operation

`clix install` writes a `systemd --user` unit and enables it. Notifications and
the tray are delivered over the desktop session and are detected at runtime by a
live compositor or X server socket, so the daemon may start before the graphical
session and still show requests once you log in; with no display, requests
appear in `clix pending`.

A `systemd --user` service only survives logout, and only starts at boot, when
the user is lingering. On a headless server enable it once:

```sh
loginctl enable-linger "$USER"
```

`clix install` prints this reminder when lingering is off.

The body name defaults from `/etc/hostname`. An FQDN or other value that is not
a valid body name is reduced to its first label (for example
`devbox.example.com` becomes `devbox`); set an explicit name with
`clix pair --name`. Before pairing, the daemon repairs an invalid saved name at
startup rather than refusing to run.

## Upgrades

This version writes state format 2, which adds per-grant argument allowlists
(`clix add --only`) and opt-in pin. A format-1 binary refuses format-2 state
rather than silently dropping an allowlist and running a tool unconstrained, so
a downgrade requires restoring the pre-upgrade backup. Upgraded machines start
with pin off; run `clix pin on` where you want `~/src` shared.

Stop the service and back up its state directory and recovery directory before
replacing the binary. The default state directory is
`${XDG_STATE_HOME:-$HOME/.local/state}/clix`. Preserve ownership and private file
permissions. Upgrade paired machines and CLI/daemon together, then restart.
This version migrates legacy history in a durable state update, retaining
identities, grants, outcomes and reservations.

New signed execution requests do not fall back to unsigned execution when an
older peer rejects them. Earlier queued jobs retain their original admission
identity and legacy provenance. Older binaries cannot interpret new recovery
phases safely. A backup rollback can also discard later replay receipts, so
downgrade or restore is not a transparent operation while peers retain work.
Keep the failed state and investigate rather than substituting a stale backup
and assuming old commands cannot run again.
