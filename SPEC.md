# git-closure Snapshot Format Specification

**Version:** 0.1
**Status:** Draft
**Canonical implementation:** `src/snapshot/serial.rs`

---

## 1. Overview

A `.gcl` file is a deterministic, content-addressed record of a source tree.
It consists of:

1. A `;;`-comment **header block** carrying structural metadata.
2. A blank line separating the header from the body.
3. A single **S-expression body** — a list of file entries.

The format is designed to be:

- **Deterministic:** Given the same source tree, the same `.gcl` file is
  produced every time (byte-identical output).
- **Content-addressed:** The `snapshot-hash` structurally commits to all file
  paths, modes, and SHA-256 digests, making any undetected tampering
  cryptographically infeasible.
- **Forward-compatible:** Unknown header comments and unknown plist keys in
  file entries are silently ignored by conformant readers.
- **Human-readable:** The file is valid UTF-8 text inspectable with any editor
  or `grep`.

---

## 2. Encoding

A `.gcl` file MUST be encoded as UTF-8 with LF (`\n`, U+000A) line endings.
CRLF line endings are not permitted.

---

## 3. Header Block

The header block consists of one or more lines beginning with `;;`.  Each
header line has the form:

```
;; <key>: <value>
```

The `<key>` is a lower-case ASCII identifier with optional hyphens.  The
`<value>` is the rest of the line after the space following the colon.
Leading and trailing whitespace in `<value>` is trimmed before use.

### 3.1 Required header fields

| Key | Format | Description |
|---|---|---|
| `snapshot-hash` | 64 hex digits (SHA-256) | Structural hash over all file entries.  See §6. |
| `file-count` | Non-negative integer | Number of file entries in the body. |

A conformant reader MUST reject a file that is missing either required field.

### 3.2 Optional header fields

| Key | Format | Description |
|---|---|---|
| `git-rev` | Git object name (typically 40 hex chars) | HEAD revision of the source repository at build time.  Informational only — not included in `snapshot-hash`. |
| `git-branch` | Short ref name | Current branch of the source repository at build time.  Informational only — not included in `snapshot-hash`. |

### 3.3 Legacy fields (rejected)

| Key | Action |
|---|---|
| `format-hash` | A conformant reader MUST return an error (`LegacyHeader`). |

### 3.4 Forward compatibility

A conformant reader MUST silently ignore any header comment whose key is not
listed in §3.1, §3.2, or §3.3.  This allows future versions to add new
informational fields without breaking existing tools.

### 3.5 Version comment

The first line of the header SHOULD be:

```
;; git-closure snapshot v0.1
```

Readers MUST NOT reject a file solely because this comment is absent or
names a different version string.

---

## 4. Separator

A single blank line MUST appear between the last header comment and the
S-expression body.

---

## 5. Body: S-expression list

The body is a single S-expression.  It MUST be parseable by a standard
Scheme/Lisp lexer using the following subset of notation:

- **Lists:** `(elem ...)`
- **Strings:** `"..."` with standard escape sequences (`\\`, `\"`, `\n`,
  `\r`, `\t`, `\uXXXX`)
- **Integers:** decimal non-negative integers (e.g. `1234`)
- **Keywords:** symbols prefixed with `:` (e.g. `:path`, `:sha256`)

The root value MUST be a list.  Each element of the root list is a
**file entry** (§5.1).

File entries MUST appear in lexicographic ascending order of their `:path`
values (UTF-8 byte ordering of the slash-delimited path string).

### 5.1 File entry structure

A file entry is a list of the form:

```
(
  (:path <path>
   [:<key> <value>]
   ...)
  <content>
)
```

The plist (property list) is the first element and the raw content is the
second element.

#### 5.1.1 Regular file entry

```scheme
(
  (:path    "src/main.rs"
   :sha256  "e3b0c44298fc1c149afb..."
   :mode    "644"
   :size    42
   [:encoding "base64"])
  "fn main() {}\n"
)
```

| Plist key | Type | Required | Description |
|---|---|---|---|
| `:path` | String | Yes | Slash-delimited path relative to the snapshot root.  No leading slash, no `..` components. |
| `:sha256` | String (64 hex) | Yes | SHA-256 of the file content bytes. |
| `:mode` | String (octal) | Yes | Unix permission bits as an octal string (e.g. `"644"`, `"755"`). |
| `:size` | Integer | Yes | Byte length of the file content. |
| `:encoding` | String | No | `"base64"` when the content is base64-encoded.  Absent for UTF-8 files. |

The **content** element (second element of the outer list) is:
- A quoted string containing the UTF-8 file content when `:encoding` is absent.
- A quoted string containing the standard Base64 encoding of the raw bytes
  when `:encoding` is `"base64"`.

#### 5.1.2 Symbolic link entry

```scheme
(
  (:path   "link"
   :type   "symlink"
   :target "target.txt")
  ""
)
```

| Plist key | Type | Required | Description |
|---|---|---|---|
| `:path` | String | Yes | Slash-delimited path of the symlink itself. |
| `:type` | String | Yes | Must be `"symlink"`. |
| `:target` | String | Yes | Symlink target as stored in the filesystem.  May be absolute or relative; the reader MUST NOT resolve it at parse time. |

The **content** element MUST be the empty string `""`.

#### 5.1.3 Forward compatibility

A conformant reader MUST silently skip any plist key it does not recognise.
Each unknown plist key consumes two elements of the plist list (the keyword
atom and its value).

---

## 6. Snapshot hash algorithm

The `snapshot-hash` is the hex-encoded SHA-256 of the concatenation of
length-prefixed fields for each file entry, in the same lexicographic path
order as the body.

For each file entry, the following fields are hashed in order:

1. Entry type — `"regular"` or `"symlink"` (UTF-8, length-prefixed)
2. `:path` value (UTF-8, length-prefixed)

For regular entries:

3. `:mode` value (UTF-8, length-prefixed)
4. `:sha256` value (UTF-8, length-prefixed)

For symlink entries:

3. `:target` value (UTF-8, length-prefixed)

**Length prefix:** a 64-bit big-endian integer giving the byte length of
the following UTF-8 byte sequence.

**Excluded fields:** `:size`, `:encoding`, content bytes, and all header
comments (including `git-rev` and `git-branch`) are intentionally excluded
from the hash.  This allows informational fields to be updated without
invalidating the structural hash.

### Reference implementation

```rust
// src/snapshot/hash.rs
fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

pub(crate) fn compute_snapshot_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        if let Some(target) = &file.symlink_target {
            hash_length_prefixed(&mut hasher, b"symlink");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, target.as_bytes());
        } else {
            hash_length_prefixed(&mut hasher, b"regular");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, file.mode.as_bytes());
            hash_length_prefixed(&mut hasher, file.sha256.as_bytes());
        }
    }
    hex::encode(hasher.finalize())  // (actual impl uses sha2::Digest::finalize)
}
```

---

## 7. Path safety constraints

All paths in a `.gcl` file MUST satisfy the following constraints:

- Non-empty.
- Slash-delimited (`/`); the path separator is always `/` regardless of host OS.
- No leading `/` (relative paths only).
- No `.` or `..` components.
- No null bytes.
- Valid UTF-8.

A conformant reader MUST reject any entry whose path violates these
constraints (see `UnsafePath` error).

---

## 8. Canonicalization

A snapshot is in **canonical form** when:

1. The header contains exactly the required fields plus any optional fields
   captured at build time, in the order: version comment, `snapshot-hash`,
   `file-count`, then optional fields.
2. File entries are in strict lexicographic ascending path order.
3. The `snapshot-hash` matches the recomputed hash of the file list.
4. The `file-count` matches the number of entries.

The `git-closure fmt` subcommand produces canonical form.  The
`git-closure fmt --check` subcommand exits non-zero if the file is not in
canonical form.

---

## 9. Error conditions

| Error | Trigger condition |
|---|---|
| `MissingHeader` | Required header field (`snapshot-hash` or `file-count`) absent. |
| `LegacyHeader` | Header contains `format-hash` key. |
| `HashMismatch` | Recomputed `snapshot-hash` does not match the stored value. |
| `ContentHashMismatch` | SHA-256 of a file's content does not match its `:sha256` field. |
| `SizeMismatch` | Byte length of a file's content does not match its `:size` field. |
| `UnsafePath` | A path violates the constraints in §7. |
| `Parse` | General structural parse error (malformed S-expression, bad file-count, etc.). |

---

## 10. MIME type and file extension

The conventional file extension is `.gcl`.  No IANA MIME type has been
registered; use `text/plain; charset=utf-8` for HTTP transport.

---

## 11. Example

```
;; git-closure snapshot v0.1
;; snapshot-hash: a1b2c3d4e5f6...
;; file-count: 3
;; git-rev: deadbeef1234567890abcdef1234567890abcdef
;; git-branch: main

(
  (
    (:path "Cargo.toml"
     :sha256 "e3b0c44298fc1c149afb4c8996fb924..."
     :mode "644"
     :size 512)
    "[package]\nname = \"my-crate\"\n..."
  )
  (
    (:path "src/lib.rs"
     :sha256 "ba7816bf8f01cfea414140de5dae2ec7..."
     :mode "644"
     :size 42)
    "pub fn hello() -> &'static str { \"world\" }\n"
  )
  (
    (:path "src/symlink"
     :type "symlink"
     :target "../README.md")
    ""
  )
)
```
