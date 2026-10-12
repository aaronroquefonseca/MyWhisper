# Container Git provenance regression

Tested correction source: `3cc92d950476a611a29182a908ba28e9fc08027e`.

The first hosted rerun (`38272434319`, head `eaf549bf5e6e46a65c71817e572fe3f79bcbc33f`)
passed formatting, strict lint, all 83 Rust tests, packaging/lifecycle regressions,
security auditing, release build, CLI checks and both broad/focused native UI
configurations. It then failed candidate staging with Git exit 128, dubious
checkout ownership. Full logs and job metadata are retained here.

The workflow now executes, inside the disposable CI container HOME:

```sh
git config --global --add safe.directory "$GITHUB_WORKSPACE"
```

This trusts one actual checkout path before version/tag/source provenance checks;
there is no wildcard or bypass of Git failures. The synthetic database key
continues to belong exclusively to the Rust test child/Nix Cargo-check subshell.

`packaging/test-release.py` now exercises actual Git and `release.stage`, using
private repositories/HOME/global-config paths and Git's ownership-test switch
`GIT_TEST_ASSUME_DIFFERENT_OWNER=1`. It asserts exit 128 and no candidate before
trust, exact committed source SHA/clean state after trust, rejection of a second
repository, and the unchanged project-license publication hold. It creates no
real tags or owner LICENSE. The mocked dependency inventory and executable are
synthetic artifact inputs; Git provenance is not mocked.

All commands from [the original clean-state command record](../pr7-fixture/COMMANDS.md)
were repeated after this correction, with the same private HOME/XDG/session/PID
fixture. Exact argv and exit status are in this directory's logs. The complete
release regression command now executes ten tests:

```sh
python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT verification \
  env -u DISPLAY -u SSH_AUTH_SOCK CARGO_HOME=/home/aaronroque/.cargo \
  RUSTUP_HOME=/home/aaronroque/.rustup python3 packaging/test-release.py
```

The new change affects only the GitHub workflow and Python regression tests.
All Rust and Nix package/check source remains identical to the explicitly
fixture-tested fresh Nix derivation recorded under `pr7-fixture`; no rebuild is
needed to establish unchanged Nix behavior. Current local and hosted security
checks already passed with the unchanged Cargo lockfile.

Final-head hosted status, immutable workflow URL, candidate integrity evidence
and tested SHA are recorded in PR #7's description/checks. Historical failing
logs here must not be mistaken for a passing run.

## Final fixture isolation

Final regression-test source: `f52058e503765d3ee56add14ac9477ed387eec42`.
The Git regression now selects private HOME/global configuration before its
first Git command and removes all inherited GIT_* context, including GIT_DIR,
GIT_WORK_TREE, templates and command configuration. This prevents local signing
policies or a caller repository from influencing synthetic commits. Actual
ownership simulation is applied only after fixture initialization.

An additional clean runner created a disposable caller repository and recorded
its HEAD, then invoked the complete ten-test suite with inherited GIT_DIR and
GIT_WORK_TREE pointing to that caller and a global config requiring a nonexistent
signing program. All ten tests passed. The caller HEAD and clean status remained
unchanged. Exact setup (inside `scripts/disposable.py ... gitconfig env -u DISPLAY
-u SSH_AUTH_SOCK bash -e -c`) was:

```sh
git init -q "$MYWHISPER_AUDIT_ROOT/caller"
git -C "$MYWHISPER_AUDIT_ROOT/caller" -c user.name=Disposable \
  -c user.email=fixture@example.invalid commit --allow-empty -qm 'Caller fixture'
callerHead=$(git -C "$MYWHISPER_AUDIT_ROOT/caller" rev-parse HEAD)
printf '[commit]\n gpgsign = true\n[gpg]\n program = /nonexistent-disposable-signer\n' > "$MYWHISPER_AUDIT_ROOT/caller.gitconfig"
GIT_DIR="$MYWHISPER_AUDIT_ROOT/caller/.git" \
GIT_WORK_TREE="$MYWHISPER_AUDIT_ROOT/caller" \
GIT_CONFIG_GLOBAL="$MYWHISPER_AUDIT_ROOT/caller.gitconfig" \
  python3 packaging/test-release.py
test "$(git -C "$MYWHISPER_AUDIT_ROOT/caller" rev-parse HEAD)" = "$callerHead"
test -z "$(git -C "$MYWHISPER_AUDIT_ROOT/caller" status --porcelain)"
```

Only Python test setup changed after the complete clean local run at `3cc92d9`;
all Rust, Cargo, workflow and Nix sources are unchanged. The complete affected
release test suite was rerun, including all existing assertions. No real desktop
Git configuration, repository, signing identity or agent is used.

## Passing hosted run

[Workflow 38275850206](https://github.com/aaronroquefonseca/MyWhisper/actions/runs/38275850206)
passed on `803ff3da2f47a978c752db63a7ae2df012da222f`: formatting, strict all-target
lint, 83 Rust tests, ten release/five upgrade/five lifecycle regressions,
security auditing, release/CLI checks, both broad/focused UI configurations,
candidate integrity and artifact upload. Publication was skipped.

The downloaded private candidate was independently checked with SHA256SUMS and
`packaging/validate-artifact.py`; digest, clean source tree, complete dependency
notices, absent owner license and zero security advisories/warnings all match.
GitHub's checked merge tree equals the PR head tree. Exact metadata is in
`hosted-artifact.json`, job/step metadata in `hosted-passed.json`, and full logs
in `hosted-passed.log`. This passing run precedes only the final test-isolation
refinement at `f52058e`; final-head CI is also checked and linked from PR #7.
