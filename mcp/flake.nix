{
  description = "ache - unified haptics MCP stdio server (Lovense Standard API + embedded Buttplug)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "ache";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          # Only what the build and tests read.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          # The embedded Buttplug provider links the system D-Bus
          # library (btleplug's bluez stack): pkg-config finds it.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.dbus ];
          # The transport tests run a throwaway loopback HTTP stub.
          nativeCheckInputs = [ pkgs.python3 ];
          meta.mainProgram = "ache";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
            pkgs.rust-analyzer
            pkgs.python3
            pkgs.pkg-config
            pkgs.dbus
          ];
        };
      });
    };
}
