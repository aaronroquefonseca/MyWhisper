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
