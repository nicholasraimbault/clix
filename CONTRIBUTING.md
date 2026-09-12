# Contributing

Clix is experimental. Start with [AGENTS.md](AGENTS.md), the
[accepted design](docs/superpowers/specs/2026-09-11-clix-design.md), and
[current work](plans/current.md). The design describes the intended system;
current work records what has actually been proved.

Keep changes focused on an observed failure or a stated milestone. Preserve
owner-local grants, fail-closed execution, explicit location, and the same
commands for owners and agents. A design change needs an explicit project
decision. Keep each authority rule and state transition in one place.

## Checks

From the repository root on Linux:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
git diff --check
```

The publication preparation uses Rust/Cargo 1.96.0 locally; the Debian machine
reported 1.96.1 in the same preparation pass. No minimum supported Rust version
has been established.
Tests use temporary state and local sockets; they do not require pairing your
real machines or granting tools on them.

For authority, retry, storage or pin changes, add a regression test that exercises
the failure boundary. Loopback tests are useful, but they do not establish
two-machine operation, physical suspend/resume, or native desktop behavior.

## Review and evidence

Describe the concrete failure, resulting behavior, commands run and remaining
uncertainty. Identify the tested revision or artifact. Do not present an ignored
test, stub, proposed acceptance test or simulated deployment as a result.

Use synthetic identities in fixtures. When sharing runtime evidence, remove
keys, pairing phrases, device identifiers and private paths. Label redactions
and retain failures and their follow-up checks. The scripts under
`plans/evidence/` are archival records, not general setup or test commands.

See [SECURITY.md](SECURITY.md) before posting a security report.
