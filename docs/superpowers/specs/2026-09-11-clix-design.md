# Clix

**Value:** Your agents can use your devices without a full SSH login.

**Name:** Clix, because the devices click. Cloud Unix is just the etymology.

The cloud is the Linux user. Devices are hands and cache. No vendor VM.

## Problem

You want the agent on the server (it stays up). You want `adb` on the laptop (the phone is plugged in there).

Today the way to get both is nested SSH: the server agent logs into the laptop. That is a full login so it can touch one device. That is sketchy.

Everything else drops a piece:

- Agent on the laptop: lid down kills it, or you disable sleep.
- Agent on a server or vendor VM: no USB phone.
- Remote Control: window into the laptop; laptop must stay up.
- Wireless ADB / USB-over-IP: extra layer; the USB host still has to be up.
- You run `adb` yourself.

## Product

One owner across your machines. A small sidecar on each box, already you, from a pair. The agent (or you) asks that sidecar to run a command. Not a login. No `sshd`. Later you can allow `adb` and not a shell. SSH cannot do that cut.

| Body | Role |
|---|---|
| Server | Always on. Builds, tests, the agent. Eligible home. |
| Laptop | `adb`, emulator, browser, display. Sleeps. A hand. Home only if it is the only box awake (then lid down pauses). |
| Phone (later, Andrix) | Always carried. Real Unix (Bionic). Small edit/commit. Eligible home. |

**Hand:** a body that runs a command you named. `clix laptop adb` is real `adb` on the laptop. If that box is asleep, wait or say so. Do not pretend the server is the laptop.

**Files:** pin `~/src` on the paired boxes. Not NFS. Not full-home Syncthing.

**Mesh:** Tailscale or Headscale. Plumbing only.

## How you use it

Stay in your normal shell. Do not enter a Clix prompt.

- This machine: run the command as usual.
- Other machine: `clix <body> <cmd>`. You always see which box. No silent routing.

Same command for you, for scripts, and for the agent.

## Pairing

Clix identity is not your Tailscale account.

v0: a short phrase. Type it on the second box. Defaults on. `~/src` pins.

Later on Andrix: QR and phrase on the same card. Phone scans. Headless box still types.

If setup is a weekend wiki, it failed.

## Sleep

You do not pick a home every day. The durable log and queued work sit on a box that is up (the server, or later the phone).

- Lid down: server work keeps going. `adb` waits.
- Everything asleep: pause. We do not fake a cloud VM.
- Laptop only: lid down pauses. Honest.

Work does not teleport. `adb` stays on the laptop. glibc tests stay glibc. Andrix later is Bionic, not a costume.

## Agent

v0 works with no agent. An agent is another client of the same commands, and will be the main user later.

The agent does not type into your terminal. It asks Clix to run `clix laptop adb` the same way you do. Same user, same log.

## Andrix (later)

A new body, not a new product. Can be home if you have no server. Small work is local. When the phone is the device, use native tools; `adb` stays a laptop hand for other devices.

v0 does not wait on Andrix.

## v0

Two Arch machines.

1. Sidecar per box. Pair with a phrase.
2. `clix <body> <cmd>` runs that command on that box. v0 is a generic exec (as powerful as one remote command, but not a login session). Allowing only `adb` (not a shell) is later. Do not claim v0 is already that cut.
3. Pin `~/src` both ways. If both sides wrote, stop and say so. No invented merge.
4. One log. Tests and `adb` are the same work. `clix log` from any box.
5. Lid down: agent on the server lives; `adb` waits.
6. One package, pair, defaults.

**Success:** the server agent uses the laptop phone without logging into the laptop.

## Later

- Phone sidecar (Andrix)
- Tighter grants, revoke, lost device
- Clipboard / file handoff
- Conflict policy that does not invent merges
- QR on the pairing card

## Constraints

- Two ABIs stay two ABIs.
- Location is honest. Named body, or this machine.
- Owner can work with no agent. Agent is attributable.
- No compositor, no new VPN, no guest distro.
- Tailscale is not the product.

## What it is not

- Termux, AVF Debian VM, Linux-on-a-phone
- Apple Continuity (similar feeling, their cloud)
- Nextcloud / Olares
- SSH + tmux + Syncthing with a new name
- Andrix the ROM (that is a later body; Clix starts on Arch)

## What we add

The missing object: agent on the always-on box, `adb` on the laptop, **not a laptop login**. Pin, one log, and wait make that true instead of a wrapper around SSH.
