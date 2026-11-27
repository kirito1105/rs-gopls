use super::super::*;
use super::{
    interface_probe::{Arity, InterfaceSpellings, interface_spellings},
    method_sets::MethodShape,
};

#[derive(PartialEq, Eq, Hash)]
pub(super) enum PackageProbeKey {
    Method(String),
    InterfaceMethod(String),
    Interface(Vec<MethodShape>),
}

#[derive(Default)]
pub(in crate::semantic) struct InterfaceSources {
    files: HashMap<PathBuf, InterfaceSource>,
    packages: HashMap<String, PackageSpellings>,
    by_method: HashMap<String, HashSet<String>>,
    uncertain: HashSet<String>,
    owners: HashMap<PathBuf, Vec<String>>,
    directories: HashMap<PathBuf, Vec<String>>,
    pending: Option<VecDeque<String>>,
    partial: PackageSpellings,
    #[cfg(test)]
    pub(super) file_probes: usize,
}

struct InterfaceSource {
    fingerprint: u64,
    spellings: Option<InterfaceSpellings>,
}

#[derive(Default)]
struct PackageSpellings {
    files: Vec<PathBuf>,
    methods: HashMap<String, HashSet<Option<Arity>>>,
    qualifiers: HashSet<String>,
    import_paths: HashSet<String>,
    uncertain: bool,
    load_error: bool,
}

impl PackageSpellings {
    fn probe(
        &mut self,
        file: &Path,
        key: PathBuf,
        sources: &mut HashMap<PathBuf, InterfaceSource>,
        indexes: &IndexReader<'_>,
    ) {
        self.files.push(file.to_owned());
        let disk = indexes.disk_index.get(&key);
        let source = match sources.entry(key) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => fs::read_to_string(file).ok().map(|text| {
                entry.insert(InterfaceSource {
                    fingerprint: text_fingerprint(&text),
                    spellings: interface_spellings(&text),
                })
            }),
        };
        // Exact resolution can still use the indexed snapshot until a watch event.
        let spellings = source
            .filter(|source| disk.is_none_or(|index| index.fingerprint == source.fingerprint))
            .and_then(|source| source.spellings.as_ref());
        if let Some(spellings) = spellings {
            self.qualifiers.extend(spellings.qualifiers.iter().cloned());
            self.import_paths
                .extend(spellings.import_paths.iter().cloned());
            for (name, arity) in &spellings.methods {
                self.methods.entry(name.clone()).or_default().insert(*arity);
            }
        } else {
            self.uncertain = true;
        }
    }
}

impl InterfaceSources {
    pub(in crate::semantic) fn invalidate(&mut self, key: &Path) {
        self.files.remove(key);
        for id in self.owners.get(key).cloned().unwrap_or_default() {
            let probed = self.forget(&id);
            let pending = self.pending.as_mut().expect("installed package graph");
            if pending.front() == Some(&id) {
                self.partial = PackageSpellings::default();
            } else if probed {
                pending.push_back(id);
            }
        }
    }

    fn forget(&mut self, id: &str) -> bool {
        let Some(summary) = self.packages.remove(id) else {
            return false;
        };
        self.uncertain.remove(id);
        for name in summary.methods.keys() {
            if let Some(packages) = self.by_method.get_mut(name) {
                packages.remove(id);
                if packages.is_empty() {
                    self.by_method.remove(name);
                }
            }
        }
        true
    }

    pub(in crate::semantic) fn retain_workspace(&mut self, workspace: &Workspace) {
        if self.pending.is_none() {
            return;
        }
        self.owners.clear();
        for package in workspace.navigation_packages() {
            for file in &package.files {
                self.owners
                    .entry(path_key(file))
                    .or_default()
                    .push(package.id.clone());
            }
        }
        self.files.retain(|key, _| self.owners.contains_key(key));
        self.queue_packages(workspace);
    }

    fn queue_packages(&mut self, workspace: &Workspace) {
        let stale: Vec<_> = self
            .packages
            .iter()
            .filter(|(id, summary)| {
                workspace.package(id).is_none_or(|package| {
                    package.files != summary.files || package.load_error != summary.load_error
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.forget(&id);
        }
        self.directories.clear();
        self.partial = PackageSpellings::default();
        let mut pending = VecDeque::new();
        for package in workspace.navigation_packages() {
            if !self.packages.contains_key(&package.id) {
                pending.push_back(package.id.clone());
            }
            self.directories
                .entry(package.directory_and_name().0.to_owned())
                .or_default()
                .push(package.id.clone());
        }
        self.pending = Some(pending);
    }

    fn advance(&mut self, resolver: &Resolver<'_, '_>) -> bool {
        if self.pending.is_none() {
            self.queue_packages(resolver.workspace);
        }
        let started = Instant::now();
        for _ in 0..32 {
            let pending = self.pending.as_mut().expect("installed package graph");
            let Some(id) = pending.front() else {
                return true;
            };
            let package = resolver.workspace.package(id).expect("active package");
            if let Some(file) = package.files.get(self.partial.files.len()) {
                let key = path_key(file);
                let owners = self.owners.entry(key.clone()).or_default();
                if !owners.contains(id) {
                    owners.push(id.clone());
                }
                self.partial
                    .probe(file, key, &mut self.files, &resolver.indexes);
                #[cfg(test)]
                {
                    self.file_probes += 1;
                }
            } else {
                let id = pending.pop_front().expect("completed package probe");
                let mut summary = std::mem::take(&mut self.partial);
                summary.load_error = package.load_error;
                for name in summary.methods.keys() {
                    self.by_method
                        .entry(name.clone())
                        .or_default()
                        .insert(id.clone());
                }
                if summary.uncertain || summary.load_error {
                    self.uncertain.insert(id.clone());
                }
                self.packages.insert(id, summary);
            }
            if started.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        false
    }

    fn candidates(&self, resolver: &Resolver<'_, '_>, probe: &PackageProbeKey) -> HashSet<String> {
        let mut eligible = HashSet::new();
        let mut include = |id: &str| {
            include_importers(resolver.workspace, &mut eligible, id, |base, importer| {
                let summary = &self.packages[&importer.id];
                summary.qualifiers.contains(&base.name)
                    || summary.import_paths.iter().any(|path| {
                        resolver
                            .workspace
                            .resolve_import(importer, path)
                            .is_some_and(|target| target.id == base.id)
                    })
            });
        };
        for id in &self.uncertain {
            include(id);
        }
        // Overlay imports can be absent from the disk graph, including in new draft files.
        for key in resolver.indexes.documents.keys() {
            for id in self.owners.get(key).into_iter().flatten() {
                include(id);
            }
        }
        for &(directory, name) in resolver.indexes.drafts.keys() {
            for id in self.directories.get(directory).into_iter().flatten() {
                if resolver
                    .workspace
                    .package(id)
                    .is_some_and(|p| p.name == name)
                {
                    include(id);
                }
            }
        }
        let mut matching = |name: &str, arity: Option<Arity>| {
            for id in self.by_method.get(name).into_iter().flatten() {
                let shapes = &self.packages[id].methods[name];
                if arity.is_none() || shapes.contains(&None) || shapes.contains(&arity) {
                    include(id);
                }
            }
        };
        match probe {
            PackageProbeKey::Interface(shapes) => {
                for shape in shapes {
                    matching(
                        &shape.name,
                        Some((shape.parameters, shape.results, shape.variadic)),
                    );
                }
            }
            PackageProbeKey::InterfaceMethod(name) => matching(name, None),
            PackageProbeKey::Method(_) => unreachable!("forward search uses receiver spellings"),
        }
        eligible
    }
}

pub(super) struct CandidatePackages {
    pub(super) key: PackageProbeKey,
    pub(super) eligible: HashSet<String>,
    pending: Vec<String>,
    current: Option<(String, Vec<PathBuf>)>,
    files: HashMap<PathBuf, bool>,
}

impl CandidatePackages {
    pub(super) fn new(key: PackageProbeKey, workspace: &Workspace) -> Self {
        let pending = if matches!(key, PackageProbeKey::Method(_)) {
            workspace
                .navigation_packages()
                .map(|p| p.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        Self {
            key,
            eligible: HashSet::new(),
            pending,
            current: None,
            files: HashMap::new(),
        }
    }

    pub(super) fn advance(
        &mut self,
        resolver: &Resolver<'_, '_>,
        sources: &mut InterfaceSources,
    ) -> bool {
        let PackageProbeKey::Method(method) = &self.key else {
            if !sources.advance(resolver) {
                return false;
            }
            self.eligible = sources.candidates(resolver, &self.key);
            return true;
        };
        let started = Instant::now();
        for _ in 0..32 {
            if self.current.is_none() {
                let Some(id) = self.pending.pop() else {
                    return true;
                };
                let Some(package) = resolver.workspace.package(&id) else {
                    continue;
                };
                if self.eligible.contains(&id) {
                    continue;
                }
                if package.load_error {
                    include_importers(resolver.workspace, &mut self.eligible, &id, |_, _| true);
                    continue;
                }
                self.current = Some((id, resolver.indexes.files(package).into_owned()));
            }

            let (id, files) = self
                .current
                .as_mut()
                .expect("candidate package was initialized");
            if let Some(file) = files.pop() {
                let key = path_key(&file);
                let may_declare = *self.files.entry(key.clone()).or_insert_with(|| {
                    resolver.indexes.documents.contains_key(&key)
                        || fs::read_to_string(&file).map_or(true, |text| {
                            text.contains(method)
                                || resolver.indexes.disk_index.get(&key).is_some_and(|index| {
                                    index.fingerprint != text_fingerprint(&text)
                                })
                        })
                });
                if may_declare {
                    include_importers(resolver.workspace, &mut self.eligible, id, |_, _| true);
                    self.current = None;
                }
            } else {
                self.current = None;
            }
            if started.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        self.current.is_none() && self.pending.is_empty()
    }
}

fn include_importers(
    workspace: &Workspace,
    eligible: &mut HashSet<String>,
    id: &str,
    may_inherit: impl Fn(&Package, &Package) -> bool,
) {
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        if eligible.insert(id.to_owned())
            && let Some(package) = workspace.package(id)
        {
            pending.extend(
                workspace
                    .direct_importers(package)
                    .filter(|p| may_inherit(package, p))
                    .map(|p| p.id.as_str()),
            );
        }
    }
}
