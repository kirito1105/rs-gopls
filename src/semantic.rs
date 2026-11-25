use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use lsp_types::{FileChangeType, Position, Range, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::scope::{lexical_binding, receiver_type_parameters};
use crate::symbols::{
    self, Container, FileSymbols, FunctionSignature, GenericParameter, InlineType, Symbol,
    SymbolKind, TypeDetails, TypeRef, TypeRefKind,
};
use crate::workspace::{Package, Workspace, WorkspaceLoad, path_key};

struct FileIndex {
    symbols: FileSymbols,
    imports: Vec<Import>,
    package_signature: u64,
}

struct DiskIndex {
    declarations: FileIndex,
    fingerprint: u64,
}

impl DiskIndex {
    fn new(source: &Source) -> Self {
        Self {
            declarations: source.collect_declarations(),
            fingerprint: text_fingerprint(&source.text),
        }
    }

    fn still_current(&self, path: &Path) -> bool {
        fs::read_to_string(path).is_ok_and(|text| self.fingerprint == text_fingerprint(&text))
    }
}

fn text_fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Copy)]
enum ReferenceQuery<'tree> {
    Name(Node<'tree>),
    Member {
        receiver: Node<'tree>,
        name: Node<'tree>,
    },
    LiteralKey {
        value: Node<'tree>,
        name: Node<'tree>,
    },
}

pub enum DefinitionProgress {
    Ready(Option<(PathBuf, Range)>),
    Pending(WorkspaceLoad),
}

#[derive(Default)]
enum GraphChanges {
    #[default]
    Clean,
    Packages(HashSet<String>),
    Global,
}

impl GraphChanges {
    fn affects(&self, package: &Package) -> bool {
        match self {
            Self::Clean => false,
            Self::Packages(packages) => packages.contains(&package.id),
            Self::Global => true,
        }
    }

    fn include(&mut self, affected: Option<HashSet<String>>) {
        match (&mut *self, affected) {
            (changes, None) => *changes = Self::Global,
            (Self::Global, _) => {}
            (Self::Packages(packages), Some(affected)) => packages.extend(affected),
            (changes, Some(affected)) => *changes = Self::Packages(affected),
        }
    }
}

pub struct Engine {
    root: PathBuf,
    workspace: Option<Workspace>,
    graph_changes: GraphChanges,
    generation: u64,
    parser: Parser,
    documents: HashMap<PathBuf, Source>,
    new_documents: HashSet<PathBuf>,
    disk_index: HashMap<PathBuf, DiskIndex>,
    include_tests: bool,
}

impl Engine {
    pub fn new(root: PathBuf) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Go grammar must match the tree-sitter runtime");
        Self {
            root,
            workspace: None,
            graph_changes: GraphChanges::default(),
            generation: 0,
            parser,
            documents: HashMap::new(),
            new_documents: HashSet::new(),
            disk_index: HashMap::new(),
            include_tests: false,
        }
    }

    pub fn open(&mut self, path: PathBuf, text: String) -> Result<()> {
        if path.extension().is_some_and(|extension| extension == "go") {
            let key = path_key(&path);
            let new_file = !path.is_file();
            let source = Source::parse(&mut self.parser, path, text, None)?;
            if new_file {
                self.new_documents.insert(key.clone());
            } else {
                self.new_documents.remove(&key);
            }
            self.documents.insert(key, source);
        }
        Ok(())
    }

    pub fn edit(&mut self, path: &Path, edits: &[TextDocumentContentChangeEvent]) -> Result<()> {
        if let Some(source) = self.documents.get_mut(&path_key(path))
            && !edits.is_empty()
        {
            source.edit(&mut self.parser, edits)?;
        }
        Ok(())
    }

    pub fn close(&mut self, path: &Path) {
        let key = path_key(path);
        self.documents.remove(&key);
        self.new_documents.remove(&key);
        if self
            .disk_index
            .get(&key)
            .is_some_and(|index| !index.still_current(path))
        {
            self.disk_index.remove(&key);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.documents.contains_key(&path_key(path))
    }

    pub fn workspace_load(&self) -> WorkspaceLoad {
        WorkspaceLoad {
            generation: self.generation,
            include_tests: self.include_tests,
        }
    }

    pub fn install_workspace(&mut self, load: WorkspaceLoad, workspace: Workspace) -> bool {
        if load.generation != self.generation {
            return false;
        }
        self.workspace = Some(workspace);
        self.graph_changes = GraphChanges::Clean;
        true
    }

    fn invalidate(&mut self, affected: Option<HashSet<String>>) {
        self.generation += 1;
        self.graph_changes.include(affected);
    }

    pub fn file_changed(&mut self, path: &Path, change: FileChangeType) {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "go.mod" | "go.sum" | "go.work" | "go.work.sum"))
        {
            self.invalidate(None);
            return;
        }
        if path.extension().is_none_or(|extension| extension != "go") {
            return;
        }

        let key = path_key(path);
        if change == FileChangeType::CREATED && path.is_file() {
            self.new_documents.remove(&key);
        }
        let previous = self.disk_index.remove(&key);
        if change == FileChangeType::CHANGED
            && let Some(previous) = previous
            && let Ok(disk) = read_source(&mut self.parser, path)
        {
            let updated = DiskIndex::new(&disk);
            if previous.declarations.package_signature == updated.declarations.package_signature {
                self.disk_index.insert(key, updated);
                return;
            }
        }
        // A new, removed, or structurally changed source can change the set
        // of Go files selected by the current build or add an import.
        let affected = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.affected_packages(path));
        self.invalidate(affected);
    }

    pub fn definition(&mut self, path: &Path, position: Position) -> Result<DefinitionProgress> {
        let disk_source;
        let source = if let Some(source) = self.documents.get(&path_key(path)) {
            source
        } else if path.extension().is_some_and(|extension| extension == "go") {
            disk_source = read_source(&mut self.parser, path)?;
            &disk_source
        } else {
            return Ok(DefinitionProgress::Ready(None));
        };

        let Some(reference) = source.reference_at(position) else {
            return Ok(DefinitionProgress::Ready(None));
        };
        if let ReferenceQuery::Name(name) = reference {
            let declaration = if is_declaration_name(name) {
                Some(name)
            } else {
                lexical_binding(name, node_text(name, &source.text), &source.text)
                    .map(|binding| binding.name)
            };
            if let Some(declaration) = declaration {
                return Ok(DefinitionProgress::Ready(Some((
                    path.to_owned(),
                    source.range(declaration),
                ))));
            }
        }
        let test_source = path
            .file_name()
            .and_then(|filename| filename.to_str())
            .is_some_and(|filename| filename.ends_with("_test.go"));
        self.include_tests |= test_source;
        let load = self.workspace_load();
        let Some(workspace) = &self.workspace else {
            return Ok(DefinitionProgress::Pending(load));
        };
        if test_source && !workspace.includes_tests() {
            return Ok(DefinitionProgress::Pending(load));
        }
        let from = workspace.package_of_file(path).or_else(|| {
            self.new_documents
                .contains(&path_key(path))
                .then(|| draft_package(workspace, source))
                .flatten()
        });
        if !matches!(self.graph_changes, GraphChanges::Clean)
            && from.is_none_or(|package| {
                self.graph_changes.affects(package)
                    || source.imports.iter().any(|import| {
                        workspace
                            .resolve_import(package, &import.path)
                            .is_none_or(|dependency| self.graph_changes.affects(dependency))
                    })
            })
        {
            return Ok(DefinitionProgress::Pending(load));
        }
        let Some(from) = from else {
            return Ok(DefinitionProgress::Ready(None));
        };
        let mut drafts = HashMap::<_, Vec<_>>::new();
        for path in &self.new_documents {
            if let Some(document) = self.documents.get(path)
                && workspace.package_of_file(&document.path).is_none()
                && let Some(package) = draft_package(workspace, document)
            {
                drafts
                    .entry(package.directory_and_name())
                    .or_default()
                    .push(&document.path);
            }
        }
        for files in drafts.values_mut() {
            files.sort_unstable();
        }
        let indexes = IndexReader {
            parser: &mut self.parser,
            documents: &self.documents,
            disk_index: &mut self.disk_index,
            drafts,
        };

        let location = match reference {
            ReferenceQuery::Name(name) => {
                let mut resolver = Resolver { workspace, indexes };
                resolver.name_location(from, source, name, &self.root)
            }
            ReferenceQuery::Member { receiver, name } => {
                let mut resolver = Resolver { workspace, indexes };
                let member =
                    resolver.selector(from, source, receiver, node_text(name, &source.text), 0)?;
                Ok(member.map(ResolvedSymbol::location))
            }
            ReferenceQuery::LiteralKey { value, name } => {
                let mut resolver = Resolver { workspace, indexes };
                let Some(ty) = resolver.literal_type(from, source, value, 0)? else {
                    return Ok(DefinitionProgress::Ready(None));
                };
                if resolver.container(ty.clone())?.is_some() {
                    return resolver
                        .name_location(from, source, name, &self.root)
                        .map(DefinitionProgress::Ready);
                }
                let member = resolver.member(from, &ty, node_text(name, &source.text), false)?;
                let member =
                    member.filter(|found| matches!(found.symbol.kind, SymbolKind::Value(_)));
                Ok(member.map(ResolvedSymbol::location))
            }
        }?;
        Ok(DefinitionProgress::Ready(location))
    }
}

fn draft_package<'w>(workspace: &'w Workspace, source: &Source) -> Option<&'w Package> {
    let name = source.path.file_stem()?.to_str()?;
    let parts = name.split_once('_').map(|(_, suffixes)| suffixes);
    if name.starts_with(['_', '.'])
        || name.ends_with("_test")
        || parts.is_some_and(|suffixes| {
            suffixes.rsplit('_').next().is_some_and(|part| {
                matches!(
                    part,
                    "aix"
                        | "android"
                        | "darwin"
                        | "dragonfly"
                        | "freebsd"
                        | "hurd"
                        | "illumos"
                        | "ios"
                        | "js"
                        | "linux"
                        | "netbsd"
                        | "openbsd"
                        | "plan9"
                        | "solaris"
                        | "wasip1"
                        | "windows"
                        | "zos"
                        | "386"
                        | "amd64"
                        | "amd64p32"
                        | "arm"
                        | "arm64"
                        | "loong64"
                        | "mips"
                        | "mipsle"
                        | "mips64"
                        | "mips64le"
                        | "ppc64"
                        | "ppc64le"
                        | "riscv64"
                        | "s390x"
                        | "sparc64"
                        | "wasm"
                )
            })
        })
        || source.imports.iter().any(|import| import.path == "C")
    {
        return None;
    }
    let mut cursor = source.tree.root_node().walk();
    for node in source.tree.root_node().named_children(&mut cursor) {
        if node.kind() == "package_clause" {
            let name = node.named_child(0)?;
            return workspace.package_of_draft(&source.path, node_text(name, &source.text));
        }
        if node.kind() == "comment"
            && node_text(node, &source.text)
                .strip_prefix("//")
                .is_some_and(|text| {
                    matches!(text.split_whitespace().next(), Some("go:build" | "+build"))
                })
        {
            return None;
        }
    }
    None
}

struct IndexReader<'a> {
    parser: &'a mut Parser,
    documents: &'a HashMap<PathBuf, Source>,
    disk_index: &'a mut HashMap<PathBuf, DiskIndex>,
    drafts: HashMap<(&'a Path, &'a str), Vec<&'a PathBuf>>,
}

impl IndexReader<'_> {
    fn files<'p>(&self, package: &'p Package) -> Cow<'p, [PathBuf]> {
        match self.drafts.get(&package.directory_and_name()) {
            None => Cow::Borrowed(&package.files),
            Some(files) => Cow::Owned(
                package
                    .files
                    .iter()
                    .chain(files.iter().copied())
                    .cloned()
                    .collect(),
            ),
        }
    }

    fn file_index(&mut self, file: &Path) -> Result<&FileIndex> {
        let key = path_key(file);
        if let Some(source) = self.documents.get(&key) {
            return Ok(source.declarations());
        }
        let cached = match self.disk_index.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let source = read_source(self.parser, file)?;
                entry.insert(DiskIndex::new(&source))
            }
        };
        Ok(&cached.declarations)
    }

    fn find(
        &mut self,
        package: &Package,
        lookup: impl Fn(&FileIndex) -> Option<Range>,
    ) -> Result<Option<(PathBuf, Range)>> {
        for file in self.files(package).iter() {
            if let Some(range) = lookup(self.file_index(file)?) {
                return Ok(Some((file.clone(), range)));
            }
        }
        Ok(None)
    }

    fn find_dot_import(
        &mut self,
        workspace: &Workspace,
        from: &Package,
        imports: &[Import],
        name: &str,
    ) -> Result<Option<(PathBuf, Range)>> {
        let mut found = None;
        for import in imports
            .iter()
            .filter(|import| import.alias.as_deref() == Some("."))
        {
            if let Some(package) = workspace.resolve_import(from, &import.path)
                && let Some(location) = self.find(package, |index| {
                    index.symbols.declaration(name).map(|symbol| symbol.range)
                })?
            {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(location);
            }
        }
        Ok(found)
    }
}

const MAX_TYPE_DEPTH: usize = 32;

#[derive(Clone)]
struct TypeIdentity<'w> {
    package: &'w Package,
    kind: TypeKind<'w>,
    pointers: u8,
    addressable: bool,
    type_expression: bool,
}

#[derive(Clone)]
enum TypeKind<'w> {
    Named {
        name: String,
        arguments: Vec<Option<TypeIdentity<'w>>>,
    },
    Container(Box<Container<TypeIdentity<'w>>>),
    Function {
        file: PathBuf,
        signature: Arc<FunctionSignature>,
        substitutions: Substitutions<'w>,
    },
    Parameter(usize),
    Inline {
        file: PathBuf,
        shape: Arc<InlineType>,
        substitutions: Substitutions<'w>,
    },
}

#[derive(Clone, Eq, PartialEq)]
enum TypeKey {
    Named(*const Package, String),
    Inline(PathBuf, Position),
}

impl<'w> TypeIdentity<'w> {
    fn new(package: &'w Package, kind: TypeKind<'w>, pointers: u8) -> Self {
        Self {
            package,
            kind,
            pointers,
            addressable: false,
            type_expression: false,
        }
    }

    fn named(package: &'w Package, name: &str) -> Self {
        Self::new(
            package,
            TypeKind::Named {
                name: name.to_owned(),
                arguments: Vec::new(),
            },
            0,
        )
    }

    fn key(&self) -> Option<TypeKey> {
        match &self.kind {
            TypeKind::Named { name, .. } => Some(TypeKey::Named(
                std::ptr::from_ref(self.package),
                name.clone(),
            )),
            TypeKind::Inline { file, shape, .. } => {
                Some(TypeKey::Inline(file.clone(), shape.origin.start))
            }
            TypeKind::Container(_) | TypeKind::Function { .. } | TypeKind::Parameter(_) => None,
        }
    }

    fn project_properties(&mut self, from: &Self) {
        self.pointers = self.pointers.saturating_add(from.pointers);
        self.addressable = from.addressable;
        self.type_expression = from.type_expression;
    }
}

struct ResolvedSymbol<'w> {
    package: &'w Package,
    file: PathBuf,
    symbol: Symbol,
    receiver: Option<TypeIdentity<'w>>,
}

impl ResolvedSymbol<'_> {
    fn location(self) -> (PathBuf, Range) {
        (self.file, self.symbol.range)
    }
}

type Substitutions<'w> = Vec<(String, Option<TypeIdentity<'w>>)>;

struct Resolver<'w, 'idx> {
    workspace: &'w Workspace,
    indexes: IndexReader<'idx>,
}

impl<'w> Resolver<'w, '_> {
    fn name_location(
        &mut self,
        from: &'w Package,
        source: &Source,
        name: Node<'_>,
        root: &Path,
    ) -> Result<Option<(PathBuf, Range)>> {
        let identifier = node_text(name, &source.text);
        if let Some(binding) = lexical_binding(name, identifier, &source.text) {
            return Ok(Some((source.path.clone(), source.range(binding.name))));
        }
        if let Some((import, _)) = source.imported_package(self.workspace, from, identifier) {
            return Ok(Some((source.path.clone(), import.binding)));
        }
        if let Some(location) = self.indexes.find(from, |index| {
            index
                .symbols
                .declaration(identifier)
                .map(|symbol| symbol.range)
        })? {
            return Ok(Some(location));
        }
        if is_exported(identifier) {
            return self
                .indexes
                .find_dot_import(self.workspace, from, &source.imports, identifier);
        }
        if is_predeclared(identifier) {
            let file = self.workspace.builtin_source(root)?;
            let definition = self
                .indexes
                .file_index(file)?
                .symbols
                .declaration(identifier);
            return Ok(definition.map(|symbol| (file.to_owned(), symbol.range)));
        }
        Ok(None)
    }

    fn literal_type(
        &mut self,
        from: &'w Package,
        source: &Source,
        value: Node<'_>,
        depth: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        if depth >= MAX_TYPE_DEPTH {
            return Ok(None);
        }
        let Some(parent) = value.parent() else {
            return Ok(None);
        };
        if parent.kind() == "composite_literal" {
            return Ok(parent
                .child_by_field_name("type")
                .and_then(|ty| self.source_type(from, source, ty)));
        }
        let Some(container_element) = parent.parent() else {
            return Ok(None);
        };
        let (surrounding, map_key) = if container_element.kind() == "keyed_element" {
            (
                container_element.parent(),
                container_element.child_by_field_name("key") == Some(parent),
            )
        } else {
            (Some(container_element), false)
        };
        let Some(surrounding) = surrounding else {
            return Ok(None);
        };
        if surrounding.kind() != "literal_value" {
            return Ok(None);
        }
        let Some(ty) = self.literal_type(from, source, surrounding, depth + 1)? else {
            return Ok(None);
        };
        Ok(match self.container(ty)? {
            Some((Container::Slice(ty) | Container::Array { element: ty, .. }, 0)) => Some(ty),
            Some((Container::Map { key, value }, 0)) => Some(if map_key { key } else { value }),
            _ => None,
        })
    }

    fn selector(
        &mut self,
        from: &'w Package,
        source: &Source,
        receiver: Node<'_>,
        name: &str,
        depth: usize,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        if matches!(receiver.kind(), "identifier" | "package_identifier") {
            let qualifier = node_text(receiver, &source.text);
            if lexical_binding(receiver, qualifier, &source.text).is_none()
                && let Some((_, package)) = source.imported_package(self.workspace, from, qualifier)
            {
                return if is_exported(name) {
                    self.find_symbol(package, name, None)
                } else {
                    Ok(None)
                };
            }
        }

        let Some(ty) = self.infer_type(from, source, receiver, depth + 1, 0)? else {
            return Ok(None);
        };
        self.member(from, &ty, name, true)
    }

    fn member(
        &mut self,
        from: &Package,
        ty: &TypeIdentity<'w>,
        name: &str,
        promote: bool,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        if ty.pointers > 1 {
            return Ok(None);
        }
        let mut frontier: Vec<(TypeIdentity<'w>, Vec<TypeKey>)> = vec![(ty.clone(), Vec::new())];

        // A unique match at the shallowest embedding depth wins. Distinct
        // paths to the same declaration are still ambiguous for struct fields.
        for _ in 0..MAX_TYPE_DEPTH {
            let mut next = Vec::new();
            let mut found = None;
            let mut blocked = false;
            for (mut owner, mut visited) in frontier {
                let mut inherited = false;
                while let Some(key) = owner.key() {
                    if owner.pointers > 1 || visited.contains(&key) {
                        break;
                    }
                    visited.push(key);

                    if (is_exported(name) || std::ptr::eq(from, owner.package))
                        && let Some(mut symbol) = self.direct_member(&owner, name, inherited)?
                    {
                        if found.is_some() || blocked {
                            return Ok(None);
                        }
                        let method = matches!(symbol.symbol.kind, SymbolKind::Function(_));
                        blocked = (owner.type_expression && !method)
                            || (symbol.symbol.pointer_receiver
                                && owner.pointers == 0
                                && !owner.addressable);
                        if !blocked {
                            symbol.receiver = Some(owner);
                            found = Some(symbol);
                        }
                        break;
                    }

                    let Some((file, details, substitutions)) = self.type_context(&owner)? else {
                        break;
                    };
                    let index = self.indexes.file_index(&file)?;
                    if let Some(base) = details.alias.or_else(|| {
                        inherited = true;
                        details.underlying
                    }) {
                        let Some(mut target) = materialize_type(
                            self.workspace,
                            owner.package,
                            &file,
                            &index.imports,
                            base,
                            &substitutions,
                        ) else {
                            break;
                        };
                        target.project_properties(&owner);
                        owner = target;
                        continue;
                    }
                    if promote && found.is_none() && !blocked {
                        for embedded in details.embedded {
                            let Some(mut target) = materialize_type(
                                self.workspace,
                                owner.package,
                                &file,
                                &index.imports,
                                embedded,
                                &substitutions,
                            ) else {
                                continue;
                            };
                            target.addressable = owner.addressable && !owner.type_expression;
                            target.pointers = target.pointers.max(owner.pointers);
                            target.type_expression = owner.type_expression;
                            next.push((target, visited.clone()));
                        }
                    }
                    break;
                }
            }
            if blocked || found.is_some() || next.is_empty() {
                return Ok(found);
            }
            frontier = next;
        }
        Ok(None)
    }

    fn direct_member(
        &mut self,
        ty: &TypeIdentity<'w>,
        name: &str,
        inherited: bool,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        match &ty.kind {
            TypeKind::Named { name: owner, .. } => {
                let member = self.find_symbol(ty.package, name, Some(owner))?;
                if inherited
                    && member
                        .as_ref()
                        .is_some_and(|member| matches!(member.symbol.kind, SymbolKind::Function(_)))
                    && !self
                        .type_info(ty.package, owner)?
                        .is_some_and(|(_, details)| details.interface)
                {
                    return Ok(None);
                }
                Ok(member)
            }
            TypeKind::Inline { file, shape, .. } => {
                Ok(shape.members.get(name).map(|symbol| ResolvedSymbol {
                    package: ty.package,
                    file: file.clone(),
                    symbol: symbol.clone(),
                    receiver: None,
                }))
            }
            TypeKind::Container(_) | TypeKind::Function { .. } | TypeKind::Parameter(_) => Ok(None),
        }
    }

    fn type_context(
        &mut self,
        ty: &TypeIdentity<'w>,
    ) -> Result<Option<(PathBuf, TypeDetails, Substitutions<'w>)>> {
        match &ty.kind {
            TypeKind::Named { name, .. } => {
                Ok(self.type_info(ty.package, name)?.map(|(file, details)| {
                    let substitutions = type_substitutions(ty, &details.parameters, &[]);
                    (file, details, substitutions)
                }))
            }
            TypeKind::Inline {
                file,
                shape,
                substitutions,
            } => Ok(Some((
                file.clone(),
                TypeDetails {
                    interface: shape.interface,
                    method_names: if shape.interface {
                        shape.members.keys().cloned().collect()
                    } else {
                        Vec::new()
                    },
                    embedded: shape.embedded.clone(),
                    ..TypeDetails::default()
                },
                substitutions.clone(),
            ))),
            TypeKind::Container(_) | TypeKind::Function { .. } | TypeKind::Parameter(_) => Ok(None),
        }
    }

    fn type_info(
        &mut self,
        package: &Package,
        name: &str,
    ) -> Result<Option<(PathBuf, TypeDetails)>> {
        for file in self.indexes.files(package).iter() {
            let index = &self.indexes.file_index(file)?.symbols;
            if let Some(details) = index.type_details(name) {
                return Ok(Some((file.clone(), details.clone())));
            }
        }
        Ok(None)
    }

    fn container(
        &mut self,
        ty: TypeIdentity<'w>,
    ) -> Result<Option<(Container<TypeIdentity<'w>>, u8)>> {
        if ty.type_expression {
            return Ok(None);
        }
        Ok(match self.underlying_type(ty)? {
            Some(TypeIdentity {
                kind: TypeKind::Container(container),
                pointers,
                ..
            }) => Some((*container, pointers)),
            _ => None,
        })
    }

    fn underlying_type(&mut self, mut ty: TypeIdentity<'w>) -> Result<Option<TypeIdentity<'w>>> {
        for _ in 0..MAX_TYPE_DEPTH {
            if !matches!(ty.kind, TypeKind::Named { .. }) {
                return Ok(Some(ty));
            }
            let Some((file, details, substitutions)) = self.type_context(&ty)? else {
                return Ok(Some(ty));
            };
            let Some(reference) = details.alias.or(details.underlying) else {
                return Ok(Some(ty));
            };
            let imports = &self.indexes.file_index(&file)?.imports;
            let Some(mut underlying) = materialize_type(
                self.workspace,
                ty.package,
                &file,
                imports,
                reference,
                &substitutions,
            ) else {
                return Ok(None);
            };
            underlying.project_properties(&ty);
            ty = underlying;
        }
        Ok(None)
    }

    fn find_symbol(
        &mut self,
        package: &'w Package,
        name: &str,
        receiver: Option<&str>,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        for file in self.indexes.files(package).iter() {
            let index = &self.indexes.file_index(file)?.symbols;
            let symbol = match receiver {
                Some(ty) => index.member(ty, name),
                None => index.declaration(name),
            };
            if let Some(symbol) = symbol {
                return Ok(Some(ResolvedSymbol {
                    package,
                    file: file.clone(),
                    symbol: symbol.clone(),
                    receiver: None,
                }));
            }
        }
        Ok(None)
    }

    fn infer_type(
        &mut self,
        from: &'w Package,
        source: &Source,
        expression: Node<'_>,
        depth: usize,
        result_index: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        if depth >= MAX_TYPE_DEPTH {
            return Ok(None);
        }
        if expression.kind() == "call_expression" {
            return self.call_result(from, source, expression, depth + 1, result_index);
        }
        if expression.kind() == "type_conversion_expression" {
            return self.conversion_result(from, source, expression, depth + 1, result_index);
        }
        if expression.kind() == "range_clause" {
            let Some(operand) = expression.child_by_field_name("right") else {
                return Ok(None);
            };
            let Some(ty) = self.infer_type(from, source, operand, depth + 1, 0)? else {
                return Ok(None);
            };
            return Ok(match (self.container(ty)?, result_index) {
                (Some((Container::Array { element, .. }, 0 | 1)), 1)
                | (Some((Container::Slice(element), 0)), 1)
                | (Some((Container::Map { value: element, .. }, 0)), 1)
                | (
                    Some((
                        Container::Channel {
                            element,
                            receive: true,
                        },
                        0,
                    )),
                    0,
                ) => Some(element),
                (Some((Container::Map { key, .. }, 0)), 0) => Some(key),
                _ => None,
            });
        }
        if result_index != 0 {
            return Ok(None);
        }
        match expression.kind() {
            "identifier" | "type_identifier" | "true" | "false" => {
                let name = node_text(expression, &source.text);
                if let Some(binding) = lexical_binding(expression, name, &source.text) {
                    let addressable = binding
                        .name
                        .parent()
                        .is_none_or(|node| node.kind() != "const_spec");
                    let result = if let Some(ty) = binding.explicit_type {
                        self.source_type(from, source, ty)
                    } else if let Some(initializer) = binding.initializer {
                        self.infer_type(from, source, initializer, depth + 1, binding.result_index)?
                    } else {
                        None
                    };
                    return Ok(result.map(|mut ty| {
                        ty.type_expression = binding.name.parent().is_some_and(is_type_declaration);
                        ty.addressable = addressable && !ty.type_expression;
                        ty
                    }));
                }
                let Some(symbol) = self.find_symbol(from, name, None)? else {
                    return Ok(None);
                };
                self.symbol_type(source, &symbol, name, depth + 1)
            }
            "func_literal" => Ok(self.source_type(from, source, expression)),
            "composite_literal" | "type_assertion_expression" => Ok(expression
                .child_by_field_name("type")
                .and_then(|ty| self.source_type(from, source, ty))),
            "index_expression" | "slice_expression" => {
                let Some(operand) = expression.child_by_field_name("operand") else {
                    return Ok(None);
                };
                let Some(ty) = self.infer_type(from, source, operand, depth + 1, 0)? else {
                    return Ok(None);
                };
                if expression.kind() == "index_expression"
                    && matches!(&ty.kind, TypeKind::Function { signature, .. } if !signature.type_parameters.is_empty())
                    && let Some(argument) = expression.child_by_field_name("index")
                {
                    return self.specialize_function(from, source, ty, [argument]);
                }
                if expression.kind() == "index_expression" {
                    return self.indexed_element(ty);
                }
                let Some((container, pointers)) = self.container(ty.clone())? else {
                    return Ok(None);
                };
                Ok(match (container, pointers) {
                    (Container::Slice(_), 0) => Some(TypeIdentity {
                        addressable: false,
                        ..ty
                    }),
                    (Container::Array { element, .. }, 0 | 1)
                        if ty.addressable || pointers == 1 =>
                    {
                        Some(TypeIdentity::new(
                            ty.package,
                            TypeKind::Container(Box::new(Container::Slice(element))),
                            0,
                        ))
                    }
                    _ => None,
                })
            }
            "type_instantiation_expression" => {
                let Some(function) = expression.child_by_field_name("type") else {
                    return Ok(None);
                };
                let Some(ty) = self.infer_type(from, source, function, depth + 1, 0)? else {
                    return Ok(None);
                };
                let mut cursor = expression.walk();
                let arguments = expression
                    .named_children(&mut cursor)
                    .filter(|node| !node.is_extra() && *node != function);
                self.specialize_function(from, source, ty, arguments)
            }
            "parenthesized_expression" => match expression.named_child(0) {
                Some(inner) => self.infer_type(from, source, inner, depth + 1, 0),
                None => Ok(None),
            },
            "unary_expression" => {
                let (Some(operator), Some(operand)) = (
                    expression.child_by_field_name("operator"),
                    expression.child_by_field_name("operand"),
                ) else {
                    return Ok(None);
                };
                if !matches!(operator.kind(), "&" | "*" | "<-") {
                    return Ok(None);
                }
                let Some(mut ty) = self.infer_type(from, source, operand, depth + 1, 0)? else {
                    return Ok(None);
                };
                if operator.kind() == "<-" {
                    return Ok(match self.container(ty)? {
                        Some((
                            Container::Channel {
                                element,
                                receive: true,
                            },
                            0,
                        )) => Some(element),
                        _ => None,
                    });
                }
                if operator.kind() == "&" || (operator.kind() == "*" && ty.type_expression) {
                    ty.pointers = ty.pointers.saturating_add(1);
                    ty.addressable = false;
                } else if operator.kind() == "*" && ty.pointers > 0 {
                    ty.pointers -= 1;
                    ty.addressable = !ty.type_expression;
                } else {
                    return Ok(None);
                }
                Ok(Some(ty))
            }
            "selector_expression" | "qualified_type" => {
                let fields = if expression.kind() == "qualified_type" {
                    ("package", "name")
                } else {
                    ("operand", "field")
                };
                let Some(receiver) = expression.child_by_field_name(fields.0) else {
                    return Ok(None);
                };
                let Some(name) = expression.child_by_field_name(fields.1) else {
                    return Ok(None);
                };
                let name = node_text(name, &source.text);
                let Some(symbol) = self.selector(from, source, receiver, name, depth + 1)? else {
                    return Ok(None);
                };
                self.symbol_type(source, &symbol, name, depth + 1)
            }
            _ => Ok(None),
        }
    }

    fn call_result(
        &mut self,
        from: &'w Package,
        source: &Source,
        call: Node<'_>,
        depth: usize,
        result_index: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let Some(function) = call.child_by_field_name("function") else {
            return Ok(None);
        };
        if result_index == 0
            && function.kind() == "identifier"
            && node_text(function, &source.text) == "new"
            && lexical_binding(function, "new", &source.text).is_none()
            && self.find_symbol(from, "new", None)?.is_none()
        {
            let argument = call
                .child_by_field_name("arguments")
                .and_then(|args| args.named_child(0));
            return Ok(argument.and_then(|node| {
                let mut ty = self.source_type(from, source, node)?;
                ty.pointers = ty.pointers.checked_add(1)?;
                Some(ty)
            }));
        }
        let Some(mut ty) = self.infer_type(from, source, function, depth, 0)? else {
            return Ok(None);
        };
        if let Some(explicit) = call.child_by_field_name("type_arguments") {
            let Some(specialized) =
                self.specialize_function(from, source, ty, explicit_type_arguments(explicit))?
            else {
                return Ok(None);
            };
            ty = specialized;
        }
        let arguments = CallArguments::List(call.child_by_field_name("arguments"));
        self.result_of_call(from, source, ty, arguments, depth, result_index)
    }

    fn conversion_result(
        &mut self,
        from: &'w Package,
        source: &Source,
        expression: Node<'_>,
        depth: usize,
        result_index: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let Some(syntax) = expression.child_by_field_name("type") else {
            return Ok(None);
        };
        let mut ty = syntax;
        while ty.kind() == "parenthesized_type" {
            let Some(inner) = ty.named_child(0) else {
                return Ok(None);
            };
            ty = inner;
        }
        // The grammar can parse F[T](x), Box[T](x), and callbacks[i](x)
        // using the same node. The binding determines which operation it is.
        if ty.kind() == "generic_type"
            && let (Some(base), Some(explicit), Some(argument)) = (
                ty.child_by_field_name("type"),
                ty.child_by_field_name("type_arguments"),
                expression.child_by_field_name("operand"),
            )
        {
            let Some(callee) = self.infer_type(from, source, base, depth, 0)? else {
                return Ok(None);
            };
            if !callee.type_expression {
                let callee = if matches!(&callee.kind, TypeKind::Function { signature, .. } if !signature.type_parameters.is_empty())
                {
                    self.specialize_function(
                        from,
                        source,
                        callee,
                        explicit_type_arguments(explicit),
                    )?
                } else if explicit_type_arguments(explicit).len() == 1 {
                    self.indexed_element(callee)?
                } else {
                    None
                };
                let Some(callee) = callee else {
                    return Ok(None);
                };
                return self.result_of_call(
                    from,
                    source,
                    callee,
                    CallArguments::Single(argument),
                    depth,
                    result_index,
                );
            }
        }
        Ok((result_index == 0)
            .then(|| self.source_type(from, source, syntax))
            .flatten())
    }

    fn indexed_element(&mut self, ty: TypeIdentity<'w>) -> Result<Option<TypeIdentity<'w>>> {
        let addressable = ty.addressable;
        let Some((container, pointers)) = self.container(ty)? else {
            return Ok(None);
        };
        let (mut element, addressable) = match (container, pointers) {
            (Container::Slice(element), 0) => (element, true),
            (Container::Array { element, .. }, 0 | 1) => (element, addressable || pointers == 1),
            (Container::Map { value, .. }, 0) => (value, false),
            _ => return Ok(None),
        };
        element.addressable = addressable;
        Ok(Some(element))
    }

    fn specialize_function<'a>(
        &mut self,
        caller: &'w Package,
        source: &Source,
        mut ty: TypeIdentity<'w>,
        arguments: impl IntoIterator<Item = Node<'a>>,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let TypeKind::Function {
            signature,
            substitutions,
            ..
        } = &mut ty.kind
        else {
            return Ok(None);
        };
        let mut parameters = signature.type_parameters.iter();
        for argument in arguments {
            let Some(parameter) = parameters.next() else {
                return Ok(None);
            };
            let Some(actual) = self.type_argument(caller, source, argument)? else {
                return Ok(None);
            };
            substitutions.push((parameter.name.clone(), Some(actual)));
        }
        Ok(Some(ty))
    }

    fn type_argument(
        &mut self,
        caller: &'w Package,
        source: &Source,
        argument: Node<'_>,
    ) -> Result<Option<TypeIdentity<'w>>> {
        if argument.has_error() {
            return Ok(None);
        }
        let Some(ty) = self.source_type(caller, source, argument) else {
            return Ok(None);
        };
        if let TypeKind::Named { name, .. } = &ty.kind
            && !is_predeclared_type(name)
            && self.type_info(ty.package, name)?.is_none()
        {
            return Ok(None);
        }
        Ok(Some(ty))
    }

    fn result_of_call(
        &mut self,
        caller: &'w Package,
        source: &Source,
        mut ty: TypeIdentity<'w>,
        arguments: CallArguments<'_>,
        depth: usize,
        result_index: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        if ty.type_expression {
            ty.type_expression = false;
            ty.addressable = false;
            return Ok((result_index == 0).then_some(ty));
        }
        let Some(TypeIdentity {
            package,
            pointers: 0,
            kind:
                TypeKind::Function {
                    file,
                    signature,
                    mut substitutions,
                },
            ..
        }) = self.underlying_type(ty)?
        else {
            return Ok(None);
        };
        let Some(Some(reference)) = signature.results.get(result_index) else {
            return Ok(None);
        };
        if !signature.type_parameters.is_empty() {
            let arguments = arguments.expressions();
            let call = GenericCall {
                caller,
                source,
                arguments: &arguments,
                depth,
            };
            let origin = TypeOrigin {
                package,
                file: &file,
            };
            let Some(inferred) =
                self.infer_type_arguments(call, origin, &signature, &substitutions)?
            else {
                return Ok(None);
            };
            substitutions = inferred;
        }
        let imports = &self.indexes.file_index(&file)?.imports;
        Ok(materialize_type(
            self.workspace,
            package,
            &file,
            imports,
            reference.clone(),
            &substitutions,
        ))
    }

    fn infer_type_arguments(
        &mut self,
        call: GenericCall<'w, '_, '_>,
        origin: TypeOrigin<'w, '_>,
        signature: &FunctionSignature,
        explicit: &Substitutions<'w>,
    ) -> Result<Option<Substitutions<'w>>> {
        if signature.parameters.len() != call.arguments.len() {
            return Ok(None);
        }
        let parameters = &signature.type_parameters;
        let placeholders: Substitutions<'w> = parameters
            .iter()
            .enumerate()
            .map(|(i, param)| {
                (
                    param.name.clone(),
                    Some(TypeIdentity::new(origin.package, TypeKind::Parameter(i), 0)),
                )
            })
            .collect();
        let mut bindings: Vec<_> = parameters
            .iter()
            .map(|param| {
                explicit
                    .iter()
                    .find(|(name, _)| name == &param.name)
                    .and_then(|(_, ty)| ty.clone())
            })
            .collect();

        let imports = self.indexes.file_index(origin.file)?.imports.clone();
        let workspace = self.workspace;
        let pattern = |reference| {
            materialize_type(
                workspace,
                origin.package,
                origin.file,
                &imports,
                reference,
                &placeholders,
            )
        };
        let mut untyped = Vec::new();
        for (argument, parameter) in call.arguments.iter().zip(&signature.parameters) {
            let Some(expected) = parameter.clone().and_then(pattern) else {
                return Ok(None);
            };
            if argument.has_error() {
                return Ok(None);
            }
            if let Some(actual) =
                self.infer_type(call.caller, call.source, *argument, call.depth + 1, 0)?
            {
                if actual.type_expression
                    || !self.unify_types(expected, actual, &mut bindings, true)?
                {
                    return Ok(None);
                }
            } else if let Some(default) = untyped_default(*argument) {
                untyped.push((expected, default));
            } else {
                return Ok(None);
            }
        }

        let approximations: Vec<_> = parameters
            .iter()
            .map(|param| param.constraint.approximation.clone().and_then(pattern))
            .collect();
        if !self.apply_approximations(&approximations, &mut bindings)? {
            return Ok(None);
        }
        for (expected, default) in untyped {
            if let TypeKind::Parameter(index) = expected.kind
                && let Some(bound) = bindings.get(index).and_then(Clone::clone)
                && expected.pointers == 0
                && bound.pointers == 0
                && let Some(underlying) = self.underlying_type(bound)?
                && let TypeKind::Named { name, .. } = underlying.kind
                && name == default
                && self.type_info(underlying.package, &name)?.is_none()
            {
                continue;
            }
            let actual = TypeIdentity::named(call.caller, default);
            if !self.unify_types(expected, actual, &mut bindings, true)? {
                return Ok(None);
            }
        }
        if !self.apply_approximations(&approximations, &mut bindings)?
            || bindings.iter().any(Option::is_none)
        {
            return Ok(None);
        }
        let substitutions = parameters
            .iter()
            .zip(&bindings)
            .map(|(param, ty)| (param.name.clone(), ty.clone()))
            .collect::<Vec<_>>();
        for (param, actual) in parameters.iter().zip(bindings) {
            if let (Some(reference), Some(mut actual)) = (param.constraint.methods.clone(), actual)
                && let Some(required) = materialize_type(
                    workspace,
                    origin.package,
                    origin.file,
                    &imports,
                    reference,
                    &substitutions,
                )
            {
                let mut names = Vec::new();
                self.collect_constraint_methods(required, &mut names, &[])?;
                // A pointer method on an addressable variable alone does not
                // satisfy a constraint for the variable's non-pointer type.
                actual.addressable = false;
                actual.type_expression = true;
                for (package, name) in names {
                    if self.member(package, &actual, &name, true)?.is_none() {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(substitutions))
    }

    fn apply_approximations(
        &mut self,
        approximations: &[Option<TypeIdentity<'w>>],
        bindings: &mut [Option<TypeIdentity<'w>>],
    ) -> Result<bool> {
        // A typed S with S ~[]E (or ~map[K]V) supplies the remaining
        // arguments through its underlying container shape.
        for _ in 0..bindings.len() {
            let previous_len = bindings.iter().filter(|slot| slot.is_some()).count();
            for (i, expected) in approximations.iter().enumerate() {
                if let (Some(expected), Some(actual)) = (expected.clone(), bindings[i].clone()) {
                    let Some(underlying) = self.underlying_type(actual)? else {
                        return Ok(false);
                    };
                    if !self.unify_types(expected, underlying, bindings, false)? {
                        return Ok(false);
                    }
                }
            }
            if previous_len == bindings.iter().filter(|slot| slot.is_some()).count() {
                break;
            }
        }
        Ok(true)
    }

    fn constraint_interface(
        &mut self,
        mut ty: TypeIdentity<'w>,
    ) -> Result<Option<(TypeIdentity<'w>, PathBuf, TypeDetails, Substitutions<'w>)>> {
        for _ in 0..MAX_TYPE_DEPTH {
            let Some((file, details, substitutions)) = self.type_context(&ty)? else {
                return Ok(None);
            };
            if details.interface && ty.pointers == 0 {
                return Ok(Some((ty, file, details, substitutions)));
            }
            let Some(reference) = details.alias.or(details.underlying) else {
                return Ok(None);
            };
            let imports = &self.indexes.file_index(&file)?.imports;
            let Some(mut next) = materialize_type(
                self.workspace,
                ty.package,
                &file,
                imports,
                reference,
                &substitutions,
            ) else {
                return Ok(None);
            };
            next.project_properties(&ty);
            ty = next;
        }
        Ok(None)
    }

    fn collect_constraint_methods(
        &mut self,
        constraint: TypeIdentity<'w>,
        names: &mut Vec<(&'w Package, String)>,
        ancestors: &[TypeKey],
    ) -> Result<()> {
        if ancestors.len() >= MAX_TYPE_DEPTH {
            return Ok(());
        }
        let Some((ty, file, details, substitutions)) = self.constraint_interface(constraint)?
        else {
            return Ok(());
        };
        let Some(key) = ty.key().filter(|key| !ancestors.contains(key)) else {
            return Ok(());
        };
        names.extend(
            details
                .method_names
                .into_iter()
                .map(|name| (ty.package, name)),
        );
        let mut ancestors = ancestors.to_vec();
        ancestors.push(key);
        let imports = self.indexes.file_index(&file)?.imports.clone();
        for embedded in details.embedded {
            if let Some(interface) = materialize_type(
                self.workspace,
                ty.package,
                &file,
                &imports,
                embedded,
                &substitutions,
            ) {
                self.collect_constraint_methods(interface, names, &ancestors)?;
            }
        }
        Ok(())
    }

    fn unify_types(
        &mut self,
        expected: TypeIdentity<'w>,
        mut actual: TypeIdentity<'w>,
        bindings: &mut [Option<TypeIdentity<'w>>],
        loose: bool,
    ) -> Result<bool> {
        if let TypeKind::Parameter(index) = expected.kind {
            let Some(pointers) = actual.pointers.checked_sub(expected.pointers) else {
                return Ok(false);
            };
            actual.pointers = pointers;
            actual.addressable = false;
            let Some(bound) = bindings.get_mut(index) else {
                return Ok(false);
            };
            if let Some(previous) = bound.clone() {
                return self.unify_types(previous, actual, bindings, false);
            }
            *bound = Some(actual);
            return Ok(true);
        }
        let Some(expected) = self.resolve_alias(expected)? else {
            return Ok(false);
        };
        let Some(actual) = self.resolve_alias(actual)? else {
            return Ok(false);
        };
        let actual = if loose && matches!(&expected.kind, TypeKind::Container(_)) {
            self.underlying_type(actual)?
        } else {
            Some(actual)
        };
        let Some(actual) = actual.filter(|actual| actual.pointers == expected.pointers) else {
            return Ok(false);
        };
        match (expected.kind, actual.kind) {
            (
                TypeKind::Named {
                    name: left,
                    arguments: left_args,
                },
                TypeKind::Named {
                    name: right,
                    arguments: right_args,
                },
            ) => {
                if left != right || left_args.len() != right_args.len() {
                    return Ok(false);
                }
                if !std::ptr::eq(expected.package, actual.package)
                    && (!is_predeclared_type(&left)
                        || self.type_info(expected.package, &left)?.is_some()
                        || self.type_info(actual.package, &right)?.is_some())
                {
                    return Ok(false);
                }
                for (left, right) in left_args.into_iter().zip(right_args) {
                    if let (Some(left), Some(right)) = (left, right)
                        && self.unify_types(left, right, bindings, false)?
                    {
                        continue;
                    }
                    return Ok(false);
                }
                Ok(true)
            }
            (TypeKind::Container(left), TypeKind::Container(right)) => match (*left, *right) {
                (Container::Slice(left), Container::Slice(right)) => {
                    self.unify_types(left, right, bindings, false)
                }
                (
                    Container::Array {
                        element: left,
                        length: Some(n),
                    },
                    Container::Array {
                        element: right,
                        length: Some(m),
                    },
                ) if n == m => self.unify_types(left, right, bindings, false),
                (Container::Map { key: lk, value: lv }, Container::Map { key: rk, value: rv }) => {
                    Ok(self.unify_types(lk, rk, bindings, false)?
                        && self.unify_types(lv, rv, bindings, false)?)
                }
                (
                    Container::Channel {
                        element: left,
                        receive: a,
                    },
                    Container::Channel {
                        element: right,
                        receive: b,
                    },
                ) if a == b => self.unify_types(left, right, bindings, false),
                _ => Ok(false),
            },
            (
                TypeKind::Inline {
                    file: a,
                    shape: left,
                    ..
                },
                TypeKind::Inline {
                    file: b,
                    shape: right,
                    ..
                },
            ) => Ok(a == b && left.origin == right.origin),
            _ => Ok(false),
        }
    }

    fn resolve_alias(&mut self, mut ty: TypeIdentity<'w>) -> Result<Option<TypeIdentity<'w>>> {
        for _ in 0..MAX_TYPE_DEPTH {
            let TypeKind::Named { name, .. } = &ty.kind else {
                return Ok(Some(ty));
            };
            let Some((file, details)) = self.type_info(ty.package, name)? else {
                return Ok(Some(ty));
            };
            let Some(reference) = details.alias else {
                return Ok(Some(ty));
            };
            let substitutions = type_substitutions(&ty, &details.parameters, &[]);
            let imports = &self.indexes.file_index(&file)?.imports;
            let Some(mut underlying) = materialize_type(
                self.workspace,
                ty.package,
                &file,
                imports,
                reference,
                &substitutions,
            ) else {
                return Ok(None);
            };
            underlying.project_properties(&ty);
            ty = underlying;
        }
        Ok(None)
    }

    fn source_type(
        &mut self,
        from: &'w Package,
        source: &Source,
        ty: Node<'_>,
    ) -> Option<TypeIdentity<'w>> {
        let parse = |node| symbols::type_ref(node, &source.text, &|node| source.range(node));
        let mut reference = parse(ty)?;
        let mut constrained = false;
        if let TypeRefKind::Named(named) = &reference.kind
            && named.qualifier.is_none()
            && let Some(binding) = lexical_binding(ty, &named.name, &source.text)
        {
            let owner = binding.name.parent()?;
            if owner.kind() == "type_parameter_declaration" && reference.pointers == 0 {
                constrained = true;
                reference =
                    symbols::type_constraint(binding.explicit_type?, &source.text, &|node| {
                        source.range(node)
                    })
                    .methods?;
            } else if owner.kind() == "type_elem" && reference.pointers == 0 {
                return self.receiver_constraint(from, source, binding.name);
            } else {
                if !is_type_declaration(owner) {
                    return None;
                }
                let mut underlying = parse(binding.explicit_type?)?;
                if !matches!(underlying.kind, TypeRefKind::Function(_)) {
                    return None;
                }
                underlying.pointers = underlying.pointers.checked_add(reference.pointers)?;
                reference = underlying;
            }
        }
        let ty = materialize_type(
            self.workspace,
            from,
            &source.path,
            &source.imports,
            reference,
            &[],
        )?;
        if !constrained {
            return Some(ty);
        }
        Some(self.constraint_interface(ty).ok()??.0)
    }

    fn receiver_constraint(
        &mut self,
        from: &'w Package,
        source: &Source,
        parameter: Node<'_>,
    ) -> Option<TypeIdentity<'w>> {
        let mut method = parameter.parent()?;
        while method.kind() != "method_declaration" {
            method = method.parent()?;
        }
        let (base, arguments) = receiver_type_parameters(method)?;
        let mut cursor = arguments.walk();
        let index = arguments
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "type_elem")
            .position(|node| node.named_child(0) == Some(parameter))?;
        let name = node_text(base, &source.text);
        let (file, details) = self.type_info(from, name).ok()??;
        let reference = details.parameters.get(index)?.constraint.methods.clone()?;
        let imports = &self.indexes.file_index(&file).ok()?.imports;
        let ty = materialize_type(self.workspace, from, &file, imports, reference, &[])?;
        Some(self.constraint_interface(ty).ok()??.0)
    }

    fn symbol_type(
        &mut self,
        source: &Source,
        resolved: &ResolvedSymbol<'w>,
        name: &str,
        depth: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        match &resolved.symbol.kind {
            SymbolKind::Value(Some(reference)) => {
                let addressable = resolved.symbol.addressable
                    && resolved
                        .receiver
                        .as_ref()
                        .is_none_or(|ty| ty.addressable || ty.pointers > 0);
                self.type_from_symbol(resolved, reference.clone(), addressable)
            }
            SymbolKind::InferredValue {
                initializer,
                result_index,
            } => {
                let documents = self.indexes.documents;
                let disk_source;
                let source = if source.path == resolved.file {
                    source
                } else if let Some(document) = documents.get(&path_key(&resolved.file)) {
                    document
                } else {
                    disk_source = read_source(self.indexes.parser, &resolved.file)?;
                    &disk_source
                };
                let Some(expression) = source
                    .tree
                    .root_node()
                    .named_descendant_for_byte_range(initializer.start, initializer.end)
                    .filter(|node| node.byte_range() == *initializer)
                else {
                    return Ok(None);
                };
                let mut ty =
                    self.infer_type(resolved.package, source, expression, depth, *result_index)?;
                if let Some(ty) = &mut ty {
                    ty.addressable = true;
                    ty.type_expression = false;
                }
                Ok(ty)
            }
            SymbolKind::Function(signature) => self.type_from_symbol(
                resolved,
                TypeRef {
                    pointers: 0,
                    kind: TypeRefKind::Function(signature.clone()),
                },
                false,
            ),
            SymbolKind::Type => {
                let mut ty = TypeIdentity::named(resolved.package, name);
                ty.type_expression = true;
                Ok(Some(ty))
            }
            _ => Ok(None),
        }
    }

    fn type_from_symbol(
        &mut self,
        resolved: &ResolvedSymbol<'w>,
        reference: TypeRef,
        addressable: bool,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let substitutions = match resolved.receiver.as_ref().map(|receiver| &receiver.kind) {
            Some(TypeKind::Named { name, arguments }) if !arguments.is_empty() => {
                let receiver = resolved.receiver.as_ref().expect("named receiver");
                match self.type_info(receiver.package, name)? {
                    Some((_, details)) => type_substitutions(
                        receiver,
                        &details.parameters,
                        &resolved.symbol.receiver_parameters,
                    ),
                    None => Vec::new(),
                }
            }
            Some(TypeKind::Inline { substitutions, .. }) => substitutions.clone(),
            _ => Vec::new(),
        };
        let index = self.indexes.file_index(&resolved.file)?;
        Ok(materialize_type(
            self.workspace,
            resolved.package,
            &resolved.file,
            &index.imports,
            reference,
            &substitutions,
        )
        .map(|mut ty| {
            ty.addressable = addressable;
            ty
        }))
    }
}

struct TypeOrigin<'w, 'path> {
    package: &'w Package,
    file: &'path Path,
}

fn explicit_type_arguments(list: Node<'_>) -> Vec<Node<'_>> {
    list.named_children(&mut list.walk())
        .filter(|element| element.kind() == "type_elem")
        .filter_map(|element| element.named_child(0))
        .collect()
}

struct GenericCall<'w, 's, 'tree> {
    caller: &'w Package,
    source: &'s Source,
    arguments: &'s [Node<'tree>],
    depth: usize,
}

enum CallArguments<'tree> {
    List(Option<Node<'tree>>),
    Single(Node<'tree>),
}

impl<'tree> CallArguments<'tree> {
    fn expressions(self) -> Vec<Node<'tree>> {
        match self {
            Self::List(Some(list)) => list
                .named_children(&mut list.walk())
                .filter(|node| !node.is_extra())
                .collect(),
            Self::List(None) => Vec::new(),
            Self::Single(node) => vec![node],
        }
    }
}

fn untyped_default(argument: Node<'_>) -> Option<&'static str> {
    match argument.kind() {
        "int_literal" => Some("int"),
        "rune_literal" => Some("rune"),
        "float_literal" => Some("float64"),
        "imaginary_literal" => Some("complex128"),
        "interpreted_string_literal" | "raw_string_literal" => Some("string"),
        "true" | "false" => Some("bool"),
        _ => None,
    }
}

fn is_predeclared_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "bool"
            | "byte"
            | "comparable"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
    )
}

fn type_substitutions<'w>(
    owner: &TypeIdentity<'w>,
    parameters: &[GenericParameter],
    receiver_parameters: &[Option<String>],
) -> Substitutions<'w> {
    let TypeKind::Named { arguments, .. } = &owner.kind else {
        return Vec::new();
    };
    arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            let name = if receiver_parameters.is_empty() {
                &parameters.get(index)?.name
            } else {
                receiver_parameters.get(index)?.as_ref()?
            };
            Some((name.clone(), argument.clone()))
        })
        .collect()
}

fn materialize_type<'w>(
    workspace: &'w Workspace,
    from: &'w Package,
    file: &Path,
    imports: &[Import],
    reference: TypeRef,
    substitutions: &[(String, Option<TypeIdentity<'w>>)],
) -> Option<TypeIdentity<'w>> {
    let resolve =
        |reference| materialize_type(workspace, from, file, imports, reference, substitutions);
    let (package, kind) = match reference.kind {
        TypeRefKind::Named(named) => {
            if named.qualifier.is_none()
                && let Some((_, argument)) =
                    substitutions.iter().find(|(name, _)| *name == named.name)
            {
                let mut ty = argument.clone()?;
                ty.pointers = ty.pointers.saturating_add(reference.pointers);
                return Some(ty);
            }
            let package = match named.qualifier {
                Some(qualifier) => imported_package(imports, workspace, from, &qualifier)?.1,
                None => from,
            };
            let arguments = named
                .arguments
                .into_iter()
                .map(|arg| arg.and_then(resolve))
                .collect();
            (
                package,
                TypeKind::Named {
                    name: named.name,
                    arguments,
                },
            )
        }
        TypeRefKind::Container(container) => {
            let container = match *container {
                Container::Slice(element) => Container::Slice(resolve(element)?),
                Container::Array { element, length } => Container::Array {
                    element: resolve(element)?,
                    length,
                },
                Container::Map { key, value } => Container::Map {
                    key: resolve(key)?,
                    value: resolve(value)?,
                },
                Container::Channel { element, receive } => Container::Channel {
                    element: resolve(element)?,
                    receive,
                },
            };
            (from, TypeKind::Container(Box::new(container)))
        }
        TypeRefKind::Function(signature) => (
            from,
            TypeKind::Function {
                file: file.to_owned(),
                signature,
                substitutions: substitutions.to_vec(),
            },
        ),
        TypeRefKind::Inline(shape) => (
            from,
            TypeKind::Inline {
                file: file.to_owned(),
                shape,
                substitutions: substitutions.to_vec(),
            },
        ),
    };
    Some(TypeIdentity::new(package, kind, reference.pointers))
}

fn read_source(parser: &mut Parser, path: &Path) -> Result<Source> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Source::parse(parser, path.to_owned(), text, None)
}

#[derive(Clone)]
struct Import {
    alias: Option<String>,
    path: String,
    binding: Range,
}

struct Source {
    path: PathBuf,
    text: String,
    lines: LineIndex,
    tree: Tree,
    imports: Vec<Import>,
    declarations: OnceCell<FileIndex>,
}

impl Source {
    fn parse(
        parser: &mut Parser,
        path: PathBuf,
        text: String,
        previous: Option<&Tree>,
    ) -> Result<Self> {
        let lines = LineIndex::new(&text);
        Self::parse_with_lines(parser, path, text, lines, previous)
    }

    fn parse_with_lines(
        parser: &mut Parser,
        path: PathBuf,
        text: String,
        lines: LineIndex,
        previous: Option<&Tree>,
    ) -> Result<Self> {
        let tree = parser.parse(&text, previous).context("parsing Go source")?;
        let imports = imports(tree.root_node(), &text, &lines);
        Ok(Self {
            path,
            text,
            lines,
            tree,
            imports,
            declarations: OnceCell::new(),
        })
    }

    fn edit(
        &mut self,
        parser: &mut Parser,
        changes: &[TextDocumentContentChangeEvent],
    ) -> Result<()> {
        let mut text = self.text.clone();
        let mut tree = self.tree.clone();
        let mut lines = self.lines.clone();
        for change in changes {
            let (start, end) = if let Some(range) = change.range {
                (
                    lines
                        .offset(&text, range.start)
                        .context("invalid edit start")?,
                    lines.offset(&text, range.end).context("invalid edit end")?,
                )
            } else {
                (0, text.len())
            };
            if start > end {
                bail!("edit ends before it begins");
            }

            let start_position = lines.point(start);
            let new_end_position = match change.text.rfind('\n') {
                Some(last_break) => Point::new(
                    start_position.row + change.text.bytes().filter(|byte| *byte == b'\n').count(),
                    change.text.len() - last_break - 1,
                ),
                None => Point::new(
                    start_position.row,
                    start_position.column + change.text.len(),
                ),
            };
            tree.edit(&InputEdit {
                start_byte: start,
                old_end_byte: end,
                new_end_byte: start + change.text.len(),
                start_position,
                old_end_position: lines.point(end),
                new_end_position,
            });
            lines.edit(start, end, &change.text);
            text.replace_range(start..end, &change.text);
        }

        *self = Self::parse_with_lines(parser, self.path.clone(), text, lines, Some(&tree))?;
        Ok(())
    }

    fn reference_at(&self, position: Position) -> Option<ReferenceQuery<'_>> {
        let offset = self.lines.offset(&self.text, position)?;
        let root = self.tree.root_node();
        let leaf = root.named_descendant_for_byte_range(offset, offset)?;
        let mut current = Some(leaf);
        while let Some(node) = current {
            let fields = match node.kind() {
                "selector_expression" => Some(("operand", "field")),
                "qualified_type" => Some(("package", "name")),
                _ => None,
            };
            if let Some((receiver_field, name_field)) = fields
                && let Some(name) = node.child_by_field_name(name_field)
                && name.start_byte() <= offset
                && offset <= name.end_byte()
            {
                let receiver = node.child_by_field_name(receiver_field)?;
                return Some(ReferenceQuery::Member { receiver, name });
            }
            if node.kind() == "import_spec" {
                if let Some(path) = node.child_by_field_name("path")
                    && path.start_byte() <= offset
                    && offset < path.end_byte()
                {
                    return Some(ReferenceQuery::Name(path));
                }
                break;
            }
            if node.kind() == "comment" {
                return None;
            }
            current = node.parent();
        }
        let parent = leaf.parent()?;
        let named = matches!(
            leaf.kind(),
            "identifier" | "type_identifier" | "package_identifier"
        );
        let field_key = parent.kind() == "literal_element"
            && parent.parent().is_some_and(|node| {
                node.kind() == "keyed_element" && node.child_by_field_name("key") == Some(parent)
            });
        if named && field_key {
            let value = parent.parent()?.parent()?;
            if value.kind() == "literal_value" {
                return Some(ReferenceQuery::LiteralKey { value, name: leaf });
            }
        }
        let predeclared_literal = matches!(leaf.kind(), "nil" | "true" | "false" | "iota");
        (named || predeclared_literal || is_declaration_name(leaf))
            .then_some(ReferenceQuery::Name(leaf))
    }

    fn imported_package<'a>(
        &self,
        workspace: &'a Workspace,
        from: &Package,
        qualifier: &str,
    ) -> Option<(&Import, &'a Package)> {
        imported_package(&self.imports, workspace, from, qualifier)
    }

    fn range(&self, node: Node<'_>) -> Range {
        Range::new(
            self.lines.position(&self.text, node.start_byte()),
            self.lines.position(&self.text, node.end_byte()),
        )
    }

    fn declarations(&self) -> &FileIndex {
        self.declarations
            .get_or_init(|| self.collect_declarations())
    }

    fn package_signature(&self) -> u64 {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let mut signature = DefaultHasher::new();
        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "package_clause" | "import_declaration" => {
                    node_text(node, &self.text).hash(&mut signature)
                }
                "comment" => {
                    let comment = node_text(node, &self.text);
                    if comment.starts_with("//go:build") || comment.starts_with("// +build") {
                        comment.hash(&mut signature);
                    }
                }
                _ => break,
            }
        }
        signature.finish()
    }

    fn collect_declarations(&self) -> FileIndex {
        FileIndex {
            symbols: symbols::collect(&self.tree, &self.text, |name| self.range(name)),
            imports: self.imports.clone(),
            package_signature: self.package_signature(),
        }
    }
}

fn imported_package<'a, 'w>(
    imports: &'a [Import],
    workspace: &'w Workspace,
    from: &Package,
    qualifier: &str,
) -> Option<(&'a Import, &'w Package)> {
    imports.iter().find_map(|import| {
        let package = workspace.resolve_import(from, &import.path)?;
        let name = import.alias.as_deref().unwrap_or(&package.name);
        (name == qualifier).then_some((import, package))
    })
}

fn import_path(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        Some(raw.replace('\r', ""))
    } else {
        serde_json::from_str(literal).ok()
    }
}

fn imports(root: Node<'_>, text: &str, lines: &LineIndex) -> Vec<Import> {
    fn collect(node: Node<'_>, text: &str, lines: &LineIndex, result: &mut Vec<Import>) {
        match node.kind() {
            "import_declaration" | "import_spec_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    collect(child, text, lines, result);
                }
            }
            "import_spec" => {
                let Some(path) = node.child_by_field_name("path") else {
                    return;
                };
                let alias = node.child_by_field_name("name");
                let name = alias.unwrap_or(path);
                let binding = Range::new(
                    lines.position(text, name.start_byte()),
                    lines.position(text, name.end_byte()),
                );
                let alias = alias.map(|node| node_text(node, text).to_owned());
                if let Some(path) = import_path(node_text(path, text))
                    && alias.as_deref() != Some("_")
                {
                    result.push(Import {
                        alias,
                        path,
                        binding,
                    });
                }
            }
            _ => {}
        }
    }

    let mut result = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        collect(node, text, lines, &mut result);
    }
    result
}

fn is_type_declaration(node: Node<'_>) -> bool {
    matches!(node.kind(), "type_spec" | "type_alias")
}

fn is_declaration_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "import_spec" {
        return parent.child_by_field_name("path") == Some(node);
    }
    if !matches!(
        parent.kind(),
        "field_declaration"
            | "function_declaration"
            | "method_declaration"
            | "method_elem"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration"
            | "type_alias"
            | "type_spec"
            | "const_spec"
            | "var_spec"
    ) {
        return false;
    }
    let mut cursor = parent.walk();
    parent
        .children_by_field_name("name", &mut cursor)
        .any(|name| name == node)
}

fn is_predeclared(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "bool"
            | "byte"
            | "comparable"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
            | "true"
            | "false"
            | "iota"
            | "nil"
            | "append"
            | "cap"
            | "clear"
            | "close"
            | "complex"
            | "copy"
            | "delete"
            | "imag"
            | "len"
            | "make"
            | "max"
            | "min"
            | "new"
            | "panic"
            | "print"
            | "println"
            | "real"
            | "recover"
    )
}

fn is_exported(symbol: &str) -> bool {
    symbol.chars().next().is_some_and(char::is_uppercase)
}

fn node_text<'text>(node: Node<'_>, text: &'text str) -> &'text str {
    &text[node.byte_range()]
}

#[derive(Clone)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { starts }
    }

    fn offset(&self, text: &str, position: Position) -> Option<usize> {
        let line = position.line as usize;
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map_or(text.len(), |next| next - 1);
        let content = &text[start..end];
        let content = content.strip_suffix('\r').unwrap_or(content);
        let character = position.character as usize;
        if content.is_ascii() {
            return Some(start + character.min(content.len()));
        }
        let mut units = 0;
        for (index, ch) in content.char_indices() {
            if units == character {
                return Some(start + index);
            }
            units += ch.len_utf16();
            if units > character {
                return None;
            }
        }
        Some(start + content.len())
    }

    fn point(&self, offset: usize) -> Point {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        Point::new(line, offset - self.starts[line])
    }

    fn position(&self, text: &str, offset: usize) -> Position {
        let point = self.point(offset);
        let column = text[self.starts[point.row]..offset].encode_utf16().count();
        Position::new(point.row as u32, column as u32)
    }

    fn edit(&mut self, start: usize, end: usize, inserted: &str) {
        let first = self.starts.partition_point(|offset| *offset <= start);
        let last = self.starts.partition_point(|offset| *offset <= end);
        for offset in &mut self.starts[last..] {
            *offset = *offset - (end - start) + inserted.len();
        }
        self.starts.splice(
            first..last,
            inserted
                .match_indices('\n')
                .map(|(index, _)| start + index + 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_public_and_private_symbols_in_grouped_declarations() {
        let mut engine = Engine::new(PathBuf::new());
        let source = Source::parse(
            &mut engine.parser,
            PathBuf::new(),
            "package test\nvar (First, Second = 1, 2)\nconst (Third, fourth = 3, 4)\ntype (Fifth int)\nfunc Sixth() {}\n".into(),
            None,
        )
        .unwrap();
        let names = &source.declarations().symbols;
        for name in ["First", "Second", "Third", "Fifth", "Sixth"] {
            assert!(names.declaration(name).is_some(), "missing {name}");
        }
        assert!(names.declaration("fourth").is_some());
    }

    #[test]
    fn import_qualifier_respects_shadowing_and_utf16() {
        let mut engine = Engine::new(PathBuf::new());
        let text = concat!(
            "package test\n",
            "import alias \"example.test/lib\"\n",
            "func a() { _ = \"😀\"; alias.Foo() }\n",
            "func b(alias T) { alias.Foo() }\n",
            "func c() { alias := T{}; alias.Foo() }\n",
            "func d() { if alias := T{}; true { alias.Foo() }; alias.Foo() }\n",
        );
        let source = Source::parse(&mut engine.parser, PathBuf::new(), text.into(), None).unwrap();
        let queries: Vec<_> = text
            .match_indices("Foo")
            .map(|(offset, _)| {
                let query = source.reference_at(source.lines.position(text, offset))?;
                let ReferenceQuery::Member { receiver, name } = query else {
                    return None;
                };
                let qualifier = node_text(receiver, text);
                lexical_binding(receiver, qualifier, text)
                    .is_none()
                    .then_some((qualifier, node_text(name, text)))
            })
            .collect();
        assert_eq!(
            queries,
            [
                Some(("alias", "Foo")),
                None,
                None,
                None,
                Some(("alias", "Foo"))
            ],
        );
        assert!(source.lines.offset(text, Position::new(2, 17)).is_none());
    }

    #[test]
    fn line_index_tracks_multiline_insertions_and_deletions() {
        for (text, start, end, insert) in [
            ("abc\ndef\n", 2, 6, "\n\n"),
            ("abc\ndef\n", 4, 4, "new\n"),
            ("abc\ndef\n", 0, 4, ""),
            ("abc\ndef\n", 0, 8, "single"),
            ("", 0, 0, "\r\n"),
        ] {
            let mut lines = LineIndex::new(text);
            lines.edit(start, end, insert);
            let mut edited = text.to_owned();
            edited.replace_range(start..end, insert);
            assert_eq!(lines.starts, LineIndex::new(&edited).starts);
        }
    }
}
