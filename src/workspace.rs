use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;

use crate::workspace_roots::{folder_groups, list_roots_for_folders};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLoad {
    pub generation: u64,
    pub include_tests: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PackageErrorKind {
    Direct,
    Dependency,
}

/// Source positions use Go's one-based lines and byte columns.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PackageLoadError {
    pub package_id: String,
    pub for_test: String,
    pub kind: PackageErrorKind,
    pub message: String,
    pub import_stack: Vec<String>,
    pub path: Option<PathBuf>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

pub struct Package {
    pub id: String,
    pub name: String,
    pub files: Vec<PathBuf>,
    pub(crate) load_error: bool,
    directory: PathBuf,
    import_map: HashMap<String, String>,
    for_test: String,
    external: bool,
    group: Option<usize>,
    standard: bool,
    module_path: Option<String>,
    direct_dependency: bool,
}

pub struct Workspace {
    roots: Vec<PathBuf>,
    module_paths: Vec<String>,
    package_errors: Vec<PackageLoadError>,
    group_imports: Vec<HashMap<String, String>>,
    packages: BTreeMap<String, Package>,
    file_packages: HashMap<PathBuf, String>,
    importers: HashMap<String, Vec<String>>,
    includes_tests: bool,
    sdk_root: OnceCell<PathBuf>,
    builtin_source: OnceCell<Result<PathBuf, String>>,
}

impl Workspace {
    #[cfg_attr(not(test), allow(dead_code, reason = "single-folder loader"))]
    pub fn load(root: &Path, include_tests: bool) -> Result<Self> {
        Self::load_roots(&[root.to_owned()], include_tests)
    }

    pub(crate) fn load_roots(folders: &[PathBuf], include_tests: bool) -> Result<Self> {
        let mut roots = BTreeSet::new();
        let mut listing = PackageListing::default();
        let groups = folder_groups(folders)?;
        for (index, group) in groups.iter().enumerate() {
            let directories = list_roots_for_folders(group)?;
            list_packages(
                &group[0],
                &directories,
                include_tests,
                (groups.len() > 1).then_some(index),
                &mut listing,
            )?;
            for directory in directories {
                roots.insert(path_key(&directory.canonicalize().unwrap_or(directory)));
            }
        }
        if groups.len() > 1 {
            for importers in listing.importers.values_mut() {
                importers.sort_unstable();
                importers.dedup();
            }
        }
        Ok(Self::from_listing(
            roots.into_iter().collect(),
            listing,
            include_tests,
        ))
    }

    fn from_listing(roots: Vec<PathBuf>, listing: PackageListing, includes_tests: bool) -> Self {
        let PackageListing {
            packages,
            importers,
            sdk_root,
            module_paths,
            mut errors,
            group_imports,
            ..
        } = listing;
        errors.sort_unstable();
        errors.dedup();
        let mut file_packages = HashMap::new();
        // Ordinary files belong to their normal package, even when the Go
        // command also reports an in-package test compilation of those files.
        let ordinary = packages
            .iter()
            .filter(|(_, package)| package.for_test.is_empty());
        let testing = packages
            .iter()
            .filter(|(_, package)| !package.for_test.is_empty());
        for (id, package) in ordinary.chain(testing) {
            for file in &package.files {
                file_packages
                    .entry(path_key(file))
                    .or_insert_with(|| id.clone());
            }
        }
        Self {
            roots,
            module_paths: module_paths.into_iter().collect(),
            package_errors: errors,
            group_imports,
            packages,
            file_packages,
            importers,
            includes_tests,
            sdk_root: sdk_root.map_or_else(OnceCell::new, OnceCell::from),
            builtin_source: OnceCell::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_listing(
        roots: &[PathBuf],
        listing: &str,
        includes_tests: bool,
    ) -> Self {
        let mut packages = PackageListing::default();
        packages
            .read(
                listing.as_bytes(),
                roots.first().map_or(Path::new("."), PathBuf::as_path),
                None,
            )
            .unwrap();
        Self::from_listing(
            roots.iter().map(|root| path_key(root)).collect(),
            packages,
            includes_tests,
        )
    }

    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "local module paths for import actions")
    )]
    pub(crate) fn module_paths(&self) -> &[String] {
        &self.module_paths
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "package graph contract for diagnostics")
    )]
    pub(crate) fn package_errors(&self) -> &[PackageLoadError] {
        &self.package_errors
    }

    pub(crate) fn builtin_source(&self, root: &Path) -> Result<&Path> {
        self.builtin_source
            .get_or_init(|| {
                if let Some(sdk) = self.sdk_root() {
                    Ok(sdk.join("src").join("builtin").join("builtin.go"))
                } else {
                    find_builtin_source(root).map_err(|error| format!("{error:#}"))
                }
            })
            .as_ref()
            .map(PathBuf::as_path)
            .map_err(|error| anyhow!("{error}"))
    }

    pub(crate) fn sdk_root(&self) -> Option<&Path> {
        self.sdk_root.get().map(PathBuf::as_path)
    }

    pub(crate) fn share_sdk_root(&self, root: &Path) {
        let _ = self.sdk_root.set(root.to_owned());
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

    pub(crate) fn navigation_packages(&self) -> impl Iterator<Item = &Package> {
        self.packages.values()
    }

    pub fn resolve_import(&self, from: &Package, import_path: &str) -> Option<&Package> {
        let id = from
            .import_map
            .get(import_path)
            .or_else(|| self.group_imports.get(from.group?)?.get(import_path));
        self.package(id.map_or(import_path, String::as_str))
    }

    pub(crate) fn matches_import_variant(
        &self,
        from: &Package,
        import_path: &str,
        id: &str,
    ) -> bool {
        let Some(group) = from.group else { return true };
        // Only colliding package IDs are qualified with their independent build group.
        match self.group_imports[group].get(import_path) {
            Some(selected) => selected == id,
            None => !id.contains(':'),
        }
    }

    pub(crate) fn package_of_draft(&self, path: &Path, name: &str) -> Option<&Package> {
        let directory = path_key(path.parent()?);
        let mut packages = self.packages.values().filter(|package| {
            package.for_test.is_empty() && package.directory == directory && package.name == name
        });
        let package = packages.next()?;
        packages.next().is_none().then_some(package)
    }

    pub(crate) fn same_package_variants<'w>(
        &'w self,
        owner: &'w Package,
    ) -> impl Iterator<Item = &'w Package> {
        let id = if owner.for_test.is_empty() {
            &owner.id
        } else {
            &owner.for_test
        };
        self.packages.values().filter(move |package| {
            package.directory == owner.directory && (&package.id == id || &package.for_test == id)
        })
    }

    pub(crate) fn is_project_package(&self, package: &Package) -> bool {
        self.contains_project_path(&package.directory)
    }

    pub(crate) fn contains_project_path(&self, key: &Path) -> bool {
        self.roots().iter().any(|root| key.starts_with(root))
    }

    pub(crate) fn contains_external_symbol_path(&self, key: &Path) -> bool {
        self.navigation_packages()
            .any(|package| package.external && key.starts_with(&package.directory))
    }

    pub(crate) fn project_symbol_files(&self) -> HashMap<PathBuf, PathBuf> {
        let mut files = HashMap::new();
        for package in self
            .navigation_packages()
            .filter(|package| !package.external && self.is_project_package(package))
        {
            for path in &package.files {
                let key = path_key(path);
                // The synthetic test main may have an absolute Go build-cache
                // path in GoFiles. A package's source files share its directory.
                if key.parent() == Some(package.directory.as_path()) && is_symbol_source(&key) {
                    files.entry(key).or_insert_with(|| path.clone());
                }
            }
        }
        files
    }

    pub(crate) fn direct_importers<'a>(
        &'a self,
        package: &Package,
    ) -> impl Iterator<Item = &'a Package> {
        self.importers
            .get(&package.id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.package(id))
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
    fn remap_imports(&mut self, imports: &[String], aliases: &HashMap<String, String>) {
        if let Some(id) = aliases.get(&self.for_test) {
            self.for_test.clone_from(id);
        }
        for target in self.import_map.values_mut() {
            if let Some(id) = aliases.get(target) {
                target.clone_from(id);
            }
        }
        for import in imports {
            if let Some(id) = aliases.get(import) {
                self.import_map
                    .entry(import.clone())
                    .or_insert_with(|| id.clone());
            }
        }
    }

    fn same_build(&self, other: &Self) -> bool {
        self.directory == other.directory
            && self.files == other.files
            && self.import_map == other.import_map
            && self.for_test == other.for_test
            && self.name == other.name
            && self.load_error == other.load_error
            && self.external == other.external
            && self.standard == other.standard
            && self.module_path == other.module_path
    }

    pub(crate) fn directory_and_name(&self) -> (&Path, &str) {
        (&self.directory, &self.name)
    }

    pub(crate) fn import_path(&self) -> &str {
        let path = self
            .id
            .split_once(':')
            .map_or(self.id.as_str(), |(_, path)| path);
        path.split_once(" [").map_or(path, |(path, _)| path)
    }

    pub(crate) fn is_production(&self) -> bool {
        self.for_test.is_empty()
    }

    pub(crate) fn is_standard(&self) -> bool {
        self.standard
    }

    pub(crate) fn is_external(&self) -> bool {
        self.external
    }

    pub(crate) fn module_path(&self) -> Option<&str> {
        self.module_path.as_deref()
    }

    pub(crate) fn is_direct_dependency(&self) -> bool {
        self.direct_dependency
    }

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

fn list_packages(
    root: &Path,
    directories: &[PathBuf],
    include_tests: bool,
    group: Option<usize>,
    listing: &mut PackageListing,
) -> Result<()> {
    let patterns = directories.iter().map(|directory| {
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
            "-json=Dir,ImportPath,Name,GoFiles,CgoFiles,Imports,ImportMap,Error,DepsErrors,ForTest,Goroot,Module",
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
    let packages = listing.read(BufReader::new(stdout), root, group);
    let status = child.wait().context("waiting for go list")?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("go list stderr reader panicked"))?
        .context("reading go list stderr")?;
    if !status.success() {
        bail!(
            "go list failed in {} ({status}): {}",
            root.display(),
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    packages.with_context(|| format!("reading Go packages in {}", root.display()))
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
    error: Option<ListedError>,
    deps_errors: Option<Vec<ListedError>>,
    goroot: bool,
    module: Option<ListedModule>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct ListedModule {
    path: String,
    main: bool,
    indirect: bool,
    version: String,
    replace: Option<Box<ListedModule>>,
}

impl ListedModule {
    fn in_module_cache(&self) -> bool {
        !self.main && !self.replace.as_deref().unwrap_or(self).version.is_empty()
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct ListedError {
    import_stack: Option<Vec<String>>,
    pos: String,
    err: String,
}

impl ListedError {
    fn resolve(
        self,
        kind: PackageErrorKind,
        package: &ListedPackage,
        root: &Path,
    ) -> PackageLoadError {
        let (path, line, column) = error_position(&self.pos, root, &package.dir);
        PackageLoadError {
            package_id: package.import_path.clone(),
            for_test: package.for_test.clone(),
            kind,
            message: self.err,
            import_stack: self.import_stack.unwrap_or_default(),
            path,
            line,
            column,
        }
    }
}

fn error_position(
    pos: &str,
    root: &Path,
    dir: &Path,
) -> (Option<PathBuf>, Option<u32>, Option<u32>) {
    fn split_number(value: &str) -> Option<(&str, u32)> {
        let (prefix, number) = value.rsplit_once(':')?;
        Some((prefix, number.parse().ok()?))
    }
    let (file, line, column) = match split_number(pos) {
        Some((prefix, last)) => match split_number(prefix) {
            Some((file, line)) => (file, Some(line), Some(last)),
            None => (prefix, Some(last), None),
        },
        None => (pos, None, None),
    };
    (
        error_path(file, root, dir),
        line.filter(|&n| n > 0),
        column.filter(|&n| n > 0),
    )
}

fn error_path(file: &str, root: &Path, dir: &Path) -> Option<PathBuf> {
    if file.is_empty() || file == "-" {
        return None;
    }
    let path = Path::new(file);
    let windows_absolute = matches!(file.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
        || file.starts_with(r"\\");
    if file.starts_with('/') || path.is_absolute() || windows_absolute {
        return Some(path.to_owned());
    }
    // cmd/go can shorten a dependency's position relative to its working directory.
    let from_root = root.join(path);
    let absolute = if dir.as_os_str().is_empty() || from_root.is_file() {
        from_root
    } else {
        root.join(dir).join(path)
    };
    if path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Some(absolute.canonicalize().unwrap_or(absolute));
    }
    Some(absolute)
}

#[derive(Default)]
struct PackageListing {
    packages: BTreeMap<String, Package>,
    sdk_root: Option<PathBuf>,
    group_imports: Vec<HashMap<String, String>>,
    alternate_ids: HashMap<String, Vec<String>>,
    importers: HashMap<String, Vec<String>>,
    module_paths: BTreeSet<String>,
    errors: Vec<PackageLoadError>,
}

impl PackageListing {
    fn read(&mut self, reader: impl Read, root: &Path, group: Option<usize>) -> Result<()> {
        // go list -deps emits dependencies before their importers, including test variants.
        let mut aliases: HashMap<String, String> = HashMap::new();
        let stream = serde_json::Deserializer::from_reader(reader).into_iter::<ListedPackage>();
        for package in stream {
            let mut package = package.context("decoding go list package metadata")?;
            if package.goroot && self.sdk_root.is_none() {
                let import = package.import_path.split(" [").next().unwrap_or_default();
                let relative = Path::new(import);
                if !import.is_empty()
                    && package.dir.is_absolute()
                    && package.dir.ends_with(relative)
                {
                    self.sdk_root = package
                        .dir
                        .ancestors()
                        .nth(relative.components().count())
                        .filter(|src| src.file_name().is_some_and(|name| name == "src"))
                        .and_then(Path::parent)
                        .map(Path::to_owned);
                }
            }
            let load_error = package.error.is_some();
            if let Some(error) = package.error.take() {
                self.errors
                    .push(error.resolve(PackageErrorKind::Direct, &package, root));
            }
            for error in package.deps_errors.take().unwrap_or_default() {
                self.errors
                    .push(error.resolve(PackageErrorKind::Dependency, &package, root));
            }
            if let Some(module) = &package.module
                && module.main
                && !module.path.is_empty()
            {
                self.module_paths.insert(module.path.clone());
            }
            if package.dir.as_os_str().is_empty()
                || package.import_path.is_empty()
                || package.name.is_empty()
                || (package.go_files.is_empty() && package.cgo_files.is_empty())
            {
                if let Some(group) = group {
                    let id = format!("{group}:{}", package.import_path);
                    aliases.insert(package.import_path, id);
                }
                continue;
            }

            let mut files: Vec<_> = package
                .go_files
                .into_iter()
                .chain(package.cgo_files)
                .map(|file| package.dir.join(file))
                .collect();
            files.sort_unstable();
            files.dedup();
            let owner = Package {
                id: package.import_path,
                name: package.name,
                files,
                load_error,
                directory: path_key(&package.dir),
                import_map: package.import_map,
                for_test: package.for_test,
                external: package.goroot
                    || package
                        .module
                        .as_ref()
                        .is_some_and(ListedModule::in_module_cache),
                group,
                standard: package.goroot,
                direct_dependency: package
                    .module
                    .as_ref()
                    .is_some_and(|m| !m.main && !m.indirect),
                module_path: package.module.map(|m| m.path).filter(|p| !p.is_empty()),
            };
            self.insert(owner, package.imports, group, &mut aliases);
        }
        if group.is_some() {
            self.group_imports.push(aliases);
        }
        Ok(())
    }

    fn insert(
        &mut self,
        mut owner: Package,
        imports: Vec<String>,
        group: Option<usize>,
        aliases: &mut HashMap<String, String>,
    ) {
        if !aliases.is_empty() {
            owner.remap_imports(&imports, aliases);
        }
        if let Some(existing) = self.packages.get_mut(&owner.id) {
            let Some(group) = group else { return };
            if existing.same_build(&owner) {
                existing.direct_dependency |= owner.direct_dependency;
                return;
            }
            // Independent modules may bind the same Go import path to different sources.
            let variants = self.alternate_ids.entry(owner.id.clone()).or_default();
            if let Some(id) = variants
                .iter()
                .find(|id| self.packages[*id].same_build(&owner))
            {
                self.packages.get_mut(id).unwrap().direct_dependency |= owner.direct_dependency;
                aliases.insert(owner.id, id.clone());
                return;
            }
            let id = format!("{group}:{}", owner.id);
            variants.push(id.clone());
            aliases.insert(owner.id, id.clone());
            owner.id = id;
        }
        for import in imports {
            let id = owner.import_id(&import);
            let unqualified = aliases.get(&import).map_or(import.as_str(), String::as_str);
            for dependency in std::iter::once(id).chain((id != unqualified).then_some(unqualified))
            {
                self.importers
                    .entry(dependency.to_owned())
                    .or_default()
                    .push(owner.id.clone());
            }
        }
        self.packages.insert(owner.id.clone(), owner);
    }
}

pub(crate) fn is_symbol_source(key: &Path) -> bool {
    key.extension().is_some_and(|extension| extension == "go")
        && key
            .file_name()
            .is_some_and(|name| !name.to_string_lossy().starts_with(['.', '_']))
        && !key
            .components()
            .any(|component| component.as_os_str() == "vendor")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestProject;
    use serde_json::json;

    #[test]
    fn parses_package_error_positions_from_the_right() {
        let project = TestProject::new();
        let dir = project.root.join("pkg");
        for (pos, file, line, column) in [
            (r"C:\src\broken.go:12:5", r"C:\src\broken.go", 12, Some(5)),
            (r"C:\src\broken.go:12", r"C:\src\broken.go", 12, None),
            ("/src/broken.go:7:2", "/src/broken.go", 7, Some(2)),
            ("/src/broken.go:7", "/src/broken.go", 7, None),
        ] {
            assert_eq!(
                error_position(pos, &project.root, &dir),
                (Some(PathBuf::from(file)), Some(line), column)
            );
        }
        assert_eq!(
            error_position("broken.go:12", &project.root, &dir),
            (Some(dir.join("broken.go")), Some(12), None)
        );
        assert_eq!(
            error_position("go.mod", &project.root, Path::new("")),
            (Some(project.root.join("go.mod")), None, None)
        );
        let file = project.write("other/broken.go", "package broken\n");
        assert_eq!(
            error_position("other/broken.go:3:1", &project.root, &dir),
            (Some(file), Some(3), Some(1))
        );
        assert_eq!(error_position("", &project.root, &dir), (None, None, None));
    }

    #[test]
    fn retains_direct_and_dependency_errors_even_without_navigable_files() {
        let project = TestProject::new();
        let id = "example.test/pkg [example.test/pkg.test]";
        let dep = json!({"ImportStack": ["example.test/pkg", "missing"], "Err": "unknown import"});
        let broken = json!({
            "Dir": project.root.join("pkg"), "ImportPath": id, "ForTest": "example.test/pkg",
            "GoFiles": [], "Error": {"Pos": "broken_test.go:8:3", "Err": "parse failed"},
            "DepsErrors": [dep, dep], "Module": {"Main": true, "Path": "example.test/pkg"},
        });
        let missing = json!({
            "ImportPath": "missing", "Name": "missing", "Error": {
                "ImportStack": null, "Pos": "go.mod:2", "Err": "unknown module"
            }
        });
        let healthy = json!({
            "Dir": project.root, "ImportPath": "example.test/importer", "Name": "importer",
            "GoFiles": ["ok.go"], "DepsErrors": [dep],
        });
        let empty = json!({
            "Dir": project.root.join("empty"), "ImportPath": "z.test/empty", "Name": "empty",
            "GoFiles": [], "Error": { "Err": "no Go files" }
        });
        let records = [&missing, &broken, &healthy, &broken, &empty];
        let load = |records: &[&serde_json::Value]| {
            let listing = records
                .iter()
                .map(|record| record.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            Workspace::from_test_listing(std::slice::from_ref(&project.root), &listing, true)
        };
        let workspace = load(&records);
        assert_eq!(
            workspace.package_errors(),
            load(&[&empty, &healthy, &broken, &missing]).package_errors()
        );
        assert_eq!(workspace.package_errors().len(), 5);
        assert!(workspace.package(id).is_none());
        assert!(
            !workspace
                .package("example.test/importer")
                .unwrap()
                .load_error
        );
        assert_eq!(workspace.module_paths(), ["example.test/pkg"]);
        let errors = workspace.package_errors();
        assert_eq!(
            errors[1],
            PackageLoadError {
                package_id: id.into(),
                for_test: "example.test/pkg".into(),
                kind: PackageErrorKind::Direct,
                message: "parse failed".into(),
                import_stack: Vec::new(),
                path: Some(project.root.join("pkg/broken_test.go")),
                line: Some(8),
                column: Some(3),
            }
        );
        assert_eq!(
            errors[2],
            PackageLoadError {
                package_id: id.into(),
                for_test: "example.test/pkg".into(),
                kind: PackageErrorKind::Dependency,
                message: "unknown import".into(),
                import_stack: vec!["example.test/pkg".into(), "missing".into()],
                path: None,
                line: None,
                column: None,
            }
        );
        assert_eq!(errors[3].path, Some(project.root.join("go.mod")));
        assert_eq!((errors[3].line, errors[3].column), (Some(2), None));
        assert_eq!(errors[4].message, "no Go files");
    }

    #[test]
    fn merges_independent_module_graphs_and_resolves_each_folders_replacements() {
        let project = TestProject::new();
        project.write("common/go.mod", "module example.test/common\n\ngo 1.22\n");
        let common = project.write("common/common.go", "package common\n");
        for folder in ["one", "two"] {
            project.write(&format!("{folder}/go.mod"), &format!(
                "module example.test/{folder}\n\ngo 1.22\n\nrequire (\nexample.test/common v0.0.0\nexample.test/dep v0.0.0\n)\nreplace example.test/common => ../common\nreplace example.test/dep => ../dep-{folder}\n"
            ));
            project.write(&format!("{folder}/app.go"), &format!(
                "package {folder}\nimport (\n_ \"example.test/common\"\n_ \"example.test/dep\"\n_ \"example.test/{folder}/missing\"\n)\n"
            ));
            project.write(
                &format!("dep-{folder}/go.mod"),
                "module example.test/dep\n\ngo 1.22\n",
            );
            project.write(&format!("dep-{folder}/dep.go"), "package dep\n");
        }
        let folders = [project.root.join("two"), project.root.join("one")];
        let workspace = Workspace::load_roots(&folders, false).unwrap();
        assert_eq!(
            workspace.roots(),
            [
                path_key(&folders[1].canonicalize().unwrap()),
                path_key(&folders[0].canonicalize().unwrap())
            ]
        );
        assert_eq!(
            workspace.module_paths(),
            ["example.test/one", "example.test/two"]
        );
        let common = workspace.package_of_file(&common).unwrap();
        assert_eq!(
            workspace
                .direct_importers(common)
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["example.test/one", "example.test/two"]
        );
        for folder in ["one", "two"] {
            let file = project.root.join(folder).join("app.go");
            let owner = workspace.package_of_file(&file).unwrap();
            let dep = workspace.resolve_import(owner, "example.test/dep").unwrap();
            assert_eq!(dep.import_path(), "example.test/dep");
            assert_eq!(dep.module_path(), Some("example.test/dep"));
            assert!(dep.is_direct_dependency());
            let dep_file = project.root.join(format!("dep-{folder}/dep.go"));
            assert_eq!(dep.id, workspace.package_of_file(&dep_file).unwrap().id);
            assert_eq!(
                workspace
                    .direct_importers(dep)
                    .map(|p| &p.id)
                    .collect::<Vec<_>>(),
                [&owner.id]
            );
            assert_eq!(
                workspace
                    .resolve_import(owner, "example.test/common")
                    .unwrap()
                    .id,
                common.id
            );
            assert!(
                workspace
                    .package_errors()
                    .iter()
                    .any(|error| error.package_id == owner.id
                        && error.kind == PackageErrorKind::Dependency
                        && error.path.as_ref() == Some(&file)
                        && error.line == Some(5))
            );
        }
    }

    #[test]
    fn groups_selected_go_work_members_without_loading_unselected_siblings() {
        let project = TestProject::new();
        project.write("go.work", "go 1.22\n\nuse (\n./one\n./two\n./three\n)\n");
        for name in ["one", "two", "three"] {
            project.write(
                &format!("{name}/go.mod"),
                &format!("module example.test/{name}\n\ngo 1.22\n"),
            );
            project.write(&format!("{name}/pkg.go"), &format!("package {name}\n"));
        }
        let folders = [project.root.join("one"), project.root.join("two")];
        project.write("one/pkg.go", "package one\nimport _ \"example.test/two\"\n");
        project.write(
            "two/pkg.go",
            "package two\nimport _ \"example.test/two/missing\"\n",
        );
        assert_eq!(folder_groups(&folders).unwrap(), [folders.to_vec()]);
        let workspace = Workspace::load_roots(&folders, false).unwrap();
        assert_eq!(
            workspace.module_paths(),
            ["example.test/one", "example.test/two"]
        );
        assert!(workspace.package("example.test/three").is_none());
        let error = workspace
            .package_errors()
            .iter()
            .find(|error| error.package_id == "example.test/one")
            .unwrap();
        let path = error.path.as_deref().unwrap();
        let owner = workspace.package_of_file(path).unwrap();
        assert_eq!(owner.id, "example.test/two");
        assert_eq!(
            crate::protocol::path_uri(path).unwrap(),
            crate::protocol::path_uri(&owner.files[0]).unwrap()
        );

        let overlapping = [project.root.clone(), folders[0].clone()];
        assert_eq!(list_roots_for_folders(&overlapping).unwrap().len(), 3);
        let workspace = Workspace::load_roots(&overlapping, false).unwrap();
        assert_eq!(workspace.roots().len(), 3);
        assert_eq!(workspace.navigation_packages().count(), 3);
    }
}
