use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use lsp_types::{Range, TextDocumentContentChangeEvent};
use tree_sitter::Parser;

use super::source::{Import, Source, TextEditDelta, node_text, read_source};
use crate::symbols::{self, FileSymbols};
use crate::workspace::{Package, Workspace, path_key};

pub(super) struct FileIndex {
    pub(super) symbols: FileSymbols,
    pub(super) imports: Vec<Import>,
    pub(super) package_signature: u64,
}

impl FileIndex {
    fn package_signature(source: &Source) -> u64 {
        let root = source.tree.root_node();
        let mut cursor = root.walk();
        let mut signature = DefaultHasher::new();
        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "package_clause" | "import_declaration" => {
                    node_text(node, &source.text).hash(&mut signature)
                }
                "comment" => {
                    let comment = node_text(node, &source.text);
                    if comment.starts_with("//go:build") || comment.starts_with("// +build") {
                        comment.hash(&mut signature);
                    }
                }
                _ => break,
            }
        }
        signature.finish()
    }

    fn new(source: &Source) -> Self {
        Self {
            symbols: symbols::collect(&source.tree, &source.text, |name| source.range(name)),
            imports: source.imports.clone(),
            package_signature: Self::package_signature(source),
        }
    }
}

pub(super) struct IndexedSource {
    source: Source,
    declarations: OnceCell<FileIndex>,
    pub(super) document_version: Option<i32>,
}

impl IndexedSource {
    pub(super) fn new(source: Source, document_version: Option<i32>) -> Self {
        Self {
            source,
            declarations: OnceCell::new(),
            document_version,
        }
    }

    pub(super) fn declarations(&self) -> &FileIndex {
        self.declarations
            .get_or_init(|| FileIndex::new(&self.source))
    }

    pub(super) fn edit(
        &mut self,
        parser: &mut Parser,
        changes: &[TextDocumentContentChangeEvent],
        version: i32,
        observe: Option<&mut dyn FnMut(TextEditDelta<'_>) -> bool>,
    ) -> Result<()> {
        self.source.edit(parser, changes, observe)?;
        self.document_version = Some(version);
        self.declarations.take();
        Ok(())
    }
}

impl Deref for IndexedSource {
    type Target = Source;

    fn deref(&self) -> &Source {
        &self.source
    }
}

pub(super) struct DiskIndex {
    pub(super) declarations: FileIndex,
    pub(super) fingerprint: u64,
    reference_source: Option<Rc<IndexedSource>>,
}

impl DiskIndex {
    pub(super) fn new(source: &Source) -> Self {
        Self {
            declarations: FileIndex::new(source),
            fingerprint: text_fingerprint(&source.text),
            reference_source: None,
        }
    }

    pub(super) fn still_current(&self, path: &Path) -> bool {
        fs::read_to_string(path).is_ok_and(|text| self.fingerprint == text_fingerprint(&text))
    }
}

pub(super) fn text_fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub(super) type DraftFiles<'a> = HashMap<(&'a Path, &'a str), Vec<&'a PathBuf>>;

pub(super) fn active_package<'w>(
    workspace: &'w Workspace,
    source: &Source,
    new_documents: &HashSet<PathBuf>,
) -> Option<&'w Package> {
    workspace.package_of_file(&source.path).or_else(|| {
        new_documents
            .contains(&path_key(&source.path))
            .then(|| draft_package(workspace, source))
            .flatten()
    })
}

pub(super) fn draft_files<'a>(
    workspace: &'a Workspace,
    documents: &'a HashMap<PathBuf, IndexedSource>,
    new_documents: &HashSet<PathBuf>,
) -> DraftFiles<'a> {
    let mut drafts = DraftFiles::new();
    for path in new_documents {
        if let Some(document) = documents.get(path)
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
    drafts
}

pub(super) fn draft_package<'w>(workspace: &'w Workspace, source: &Source) -> Option<&'w Package> {
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

pub(super) struct IndexReader<'a> {
    pub(super) parser: &'a mut Parser,
    pub(super) documents: &'a HashMap<PathBuf, IndexedSource>,
    pub(super) disk_index: &'a mut HashMap<PathBuf, DiskIndex>,
    pub(super) drafts: DraftFiles<'a>,
}

pub(super) enum ReferenceSource<'a> {
    Open(&'a IndexedSource),
    Disk(Rc<IndexedSource>),
}

impl AsRef<IndexedSource> for ReferenceSource<'_> {
    fn as_ref(&self) -> &IndexedSource {
        match self {
            Self::Open(source) => source,
            Self::Disk(source) => source,
        }
    }
}

impl<'a> IndexReader<'a> {
    pub(super) fn reference_source(&mut self, path: &Path) -> Result<Option<ReferenceSource<'a>>> {
        let key = path_key(path);
        if let Some(source) = self.documents.get(&key) {
            return Ok(Some(ReferenceSource::Open(source)));
        }
        if let Some(source) = self
            .disk_index
            .get(&key)
            .and_then(|entry| entry.reference_source.as_ref())
        {
            return Ok(Some(ReferenceSource::Disk(Rc::clone(source))));
        }
        let source = match read_source(self.parser, path) {
            Ok(source) => Rc::new(IndexedSource::new(source, None)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let fingerprint = text_fingerprint(&source.text);
        let entry = match self.disk_index.entry(key) {
            Entry::Occupied(mut entry) => {
                if entry.get().fingerprint != fingerprint {
                    entry.insert(DiskIndex::new(&source));
                }
                entry.into_mut()
            }
            Entry::Vacant(entry) => entry.insert(DiskIndex::new(&source)),
        };
        entry.reference_source = Some(Rc::clone(&source));
        Ok(Some(ReferenceSource::Disk(source)))
    }

    pub(super) fn files<'p>(&self, package: &'p Package) -> Cow<'p, [PathBuf]> {
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

    pub(super) fn file_index(&mut self, file: &Path) -> Result<&FileIndex> {
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

    pub(super) fn find(
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

    pub(super) fn find_dot_import(
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

impl IndexReader<'_> {
    pub(super) fn candidate_index(&mut self, file: &Path) -> Result<Option<&FileIndex>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::Engine;

    #[test]
    fn indexes_public_and_private_symbols_in_grouped_declarations() {
        let mut engine = Engine::new(PathBuf::new());
        let source = Source::parse(
            &mut engine.state.parser,
            PathBuf::new(),
            "package test\nvar (First, Second = 1, 2)\nconst (Third, fourth = 3, 4)\ntype (Fifth int)\nfunc Sixth() {}\n".into(),
            None,
        )
        .unwrap();
        let source = IndexedSource::new(source, None);
        let names = &source.declarations().symbols;
        for name in ["First", "Second", "Third", "Fifth", "Sixth"] {
            assert!(names.declaration(name).is_some(), "missing {name}");
        }
        assert!(names.declaration("fourth").is_some());
    }
}
