use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub(crate) fn list_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let single_root = || vec![root.to_owned()];
    let workspace_override = env::var_os("GOWORK").unwrap_or_default();
    if env::var_os("GO111MODULE").is_some_and(|mode| mode == "off") || workspace_override == "off" {
        return Ok(single_root());
    }

    // A module directory stays scoped to itself even under an ancestor go.work.
    if (workspace_override.is_empty() || workspace_override == "auto")
        && root.join("go.mod").is_file()
        && !root.join("go.work").is_file()
    {
        return Ok(single_root());
    }

    let environment: GoEnvironment = go_json(root, &["env", "-json", "GO111MODULE", "GOWORK"])?;
    if environment.module_mode == "off"
        || environment.work_file.is_empty()
        || environment.work_file == "off"
    {
        return Ok(single_root());
    }

    let work_file = Path::new(&environment.work_file);
    let work_dir = work_file
        .parent()
        .with_context(|| format!("cannot determine the directory of {}", work_file.display()))?;
    let root_key = canonical_directory(root)?;
    let at_work_root = root_key == canonical_directory(work_dir)?;
    if !at_work_root && root.join("go.mod").is_file() {
        return Ok(single_root());
    }

    let workspace: GoWorkspace = go_json(root, &["work", "edit", "-json"])?;
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for member in workspace.members.unwrap_or_default() {
        let directory = work_dir.join(member.disk_path);
        let key = canonical_directory(&directory)?;
        if !at_work_root && root_key.starts_with(&key) {
            // Opening a subdirectory of a used module limits discovery to that subtree.
            return Ok(single_root());
        }
        if (at_work_root || key.starts_with(&root_key)) && seen.insert(key) {
            roots.push(directory);
        }
    }

    if roots.is_empty() {
        ensure!(
            !at_work_root,
            "Go workspace {} has no used modules",
            work_file.display()
        );
        return Ok(single_root());
    }
    Ok(roots)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("locating Go workspace directory {}", path.display()))
}

fn go_json<T: DeserializeOwned>(root: &Path, args: &[&str]) -> Result<T> {
    let command = format!("go {}", args.join(" "));
    let output = Command::new("go")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("starting {command} in {}", root.display()))?;
    ensure!(
        output.status.success(),
        "{command} failed in {} ({}): {}",
        root.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("decoding {command} output in {}", root.display()))
}

#[derive(Deserialize)]
struct GoEnvironment {
    #[serde(rename = "GO111MODULE")]
    module_mode: String,
    #[serde(rename = "GOWORK")]
    work_file: String,
}

#[derive(Deserialize)]
struct GoWorkspace {
    #[serde(rename = "Use")]
    members: Option<Vec<WorkspaceMember>>,
}

#[derive(Deserialize)]
struct WorkspaceMember {
    #[serde(rename = "DiskPath")]
    disk_path: PathBuf,
}
