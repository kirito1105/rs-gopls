use std::collections::{BTreeSet, HashMap, hash_map::Entry};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, ensure};
use lsp_types::{Range, SymbolInformation, Uri};
use tree_sitter::Parser;

use super::document_symbols::symbol_information;
use super::{Engine, GraphChanges, QueryProgress, Search, Source, text_fingerprint};
use crate::path_uri;
use crate::symbols::{OutlineKind, OutlineSymbol};
use crate::workspace::{Workspace, is_symbol_source, path_key};

const INDEX_FILE_BATCH: usize = 8;
const SEARCH_FILE_BATCH: usize = 128;
const SEARCH_BUDGET: Duration = Duration::from_millis(2);

#[derive(Default)]
pub(super) struct WorkspaceSymbolIndex {
    files: HashMap<PathBuf, CachedFile>,
    paths: Option<Vec<PathBuf>>,
    pending: BTreeSet<PathBuf>,
    project_current: bool,
    revision: u64,
}

struct CachedFile {
    path: PathBuf,
    project_file: bool,
    disk: Option<DiskSymbols>,
    overlay: Option<(i32, Rc<WorkspaceFileSymbols>)>,
}

struct DiskSymbols {
    stamp: Option<FileStamp>,
    fingerprint: u64,
    symbols: Rc<WorkspaceFileSymbols>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
}

struct WorkspaceFileSymbols {
    key: PathBuf,
    uri: Uri,
    package: Option<ContainerName>,
    entries: Vec<WorkspaceSymbolEntry>,
}

struct WorkspaceSymbolEntry {
    name: SearchName,
    kind: OutlineKind,
    owner: Option<Rc<ContainerName>>,
    range: Range,
}

struct ContainerName {
    display: String,
    package: Option<String>,
    owner: Option<SearchName>,
}

pub struct WorkspaceSymbolSearch {
    query: String,
    revision: u64,
    next_file: usize,
    matches: Vec<RankedMatch>,
}

struct RankedMatch {
    rank: MatchRank,
    qualifier_rank: Option<MatchRank>,
    file: Rc<WorkspaceFileSymbols>,
    entry: usize,
}

impl Engine {
    pub fn workspace_symbols(&mut self, query: &str) -> QueryProgress<Vec<SymbolInformation>> {
        let query = query.trim();
        if query.is_empty() {
            return QueryProgress::Ready(Vec::new());
        }

        self.include_tests = true;
        let load = self.workspace_load();
        let Some(workspace) = &self.workspace else {
            return QueryProgress::Pending(load);
        };
        if !workspace.includes_tests() || !matches!(self.graph_changes, GraphChanges::Clean) {
            return QueryProgress::Pending(load);
        }

        let index = &mut self.workspace_symbol_index;
        if !index.project_current {
            index.synchronize(workspace, &self.root, &self.documents);
        }
        index
            .paths
            .get_or_insert_with(|| index.files.keys().cloned().collect());
        QueryProgress::Searching(Search::WorkspaceSymbols(WorkspaceSymbolSearch {
            query: fold(query),
            revision: index.revision,
            next_file: 0,
            matches: Vec::new(),
        }))
    }

    pub(super) fn advance_workspace_symbol_search(
        &mut self,
        search: &mut WorkspaceSymbolSearch,
    ) -> Result<bool> {
        let index = &mut self.workspace_symbol_index;
        ensure!(
            index.project_current
                && index.revision == search.revision
                && matches!(self.graph_changes, GraphChanges::Clean),
            "workspace changed during symbol search"
        );
        let started = Instant::now();
        // All concurrent queries share indexing work. A file only becomes
        // searchable after its entire current projection has been installed.
        for _ in 0..INDEX_FILE_BATCH {
            let Some(key) = index.pending.first() else {
                break;
            };
            let file = index
                .files
                .get_mut(key)
                .expect("pending symbol index file exists");
            file.refresh(key, self.documents.get(key), &mut self.parser)?;
            index.pending.pop_first();
            if started.elapsed() >= SEARCH_BUDGET {
                return Ok(false);
            }
        }
        if !index.pending.is_empty() {
            return Ok(false);
        }

        let paths = index
            .paths
            .as_deref()
            .expect("workspace symbol search paths prepared");
        let (qualifier, name) = match search.query.rsplit_once('.') {
            Some((qualifier, name)) => (Some(qualifier), name),
            None => (None, search.query.as_str()),
        };
        for _ in 0..SEARCH_FILE_BATCH {
            let Some(key) = paths.get(search.next_file) else {
                break;
            };
            search.next_file += 1;
            if let Some(file) = index.files[key].symbols() {
                for (entry, symbol) in file.entries.iter().enumerate() {
                    if let Some(rank) = symbol.name.rank(name) {
                        let qualifier_rank = if let Some(qualifier) = qualifier {
                            let Some(qualifier_rank) = file
                                .container(symbol)
                                .and_then(|container| container.rank(qualifier))
                            else {
                                continue;
                            };
                            Some(qualifier_rank)
                        } else {
                            None
                        };
                        search.matches.push(RankedMatch {
                            rank: rank.max(qualifier_rank.unwrap_or(MatchRank::Exact)),
                            qualifier_rank,
                            file: Rc::clone(file),
                            entry,
                        });
                    }
                }
            }
            if started.elapsed() >= SEARCH_BUDGET {
                break;
            }
        }
        Ok(search.next_file == paths.len())
    }
}

impl WorkspaceSymbolIndex {
    pub(super) fn graph_changed(&mut self) {
        self.project_current = false;
        self.revision += 1;
    }

    fn synchronize(
        &mut self,
        workspace: &Workspace,
        root: &Path,
        documents: &HashMap<PathBuf, Source>,
    ) {
        let root = path_key(root);
        let mut previous = std::mem::take(&mut self.files);
        for (key, path) in workspace.project_symbol_files() {
            let file = previous
                .remove(&key)
                .filter(|file| file.project_file && file.path == path)
                .unwrap_or_else(|| CachedFile::new(path, true));
            self.files.insert(key, file);
        }
        for (key, source) in documents {
            if !self.files.contains_key(key)
                && in_draft_scope(workspace, &root, key)
                && !source.path.is_file()
            {
                let file = previous
                    .remove(key)
                    .filter(|file| !file.project_file)
                    .unwrap_or_else(|| CachedFile::new(source.path.clone(), false));
                self.files.insert(key.clone(), file);
            }
        }
        self.pending = self.files.keys().cloned().collect();
        self.paths = None;
        self.project_current = true;
    }

    pub(super) fn document_opened(
        &mut self,
        workspace: Option<&Workspace>,
        root: &Path,
        key: &Path,
        path: &Path,
        draft: bool,
    ) {
        if self.project_current
            && draft
            && let Some(workspace) = workspace
            && in_draft_scope(workspace, &path_key(root), key)
            && let Entry::Vacant(slot) = self.files.entry(key.to_owned())
        {
            slot.insert(CachedFile::new(path.to_owned(), false));
            self.paths = None;
        }
        self.document_edited(key);
    }

    pub(super) fn document_edited(&mut self, key: &Path) {
        if let Some(file) = self.files.get_mut(key) {
            file.overlay = None;
            self.pending.insert(key.to_owned());
            self.revision += 1;
        }
    }

    pub(super) fn document_closed(&mut self, key: &Path) {
        let Some(file) = self.files.get_mut(key) else {
            return;
        };
        file.overlay = None;
        if file.project_file {
            self.disk_changed(key, false);
        } else {
            self.files.remove(key);
            self.pending.remove(key);
            self.paths = None;
            self.revision += 1;
        }
    }

    pub(super) fn disk_changed(&mut self, key: &Path, open: bool) {
        if let Some(file) = self.files.get_mut(key) {
            if let Some(disk) = &mut file.disk {
                disk.stamp = None;
            }
            if !open {
                self.pending.insert(key.to_owned());
                self.revision += 1;
            }
        }
    }
}

fn in_draft_scope(workspace: &Workspace, root: &Path, key: &Path) -> bool {
    is_symbol_source(key)
        && (key.starts_with(root) || workspace.contains_project_path(key))
        && !workspace.contains_external_symbol_path(key)
}

impl CachedFile {
    fn new(path: PathBuf, project_file: bool) -> Self {
        Self {
            path,
            project_file,
            disk: None,
            overlay: None,
        }
    }

    fn symbols(&self) -> Option<&Rc<WorkspaceFileSymbols>> {
        self.overlay
            .as_ref()
            .map(|(_, symbols)| symbols)
            .or_else(|| self.disk.as_ref().map(|disk| &disk.symbols))
    }

    fn refresh(
        &mut self,
        key: &Path,
        document: Option<&Source>,
        parser: &mut Parser,
    ) -> Result<()> {
        if let Some(source) = document {
            let version = source
                .document_version
                .expect("open document has a client version");
            if self
                .overlay
                .as_ref()
                .is_none_or(|(previous, _)| *previous != version)
            {
                self.overlay = Some((
                    version,
                    Rc::new(WorkspaceFileSymbols::new(key, source, self.project_file)?),
                ));
            }
            return Ok(());
        }

        self.overlay = None;
        let stamp = fs::metadata(&self.path).ok().and_then(|metadata| {
            Some(FileStamp {
                modified: metadata.modified().ok()?,
                len: metadata.len(),
            })
        });
        if stamp.is_some() && self.disk.as_ref().is_some_and(|disk| disk.stamp == stamp) {
            return Ok(());
        }
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) => {
                self.disk = None;
                return Ok(());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", self.path.display()));
            }
        };
        let fingerprint = text_fingerprint(&text);
        if let Some(disk) = &mut self.disk
            && disk.fingerprint == fingerprint
        {
            disk.stamp = stamp;
        } else {
            let source = Source::parse(parser, self.path.clone(), text, None)?;
            self.disk = Some(DiskSymbols {
                stamp,
                fingerprint,
                symbols: Rc::new(WorkspaceFileSymbols::new(key, &source, true)?),
            });
        }
        Ok(())
    }
}

impl WorkspaceFileSymbols {
    fn new(key: &Path, source: &Source, project_file: bool) -> Result<Self> {
        let outline = source.outline();
        let package = outline.package.as_deref();
        let mut file = Self {
            key: key.to_owned(),
            uri: path_uri(&source.path)?,
            package: package.map(|package| ContainerName::new(Some(package), None)),
            entries: Vec::new(),
        };
        if !project_file && package.is_none() {
            return Ok(file);
        }
        let mut owners = HashMap::new();
        for symbol in &outline.symbols {
            let receiver = symbol
                .receiver
                .as_ref()
                .map(|receiver| type_container(&mut owners, package, &receiver.base));
            file.push(source, symbol, receiver);
            if !symbol.children().is_empty() {
                let owner = type_container(&mut owners, package, &symbol.name);
                file.push_members(source, symbol.children(), &owner);
            }
        }
        Ok(file)
    }

    fn push_members(
        &mut self,
        source: &Source,
        children: &[OutlineSymbol],
        owner: &Rc<ContainerName>,
    ) {
        let mut previous_children = &[][..];
        for symbol in children {
            self.push(source, symbol, Some(Rc::clone(owner)));
            let children = symbol.children();
            // A multi-name field shares one AST for its inline type.
            if !children.is_empty() && !std::ptr::eq(children, previous_children) {
                self.push_members(source, children, owner);
            }
            previous_children = children;
        }
    }

    fn push(&mut self, source: &Source, symbol: &OutlineSymbol, owner: Option<Rc<ContainerName>>) {
        self.entries.push(WorkspaceSymbolEntry {
            name: SearchName::new(&symbol.name),
            kind: symbol.kind,
            owner,
            range: source.byte_range(symbol.selection_range.clone()),
        });
    }

    fn container<'a>(&'a self, entry: &'a WorkspaceSymbolEntry) -> Option<&'a ContainerName> {
        entry.owner.as_deref().or(self.package.as_ref())
    }
}

fn type_container<'a>(
    owners: &mut HashMap<&'a str, Rc<ContainerName>>,
    package: Option<&str>,
    name: &'a str,
) -> Rc<ContainerName> {
    Rc::clone(
        owners
            .entry(name)
            .or_insert_with(|| Rc::new(ContainerName::new(package, Some(name)))),
    )
}

impl ContainerName {
    fn new(package: Option<&str>, owner: Option<&str>) -> Self {
        let display = match (package, owner) {
            (Some(package), Some(owner)) => format!("{package}.{owner}"),
            (Some(name), None) | (None, Some(name)) => name.to_owned(),
            (None, None) => unreachable!("container requires a package or type name"),
        };
        Self {
            display,
            package: package.map(fold),
            owner: owner.map(SearchName::new),
        }
    }

    fn rank(&self, qualifier: &str) -> Option<MatchRank> {
        match (&self.owner, qualifier.rsplit_once('.')) {
            (Some(owner), None) if !qualifier.is_empty() => owner.rank(qualifier),
            (Some(owner), Some((package, name)))
                if !name.is_empty() && self.package.as_deref() == Some(package) =>
            {
                owner.rank(name)
            }
            (None, None) if self.package.as_deref() == Some(qualifier) => Some(MatchRank::Exact),
            _ => None,
        }
    }
}

impl WorkspaceSymbolSearch {
    pub(super) fn into_matches(mut self) -> Vec<SymbolInformation> {
        self.matches
            .sort_unstable_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        self.matches
            .into_iter()
            .map(|matched| {
                let file = matched.file;
                let entry = &file.entries[matched.entry];
                symbol_information(
                    entry.name.display.to_string(),
                    entry.kind,
                    file.container(entry)
                        .map(|container| container.display.clone()),
                    file.uri.clone(),
                    entry.range,
                )
            })
            .collect()
    }
}

impl RankedMatch {
    fn sort_key(&self) -> impl Ord + '_ {
        let entry = &self.file.entries[self.entry];
        (
            self.rank,
            self.qualifier_rank,
            entry.name.display.as_ref(),
            self.file
                .container(entry)
                .map(|container| container.display.as_str()),
            &self.file.key,
            entry.range.start,
            entry.range.end,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MatchRank {
    Exact,
    Prefix,
    Substring,
    Camel,
    Subsequence,
}

struct SearchName {
    display: Box<str>,
    folded: Option<Box<str>>,
    initials: Box<str>,
}

impl SearchName {
    fn new(name: &str) -> Self {
        let mut folded = String::with_capacity(name.len());
        let mut chars = name.chars().peekable();
        let mut previous: Option<char> = None;
        let mut initials = String::new();
        while let Some(ch) = chars.next() {
            let byte = folded.len();
            folded.extend(ch.to_lowercase());
            if ch.is_alphanumeric()
                && previous.is_none_or(|prev| {
                    !prev.is_alphanumeric()
                        || (ch.is_uppercase()
                            && (prev.is_lowercase()
                                || chars.peek().is_some_and(|next| next.is_lowercase())))
                        || ch.is_numeric() != prev.is_numeric()
                })
            {
                initials.push_str(&folded[byte..]);
            }
            previous = Some(ch);
        }
        Self {
            display: name.into(),
            folded: (folded != name).then(|| folded.into_boxed_str()),
            initials: initials.into_boxed_str(),
        }
    }

    fn rank(&self, query: &str) -> Option<MatchRank> {
        let name = self.folded.as_deref().unwrap_or(&self.display);
        if name == query {
            Some(MatchRank::Exact)
        } else if name.starts_with(query) {
            Some(MatchRank::Prefix)
        } else if name.contains(query) {
            Some(MatchRank::Substring)
        } else if is_subsequence(query, &self.initials) {
            Some(MatchRank::Camel)
        } else if is_subsequence(query, name) {
            Some(MatchRank::Subsequence)
        } else {
            None
        }
    }
}

fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
}

fn is_subsequence(query: &str, name: &str) -> bool {
    if query.len() > name.len() {
        return false;
    }
    let mut available = name.chars();
    query.chars().all(|wanted| available.any(|ch| ch == wanted))
}

#[cfg(test)]
mod tests {
    use lsp_types::{FileChangeType, Position, SymbolKind, TextDocumentContentChangeEvent};
    use serde_json::json;

    use super::*;
    use crate::semantic::SearchResult;
    use crate::test_support::TestProject;

    fn query(engine: &mut Engine, name: &str) -> Vec<SymbolInformation> {
        let mut search = match engine.workspace_symbols(name) {
            QueryProgress::Ready(symbols) => return symbols,
            QueryProgress::Searching(search) => search,
            QueryProgress::Pending(load) => panic!("missing workspace for query {name}: {load:?}"),
        };
        while !engine.advance_search(&mut search).unwrap() {}
        let SearchResult::WorkspaceSymbols(symbols) = search.into_result() else {
            panic!("expected workspace symbol results");
        };
        symbols
    }

    fn install(engine: &mut Engine, workspace: Workspace) {
        assert!(engine.install_workspace(engine.workspace_load(), workspace));
    }

    fn replace(engine: &mut Engine, path: &Path, text: &str, version: i32) {
        engine
            .edit(
                path,
                &[TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.into(),
                }],
                version,
            )
            .unwrap();
    }

    #[test]
    fn ranks_case_insensitive_word_matches() {
        use MatchRank::*;
        for (name, query, expected) in [
            ("HS", "hs", Some(Exact)),
            ("HSWide", "hs", Some(Prefix)),
            ("HandleStart", "hs", Some(Camel)),
            ("MakeHTTPServer", "hs", Some(Camel)),
            ("When_Sorted", "sort", Some(Substring)),
            ("densehsvalue", "hs", Some(Substring)),
            ("hexes", "hs", Some(Subsequence)),
            ("İtemCat", "cat", Some(Substring)),
            ("Öpen", "ÖPEN", Some(Exact)),
            ("Else", "hs", None),
        ] {
            assert_eq!(
                SearchName::new(name).rank(&fold(query)),
                expected,
                "{name} {query}"
            );
        }
    }

    #[test]
    fn filters_project_files_and_deduplicates_test_compilations() {
        let project = TestProject::new();
        let first = project.root.join("first");
        let second = project.root.join("second");
        let original = project.write(
            "first/ordinary.go",
            "package sample\r\nvar Emoji = \"😀\"; var Same int\r\n",
        );
        let other = project.write(
            "second/ordinary.go",
            "package sample\nvar Same int\nvar Same int\n",
        );
        project.write(
            "first/inner_test.go",
            "package sample\nfunc TestNeedle() {}\n",
        );
        project.write(
            "first/outer_test.go",
            "package sample_test\nfunc TestNeedle() {}\n",
        );
        let synthetic_outside = project.write(
            "build/testmain.go",
            "package main\nvar Same int\nfunc TestNeedle() {}\n",
        );
        let vendor = project.write(
            "first/vendor/dep/a.go",
            "package sample\nvar Same int\nfunc TestNeedle() {}\n",
        );
        let outside = project.write(
            "other/a.go",
            "package sample\nvar Same int\nfunc TestNeedle() {}\n",
        );
        let goroot = project.write(
            "first/sdk/a.go",
            "package sample\nvar Same int\nfunc TestNeedle() {}\n",
        );
        let modcache = project.write(
            "first/deps/dep@v1/a.go",
            "package sample\nvar Same int\nfunc TestNeedle() {}\n",
        );
        project.write(
            "first/_testmain.go",
            "package main\nvar Same int\nfunc TestNeedle() {}\n",
        );
        let listing = [
            json!({"Dir": first, "Name": "sample", "ImportPath": "example.test/first", "GoFiles": ["ordinary.go"]}),
            json!({"Dir": first, "Name": "sample", "ImportPath": "example.test/first [example.test/first.test]", "ForTest": "example.test/first", "GoFiles": ["ordinary.go", "inner_test.go"]}),
            json!({"Dir": first, "Name": "sample_test", "ImportPath": "example.test/first_test [example.test/first.test]", "ForTest": "example.test/first", "GoFiles": ["outer_test.go"]}),
            json!({"Dir": first, "Name": "main", "ImportPath": "example.test/first.test", "GoFiles": [json!("_testmain.go"), json!(synthetic_outside)]}),
            json!({"Dir": second, "Name": "sample", "ImportPath": "example.test/second", "GoFiles": ["ordinary.go"]}),
            json!({"Dir": vendor.parent(), "Name": "sample", "ImportPath": "example.test/vendor", "GoFiles": ["a.go"]}),
            json!({"Dir": outside.parent(), "Name": "sample", "ImportPath": "example.test/outside", "GoFiles": ["a.go"]}),
            json!({"Dir": goroot.parent(), "Name": "sample", "ImportPath": "sdk", "GoFiles": ["a.go"], "Goroot": true}),
            json!({"Dir": modcache.parent(), "Name": "sample", "ImportPath": "example.test/deps", "GoFiles": ["a.go"], "Module": {"Version": "v1.0.0"}}),
        ].into_iter().map(|value| value.to_string()).collect::<Vec<_>>().join("\n");
        let mut engine = Engine::new(project.root.clone());
        install(
            &mut engine,
            Workspace::from_test_listing(&[first, second], &listing, true),
        );
        for external in [&vendor, &goroot, &modcache] {
            engine
                .open(
                    external.with_file_name("draft.go"),
                    "package sample\nvar Same int\n".into(),
                    1,
                )
                .unwrap();
        }

        let same = query(&mut engine, "sAmPlE.SaMe");
        assert_eq!(same.len(), 3);
        assert_eq!(same[0].location.uri, path_uri(&original).unwrap());
        assert_eq!(
            same[0].location.range,
            Range::new(Position::new(1, 22), Position::new(1, 26))
        );
        assert_eq!(same[1].location.uri, path_uri(&other).unwrap());
        assert_eq!(same[1].location.range.start, Position::new(1, 4));
        assert_eq!(same[2].location.uri, same[1].location.uri);
        assert_eq!(same[2].location.range.start, Position::new(2, 4));
        assert_eq!(query(&mut engine, "same"), same);
        let tests = query(&mut engine, "TestNeedle");
        assert_eq!(
            tests
                .iter()
                .map(|symbol| symbol.container_name.as_deref())
                .collect::<Vec<_>>(),
            [Some("sample"), Some("sample_test")]
        );
    }

    #[test]
    fn searches_declarations_and_type_members_with_stable_ranking() {
        let project = TestProject::new();
        project.write("outline.go", concat!(
            "package sample\nimport ExcludedAlias \"io\"\n",
            "const Limit = 1\nvar Visible int\nvar _ int\n",
            "type Count int\ntype Rename = Count\n",
            "type Widget[ExcludedType any] struct { *pkg.Embedded; Caption, Title string; Left, Right struct{ Nested int } }\n",
            "type Interface interface { io.EmbeddedFace; ReadFile(ExcludedParam int) bool }\n",
            "func HS() { var ExcludedLocal int }\nfunc HSWide() {}\nfunc densehsvalue() {}\nfunc hexes() {}\n",
            "func init() {}\nfunc init() {}\n",
        ));
        project.write(
            "method.go",
            concat!(
                "package sample\nfunc (*Widget[T]) HandleStart() {}\n",
                "type WidgetCore[T any] struct{}\n",
                "func (*WidgetCore[T]) HandleStart() {}\n",
                "type MemoryWidget[T any] struct{}\n",
                "func (*MemoryWidget[T]) HandleStart() {}\n",
            ),
        );
        let mut engine = Engine::new(project.root.clone());
        install(
            &mut engine,
            project.workspace(&["outline.go", "method.go"], true),
        );
        let names = |results: Vec<SymbolInformation>| {
            results
                .into_iter()
                .map(|symbol| symbol.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(query(&mut engine, "HS")),
            [
                "HS",
                "HSWide",
                "densehsvalue",
                "HandleStart",
                "HandleStart",
                "HandleStart",
                "hexes"
            ]
        );
        for (text, name, kind, owner) in [
            ("sAmPlE.LiMiT", "Limit", SymbolKind::CONSTANT, "sample"),
            ("sample.Visible", "Visible", SymbolKind::VARIABLE, "sample"),
            ("sample.Count", "Count", SymbolKind::CLASS, "sample"),
            ("sample.Rename", "Rename", SymbolKind::CLASS, "sample"),
            ("sample.Widget", "Widget", SymbolKind::STRUCT, "sample"),
            (
                "sample.Interface",
                "Interface",
                SymbolKind::INTERFACE,
                "sample",
            ),
            (
                "widget.handlestart",
                "HandleStart",
                SymbolKind::METHOD,
                "sample.Widget",
            ),
            (
                "sample.Widget.HS",
                "HandleStart",
                SymbolKind::METHOD,
                "sample.Widget",
            ),
            (
                "sample.Widget.Embedded",
                "Embedded",
                SymbolKind::FIELD,
                "sample.Widget",
            ),
            (
                "widget.Caption",
                "Caption",
                SymbolKind::FIELD,
                "sample.Widget",
            ),
            ("widget.Title", "Title", SymbolKind::FIELD, "sample.Widget"),
            (
                "widget.Nested",
                "Nested",
                SymbolKind::FIELD,
                "sample.Widget",
            ),
            (
                "interface.ReadFile",
                "ReadFile",
                SymbolKind::METHOD,
                "sample.Interface",
            ),
            (
                "interface.EmbeddedFace",
                "EmbeddedFace",
                SymbolKind::INTERFACE,
                "sample.Interface",
            ),
            ("sample.HS", "HS", SymbolKind::FUNCTION, "sample"),
        ] {
            let results = query(&mut engine, text);
            let first = results
                .first()
                .unwrap_or_else(|| panic!("missing match for {text}"));
            assert_eq!(
                (
                    first.name.as_str(),
                    first.kind,
                    first.container_name.as_deref()
                ),
                (name, kind, Some(owner))
            );
            if name == "Nested" {
                assert_eq!(
                    results.len(),
                    1,
                    "the AST of a shared inline type is indexed once"
                );
            }
        }
        let embedded = query(&mut engine, "EmbeddedFace");
        assert_eq!(embedded.len(), 1);
        assert_eq!(
            embedded[0].location.range,
            Range::new(Position::new(8, 30), Position::new(8, 42))
        );
        for (text, owners) in [
            (
                "widget.Handlestart",
                &["sample.Widget", "sample.WidgetCore", "sample.MemoryWidget"][..],
            ),
            ("sample.WgCo.HandleStart", &["sample.WidgetCore"][..]),
        ] {
            assert_eq!(
                query(&mut engine, text)
                    .iter()
                    .map(|symbol| symbol.container_name.as_deref().unwrap())
                    .collect::<Vec<_>>(),
                owners
            );
        }
        assert!(query(&mut engine, "other.Widget.HandleStart").is_empty());
        assert_eq!(names(query(&mut engine, "sample.init")), ["init", "init"]);
        assert!(query(&mut engine, "Excluded").is_empty());
        assert!(query(&mut engine, "_").is_empty());
    }

    #[test]
    fn refreshes_only_changed_views_and_reuses_current_disk_entries() {
        let project = TestProject::new();
        let a = project.write("a.go", "package sample\nvar DiskOriginal int\n");
        let b = project.write("b.go", "package sample\nvar Stable int\n");
        let a_key = path_key(&a);
        let b_key = path_key(&b);
        let mut engine = Engine::new(project.root.clone());
        install(&mut engine, project.workspace(&["a.go", "b.go"], true));
        assert_eq!(query(&mut engine, "DiskOriginal").len(), 1);
        let stable = Rc::clone(
            engine.workspace_symbol_index.files[&b_key]
                .symbols()
                .unwrap(),
        );
        let original = Rc::clone(
            engine.workspace_symbol_index.files[&a_key]
                .symbols()
                .unwrap(),
        );

        engine
            .open(a.clone(), "package buffer\nfunc Overlay() {}\n".into(), 1)
            .unwrap();
        assert_eq!(
            engine.workspace_symbol_index.pending,
            BTreeSet::from([a_key.clone()])
        );
        assert!(query(&mut engine, "DiskOriginal").is_empty());
        assert_eq!(query(&mut engine, "buffer.Overlay").len(), 1);
        replace(
            &mut engine,
            &a,
            "package buffer\n\nfunc ChangedOverlay() {}\nvar NewEntry int\n",
            2,
        );
        assert_eq!(
            engine.workspace_symbol_index.pending,
            BTreeSet::from([a_key.clone()])
        );
        let current = query(&mut engine, "buffer.ChangedOverlay");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].location.range.start, Position::new(2, 5));
        assert_eq!(query(&mut engine, "buffer.NewEntry").len(), 1);
        assert_eq!(
            engine.workspace_symbol_index.files[&a_key]
                .overlay
                .as_ref()
                .unwrap()
                .0,
            2
        );
        engine.close(&a);
        assert_eq!(query(&mut engine, "DiskOriginal").len(), 1);
        assert!(Rc::ptr_eq(
            &original,
            engine.workspace_symbol_index.files[&a_key]
                .symbols()
                .unwrap()
        ));

        engine
            .open(a.clone(), "package buffer\nvar OpenOnly int\n".into(), 3)
            .unwrap();
        assert_eq!(query(&mut engine, "OpenOnly").len(), 1);
        let open = Rc::clone(
            engine.workspace_symbol_index.files[&a_key]
                .symbols()
                .unwrap(),
        );
        project.write("a.go", "package disk\nvar ChangedOnDisk int\n");
        engine.file_changed(&a, FileChangeType::CHANGED);
        assert!(matches!(
            engine.workspace_symbols("OpenOnly"),
            QueryProgress::Pending(_)
        ));
        install(&mut engine, project.workspace(&["a.go", "b.go"], true));
        assert_eq!(query(&mut engine, "OpenOnly").len(), 1);
        assert!(query(&mut engine, "ChangedOnDisk").is_empty());
        assert!(Rc::ptr_eq(
            &open,
            engine.workspace_symbol_index.files[&a_key]
                .symbols()
                .unwrap()
        ));
        engine.close(&a);
        assert!(query(&mut engine, "OpenOnly").is_empty());
        assert_eq!(query(&mut engine, "disk.ChangedOnDisk").len(), 1);
        assert!(Rc::ptr_eq(
            &stable,
            engine.workspace_symbol_index.files[&b_key]
                .symbols()
                .unwrap()
        ));

        let c = project.write("c.go", "package sample\nvar Created int\n");
        fs::remove_file(&a).unwrap();
        engine.file_changed(&a, FileChangeType::DELETED);
        engine.file_changed(&c, FileChangeType::CREATED);
        install(&mut engine, project.workspace(&["b.go", "c.go"], true));
        assert_eq!(query(&mut engine, "Created").len(), 1);
        assert!(query(&mut engine, "ChangedOnDisk").is_empty());
        assert!(!engine.workspace_symbol_index.files.contains_key(&a_key));
        assert!(Rc::ptr_eq(
            &stable,
            engine.workspace_symbol_index.files[&b_key]
                .symbols()
                .unwrap()
        ));
    }

    #[test]
    fn uses_valid_open_drafts_with_an_empty_project_and_removes_them_on_close() {
        let project = TestProject::new();
        project.write("go.mod", "module example.test/empty\n\ngo 1.22\n");
        let draft = project.root.join("draft.go");
        let mut engine = Engine::new(project.root.clone());
        assert!(query(&mut engine, "　\n  ").is_empty());
        assert!(!engine.workspace_load().include_tests);
        let QueryProgress::Pending(load) = engine.workspace_symbols("Draft") else {
            panic!("workspace symbols require a test-inclusive package graph");
        };
        assert!(load.include_tests);
        let workspace = Workspace::load(engine.root(), load.include_tests).unwrap();
        assert!(workspace.project_symbol_files().is_empty());
        install(&mut engine, workspace);
        assert!(query(&mut engine, "Draft").is_empty());

        engine
            .open(
                draft.clone(),
                "package draft\nvar DraftName int\n".into(),
                1,
            )
            .unwrap();
        engine
            .open(
                project.root.join("sub/vendor/dep.go"),
                "package draft\nvar DraftVendor int\n".into(),
                1,
            )
            .unwrap();
        let named = query(&mut engine, "Draft");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].container_name.as_deref(), Some("draft"));
        replace(&mut engine, &draft, "var DraftWithoutPackage int\n", 2);
        assert!(query(&mut engine, "Draft").is_empty());
        replace(
            &mut engine,
            &draft,
            "package renamed\nvar DraftRenamed int\n",
            3,
        );
        assert_eq!(query(&mut engine, "renamed.DraftRenamed").len(), 1);
        assert!(query(&mut engine, "draft.DraftRenamed").is_empty());
        engine.close(&draft);
        assert!(query(&mut engine, "Draft").is_empty());

        engine
            .open(
                draft.clone(),
                "package renamed\nvar UnsavedDraft int\n".into(),
                4,
            )
            .unwrap();
        assert_eq!(query(&mut engine, "UnsavedDraft").len(), 1);
        project.write("draft.go", "package renamed\nvar SavedToDisk int\n");
        engine.close(&draft);
        let QueryProgress::Pending(load) = engine.workspace_symbols("SavedToDisk") else {
            panic!("a saved draft must refresh the package graph");
        };
        let workspace = Workspace::load(engine.root(), load.include_tests).unwrap();
        install(&mut engine, workspace);
        assert_eq!(query(&mut engine, "renamed.SavedToDisk").len(), 1);
        assert!(query(&mut engine, "UnsavedDraft").is_empty());
    }
}
