//! Local filesystem provider.

use std::fs;
use std::path::Path;

use super::{FetchedSource, Provider, Result};

pub struct LocalProvider;

impl Provider for LocalProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let path = Path::new(source);
        if !path.exists() {
            return Err(crate::error::GitClosureError::Parse(format!(
                "local source path does not exist: {source}"
            )));
        }
        let absolute = fs::canonicalize(path)?;
        Ok(FetchedSource::local(absolute))
    }
}
