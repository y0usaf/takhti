{
  description = "tomoe — a Wayland compositor built with Smithay and embedded Lua";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = lib.intersectLists lib.systems.flakeExposed lib.platforms.linux;
      forAllSystems = lib.genAttrs systems;
      nixpkgsFor = forAllSystems (system: nixpkgs.legacyPackages.${system});

      # RUSTFLAGS needed for dlopen() to find EGL/wayland-client at runtime.
      devRustflags = toString (
        map (arg: "-C link-arg=" + arg) [
          "-Wl,--push-state,--no-as-needed"
          "-lEGL"
          "-lwayland-client"
          "-Wl,--pop-state"
        ]
      );

      tomoe-package =
        {
          lib,
          pkg-config,
          rustPlatform,
          libGL,
          libdisplay-info,
          libinput,
          seatd,
          libxkbcommon,
          libgbm,
          wayland,
          systemd,        # provides libudev
          dbus,
          pipewire,       # xdg-desktop-portal-tomoe links libpipewire-0.3
        }:
        rustPlatform.buildRustPackage {
          pname = "tomoe";
          version = "0.1.0";

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./crates
              ./resources
              # The docgen test compares docs/lua-api.md against what the
              # sources generate; without it in the sandbox the test always
              # reads an empty file and fails as "stale".
              ./docs
              ./Cargo.toml
              ./Cargo.lock
            ];
          };

          cargoLock = {
            allowBuiltinFetchGit = true;
            lockFile = ./Cargo.lock;
          };

          strictDeps = true;

          nativeBuildInputs = [
            rustPlatform.bindgenHook
            pkg-config
          ];

          buildInputs = [
            libGL
            libdisplay-info
            libinput
            seatd
            libxkbcommon
            libgbm
            wayland
            systemd   # libudev
            dbus
            pipewire
          ];

          env = {
            RUSTFLAGS = devRustflags;
          };

          # tomoe-session.target is how the compositor activates
          # graphical-session.target (which refuses manual start): starting
          # the session target pulls it up via BindsTo.
          # Portal discovery: the .portal file tells xdg-desktop-portal the
          # backend exists, portals.conf routes ScreenCast to it under
          # XDG_CURRENT_DESKTOP=tomoe, and the D-Bus service file lets the
          # bus activate the binary on demand.
          postInstall = ''
            install -Dm644 resources/tomoe-session.target \
              $out/share/systemd/user/tomoe-session.target
            install -Dm644 resources/tomoe.portal \
              $out/share/xdg-desktop-portal/portals/tomoe.portal
            install -Dm644 resources/tomoe-portals.conf \
              $out/share/xdg-desktop-portal/tomoe-portals.conf
            install -d $out/share/dbus-1/services
            printf '[D-BUS Service]\nName=org.freedesktop.impl.portal.desktop.tomoe\nExec=%s/bin/xdg-desktop-portal-tomoe\n' \
              "$out" > $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tomoe.service
          '';

          meta = {
            description = "Wayland compositor with Smithay + embedded Lua";
            license = lib.licenses.agpl3Plus;
            mainProgram = "tomoe";
            platforms = lib.platforms.linux;
          };
        };
    in
    {
      packages = forAllSystems (system:
        let pkgs = nixpkgsFor.${system};
        in {
          default = pkgs.callPackage tomoe-package { };
          tomoe = pkgs.callPackage tomoe-package { };
        }
      );

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgsFor.${system};
          # Build inputs shared between the package and devShell
          buildInputs = [
            pkgs.libGL
            pkgs.libdisplay-info
            pkgs.libinput
            pkgs.seatd
            pkgs.libxkbcommon
            pkgs.libgbm
            pkgs.wayland
            pkgs.systemd   # libudev
            pkgs.dbus
            pkgs.pipewire
          ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
            ];

            nativeBuildInputs = [
              pkgs.rustPlatform.bindgenHook
              pkgs.pkg-config
            ];

            inherit buildInputs;

            env = {
              # Required for dlopen() to find EGL and wayland-client at runtime.
              RUSTFLAGS = devRustflags;
              RUST_LOG = "tomoe=debug";
            };
          };
        }
      );

      formatter = forAllSystems (system: nixpkgsFor.${system}.nixfmt-rfc-style);
    };
}
