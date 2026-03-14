# git-closure — Deterministic Source Code Snapshots

## The Problem

You've been there. You need to capture the exact state of a codebase at a point in time. Maybe for an audit. Maybe to reproduce a build. Maybe to archive research materials. Maybe to send to a colleague who doesn't have access to your private repositories.

Git does this, of course — but the format is opaque, requires the full repository, and you can't just drop it into an email thread or upload it to a service that expects plain text.

What if there was a format that was:
- **Human-readable** — open it in any editor, search with grep, version with git itself
- **Self-describing** — every file includes its path, hash, and metadata
- **Verifiable** — SHA-256 hashes on every file, deterministically computed
- **Deterministic** — same source produces identical output, byte-for-byte
- **Portable** — plain text that works everywhere, even in an email body or iPhone mail client

That's what `git-closure` creates.

## What It Produces

Given any local directory or remote repository, `git-closure` produces a `.gcl` file (text/plain, UTF-8) that contains:

```
;; git-closure snapshot
;; generated: 2024-03-15T14:32:00Z
;; source: /home/wap/dotfiles
;; commit: 9dcb002a3f7e2d1c8e5f6a9b0d1e2f3c4a5b6d7
;; tree-hash: 5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f

(
  (:file "README.md"
   :sha256 "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b"
   :size 1234
   :author "Wap <wap@example.com>"
   :created "2024-01-15T09:22:33Z"
   :modified "2024-03-14T18:45:00Z"
   :permissions "-rw-r--r--")

  "================================================================================
   README.md
   ================================================================================"
  "# Welcome to dotfiles\n\nThis is my..."

  (:file "hosts/thinkpad/default.nix"
   :sha256 "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2"
   :size 5678
   :author "Wap <wap@example.com>"
   :created "2023-06-01T12:00:00Z"
   :modified "2024-03-10T16:20:00Z"
   :permissions "-rw-r--r--")

  "================================================================================
   hosts/thinkpad/default.nix
   ================================================================================"
  "{ ... }:\n\n{\n  imports = [\n    ./hardware.nix\n    ./gnome.nix\n  ];\n\n  # ...\n"
)
```

Each S-expression contains metadata (not part of the hash) followed by the file contents. The hash is computed on the **essential content only** — the actual bytes of the file — not the metadata wrapper.

### The Magic: Deterministic Hashing

Every file's SHA-256 is computed on its content alone. This means:
- The same directory always produces the same `.gcl` file
- Two `.gcl` files can be compared to know exactly what changed
- You can verify integrity without parsing the whole file

### The Top-Level Header

```
;; git-closure snapshot v1.0
;; generated: 2024-03-15T14:32:00Z
;; hostname: thinkpad
;; username: wap
;; source: /home/wap/dotfiles
;; commit: 9dcb002a3f7e2d1c8e5f6a9b0d1e2f3c4a5b6d7
;; tree-hash: 5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f
;; file-count: 247
;; total-size: 1048576
;; format-hash: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

The `format-hash` is the SHA-256 of all file contents concatenated in sorted path order — the ultimate content fingerprint.

## Why S-Expressions?

Three reasons:

1. **Universal parsing** — Every language has an S-expression parser. Lisp, Python, JavaScript, Rust, Go. It's trivially machine-readable.

2. **Extensibility** — You can add new metadata fields without breaking parsers. Unknown fields are ignored.

3. **Emacs integration** — If you're an Emacs user (and if you're reading this, you probably are), the entire snapshot is a valid Lisp data structure. You can:
   - Load it with `(load-file "dotfiles.gcl")`
   - Query files with `(assoc :file *snapshot*)`
   - Write Elisp to transform, filter, or analyze the snapshot
   - Evaluate embedded code snippets (with `- unsafe`)

## Usage

```bash
# Snapshot a local directory
git-closure build ~/dotfiles -o dotfiles.gcl

# Snapshot a GitHub repository (uses gh CLI or raw URLs)
git-closure build https://github.com/cypl0x/dotfiles -o dotfiles.gcl

# Watch for changes and rebuild
git-closure watch ~/dotfiles

# Extract a snapshot to a directory
git-closure explode dotfiles.gcl -o ~/restored-dotfiles

# Verify integrity of a snapshot
git-closure verify dotfiles.gcl

# Show what changed between two snapshots
git-closure diff old.gcl new.gcl

# Query snapshots (like jq but for .gcl)
git-closure query dotfiles.gcl '(:file "**/default.nix")'

# Generate just the format-hash (for verification)
git-closure hash ~/dotfiles

# List all files in a snapshot
git-closure list dotfiles.gcl

# Export to other formats
git-closure export dotfiles.gcl --format tar --output dotfiles.tar
git-closure export dotfiles.gcl --format zip --output dotfiles.zip
git-closure export dotfiles.gcl --format json --output dotfiles.json
git-closure export dotfiles.gcl --format json --output dotfiles.jsonc
git-closure export dotfiles.gcl --format json --output dotfiles.toml
git-closure export dotfiles.gcl --format json --output dotfiles.yaml
git-closure export dotfiles.gcl --format nix-drv --output dotfiles.drv
git-closure export dotfiles.gcl --format nix-nar --output dotfiles.nar
git-closure export dotfiles.gcl --format web-server
git-closure export dotfiles.gcl --format ftp-server
git-closure export dotfiles.gcl --format sftp-server
git-closure export dotfiles.gcl --format smb-server
git-closure export dotfiles.gcl --format fuse-server
git-closure export dotfiles.gcl --format fuse-cryptomator-server
```

## Advanced Features

### Remote Sources

```bash
# GitHub (requires gh CLI or works without)
git-closure build gh:owner/repo
git-closure build gh:owner/repo@branch
git-closure build gh:owner/repo@tag

# GitLab
git-closure build gl:owner/repo

# Codeberg
git-closure build bb:owner/repo

# Raw URLs (fallback)
git-closure build https://github.com/owner/repo/archive/main.tar.gz
```

### Filtering

```bash
# Exclude patterns
git-closure build ~/dotfiles --exclude '*.git*' --exclude 'result*'

# Include only patterns
git-closure build ~/dotfiles --include '*.nix' --include '*.el'

# By file size
git-closure build ~/dotfiles --max-file-size 1MB

# By date
git-closure build ~/dotfiles --modified-after 2024-01-01
```

### Metadata Options

```bash
# Include git log for each file (expensive)
git-closure build ~/dotfiles --include-git-log

# Include commit history (very expensive)
git-closure build ~/dotfiles --include-history

# Exclude metadata entirely (minimal output)
git-closure build ~/dotfiles --minimal

# Add custom fields
git-closure build ~/dotfiles --tag "research-2024-Q1" --label "approved-by: alice"
```

### Emacs Integration

```elisp
;; Load a snapshot
(setq my-snapshot (car (read-from-whole-file "~/dotfiles.gcl")))

;; Find all Nix files
(loop for (key . val) in my-snapshot
      when (string-match-p "\\.nix$" (cdr (assq :file val)))
      collect val)

;; Extract all file contents
(loop for (key . val) in my-snapshot
      when (stringp key)
      collect (cdr val))

;; Evaluate embedded code (DANGER)
(eval (car (read-from-whole-file "dotfiles-with-code.gcl")))
```

### The "Closure" in git-closure

Because the format is deterministic and self-contained, you can:
- Email snapshots to colleagues
- Store them in any VCS (yes, even git — `.gcl` files version nicely)
- Use them as build artifacts
- Submit them as evidence in audits
- Archive them for legal compliance
- Share them on systems that don't support binary formats

The snapshot **closes** over the source — it's complete, self-contained, and verified.

## Implementation

Written in Rust using [clap](https://docs.rs/clap/) for CLI argument parsing. The S-expression output uses a custom serializer to ensure deterministic formatting (sorted keys, consistent whitespace).

### Why Rust?

- Predictable performance
- Single static binary, no runtime dependencies
- Excellent ecosystem for CLI tools
- clap provides the best DX for command-line interfaces

### Why Not [X]?

- **Shell scripts**: Fine for prototypes, painful for cross-platform and error handling
- **Python**: Requires interpreter on target system
- **Go**: Good, but Rust's type system catches more errors at compile time
- **Emacs Lisp**: Can't expect everyone to have Emacs

## Roadmap

1. **v0.1** — Core: `build` local directories, S-expression output, SHA-256 hashing
2. **v0.2** — `explode` and `verify` — round-trip integrity
3. **v0.3** — GitHub/GitLab support via gh CLI and raw URLs
4. **v0.4** — `watch` mode with file system notifications
5. **v0.5** — Export formats (tar, zip, json)
6. **v1.0** — Stable format, comprehensive tests

## Related Work

- **git-archive**: Produces tarballs, not human-readable
- **git-bundle**: Git-only, binary format
- **github/gh-issue-loader**: Converts issues to markdown
- **NotebookLM/converter**: Converts to audio, not what we need
- **tarpipe**: Similar idea, different implementation

## Name

"Closure" in the mathematical sense: complete, self-contained, no external references. Just like your source code should be when you archive it.

## License

MIT. Go forth and snapshot.
