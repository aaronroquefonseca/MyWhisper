# Focused guest clipboard/compositor gate (R32/R45/R39 delivery boundary).
#
# Owner: #21 (clipboard semantics). Runner: #15 (sole Nix-store/VM mutations).
# The FINAL app package and its module arrive as parameters, so the candidate
# under test is supplied by the caller and never pinned here.
#
# What this certifies, honestly labeled:
# - tool/protocol legs (writer is wrapper-shipped wl-copy, NOT the app):
#   Sway data-control MIME round-trip; wtype + guest-ydotoold chord injection
#   into foot with byte-exact assertions; stuck-modifier sentinels.
# - app presence leg: the packaged app runs as a user service alongside every
#   leg (realistic contention; journal clean). It is NOT the writer here.
# - manager leg: observation liveness only (plain offer lands in the manager
#   store). Borrowed-hint skip is NOT asserted: it needs the app writer.
# - app-writer route (dictation with synthetic audio/local model, or a
#   supported headless UI action) does not exist yet; see the OPEN note at
#   the end of the test script. No product test hooks were added for this.
{ pkgs, mywhisper, module }:
pkgs.testers.runNixOSTest {
  name = "mywhisper-clipboard-guest";
  nodes.desktop = { lib, ... }: {
    imports = [ module ];
    services.mywhisper.enable = true;
    systemd.user.services.mywhisper.wantedBy = lib.mkForce [ ];
    systemd.user.services.mywhisper.environment = {
      MYWHISPER_DB_KEY = "0000000000000000000000000000000000000000000000000000000000000000";
      GTK_A11Y = "none";
      G_DEBUG = "fatal-criticals";
      GSK_RENDERER = "cairo";
    };
    users.users.audit = {
      isNormalUser = true;
      uid = 1000;
      password = "disposable-audit";
      extraGroups = [ "input" "audio" ];
    };
    services.getty.autologinUser = "audit";
    programs.sway.enable = true;
    programs.dconf.enable = true;
    services.pipewire = { enable = true; pulse.enable = true; };
    xdg.portal = {
      enable = true;
      wlr.enable = true;
      extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
      config.sway.default = [ "gtk" ];
    };
    # uinput for the guest-only ydotool daemon. The host input stack is
    # never involved: the device node, daemon, socket and injected events
    # all live inside this disposable guest.
    boot.kernelModules = [ "uinput" ];
    environment.systemPackages = with pkgs; [
      foot
      wl-clipboard
      wtype
      ydotool
      cliphist
      glib
      jq
      python3
      wayland-utils
    ];
    environment.variables = {
      SWAYSOCK = "/tmp/sway-ipc.sock";
      WLR_RENDERER = "pixman";
      GSK_RENDERER = "cairo";
      GTK_A11Y = "none";
    };
    programs.bash.loginShellInit = ''
      if [ "$(tty)" = "/dev/tty1" ]; then
        exec sway
      fi
    '';
    virtualisation = {
      memorySize = 4096;
      cores = 2;
      qemu.options = [ "-vga none -device virtio-gpu-pci" ];
    };
  };
  testScript = ''
    import shlex

    ART = "/home/audit/clipboard-guest"
    SYN_TEXT = "synthetic transcript"

    def user(command: str) -> str:
        environment = "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
        return desktop.succeed("su - audit -c " + shlex.quote(environment + command))

    def wuser(command: str) -> str:
        # Wayland clients need the compositor socket explicitly; the stock
        # helper does not export it (mirrors the packaged wrapper, which
        # relies on its own runtime environment instead).
        environment = "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus WAYLAND_DISPLAY=wayland-1 "
        return desktop.succeed("su - audit -c " + shlex.quote(environment + command))

    def note(line: str) -> None:
        desktop.succeed("echo " + shlex.quote(line) + " >> " + ART + "/summary.log")

    def foot_tree() -> str:
        return "swaymsg -t get_tree | jq -e '.. | objects | select(.app_id? == \"foot\") | .id'"

    start_all()
    desktop.wait_for_unit("multi-user.target")
    desktop.wait_for_file("/tmp/sway-ipc.sock")
    desktop.wait_for_file("/run/user/1000/wayland-1")
    desktop.succeed("mkdir -p " + ART)
    desktop.succeed("echo -n " + shlex.quote(SYN_TEXT) + " > " + ART + "/primed-text")
    desktop.succeed("printf '\\x89PNG\\x00\\xff' > " + ART + "/primed-png")

    # P0: prerequisites. Missing package binaries fail fast with the exact
    # absent tool named. /dev/uinput gates only the ydotool leg.
    for binary in ["foot", "wl-copy", "wl-paste", "wtype", "ydotool", "cliphist", "swaymsg"]:
        user("command -v " + binary + " >/dev/null")
    note("sway: " + user("swaymsg -t get_version | head -c 200"))
    user("WAYLAND_DISPLAY=wayland-1 WAYLAND_DEBUG=1 timeout 3 wl-copy --foreground probe-globals < /dev/null > " + ART + "/globals-debug.log 2>&1 || true")
    note("globals evidence: " + ART + "/globals-debug.log (ext_data_control_manager_v1 binds first per Selection::open; zwlr fallback otherwise)")
    has_uinput = desktop.succeed("test -c /dev/uinput && echo yes || echo no").strip() == "yes"
    note("uinput present: " + str(has_uinput))

    # Packaged app runs as a user service through every leg (presence and
    # contention realism; it is not the writer in legs P1-P3).
    user("systemctl --user import-environment WAYLAND_DISPLAY SWAYSOCK")
    user("systemctl --user set-environment WAYLAND_DISPLAY=wayland-1")
    user("systemctl --user start mywhisper.service")
    desktop.wait_until_succeeds("su - audit -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active mywhisper.service'")
    journal = user("journalctl --user -u mywhisper.service --no-pager")
    assert "CRITICAL" not in journal and "panicked" not in journal

    # P1: compositor MIME round-trip (writer: wl-copy; labeled tool leg).
    # wl-paste appends a trailing newline unless -n/--no-newline is given;
    # the -n flag reads back the actual selection bytes, so cmp asserts
    # exact payload fidelity (no trimming, no weakening).
    wuser("wl-copy --type text/plain < " + ART + "/primed-text")
    assert wuser("wl-paste -n --type text/plain | cmp - " + ART + "/primed-text && echo SAME").strip() == "SAME"
    wuser("wl-copy --type image/png < " + ART + "/primed-png")
    assert wuser("wl-paste -n --type image/png | cmp - " + ART + "/primed-png && echo SAME").strip() == "SAME"
    wuser("wl-copy --clear")
    wuser("wl-paste -n --type text/plain > " + ART + "/pasted-empty 2>/dev/null || true")
    assert user("cmp -s /dev/null " + ART + "/pasted-empty && echo EMPTY").strip() == "EMPTY"
    note("P1 round-trip: text/png/empty byte-exact")

    # P2: chord injection into foot (writer: primed selection; injector leg).
    # Confirm foot's clipboard-paste binding in-guest via `man foot`
    # (Ctrl+Shift+V default); drive the documented binding on deviation.
    user("swaymsg exec foot -e sh -c 'cat > " + ART + "/pasted-wtype'")
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 " + foot_tree()))
    user("swaymsg '[app_id=\"foot\"] focus'")
    wuser("wl-copy --type text/plain < " + ART + "/primed-text")
    wuser("wtype -M ctrl -M shift -k v -m shift -m ctrl")
    wuser("wtype -M ctrl -k d -m ctrl")
    desktop.wait_until_succeeds("test -s " + ART + "/pasted-wtype")
    assert user("cmp " + ART + "/pasted-wtype " + ART + "/primed-text && echo SAME").strip() == "SAME"
    note("P2 wtype injection: byte-exact")
    if has_uinput:
        user("systemd-run --user --unit=ydotool-test ydotoold > " + ART + "/ydotoold.log 2>&1 || true")
        sock = user("for i in $(seq 1 10); do found=$(find /run/user/1000 /tmp -maxdepth 2 -name '*ydotool*' 2>/dev/null | head -1); if [ -n \"$found\" ]; then echo $found; break; fi; sleep 1; done; echo NOSOCKET").strip().splitlines()[0]
        if sock == "NOSOCKET":
            note("P2 ydotool leg UNAVAILABLE: daemon socket not discovered (log kept; recorded, not failed)")
        else:
            note("ydotool socket: " + sock)
            user("swaymsg exec foot -e sh -c 'cat > " + ART + "/pasted-ydotool'")
            desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 " + foot_tree()))
            user("swaymsg '[app_id=\"foot\"] focus'")
            wuser("wl-copy --type text/plain < " + ART + "/primed-text")
            note(wuser("ydotool key 29:1 42:1 47:1 47:0 42:0 29:0; echo ydotool-rc=$?"))
            wuser("ydotool key 29:1 32:1 32:0 29:0 || true")
            desktop.wait_until_succeeds("test -s " + ART + "/pasted-ydotool")
            assert user("cmp " + ART + "/pasted-ydotool " + ART + "/primed-text && echo SAME").strip() == "SAME"
            note("P2 ydotool injection (guest daemon/uinput only): byte-exact")
    else:
        note("P2 ydotool leg UNAVAILABLE: no /dev/uinput in guest (recorded, not failed)")
    # Stuck-modifier sentinel for every attempted backend.
    user("swaymsg exec foot -e sh -c 'cat > " + ART + "/sentinel'")
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 " + foot_tree()))
    user("swaymsg '[app_id=\"foot\"] focus'")
    wuser("wtype SENTINEL")
    wuser("wtype -M ctrl -k d -m ctrl")
    desktop.wait_until_succeeds("test -s " + ART + "/sentinel")
    assert user("grep -q SENTINEL " + ART + "/sentinel && echo CLEAN").strip() == "CLEAN"
    note("P2 sentinel: no stuck modifiers")

    # P3: manager observation liveness (writer: wl-copy; hint-skip NOT asserted).
    user("cliphist wipe || true")
    user("systemd-run --user -E WAYLAND_DISPLAY=wayland-1 --unit=cliphist-watch sh -c 'wl-paste --watch cliphist store' || true")
    wuser("wl-copy --type text/plain < " + ART + "/primed-text")
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 cliphist list | grep -q " + shlex.quote(SYN_TEXT)))
    note("P3 manager observes plain offers (hint-skip needs the app writer: OPEN)")

    # P4: guest-only isolation (host clipboard/input never involved).
    assert user("test -z \"$WAYLAND_DISPLAY\" && echo GUESTENV").strip() == "GUESTENV"
    user("systemctl is-active spice-vdagentd >/dev/null 2>&1 && exit 1 || echo NOSPICE")
    assert "CRITICAL" not in user("journalctl --user -u mywhisper.service --no-pager")
    desktop.screenshot("clipboard-guest-foot")
    for artifact in ["summary.log", "globals-debug.log", "ydotoold.log", "pasted-wtype", "sentinel"]:
        desktop.succeed("test -e " + ART + "/" + artifact + " || echo missing-ok:" + artifact)
    desktop.copy_from_machine(ART + "/summary.log")
    note("OPEN, not certified here: app-writer leg (needs synthetic-audio dictation with a local model, or a supported headless UI action — coordinate, do not fake with wl-copy); fallback (no-data-control) compositor; mutter family; manager hint-skip; live ydotool outside a disposable guest.")
  '';
}
