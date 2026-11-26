use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use anyhow::Result;
use lsp_types::{CompletionItemKind as Kind, Range};
use serde::Deserialize;
use tree_sitter::Parser;

use crate::semantic::{
    index::IndexedSource,
    resolver::{ResolvedSymbol, is_exported},
    snapshot::{DocumentSnapshot, EngineSnapshot},
    source::{Source, is_test_source, read_source},
};
use crate::symbols::{Container, SymbolKind, TypeRef, TypeRefKind};
use crate::workspace::{Package, Workspace, is_symbol_source, path_key};

use super::CompletionClientCapabilities;
use super::candidates::resolved_candidate;
use super::cursor::{identifier_char, identifier_start};
use super::expected::{Expected, named_key};
use super::ranking::{
    AUTO_IMPORT, Candidate, Insert, Match, SYMBOL, Weight, match_member_hump, match_name,
    match_start, package_priority,
};
use super::render::{Description, PendingImport, Target, describe, package_name};
use super::sdk_names::exported_names;

const BATCH_SIZE: usize = 8;

pub(super) struct FileImport {
    pub(super) path: String,
    pub(super) qualifier: String,
    pub(super) in_graph: bool,
}

pub(super) enum Selection {
    Name { auto_import: bool },
    Qualifier { name: String, start: usize },
}

pub(super) struct SymbolSearch {
    pub(super) selection: Selection,
    pub(super) types_only: bool,
    pub(super) address_of: bool,
    pub(super) literal: bool,
    pub(super) declared: HashSet<String>,
    pub(super) imports: Vec<FileImport>,
    pub(super) expected: Option<Expected>,
}

pub(super) enum SearchKind {
    Path { close: Option<char> },
    Symbols(SymbolSearch),
}

#[derive(Default)]
pub(in crate::semantic) struct ImportCache {
    generation: Option<u64>,
    graph: Rc<[Importable]>,
    sdk: SdkState,
    sdk_identity: Option<SdkIdentity>,
    disk_names: HashMap<PathBuf, Vec<String>>,
    sdk_exports: HashMap<PathBuf, Rc<[Export]>>,
}

#[derive(Default)]
enum SdkState {
    #[default]
    Unloaded,
    Loading(Receiver<Vec<Importable>>),
    Ready(Rc<[Importable]>),
}

#[derive(PartialEq, Eq)]
struct SdkIdentity {
    src: PathBuf,
    version_stamp: Option<SystemTime>,
}

#[derive(Clone)]
struct Importable {
    path: String,
    name: String,
    directory: PathBuf,
    vendor_parent: Option<PathBuf>,
    prefilter_disk: bool,
    origin: Origin,
}

#[derive(Clone)]
enum Origin {
    Graph(String),
    Sdk(Vec<PathBuf>),
}

impl ImportCache {
    fn prepare(&mut self, generation: u64, workspace: &Workspace) {
        if self.generation == Some(generation) {
            return;
        }
        self.generation = Some(generation);
        self.graph = workspace
            .navigation_packages()
            .filter(|package| package.is_production())
            .filter_map(|package| Importable::from_graph(workspace, package))
            .collect();
        self.sdk = SdkState::Unloaded;
    }

    fn sdk(&mut self, workspace: &Workspace, root: &Path) -> Option<Rc<[Importable]>> {
        if matches!(self.sdk, SdkState::Unloaded) {
            let src = workspace
                .builtin_source(root)
                .ok()
                .and_then(|file| file.parent()?.parent());
            let identity = src.map(|src| SdkIdentity {
                src: path_key(src),
                version_stamp: src
                    .parent()
                    .and_then(|root| fs::metadata(root.join("VERSION")).ok()?.modified().ok()),
            });
            if self.sdk_identity != identity {
                self.disk_names.clear();
                self.sdk_exports.clear();
                self.sdk_identity = identity;
            }
            self.sdk = src
                .and_then(|src| start_sdk_inventory(root, src))
                .map_or_else(|| SdkState::Ready(Rc::from([])), SdkState::Loading);
        }
        if let SdkState::Loading(receiver) = &self.sdk {
            let entries = match receiver.try_recv() {
                Ok(entries) => entries,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Vec::new(),
            };
            self.sdk = SdkState::Ready(entries.into());
        }
        match &self.sdk {
            SdkState::Ready(entries) => Some(Rc::clone(entries)),
            _ => None,
        }
    }

    fn exports(
        &mut self,
        parser: &mut Parser,
        file: &Path,
        overlay: Option<&IndexedSource>,
        entry: &Importable,
    ) -> Rc<[Export]> {
        if let Some(overlay) = overlay {
            return sdk_exports(overlay, entry);
        }
        let key = path_key(file);
        if let Some(exports) = self.sdk_exports.get(&key) {
            return Rc::clone(exports);
        }
        self.sdk_exports
            .entry(key)
            .or_insert_with(|| {
                let Ok(source) = read_source(parser, file) else {
                    return Rc::from([]);
                };
                sdk_exports(&IndexedSource::new(source, None), entry)
            })
            .clone()
    }

    fn disk_file_matches(&mut self, file: &Path, matcher: &SymbolMatcher<'_>) -> bool {
        self.disk_names
            .entry(path_key(file))
            .or_insert_with(|| {
                fs::read_to_string(file).map_or_else(|_| Vec::new(), |text| exported_names(&text))
            })
            .iter()
            .any(|name| matcher.match_symbol(name).is_some())
    }

    pub(in crate::semantic) fn source_changed(&mut self, key: &Path) {
        self.disk_names.remove(key);
        self.sdk_exports.remove(key);
    }

    #[cfg(test)]
    pub(super) fn use_empty_sdk(&mut self, generation: u64, workspace: &Workspace) {
        self.prepare(generation, workspace);
        self.sdk = SdkState::Ready(Rc::from([]));
    }

    #[cfg(test)]
    pub(super) fn sdk_files(&self) -> impl Iterator<Item = &Path> {
        self.sdk_exports.keys().map(PathBuf::as_path)
    }
}

impl Importable {
    fn from_graph(workspace: &Workspace, package: &Package) -> Option<Self> {
        let (directory, name) = package.directory_and_name();
        let vendor = directory
            .ancestors()
            .find(|part| part.file_name().is_some_and(|name| name == "vendor"));
        let path = package.import_path();
        let path = path
            .rsplit_once("/vendor/")
            .map_or(path, |(_, suffix)| suffix);
        let path = path.strip_prefix("vendor/").unwrap_or(path);
        if !valid_package(path, name) {
            return None;
        }
        Some(Self {
            path: path.into(),
            name: name.into(),
            directory: directory.to_owned(),
            vendor_parent: vendor.and_then(Path::parent).map(Path::to_owned),
            prefilter_disk: !package.load_error
                && workspace.contains_external_symbol_path(directory),
            origin: Origin::Graph(package.id.clone()),
        })
    }

    fn weight(&self, workspace: &Workspace, caller: Option<&Package>, weight: &mut Weight) {
        match &self.origin {
            Origin::Graph(id) => {
                if let Some(package) = workspace.package(id) {
                    package_weight(workspace, caller, package, weight);
                }
            }
            Origin::Sdk(_) => weight.vgo = 1,
        }
    }
}

pub(super) fn package_weight(
    workspace: &Workspace,
    caller: Option<&Package>,
    target: &Package,
    weight: &mut Weight,
) {
    weight.usage = workspace
        .direct_importers(target)
        .filter(|package| package.is_production() && workspace.is_project_package(package))
        .map(|package| &package.id)
        .collect::<HashSet<_>>()
        .len();
    weight.vgo = if target.module_path().is_some()
        && caller.and_then(Package::module_path) == target.module_path()
    {
        3
    } else if workspace.is_project_package(target) || target.is_direct_dependency() {
        2
    } else if target.is_standard() {
        1
    } else {
        0
    };
}

fn valid_package(path: &str, name: &str) -> bool {
    !matches!(name, "" | "_" | "main" | "documentation")
        && path != "builtin"
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "testdata" | "vendor")
                && !part.starts_with(['_', '.'])
        })
}

fn can_import(
    entry: &Importable,
    caller: Option<&Package>,
    directory: &Path,
    test_package: Option<&str>,
) -> bool {
    let external_test = entry.directory == directory
        && test_package.and_then(|name| name.strip_suffix("_test")) == Some(entry.name.as_str());
    if !external_test
        && (entry.directory == directory
            || caller.is_some_and(|package| package.import_path() == entry.path))
    {
        return false;
    }
    if entry
        .vendor_parent
        .as_ref()
        .is_some_and(|parent| !directory.starts_with(parent))
    {
        return false;
    }
    let Some(internal) = entry
        .path
        .split('/')
        .enumerate()
        .filter_map(|(index, part)| (part == "internal").then_some(index))
        .last()
    else {
        return true;
    };
    let parent = entry
        .path
        .split('/')
        .take(internal)
        .collect::<Vec<_>>()
        .join("/");
    if !parent.is_empty()
        && caller.is_some_and(|package| {
            let path = package.import_path();
            path == parent
                || path
                    .strip_prefix(&parent)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    {
        return true;
    }
    entry
        .directory
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "internal"))
        .and_then(Path::parent)
        .is_some_and(|parent| directory.starts_with(parent))
}

pub(super) fn assumed_package_name(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let tail = parts.next().unwrap_or("");
    let tail = if tail.strip_prefix('v').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        parts.next().unwrap_or(tail)
    } else {
        tail
    };
    let name = tail.strip_prefix("go-").unwrap_or(tail);
    let mut clean: String = name
        .chars()
        .map(|ch| if identifier_char(ch) { ch } else { '_' })
        .collect();
    if clean.is_empty() || clean == "_" {
        return "pkg".into();
    }
    if clean.chars().next().is_some_and(|ch| !identifier_start(ch)) {
        clean.insert(0, '_');
    }
    clean
}

pub(super) struct Search {
    kind: SearchKind,
    caller: Option<String>,
    graph_prepared: bool,
    sdk_prepared: bool,
    seen_paths: HashSet<String>,
    queue: VecDeque<Importable>,
    current: Option<Job>,
}

struct Job {
    entry: Importable,
    files: VecDeque<PathBuf>,
    names: HashSet<String>,
    binding: Binding,
    weight: Weight,
}

struct SymbolMatcher<'a> {
    prefix: &'a str,
    qualified_prefix: Option<&'a str>,
    allow_substring: bool,
    qualified_member: bool,
}

impl SymbolMatcher<'_> {
    fn match_symbol(&self, name: &str) -> Option<Match> {
        if self.allow_substring {
            return match_name(self.prefix, name);
        }
        if self.qualified_member {
            return match_member_hump(self.prefix, name);
        }
        let member = match_start(self.prefix, name);
        let qualified = self.qualified_prefix.and_then(|qualifier| {
            if qualifier.starts_with(self.prefix) {
                Some(Match::Prefix)
            } else {
                match_start(self.prefix, &format!("{qualifier}.{name}"))
            }
        });
        match (member, qualified) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => member.or(qualified),
        }
    }
}

enum Binding {
    Dot,
    Existing(String),
    New {
        qualifier: String,
        import: PendingImport,
    },
}

impl Binding {
    fn qualifier(&self) -> Option<&str> {
        match self {
            Self::Dot => None,
            Self::Existing(qualifier) | Self::New { qualifier, .. } => Some(qualifier),
        }
    }
}

impl Search {
    pub(super) fn new(kind: SearchKind, caller: Option<&Package>) -> Self {
        Self {
            kind,
            caller: caller.map(|package| package.id.clone()),
            graph_prepared: false,
            sdk_prepared: false,
            seen_paths: HashSet::new(),
            queue: VecDeque::new(),
            current: None,
        }
    }

    pub(super) fn advance(
        &mut self,
        cache: &mut ImportCache,
        snapshot: &mut EngineSnapshot<'_>,
        document: &DocumentSnapshot<'_>,
        generation: u64,
        prefix: &str,
        candidates: &mut Vec<Candidate>,
        caps: &CompletionClientCapabilities,
    ) -> Result<bool> {
        let workspace = snapshot
            .workspace()
            .expect("import search requires an active graph");
        let caller = self.caller.as_deref().and_then(|id| workspace.package(id));
        cache.prepare(generation, workspace);
        if !self.graph_prepared {
            self.enqueue(&cache.graph, caller, document);
            self.graph_prepared = true;
        }
        if !self.sdk_prepared
            && let Some(sdk) = cache.sdk(workspace, snapshot.root())
        {
            self.enqueue(&sdk, caller, document);
            self.sdk_prepared = true;
        }
        let docs = snapshot.docs();
        let mut resolver = snapshot.resolver(workspace);
        for _ in 0..BATCH_SIZE {
            if let Some(job) = &mut self.current {
                if let Some(file) = job.files.pop_front() {
                    match &job.entry.origin {
                        Origin::Graph(id) => {
                            let Some((caller, target)) = caller.zip(workspace.package(id)) else {
                                continue;
                            };
                            if let SearchKind::Symbols(query) = &self.kind {
                                let matcher = query.matcher(&job.binding, prefix);
                                // Keep the recovering parser for mutable project
                                // files and open buffers, including broken drafts.
                                if job.entry.prefilter_disk
                                    && !docs.contains_key(&path_key(&file))
                                    && !cache.disk_file_matches(&file, &matcher)
                                {
                                    continue;
                                }
                                let symbols = resolver
                                    .indexes
                                    .file_index(&file)
                                    .ok()
                                    .into_iter()
                                    .flat_map(|index| index.symbols.declarations())
                                    .filter(|(name, symbol)| {
                                        is_exported(name) && allows_symbol(query, &symbol.kind)
                                    })
                                    .filter_map(|(name, symbol)| {
                                        Some((
                                            name.to_owned(),
                                            symbol.clone(),
                                            matcher.match_symbol(name)?,
                                        ))
                                    })
                                    .collect::<Vec<_>>();
                                for (name, symbol, matched) in symbols {
                                    if !job.names.insert(name.clone())
                                        || matches!(job.binding, Binding::Dot)
                                            && query.shadows_dot_name(&name)
                                    {
                                        continue;
                                    }
                                    let resolved = ResolvedSymbol {
                                        package: target,
                                        file: file.clone(),
                                        symbol,
                                        receiver: None,
                                        method_receiver: None,
                                    };
                                    let candidate = resolved_candidate(
                                        &mut resolver,
                                        caller,
                                        document.indexed(),
                                        &name,
                                        &resolved,
                                        matched,
                                        query.expected.as_ref(),
                                        query.literal,
                                        caps,
                                    )?;
                                    candidates.push(job.decorate(query, prefix, candidate));
                                }
                            }
                        }
                        Origin::Sdk(_) => {
                            if let SearchKind::Symbols(query) = &self.kind {
                                let matcher = query.matcher(&job.binding, prefix);
                                let source = docs.get(&path_key(&file));
                                if source.is_none() && !cache.disk_file_matches(&file, &matcher) {
                                    continue;
                                }
                                let exports = cache.exports(
                                    resolver.indexes.parser,
                                    &file,
                                    source,
                                    &job.entry,
                                );
                                for export in exports.iter() {
                                    if !export.allowed(query)
                                        || matches!(job.binding, Binding::Dot)
                                            && query.shadows_dot_name(&export.name)
                                    {
                                        continue;
                                    }
                                    let Some(matched) = matcher.match_symbol(&export.name) else {
                                        continue;
                                    };
                                    if job.names.insert(export.name.clone()) {
                                        candidates.push(job.decorate(
                                            query,
                                            prefix,
                                            export.candidate(&file, query, matched),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                self.current = None;
            }
            let Some(entry) = self.queue.pop_front() else {
                if !self.sdk_prepared {
                    // Leave the request in the scheduler while go list
                    // runs. A short yield also avoids polling at full CPU.
                    thread::sleep(Duration::from_millis(2));
                }
                return Ok(self.sdk_prepared);
            };
            let mut weight = Weight::default();
            entry.weight(workspace, caller, &mut weight);
            match &self.kind {
                SearchKind::Path { close } => {
                    if let Some(candidate) = path_candidate(&entry, caller, prefix, *close, weight)
                    {
                        candidates.push(candidate);
                    }
                }
                SearchKind::Symbols(query) => {
                    let Some(binding) = query.binding(&entry) else {
                        continue;
                    };
                    let files = match &entry.origin {
                        Origin::Graph(id) => workspace
                            .package(id)
                            .into_iter()
                            .flat_map(|target| resolver.indexes.files(target).into_owned())
                            .filter(|file| !is_test_source(file))
                            .collect(),
                        Origin::Sdk(files) => files.iter().cloned().collect(),
                    };
                    self.current = Some(Job {
                        entry,
                        binding,
                        weight,
                        files,
                        names: HashSet::new(),
                    });
                }
            }
        }
        Ok(false)
    }

    fn enqueue(
        &mut self,
        entries: &[Importable],
        caller: Option<&Package>,
        document: &DocumentSnapshot<'_>,
    ) {
        let mut visible = BTreeMap::<&str, &Importable>::new();
        let directory = path_key(document.path().parent().unwrap_or_else(|| Path::new("")));
        let test = is_test_source(document.path())
            .then(|| package_name(document.indexed()))
            .flatten();
        for entry in entries
            .iter()
            .filter(|entry| can_import(entry, caller, &directory, test))
        {
            if matches!(&self.kind, SearchKind::Symbols(query) if !query.matches_package(entry)) {
                continue;
            }
            match visible.entry(entry.path.as_str()) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(entry);
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    let depth = |package: &Importable| {
                        package
                            .vendor_parent
                            .as_ref()
                            .map_or(0, |path| path.components().count())
                    };
                    if depth(entry) > depth(slot.get()) {
                        slot.insert(entry);
                    }
                }
            }
        }
        self.queue.extend(
            visible
                .into_values()
                .filter(|entry| self.seen_paths.insert(entry.path.clone()))
                .cloned(),
        );
    }
}

impl SymbolSearch {
    fn matcher<'a>(&self, binding: &'a Binding, prefix: &'a str) -> SymbolMatcher<'a> {
        let qualified_prefix = binding.qualifier().filter(|qualifier| {
            matches!(self.selection, Selection::Name { .. })
                && prefix
                    .chars()
                    .next()
                    .zip(qualifier.chars().next())
                    .is_some_and(|(left, right)| left.to_lowercase().eq(right.to_lowercase()))
        });
        SymbolMatcher {
            prefix,
            qualified_prefix,
            allow_substring: matches!(binding, Binding::Dot)
                || matches!(
                    (&self.selection, binding),
                    (Selection::Qualifier { .. }, Binding::Existing(_))
                ),
            qualified_member: matches!(self.selection, Selection::Qualifier { .. }),
        }
    }

    fn shadows_dot_name(&self, name: &str) -> bool {
        self.declared.contains(name) || self.imports.iter().any(|import| import.qualifier == name)
    }

    fn matches_package(&self, entry: &Importable) -> bool {
        match &self.selection {
            Selection::Name { auto_import } => {
                *auto_import
                    || self.imports.iter().any(|import| {
                        import.path == entry.path && import.qualifier == "." && !import.in_graph
                    })
            }
            Selection::Qualifier { name, .. } => {
                let existing = self
                    .imports
                    .iter()
                    .find(|import| &import.qualifier == name && !self.declared.contains(name));
                existing.map_or_else(
                    || {
                        entry.name.starts_with(name)
                            || assumed_package_name(&entry.path).starts_with(name)
                    },
                    |import| entry.path == import.path,
                )
            }
        }
    }

    fn binding(&self, entry: &Importable) -> Option<Binding> {
        let imports = || {
            self.imports
                .iter()
                .filter(|import| import.path == entry.path)
        };
        if let Some(dot) = imports().find(|import| import.qualifier == ".") {
            return (matches!(self.selection, Selection::Name { .. }) && !dot.in_graph)
                .then_some(Binding::Dot);
        }
        if let Some(import) = imports().find(|import| import.qualifier != "_") {
            if self.declared.contains(&import.qualifier) {
                return None;
            }
            return Some(Binding::Existing(import.qualifier.clone()));
        }
        let clean = |qualifier: &str| {
            !self.declared.contains(qualifier)
                && self
                    .imports
                    .iter()
                    .all(|import| import.qualifier != qualifier)
                && qualifier != "init"
        };
        let qualifier = if clean(&entry.name) {
            entry.name.clone()
        } else {
            (2..)
                .map(|suffix| format!("{}{suffix}", entry.name))
                .find(|name| clean(name))?
        };
        let alias = (qualifier != entry.name || qualifier != assumed_package_name(&entry.path))
            .then(|| qualifier.clone());
        Some(Binding::New {
            qualifier,
            import: PendingImport {
                path: entry.path.clone(),
                alias,
            },
        })
    }
}

impl Job {
    fn decorate(&self, query: &SymbolSearch, prefix: &str, mut candidate: Candidate) -> Candidate {
        candidate.import_path = Some(self.entry.path.clone());
        candidate.weight.usage = self.weight.usage;
        candidate.weight.vgo = self.weight.vgo;
        let qualifier = match &self.binding {
            Binding::Dot => return candidate,
            Binding::Existing(qualifier) => qualifier,
            Binding::New { qualifier, import } => {
                candidate.weight.priority += AUTO_IMPORT - SYMBOL;
                candidate.pending_import = Some(import.clone());
                qualifier
            }
        };
        candidate.qualified = true;
        match &query.selection {
            Selection::Name { .. } => {
                candidate.qualifier = Some(qualifier.clone());
                if match_start(prefix, &candidate.label).is_none() {
                    candidate.filter = Some(format!("{qualifier}.{}", candidate.label));
                }
            }
            Selection::Qualifier { name, start }
                if matches!(self.binding, Binding::New { .. }) || name != qualifier =>
            {
                // The typed qualifier is part of the edit and therefore
                // also part of the prefix the LSP client filters on.
                candidate.qualifier = Some(qualifier.clone());
                candidate.replace_start = Some(*start);
                candidate.filter = Some(format!("{name}.{}", candidate.label));
            }
            Selection::Qualifier { .. } => {}
        }
        candidate
    }
}

fn allows_symbol(query: &SymbolSearch, kind: &SymbolKind) -> bool {
    !(query.types_only || query.address_of) || matches!(kind, SymbolKind::Type)
}

fn path_candidate(
    entry: &Importable,
    caller: Option<&Package>,
    prefix: &str,
    close: Option<char>,
    mut weight: Weight,
) -> Option<Candidate> {
    if prefix.starts_with("./") || prefix.starts_with("../") {
        return None;
    }
    let (head, tail) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
    let full = match_start(prefix, &entry.path);
    let last = match prefix.rsplit_once('/') {
        Some((directory, segment)) if directory == head => {
            match_start(segment, tail).map(|matched| (matched, entry.path.clone()))
        }
        Some((directory, segment)) if !segment.is_empty() && !directory.contains('/') => head
            .split('/')
            .any(|part| part.ends_with(directory))
            .then(|| match_start(segment, tail))
            .flatten()
            .map(|_| (Match::CamelHump, entry.path.clone())),
        Some(_) => None,
        None => match_start(prefix, tail).map(|matched| (matched, tail.to_owned())),
    };
    let (matched, filter) = match (full, last) {
        (Some(full), Some((segment, filter))) if segment < full => (segment, filter),
        (Some(full), _) => (full, entry.path.clone()),
        (None, Some(segment)) => segment,
        (None, None) => return None,
    };
    weight.priority = package_priority(caller.map_or("", Package::import_path), &entry.path);
    let mut candidate = Candidate::new(&entry.path, Kind::MODULE, matched, weight.priority);
    candidate.weight = weight;
    candidate.filter = Some(filter);
    candidate.import_path = Some(entry.path.clone());
    candidate.insert = Insert::ImportPath { close };
    Some(candidate)
}

fn start_sdk_inventory(root: &Path, src: &Path) -> Option<Receiver<Vec<Importable>>> {
    let src = src.to_owned();
    let root = root.to_owned();
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("go-completion-sdk".into())
        .spawn(move || {
            let _ = sender.send(sdk_inventory(&root, &src).unwrap_or_default());
        })
        .ok()?;
    Some(receiver)
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct SdkPackage {
    dir: PathBuf,
    import_path: String,
    name: String,
    go_files: Vec<PathBuf>,
    cgo_files: Vec<PathBuf>,
}

fn sdk_inventory(root: &Path, src: &Path) -> Result<Vec<Importable>> {
    // Use the Go command's build view for SDK metadata. It can select the
    // right GOOS/GOARCH, cgo and tagged files without parsing declarations.
    let output = Command::new("go")
        .args([
            "list",
            "-e",
            "-json=Dir,ImportPath,Name,GoFiles,CgoFiles",
            "std",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let src = path_key(src);
    let entries = serde_json::Deserializer::from_slice(&output.stdout).into_iter::<SdkPackage>();
    let mut packages = Vec::new();
    for entry in entries {
        let entry = entry?;
        let directory = path_key(&entry.dir);
        if !directory.starts_with(&src)
            || entry.import_path == "cmd"
            || entry.import_path.starts_with("cmd/")
            || !valid_package(&entry.import_path, &entry.name)
        {
            continue;
        }
        let files = entry
            .go_files
            .into_iter()
            .chain(entry.cgo_files)
            .filter(|path| is_symbol_source(path) && !is_test_source(path))
            .map(|file| entry.dir.join(file))
            .collect::<Vec<_>>();
        if !files.is_empty() {
            packages.push(Importable {
                path: entry.import_path,
                name: entry.name,
                directory,
                origin: Origin::Sdk(files),
                vendor_parent: None,
                prefilter_disk: true,
            });
        }
    }
    Ok(packages)
}

struct Export {
    name: String,
    range: Range,
    description: Description,
    is_type: bool,
    braces: bool,
    parameters: Option<bool>,
    value: Option<String>,
}

impl Export {
    fn allowed(&self, query: &SymbolSearch) -> bool {
        !(query.types_only || query.address_of) || self.is_type
    }

    fn candidate(&self, file: &Path, query: &SymbolSearch, matched: Match) -> Candidate {
        let mut candidate = Candidate::new(&self.name, self.description.kind, matched, SYMBOL);
        candidate.detail = self.description.detail.clone();
        candidate.label_suffix = self.description.label_suffix.clone();
        candidate.deprecated = self.description.deprecated;
        candidate.target = Some(Target {
            file: file.to_owned(),
            range: self.range,
        });
        if let Some(has_parameters) = self.parameters {
            candidate.insert = Insert::Call { has_parameters };
        } else if self.braces && query.literal {
            candidate.insert = Insert::Braces;
        }
        if let Some((expected, value)) = query.expected.as_ref().zip(self.value.as_deref()) {
            candidate.weight.priority += expected.keys_weight(value, Some(value));
        }
        candidate
    }
}

fn sdk_exports(source: &IndexedSource, entry: &Importable) -> Rc<[Export]> {
    if package_name(source) != Some(entry.name.as_str()) {
        return Rc::from([]);
    }
    let declarations = &source.declarations().symbols;
    declarations.declarations().filter(|(name, _)| is_exported(name)).map(|(name, symbol)| {
        let mut braces = false;
        let mut value = None;
        let kind = match &symbol.kind {
            SymbolKind::Type => {
                value = Some(named_key(&entry.path, name));
                if let Some(details) = declarations.type_details(name) {
                    let base = details.alias.as_ref().or(details.underlying.as_ref());
                    braces = !details.interface && details.parameters.is_empty()
                        && base.is_none_or(|ty| ty.pointers == 0 && matches!(&ty.kind,
                            TypeRefKind::Container(container, _) if !matches!(container.as_ref(), Container::Channel { .. })
                        ));
                    if details.interface { Kind::INTERFACE } else if base.is_none() { Kind::STRUCT } else { Kind::CLASS }
                } else { Kind::CLASS }
            }
            SymbolKind::Function(signature) => {
                if let [Some(ty)] = signature.results.as_ref() {
                    value = sdk_type_key(source, &entry.path, ty);
                }
                Kind::FUNCTION
            }
            SymbolKind::Value(Some(ty)) => {
                value = sdk_type_key(source, &entry.path, ty);
                Kind::VARIABLE
            }
            _ => Kind::VARIABLE,
        };
        let description = describe(source, symbol.range, name, kind, true);
        let parameters = matches!(symbol.kind, SymbolKind::Function(_)).then_some(description.has_parameters);
        Export {
            name: name.to_owned(), range: symbol.range, is_type: matches!(symbol.kind, SymbolKind::Type),
            description, parameters, braces, value,
        }
    }).collect()
}

fn sdk_type_key(source: &Source, package: &str, ty: &TypeRef) -> Option<String> {
    let child = |ty| sdk_type_key(source, package, ty);
    let key = match &ty.kind {
        TypeRefKind::Named(named) => {
            let owner = if let Some(qualifier) = &named.qualifier {
                &source
                    .imports
                    .iter()
                    .find(|import| {
                        import.alias.as_ref().map_or_else(
                            || assumed_package_name(&import.path) == *qualifier,
                            |alias| alias == qualifier,
                        )
                    })?
                    .path
            } else {
                package
            };
            let name = named_key(owner, &named.name);
            if named.arguments.is_empty() {
                name
            } else {
                let arguments = named
                    .arguments
                    .iter()
                    .map(|ty| ty.as_ref().and_then(child))
                    .collect::<Option<Vec<_>>>()?;
                format!("{name}[{}]", arguments.join(","))
            }
        }
        TypeRefKind::Container(container, _) => match container.as_ref() {
            Container::Slice(element) => format!("[]{}", child(element)?),
            Container::Array { element, length } => {
                format!(
                    "[{}]{}",
                    length.as_deref().unwrap_or("..."),
                    child(element)?
                )
            }
            Container::Map { key, value } => format!("map[{}]{}", child(key)?, child(value)?),
            Container::Channel {
                element,
                receive,
                send,
            } => format!("chan{receive}{send}:{}", child(element)?),
        },
        _ => return None,
    };
    Some(format!("{}{key}", "*".repeat(ty.pointers.into())))
}
