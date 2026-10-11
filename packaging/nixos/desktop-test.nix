{ pkgs, mywhisper, module }:
let
  guiLaunch = pkgs.writeShellScript "mywhisper-desktop-launch" ''
    MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000 G_DEBUG=fatal-criticals ${mywhisper}/bin/mywhisper > /home/audit/gui.log 2>&1
    echo $? > /home/audit/gui.exit
  '';
in pkgs.testers.runNixOSTest {
  name = "mywhisper-desktop";
  nodes.desktop = { lib, ... }: {
    imports = [ module ];
    services.mywhisper.enable = true;
    # Provision synthetic credentials before starting this disposable service.
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
    environment.systemPackages = with pkgs; [ glib gsettings-desktop-schemas jq python3 wayland-utils ];
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

    def user(command: str) -> str:
        environment = "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
        return desktop.succeed("su - audit -c " + shlex.quote(environment + command))

    start_all()
    desktop.wait_for_unit("multi-user.target")
    desktop.wait_for_file("/tmp/sway-ipc.sock")
    desktop.wait_for_file("/run/user/1000/wayland-1")
    assert "NixOS" in desktop.succeed("cat /etc/os-release")
    desktop.succeed("test -e /lib64/ld-linux-x86-64.so.2")
    assert "mywhisper ${mywhisper.version}" in user("${mywhisper}/bin/mywhisper --version")
    user("GSETTINGS_SCHEMA_DIR=${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/gsettings-desktop-schemas-${pkgs.gsettings-desktop-schemas.version}/glib-2.0/schemas gsettings set org.gnome.desktop.interface color-scheme prefer-dark")
    user("swaymsg exec ${guiLaunch}")
    tree = "swaymsg -t get_tree | jq -e '.. | objects | select(.app_id? == \"com.mywhisper.App\") | .id'"
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 " + tree))
    desktop.wait_until_succeeds("grep -q 'palette -> dark' /home/audit/gui.log")
    desktop.screenshot("mywhisper-system-dark")
    user("swaymsg '[app_id=\"com.mywhisper.App\"] kill'")
    desktop.wait_for_file("/home/audit/gui.exit")
    assert user("cat /home/audit/gui.exit").strip() == "0"
    assert "owned shutdown complete; database checkpointed" in user("cat /home/audit/gui.log")
    desktop.succeed("test -s /home/audit/.local/share/mywhisper/state.db")
    header = desktop.succeed("head -c 16 /home/audit/.local/share/mywhisper/state.db | base64")
    assert header.strip() != "U1FMaXRlIGZvcm1hdCAzAA=="
    user("systemctl --user import-environment WAYLAND_DISPLAY SWAYSOCK")
    user("systemctl --user set-environment WAYLAND_DISPLAY=wayland-1")
    user("systemctl --user start mywhisper.service")
    desktop.wait_until_succeeds("su - audit -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active mywhisper.service'")
    pid = user("systemctl --user show mywhisper.service -p MainPID --value").strip()
    assert int(pid) > 0
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 journalctl --user _PID=" + pid + " --no-pager | grep -q 'activate, headless engine'"))
    user("systemctl --user stop mywhisper.service")
    assert user("systemctl --user show mywhisper.service -p ExecMainStatus --value").strip() == "0"
    journal = user("journalctl --user -u mywhisper.service --no-pager")
    assert "owned shutdown complete; database checkpointed" in journal
    assert "CRITICAL" not in journal and "panicked" not in journal
    user("systemctl --user start mywhisper.service")
    desktop.wait_until_succeeds("su - audit -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active mywhisper.service'")
    pid = user("systemctl --user show mywhisper.service -p MainPID --value").strip()
    assert int(pid) > 0
    desktop.wait_until_succeeds("su - audit -c " + shlex.quote("XDG_RUNTIME_DIR=/run/user/1000 journalctl --user _PID=" + pid + " --no-pager | grep -q 'activate, headless engine'"))
    user("systemctl --user stop mywhisper.service")
    assert user("systemctl --user show mywhisper.service -p ExecMainStatus --value").strip() == "0"
    user("journalctl --user -u mywhisper.service --no-pager > /home/audit/service.log")
    desktop.copy_from_machine("/home/audit/gui.log")
    desktop.copy_from_machine("/home/audit/service.log")
  '';
}
