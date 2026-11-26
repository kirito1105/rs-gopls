mod candidates;
mod cursor;
mod expected;
mod imports;
pub(super) mod prefilter;
mod profile;
mod ranking;
pub(super) mod render;
mod sdk;
pub(super) mod sdk_names;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_types::{CompletionItem, CompletionResponse, InitializeParams, MarkupKind, Position};

use crate::protocol::file_path;
use crate::workspace::{Package, Workspace, path_key};

use super::{
    Engine, QueryProgress, SemanticTask,
    index::{CompletionRead, text_fingerprint},
    snapshot::{DocumentSnapshot, EngineSnapshot},
    source::is_test_source,
};
use cursor::{Edit, Probe, Site};
pub(super) use expected::InferredMemo;
pub(super) use imports::ImportCache;
use imports::{Search, SearchKind};
use prefilter::Stamp;
pub(crate) use profile::Trace;
use render::{
    ItemData, build_import_edits, markup, nonoverlapping, package_name, response, target_markup,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompletionClientCapabilities {
    pub(crate) snippet_support: bool,
    pub(crate) label_details_support: bool,
    pub(crate) insert_replace_support: bool,
    pub(crate) deprecated_support: bool,
    pub(crate) markdown_docs: bool,
    pub(crate) resolve_documentation: bool,
    pub(crate) resolve_detail: bool,
    pub(crate) resolve_additional_text_edits: bool,
}

impl CompletionClientCapabilities {
    pub(crate) fn from_initialize(params: &InitializeParams) -> Self {
        let Some(item) = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|document| document.completion.as_ref())
            .and_then(|completion| completion.completion_item.as_ref())
        else {
            return Self::default();
        };
        let can_resolve = |property| {
            item.resolve_support
                .as_ref()
                .is_some_and(|support| support.properties.iter().any(|name| name == property))
        };
        Self {
            snippet_support: item.snippet_support == Some(true),
            label_details_support: item.label_details_support == Some(true),
            insert_replace_support: item.insert_replace_support == Some(true),
            deprecated_support: item.deprecated_support == Some(true)
                || item.tag_support.as_ref().is_some_and(|tags| {
                    tags.value_set
                        .contains(&lsp_types::CompletionItemTag::DEPRECATED)
                }),
            markdown_docs: item
                .documentation_format
                .as_ref()
                .is_some_and(|formats| formats.first() == Some(&MarkupKind::Markdown)),
            resolve_documentation: can_resolve("documentation"),
            resolve_detail: can_resolve("detail"),
            resolve_additional_text_edits: can_resolve("additionalTextEdits"),
        }
    }
}

pub(crate) enum ResolveCompletionResult {
    Item(CompletionItem),
    ContentModified,
    InvalidData,
}

#[derive(Debug)]
pub(crate) struct StaleCompletion;

impl std::fmt::Display for StaleCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the completion document or package graph changed")
    }
}

impl std::error::Error for StaleCompletion {}

impl Engine {
    #[cfg(test)]
    pub(crate) fn complete(
        &mut self,
        path: &Path,
        position: Position,
        _context: Option<lsp_types::CompletionContext>,
        caps: &CompletionClientCapabilities,
    ) -> Result<QueryProgress<CompletionResponse, CompletionTask>> {
        self.complete_traced(path, position, caps, None)
    }

    pub(crate) fn complete_traced(
        &mut self,
        path: &Path,
        position: Position,
        caps: &CompletionClientCapabilities,
        trace: Option<&Trace>,
    ) -> Result<QueryProgress<CompletionResponse, CompletionTask>> {
        let generation = self.state.completion_graph_generation;
        let progress = self
            .snapshot()
            .complete(path, position, caps, Some(generation), trace)?;
        if let QueryProgress::Pending(load) = &progress {
            self.include_tests |= load.include_tests;
        }
        if matches!(progress, QueryProgress::Ready(_))
            && let Some(trace) = trace
        {
            trace.outcome("completed");
        }
        Ok(progress)
    }

    pub(crate) fn complete_after_load_failure(
        &mut self,
        path: &Path,
        position: Position,
        caps: &CompletionClientCapabilities,
        trace: Option<&Trace>,
    ) -> Result<CompletionResponse> {
        match self
            .snapshot()
            .complete(path, position, caps, None, trace)?
        {
            QueryProgress::Ready(response) => Ok(response),
            _ => unreachable!("completion fallback uses local syntax"),
        }
    }

    pub(crate) fn resolve_completion_item(
        &mut self,
        item: CompletionItem,
        caps: &CompletionClientCapabilities,
    ) -> Result<ResolveCompletionResult> {
        let generation = self.state.completion_graph_generation;
        self.snapshot()
            .resolve_completion_item(item, caps, generation)
    }
}

impl EngineSnapshot<'_> {
    fn complete(
        &mut self,
        path: &Path,
        position: Position,
        caps: &CompletionClientCapabilities,
        generation: Option<u64>,
        trace: Option<&Trace>,
    ) -> Result<QueryProgress<CompletionResponse, CompletionTask>> {
        let mut request = CompletionRead::new(trace.cloned());
        let phase = trace.map(|trace| trace.phase("cursor"));
        let Some(document) = completion_document(self, path, trace)? else {
            return Ok(empty());
        };
        let Some(probe) = Probe::new(&document, self.parser(), position, trace)? else {
            return Ok(empty());
        };
        let source = probe.source(&document);
        let anchor = probe.anchor(source);
        let site = probe.site(source, document.text());
        let edit = &probe.edit;
        drop(phase);
        if let Some(candidates) = candidates::syntax_candidates(
            &site,
            source,
            anchor,
            edit.prefix(document.text()),
            self.docs(),
        ) {
            return Ok(QueryProgress::Ready(response(
                self,
                &document,
                edit,
                candidates,
                caps,
                None,
                &mut request,
            )?));
        }

        let Some(generation) = generation else {
            let candidates =
                candidates::collect(&document, source, anchor, site, edit, None, caps)?.candidates;
            return Ok(QueryProgress::Ready(response(
                self,
                &document,
                edit,
                candidates,
                caps,
                None,
                &mut request,
            )?));
        };
        let test = is_test_source(path);
        let workspace = match self.active_graph(test) {
            QueryProgress::Ready(workspace) => workspace,
            QueryProgress::Pending(load) => return Ok(QueryProgress::Pending(load)),
            QueryProgress::Searching(task) => match task {},
        };
        let package = match self.package_for(&document, test) {
            QueryProgress::Ready(package) => package,
            QueryProgress::Pending(load) => return Ok(QueryProgress::Pending(load)),
            QueryProgress::Searching(task) => match task {},
        };
        let package = completion_package(workspace, &document, package);
        let (items, search) = if let Site::ImportPath { close } = site {
            (Vec::new(), Some(SearchKind::Path { close }))
        } else {
            let _phase = trace.map(|trace| trace.phase("local_candidates"));
            let mut resolver = self.resolver_with_completion(workspace, Some(&mut request));
            let graph = package.map(|package| (&mut resolver, package));
            let collection =
                candidates::collect(&document, source, anchor, site, edit, graph, caps)?;
            (
                collection.candidates,
                collection.search.map(SearchKind::Symbols),
            )
        };
        if let Some(search) = search {
            let version = document.version();
            let prefix = edit.prefix(document.text()).to_owned();
            let edit = edit.clone();
            if let Some(source) = document.into_disk()
                && let Ok(metadata) = std::fs::metadata(path)
            {
                request.pin_document(source, metadata.into());
            }
            return Ok(QueryProgress::Searching(CompletionTask {
                path: path.to_owned(),
                generation,
                source_generation: self.completion_source_generation(),
                version,
                prefix,
                edit,
                caps: *caps,
                search: Search::new(search, package),
                request,
                candidates: items,
                result: None,
            }));
        }
        Ok(QueryProgress::Ready(response(
            self,
            &document,
            edit,
            items,
            caps,
            Some(generation),
            &mut request,
        )?))
    }

    fn resolve_completion_item(
        &mut self,
        mut item: CompletionItem,
        caps: &CompletionClientCapabilities,
        generation: u64,
    ) -> Result<ResolveCompletionResult> {
        let Some(data) = item
            .data
            .as_ref()
            .and_then(|data| serde_json::from_value::<ItemData>(data.clone()).ok())
        else {
            return Ok(ResolveCompletionResult::InvalidData);
        };
        let Ok(path) = file_path(&data.uri) else {
            return Ok(ResolveCompletionResult::InvalidData);
        };
        let Some(source) = self.docs().get(&path_key(&path)) else {
            return Ok(ResolveCompletionResult::ContentModified);
        };
        let document = DocumentSnapshot::borrowed(&path, source);
        if data.document_version.is_none()
            || document.version() != data.document_version
            || format!("{:016x}", text_fingerprint(document.text())) != data.document_fingerprint
            || data
                .graph_generation
                .is_some_and(|graph| graph != generation)
        {
            return Ok(ResolveCompletionResult::ContentModified);
        }
        if item.detail.is_none() {
            item.detail = data.detail;
        }
        if item.documentation.is_none()
            && let Some(target) = data.target
        {
            if path_key(&target.file) == path_key(&path) {
                item.documentation = markup(source, target.range, caps);
            } else if let Some(workspace) = self.workspace() {
                let mut request = CompletionRead::new(None);
                let mut resolver = self.resolver_with_completion(workspace, Some(&mut request));
                item.documentation = target_markup(&mut resolver.indexes, &target, caps);
            }
        }
        if caps.resolve_additional_text_edits
            && let Some(import) = data.pending_import
        {
            let Some(edits) = build_import_edits(&document, &import) else {
                return Ok(ResolveCompletionResult::ContentModified);
            };
            if item
                .text_edit
                .as_ref()
                .is_none_or(|primary| !nonoverlapping(primary, &edits))
            {
                return Ok(ResolveCompletionResult::InvalidData);
            }
            if !edits.is_empty() {
                item.additional_text_edits = Some(edits);
            }
        }
        Ok(ResolveCompletionResult::Item(item))
    }
}

fn completion_document<'w, 'd>(
    snapshot: &mut EngineSnapshot<'w>,
    path: &'d Path,
    trace: Option<&Trace>,
) -> Result<Option<DocumentSnapshot<'d>>>
where
    'w: 'd,
{
    if let Some(trace) = trace
        && path.extension().is_some_and(|extension| extension == "go")
    {
        let key = path_key(path);
        if !snapshot.docs().contains_key(&key) {
            trace.read(&key, false, false, false);
        }
    }
    match snapshot.document(path) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Ok(Some(document)) => {
            if document.version().is_none()
                && let Some(trace) = trace
            {
                trace.parse();
            }
            Ok(Some(document))
        }
        document => document,
    }
}

fn completion_package<'w>(
    workspace: &'w Workspace,
    document: &DocumentSnapshot<'_>,
    active: Option<&'w Package>,
) -> Option<&'w Package> {
    let name = package_name(document.indexed())?;
    if let Some(active) = active.filter(|package| package.name == name) {
        return Some(active);
    }
    let directory = path_key(document.path().parent()?);
    let test = is_test_source(document.path());
    workspace
        .navigation_packages()
        .filter(|package| {
            package.directory_and_name() == (directory.as_path(), name)
                && (test || package.is_production())
        })
        .min_by_key(|package| test == package.is_production())
}

fn empty() -> QueryProgress<CompletionResponse, CompletionTask> {
    QueryProgress::Ready(CompletionResponse::Array(Vec::new()))
}

pub(crate) struct CompletionTask {
    path: PathBuf,
    generation: u64,
    source_generation: u64,
    version: Option<i32>,
    prefix: String,
    edit: Edit,
    caps: CompletionClientCapabilities,
    search: Search,
    request: CompletionRead,
    candidates: Vec<ranking::Candidate>,
    result: Option<CompletionResponse>,
}

impl SemanticTask<CompletionResponse> for CompletionTask {
    fn ready(&self, engine: &Engine) -> bool {
        engine.state.completion_graph_generation != self.generation
            || engine.state.completion_source_generation != self.source_generation
            || self.search.is_ready(&engine.completion_imports)
    }

    fn discard(&mut self, code: lsp_server::ErrorCode) {
        if let Some(trace) = &self.request.trace {
            trace.error(code);
        }
    }

    fn advance(&mut self, engine: &mut Engine) -> Result<bool> {
        if !self.ready(engine) {
            // Synchronous callers also drive the same asynchronous batches.
            std::thread::yield_now();
            return Ok(false);
        }
        if engine.state.completion_graph_generation != self.generation
            || engine.state.completion_source_generation != self.source_generation
        {
            self.discard(lsp_server::ErrorCode::ContentModified);
            return Err(StaleCompletion.into());
        }
        let mut snapshot = engine.state.snapshot(engine.include_tests);
        let retained;
        let document = if let Some((source, stamp)) = &self.request.document {
            if !std::fs::metadata(&self.path).is_ok_and(|metadata| Stamp::from(metadata) == *stamp)
            {
                return Err(StaleCompletion.into());
            }
            retained = std::rc::Rc::clone(source);
            DocumentSnapshot::borrowed(&self.path, &retained)
        } else {
            completion_document(&mut snapshot, &self.path, self.request.trace.as_ref())?
                .filter(|document| document.version() == self.version)
                .ok_or(StaleCompletion)?
        };
        if !self.search.advance(
            &mut engine.completion_imports,
            &mut snapshot,
            &document,
            self.generation,
            &self.prefix,
            &mut self.candidates,
            &self.caps,
            &mut self.request,
        )? {
            return Ok(false);
        }
        self.result = Some(response(
            &mut snapshot,
            &document,
            &self.edit,
            std::mem::take(&mut self.candidates),
            &self.caps,
            Some(self.generation),
            &mut self.request,
        )?);
        Ok(true)
    }

    fn finish(self) -> CompletionResponse {
        if let Some(trace) = &self.request.trace {
            trace.outcome("completed");
        }
        self.result
            .expect("completion task must finish its last batch")
    }
}
