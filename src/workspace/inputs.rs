use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::path_key;

// A rename needs a complete reference set even when the editor supplies no
// file watcher. Capture before go list so a newly added importer, package or
// build-selected source cannot silently fall outside the installed graph.
pub(super) struct PackageInputs {
    root: PathBuf,
    files: BTreeMap<PathBuf, u64>,
}

impl PackageInputs {
    pub(super) fn capture(root: &Path, roots: &[PathBuf]) -> Result<Self> {
        let mut files = BTreeMap::new();
        let mut pending = roots.to_vec();
        let mut seen = HashSet::new();
        while let Some(dir) = pending.pop() {
            if !seen.insert(path_key(&dir)) {
                continue;
            }
            let entries =
                fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))?;
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let filename = entry.file_name();
                let name = filename.to_string_lossy();
                if name.starts_with(['.', '_']) {
                    continue;
                }
                if entry.file_type()?.is_dir() {
                    if matches!(name.as_ref(), "vendor" | "testdata") {
                        continue;
                    }
                    pending.push(path);
                } else if path.extension().is_some_and(|x| x == "go")
                    || matches!(
                        name.as_ref(),
                        "go.mod" | "go.sum" | "go.work" | "go.work.sum"
                    )
                {
                    record(&mut files, &path)?;
                }
            }
        }
        for dir in root.ancestors() {
            for name in ["go.work", "go.work.sum", "go.mod", "go.sum"] {
                let path = dir.join(name);
                if path.is_file() {
                    record(&mut files, &path)?;
                }
            }
        }
        if let Some(path) = std::env::var_os("GOWORK").map(PathBuf::from)
            && path.is_absolute()
            && path.is_file()
        {
            record(&mut files, &path)?;
        }
        Ok(Self {
            root: root.to_owned(),
            files,
        })
    }

    pub(super) fn current(&self, roots: &[PathBuf]) -> Result<bool> {
        Ok(Self::capture(&self.root, roots)?.files == self.files)
    }
}

fn record(files: &mut BTreeMap<PathBuf, u64>, path: &Path) -> Result<()> {
    anyhow::ensure!(path.is_file(), "not a regular Go input: {}", path.display());
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hash = DefaultHasher::new();
    bytes.hash(&mut hash);
    files.insert(path_key(path), hash.finish());
    Ok(())
}
