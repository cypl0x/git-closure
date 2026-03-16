{
  description = "git-closure - deterministic code snapshot CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        rustToolchain = with pkgs; [
          cargo
          rustc
          clippy
          rust-analyzer
        ];

        texliveEnv = pkgs.texlive.combined.scheme-medium;

        fontsConf = pkgs.makeFontsConf {
          fontDirectories = with pkgs; [
            liberation_ttf
            dejavu_fonts
          ];
        };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "git-closure";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        packages.doc = pkgs.stdenvNoCC.mkDerivation {
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

            sed 's/✓/$\\checkmark$/g; s/✗/$\\times$/g' README.md > gcl.md

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

        devShells.default = pkgs.mkShell {
          name = "git-closure-dev";

          packages = rustToolchain ++ [
            pkgs.pandoc
            texliveEnv
            pkgs.liberation_ttf
            pkgs.dejavu_fonts
          ];

          shellHook = ''
            build-docs() {
              mkdir -p build
              sed 's/✓/$\\checkmark$/g; s/✗/$\\times$/g' README.md > /tmp/gcl.md
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
          '';
        };
      });
}
