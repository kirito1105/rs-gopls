use super::*;

mod candidates;
mod interface_index;
mod interface_probe;
mod method_sets;

pub(super) use candidates::InterfaceSources;
use candidates::{CandidatePackages, PackageProbe, PackageProbeKey};
use interface_index::{IndexedInterface, InterfaceIndex};
use method_sets::{InterfaceMethods, MethodKey, MethodShape};

type Location = (PathBuf, Range);
type QueryKey = (PathBuf, Position);

#[derive(Default)]
pub(super) struct ImplementationCache {
    results: HashMap<QueryKey, Vec<Location>>,
    packages: HashMap<PackageProbeKey, HashSet<String>>,
    interfaces: HashMap<String, InterfaceIndex>,
}

impl ImplementationCache {
    pub(super) fn is_empty(&self) -> bool {
        self.results.is_empty() && self.packages.is_empty() && self.interfaces.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.results.clear();
        self.packages.clear();
        self.interfaces.clear();
    }

    fn remember(&mut self, key: QueryKey, result: Vec<Location>) {
        if self.results.len() >= 128 {
            self.results.clear();
        }
        self.results.insert(key, result);
    }

    fn remember_packages(&mut self, candidates: CandidatePackages) {
        if self.packages.len() >= 32 {
            self.packages.clear();
        }
        self.packages.insert(candidates.key, candidates.eligible);
    }

    pub(super) fn shift(&mut self, path: &Path, shift: LineShift) {
        if shift.old_end == shift.new_end {
            return;
        }
        // Method signatures in this index can contain declaration ranges.
        self.interfaces.clear();
        self.results = std::mem::take(&mut self.results)
            .into_iter()
            .filter_map(|((query_path, mut position), mut locations)| {
                if query_path == path {
                    position = shift.position(position)?;
                }
                for (file, range) in &mut locations {
                    if file == path || path_key(file) == path {
                        range.start = shift.position(range.start)?;
                        range.end = shift.position(range.end)?;
                    }
                }
                Some(((query_path, position), locations))
            })
            .collect();
    }
}

pub(super) struct LineShift {
    start: u32,
    old_end: u32,
    new_end: u32,
}

impl LineShift {
    pub(super) fn for_edit(
        tree: &Tree,
        text: &str,
        bytes: std::ops::Range<usize>,
        change: &TextDocumentContentChangeEvent,
        new_end: usize,
    ) -> Option<Self> {
        let range = change.range?;
        let root = tree.root_node();
        if range.start.character != 0
            || range.end.character != 0
            || root.has_error()
            || (bytes.start == 0 && text.starts_with('\u{feff}'))
            || !top_level_trivia(&text[bytes.clone()])
            || !top_level_trivia(&change.text)
        {
            return None;
        }
        let mut cursor = root.walk();
        let unaffected = root.named_children(&mut cursor).all(|node| {
            let content = node_text(node, text);
            if node.kind() == "comment" {
                if ordinary_comment(content) {
                    return true;
                }
                // Keep directives and block comments on the conservative invalidation path.
                return false;
            }
            node.end_byte() <= bytes.start || node.start_byte() >= bytes.end
        });
        unaffected.then_some(Self {
            start: range.start.line,
            old_end: range.end.line,
            new_end: u32::try_from(new_end).ok()?,
        })
    }

    fn position(&self, mut position: Position) -> Option<Position> {
        if position.line >= self.start {
            position.line = position
                .line
                .checked_sub(self.old_end)?
                .checked_add(self.new_end)?;
        }
        Some(position)
    }
}

fn top_level_trivia(text: &str) -> bool {
    (text.is_empty() || text.ends_with('\n'))
        && text.lines().all(|line| {
            let line = line.trim_matches([' ', '\t', '\r']);
            line.is_empty() || ordinary_comment(line)
        })
}

fn ordinary_comment(text: &str) -> bool {
    text.strip_prefix("//").is_some_and(|line| {
        !line.contains('\n')
            && !["go:", "+build", "line "]
                .iter()
                .any(|prefix| line.trim_start().starts_with(prefix))
    })
}

pub struct ImplementationSearch {
    key: QueryKey,
    direction: Direction,
    selected: Option<MethodKey>,
    declaration: Location,
    packages: VecDeque<String>,
    package_filter: Option<CandidatePackages>,
    current: Option<PackageSearch>,
    matches: Vec<Location>,
}

enum Direction {
    Implementations {
        required: InterfaceMethods,
        anchor: String,
    },
    Interfaces {
        package_id: String,
        name: String,
        shapes: Option<Rc<HashSet<MethodShape>>>,
    },
}

impl Direction {
    fn receiver_shapes(&self) -> Option<&HashSet<MethodShape>> {
        match self {
            Self::Interfaces { shapes, .. } => shapes.as_deref(),
            Self::Implementations { .. } => None,
        }
    }
}

struct PackageSearch {
    id: String,
    files: Vec<PathBuf>,
    direct_owners: HashSet<String>,
    candidates: Vec<Candidate>,
    interface_index: Option<InterfaceIndex>,
    indexed_interfaces: Vec<Rc<IndexedInterface>>,
}

struct Candidate {
    name: String,
    location: Location,
    inherits: bool,
    may_be_interface: bool,
}

impl ImplementationSearch {
    pub(super) fn into_matches(self) -> Vec<Location> {
        self.matches
    }

    fn complete(&mut self, cache: &mut ImplementationCache) {
        self.matches
            .sort_by_cached_key(|(file, range)| (path_key(file), range.start, range.end));
        self.matches
            .dedup_by(|left, right| same_location(left, right));
        cache.remember(self.key.clone(), self.matches.clone());
    }

    fn indexes_interfaces(&self) -> bool {
        self.selected.is_none() && matches!(self.direction, Direction::Interfaces { .. })
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
        let key = (path_key(path), position);
        if let Some(cached) = self.implementation_cache.results.get(&key) {
            return Ok(QueryProgress::Ready(cached.clone()));
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
        let mut matches = Vec::new();
        let direction = if let Some(required) = resolver.interface_methods(&subject.owner)? {
            let same_file = path_key(path) == path_key(&subject.declaration.0);
            let range = subject.declaration.1;
            if !same_file || position < range.start || position >= range.end {
                matches.push(subject.declaration.clone());
            }
            let Some(anchor) = required
                .method_names()
                .max_by_key(|name| (name.len(), *name))
                .map(str::to_owned)
            else {
                return Ok(QueryProgress::Ready(matches));
            };
            Direction::Implementations { required, anchor }
        } else if let TypeKind::Named { name, .. } = &subject.owner.kind {
            let concrete = TypeIdentity::named(subject.owner.package, name);
            let shapes = if subject.method.is_none() {
                resolver
                    .concrete_method_shapes(&concrete)
                    .unwrap_or(None)
                    .map(Rc::new)
            } else {
                None
            };
            if shapes.as_ref().is_some_and(|shapes| shapes.is_empty()) {
                return Ok(QueryProgress::Ready(matches));
            }
            Direction::Interfaces {
                package_id: subject.owner.package.id.clone(),
                name: name.clone(),
                shapes,
            }
        } else {
            return Ok(QueryProgress::Ready(matches));
        };

        let package = subject.owner.package;
        let package_only = subject
            .method
            .as_ref()
            .is_some_and(|method| !method.is_exported());
        let mut packages: VecDeque<_> = workspace
            .navigation_packages()
            .filter(|candidate| {
                !package_only || candidate.directory_and_name() == package.directory_and_name()
            })
            .map(|candidate| candidate.id.clone())
            .collect();
        let filter_method = match &direction {
            Direction::Implementations { anchor, .. } => Some(anchor.as_str()),
            Direction::Interfaces { .. } => subject.method.as_ref().map(MethodKey::name),
        };
        // The predeclared error interface can supply Error outside the go list graph.
        let probe = if let Some(method) =
            filter_method.filter(|method| !package_only && *method != "Error")
        {
            Some(PackageProbe::Method(method.to_owned()))
        } else if let Direction::Interfaces {
            shapes: Some(shapes),
            ..
        } = &direction
        {
            (!shapes.iter().any(|shape| shape.name == "Error"))
                .then(|| PackageProbe::Interface(Rc::clone(shapes)))
        } else {
            None
        };
        let package_filter = probe.and_then(|probe| {
            let key = probe.key();
            if let Some(eligible) = self.implementation_cache.packages.get(&key) {
                packages.retain(|id| eligible.contains(id));
                None
            } else {
                Some(CandidatePackages::new(probe, key, &packages))
            }
        });
        Ok(QueryProgress::Searching(Search::Implementations(
            ImplementationSearch {
                key,
                direction,
                selected: subject.method,
                declaration: subject.declaration,
                packages,
                package_filter,
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
        if let Some(filter) = &mut search.package_filter {
            if filter.advance(&resolver, &mut self.implementation_sources) {
                search.packages.retain(|id| filter.eligible.contains(id));
                if let Some(filter) = search.package_filter.take() {
                    self.implementation_cache.remember_packages(filter);
                }
            }
            return Ok(false);
        }
        let started = Instant::now();
        let index_interfaces = search.indexes_interfaces();
        let receiver_shapes = search.direction.receiver_shapes();
        for _ in 0..32 {
            if search.current.is_none() {
                let Some(id) = search.packages.pop_front() else {
                    break;
                };
                let Some(package) = workspace.package(&id) else {
                    continue;
                };
                let existing = index_interfaces
                    .then(|| self.implementation_cache.interfaces.get(&id))
                    .flatten();
                search.current = Some(PackageSearch {
                    id,
                    files: if existing.is_some() {
                        Vec::new()
                    } else {
                        resolver.indexes.files(package).into_owned()
                    },
                    direct_owners: HashSet::new(),
                    candidates: Vec::new(),
                    interface_index: (index_interfaces && existing.is_none())
                        .then(InterfaceIndex::default),
                    indexed_interfaces: existing
                        .map_or_else(Vec::new, |index| index.candidates(receiver_shapes)),
                });
            }
            let current = search.current.as_mut().expect("initialized package search");
            if let Some(file) = current.files.pop() {
                resolver.collect_implementation_candidates(
                    current,
                    &file,
                    &search.direction,
                    index_interfaces,
                )?;
            } else if let Some(candidate) = current.candidates.pop() {
                let eligible = match search.direction {
                    Direction::Implementations { .. } => {
                        candidate.inherits || current.direct_owners.contains(&candidate.name)
                    }
                    Direction::Interfaces { .. } => candidate.may_be_interface,
                };
                if eligible && let Some(package) = workspace.package(&current.id) {
                    if let Some(index) = &mut current.interface_index {
                        let ty = TypeIdentity::named(package, &candidate.name);
                        if let Some(required) = resolver.interface_methods(&ty)? {
                            index.insert(candidate.location, required);
                        }
                    } else if let Some(target) =
                        resolver.match_implementation(package, search, candidate)?
                    {
                        search.matches.push(target);
                    }
                }
            } else if let Some(index) = current.interface_index.take() {
                current.indexed_interfaces = index.candidates(receiver_shapes);
                self.implementation_cache
                    .interfaces
                    .insert(current.id.clone(), index);
            } else if let Some(candidate) = current.indexed_interfaces.pop() {
                if let Some(package) = workspace.package(&current.id)
                    && let Some(target) = resolver.match_interface(
                        package,
                        search,
                        &candidate.methods,
                        &candidate.location,
                    )?
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
        let done = search.current.is_none() && search.packages.is_empty();
        if done {
            search.complete(&mut self.implementation_cache);
        }
        Ok(done)
    }
}

impl IndexReader<'_> {
    fn interface_candidate_index(&mut self, file: &Path) -> Result<Option<&FileIndex>> {
        let key = path_key(file);
        if let Some(source) = self.documents.get(&key) {
            return Ok(Some(source.declarations()));
        }
        let cached = match self.disk_index.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let text = fs::read_to_string(file)
                    .with_context(|| format!("reading {}", file.display()))?;
                // Candidate declarations require a type spec, including aliases.
                if !text.contains("type") {
                    return Ok(None);
                }
                let source = Source::parse(self.parser, file.to_owned(), text, None)?;
                entry.insert(DiskIndex::new(&source))
            }
        };
        Ok(Some(&cached.declarations))
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
        direction: &Direction,
        index_interfaces: bool,
    ) -> Result<()> {
        let index = if index_interfaces {
            self.indexes.interface_candidate_index(file)
        } else {
            self.indexes.file_index(file).map(Some)
        };
        let index = match index {
            Ok(Some(index)) => index,
            Ok(None) => return Ok(()),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if let Direction::Implementations { anchor, .. } = direction {
            package
                .direct_owners
                .extend(index.symbols.member_owners(anchor).map(str::to_owned));
        }
        for (name, details) in index.symbols.type_declarations() {
            if let Some(symbol) = index.symbols.declaration(name) {
                let named = |reference: &TypeRef| matches!(reference.kind, TypeRefKind::Named(_));
                let alias_or_base = details.alias.as_ref().is_some_and(|alias| {
                    named(alias) || matches!(alias.kind, TypeRefKind::Inline(_))
                }) || details.underlying.as_ref().is_some_and(named);
                package.candidates.push(Candidate {
                    name: name.to_owned(),
                    location: (file.to_owned(), symbol.range),
                    inherits: alias_or_base || !details.embedded.is_empty(),
                    may_be_interface: details.interface || alias_or_base,
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
        let selected = search.selected.as_ref();
        let target = match &search.direction {
            Direction::Implementations { required, .. } => {
                let Some(matched) = self.satisfies_interface(required, &ty, selected, true)? else {
                    return Ok(None);
                };
                if selected.is_some() {
                    matched.location
                } else {
                    Some(candidate.location)
                }
            }
            Direction::Interfaces { .. } => {
                let Some(required) = self.interface_methods(&ty)? else {
                    return Ok(None);
                };
                return self.match_interface(package, search, &required, &candidate.location);
            }
        };
        Ok(target.filter(|target| !same_location(target, &search.declaration)))
    }

    fn match_interface(
        &mut self,
        package: &'w Package,
        search: &ImplementationSearch,
        required: &InterfaceMethods,
        location: &Location,
    ) -> Result<Option<Location>> {
        let Direction::Interfaces {
            package_id, name, ..
        } = &search.direction
        else {
            return Ok(None);
        };
        let Some(owner) = self.workspace.package(package_id) else {
            return Ok(None);
        };
        if same_location(location, &search.declaration)
            || (owner.directory_and_name() != package.directory_and_name()
                && !required.method_names().any(is_exported))
        {
            return Ok(None);
        }
        let concrete = TypeIdentity::named(owner, name);
        let selected = search.selected.as_ref();
        if self
            .satisfies_interface(required, &concrete, selected, true)?
            .is_none()
        {
            return Ok(None);
        }
        let target = match selected {
            Some(method) => required.location(method),
            None => Some(location.clone()),
        };
        Ok(target.filter(|target| !same_location(target, &search.declaration)))
    }
}

fn same_location(left: &Location, right: &Location) -> bool {
    left.1 == right.1 && path_key(&left.0) == path_key(&right.0)
}
