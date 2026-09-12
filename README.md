# Clix

Safer access to your devices for you or your agent. Not a login.

## v0 (two Arch boxes on Tailscale)

On both:

    cargo install --path .
    clix install

On the laptop:

    clix pair
    # prints: pair with: mango-river-4
    clix add adb

On the server:

    clix pair mango-river-4
    clix laptop adb devices

If the laptop is closed, that last command waits. If adb is not added, the laptop gets a notification: Allow once / Allow / Deny.

Work state is recorded in [plans/current.md](plans/current.md).
