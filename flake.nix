{
  description = "testquorum";

  nixConfig = {
    extra-substituters = [
      "https://nixcache.testquorum.dev"
    ];
    extra-trusted-public-keys = [
      "nixcache.testquorum.dev-1:w8eYAwlsCrkOoPWvRFZa/haM19qkHu0kAHD0zkGN+0g="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";

    crane.url = "github:ipetkov/crane";

    advisory-db.url = "github:rustsec/advisory-db";
    advisory-db.flake = false;

    nix-fast-build.url = "github:Mic92/nix-fast-build";
    nix-fast-build.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, treefmt-nix, fenix, crane, advisory-db, nix-fast-build }:
    let
      systems = [ "aarch64-linux" "x86_64-linux" ];
    in
    flake-utils.lib.eachSystem systems
      (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          lib = pkgs.lib;
          toolchain = fenix.packages.${system}.combine [
            (fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
            ])
            (fenix.packages.${system}.complete.withComponents [
              "rustfmt"
            ])
          ];
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

          treefmtEval = treefmt-nix.lib.evalModule pkgs {
            projectRootFile = "flake.nix";
            programs = {
              rustfmt = {
                enable = true;
                package = toolchain;
              };
              nixpkgs-fmt.enable = true;
            };
          };

          src = craneLib.cleanCargoSource (craneLib.path ./.);
          inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

          fileSetForCrate = crate:
            lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                (craneLib.fileset.commonCargoSources crate)
              ];
            };

          commonArgs = {
            inherit src;
            strictDeps = true;
            buildInputs = [ ];
            nativeBuildInputs = [ ];
          };

          individualCrateArgs = commonArgs // {
            inherit cargoArtifacts;
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
            doCheck = false;
          };

          cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
            pname = "testquorum-deps";
            version = "git";
          });

          testquorum-runner = craneLib.buildPackage (individualCrateArgs // {
            pname = "testquorum-runner";
            cargoExtraArgs = "-p testquorum-runner";
            src = fileSetForCrate ./src/testquorum-runner;
          });

          # Static musl build for portable CI binaries
          testquorum-runner-static =
            let
              muslToolchain = fenix.packages.${system}.combine [
                (fenix.packages.${system}.stable.withComponents [
                  "cargo"
                  "clippy"
                  "rust-src"
                  "rustc"
                ])
                fenix.packages.${system}.targets.${
                if system == "aarch64-linux" then "aarch64-unknown-linux-musl" else "x86_64-unknown-linux-musl"
                }.stable.rust-std
              ];
              muslCraneLib = (crane.mkLib pkgs).overrideToolchain muslToolchain;
              muslArgs = {
                inherit version;
                strictDeps = true;
                buildInputs = [ ];
                nativeBuildInputs = [ ];
                CARGO_BUILD_TARGET = if system == "aarch64-linux" then "aarch64-unknown-linux-musl" else "x86_64-unknown-linux-musl";
                CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              };
              muslCargoArtifacts = muslCraneLib.buildDepsOnly (muslArgs // {
                pname = "testquorum-runner-musl-deps";
                version = "git";
                src = fileSetForCrate ./src/testquorum-runner;
              });
              muslBinary = muslCraneLib.buildPackage (muslArgs // {
                pname = "testquorum-runner";
                cargoExtraArgs = "-p testquorum-runner";
                src = fileSetForCrate ./src/testquorum-runner;
                cargoArtifacts = muslCargoArtifacts;
                doCheck = false;
              });
              archSuffix = if system == "aarch64-linux" then "aarch64" else "x86_64";
            in
            pkgs.stdenv.mkDerivation {
              name = "testquorum-runner-static-${archSuffix}";
              inherit version;
              buildInputs = [ pkgs.zstd muslBinary ];
              phases = [ "installPhase" ];
              installPhase = ''
                mkdir -p $out
                zstd -19 --long --force --no-progress \
                  -o $out/testquorum-runner-${archSuffix}.zst \
                  ${muslBinary}/bin/testquorum-runner
              '';
            };

        in
        {
          packages = {
            inherit testquorum-runner testquorum-runner-static;
            default = testquorum-runner;
          };

          devShells.default = craneLib.devShell {
            checks = self.checks.${system};
            packages = with pkgs; [
              rust-analyzer
              treefmtEval.config.build.wrapper
            ];
          };

          devShells.ci = pkgs.mkShell {
            packages = [
              pkgs.jq
              nix-fast-build.packages.${system}.nix-fast-build
            ];
          };

          formatter = treefmtEval.config.build.wrapper;

          checks = {
            inherit testquorum-runner;

            testquorum-clippy = craneLib.cargoClippy (commonArgs // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

            testquorum-doc = craneLib.cargoDoc (commonArgs // {
              inherit cargoArtifacts;
              env.RUSTDOCFLAGS = "--deny warnings";
            });

            formatting = treefmtEval.config.build.check self;

            testquorum-audit = craneLib.cargoAudit {
              inherit src advisory-db;
            };

            testquorum-deny = craneLib.cargoDeny {
              inherit src;
            };

            testquorum-nextest = craneLib.cargoNextest (commonArgs // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            });
          };
        }) // {
      ci = nixpkgs.lib.genAttrs systems (system:
        (self.packages.${system} or { })
        // (self.checks.${system} or { })
        // (self.devShells.${system} or { })
      );
    };
}
