# Runtime evidence provenance

These are **pseudonymized copies of actual runtime records**, not fresh runs.
The original records and pre-publication working tree were preserved privately
outside the Git checkout before redaction.

The public copies replace machine names with `laptop` and `server`, personal
home paths with `/home/owner`, and the attached phone's serial with
`PHONE_SERIAL_REDACTED`. Serial bytes inside numeric stdout arrays were replaced
as well. Tailnet identifiers, where present, use synthetic values. These aliases
are not addresses or paths for operating the owner's live machines.

Timestamps, job IDs, statuses, exit codes, artifact hashes and test outcomes
were preserved. In particular, `two-machine.log` still contains its failed
immediate log-equality assertion. The separate `log-convergence.log` is the
evidence that the results later converged.

`source-sha256.json` is unchanged: it describes the source of the measured
runtime build, including the original Tailscale fixture. Later public-preparation
edits to package metadata, that fixture and its test assertions postdate the
measured build. A public copy is not byte-for-byte original evidence, and the
old source manifest must not be presented as the hash of a later revision.

The Python files are archived, pseudonymized drivers. They change grants, files
and service state and assume a particular starting deployment. Do not run them
against an existing installation as a general test suite. Use `cargo test --locked`
for isolated automated checks.
