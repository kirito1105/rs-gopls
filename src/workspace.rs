use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;

use crate::workspace_roots::list_roots;

pub struct Package {
    pub name: String,
    pub files: Vec<PathBuf>,
    import_map: HashMap<String, String>,
    test_variant: bool,
}

pub struct Workspace {
    packages: BTreeMap<String, Package>,
    file_packages: HashMap<PathBuf, String>,
    includes_tests: bool,
    builtin_source: OnceCell<Result<PathBuf, String>>,
}

impl Workspace {
    pub fn load(root: &Path, include_tests: bool) -> Result<Self> {
        ensure!(
            root.is_dir(),
            "Go workspace root is not a directory: {}",
            root.display()
        );
        let roots = list_roots(root)?;
        let patterns = roots.iter().map(|directory| {
            if directory == root {
                "./...".to_owned()
            } else {
                directory.join("...").to_string_lossy().replace('\\', "/")
            }
        });
        let mut child = Command::new("go")
            .args([
                "list",
                "-e",
                "-json=Dir,ImportPath,Name,GoFiles,CgoFiles,ImportMap,Error,ForTest",
                "-deps",
            ])
            .args(include_tests.then_some("-test"))
            .args(patterns)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting go list in {}", root.display()))?;
        let stdout = child.stdout.take().expect("go list stdout is piped");
        let mut stderr = child.stderr.take().expect("go list stderr is piped");
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        });

        let packages = read_packages(BufReader::new(stdout));
        let status = child.wait().context("waiting for go list")?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| anyhow!("go list stderr reader panicked"))?
            .context("reading go list stderr")?;
        let stderr = String::from_utf8_lossy(&stderr);
        if !status.success() {
            bail!(
                "go list failed in {} ({status}): {}",
                root.display(),
                stderr.trim()
            );
        }
        let (packages, package_error) =
            packages.with_context(|| format!("reading Go packages in {}", root.display()))?;
        if packages.is_empty() {
            let detail = package_error.as_deref().unwrap_or_else(|| stderr.trim());
            if detail.is_empty() {
                bail!("no Go packages found in {}", root.display());
            }
            bail!("no Go packages found in {}: {detail}", root.display());
        }

        let mut file_packages = HashMap::new();
        // Ordinary files belong to their normal package, even when the Go
        // command also reports an in-package test compilation of those files.
        let ordinary = packages.iter().filter(|(_, package)| !package.test_variant);
        let testing = packages.iter().filter(|(_, package)| package.test_variant);
        for (id, package) in ordinary.chain(testing) {
            for file in &package.files {
                file_packages
                    .entry(path_key(file))
                    .or_insert_with(|| id.clone());
            }
        }
        Ok(Self {
            packages,
            file_packages,
            includes_tests: include_tests,
            builtin_source: OnceCell::new(),
        })
    }

    pub(crate) fn builtin_source(&self, root: &Path) -> Result<&Path> {
        self.builtin_source
            .get_or_init(|| find_builtin_source(root).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map(PathBuf::as_path)
            .map_err(|error| anyhow!("{error}"))
    }

    pub fn includes_tests(&self) -> bool {
        self.includes_tests
    }

    pub fn package_of_file(&self, path: &Path) -> Option<&Package> {
        self.package(self.file_packages.get(&path_key(path))?)
    }

    pub fn package(&self, import_path: &str) -> Option<&Package> {
        self.packages.get(import_path)
    }

    pub fn resolve_import(&self, from: &Package, import_path: &str) -> Option<&Package> {
        self.package(
            from.import_map
                .get(import_path)
                .map(String::as_str)
                .unwrap_or(import_path),
        )
    }
}

fn find_builtin_source(root: &Path) -> Result<PathBuf> {
    let output = Command::new("go")
        .args(["env", "GOROOT"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("starting go env GOROOT in {}", root.display()))?;
    ensure!(
        output.status.success(),
        "go env GOROOT failed in {} ({}): {}",
        root.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let goroot = std::str::from_utf8(&output.stdout).context("decoding go env GOROOT")?;
    let goroot = Path::new(goroot.trim_end_matches(['\r', '\n']));
    ensure!(
        goroot.is_absolute(),
        "go env GOROOT did not return an absolute path"
    );
    Ok(goroot.join("src").join("builtin").join("builtin.go"))
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct ListedPackage {
    dir: PathBuf,
    import_path: String,
    for_test: String,
    name: String,
    go_files: Vec<PathBuf>,
    cgo_files: Vec<PathBuf>,
    import_map: HashMap<String, String>,
    error: Option<PackageError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PackageError {
    err: String,
}

fn read_packages(reader: impl Read) -> Result<(BTreeMap<String, Package>, Option<String>)> {
    let mut packages = BTreeMap::new();
    let mut first_error = None;
    let stream = serde_json::Deserializer::from_reader(reader).into_iter::<ListedPackage>();
    for package in stream {
        let package = package.context("decoding go list package metadata")?;
        if first_error.is_none() {
            first_error = package.error.map(|error| error.err);
        }
        if package.dir.as_os_str().is_empty()
            || package.import_path.is_empty()
            || package.name.is_empty()
        {
            continue;
        }

        let mut files: Vec<_> = package
            .go_files
            .into_iter()
            .chain(package.cgo_files)
            .map(|file| package.dir.join(file))
            .collect();
        if files.is_empty() {
            continue;
        }
        files.sort_unstable();
        files.dedup();
        let import_path = package.import_path;
        packages
            .entry(import_path.clone())
            .or_insert_with(|| Package {
                name: package.name,
                files,
                import_map: package.import_map,
                test_variant: !package.for_test.is_empty(),
            });
    }
    Ok((packages, first_error))
}

pub(crate) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().replace('/', "\\").to_lowercase();
        if let Some(unc) = path.strip_prefix("\\\\?\\unc\\") {
            return PathBuf::from(format!("\\\\{unc}"));
        }
        if let Some(disk) = path.strip_prefix("\\\\?\\") {
            return PathBuf::from(disk);
        }
        PathBuf::from(path)
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}
