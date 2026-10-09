# Parameterised by the flake (see flake.nix): { mywhisper } -> module.
{ mywhisper }: { config, lib, pkgs, ... }:
let
  cfg = config.services.mywhisper;
in {
  options.services.mywhisper = {
    enable = lib.mkEnableOption "MyWhisper background dictation engine";
    package = lib.mkOption {
      type = lib.types.package;
      default = mywhisper;
      description = "MyWhisper package (private release binary).";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    systemd.user.services.mywhisper = {
      description = "MyWhisper background dictation engine";
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/mywhisper-app --service";
        Restart = "on-failure";
        RestartSec = "3s";
      };
    };
  };
}
