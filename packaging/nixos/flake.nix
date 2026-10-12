{
  description = "MyWhisper voice dictation";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
    manifest = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
    runtimeLibraries = with pkgs; [ stdenv.cc.cc alsa-lib zlib openssl vulkan-loader ];
    mywhisper = pkgs.rustPlatform.buildRustPackage {
      pname = "mywhisper";
      version = manifest.package.version;
      meta.license = pkgs.lib.licenses.mit;
      src = pkgs.lib.fileset.toSource {
        root = ../../.;
        fileset = pkgs.lib.fileset.unions ([
          ../../Cargo.toml ../../Cargo.lock ../../src ../../assets
          ../../examples ../systemd ../release.py ../notices
        ] ++ pkgs.lib.optional (builtins.pathExists ../../vendor) ../../vendor
          ++ pkgs.lib.optional (builtins.pathExists ../../LICENSE) ../../LICENSE);
      };
      cargoLock.lockFile = ../../Cargo.lock;
      nativeBuildInputs = with pkgs; [ pkg-config wrapGAppsHook4 python3 ];
      buildInputs = with pkgs; [ gtk4 libadwaita gtk4-layer-shell alsa-lib openssl ];
      # Build against NixOS's own libraries instead of patching an Arch ABI.
      cargoBuildFlags = [ "--bin" "mywhisper" ];
      doCheck = true;
      nativeCheckInputs = [ pkgs.python3 pkgs.cacert pkgs.dbus pkgs.getent ];
      # Retain the standard Cargo hook and all its flags/assertions. The
      # subshell scopes the synthetic key and HOME to checking, never install
      # hooks, wrappers or the resulting package.
      checkPhase = ''
        (
          umask 077
          storeFixture=$(mktemp -d "$TMPDIR/mywhisper-store-check.XXXXXXXX")
          trap 'rm -rf -- "$storeFixture"' EXIT
          export HOME="$storeFixture/home"
          export XDG_CONFIG_HOME="$storeFixture/config"
          export XDG_DATA_HOME="$storeFixture/data"
          export XDG_CACHE_HOME="$storeFixture/cache"
          export XDG_STATE_HOME="$storeFixture/state"
          export XDG_RUNTIME_DIR="$storeFixture/runtime"
          mkdir -m700 "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" \
            "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
          unset DBUS_SESSION_BUS_ADDRESS DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET \
            NIRI_SOCKET GNOME_KEYRING_CONTROL SSH_AUTH_SOCK
          export MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000
          echo "Checking with disposable HOME/XDG state and explicit encrypted-store key (no desktop session)"
          cargoCheckHook
        )
      '';
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
    checks.${system} = {
      desktop = import ./package-smoke.nix {
        inherit pkgs mywhisper;
        module = self.nixosModules.default;
      };
      whisper-vulkan-loader = import ./runtime-loader-test.nix {
        inherit pkgs mywhisper;
        module = self.nixosModules.default;
      };
      symbolic-logo = let
        probe = import ./svg-probe.nix { inherit pkgs; };
      in pkgs.runCommand "mywhisper-symbolic-logo" {
        nativeBuildInputs = [ pkgs.dbus pkgs.xvfb-run pkgs.xorg-server ];
      } ''
        export XDG_RUNTIME_DIR="$TMPDIR/runtime" XDG_CACHE_HOME="$TMPDIR/cache"
        mkdir -m700 "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME"
        export GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals GSK_RENDERER=cairo
        export FONTCONFIG_FILE=${pkgs.fontconfig.out}/etc/fonts/fonts.conf
        dbus-run-session --config-file=${pkgs.dbus}/share/dbus-1/session.conf -- xvfb-run -a ${probe}/bin/svg-probe ${../../assets/icons/symbolic.svg} 1
        touch $out
      '';
    };
  };
}
