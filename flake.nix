{
  description = "Build a cargo project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    flake-utils.url = "github:numtide/flake-utils";

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      fenix,
      flake-utils,
      advisory-db,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        inherit (pkgs) lib;

        craneLib = crane.mkLib pkgs;
        src = craneLib.cleanCargoSource ./.;

        # Common arguments can be set here to avoid repeating them later
        commonArgs = {
          inherit src;
          strictDeps = true;

          buildInputs =
            [
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              # Additional darwin specific inputs can be set here
              pkgs.libiconv
            ];

          propagatedBuildInputs = with pkgs; [
            rsync
          ];

          # Additional environment variables can be set directly
          # MY_CUSTOM_VAR = "some value";
        };

        craneLibLLvmTools = craneLib.overrideToolchain (
          fenix.packages.${system}.complete.withComponents [
            "cargo"
            "llvm-tools"
            "rustc"
          ]
        );

        # Build *just* the cargo dependencies, so we can reuse
        # all of that work (e.g. via cachix) when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the dependency
        # artifacts from above.
        atune = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
          }
        );

        dockerImage = pkgs.dockerTools.buildImage {
          name = "atune";
          tag = "latest";
          copyToRoot = [
            atune
            pkgs.rsync
            pkgs.openssh
          ];
          config = {
            Env = [
              "ATUNE_CONFIG_PATH=/etc/atune/atune.yaml"
            ];
            Entrypoint = [
              "atune"
              "watch"
            ];
          };
        };
      in
      {
        checks = {
          # Build the crate as part of `nix flake check` for convenience
          inherit atune;

          # Run clippy (and deny all warnings) on the crate source,
          # again, reusing the dependency artifacts from above.
          #
          # Note that this is done as a separate derivation so that
          # we can block the CI if there are issues here, but not
          # prevent downstream consumers from building our crate by itself.
          atune-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets";
            }
          );

          atune-doc = craneLib.cargoDoc (
            commonArgs
            // {
              inherit cargoArtifacts;
            }
          );

          # Check formatting
          atune-fmt = craneLib.cargoFmt {
            inherit src;
          };

          # Audit dependencies
          atune-audit = craneLib.cargoAudit {
            inherit src advisory-db;
          };

          # Run tests with cargo-nextest
          # Consider setting `doCheck = false` on `atune` if you do not want
          # the tests to run twice
          atune-nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
        };

        packages =
          {
            inherit atune dockerImage;
            default = atune;
          }
          // lib.optionalAttrs (!pkgs.stdenv.isDarwin) {
            atune-llvm-coverage = craneLibLLvmTools.cargoLlvmCov (
              commonArgs
              // {
                inherit cargoArtifacts;
              }
            );
          };

        apps.default = flake-utils.lib.mkApp {
          drv = atune;
        };

        devShells.default = craneLib.devShell {
          # Inherit inputs from checks.
          checks = self.checks.${system};

          # Additional dev-shell environment variables can be set directly
          RUST_LOG = "info,atune=debug";
          RUST_BACKTRACE = "1";

          packages = with pkgs; [
            just
            git-cliff
            lldb
            rust-analyzer
          ];
        };
      }
    );
}
