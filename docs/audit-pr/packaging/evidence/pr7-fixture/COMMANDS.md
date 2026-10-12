# PR #7 clean encrypted-store check fixture

Reviewed failing source: `94b013938e35dbbbb5883f09cc583b03dcc3d3c1`.
Corrected check source: `09af73966ddf549b836fba0080df00eee3f89ccb`.
No Rust production/test source changed. The five existing ignored integration tests are unchanged.

All local commands below run from the packaging worktree. Every gate uses
`scripts/disposable.py`: a new mode-0700 HOME, XDG config/data/cache/state/runtime,
private temporary directory, no inherited session bus/Wayland socket/database key,
and a process-owned user/PID namespace. DISPLAY and SSH_AUTH_SOCK are additionally
removed. Rust toolchain caches are explicit; they are not application state.

Prefix (shown as `clean` below for readability):

```sh
python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT verification \
  env -u DISPLAY -u SSH_AUTH_SOCK \
  CARGO_HOME=/home/aaronroque/.cargo RUSTUP_HOME=/home/aaronroque/.rustup
```

Executed commands (exact argv is also recorded in each corresponding log):

```sh
clean cargo fmt --all -- --check
clean cargo clippy --locked --all-targets -- -D warnings
clean cargo test --locked --all-targets --no-run
clean env MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000 cargo test --locked --all-targets
clean cargo build --release --locked --bin mywhisper
clean python3 packaging/test-upgrade.py
clean python3 packaging/test-release.py
clean python3 scripts/test-disposable.py
clean python3 packaging/test-cli.py
clean bash scripts/ui-smoke.sh
```

`clean-no-key.log` is an expected failing reproduction, not a passing regression:

```sh
python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT nokey \
  env -u DISPLAY -u SSH_AUTH_SOCK CARGO_HOME=/home/aaronroque/.cargo \
  RUSTUP_HOME=/home/aaronroque/.rustup cargo test --locked --lib net::tests
```

It exited 101 with the same two keyring errors. The full fixture run passed both
unchanged trust/spoofing assertions, all 83 tests and all-target harnesses.
The GitHub workflow precompiles before entering the bounded test runner so a
cold compiler does not consume the test runner's execution budget. Its key is a
child `env` argument; it is never exported to the job or GitHub environment.
CLI tests explicitly remove the database key and assert no application files.
Native smoke creates separate owned sessions for light/en and dark/es broad
36-step flows and both focused logo/toast flows.

Current security audit:

```sh
python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT security \
  env -u DISPLAY -u SSH_AUTH_SOCK \
  /home/aaronroque/.cache/mywhisper-audit-tools.susbq3ho/bin/cargo-audit \
  audit --db /home/aaronroque/.cache/mywhisper-pr-extract.ie8ambq6/advisory-db \
  --deny warnings --json
```

Fresh Nix package and clean NixOS gates:

```sh
python3 /home/aaronroque/.t3/worktrees/MyWhisper/t3code-720c4be1/target/nix-audit-run.py \
  /tmp/mywhisper-pr7-nix-build.log build --no-link --print-out-paths \
  ./packaging/nixos#checks.x86_64-linux.desktop \
  ./packaging/nixos#checks.x86_64-linux.whisper-vulkan-loader
```

The retained `nix-runner.py` specifies the exact task-owned local store, private
HOME/cache/state, namespace mounts and KVM access. No system Nix store or desktop
session is used. Nix's new package derivation executes its standard
`cargoCheckHook` in a subshell with fresh mode-0700 HOME/XDG directories, no
session/keyring socket variables, and the explicit 64-zero-hex test key. The
EXIT trap removes the fixture on success or failure. Subshell scope restores
the parent phase environment before installation/wrapping; no synthetic key is
installed in the package. The Cargo hook and all tests/flags are retained.

The owner deferred the project-license decision. Publication remains held;
this work does not merge, tag, release, deploy or change runtime/keyring policy.
