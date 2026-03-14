# git-closure — Deterministic Source Code Snapshots

> **Status: Research & Design** — This document is a living exploration of the problem space, not a committed specification. The goal is to surface tradeoffs with sufficient rigour to drive an informed implementation decision.

---

## The Problem

You need to capture the exact state of a codebase at a point in time. For an audit. For reproducibility. To archive research materials. To hand off to a colleague who lacks repository access. To attach to a bug report with a hard timestamp. To drop into an email thread as a single self-contained artefact. To review a branch on a device that cannot conveniently access the repository itself.

Git does this — but the format is opaque, binary, and requires the complete toolchain to inspect. A git bundle is not something you open in your iPhone mail client. `git archive --remote` is [disabled by GitHub and most public hosting providers](https://stackoverflow.com/questions/11258599/git-archive-fatal-operation-not-supported-by-protocol).

A trivial workaround is to concatenate repository files into one big text file. That helps immediately, but raises deeper questions: How are file boundaries represented? How are paths preserved? How can integrity be verified? How can two implementations produce the exact same result? Can the result later be turned back into a real directory tree?

That is where "repo to text" becomes a format design problem.

---

## Core Thesis

`git-closure` is a **deterministic, human-readable, verifiable source snapshot format** that also happens to work very well for AI ingestion, offline review, archival, and selected Git-adjacent workflows.

### What it is

- a deterministic source tree snapshot format
- a human-readable transport artefact
- a verifiable representation of a repository state
- a format especially convenient for AI ingestion and code review
- a potential foundation for new Git-adjacent workflows

### What it is not

- a replacement for Git or Git history
- a patch / merge / branch management system
- a build system or execution environment
- a universal binary archive format

This distinction matters. The project becomes significantly stronger when scoped as a **state representation format** rather than a version-control competitor.

---

## Core Requirements

The following properties are non-negotiable regardless of which design direction is ultimately chosen.

| Property | Description | Key Risk |
|---|---|---|
| **Deterministic** | Identical source input always produces identical output, byte-for-byte | Small ambiguities in whitespace, sorting, or escaping can destroy determinism |
| **Verifiable** | Per-file SHA-256 hashes; `sha256sum` on the original file must match without tooling | Must distinguish carefully between hash layers |
| **Self-describing** | File paths and metadata are embedded; no external manifest required | Some metadata may be volatile or host-specific |
| **Portable** | Opens in any text editor; survives email transit, HTTP upload, and archival decades | Binary-heavy repositories become awkward |
| **Round-trippable** | `build` followed by `materialize` reconstructs the original file tree exactly | Requires careful handling of modes, symlinks, and binary files |
| **Composable** | The output can be piped, grepped, diffed, and versioned with git itself | The more structured the format, the less pleasant for raw text tooling |

---

## What It Produces

Given any local directory or remote repository, `git-closure` produces a `.gcl` file (text/plain, UTF-8). Using the S-expression format as a concrete example:

```
;; git-closure snapshot v1.0
;; generated:   2026-03-14T14:32:00Z
;; source:      /home/wap/dotfiles
;; commit:      9dcb002a3f7e2d1c8e5f6a9b0d1e2f3c4a5b6d7
;; file-count:  247
;; format-hash: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

(
  (:file        "hosts/thinkpad/default.nix"
   :sha256      "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2"
   :size        5678
   :modified    "2024-03-10T16:20:00Z"
   :permissions "-rw-r--r--")

  "{ config, pkgs, ... }:\n{\n  imports = [ ./hardware.nix ];\n  ...\n}"

  (:file        "flake.nix"
   :sha256      "c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3"
   :size        1200
   :modified    "2024-03-14T09:00:00Z"
   :permissions "-rw-r--r--")

  "{ description = \"...\"; ... }"
)
```

Each entry contains a metadata plist followed by the raw file content. The SHA-256 is computed on the **raw file bytes only** — not the metadata wrapper — so it can be verified independently with `sha256sum`.

The `format-hash` header field is the SHA-256 of all file contents concatenated in lexicographic path order. This provides a single fingerprint for the entire snapshot:

```bash
find . -type f | sort | xargs cat | sha256sum
```

---

## The Central Design Decision: Output Format

The output format determines the tooling required, the ecosystem alignment, the export story, and the long-term viability of the project. Four candidates are evaluated below.

### Option A: S-Expressions (Custom `.gcl`)

A Lisp-inspired format where each file is a property list followed by its content as a string (shown above).

**Upsides**
- Clean, minimal, unambiguous — the format *is* the spec; no external reference needed
- Trivially parseable in any language (`lexpr` in Rust, `read` in any Lisp)
- Emacs can load the entire snapshot as a native data structure
- No dependency on external tooling for generation or parsing
- Extensible: unknown plist keys are silently ignored by any conformant reader

**Downsides**
- Zero existing ecosystem — no viewers, exporters, or third-party tooling
- Requires implementing a custom deterministic serializer
- "Weird Lisp format" is a genuine adoption barrier

### Option B: Emacs Org-Mode (`.org`)

Each captured file becomes a source block with metadata in an org property drawer. The `:tangle` header argument maps directly to the output path, making round-trip reconstruction a first-class feature.

**Upsides**
- *Literate programming native* — `org-babel-tangle` reconstructs the file tree; `C-c C-c` evaluates any source block in-place
- *Export pipeline already exists* — `ox-html`, `ox-latex`, `ox-md`, `ox-pandoc`, DOCX, PDF — all without writing a single exporter
- The `:tangle` property provides a direct, unambiguous mapping to output paths

**Downsides**
- *No hard specification* — the Emacs implementation is effectively the canonical parser
- *Emacs as a runtime dependency* — either ship Emacs or require it in `PATH`
- Parsing org reliably outside Emacs is notoriously difficult
- VS Code org support is incomplete — no tangle, no property drawer evaluation

### Option C: Markdown (`.md`)

Fenced code blocks with YAML frontmatter and per-file headings.

**Upsides**
- Renders natively on GitHub, GitLab, Obsidian, every static site generator
- Pandoc converts to PDF, DOCX, EPUB, LaTeX without any custom code
- Maximum audience — every developer reads Markdown; no tooling prerequisite

**Downsides**
- *No tangle equivalent* — no standard mechanism to reconstruct a file tree from fenced code blocks
- *No structured per-block metadata* — HTML comments are a workaround, not a standard
- *Spec fragmentation* — CommonMark, GFM, Pandoc Markdown, and MDX all diverge

### Option D: JSON

Structured JSON representation for files, metadata, and content.

**Upsides**
- Universally parseable; formal enough for machine use
- Familiar to modern developers

**Downsides**
- Large embedded source content becomes ugly; escaping harms readability
- Poor fit for "open in any editor and read comfortably for review"

### Comparison Matrix

| Concern | S-Expr `.gcl` | Org-Mode `.org` | Markdown `.md` | JSON |
|---|:---:|:---:|:---:|:---:|
| Human-readable without tooling | ✓ | ✓ | ✓ | ✗ |
| Hard, unambiguous specification | ✓ (self-spec) | ✗ | ✗ (fragmented) | ✓ |
| Literate programming / tangle | ✗ | ✓ | ✗ | ✗ |
| Export to PDF / HTML / DOCX | requires work | ✓ (`ox-*`) | ✓ (pandoc) | requires work |
| Emacs as hard dependency | optional | required | none | none |
| Parseable outside Emacs | ✓ | difficult | ✓ | ✓ |
| Existing ecosystem | none | Emacs-only | ubiquitous | ubiquitous |
| Adoption barrier | high | high | low | medium |
| Round-trip fidelity | ✓ | ✓ (`:tangle`) | requires work | ✓ |

### Working Recommendation

A promising architecture is a **custom canonical core format** (S-expressions or a strict record format) for correctness and longevity, plus **derived projections** into Markdown, Org, HTML, JSON, and PDF. This avoids forcing one format to simultaneously be canonical truth, review surface, and publishing format.

---

## Proposed CLI

The CLI surface is intended to remain stable regardless of which output format wins the design discussion. Format is a flag, not a subcommand.

### Core Operations

```bash
# Snapshot a local directory
git-closure build ~/dotfiles -o dotfiles.gcl

# Snapshot a remote repository
git-closure build gh:owner/repo -o dotfiles.gcl
git-closure build gh:owner/repo@main
git-closure build gl:owner/repo
git-closure build https://github.com/owner/repo/archive/main.tar.gz

# Reconstruct a file tree from a snapshot
git-closure materialize dotfiles.gcl -o ~/restored-dotfiles

# Verify all per-file hashes in a snapshot
git-closure verify dotfiles.gcl

# Diff two snapshots
git-closure diff before.gcl after.gcl

# List files recorded in a snapshot
git-closure list dotfiles.gcl

# Query by glob pattern (analogous to jq for .gcl files)
git-closure query dotfiles.gcl '**/*.nix'

# Watch a directory and rebuild on changes
git-closure watch ~/dotfiles
```

### Render / Export

```bash
git-closure render dotfiles.gcl --format pdf      -o dotfiles.pdf
git-closure render dotfiles.gcl --format html     -o dotfiles.html
git-closure render dotfiles.gcl --format markdown -o dotfiles.md
git-closure render dotfiles.gcl --format org      -o dotfiles.org
git-closure render dotfiles.gcl --format docx     -o dotfiles.docx
git-closure render dotfiles.gcl --format json     -o dotfiles.json
git-closure render dotfiles.gcl --format tar      -o dotfiles.tar
```

### Filtering

```bash
git-closure build ~/dotfiles \
  --exclude '*.git*'         \
  --exclude 'result*'        \
  --include '*.nix'          \
  --max-file-size 1MB        \
  --modified-after 2024-01-01 \
  --tag "research-2026-Q1"
```

### Provider Management

```bash
git-closure provider add github --token $GITHUB_TOKEN
git-closure provider add gitlab --token $GITLAB_TOKEN
git-closure provider add codeberg
```

### Git Integration (Future)

```bash
git gcl <commit>
git gcl --branch feature/x
git diff <commit> <snapshot.gcl>
git verify-snapshot <snapshot.gcl>
```

---

## The `materialize` Verb

The operation previously named `explode` is better designated **materialize** — to bring an abstract, encoded description into concrete physical form. This is the term used in database theory for [materialized views](https://en.wikipedia.org/wiki/Materialized_view), and in formal methods for realising a specification into an implementation.

| Verb | Reason rejected |
|---|---|
| `explode` | Vivid but informal; connotes destruction rather than construction |
| `extract` | Implies compression (tar/zip semantics); not quite right |
| `realize` | Viable; slightly more common in type theory than systems tooling |
| `instantiate` | Accurate but verbose; suggests template instantiation |
| `emit` | Compiler-centric; implies generation from source, not snapshot |
| `render` | Overlaps with the format-conversion subcommand |

---

## Hash Strategy

A key design constraint: per-file hashes must be independently verifiable without `git-closure` itself.

### Hash Layers

**Layer 1 — File Content Hash**
SHA-256 of the raw bytes of an individual file. Simple, stable, and independently verifiable with `sha256sum`. Says nothing about path or tree structure.

**Layer 2 — Snapshot Hash**
SHA-256 over the canonical representation of the entire represented state, including path, type, mode, content identity, and deterministic ordering. This is `git-closure`'s primary integrity identity.

**Layer 3 — Git Tree Identity** *(optional)*
The exact Git tree object identity for the represented state. Enables interoperability with native Git semantics. Adds complexity; not every user needs it.

**Layer 4 — Git Commit Identity** *(optional)*
To reproduce a Git commit hash, the artefact must carry parent commit hashes, author/committer info, timestamps, and commit message. Moves the project closer to commit-object modelling; blurs the line between state snapshot and history object.

### Recommended Approach

Do not collapse everything into one magical "ultimate hash". Treat each layer as a distinct, explicitly named identity. Volatile environment data (e.g. build host, runtime paths) may be useful as optional provenance but must not contaminate the canonical content identity.

### Why SHA-256?

Nix uses SHA-256 throughout the Nix store. Git is [transitioning to SHA-256](https://git-scm.com/docs/hash-function-transition) (available since Git 2.29). SHA-256 is collision-resistant, universally available, and already the default in the tools this project interoperates with.

---

## Design Considerations

### State vs History

`git-closure` is fundamentally **state-oriented**, not history-oriented. A snapshot represents:

- the set of files, their contents, their paths, and selected metadata
- optionally, a Git-derived identity for the corresponding tree or commit

A snapshot alone does not capture history. A Git commit hash depends on parent hashes, author/committer info, timestamps, and commit message — none of which are captured by file contents alone. A snapshot proves a **state**; proving a **history** requires additional commit metadata.

This is not a limitation but a deliberate scope constraint. Staying state-oriented keeps the mental model simpler, the implementation tractable, and the format well-suited for offline reading and AI upload.

### Data vs Code

The format must remain purely **declarative and inert**. As soon as a snapshot becomes executable, it is no longer an artefact, a document, or an audit object — it is a program. This changes the trust model significantly.

The clean design:

```bash
git-closure render snapshot.gcl --format markdown   # rendering is tooling, not format
git-closure render snapshot.gcl --format html
```

The `.gcl` file itself carries no executable semantics. Renderers, transformers, and exporters live in tooling around it.

### Provenance vs Core Identity

Two distinct layers:

1. **Content truth** — files, paths, content identity, modes, types. This is the core integrity identity.
2. **Build provenance** — filters, options, tool version, source URI. Useful for reproducibility documentation but must not affect content truth hashes.

### Longevity and Reimplementation

The ideal is that a Rust CLI, an Emacs Lisp implementation, a Python implementation, and a Go implementation — all given the same input and the same canonicalization rules — produce the same snapshot semantics. This requires:

- a strict canonicalization spec
- precise path ordering rules
- exact text/byte and escaping rules
- a hard separation between normative format behaviour and implementation convenience

### Nix Ecosystem Integration

Two Nix primitives are directly relevant to git-closure: `.drv` derivation files and `.nar` archives.

#### What `.drv` files actually are

A Nix derivation is a build recipe stored as a text file in the Nix store. Its actual on-disk format looks like this:

```
Derive(
  [("out","/nix/store/yyy-hello-2.12.1","","")],
  [("/nix/store/xxx-bash-5.2.drv",["out"]),
   ("/nix/store/abc-gcc-12.drv",["out"]),
   ("/nix/store/def-glibc-2.37.drv",["out"])],
  ["/nix/store/ghi-hello-2.12.1.tar.gz"],
  "x86_64-linux",
  "/nix/store/mno-bash-5.2/bin/bash",
  ["-c","tar xf $src && cd hello-* && ./configure --prefix=$out && make install"],
  [("name","hello-2.12.1"),
   ("out","/nix/store/yyy-hello-2.12.1"),
   ("src","/nix/store/ghi-hello-2.12.1.tar.gz"),
   ("system","x86_64-linux")]
)
```

Every reference is an absolute `/nix/store/hash-name` path. Nothing is ever inlined. The source tarball itself is a separate store path — a fixed-output derivation whose hash was fixed at fetch time. Without access to those store paths (or a binary cache that has them), the `.drv` is not actionable. A `.drv` is a recipe that assumes its entire dependency graph is already present elsewhere.

**Consequence for git-closure:** `.drv` files cannot replace or contain source snapshots. They are build instructions, not source representations. `materialize` must remain a first-class git-closure operation independent of Nix.

#### What NAR files actually are

NAR (Nix ARchive) is a **formally specified, deterministic, canonical serialization of a file tree**. `nix-store --dump` produces a NAR; `nix-store --restore` unpacks it. Nix computes the SHA-256 of a store path's NAR to derive its content address.

NAR is directly relevant to git-closure in two ways:

1. **Canonicalization reference** — NAR has already solved the hard problem of deterministic file tree serialization: lexicographic path ordering, precise handling of regular files, symlinks, and directories, no host-specific data, exact byte semantics. git-closure should study the [NAR format specification](https://edolstra.github.io/pubs/phd-thesis.pdf) closely when writing its own canonicalization rules rather than reinventing them from scratch.

2. **Export target** — `git-closure render snapshot.gcl --format nar` is a natural and useful operation for Nix users. A git-closure snapshot converted to NAR can be imported directly into the Nix store as a fixed-output source path, bridging the human-readable snapshot world with native Nix workflows.

NAR is binary, not human-readable — so it cannot serve as the `.gcl` core format. But it is the right model for the canonical layer that sits beneath the human-readable representation.

#### NAR as a universal format converter?

The Nix derivation system *can* produce Docker/OCI images (`pkgs.dockerTools.buildImage`), ISO images (`nixos-generators`), VM disk images, and many other output formats. But these are build outputs from Nix expressions evaluated by the Nix daemon — they require Nix to be installed, network access to dependency caches, and bespoke Nix code.

NAR → JSON / YAML / Markdown / Docker is not something standard Nix tooling provides. NAR is a file-tree format; structured data conversion is not in its scope. The Nix ecosystem's output variety comes from the derivation system, not from NAR. Treating NAR as a format conversion hub would require writing custom derivations for each target, eliminating the "shortcut" entirely.

**Practical conclusion:** NAR is valuable as a canonicalization reference and as one export target. It is not a universal bridge to the broader Nix ecosystem's output variety.

#### The Lightweight Pointer Variant (`.gcl.ref`)

A compelling design variant emerges from the `.drv` analogy: instead of embedding file content, store **content-addressed references** to files hosted on an existing provider.

GitHub raw URLs pinned to a specific commit SHA are already effectively content-addressed — Git commits are immutable, so the URL is deterministic and the content it points to will never change:

```
https://raw.githubusercontent.com/owner/repo/9dcb002.../hosts/thinkpad/default.nix
```

A lightweight reference file might look like:

```
;; git-closure reference snapshot v0.1
;; source:  gh:owner/dotfiles@9dcb002a3f7e2d1c8e5f6a9b0d1e2f3c4a5b6d7
;; format-hash: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

(
  (:file    "hosts/thinkpad/default.nix"
   :sha256  "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2"
   :size    5678
   :ref     "https://raw.githubusercontent.com/owner/dotfiles/9dcb002.../hosts/thinkpad/default.nix")

  (:file    "flake.nix"
   :sha256  "c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3"
   :size    1200
   :ref     "https://raw.githubusercontent.com/owner/dotfiles/9dcb002.../flake.nix")
)
```

The `:sha256` field makes each reference independently verifiable upon fetch: download the file, hash it, compare. The file is tiny — a few kilobytes for a repository with hundreds of files.

This is structurally identical to what `git-lfs` pointer files do: the pointer is stored in Git, the content is stored elsewhere and fetched on demand.

| Property | Full `.gcl` (content-embedded) | Reference `.gcl.ref` (pointer-based) |
|---|:---:|:---:|
| Self-contained | ✓ | ✗ |
| Archival without network | ✓ | ✗ |
| File size | large | tiny |
| Works offline | ✓ | ✗ |
| AI upload / email | ✓ | ✓ (with fetch step) |
| Verifiable | ✓ | ✓ (after fetch) |
| Requires provider access | ✗ | ✓ |

When IPFS content identifiers (CIDs) become practical as references, they eliminate the dependency on any specific provider entirely — the CID *is* the hash, and any IPFS node can serve the content. This is the ideal long-term target for the pointer format.

---

## Implementation Notes

### CLI Layer: Rust + clap

The CLI is almost certainly Rust with [clap](https://docs.rs/clap/). Rationale:

- Single static binary with no runtime dependencies
- Predictable, benchmarkable performance on large repository trees
- Strong ecosystem: [`walkdir`](https://crates.io/crates/walkdir), [`ignore`](https://crates.io/crates/ignore) (respects `.gitignore`), [`sha2`](https://crates.io/crates/sha2), [`anyhow`](https://crates.io/crates/anyhow)
- Rust's type system enforces determinism and exhaustive error handling

### Proposed Module Structure

```
src/
  main.rs              — clap CLI entry point
  commands/
    build.rs           — filesystem walk, hash computation, snapshot serialization
    materialize.rs     — snapshot deserialization, file tree reconstruction
    verify.rs          — hash verification against recorded values
    diff.rs            — structural diff between two snapshots
    query.rs           — glob-based file selection within a snapshot
    render.rs          — format conversion (delegates to pandoc or ox-*)
    watch.rs           — inotify/kqueue watcher, rebuild on change
  format/
    sexpr.rs           — S-expression serializer (deterministic, sorted keys)
    org.rs             — Org-mode serializer (or emacs --batch bridge)
    json.rs            — JSON serializer (for tooling integration)
  providers/
    local.rs           — local filesystem source
    github.rs          — GitHub via gh CLI or REST API
    gitlab.rs          — GitLab via REST API
    codeberg.rs        — Codeberg via Gitea API
  hash.rs              — SHA-256 primitives, format-hash computation
  metadata.rs          — git log extraction, filesystem stat
tests/
  integration/         — round-trip tests: build → materialize → verify
  fixtures/            — minimal test repositories
```

---

## Roadmap

1. **v0.1** — Core: `build` local directories, S-expression output, SHA-256 hashing
2. **v0.2** — `materialize` and `verify` — round-trip integrity
3. **v0.3** — GitHub/GitLab support via gh CLI and raw URLs
4. **v0.4** — `watch` mode with filesystem notifications
5. **v0.5** — `render` / export formats (tar, json, markdown, html)
6. **v1.0** — Stable format spec, comprehensive tests, normative canonicalization document

---

## Related Work

| Project | Relation to git-closure |
|---|---|
| [`git-archive`](https://git-scm.com/docs/git-archive) | Produces tarballs; binary format, not human-readable, `--remote` disabled on major hosts |
| [`git-bundle`](https://git-scm.com/docs/git-bundle) | Git-native bundles; binary, requires git toolchain to inspect |
| [`repo2txt`](https://github.com/abinthomasonline/repo2txt) | Concatenates repo to plain text for LLM prompts; validates the pain point, but no hashing or round-trip |
| [`repomix`](https://github.com/yamadashy/repomix) | Packs repo to XML/text for LLM context; no integrity guarantees |
| [`git2txt`](https://github.com/addyosmani/git2txt) | GitHub repo → single text file; no metadata, no verification |
| [Pandoc](https://pandoc.org/) | Universal document converter; natural `render` backend for this project |
| [Quarto](https://quarto.org/) | Literate programming over Markdown; complex framework, not a file format |
| [`org-babel-tangle`](https://orgmode.org/manual/Extracting-Source-Code.html) | Reconstructs files from org source blocks; Emacs-only, no hash support |

`repo2txt` is the closest prior art for the "repo to text for AI tools" use case. It validates the original pain point and provides a usability benchmark. `git-closure` only becomes interesting if it aims higher: deterministic canonicalization, explicit file-level and snapshot-level hashing, round-trippability, and long-term archival value.

---

## Open Questions

1. **Format decision** — S-expr, org-mode, or Markdown? Or format-agnostic with pluggable backends from the start?

2. **Emacs as dependency** — if org-mode is chosen: acceptable hard requirement, or must it be an optional enhancement?

3. **Spec stability risk** — org-mode's lack of a formal grammar: acceptable given the single canonical implementation, or a dealbreaker for long-term archival use?

4. **Binary files** — skip silently with a warning, encode as base64, or store hash-only with a sentinel?

5. **Git tree / commit compatibility** — how far should compatibility go? File hashes only, full tree identity, or full commit reconstruction?

6. **Metadata policy** — which metadata is genuinely useful and stable? Timestamps and author info can be volatile or host-specific.

7. **Editing model** — is an edited snapshot still a valid snapshot? How should dirty state be represented?

8. **Canonicalization spec** — exact rules for path normalization, sort order, line ending policy, string escaping, and symlink semantics must be written before any implementation can claim correctness.

9. **Literate programming scope** — is source block evaluation (à la `org-babel`) in scope for v1, or deferred?

10. **Pandoc delegation boundary** — what should be delegated to Pandoc or document-oriented exporters, and what must remain first-class in `git-closure` itself?

11. **Nix integration depth** — Three distinct levels of Nix integration are possible, each with different costs: (a) NAR as an export target only; (b) NAR canonicalization rules borrowed for the core spec; (c) a first-class reference variant (`.gcl.ref`) that uses content-addressed provider URLs and optionally IPFS CIDs. Which levels belong in v1, and which are post-v1?

12. **Lightweight pointer format** — The `.gcl.ref` reference variant (see Design Considerations above) is architecturally clean but not self-contained. Should it be a first-class format variant, a separate tool, or explicitly out of scope until IPFS references become practical?

13. **Data fluidity and export surface** — The hypothesis is that broader import/export support drives adoption. Candidate targets span a wide range: `fuse`, `nfs`, `webdav`, `git-lfs`, `git-annex`, `ipfs`, `oci`, `docker`, `kubernetes`, `s3`, `syncthing`, `iso`, `squashfs`. These divide into two fundamentally different categories: *document formats* (Markdown, Org, HTML, PDF, JSON) delegatable to Pandoc or Nix derivations, and *infrastructure formats* (OCI, ISO, FUSE, NFS) requiring bespoke integration work. Should these be treated as one roadmap or two separate surface areas?

---

## Suggested Next Step

Before implementation, the most valuable next artefact is a **normative v0.1 specification** defining:

- the exact minimum field set
- the exact canonical ordering and serialization rules
- what goes into each hash layer
- what is explicitly out of scope
- which parts are provenance only

That would turn the project from a compelling design space into an implementable protocol.

---

## Name

**git-closure** — "closure" in the mathematical sense: an operation whose result contains everything necessary to reproduce itself. A closed set under the relevant operations. No external references. The snapshot is complete, self-contained, and self-verifying.

| Alternative | Assessment |
|---|---|
| `repo-snapshot` | Descriptive but generic; no distinctiveness |
| `src-seal` | Suggests immutability; good, but slightly awkward |
| `codeseal` | Similar; domain-squatting risk |
| `gcl` | Too terse for a project name; works as file extension |
| `archivist` | Evocative but implies passive storage rather than active tooling |
| `sourcebox` | Informal; does not convey verifiability |

---

## License

MIT. Go forth and snapshot.
