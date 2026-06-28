{
  description = "testquorum";

  nixConfig = {
    extra-substituters = [
      "https://nixcache.testquorum.dev"
    ];
    extra-trusted-public-keys = [
      "nixcache.testquorum.dev-1:aS+CJF8O8Ebirc6hypMfq/061h5TJlbsej1+zUJHPec="
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

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              (lib.fileset.fileFilter (file: file.hasExt "json") ./src)
            ];
          };
          inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

          fileSetForCrate = crate:
            lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                (craneLib.fileset.commonCargoSources crate)
                (lib.fileset.fileFilter (file: file.hasExt "json") crate)
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

          testquorum-api = craneLib.buildPackage (individualCrateArgs // {
            pname = "testquorum-api";
            cargoExtraArgs = "-p testquorum-api";
            src = fileSetForCrate ./src/testquorum-api;
          });

          testquorum-config = craneLib.buildPackage (individualCrateArgs // {
            pname = "testquorum-config";
            cargoExtraArgs = "-p testquorum-config";
            src = fileSetForCrate ./src/testquorum-config;
          });

          testquorum-runner = craneLib.buildPackage (individualCrateArgs // {
            pname = "testquorum-runner";
            cargoExtraArgs = "-p testquorum-runner";
            src = fileSetForCrate ./src/testquorum-runner;
          });

          # Static musl build for portable CI binaries
          testquorum-runner-static =
            let
              muslTriple = if system == "aarch64-linux" then "aarch64-unknown-linux-musl" else "x86_64-unknown-linux-musl";
              # Cross-compile C dependencies (e.g. aws-lc-sys pulled in by reqwest's
              # rustls feature) against musl headers; otherwise build scripts pick
              # up glibc's fortify macros and the final link fails on missing
              # symbols like __memcpy_chk and __isoc23_strtol.
              muslPkgs =
                if system == "aarch64-linux"
                then pkgs.pkgsCross.aarch64-multiplatform-musl
                else pkgs.pkgsCross.musl64;
              muslCC = "${muslPkgs.stdenv.cc}/bin/${muslPkgs.stdenv.cc.targetPrefix}cc";
              muslAR = "${muslPkgs.stdenv.cc.bintools}/bin/${muslPkgs.stdenv.cc.targetPrefix}ar";
              cargoLinkerEnv =
                if system == "aarch64-linux"
                then "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"
                else "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER";
              muslToolchain = fenix.packages.${system}.combine [
                (fenix.packages.${system}.stable.withComponents [
                  "cargo"
                  "clippy"
                  "rust-src"
                  "rustc"
                ])
                fenix.packages.${system}.targets.${muslTriple}.stable.rust-std
              ];
              muslCraneLib = (crane.mkLib pkgs).overrideToolchain muslToolchain;
              muslArgs = {
                inherit version;
                strictDeps = true;
                buildInputs = [ ];
                nativeBuildInputs = [ ];
                CARGO_BUILD_TARGET = muslTriple;
                CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
                "CC_${muslTriple}" = muslCC;
                "AR_${muslTriple}" = muslAR;
                ${cargoLinkerEnv} = muslCC;
              };
              muslCargoArtifacts = muslCraneLib.buildDepsOnly (muslArgs // {
                pname = "testquorum-runner-musl-deps";
                version = "git";
                inherit src;
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
            inherit testquorum-api testquorum-config testquorum-runner testquorum-runner-static;
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
              # testquorum-runner drives the workspace tests in CI through its
              # cargo nextest backend, so the toolchain and nextest must be on
              # PATH for it to detect and run them.
              toolchain
              pkgs.cargo-nextest
            ];
          };

          formatter = treefmtEval.config.build.wrapper;

          checks = {
            inherit testquorum-api testquorum-config testquorum-runner;

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
          };
        }) // {
      ci = nixpkgs.lib.genAttrs systems (system:
        (self.packages.${system} or { })
        // (self.checks.${system} or { })
        // (self.devShells.${system} or { })
      );
    };
}
