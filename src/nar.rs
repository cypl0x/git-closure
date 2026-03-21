//! NAR (Nix ARchive) writer for git-closure.
//!
//! Implements the NAR wire format as described in the Nix PhD thesis (Eelco
//! Dolstra, 2006) and verified against the tvix/nix-compat reference
//! implementation.
//!
//! # Format summary
//!
//! NAR is a deterministic, stream-oriented archive format used by the Nix
//! package manager to represent filesystem trees.  Every value in the format
//! is a length-prefixed byte string:
//!
//! ```text
//! [u64 LE length] [bytes] [0..7 zero padding to 8-byte alignment]
//! ```
//!
//! Pre-computed token byte sequences (verified against tvix wire constants)
//! are used for structural markers to avoid repeated string encoding overhead.
//!
//! # Limitations
//!
//! - This module implements **writing only**; there is no NAR reader.
//! - Only two permission modes are representable: executable and
//!   non-executable.  Arbitrary Unix mode bits (setuid, sticky, etc.) are not
//!   preserved.
//! - NAR does not store any git-closure snapshot metadata (snapshot-hash,
//!   git-rev, git-branch, source-uri).  Use the `.gcl` format for provenance
//!   tracking.
//! - This implementation does not compute Nix store hashes; the output is a
//!   valid NAR byte stream but does not correspond to any Nix store path.

use std::collections::BTreeMap;
use std::io::{self, Write};

// ── Wire token constants ──────────────────────────────────────────────────────
//
// Pre-computed byte sequences for the NAR structural keywords, verified
// against the tvix nix-compat implementation (nix-compat/src/nar/wire/mod.rs).
//
// Each constant concatenates the length-prefixed encodings of all keyword
// strings that form a logical token in the NAR grammar.  Using pre-computed
// constants avoids repeated allocation and keeps the writer straightforward.
//
// Wire encoding for each string field:
//   [u64 LE length][bytes][0..7 zero padding to 8-byte alignment]

/// Archive header: encodes "nix-archive-1", "(", "type" (56 bytes)
pub(crate) const TOK_NAR: [u8; 56] =
    *b"\x0d\0\0\0\0\0\0\0nix-archive-1\0\0\0\x01\0\0\0\0\0\0\0(\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0type\0\0\0\0";

/// Symlink node prefix: encodes "symlink", "target" (32 bytes)
const TOK_SYM: [u8; 32] = *b"\x07\0\0\0\0\0\0\0symlink\0\x06\0\0\0\0\0\0\0target\0\0";

/// Non-executable regular file prefix: encodes "regular", "contents" (32 bytes)
const TOK_REG: [u8; 32] = *b"\x07\0\0\0\0\0\0\0regular\0\x08\0\0\0\0\0\0\0contents";

/// Executable regular file prefix: encodes "regular", "executable", "", "contents" (64 bytes)
const TOK_EXE: [u8; 64] =
    *b"\x07\0\0\0\0\0\0\0regular\0\x0a\0\0\0\0\0\0\0executable\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0contents";

/// Directory type marker: encodes "directory" (24 bytes)
const TOK_DIR: [u8; 24] = *b"\x09\0\0\0\0\0\0\0directory\0\0\0\0\0\0\0";

/// Directory entry prefix: encodes "entry", "(", "name" (48 bytes)
const TOK_ENT: [u8; 48] =
    *b"\x05\0\0\0\0\0\0\0entry\0\0\0\x01\0\0\0\0\0\0\0(\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0name\0\0\0\0";

/// Node prefix within a directory entry: encodes "node", "(", "type" (48 bytes)
const TOK_NOD: [u8; 48] =
    *b"\x04\0\0\0\0\0\0\0node\0\0\0\0\x01\0\0\0\0\0\0\0(\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0type\0\0\0\0";

/// Closing parenthesis: encodes ")" (16 bytes)
const TOK_PAR: [u8; 16] = *b"\x01\0\0\0\0\0\0\0)\0\0\0\0\0\0\0";

// ── Public API ────────────────────────────────────────────────────────────────

/// A node in a NAR archive tree.
///
/// Produced by [`crate::gcl::export::build_nar_tree`] from a parsed
/// `.gcl` snapshot and passed to [`write_nar`] for serialization.
#[derive(Debug, Clone)]
pub enum NarNode {
    /// A regular file.  The `executable` flag selects `TOK_EXE` vs `TOK_REG`.
    File { executable: bool, content: Vec<u8> },
    /// A symbolic link.  Only the target string is stored; no mode bits.
    Symlink { target: String },
    /// A directory.  [`BTreeMap`] guarantees strictly ascending lexicographic
    /// iteration order, satisfying the NAR wire format requirement.
    Directory(BTreeMap<String, NarNode>),
}

/// Serialize a [`NarNode`] tree as a complete NAR archive into `writer`.
///
/// Output is deterministic: the same `root` always produces identical bytes.
/// No external tools are invoked.
///
/// # Example
///
/// ```rust
/// use git_closure::nar::{NarNode, write_nar};
/// let node = NarNode::File { executable: false, content: b"hello\n".to_vec() };
/// let mut buf = Vec::new();
/// write_nar(&mut buf, &node).expect("write_nar");
/// assert!(buf.starts_with(b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1"));
/// ```
pub fn write_nar<W: Write>(writer: &mut W, root: &NarNode) -> io::Result<()> {
    writer.write_all(&TOK_NAR)?;
    write_node(writer, root)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Write a single NAR node without the archive-level `TOK_NAR` prefix.
fn write_node<W: Write>(writer: &mut W, node: &NarNode) -> io::Result<()> {
    match node {
        NarNode::Symlink { target } => {
            writer.write_all(&TOK_SYM)?;
            write_str(writer, target.as_bytes())?;
            writer.write_all(&TOK_PAR)?;
        }
        NarNode::File {
            executable,
            content,
        } => {
            writer.write_all(if *executable { &TOK_EXE } else { &TOK_REG })?;
            write_str(writer, content)?;
            writer.write_all(&TOK_PAR)?;
        }
        NarNode::Directory(entries) => {
            writer.write_all(&TOK_DIR)?;
            // BTreeMap iteration is in ascending key order — satisfies NAR's
            // strictly ascending lexicographic entry ordering requirement.
            for (name, child) in entries {
                writer.write_all(&TOK_ENT)?;
                write_str(writer, name.as_bytes())?;
                writer.write_all(&TOK_NOD)?;
                write_node(writer, child)?;
                writer.write_all(&TOK_PAR)?; // closes the entry "("
            }
            writer.write_all(&TOK_PAR)?; // closes the directory "("
        }
    }
    Ok(())
}

/// Write a length-prefixed byte string with 8-byte-aligned zero padding.
///
/// Wire encoding: `[u64 LE length][bytes][0..7 zero padding]`
pub(crate) fn write_str<W: Write>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
    writer.write_all(bytes)?;
    let pad = pad_len(bytes.len());
    if pad > 0 {
        writer.write_all(&[0u8; 7][..pad])?;
    }
    Ok(())
}

/// Number of zero padding bytes to reach 8-byte alignment.
fn pad_len(n: usize) -> usize {
    match n % 8 {
        0 => 0,
        r => 8 - r,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn nar_bytes(node: &NarNode) -> Vec<u8> {
        let mut buf = Vec::new();
        write_nar(&mut buf, node).expect("write_nar must not fail in tests");
        buf
    }

    // ── write_str encoding ────────────────────────────────────────────────────

    #[test]
    fn write_str_empty_is_eight_zero_bytes() {
        let mut buf = Vec::new();
        write_str(&mut buf, b"").unwrap();
        assert_eq!(
            buf,
            vec![0u8; 8],
            "empty string must encode as 8 zero bytes"
        );
    }

    #[test]
    fn write_str_five_byte_string_has_three_padding_bytes() {
        let mut buf = Vec::new();
        write_str(&mut buf, b"hello").unwrap();
        assert_eq!(
            buf, b"\x05\x00\x00\x00\x00\x00\x00\x00hello\x00\x00\x00",
            "5-byte string: 8-byte length + 5 content + 3 padding"
        );
    }

    #[test]
    fn write_str_eight_byte_string_has_no_padding() {
        let mut buf = Vec::new();
        write_str(&mut buf, b"contents").unwrap();
        assert_eq!(
            buf, b"\x08\x00\x00\x00\x00\x00\x00\x00contents",
            "8-byte string: 8-byte length + 8 content + 0 padding"
        );
    }

    #[test]
    fn pad_len_correct_for_key_values() {
        assert_eq!(pad_len(0), 0);
        assert_eq!(pad_len(1), 7);
        assert_eq!(pad_len(7), 1);
        assert_eq!(pad_len(8), 0);
        assert_eq!(pad_len(9), 7);
        assert_eq!(pad_len(13), 3); // "nix-archive-1"
        assert_eq!(pad_len(14), 2); // "Hello, World!\n"
    }

    // ── NAR archive starts with TOK_NAR ───────────────────────────────────────

    #[test]
    fn every_nar_starts_with_tok_nar_magic() {
        let node = NarNode::File {
            executable: false,
            content: vec![],
        };
        let bytes = nar_bytes(&node);
        assert!(
            bytes.starts_with(&TOK_NAR),
            "every NAR archive must begin with TOK_NAR (56 bytes)"
        );
    }

    // ── Regular file ──────────────────────────────────────────────────────────

    #[test]
    fn nar_regular_file_hello_world() {
        // "Hello, World!\n" is 14 bytes; padding = 2
        let node = NarNode::File {
            executable: false,
            content: b"Hello, World!\n".to_vec(),
        };
        let bytes = nar_bytes(&node);

        // TOK_NAR(56) + TOK_REG(32) + len(8) + content(14) + pad(2) + TOK_PAR(16) = 128
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(&TOK_NAR);
        expected.extend_from_slice(&TOK_REG);
        expected.extend_from_slice(b"\x0e\x00\x00\x00\x00\x00\x00\x00"); // length = 14
        expected.extend_from_slice(b"Hello, World!\n\x00\x00"); // content + 2 padding
        expected.extend_from_slice(&TOK_PAR);

        assert_eq!(
            bytes.len(),
            128,
            "single regular file NAR must be exactly 128 bytes"
        );
        assert_eq!(
            bytes, expected,
            "single regular file NAR bytes must match spec"
        );
    }

    #[test]
    fn nar_empty_file_content() {
        let node = NarNode::File {
            executable: false,
            content: vec![],
        };
        let bytes = nar_bytes(&node);

        // TOK_NAR(56) + TOK_REG(32) + write_str("")(8) + TOK_PAR(16) = 112
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(&TOK_NAR);
        expected.extend_from_slice(&TOK_REG);
        expected.extend_from_slice(&[0u8; 8]); // write_str("") = 8 zero bytes
        expected.extend_from_slice(&TOK_PAR);

        assert_eq!(bytes.len(), 112, "empty file NAR must be exactly 112 bytes");
        assert_eq!(bytes, expected);
    }

    #[test]
    fn nar_executable_file_uses_tok_exe_not_tok_reg() {
        let node = NarNode::File {
            executable: true,
            content: b"#!/bin/sh\n".to_vec(),
        };
        let bytes = nar_bytes(&node);

        assert!(
            bytes.windows(TOK_EXE.len()).any(|w| w == TOK_EXE.as_ref()),
            "executable file must contain TOK_EXE token"
        );
        assert!(
            !bytes.windows(TOK_REG.len()).any(|w| w == TOK_REG.as_ref()),
            "executable file must NOT contain TOK_REG token"
        );
    }

    #[test]
    fn nar_non_executable_file_uses_tok_reg_not_tok_exe() {
        let node = NarNode::File {
            executable: false,
            content: b"data".to_vec(),
        };
        let bytes = nar_bytes(&node);

        assert!(
            bytes.windows(TOK_REG.len()).any(|w| w == TOK_REG.as_ref()),
            "non-executable file must contain TOK_REG token"
        );
        assert!(
            !bytes.windows(TOK_EXE.len()).any(|w| w == TOK_EXE.as_ref()),
            "non-executable file must NOT contain TOK_EXE token"
        );
    }

    // ── Symlink ───────────────────────────────────────────────────────────────

    #[test]
    fn nar_symlink_target_encoded_correctly() {
        // "alpha.txt" is 9 bytes; padding = 7
        let node = NarNode::Symlink {
            target: "alpha.txt".to_string(),
        };
        let bytes = nar_bytes(&node);

        // TOK_NAR(56) + TOK_SYM(32) + len(8) + content(9) + pad(7) + TOK_PAR(16) = 128
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(&TOK_NAR);
        expected.extend_from_slice(&TOK_SYM);
        expected.extend_from_slice(b"\x09\x00\x00\x00\x00\x00\x00\x00"); // length = 9
        expected.extend_from_slice(b"alpha.txt\x00\x00\x00\x00\x00\x00\x00"); // content + 7 padding
        expected.extend_from_slice(&TOK_PAR);

        assert_eq!(
            bytes.len(),
            128,
            "single symlink NAR must be exactly 128 bytes"
        );
        assert_eq!(bytes, expected, "symlink NAR bytes must match spec");
    }

    // ── Directory ─────────────────────────────────────────────────────────────

    #[test]
    fn nar_directory_entries_in_lexicographic_order() {
        // Insert in non-lexicographic order; BTreeMap must reorder
        let mut dir = BTreeMap::new();
        dir.insert(
            "z.txt".to_string(),
            NarNode::File {
                executable: false,
                content: b"Z".to_vec(),
            },
        );
        dir.insert(
            "a.txt".to_string(),
            NarNode::File {
                executable: false,
                content: b"A".to_vec(),
            },
        );
        dir.insert(
            "m.txt".to_string(),
            NarNode::File {
                executable: false,
                content: b"M".to_vec(),
            },
        );
        let bytes = nar_bytes(&NarNode::Directory(dir));

        let a_pos = bytes
            .windows(5)
            .position(|w| w == b"a.txt")
            .expect("a.txt must appear");
        let m_pos = bytes
            .windows(5)
            .position(|w| w == b"m.txt")
            .expect("m.txt must appear");
        let z_pos = bytes
            .windows(5)
            .position(|w| w == b"z.txt")
            .expect("z.txt must appear");

        assert!(a_pos < m_pos, "a.txt must precede m.txt in the archive");
        assert!(m_pos < z_pos, "m.txt must precede z.txt in the archive");
    }

    #[test]
    fn nar_nested_directory_contains_expected_names() {
        let mut inner = BTreeMap::new();
        inner.insert(
            "file.txt".to_string(),
            NarNode::File {
                executable: false,
                content: b"content".to_vec(),
            },
        );
        let mut root = BTreeMap::new();
        root.insert("subdir".to_string(), NarNode::Directory(inner));
        let bytes = nar_bytes(&NarNode::Directory(root));

        assert!(
            bytes.windows(6).any(|w| w == b"subdir"),
            "nested directory name 'subdir' must appear in NAR output"
        );
        assert!(
            bytes.windows(8).any(|w| w == b"file.txt"),
            "nested file name 'file.txt' must appear in NAR output"
        );
    }

    // ── Determinism ───────────────────────────────────────────────────────────

    #[test]
    fn nar_output_is_deterministic() {
        let node = NarNode::File {
            executable: false,
            content: b"determinism test".to_vec(),
        };
        let bytes1 = nar_bytes(&node);
        let bytes2 = nar_bytes(&node);
        assert_eq!(
            bytes1, bytes2,
            "write_nar must produce identical bytes for identical input"
        );
    }

    // ── Token verification (the token constants must decode to expected strings) ──

    #[test]
    fn tok_nar_starts_with_nix_archive_magic() {
        // First 8 bytes are u64 LE length 13, then "nix-archive-1"
        assert_eq!(&TOK_NAR[..8], b"\x0d\x00\x00\x00\x00\x00\x00\x00");
        assert_eq!(&TOK_NAR[8..21], b"nix-archive-1");
    }
}
