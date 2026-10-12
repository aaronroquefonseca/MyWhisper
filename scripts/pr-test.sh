#!/usr/bin/env bash
# Build an exact PR head and run a reversible desktop session. No Python needed.
set -Eeuo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
UNIT=mywhisper.service
GUI_ID=com.mywhisper.App
SERVICE_ID=com.mywhisper.Service
OVERRIDE_NAME=zz-mywhisper-pr-test.conf
GUI_PID=
SERVICE_CHANGED=0
declare -A APP_ENV=()

fail() { printf 'PR test failed: %s\n' "$*" >&2; return 1; }
manager() { timeout 30 systemctl --user "$@"; }
bus_call() {
  timeout 5 gdbus call --session --dest org.freedesktop.DBus \
    --object-path /org/freedesktop/DBus --method "org.freedesktop.DBus.$1" "$2"
}
bus_owned() {
  local reply
  reply=$(bus_call NameHasOwner "$1") || return
  case $reply in '(true,)') printf true ;; '(false,)') printf false ;; *) fail 'invalid desktop bus response' ;; esac
}
private_directory() {
  [[ ! -L $1 ]] || { fail "refusing linked directory: $1"; return 1; }
  mkdir -p -- "$1" || return
  [[ -d $1 && -O $1 ]] || { fail "directory is not user-owned: $1"; return 1; }
  chmod 700 -- "$1"
}
write_private() {
  [[ ! -e $1 && ! -L $1 ]] || { fail "file already exists: $1"; return 1; }
  (umask 077; set -o noclobber; printf '%s' "$2" > "$1")
}
checksum() { local value; value=$(sha256sum -- "$1") || return; printf '%s' "${value%% *}"; }
systemd_quote() {
  local value=$1
  [[ $value != *$'\n'* && $value != *$'\r'* ]] || { fail 'newline in systemd argument'; return 1; }
  value=${value//\\/\\\\}; value=${value//\"/\\\"}; value=${value//%/%%}
  printf '"%s"' "$value"
}
application_environment() {
  local session=$1 isolated=$2 key fallback name original_home=$HOME
  APP_ENV=()
  private_directory "$session/home"
  APP_ENV[HOME]=$session/home
  while read -r key fallback name; do
    if [[ $isolated == 1 ]]; then
      private_directory "$session/$name"
      APP_ENV[$key]=$session/$name
    else
      APP_ENV[$key]=$(realpath -m -- "${!key:-$original_home/$fallback}")
    fi
  done <<'PATHS'
XDG_CONFIG_HOME .config config
XDG_DATA_HOME .local/share data
XDG_STATE_HOME .local/state state
XDG_CACHE_HOME .cache cache
PATHS
  if [[ $isolated == 1 ]]; then
    APP_ENV[MYWHISPER_DB_KEY]=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
    [[ ${APP_ENV[MYWHISPER_DB_KEY]} =~ ^[0-9a-f]{64}$ ]] || { fail 'could not create test key'; return 1; }
    write_private "$session/test-db-key" "${APP_ENV[MYWHISPER_DB_KEY]}"$'\n'
  fi
}
override_text() {
  local binary=$1 key value quoted
  printf '# Temporary MyWhisper PR session; managed by scripts/pr-test.sh\n[Service]\n'
  printf 'ExecStart=\nEnvironmentFile=\nPassEnvironment=\nUnsetEnvironment=\n'
  # ':' disables systemd's dollar-variable expansion in the literal binary path.
  quoted=$(systemd_quote "$binary") || return
  printf 'ExecStart=:%s --service\n' "$quoted"
  for key in HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME \
    XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS DISPLAY WAYLAND_DISPLAY XAUTHORITY \
    SSH_AUTH_SOCK MYWHISPER_DB_KEY PATH LD_LIBRARY_PATH SSL_CERT_FILE SSL_CERT_DIR; do
    if [[ -v APP_ENV[$key] ]]; then value=${APP_ENV[$key]}
    elif [[ -v $key ]]; then value=${!key}
    else printf 'UnsetEnvironment=%s\n' "$key"; continue
    fi
    quoted=$(systemd_quote "$key=$value") || return
    printf 'Environment=%s\n' "$quoted"
  done
}
restore_service() {
  local control=$1 override=$2 record=$1/restore.state key value version='' active='' digest='' count=0
  if [[ ! -e $record && ! -L $record ]]; then
    [[ ! -e $override && ! -L $override ]] || { fail 'override has no recovery record; preserving it'; return 1; }
    return 0
  fi
  [[ -f $record && ! -L $record && -O $record ]] || { fail 'invalid recovery record'; return 1; }
  while IFS='=' read -r key value; do
    case $key in
      version) [[ -z $version ]] || return 1; version=$value ;;
      was_active) [[ -z $active ]] || return 1; active=$value ;;
      override_sha256) [[ -z $digest ]] || return 1; digest=$value ;;
      *) fail 'invalid recovery record'; return 1 ;;
    esac
    count=$((count + 1))
  done < "$record"
  [[ $count == 3 && $version == 1 && $active =~ ^[01]$ && $digest =~ ^[0-9a-f]{64}$ ]] || {
    fail 'invalid recovery record'; return 1;
  }
  if [[ -e $override || -L $override ]]; then
    [[ -f $override && ! -L $override && -O $override ]] || { fail 'invalid override; preserving it'; return 1; }
    [[ $(checksum "$override") == "$digest" ]] || { fail 'test override changed; preserving it and the service'; return 1; }
  fi
  manager stop "$UNIT" || return
  if [[ -e $override ]]; then rm -- "$override" || return; fi
  manager daemon-reload || return
  if [[ $active == 1 ]]; then manager start "$UNIT" || return; fi
  rm -- "$record" || return
  printf 'Installed service configuration and previous running state restored.\n'
}
wait_for_service() {
  local binary=$1 deadline=$((SECONDS + 30)) pid owner
  while (( SECONDS < deadline )); do
    pid=$(manager show "$UNIT" --property=MainPID --value) || return
    owner=$(bus_owned "$SERVICE_ID") || return
    if [[ $pid =~ ^[1-9][0-9]*$ && $owner == true ]]; then
      [[ /proc/$pid/exe -ef $binary && $(bus_call GetConnectionUnixProcessID "$SERVICE_ID") == "(uint32 $pid,)" ]] || {
        fail 'service is not running the built PR binary'; return 1;
      }
      return 0
    fi
    manager is-active "$UNIT" >/dev/null || { fail 'test service exited; inspect its journal'; return 1; }
    sleep 0.1
  done
  fail 'test service did not register within 30 seconds'
}
replace_service() {
  local control=$1 override=$2 binary=$3 state key value active='' load='' text digest effective owner
  [[ ! -e $override && ! -L $override ]] || { fail 'test override already exists; run --restore'; return 1; }
  state=$(manager show "$UNIT" --property=LoadState,ActiveState) || return
  while IFS='=' read -r key value; do
    case $key in LoadState) load=$value ;; ActiveState) active=$value ;; esac
  done <<< "$state"
  [[ $load == loaded && $active =~ ^(active|inactive|failed)$ ]] || {
    fail 'service modes need an installed, settled unit'; return 1;
  }
  text=$(override_text "$binary") || return
  digest=$(printf '%s\n' "$text" | sha256sum); digest=${digest%% *}
  local was_active=0
  [[ $active != active ]] || was_active=1
  write_private "$control/restore.state" "version=1"$'\n'"was_active=$was_active"$'\n'"override_sha256=$digest"$'\n' || return
  manager stop "$UNIT" || return
  owner=$(bus_owned "$SERVICE_ID") || return
  if [[ $owner == true ]]; then fail 'unmanaged engine still runs; stop it before testing'; return 1; fi
  private_directory "$(dirname -- "$override")" || return
  write_private "$override" "$text"$'\n' || return
  manager daemon-reload || return
  effective=$(manager show "$UNIT" --property=DropInPaths --value) || return
  # systemctl escapes whitespace inside path names; compare its final token
  # against our escaped pathname without evaluating any returned shell text.
  local escaped=$override
  escaped=${escaped//\\/\\\\}; escaped=${escaped// /\\x20}
  [[ ${effective##* } == "$escaped" ]] || { fail 'another override masks the test command'; return 1; }
  manager start "$UNIT" || return
  manager is-active "$UNIT" >/dev/null || return
  wait_for_service "$binary"
}
build_source() {
  local pr=$1 cache=$2 target=$3 source=$ROOT reference sha label='local checkout'
  if [[ -n $pr ]]; then
    reference=refs/mywhisper-pr-test/$pr
    timeout 120 git -C "$ROOT" fetch origin "+refs/pull/$pr/head:$reference" 9>&- || return
    sha=$(git -C "$ROOT" rev-parse --verify "$reference") || return
    source=$cache/worktrees/$sha
    if [[ ! -e $source && ! -L $source ]]; then
      private_directory "$cache/worktrees" || return
      git -C "$ROOT" -c core.hooksPath=/dev/null worktree add --detach "$source" "$sha" || return
    fi
    [[ -d $source && ! -L $source && -O $source ]] || { fail 'invalid cached worktree'; return 1; }
    [[ -z $(git -C "$source" status --porcelain --untracked-files=all) ]] || { fail 'cached PR source was edited'; return 1; }
    [[ $(git -C "$source" rev-parse HEAD) == "$sha" ]] || { fail 'cached worktree has the wrong commit'; return 1; }
  fi
  BUILD_SHA=$(git -C "$source" rev-parse HEAD) || return
  [[ -z $pr ]] || label="PR #$pr"
  printf 'Building %s at %.12s (debug, no tests).\n' "$label" "$BUILD_SHA"
  (cd -- "$source"; CARGO_TARGET_DIR=$target cargo build --locked --bin mywhisper 9>&-) || return
  BUILD_BINARY=$target/debug/mywhisper
  [[ -f $BUILD_BINARY ]] || { fail 'Cargo did not produce the requested binary'; return 1; }
}
wait_for_gui() {
  local pid=$1 deadline=$((SECONDS + 30)) owner
  while (( SECONDS < deadline )); do
    kill -0 "$pid" 2>/dev/null || { fail 'GUI exited before registration; inspect gui.log'; return 1; }
    owner=$(bus_owned "$GUI_ID") || return
    if [[ $owner == true ]]; then
      owner=$(bus_call GetConnectionUnixProcessID "$GUI_ID") || return
      [[ $owner == "(uint32 $pid,)" ]] || { fail 'another GUI acquired the application ID'; return 1; }
      return 0
    fi
    sleep 0.1
  done
  fail 'GUI did not register within 30 seconds; inspect gui.log'
}
gui_running() {
  local pid
  while read -r pid; do [[ $pid != "$GUI_PID" ]] || return 0; done < <(jobs -pr)
  return 1
}
cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n $GUI_PID ]] && gui_running; then
    kill -TERM -- "-$GUI_PID" 2>/dev/null || true
    kill -TERM "$GUI_PID" 2>/dev/null || true
    for _ in {1..50}; do gui_running || break; sleep 0.1; done
    if gui_running; then
      kill -KILL -- "-$GUI_PID" 2>/dev/null || true
      kill -KILL "$GUI_PID" 2>/dev/null || true
    fi
    wait "$GUI_PID" 2>/dev/null || true
  fi
  if [[ $SERVICE_CHANGED == 1 ]] && ! restore_service "$CONTROL" "$OVERRIDE"; then
    printf 'Recovery retained; run bash scripts/pr-test.sh --restore.\n' >&2
    [[ $status != 0 ]] || status=1
  fi
  exit "$status"
}
main() {
  local pr='' local_source=0 mode=gui recover=0 started=$SECONDS tool runtime cache_base repo_id cache target session key owner
  while (( $# )); do
    case $1 in
      --pr) (( $# >= 2 )) || { fail '--pr needs a number'; return 1; }; pr=$2; shift 2 ;;
      --local) local_source=1; shift ;;
      --mode) (( $# >= 2 )) || { fail '--mode needs a value'; return 1; }; mode=$2; shift 2 ;;
      --restore) recover=1; shift ;;
      -h|--help) printf 'Usage: pr-test.sh (--pr NUMBER | --local) [--mode gui|service|isolated]\n       pr-test.sh --restore\n'; return 0 ;;
      *) fail "unknown argument: $1"; return 1 ;;
    esac
  done
  [[ $mode =~ ^(gui|service|isolated)$ ]] || { fail 'invalid mode'; return 1; }
  [[ -z $pr || $pr =~ ^[1-9][0-9]*$ ]] || { fail 'PR number must be positive'; return 1; }
  if (( recover )); then
    [[ -z $pr && $local_source == 0 ]] || { fail '--restore is separate from source selection'; return 1; }
  else
    [[ ( -n $pr && $local_source == 0 ) || ( -z $pr && $local_source == 1 ) ]] || { fail 'choose --pr NUMBER or --local'; return 1; }
  fi
  local -a required=(timeout flock sha256sum)
  if (( recover )); then required+=(systemctl)
  else
    required+=(git cargo gdbus setsid setpriv realpath od tr)
    [[ $mode == gui ]] || required+=(systemctl)
  fi
  for tool in "${required[@]}"; do
    command -v "$tool" >/dev/null || { fail "missing command: $tool"; return 1; }
  done
  if (( ! recover )); then
    setpriv --pdeathsig TERM true || { fail 'setpriv needs parent-death signal support'; return 1; }
  fi
  umask 077
  runtime=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
  [[ $runtime == /* && -d $runtime && ! -L $runtime && -O $runtime ]] || { fail 'user-owned XDG_RUNTIME_DIR required'; return 1; }
  CONTROL=$runtime/mywhisper-pr-test
  OVERRIDE=$runtime/systemd/user/$UNIT.d/$OVERRIDE_NAME
  private_directory "$CONTROL"
  [[ ! -L $CONTROL/lock && ( ! -e $CONTROL/lock || ( -f $CONTROL/lock && -O $CONTROL/lock ) ) ]] || { fail 'invalid session lock'; return 1; }
  exec 9> "$CONTROL/lock"
  flock -n 9 || { fail 'another PR test session is running'; return 1; }
  if (( recover )); then restore_service "$CONTROL" "$OVERRIDE"; return; fi
  [[ ! -e $CONTROL/restore.state && ! -L $CONTROL/restore.state ]] || { fail 'interrupted service test needs --restore'; return 1; }
  owner=$(bus_owned "$GUI_ID")
  if [[ $owner == true ]]; then fail 'close the installed MyWhisper window before testing'; return 1; fi
  cache_base=${XDG_CACHE_HOME:-$HOME/.cache}
  [[ $cache_base == /* ]] || { fail 'XDG_CACHE_HOME must be absolute'; return 1; }
  repo_id=$(printf '%s' "$ROOT" | sha256sum); repo_id=${repo_id:0:12}
  cache=$cache_base/mywhisper/pr-tests/$repo_id
  private_directory "$cache"
  target=$(realpath -m -- "${CARGO_TARGET_DIR:-$ROOT/target}")
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
  build_source "$pr" "$cache" "$target"
  private_directory "$cache/sessions"
  session=$(mktemp -d "$cache/sessions/${BUILD_SHA:0:12}-XXXXXXXX")
  cp -- "$BUILD_BINARY" "$session/mywhisper"
  chmod 700 -- "$session/mywhisper"
  application_environment "$session" "$([[ $mode == isolated ]] && printf 1 || printf 0)"
  printf 'Session/logs: %s\n' "$session"
  if [[ $mode == isolated ]]; then printf 'Fresh test storage; desktop devices/session are shared.\n'
  else printf 'Uses your current profile; settings/history edits are real.\n'; fi
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  if [[ $mode != gui ]]; then SERVICE_CHANGED=1; replace_service "$CONTROL" "$OVERRIDE" "$session/mywhisper"; fi
  local -a environment=(env)
  for key in "${!APP_ENV[@]}"; do environment+=("$key=${APP_ENV[$key]}"); done
  # A separate process group permits owned cleanup. setpriv also terminates the
  # GUI when the foreground Bash helper is SIGKILLed; systemd owns the engine.
  local parent=$BASHPID
  # Expanded by the child Bash after setting the parent-death signal.
  # shellcheck disable=SC2016
  setsid setpriv --pdeathsig TERM bash -c \
    '[[ $PPID == "$1" ]] || exit 127; shift; exec "$@"' \
    bash "$parent" "${environment[@]}" "$session/mywhisper" > "$session/gui.log" 2>&1 9>&- &
  GUI_PID=$!
  wait_for_gui "$GUI_PID"
  printf 'PR GUI registered from the built binary in %ss. Close it or press Ctrl-C to finish.\n' "$((SECONDS - started))"
  wait "$GUI_PID"
}
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
