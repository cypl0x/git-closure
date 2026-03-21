//! GCL artifact backend.
//!
//! Serializes a [`Closure`] to the human-readable `.gcl` S-expression snapshot
//! format via the existing [`TryFrom<Closure>`] round-trip in `crate::ir` and
//! [`crate::gcl::serial::serialize_snapshot`].

use std::fs;
use std::io;
use std::path::Path;

use crate::backends::{ArtifactBackend, Result};
use crate::error::GitClosureError;
use crate::gcl::serial::serialize_snapshot;
use crate::ir::Closure;

/// Artifact backend that writes a [`Closure`] as a `.gcl` snapshot file.
pub struct GclBackend;

impl ArtifactBackend for GclBackend {
    fn name(&self) -> &'static str {
        "gcl"
    }

    fn extension(&self) -> &'static str {
        "gcl"
    }

    fn write(&self, closure: &Closure, output: &Path) -> Result<()> {
        let (header, files) = closure.clone().try_into()?;
        let serialized = serialize_snapshot(&files, &header);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, serialized.as_bytes()).map_err(|e| {
            GitClosureError::Io(io::Error::new(
                e.kind(),
                format!("{}: {e}", output.display()),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::ArtifactBackend;
    use crate::gcl::serial::parse_snapshot;
    use crate::ir::{Closure, ClosureNode, FileNode};

    #[test]
    fn gcl_backend_roundtrip() {
        let closure = Closure {
            nodes: vec![ClosureNode::File(FileNode {
                path: "hello.txt".to_string(),
                sha256: crate::gcl::hash::sha256_hex(b"hello world\n"),
                mode: "644".to_string(),
                size: 12,
                content: b"hello world\n".to_vec(),
            })],
            provenance: vec![],
        };

        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("out.gcl");

        GclBackend.write(&closure, &output).unwrap();
        assert!(output.exists());

        let text = std::fs::read_to_string(&output).unwrap();
        let (_header, files) = parse_snapshot(&text).expect("GclBackend must emit valid .gcl");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "hello.txt");
        assert_eq!(files[0].content, b"hello world\n");
    }
}
