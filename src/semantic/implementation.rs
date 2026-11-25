use super::*;

mod method_sets;

use method_sets::{InterfaceMethods, MethodKey};

type Location = (PathBuf, Range);

pub struct ImplementationSearch {
    required: InterfaceMethods,
    anchor: String,
    selected: Option<MethodKey>,
    declaration: Location,
    packages: VecDeque<String>,
    current: Option<PackageSearch>,
    matches: Vec<Location>,
}

struct PackageSearch {
    id: String,
    files: Vec<PathBuf>,
    direct_owners: HashSet<String>,
    candidates: Vec<Candidate>,
}

struct Candidate {
    name: String,
    location: Location,
    inherits: bool,
}

impl ImplementationSearch {
    pub(super) fn into_matches(self) -> Vec<Location> {
        let mut locations = self.matches;
        locations.sort_by_cached_key(|(file, range)| (path_key(file), range.start, range.end));
        locations.dedup_by(|left, right| same_location(left, right));
        locations
    }
}

struct Subject<'w> {
    owner: TypeIdentity<'w>,
    method: Option<MethodKey>,
    declaration: Location,
}

impl Engine {
    pub fn implementations(
        &mut self,
        path: &Path,
        position: Position,
    ) -> Result<QueryProgress<Vec<Location>>> {
        if path.extension().is_none_or(|extension| extension != "go") {
            return Ok(QueryProgress::Ready(Vec::new()));
        }
        self.include_tests = true;
        let load = self.workspace_load();
        let Some(workspace) = &self.workspace else {
            return Ok(QueryProgress::Pending(load));
        };
        if !workspace.includes_tests() || !matches!(self.graph_changes, GraphChanges::Clean) {
            return Ok(QueryProgress::Pending(load));
        }

        let indexes = IndexReader {
            parser: &mut self.parser,
            documents: &self.documents,
            disk_index: &mut self.disk_index,
            drafts: draft_files(workspace, &self.documents, &self.new_documents),
        };
        let mut resolver = Resolver { workspace, indexes };
        let Some(source) = resolver.indexes.reference_source(path)? else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };
        let source = source.as_ref();
        let Some(from) = active_package(workspace, source, &self.new_documents) else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };
        let Some(subject) = resolver.implementation_subject(from, source, position, &self.root)?
        else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };
        let Some(required) = resolver.interface_methods(&subject.owner)? else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };

        let mut matches = Vec::new();
        let same_file = path_key(path) == path_key(&subject.declaration.0);
        let range = subject.declaration.1;
        if !same_file || position < range.start || position >= range.end {
            matches.push(subject.declaration.clone());
        }
        let Some(anchor) = required.method_names().min().map(str::to_owned) else {
            return Ok(QueryProgress::Ready(matches));
        };

        let package = subject.owner.package;
        let package_only = required.method_names().any(|name| !is_exported(name));
        let packages = workspace
            .navigation_packages()
            .filter(|candidate| {
                !package_only || candidate.directory_and_name() == package.directory_and_name()
            })
            .map(|candidate| candidate.id.clone())
            .collect();
        Ok(QueryProgress::Searching(Search::Implementations(
            ImplementationSearch {
                required,
                anchor,
                selected: subject.method,
                declaration: subject.declaration,
                packages,
                current: None,
                matches,
            },
        )))
    }

    pub(super) fn advance_implementation_search(
        &mut self,
        search: &mut ImplementationSearch,
    ) -> Result<bool> {
        let workspace = self
            .workspace
            .as_ref()
            .context("implementation search needs a workspace")?;
        ensure!(
            matches!(self.graph_changes, GraphChanges::Clean),
            "workspace changed during implementation search"
        );
        let indexes = IndexReader {
            parser: &mut self.parser,
            documents: &self.documents,
            disk_index: &mut self.disk_index,
            drafts: draft_files(workspace, &self.documents, &self.new_documents),
        };
        let mut resolver = Resolver { workspace, indexes };
        let started = Instant::now();
        for _ in 0..32 {
            if search.current.is_none() {
                let Some(id) = search.packages.pop_front() else {
                    return Ok(true);
                };
                let Some(package) = workspace.package(&id) else {
                    continue;
                };
                search.current = Some(PackageSearch {
                    id,
                    files: resolver.indexes.files(package).into_owned(),
                    direct_owners: HashSet::new(),
                    candidates: Vec::new(),
                });
            }
            let current = search.current.as_mut().expect("initialized package search");
            if let Some(file) = current.files.pop() {
                resolver.collect_implementation_candidates(current, &file, &search.anchor)?;
            } else if let Some(candidate) = current.candidates.pop() {
                if (candidate.inherits || current.direct_owners.contains(&candidate.name))
                    && let Some(package) = workspace.package(&current.id)
                    && let Some(target) =
                        resolver.match_implementation(package, search, candidate)?
                {
                    search.matches.push(target);
                }
            } else {
                search.current = None;
            }
            if started.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        Ok(search.current.is_none() && search.packages.is_empty())
    }
}

impl<'w> Resolver<'w, '_> {
    fn implementation_subject(
        &mut self,
        from: &'w Package,
        source: &Source,
        position: Position,
        root: &Path,
    ) -> Result<Option<Subject<'w>>> {
        let Some(reference) = source.reference_at(position) else {
            return Ok(None);
        };
        let Some(declaration) = self.reference_location(from, source, reference, root)? else {
            return Ok(None);
        };
        let Some(origin) = self.indexes.reference_source(&declaration.0)? else {
            return Ok(None);
        };
        let origin = origin.as_ref();
        let same_package = self
            .indexes
            .files(from)
            .iter()
            .any(|path| path == &origin.path);
        let Some(package) = same_package
            .then_some(from)
            .or_else(|| active_package(self.workspace, origin, &HashSet::new()))
        else {
            return Ok(None);
        };
        let Some(name) = origin
            .reference_at(declaration.1.start)
            .map(ReferenceQuery::name)
        else {
            return Ok(None);
        };
        let Some(parent) = name.parent() else {
            return Ok(None);
        };

        let method = matches!(parent.kind(), "method_declaration" | "method_elem")
            .then(|| MethodKey::new(node_text(name, &origin.text), package));
        let owner = match parent.kind() {
            "method_declaration" => parent
                .child_by_field_name("receiver")
                .and_then(|receiver| receiver.named_child(0))
                .and_then(|parameter| parameter.child_by_field_name("type"))
                .and_then(|ty| symbols::type_ref(ty, &origin.text, &|node| origin.range(node)))
                .and_then(|ty| match ty.kind {
                    TypeRefKind::Named(named) => Some(TypeIdentity::named(package, &named.name)),
                    _ => None,
                }),
            "method_elem" => parent.parent().and_then(|interface| {
                let declaration = interface.parent().filter(|node| is_type_declaration(*node));
                if let Some(declaration) = declaration {
                    let name = declaration.child_by_field_name("name")?;
                    Some(TypeIdentity::named(package, node_text(name, &origin.text)))
                } else {
                    self.source_type(package, origin, interface)
                }
            }),
            "type_spec" | "type_alias" => {
                let ty = TypeIdentity::named(package, node_text(name, &origin.text));
                let mut use_node = reference.name();
                if let Some(parent) = use_node
                    .parent()
                    .filter(|node| node.kind() == "qualified_type")
                {
                    use_node = parent;
                }
                if let Some(parent) = use_node
                    .parent()
                    .filter(|node| node.kind() == "generic_type")
                {
                    self.source_type(from, source, parent).or(Some(ty))
                } else {
                    Some(ty)
                }
            }
            _ => None,
        };
        Ok(owner.map(|owner| Subject {
            owner,
            method,
            declaration,
        }))
    }

    fn collect_implementation_candidates(
        &mut self,
        package: &mut PackageSearch,
        file: &Path,
        anchor: &str,
    ) -> Result<()> {
        let index = match self.indexes.file_index(file) {
            Ok(index) => index,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        package
            .direct_owners
            .extend(index.symbols.member_owners(anchor).map(str::to_owned));
        for (name, details) in index.symbols.type_declarations() {
            if let Some(symbol) = index.symbols.declaration(name) {
                let named = |reference: &TypeRef| matches!(reference.kind, TypeRefKind::Named(_));
                let inherits = details.alias.as_ref().is_some_and(|alias| {
                    named(alias) || matches!(alias.kind, TypeRefKind::Inline(_))
                }) || details.underlying.as_ref().is_some_and(named)
                    || !details.embedded.is_empty();
                package.candidates.push(Candidate {
                    name: name.to_owned(),
                    location: (file.to_owned(), symbol.range),
                    inherits,
                });
            }
        }
        Ok(())
    }

    fn match_implementation(
        &mut self,
        package: &'w Package,
        search: &ImplementationSearch,
        candidate: Candidate,
    ) -> Result<Option<Location>> {
        if same_location(&candidate.location, &search.declaration) {
            return Ok(None);
        }
        let ty = TypeIdentity::named(package, &candidate.name);
        let Some(matched) =
            self.satisfies_interface(&search.required, &ty, search.selected.as_ref(), true)?
        else {
            return Ok(None);
        };
        let target = if search.selected.is_some() {
            matched.location
        } else {
            Some(candidate.location)
        };
        Ok(target.filter(|target| !same_location(target, &search.declaration)))
    }
}

fn same_location(left: &Location, right: &Location) -> bool {
    left.1 == right.1 && path_key(&left.0) == path_key(&right.0)
}
