use super::super::*;

pub(super) struct CandidatePackages {
    pub(super) method: String,
    pub(super) eligible: HashSet<String>,
    pending: Vec<String>,
    current: Option<(String, Vec<PathBuf>)>,
    files: HashMap<PathBuf, bool>,
}

impl CandidatePackages {
    pub(super) fn new(method: &str, packages: &VecDeque<String>) -> Self {
        Self {
            method: method.to_owned(),
            eligible: HashSet::new(),
            pending: packages.iter().rev().cloned().collect(),
            current: None,
            files: HashMap::new(),
        }
    }

    pub(super) fn advance(&mut self, resolver: &Resolver<'_, '_>) -> bool {
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
                    // An overlay can introduce an import absent from go list. Including
                    // its package also keeps disk importers that can inherit its methods.
                    resolver.indexes.documents.contains_key(&key)
                        || fs::read_to_string(&file).map_or(true, |text| {
                            text.contains(&self.method)
                                || resolver.indexes.disk_index.get(&key).is_some_and(|index| {
                                    // Resolution can use the indexed snapshot until a watch event.
                                    index.fingerprint != text_fingerprint(&text)
                                })
                        })
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
