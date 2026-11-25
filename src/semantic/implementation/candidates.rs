use super::super::*;
use super::{
    interface_probe::{InterfaceSpellings, interface_spellings},
    method_sets::MethodShape,
};

pub(super) enum PackageProbe {
    Method(String),
    Interface(Rc<HashSet<MethodShape>>),
}

#[derive(PartialEq, Eq, Hash)]
pub(super) enum PackageProbeKey {
    Method(String),
    Interface(Vec<MethodShape>),
}

impl PackageProbe {
    pub(super) fn key(&self) -> PackageProbeKey {
        match self {
            Self::Method(method) => PackageProbeKey::Method(method.clone()),
            Self::Interface(shapes) => {
                let mut shapes: Vec<_> = shapes.iter().cloned().collect();
                shapes.sort_unstable();
                PackageProbeKey::Interface(shapes)
            }
        }
    }

    fn matches_file(
        &self,
        file: &Path,
        key: &Path,
        indexes: &IndexReader<'_>,
        sources: &mut InterfaceSources,
    ) -> bool {
        // The import edge from an unsaved file may be absent from go list.
        if indexes.documents.contains_key(key) {
            return true;
        }
        match self {
            Self::Method(method) => fs::read_to_string(file).map_or(true, |text| {
                text.contains(method)
                    || indexes
                        .disk_index
                        .get(key)
                        .is_some_and(|index| index.fingerprint != text_fingerprint(&text))
            }),
            Self::Interface(methods) => sources.matches_file(file, key, indexes, methods),
        }
    }
}

#[derive(Default)]
pub(in crate::semantic) struct InterfaceSources {
    files: HashMap<PathBuf, InterfaceSource>,
}

struct InterfaceSource {
    fingerprint: u64,
    spellings: InterfaceSpellings,
}

impl InterfaceSources {
    pub(in crate::semantic) fn invalidate(&mut self, key: &Path) {
        self.files.remove(key);
    }

    pub(in crate::semantic) fn retain_workspace(&mut self, workspace: &Workspace) {
        if !self.files.is_empty() {
            let active: HashSet<_> = workspace
                .navigation_packages()
                .flat_map(|package| package.files.iter().map(|file| path_key(file)))
                .collect();
            self.files.retain(|key, _| active.contains(key));
        }
    }

    fn matches_file(
        &mut self,
        file: &Path,
        key: &Path,
        indexes: &IndexReader<'_>,
        methods: &HashSet<MethodShape>,
    ) -> bool {
        let source = match self.files.entry(key.to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let Ok(text) = fs::read_to_string(file) else {
                    return true;
                };
                entry.insert(InterfaceSource {
                    fingerprint: text_fingerprint(&text),
                    spellings: interface_spellings(&text),
                })
            }
        };
        // Exact resolution can still use the indexed snapshot until a watch event.
        if indexes
            .disk_index
            .get(key)
            .is_some_and(|index| index.fingerprint != source.fingerprint)
        {
            return true;
        }
        source.spellings.matches(|name, arity| {
            methods.iter().any(|method| {
                method.name == name
                    && arity.is_none_or(|arity| {
                        arity == (method.parameters, method.results, method.variadic)
                    })
            })
        })
    }
}

pub(super) struct CandidatePackages {
    probe: PackageProbe,
    pub(super) key: PackageProbeKey,
    pub(super) eligible: HashSet<String>,
    pending: Vec<String>,
    current: Option<(String, Vec<PathBuf>)>,
    files: HashMap<PathBuf, bool>,
}

impl CandidatePackages {
    pub(super) fn new(
        probe: PackageProbe,
        key: PackageProbeKey,
        packages: &VecDeque<String>,
    ) -> Self {
        Self {
            probe,
            key,
            eligible: HashSet::new(),
            pending: packages.iter().rev().cloned().collect(),
            current: None,
            files: HashMap::new(),
        }
    }

    pub(super) fn advance(
        &mut self,
        resolver: &Resolver<'_, '_>,
        sources: &mut InterfaceSources,
    ) -> bool {
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
                    self.include_importers(resolver.workspace, &id);
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
                    self.probe
                        .matches_file(&file, &key, &resolver.indexes, sources)
                });
                if may_declare {
                    let id = std::mem::take(id);
                    self.current = None;
                    self.include_importers(resolver.workspace, &id);
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

    fn include_importers(&mut self, workspace: &Workspace, id: &str) {
        // An alias or a promoted method may be declared in a dependency, so
        // spelling in the destination package alone is insufficient.
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            if self.eligible.insert(id.to_owned())
                && let Some(package) = workspace.package(id)
            {
                pending.extend(
                    workspace
                        .direct_importers(package)
                        .map(|importer| importer.id.as_str()),
                );
            }
        }
    }
}
