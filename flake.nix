{
  description = "zax - Zed Agent eXtensions";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachSystem [ "aarch64-darwin" "x86_64-linux" ] (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        # `cleanCargoSource` keeps only Cargo manifests and `.rs` files, which
        # would strip the tmux.conf template and the recorded gh JSON fixtures
        # that `include_str!` pulls in at compile time. Keep those two trees too.
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          name = "zax-source";
          filter = path: type:
            (builtins.match ".*/assets/.*" path != null)
            || (builtins.match ".*/tests/fixtures/.*" path != null)
            || (craneLib.filterCargoSources path type);
        };

        # tmux, gh and git are runtime dependencies, and the integration tests
        # shell out to all three, so they are build inputs for `checks` too.
        runtimeTools = [ pkgs.tmux pkgs.gh pkgs.git ];

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = runtimeTools;
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        zax = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          doCheck = false;
        });
      in
      {
        packages.default = zax;
        packages.zax = zax;

        checks = {
          inherit zax;

          fmt = craneLib.cargoFmt { inherit src; };

          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          tests = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = runtimeTools ++ [
            pkgs.cargo-nextest
            pkgs.rust-analyzer
          ];
        };
      });
}
