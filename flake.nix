{
  description = "moonshell — a GPU-free, Lua-scriptable Wayland desktop shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = lib.intersectLists lib.systems.flakeExposed lib.platforms.linux;
      forAllSystems = lib.genAttrs systems;
      nixpkgsFor = forAllSystems (system: nixpkgs.legacyPackages.${system});

      # Git dependencies (Cargo.lock sources without a registry checksum)
      # need their fetch hash pinned for importCargoLock. tomoe-ipc is
      # the doctrine-03 wire contract, deliberately consumed over git.
      cargoLock = {
        lockFile = ./Cargo.lock;
        outputHashes = {
          "tomoe-ipc-0.1.0" = "sha256-OMDiOpRuwPTvM10MneBDh70+hqrAnVMwuoJRlQZLrr4=";
        };
      };

      moonshell-package =
        { lib, rustPlatform }:
        rustPlatform.buildRustPackage {
          pname = "moonshell";
          version = "0.1.0";

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./crates
              ./lua # the ui.* stdlib, include_str!'d by crates/runtime
              ./examples # simple-bar acceptance fixture, include_str!'d by runtime tests
              ./Cargo.toml
              ./Cargo.lock
            ];
          };

          inherit cargoLock;

          strictDeps = true;

          # Everything is pure Rust: wayland-rs Rust backend, tiny-skia,
          # cosmic-text/fontdb. No C libraries, no pkg-config — that's
          # the memory doctrine showing up in the closure too.

          meta = {
            description = "GPU-free, Lua-scriptable Wayland desktop shell";
            license = lib.licenses.agpl3Only;
            mainProgram = "moonshell";
            platforms = lib.platforms.linux;
          };
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgsFor.${system};
        in
        {
          default = pkgs.callPackage moonshell-package { };
          moonshell = pkgs.callPackage moonshell-package { };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgsFor.${system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.sway # headless boot-check locally: WLR_BACKENDS=headless
            ];
            env.RUST_LOG = "moonshell=debug";
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = nixpkgsFor.${system};
          moonshell = self.packages.${system}.default;
        in
        {
          # Build + unit tests (buildRustPackage runs `cargo test`).
          build = moonshell;

          fmt =
            pkgs.runCommand "moonshell-fmt"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  pkgs.rustfmt
                ];
              }
              ''
                cd ${self}
                cargo fmt --check
                touch $out
              '';

          clippy = pkgs.stdenv.mkDerivation {
            name = "moonshell-clippy";
            src = moonshell.src;
            nativeBuildInputs = [
              pkgs.rustPlatform.cargoSetupHook
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
            ];
            cargoDeps = pkgs.rustPlatform.importCargoLock cargoLock;
            buildPhase = ''
              export HOME=$TMPDIR
              cargo clippy --workspace --all-targets -- -D warnings
              touch $out
            '';
            dontInstall = true;
          };

          # Doctrine 06: the bare, no-config binary must boot — map a
          # layer surface, draw, commit — under a headless compositor.
          # Also gates idle RSS at the M0 budget (20 MB).
          boot =
            pkgs.runCommand "moonshell-boot"
              {
                # Unwrapped: the sway wrapper execs via dbus-run-session,
                # and there is no session bus config in the sandbox.
                nativeBuildInputs = [ pkgs.sway-unwrapped ];
              }
              ''
            export XDG_RUNTIME_DIR=$(mktemp -d)
            chmod 700 $XDG_RUNTIME_DIR
            export WLR_BACKENDS=headless
            export WLR_RENDERER=pixman
            export WLR_LIBINPUT_NO_DEVICES=1

            # fontdb honors FONTCONFIG_FILE; give the sandbox one real
            # font so the text path is exercised, not skipped.
            printf '%s\n' \
              '<?xml version="1.0"?>' \
              '<fontconfig>' \
              "  <dir>${pkgs.dejavu_fonts}/share/fonts</dir>" \
              "  <cachedir>$XDG_RUNTIME_DIR/fc-cache</cachedir>" \
              '</fontconfig>' > fonts.conf
            export FONTCONFIG_FILE=$PWD/fonts.conf

            touch empty-config
            sway -c empty-config &
            for _ in $(seq 100); do
              [ -S $XDG_RUNTIME_DIR/wayland-1 ] && break
              sleep 0.1
            done
            [ -S $XDG_RUNTIME_DIR/wayland-1 ] || { echo "headless sway never came up" >&2; exit 1; }
            export WAYLAND_DISPLAY=wayland-1

            # Boot: first frame committed => exit 0.
            timeout 30 ${moonshell}/bin/moonshell --boot-check

            # RSS gate: idle for 3s, then check against the M0 budget.
            ${moonshell}/bin/moonshell &
            ms=$!
            sleep 3
            rss_kb=$(awk '/^VmRSS/{print $2}' /proc/$ms/status)
            kill $ms
            echo "idle RSS: ''${rss_kb} kB (budget 20480 kB)"
            [ "$rss_kb" -le 20480 ] || { echo "RSS over M0 budget" >&2; exit 1; }
            touch $out
          '';
        }
      );

      formatter = forAllSystems (system: nixpkgsFor.${system}.nixfmt-rfc-style);
    };
}
