use std::collections::{BTreeMap, HashMap, HashSet, VecDeque, hash_map::Entry};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use lsp_types::{FileChangeType, Range, TextDocumentContentChangeEvent};
use tree_sitter::{Node, Parser};

use crate::symbols::{
    Container, FunctionSignature, Symbol, SymbolKind, TypeDetails, TypeRef, TypeRefKind,
};
use crate::workspace::{Package, Workspace, WorkspaceLoad, path_key};

pub(crate) mod actions;
mod definition;
pub(crate) mod diagnostics;
mod document_symbols;
pub(crate) mod hierarchy;
mod hover;
mod implementation;
mod index;
mod presentation;
mod query;
mod references;
mod resolver;
mod signature_help;
pub(crate) mod snapshot;
mod source;
mod workspace_symbols;

pub(crate) use definition::DefinitionKind;
use index::{DiskIndex, IndexReader, IndexedSource, text_fingerprint};
pub(crate) use query::{QueryProgress, SemanticTask};
pub(crate) use resolver::TypeIdentity;
// Preserve the shared imports used by the existing presentation and feature submodules.
use index::IndexedSource as Source;
use resolver::{
    MAX_TYPE_DEPTH, ResolvedSymbol, Resolver, Substitutions, TypeKey, TypeKind, is_exported,
    is_predeclared, is_predeclared_type, materialize_type, untyped_default,
};
use source::{ReferenceQuery, Source as SyntaxSource, node_text, read_source};

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

    fn affects_source(
        &self,
        workspace: &Workspace,
        from: Option<&Package>,
        source: &SyntaxSource,
    ) -> bool {
        !matches!(self, Self::Clean)
            && from.is_none_or(|package| {
                self.affects(package)
                    || source.imports.iter().any(|import| {
                        workspace
                            .resolve_import(package, &import.path)
                            .is_none_or(|dependency| self.affects(dependency))
                    })
            })
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
    state: EngineState,
    implementation_cache: implementation::ImplementationCache,
    implementation_sources: implementation::InterfaceSources,
    workspace_symbol_index: workspace_symbols::WorkspaceSymbolIndex,
    include_tests: bool,
}

struct EngineState {
    root: PathBuf,
    workspace: Option<Workspace>,
    graph_changes: GraphChanges,
    generation: u64,
    parser: Parser,
    documents: HashMap<PathBuf, IndexedSource>,
    new_documents: HashSet<PathBuf>,
    disk_index: HashMap<PathBuf, DiskIndex>,
}

impl Engine {
    pub fn new(root: PathBuf) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Go grammar must match the tree-sitter runtime");
        Self {
            state: EngineState {
                root,
                workspace: None,
                graph_changes: GraphChanges::default(),
                generation: 0,
                parser,
                documents: HashMap::new(),
                new_documents: HashSet::new(),
                disk_index: HashMap::new(),
            },
            implementation_cache: implementation::ImplementationCache::default(),
            implementation_sources: implementation::InterfaceSources::default(),
            workspace_symbol_index: workspace_symbols::WorkspaceSymbolIndex::default(),
            include_tests: false,
        }
    }

    pub fn open(&mut self, path: PathBuf, text: String, version: i32) -> Result<()> {
        if path.extension().is_some_and(|extension| extension == "go") {
            let key = path_key(&path);
            let new_file = !path.is_file();
            if !self.implementation_cache.is_empty() {
                let unchanged = match self.state.documents.get(&key) {
                    Some(previous) => previous.text == text,
                    None => {
                        !new_file
                            && self
                                .state
                                .disk_index
                                .get(&key)
                                .is_some_and(|disk| disk.fingerprint == text_fingerprint(&text))
                    }
                };
                if !unchanged {
                    self.implementation_cache.clear();
                }
            }
            let source = SyntaxSource::parse(&mut self.state.parser, path, text, None)?;
            if new_file {
                self.state.new_documents.insert(key.clone());
            } else {
                self.state.new_documents.remove(&key);
            }
            self.workspace_symbol_index.document_opened(
                self.state.workspace.as_ref(),
                &self.state.root,
                &key,
                &source.path,
                new_file,
            );
            self.state
                .documents
                .insert(key, IndexedSource::new(source, Some(version)));
        }
        Ok(())
    }

    pub fn edit(
        &mut self,
        path: &Path,
        edits: &[TextDocumentContentChangeEvent],
        version: i32,
    ) -> Result<()> {
        let key = path_key(path);
        if let Some(source) = self.state.documents.get_mut(&key)
            && !edits.is_empty()
        {
            let mut edit = implementation::ObservedEdit::new();
            let mut observe = |delta: source::TextEditDelta<'_>| edit.observe(delta);
            source.edit(
                &mut self.state.parser,
                edits,
                version,
                (!self.implementation_cache.is_empty()).then_some(&mut observe as _),
            )?;
            self.workspace_symbol_index.document_edited(&key);
            edit.apply(&mut self.implementation_cache, &key);
        }
        Ok(())
    }

    pub fn close(&mut self, path: &Path) {
        self.implementation_cache.clear();
        let key = path_key(path);
        self.implementation_sources.invalidate(&key);
        if self.state.documents.remove(&key).is_some() {
            self.workspace_symbol_index.document_closed(&key);
        }
        if self.state.new_documents.remove(&key) && path.is_file() {
            // A new buffer can be saved by a client that does not send a
            // watched-file notification. Refresh the build's file selection.
            self.file_changed(path, FileChangeType::CREATED);
        }
        if self
            .state
            .disk_index
            .get(&key)
            .is_some_and(|index| !index.still_current(path))
        {
            self.state.disk_index.remove(&key);
        }
    }

    pub fn root(&self) -> &Path {
        &self.state.root
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.state.documents.contains_key(&path_key(path))
    }

    pub fn workspace_load(&self) -> WorkspaceLoad {
        WorkspaceLoad {
            generation: self.state.generation,
            include_tests: self.include_tests,
        }
    }

    pub fn install_workspace(&mut self, load: WorkspaceLoad, workspace: Workspace) -> bool {
        if load.generation != self.state.generation {
            return false;
        }
        self.implementation_sources.retain_workspace(&workspace);
        self.state.workspace = Some(workspace);
        self.state.graph_changes = GraphChanges::Clean;
        self.implementation_cache.clear();
        self.workspace_symbol_index.graph_changed();
        true
    }

    fn invalidate(&mut self, affected: Option<HashSet<String>>) {
        self.implementation_cache.clear();
        self.state.generation += 1;
        self.state.graph_changes.include(affected);
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

        self.implementation_cache.clear();
        let key = path_key(path);
        self.workspace_symbol_index
            .disk_changed(&key, self.state.documents.contains_key(&key));
        self.implementation_sources.invalidate(&key);
        if change == FileChangeType::CREATED && path.is_file() {
            self.state.new_documents.remove(&key);
        }
        let previous = self.state.disk_index.remove(&key);
        if change == FileChangeType::CHANGED
            && let Some(previous) = previous
            && let Ok(disk) = read_source(&mut self.state.parser, path)
        {
            let updated = DiskIndex::new(&disk);
            if previous.declarations.package_signature == updated.declarations.package_signature {
                self.state.disk_index.insert(key, updated);
                return;
            }
        }
        // A new, removed, or structurally changed source can change the set
        // of Go files selected by the current build or add an import.
        let affected = self
            .state
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.affected_packages(path));
        self.invalidate(affected);
    }
}
