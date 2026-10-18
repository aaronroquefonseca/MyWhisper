# PR iteration and releases

Each PR should describe one coherent purpose in its title. A purpose can cover
several related changes: "Make packaging and upgrades reliable across Arch and
NixOS" is a useful release entry. Explain the individual changes and verification
in the description, including migration or breaking details when relevant. No
special commit prefix, changelog file or rigid PR-body format is required.

GitHub generates the initial release notes from merged PR titles and links.
Optional `bug`, `enhancement`, `security` and `breaking-change` labels group
them; unlabelled PRs appear in Other improvements. Maintainers can use
`skip-changelog` for changes that should not appear. Release preparation PRs
receive `release` and are excluded from the next generated notes.

## Running a PR locally

Prerequisites: Linux, the normal native build dependencies, Cargo/Rust, Git,
`gdbus`, Bash, coreutils, util-linux (`flock`, `setsid`, `setpriv` with parent-death
signal support), and a working desktop session. No Python or jq is needed by
the PR tester. Service modes additionally require the
installed systemd user service. Close the installed GUI first: GTK would otherwise
activate that instance instead of starting the PR binary.

```sh
bash scripts/pr-test.sh --pr 123 --mode gui
bash scripts/pr-test.sh --pr 123 --mode service
bash scripts/pr-test.sh --pr 123 --mode isolated
```

- `gui`: runs the PR GUI with your current profile; leaves the installed service
  unchanged. It does not start another engine. Your settings/history edits are real.
- `service`: builds first, then temporarily replaces the installed service command
  and opens the PR GUI using your current profile. Normal keyring behavior remains.
- `isolated`: temporarily replaces the service and opens the GUI with fresh
  configuration, history, identity and caches, plus a private random database key.
  It is storage isolation, not a sandbox: the desktop, microphone, hotkeys,
  network and runtime sockets are still real. Configure a model in the fresh profile
  before testing dictation.

The helper fetches the exact PR head from `origin` into a separate cached worktree.
It builds only the debug executable (`cargo build --locked --bin mywhisper`),
never tests or a release build. It reuses this checkout's `target` directory, or
your explicit `CARGO_TARGET_DIR`. A warm native dependency cache is the path to
the two-minute target; a cold build or changed dependencies can take longer.
The executable is copied into a private session directory so another build
cannot replace it mid-session. The helper reports source SHA and launch time,
and verifies actual GUI/service process ownership.

Use `--local` instead of `--pr` to test this checkout, including local edits.
If your existing build cache is elsewhere, point `CARGO_TARGET_DIR` at it.

The foreground command ends when you close the GUI or press Ctrl-C. Service
modes stop the test engine, remove only their own runtime override and restore
whether the installed service was previously running. They do not install a
binary, enable/disable the unit, overwrite persistent systemd configuration,
or roll back intentional edits to the real profile. A temporary HOME keeps
the GUI's service-installer path out of your real HOME; explicit XDG directories
retain the selected profile and the original desktop runtime/bus is preserved.

Logs and isolated storage are retained in the printed private session directory;
the test-only database key is in its mode-0600 `test-db-key` file. If the helper
was killed with SIGKILL or its terminal disappeared before recovery:

```sh
bash scripts/pr-test.sh --restore
```

The GUI terminates with its foreground helper even on SIGKILL. Systemd owns the
test engine's descendants; `--restore` recovers the service after an uncatchable
interruption. A recovery record is retained if restoration fails. Changed
overrides are preserved and reported instead of being deleted. Concurrent test
sessions are rejected. Service toggles during GUI-only testing still act on the
installed service; in service modes they act on the temporary command.

## Preparing a release

Choose a new semantic version only when ready to collect the pending work.
The script uses the previous published stable release as its baseline; pass
`--previous-tag` to select a different one, including a prerelease. This makes
the range explicit, rather than guessing from PR dates. The previous tag must
be an ancestor of the selected source commit.

Preview from a checkout with the relevant tags fetched:

```sh
python3 scripts/prepare-release.py 0.9.5 --previous-tag v0.9.4 --output /tmp/mywhisper-release-preview
```

This writes a patch, PR-linked Markdown and source/baseline metadata. It creates
no commit, remote branch, PR, tag or GitHub Release and preserves your checkout.
Use `--notes-file PATH` to seed preparation with Markdown you have already reviewed.

To create the preparation PR from the current `origin/main` commit:

```sh
python3 scripts/prepare-release.py 0.9.5 --create-pr
```

Alternatively, run **prepare release** in GitHub Actions and supply the version.
Select `preview_only` to download a patch and generated notes without opening a PR.
Preparation creates `release/vVERSION-SOURCE_SHA`, updates Cargo, the locked root-package
version and Arch `pkgver`/`pkgrel`, and prepends the generated notes to
`CHANGELOG.md`. Nix continues deriving its version from Cargo. Already existing
release tags or branches are not overwritten; retain reviewed edits in the PR.
Git transport and `gh` authentication must be configured for the repository.
Preparation also records its source commit and previous-tag identity in
`.github/release-source.json`; this is automation-owned metadata, not a file
feature PR authors must supply.

Review that PR before merging. Expand a title into several bullets when needed,
keep the PR link, and add migrations/breaking information from the descriptions.
Ordinary Markdown paragraphs, `*`/`-` lists and nested headings are supported.
Release-version headings must use `## [VERSION](https://github.com/OWNER/REPO/releases/tag/vVERSION) - YYYY-MM-DD`;
older heading forms remain readable in historical sections. Keep the generated
`###` categories from `.github/release.yml`, and add a one-line upgrade action
to entries marked **Breaking:** or **Security:**. Feature PR authors do not
need to edit release headings. Generated GitHub notes are a starting point for review; direct
commits or important details absent from titles need a maintainer's addition.

The Actions preparation job uses `GITHUB_TOKEN` by default. Enable
Settings → Actions → General → Allow GitHub Actions to create and approve pull
requests before using it. Repository token permissions otherwise remain read-only.
GitHub may require
approval before running CI on its bot-created PR; approve those runs before
merging. An optional `RELEASE_PR_TOKEN` (GitHub App or suitably scoped maintainer
token) can be used when automatic downstream triggering is needed. Preparation
does not publish or tag anything and does not remove the existing license gate.

After CI and practical testing pass, tag that tested release commit and push
the tag. The tag must match the committed Cargo/Arch/changelog version and point
to a commit already merged into `origin/main`. The
release pipeline builds and verifies the candidate, then publishes the same
archive and notes; prerelease tags stay marked as prereleases.
Release preparation requires Python 3.11+ only when run locally; the GitHub
Actions preparation workflow supplies Python and can be used without a local
interpreter. Normal Cargo development and the Bash PR tester do not require it.

CI checks the source snapshot when the package version or snapshot changes and
when a tag is verified. If product/dependency changes were merged after preparation, it
blocks publication until the notes and source snapshot are refreshed. The
snapshot may change only after those changes are included and reviewed in the
release PR. You can also close the stale preparation PR and prepare a fresh one
from the updated main commit; its existing branch is deliberately not replaced.
The source suffix lets fresh preparation create a new branch without deleting
or overwriting reviewed notes in the old PR. Close the superseded PR.

If preparation has already merged but the version is still untagged, run
`python3 scripts/prepare-release.py VERSION --refresh --create-pr`, or select
`refresh_notes` in Actions. This opens a new preparation PR for the same version.
It regenerates notes from the previous published release; review the diff and
retain any manually expanded details. Use `--notes-file` to supply reviewed notes
instead. Refresh emits the current version heading with the preparation date
and release link, preserves historical sections, and refuses versions that
already have a remote tag.

The archive checksum cannot be known before the build. CI therefore generates
and attaches a final `PKGBUILD` with the checked archive's checksum, and includes
both files in SHA256SUMS without changing the tagged source. The source PKGBUILD
is a template for `update.sh`, which already injects the selected release's
authoritative digest into a temporary recipe before installing. For direct
`makepkg` use, use the release-attached recipe, not the source template's old
artifact checksum. Never move an existing published version tag.

## CI triggers

- PR opened/reopened/new commits: formatting, strict lint, Rust tests,
  packaging/lifecycle/iteration regressions, security audit, release/CLI build,
  both broad and focused UI configurations, candidate integrity and artifact upload.
- Push to `main`: the same checks and a downloadable candidate; no publication.
- Push a `v*` tag: the same checks, tag/version/source/license validation and
  publication of the checked candidate.
- Manual **release checks**, tag required: verify an existing tag; publication
  defaults to off and must be requested explicitly. Historical tags predating
  the tooling cannot be checked with this workflow.
- Manual **prepare release**, version required: open a release preparation PR,
  or upload a patch/notes preview when `preview_only` is selected; no product
  build, publication or tag creation.

Nix and actual input-device/service/desktop acceptance tests remain separate
gates; ordinary hosted CI does not establish those behaviors. This workflow does
not mark the broader release audit complete.
