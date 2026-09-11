# Clix

**One-liner:** The cloud is the Linux user. Devices are hands and cache. No vendor VM.

**Name:** Clix, because it clicks. Cloud Unix is secondary.

## Problem

Agents want to live on a machine that stays up (the server). Tools often live on another (the laptop: `adb`, the plugged-in phone). A human SSHes and remembers which box has what. Lid down kills the laptop agent. Nested SSH (server agent logs into the laptop) is the only way to get both, and it is the wrong object: a **login** so an agent can touch a USB device.

Current fixes each drop a piece:

- Agent on the laptop plus stay-awake hacks (`caffeinate`, `pmset`, plastic lid-wedges).
- Agent on a server or vendor VM: no USB phone.
- Remote Control: the phone is a window into the laptop; laptop must stay up.
- Wireless ADB / USB-over-IP: extra layer; the USB host still has to be up.
- MCP-SSH / nested Tailscale SSH: the trombone.
- Human runs `adb` and stitches two logs.

Nobody ships: always-on agent, real `adb` on the laptop, no login, one user, one log, wait if the lid is down.

## Product

One **owner** identity across bodies.

| Body | Role |
|---|---|
| Server | Always on. Archive. Heavy build/test. Eligible home. |
| Laptop | Interactive. `adb`, emulator, browser, display. Sleeps. A hand. Home only when it is the only awake body (then lid down pauses). |
| Phone (later, Andrix) | Always carried. Real Unix body (Bionic). Small edit/commit. Eligible home. |

**User** = one principal, one log. Not a process glued to a host.

**Hands** = a body that can run a named command. The agent (or you) calls `clix laptop adb`. That is real `adb` on the laptop. If that body is asleep, say so or wait. Do not pretend the server is the laptop.

**Data** = one object catalog. Partial replica (working set / pin, default `~/src`). Not NFS, not full-home Syncthing, not two clones you manage by hand.

**Mesh** = Tailscale or Headscale. Plumbing. Not the product. Not Tailcat (netcat-over-magicsock, no tailnet).

## Why this is not SSH

SSH starts a **login** on the laptop. New session, that user, `sshd`, keys. The agent is now a logged-in user on your machine.

Clix does not log in. Each box runs a sidecar as **you**, from a pair. The agent asks that sidecar: run `adb` with these args. A call, not a login. Later: allow `adb` and not a shell. SSH cannot make that cut.

The command `clix laptop adb` looks like `ssh laptop adb`. If that wrapper is all we ship, it *is* glorified SSH. The product is the rest: pair, pin, one log, wait, hand not login.

## How it feels

You stay in the real shell on the box in your hands (bash, fish, whatever is already there). You do not enter a special Clix prompt. That would be SSH again.

- Local command: just run it (`adb` on the laptop, `cargo test` on the server).
- Other body: `clix <body> <cmd>`. Devs see where work runs. No silent placement.
- Same spelling for scripts and for the agent.

Jobs bar: opening the laptop is being at the computer. Unlocking the phone (later) is being at the computer. No second world.

## Placement (one rule)

The shell you type into is **this** machine. Named commands go to a named body. No “best body” scheduler. No unique-hand inference.

If two phones both have `adb`, you name which. If the hand is asleep, wait or say “laptop is down.”

## Pairing

Clix identity is not a Tailscale account. Mesh only carries packets.

v0: short phrase. Laptop shows `mango-river-4`. Server: `clix pair mango-river-4`. Defaults on. `~/src` pins.

Later (Andrix): same pair on one card, QR plus phrase. No mode switch. Phone scans; headless box still types.

Weekend wiki = failed.

## Home and sleep

You never pick a home in daily use. Among awake bodies, one quietly hosts the durable work (log, queued hands). Server if it is up; phone later if that is the always-carried box.

- Lid down: work that can run elsewhere keeps running. `adb` waits.
- Every body asleep: the thread pauses. No ghost VM. Apple still has iCloud; we do not.
- Laptop only: no always-on body. Lid down pauses. Honest, not a bug.

Picking up another body does not migrate in-flight processes. The **user** (log, files, next command) is there. Hands stay on the body that accepted them. Two ABIs stay two ABIs: a glibc `cargo test` does not become a Bionic process.

## Agent

v0 is human-usable. An agent is another client of the same verbs, and will be the primary user later.

The agent does not type into your shell. It asks Clix to run the same `clix laptop adb` the shell already does. Same user, same log, attributable.

If a person can do it, an agent can do it. Do not invent a second API.

## Andrix (later)

Andrix is a new body, not a new architecture. GrapheneOS-derived Android, one native Bionic Unix, owner UID, CE home, unlock → terminal → edit → compile → leave → return.

- Can **be** home for laptop+phone people (no server).
- Small work is local Unix. Heavy work still names the strong box.
- When the phone *is* the device, native tools; `adb` remains a laptop hand for other devices.
- Never a glibc process in a Bionic costume.

v0 does not wait on Andrix.

## v0

Two Arch machines (laptop + a box playing home).

1. Sidecar per box, owner pair (phrase).
2. `clix <body> <cmd>` runs that argv on that body. Native shells stay native. v0 is a generic exec (same power as one remote command). Scoped grants (`adb` only, not a shell) are later; do not advertise v0 as that cut.
3. Pin sync for `~/src` only. Both ways. If both sides wrote, stop and say so. Do not invent a merge.
4. One log: tests on the server and `adb` on the laptop are the same work. `clix log` shows it from any body.
5. Lid down: thread lives on the awake box; `adb` waits.
6. One package, pair, defaults.

**Success:** during Android-style work, the human stops being the router. The server agent uses the laptop phone without a login. Lid down does not kill the agent.

## Later

- Phone sidecar (Andrix).
- Richer grants, revoke, lost-device.
- Clipboard / file handoff.
- Conflict policy that does not invent merges.
- QR on the pairing card.

## Constraints

- Two ABIs stay two ABIs. No merge Bionic/glibc.
- Location stays honest. Named body, or it is this machine.
- Agent is attributable and scoped. Owner can work with no agent.
- Do not build a compositor, a new VPN, or a guest distro.
- Do not make Tailscale the product.
- A hand is a hand. v0 exec is still “run this command as me” on that body (no login session, no sshd). That is already better than a laptop login. It is not yet a tight allowlist. Do not call v0 “the agent can only use adb.”

## What it is not

- Linux-on-a-phone, Termux, AVF Debian VM, DroidDesk
- Apple Continuity / iCloud (feeling similar, tenant different)
- Nextcloud, Olares, “personal cloud OS”
- SSH + tmux + Syncthing with a new name
- Andrix the ROM (Andrix = later phone body; Clix starts on Arch)

## New value

Always-on brain, desk hand, no login, one user, one log, wait if asleep. Every current solution throws one of those away. The command looks like SSH. The object is not a login.
