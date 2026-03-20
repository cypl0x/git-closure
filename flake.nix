{
  nixConfig = {
    extra-substituters = ["https://git-closure.cachix.org"];
    extra-trusted-public-keys = [
      "git-closure.cachix.org-1:lAu8rMR1hyq+PISC2ABAs8ZA9+RzPM1T7xBQLabYQU4="
    ];
  };

  description = "git-closure - deterministic code snapshot CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
    treefmt-nix,
    git-hooks,
  }:
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "aarch64-linux"
      "i686-linux"
      "aarch64-darwin"
      "x86_64-darwin"
    ] (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable."1.85.0".default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        # Keep full repo source so trycmd fixture files (.gcl/.stdout/.stderr)
        # remain available during sandboxed cargo test checks.
        src = ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          pname = "git-closure";
          version = "0.1.0";
          nativeBuildInputs = [pkgs.git];
        };

        cargoArtifacts = craneLib.buildDepsOnly (commonArgs
          // {
            cargoExtraArgs = "--locked";
          });

        git-closure = craneLib.buildPackage (commonArgs
          // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--locked";
            doCheck = true;
          });

        texliveEnv = pkgs.texlive.combined.scheme-medium;
        fontsConf = pkgs.makeFontsConf {
          fontDirectories = with pkgs; [
            liberation_ttf
            dejavu_fonts
          ];
        };

        packageScript = pkgs.writeShellApplication {
          name = "git-closure-package";
          runtimeInputs = [rustToolchain];
          text = ''
            cargo package --locked "$@"
          '';
        };

        publishDryRunScript = pkgs.writeShellApplication {
          name = "git-closure-publish-dry-run";
          runtimeInputs = [rustToolchain];
          text = ''
            cargo publish --dry-run --locked "$@"
          '';
        };

        publishScript = pkgs.writeShellApplication {
          name = "git-closure-publish";
          runtimeInputs = [rustToolchain];
          text = ''
            cargo publish --locked "$@"
          '';
        };

        treefmtEval = treefmt-nix.lib.evalModule pkgs {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            prettier.enable = true;
            taplo.enable = true;
          };
        };

        preCommitCheck = git-hooks.lib.${system}.run {
          src = ./.;
          hooks = {
            rustfmt = {
              enable = true;
              packageOverrides = {
                cargo = rustToolchain;
                rustfmt = rustToolchain;
              };
            };
            alejandra.enable = true;
            prettier = {
              enable = true;
              excludes = [".*\\.md$"];
            };
            taplo.enable = true;

            deadnix.enable = true;
            statix.enable = true;
          };
        };
      in {
        packages = {
          default = git-closure;

          doc = pkgs.stdenvNoCC.mkDerivation {
            pname = "git-closure-doc";
            version = "0.1.0";
            src = ./.;

            nativeBuildInputs = [
              pkgs.pandoc
              texliveEnv
            ];

            buildPhase = ''
              export HOME=$TMPDIR
              export TEXMFHOME=$TMPDIR/texmf-home
              export TEXMFVAR=$TMPDIR/texmf-var
              export TEXMFCACHE=$TMPDIR/texmf-cache
              export FONTCONFIG_FILE=${fontsConf}

              sed 's/✓/$\checkmark$/g; s/✗/$\times$/g' README.md > gcl.md

              pandoc gcl.md \
                --pdf-engine=lualatex \
                --include-in-header=pdf-style.tex \
                --toc \
                --toc-depth=3 \
                --highlight-style=kate \
                -V documentclass=article \
                -V papersize=a4 \
                -V geometry=top=3cm \
                -V geometry=bottom=3cm \
                -V geometry=left=2.5cm \
                -V geometry=right=2.5cm \
                -V fontsize=11pt \
                -V colorlinks=true \
                -V linkcolor=linkblue \
                -V urlcolor=linkblue \
                -V toccolor=black \
                -V 'mainfont=Liberation Serif' \
                -V 'monofont=DejaVu Sans Mono' \
                -V 'monofontoptions=Scale=0.82' \
                -o git-closure.pdf
            '';

            installPhase = ''
              install -Dm644 git-closure.pdf \
                $out/share/doc/git-closure/git-closure.pdf
            '';
          };
        };

        apps = {
          default =
            (flake-utils.lib.mkApp {drv = git-closure;})
            // {
              meta.description = "Run git-closure CLI binary";
            };
          package = {
            type = "app";
            program = "${packageScript}/bin/git-closure-package";
            meta.description = "Run cargo package with --locked";
          };
          publish-dry-run = {
            type = "app";
            program = "${publishDryRunScript}/bin/git-closure-publish-dry-run";
            meta.description = "Run cargo publish --dry-run --locked";
          };
          publish = {
            type = "app";
            program = "${publishScript}/bin/git-closure-publish";
            meta.description = "Run cargo publish --locked";
          };
        };

        formatter = treefmtEval.config.build.wrapper;

        checks = {
          inherit git-closure;
          pre-commit-check = preCommitCheck;

          cargo-fmt = craneLib.cargoFmt {
            inherit src;
          };

          cargo-clippy = craneLib.cargoClippy (commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--locked";
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

          cargo-test = craneLib.cargoTest (commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--locked";
            });

          cargo-doc = craneLib.cargoDoc (commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--locked";
            });

          deadnix-check =
            pkgs.runCommand "deadnix-check" {
              nativeBuildInputs = [pkgs.deadnix];
            } ''
              deadnix --fail ${self}
              touch $out
            '';

          statix-check =
            pkgs.runCommand "statix-check" {
              nativeBuildInputs = [pkgs.statix];
            } ''
              statix check ${self}
              touch $out
            '';
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            rustToolchain
            cargo-watch
            cargo-edit
            cargo-audit
            cargo-outdated

            pandoc
            texliveEnv
            liberation_ttf
            dejavu_fonts

            treefmtEval.config.build.wrapper
            alejandra
            nodePackages.prettier
            taplo
            deadnix
            statix
          ];

          shellHook = ''
            build-docs() {
              mkdir -p build
              sed 's/✓/$\checkmark$/g; s/✗/$\times$/g' README.md > /tmp/gcl.md
              pandoc /tmp/gcl.md \
                --pdf-engine=lualatex \
                --include-in-header=pdf-style.tex \
                --toc \
                --toc-depth=3 \
                --highlight-style=kate \
                -V documentclass=article \
                -V papersize=a4 \
                -V geometry=top=3cm \
                -V geometry=bottom=3cm \
                -V geometry=left=2.5cm \
                -V geometry=right=2.5cm \
                -V fontsize=11pt \
                -V colorlinks=true \
                -V linkcolor=linkblue \
                -V urlcolor=linkblue \
                -V toccolor=black \
                -V 'mainfont=Liberation Serif' \
                -V 'monofont=DejaVu Sans Mono' \
                -V 'monofontoptions=Scale=0.82' \
                -o build/git-closure.pdf \
                && echo "-> build/git-closure.pdf"
            }

            echo "git-closure dev shell | run 'build-docs' for docs"
            echo "nix fmt and nix flake check are enabled"
            echo "publish helpers: nix run .#package | nix run .#publish-dry-run | nix run .#publish"
          '';
        };
      }
    );
}
