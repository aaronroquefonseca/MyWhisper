{ pkgs, mywhisper, module }:
let
  # Exercise the actual advertised upstream ELF through nix-ld, rather than
  # replacing its interpreter/RPATH with a Nix-linked fixture executable.
  runtime = pkgs.runCommand "mywhisper-whisper-vulkan-0.0.10" {
    src = pkgs.fetchurl {
      url = "https://github.com/OpenWhispr/whisper.cpp/releases/download/0.0.10/whisper-server-linux-x64-vulkan.zip";
      sha256 = "7bcd8b226cae9b0384613ce315fd8afab533fbdc5c3731343a09de8693eca203";
    };
    nativeBuildInputs = [ pkgs.unzip ];
  } ''
    test "$(stat -c%s $src)" = 24013074
    unzip -q $src
    install -Dm755 whisper-server-linux-x64-vulkan $out/bin/whisper-server
  '';
in pkgs.testers.runNixOSTest {
  name = "mywhisper-downloaded-loader";
  nodes.desktop = { ... }: {
    imports = [ module ];
    services.mywhisper = { enable = true; package = mywhisper; };
    users.users.audit = {
      isNormalUser = true;
      uid = 1000;
      password = "disposable-audit";
    };
    virtualisation.memorySize = 1024;
  };
  testScript = ''
    start_all()
    desktop.wait_for_unit("multi-user.target")
    desktop.succeed("install -o audit -g users -m700 ${runtime}/bin/whisper-server /home/audit/whisper-server")
    output = desktop.succeed("su - audit -c '/home/audit/whisper-server --help 2>&1'")
    assert "--model" in output and "--port" in output
  '';
}
