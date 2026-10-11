# Parameterised by the flake (see flake.nix): { mywhisper } -> module.
{ mywhisper, runtimeLibraries }: { config, lib, ... }:
let
  cfg = config.services.mywhisper;
in {
  options.services.mywhisper = {
    enable = lib.mkEnableOption "MyWhisper background dictation engine";
    package = lib.mkOption {
      type = lib.types.package;
      default = mywhisper;
      description = "MyWhisper package built from the authenticated source.";
    };
    enableDownloadedRuntimes = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Provide nix-ld and CPU/Vulkan libraries for the downloaded upstream local runtimes.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    programs.nix-ld = lib.mkIf cfg.enableDownloadedRuntimes {
      enable = lib.mkDefault true;
      libraries = runtimeLibraries;
    };
    systemd.user.services.mywhisper = {
      description = "MyWhisper background dictation engine";
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/mywhisper --service";
        Restart = "on-failure";
        RestartSec = "3s";
      };
    };
  };
}
