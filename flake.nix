{
  description = "Interactive historical diff viewer for Nix generations";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "nix-interactive";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # --suffix so a system-installed nix (matching the daemon) wins over the bundled one.
          postInstall = ''
            wrapProgram $out/bin/nixi --suffix PATH : ${
              pkgs.lib.makeBinPath [
                pkgs.nix
                pkgs.nvd
                pkgs.git
              ]
            }
          '';
          meta = {
            description = "Interactive historical diff viewer for Nix generations";
            homepage = "https://github.com/Geekiac/nix-interactive";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.unix;
            mainProgram = "nixi";
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      checks = forAllSystems (
        pkgs:
        let
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          build = package;
          clippy = package.overrideAttrs (old: {
            pname = "nix-interactive-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --offline --all-targets -- -D warnings";
            doCheck = false;
            installPhase = "touch $out";
            postInstall = "";
          });
          fmt =
            pkgs.runCommand "nix-interactive-fmt"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  pkgs.rustfmt
                ];
              }
              ''
                cd ${package.src}
                cargo fmt --check
                touch $out
              '';
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
