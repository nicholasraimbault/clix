# Public repository preparation — 2026-09-12

This is publication preparation, not completion of the accepted product design.
The repository remains private. No clone, push, release or visibility change was
performed. The owner authorized removing identifying device information from
local history.

## Prepared

- README and About describe owner-granted tools across machines, with Linux as
  the current implementation and laptop/server as the demonstrated workflow.
- Setup documents default pin synchronization before pairing, access through
  existing grants, and the full capabilities exposed by a granted executable.
- CONTRIBUTING and SECURITY document evidence standards and reporting.
- The owner selected Apache-2.0. The repository includes the standard
  [license text](../LICENSE), a README notice and Cargo license metadata.
- The Linux CI workflow pins Rust 1.96.0 and its action revisions, and runs
  formatting, Clippy, tests and a release build. It has not run on GitHub.
- Parser fixtures are synthetic. Public runtime records are explicitly
  pseudonymized; artifact hashes and the original failed assertion are preserved.
  Original records and a Git bundle were saved privately outside this checkout.
- Local history was sanitized with git-filter-repo 2.47.0. The cleanup preserved
  all 44 original commits and the two preparation commits, with their messages and authors.
  The public GitHub username/noreply authorship is intentional. Device names,
  tailnet addresses, node identifiers and personal paths use replacements.
  The measured repair is committed as `42290b4`.

## Validation

The [pre-license build-source manifest](evidence/2026-09-12/source-sha256.json) identifies
the source checked during the preparation pass. Compared with the earlier installed-build
manifest, only package metadata, the Tailscale fixture and its assertions changed.
The production behavior of the installed hand was not changed or re-proved.

[Saved command output](evidence/2026-09-12/publication-checks.log) records Rust/Cargo
1.96.0 on the laptop, Clippy with warnings denied, **125 passing tests with zero
failures or ignored tests**, and a successful release build. Formatting and
`git diff --check` also passed. The workflow YAML was parsed locally; a hosted
run is still required before claiming CI passes.

The history rewrite preserved the exact checked tip tree. `git fsck --full`
passed. The only remaining local ref is `master`; the origin URL was restored
without fetching the old history. A private old/new commit map is retained with
the backup. Captured log whitespace is preserved and excluded from whitespace
lint through `.gitattributes`.

At `51f2495`, the disclosure scan covered all 46 reachable commits, 228 blobs and 71 tracked
files. It found zero occurrences of the known private identifiers, zero
additional candidates under its credential/address patterns, and no missing
protected provenance. This is a targeted scan of publishable Git content; it
does not establish that ignored local artifacts or the hosted repository are
sanitized.

The subsequent license addition changes only licensing documentation and Cargo
metadata. The saved test/build manifest is retained unchanged and predates the
`license = "Apache-2.0"` field; those tests are not presented as a new run after
the licensing change.

License verification: `cargo metadata --offline --locked --format-version 1
--no-deps` reported `Apache-2.0`; `cargo package --list --offline --locked
--allow-dirty` included LICENSE, README.md and Cargo.toml. `git diff --check`
passed.

## Before publication

- The private GitHub repository still contains the old history. Replacing it
  requires a separately authorized push. Verify remote refs and access to old
  identifying objects before changing visibility; a local rewrite alone cannot
  clean the hosted repository. See
  [GitHub's history-removal guidance](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository).
- Run the prepared CI workflow against the actual candidate on GitHub.
- Enable and verify GitHub private vulnerability reporting when the repository
  is made public. Its API returned 404 while private; no working form is claimed.
  [GitHub documents this feature for public repositories](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/configure-vulnerability-reporting/configure-for-a-repository).

The remaining product work is in [current work](current.md). Making the source
public would not establish physical sleep, desktop UX, complete shared history,
ordinary owner recovery or operating longevity.
