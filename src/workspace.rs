use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;

use crate::workspace_roots::list_roots;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLoad {
    pub generation: u64,
    pub include_tests: bool,
}

pub struct Package {
    pub id: String,
    pub name: String,
    pub files: Vec<PathBuf>,
    directory: PathBuf,
    import_map: HashMap<String, String>,
    test_variant: bool,
}

pub struct Workspace {
    packages: BTreeMap<String, Package>,
    file_packages: HashMap<PathBuf, String>,
    importers: HashMap<String, Vec<String>>,
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
                "-json=Dir,ImportPath,Name,GoFiles,CgoFiles,Imports,ImportMap,Error,ForTest",
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
        let PackageListing {
            packages,
            importers,
            package_error,
        } = packages.with_context(|| format!("reading Go packages in {}", root.display()))?;
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
            importers,
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
        self.package(from.import_id(import_path))
    }

    pub(crate) fn package_of_draft(&self, path: &Path, name: &str) -> Option<&Package> {
        let directory = path_key(path.parent()?);
        let mut packages = self.packages.values().filter(|package| {
            !package.test_variant && package.directory == directory && package.name == name
        });
        let package = packages.next()?;
        packages.next().is_none().then_some(package)
    }

    pub(crate) fn affected_packages(&self, path: &Path) -> Option<HashSet<String>> {
        let directory = path_key(path.parent()?);
        let mut queue: Vec<_> = self
            .packages
            .values()
            .filter(|package| package.directory == directory)
            .map(|package| package.id.as_str())
            .collect();
        if queue.is_empty() {
            return None;
        }

        let mut dirty = HashSet::new();
        while let Some(id) = queue.pop() {
            if dirty.insert(id.to_owned())
                && let Some(importers) = self.importers.get(id)
            {
                queue.extend(importers.iter().map(String::as_str));
            }
        }
        Some(dirty)
    }
}

impl Package {
    fn import_id<'a>(&'a self, path: &'a str) -> &'a str {
        self.import_map
            .get(path)
            .map(String::as_str)
            .unwrap_or(path)
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
    imports: Vec<String>,
    import_map: HashMap<String, String>,
    error: Option<PackageError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PackageError {
    err: String,
}

#[derive(Default)]
struct PackageListing {
    packages: BTreeMap<String, Package>,
    importers: HashMap<String, Vec<String>>,
    package_error: Option<String>,
}

fn read_packages(reader: impl Read) -> Result<PackageListing> {
    let mut listing = PackageListing::default();
    let stream = serde_json::Deserializer::from_reader(reader).into_iter::<ListedPackage>();
    for package in stream {
        let package = package.context("decoding go list package metadata")?;
        if listing.package_error.is_none() {
            listing.package_error = package.error.map(|error| error.err);
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
        let std::collections::btree_map::Entry::Vacant(slot) =
            listing.packages.entry(import_path.clone())
        else {
            continue;
        };
        let owner = slot.insert(Package {
            id: import_path,
            name: package.name,
            files,
            directory: path_key(&package.dir),
            import_map: package.import_map,
            test_variant: !package.for_test.is_empty(),
        });
        for import in package.imports {
            let id = owner.import_id(&import);
            for dependency in
                std::iter::once(id).chain((id != import.as_str()).then_some(import.as_str()))
            {
                listing
                    .importers
                    .entry(dependency.to_owned())
                    .or_default()
                    .push(owner.id.clone());
            }
        }
    }
    Ok(listing)
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
