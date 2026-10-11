{
  description = "MyWhisper voice dictation";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
    manifest = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
    runtimeLibraries = with pkgs; [ stdenv.cc.cc alsa-lib zlib openssl ];
    mywhisper = pkgs.rustPlatform.buildRustPackage {
      pname = "mywhisper";
      version = manifest.package.version;
      src = pkgs.lib.fileset.toSource {
        root = ../../.;
        fileset = pkgs.lib.fileset.unions ([
          ../../Cargo.toml ../../Cargo.lock ../../src ../../assets ../../vendor
          ../../examples ../systemd ../release.py ../notices
        ] ++ pkgs.lib.optional (builtins.pathExists ../../LICENSE) ../../LICENSE);
      };
      cargoLock.lockFile = ../../Cargo.lock;
      nativeBuildInputs = with pkgs; [ pkg-config wrapGAppsHook4 python3 ];
      buildInputs = with pkgs; [ gtk4 libadwaita gtk4-layer-shell alsa-lib openssl ];
      # Build against NixOS's own libraries instead of patching an Arch ABI.
      cargoBuildFlags = [ "--bin" "mywhisper" ];
      doCheck = true;
      nativeCheckInputs = [ pkgs.python3 pkgs.cacert pkgs.dbus pkgs.getent ];
      preCheck = ''
        export NO_PROXY=127.0.0.1,localhost,::1
        export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
      '';
      postInstall = ''
        python3 packaging/release.py --notices-output $out/share/licenses/mywhisper
        if test -f LICENSE; then
          install -Dm644 LICENSE $out/share/licenses/mywhisper/LICENSE
        fi
        install -Dm644 packaging/systemd/mywhisper.service $out/share/mywhisper/mywhisper.service
        substituteInPlace $out/share/mywhisper/mywhisper.service \
          --replace-fail /usr/bin/mywhisper $out/bin/mywhisper
        install -Dm644 assets/applications/com.mywhisper.App.desktop $out/share/applications/com.mywhisper.App.desktop
        install -Dm644 assets/icons/app-icon.svg $out/share/icons/hicolor/scalable/apps/com.mywhisper.App.svg
        install -Dm644 assets/icons/symbolic.svg $out/share/icons/hicolor/symbolic/apps/com.mywhisper.App-symbolic.svg
      '';
      preFixup = ''
        # GTK's built-in icons omit several navigation symbols. Include the
        # fallback themes even on desktops with no host icon installation.
        gappsWrapperArgs+=(--prefix XDG_DATA_DIRS : ${pkgs.lib.makeSearchPath "share" [ pkgs.adwaita-icon-theme pkgs.hicolor-icon-theme ]})
        gappsWrapperArgs+=(--prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.wl-clipboard pkgs.wtype pkgs.pulseaudio pkgs.coreutils pkgs.getent ]})
        gappsWrapperArgs+=(--prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries})
        gappsWrapperArgs+=(--set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt)
      '';
    };
  in {
    packages.${system}.default = mywhisper;
    nixosModules.default = import ./module.nix { inherit mywhisper runtimeLibraries; };
    checks.${system}.symbolic-logo = let
      probe = import ./svg-probe.nix { inherit pkgs; };
    in pkgs.runCommand "mywhisper-symbolic-logo" {
      nativeBuildInputs = [ pkgs.dbus pkgs.xvfb-run pkgs.xorg.xorgserver ];
    } ''
      export XDG_RUNTIME_DIR="$TMPDIR/runtime" XDG_CACHE_HOME="$TMPDIR/cache"
      mkdir -m700 "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME"
      export GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals GSK_RENDERER=cairo
      export FONTCONFIG_FILE=${pkgs.fontconfig.out}/etc/fonts/fonts.conf
      dbus-run-session --config-file=${pkgs.dbus}/share/dbus-1/session.conf -- xvfb-run -a ${probe}/bin/svg-probe ${../../assets/icons/symbolic.svg} 1
      touch $out
    '';
  };
}
