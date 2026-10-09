{
  description = "MyWhisper voice dictation (private binary release)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
    version = "0.9.2";
    mywhisper = pkgs.stdenv.mkDerivation {
      pname = "mywhisper";
      inherit version;
      # Private release: needs a GitHub token in nix.conf, e.g.
      #   access-tokens = github.com=ghp_...
      src = pkgs.fetchzip {
        url = "https://github.com/aaronroquefonseca/MyWhisper/releases/download/v${version}/mywhisper-v${version}-x86_64-unknown-linux-gnu.tar.gz";
        hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
      };
      nativeBuildInputs = with pkgs; [ autoPatchelfHook wrapGAppsHook4 ];
      buildInputs = with pkgs; [
        gtk4
        libadwaita
        gtk4-layer-shell
        alsa-lib
      ];
      # fetchzip unpacks the staged dir; find the binary inside it.
      # Per release: replace `hash` with the real one
      #   nix store prefetch-file --json <tarball-url>
      # (needs the same github.com access-token in nix.conf).
      installPhase = ''
        runHook preInstall
        inner=$(echo */ | head -n1)
        install -Dm755 "$inner/mywhisper" $out/bin/mywhisper
        install -Dm644 "$inner/mywhisper.service" $out/share/mywhisper/mywhisper.service
        install -Dm644 "$inner/com.mywhisper.App.desktop" $out/share/applications/com.mywhisper.App.desktop
        install -Dm644 "$inner/com.mywhisper.App.svg" $out/share/icons/hicolor/scalable/apps/com.mywhisper.App.svg
        install -Dm644 "$inner/com.mywhisper.App-symbolic.svg" $out/share/icons/hicolor/symbolic/apps/com.mywhisper.App-symbolic.svg
        runHook postInstall
      '';
    };
  in {
    packages.${system}.default = mywhisper;
    nixosModules.default = import ./module.nix { inherit mywhisper; };
  };
}
