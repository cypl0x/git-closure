{
  description = "git-closure — documentation build";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      # ── nix build .#doc ──────────────────────────────────────────────────────
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};

          # TeX Live — scheme-medium covers lualatex, fontspec, geometry,
          # xcolor, hyperref, fancyhdr, titlesec, framed, microtype,
          # enumitem, amssymb, booktabs, longtable, and setspace.
          texliveEnv = pkgs.texlive.combined.scheme-medium;

          # Fonts made available to fontconfig inside the Nix sandbox.
          fontsConf = pkgs.makeFontsConf {
            fontDirectories = with pkgs; [
              liberation_ttf   # Liberation Serif  — main text (Times-compatible)
              dejavu_fonts     # DejaVu Sans Mono  — monospace / code blocks
            ];
          };

          buildDoc = pkgs.stdenvNoCC.mkDerivation {
            pname    = "git-closure-doc";
            version  = "0.1.0";
            src      = ./.;

            nativeBuildInputs = with pkgs; [
              pandoc
              texliveEnv
            ];

            buildPhase = ''
              # ── Sandbox environment ─────────────────────────────────────────
              export HOME=$TMPDIR
              export TEXMFHOME=$TMPDIR/texmf-home
              export TEXMFVAR=$TMPDIR/texmf-var
              export TEXMFCACHE=$TMPDIR/texmf-cache
              export FONTCONFIG_FILE=${fontsConf}

              # ── Pre-process ─────────────────────────────────────────────────
              # Replace Unicode symbols that Liberation Serif lacks with
              # equivalent LaTeX math constructs.
              sed 's/✓/$\\checkmark$/g; s/✗/$\\times$/g' README.md > gcl.md

              # ── Build ───────────────────────────────────────────────────────
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

            meta = with nixpkgs.lib; {
              description = "git-closure design document (PDF)";
              license     = licenses.mit;
            };
          };
        in
        {
          doc     = buildDoc;
          default = buildDoc;
        }
      );

      # ── nix develop ─────────────────────────────────────────────────────────
      # Provides `build-doc` shell function for local iteration without
      # going through the full Nix sandbox build.
      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            name = "git-closure-dev";

            packages = with pkgs; [
              pandoc
              texlive.combined.scheme-medium
              liberation_ttf
              dejavu_fonts
            ];

            shellHook = ''
              build-doc() {
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
                  && echo "→ build/git-closure.pdf"
              }

              echo "git-closure dev shell  |  run 'build-doc' to rebuild the PDF"
            '';
          };
        }
      );
    };
}
