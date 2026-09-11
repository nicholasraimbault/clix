# Clix

**Value:** Safer, easier access to your devices for you or your agent, so a dev workflow across machines is not a pile of SSH.

You (or the agent) only get tools you added on that box. Not a login. Not a silent session. Not anything you did not add.

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

One owner across your machines. A small sidecar on each box, already you, from a pair. On a machine, you **add** the tools that box may run for this user (`clix add adb` on the laptop). The agent asks that sidecar to run a granted tool. Not a login. No `sshd`. No leftover session. The agent cannot add hands on a machine it is not sitting at.

| Body | Role |
|---|---|
| Server | Always on. Builds, tests, the agent. Eligible home. |
| Laptop | `adb`, emulator, browser, display. Sleeps. A hand. Home only if it is the only box awake (then lid down pauses). |
| Phone (later, Andrix) | Always carried. Real Unix (Bionic). Small edit/commit. Eligible home. |

**Hand:** a tool you added on a body. `clix laptop adb` is real `adb` on the laptop, only if you ran `clix add adb` there. Anything not added is denied. If that box is asleep, wait or say so. Do not pretend the server is the laptop. Each run is a visible job (who, what, running or done). No silent user session.

**Files:** pin `~/src` on the paired boxes. Not NFS. Not full-home Syncthing.

**Mesh:** Tailscale or Headscale. Plumbing only.

## How you use it

Stay in your normal shell. Do not enter a Clix prompt.

- This machine: run the command as usual.
- Other machine: `clix <body> <cmd>`. Only works if that cmd was added on that body. You always see which box. No silent routing.

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

If a tool is not granted, the agent may **request** it (`clix request laptop adb`, or a denied exec becomes a request). That shows on the box that has the tool.

**Request UI:** a small prompt on that box (notification + dialog), not a Clix control panel and not a compositor. Who wants what, from which body. Buttons: Allow once, Allow (you pick `--for` / `--until` / which body), Deny. Default button is Allow once.

`clix pending` / `clix allow` / `clix deny` still work in the terminal (headless, or you prefer the shell). Same objects as the GUI. Do not approve in the agent chat; the agent can type “yes”. Lid down: the request waits; the prompt appears when the box is up. The agent cannot `clix allow` and cannot click the dialog.

## Andrix (later)

A new body, not a new product. Can be home if you have no server. Small work is local. When the phone is the device, use native tools; `adb` stays a laptop hand for other devices.

v0 does not wait on Andrix.

## v0

Two Arch machines.

1. Sidecar per box. Pair with a phrase.
2. On each box, `clix add <tool>` grants that binary (on PATH or a path you give). `clix remove <tool>` revokes. `clix hands` lists grants. Grant and revoke only on that box, as you, not from the agent over the mesh. `clix add adb` with no extra flags: every paired body, until you `clix remove adb`. All boxes, all the time.

Narrow it when you want:

- Who: `--allow <body>` (repeatable). `clix add adb --allow server`. `--server` is the same field with body name `server`; error if no paired body has that name. Unnamed bodies are denied.
- How long: `--for 2h`, `--until 5pm`, `--once` (one successful run, then gone).
- Repeating schedule, this box’s clock. Any days of the week, any dates of the month, optional hours. Today is allowed if it matches **either** the weekday list or the month-date list (you named the days that are allowed). No `--from`/`--to` means all day.

```
clix add adb --days mon,wed,fri
clix add adb --days sat,sun --from 10am --to 2pm
clix add adb --dates 1,15
clix add adb --days fri --dates 1 --from 9am --to 5pm
clix add adb --weekdays --from 9am --to 5pm
```

`--weekdays` sets `days` to Monday–Friday on the same `Schedule`. Same check, same `--from` / `--to` / `--dates` / `--allow`. Not a second scheduler. Do not pass `--days` and `--weekdays` together.

`--once` does not stack with a repeating schedule. `--server` / `--allow` stacks with any of them. Grant still happens on this box, as you. A body cannot add itself.
3. `clix <body> <cmd>` runs that argv on that box only if `cmd` is granted there. Denied otherwise. Each run is a job in the log, visible on that device. No lingering shell.
4. Pin `~/src` both ways. If both sides wrote, stop and say so. No invented merge.
5. One log. Tests and `adb` are the same work. `clix log` from any box.
6. Lid down: agent on the server lives; `adb` waits.
7. One package, pair, defaults.

**Success:** you `clix add adb` on the laptop. The server agent runs `clix laptop adb` and cannot run anything else on the laptop. No login. You can see the job.

## Later

- Phone sidecar (Andrix)
- Arg limits, adb serial / one USB device, lost device
- Clipboard / file handoff
- Conflict policy that does not invent merges
- QR on the pairing card

## Constraints

- Two ABIs stay two ABIs.
- Location is honest. Named body, or this machine.
- Owner can work with no agent. Agent is attributable.
- No compositor, no new VPN, no guest distro. Native OS notifications are the request UI. A custom Clix window is not a substitute and not required.
- Tailscale is not the product.
- Hands are added on that box, by you. The agent cannot grant itself tools on another machine.

## Integrity (v0 is not done if any of this is faked)

These are the product. An implementation that matches the CLI and cheats the object is not Clix.

**Fail closed.** No grant ⇒ `clix <body> <cmd>` is denied. An empty grant list is not “allow everything.” A missing daemon is not “run it locally anyway.”

**Not SSH.** Mesh exec is not `ssh`, not `ssh -o Restricted`, not `ForceCommand`. If `sshd` is how a tool runs, it failed.

**Not a shell.** Exec is the granted binary (`execve`). Not `/bin/sh -c`, not `bash -c`, not `cmd.exe /c`. `argv[0]` must be that tool. `--once` is removed only after exit 0.

**One grant object.** Who is `allow_from` (CLI `--allow <body>`). `--server` is `--allow server` on that same field, and it errors if no paired body is named `server`. Time is `once` / `until` / `Schedule`. `--weekdays` only writes `Schedule.days` to Mon–Fri. A second calendar or a second allow-list is cheating.

**Pair is identity.** Phrase completes a PAKE and stores owner keys. “Both ends have the same string” over the wire is not pairing. Tailscale login is not pairing.

**Mesh is not owner.** The tailnet listener accepts `exec` and `request` only, and only from a paired key. `add`, `remove`, `allow`, `deny`, `pair` on that listener are denied in production and in tests. A `from` field in a mesh message cannot become the owner.

**Owner socket is you.** Unix socket mode 0600, peer uid must be this user. That is the only path that may change grants.

**Wait is wait.** Unreachable body ⇒ job stays `waiting`, user-facing text is `laptop is asleep, waiting…`, and it runs when that sidecar is back unless the grant has expired. One retry then `connection refused` is not wait. Raw I/O errors do not reach the user.

**Pin is honest.** Same relative path, different hashes, both changed since last sync ⇒ error that names the path and says it will not merge. Returning success in that case is cheating. Calling Syncthing or NFS is cheating.

**Visible.** A remote run is a job on both boxes. A request is a pending row plus a real OS notification (or `clix pending` if there is no display). A no-op `notify()` is cheating. Tray, when a display exists, reflects running/pending from the same store. Tests may inject a hook; production must call the OS.

**Done means two machines.** Localhost two-daemon tests are required and not a substitute for the two-Arch success: pair, `clix add adb` on the laptop, `clix laptop adb` from the server, `clix laptop bash` denied, lid down waits.

Marking a task complete with a stub, `todo!()`, `#[ignore]`, or “works if you pretend” is not complete.

## What it is not

- Termux, AVF Debian VM, Linux-on-a-phone
- Apple Continuity (similar feeling, their cloud)
- Nextcloud / Olares
- SSH + tmux + Syncthing with a new name
- Andrix the ROM (that is a later body; Clix starts on Arch)

## What we add

Safer than a laptop login. Easier than nested SSH. Same commands for you and the agent. That is the DX for work that spans machines.
