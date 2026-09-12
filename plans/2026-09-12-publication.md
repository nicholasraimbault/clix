# Public repository preparation — 2026-09-12

This is publication preparation, not completion of the accepted product design.
The replacement repository is public. The owner authorized sanitizing local
history, pushing it privately, replacing the GitHub repository to resolve
retained identifying objects, and then making the replacement public.
The original repository remains a private archive. No clone or release was
performed.

## Prepared

- README and About describe owner-granted tools across machines, with Linux as
  the current implementation and laptop/server as the demonstrated workflow.
- Setup documents default pin synchronization before pairing, access through
  existing grants, and the full capabilities exposed by a granted executable.
- CONTRIBUTING and SECURITY document evidence standards and reporting.
- The owner selected Apache-2.0. The repository includes the standard
  [license text](../LICENSE), a README notice and Cargo license metadata.
- The Linux CI workflow pins Rust 1.96.0 and its action revisions, and runs
  formatting, Clippy, tests and a release build.
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
`git diff --check` also passed. The workflow YAML was parsed locally; hosted
validation is recorded separately below.

The history rewrite preserved the exact checked tip tree. `git fsck --full`
passed. Immediately after the rewrite, the only local ref was `master`; the
origin URL was restored without fetching the old history. A private old/new commit map is retained with
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

## Original private repository and retained objects

The authorized push used `--force-with-lease` against the exact old remote
master. The original repository received the sanitized history; the initial private push was
`ad285788f23ea9288031c1ea1db154b087eeb7b6`. The repository API confirmed private
visibility, Apache-2.0, one branch, no tags, no forks and no pull requests.
The push started [CI run 34675613769](https://github.com/nicholasraimbault/clix-private-history-20260912/actions/runs/34675613769)
in the original repository, retained as a private archive. Public readers may
not be able to open that historical run; its sanitized result is captured below.
That run passed formatting, Clippy with warnings denied, all 125 tests (zero
failures or ignored tests), and the release build on the hosted Linux runner.
[The captured result](evidence/2026-09-12/github-ci.json) records the exact
revision and step outcomes. This adds hosted Linux validation, not new physical
sleep, desktop, or two-machine proof.

**The branch rewrite did not remove the original repository's retained data.**
Authenticated checks after that
push returned HTTP 200 for the old tip, the introducing commit, the old fixture
blob, and the fixture at the old tip. The old fixture still contained 19 known
identifying strings; the fixture at the clean tip contained zero. These were
authorized requests against a private repository, not proof of public access.

A branch rewrite did not remove those retained GitHub objects. The original
repository must remain private. GitHub documents Support-assisted cleanup
for qualifying sensitive data; eligibility for these infrastructure identifiers
has not been established. A private cleanup-request draft and the redacted check
results were saved outside the checkout. No Support message has been sent.

## Replacement repository

On 2026-09-12, the owner authorized the replacement remedy. The original
repository (GitHub ID `1366927226`) was renamed to
`nicholasraimbault/clix-private-history-20260912` and archived while private.
A new, independent private repository, `nicholasraimbault/clix` (ID
`1366994929`), received sanitized `master` at
`c96c8cc88e75e48e1f06fe5e9b663008a20f38f2` using
`git push --set-upstream origin master:master` from the existing checkout.
The API confirmed it is not a fork and has no parent or source repository.
The About text, default branch and Actions permissions were restored; the
README badge and canonical repository links keep the `clix` URL.

Authenticated checks against the replacement returned **404 for all 25 old
object requests**: nine rewritten commits, two identifying blobs and fourteen
historical file reads. Both current file controls returned **200**, with zero
known identifying strings. The [captured checks](evidence/2026-09-12/repository-object-checks.json)
record the exact objects, paths, revision and responses. These used
`gh api repos/nicholasraimbault/clix/git/commits/{sha}`,
`gh api repos/nicholasraimbault/clix/git/blobs/{sha}`, and
`gh api repos/nicholasraimbault/clix/contents/{path}?ref={sha}`.

The retained-object blocker is resolved for the replacement repository within
those checks. This is not server-wide erasure or proof against unknown secrets.
The original private archive still returned its old identifying blobs to an
authorized reader; it must remain private. Historical CI links point to that
archive, and do not claim to be runs in the replacement.

## Publication

The owner authorized publication on 2026-09-12. At `06:07:15 UTC`, GitHub's
repository API confirmed public visibility for replacement ID `1366994929`
after `gh api --method PATCH repos/nicholasraimbault/clix -F private=false`.
The published revision was `6957936f56a88627eba734161be186b8c5483022`.
[CI run 34676803667](https://github.com/nicholasraimbault/clix/actions/runs/34676803667)
passed formatting, Clippy, all 125 tests (zero failures or ignored tests), and
the release build at that exact revision.

`gh api --method PUT repos/nicholasraimbault/clix/private-vulnerability-reporting`
enabled private vulnerability reporting. A subsequent GET to that endpoint
returned `{"enabled":true}`. The original archived repository remains private.

The [anonymous publication checks](evidence/2026-09-12/public-visibility-checks.json)
repeated all 25 historical-object requests without credentials: all returned
404. The two clean file controls returned 200 with zero known identifying
strings. The public README matched the committed bytes, and the CI badge was
passing. The archive's page and API returned 404 without authentication;
authenticated metadata still confirmed private and archived status. Supplemental
lookups through `/commits/{sha}` returned 422 for the missing old commits;
these are recorded separately from the `/git/commits/{sha}` checks.

The remaining product work is in [current work](current.md). Publication does
not establish physical sleep, desktop UX, complete shared history,
ordinary owner recovery or operating longevity.
