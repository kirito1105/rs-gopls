mod candidates;
mod cursor;
mod expected;
mod imports;
mod ranking;
mod render;
mod sdk_names;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, InitializeParams, MarkupKind, Position,
};

use crate::protocol::file_path;
use crate::workspace::{Package, Workspace, path_key};

use super::{
    Engine, QueryProgress, SemanticTask,
    index::text_fingerprint,
    snapshot::{DocumentSnapshot, EngineSnapshot},
    source::is_test_source,
};
use cursor::{Edit, Probe, Site};
pub(super) use imports::ImportCache;
use imports::{Search, SearchKind};
use render::{ItemData, build_import_edits, markup, nonoverlapping, package_name, response};

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
    pub(crate) fn complete(
        &mut self,
        path: &Path,
        position: Position,
        _context: Option<CompletionContext>,
        caps: &CompletionClientCapabilities,
    ) -> Result<QueryProgress<CompletionResponse, CompletionTask>> {
        let generation = self.state.completion_graph_generation;
        let progress = self
            .snapshot()
            .complete(path, position, caps, Some(generation))?;
        if let QueryProgress::Pending(load) = &progress {
            self.include_tests |= load.include_tests;
        }
        Ok(progress)
    }

    pub(crate) fn complete_after_load_failure(
        &mut self,
        path: &Path,
        position: Position,
        caps: &CompletionClientCapabilities,
    ) -> Result<CompletionResponse> {
        match self.snapshot().complete(path, position, caps, None)? {
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
    ) -> Result<QueryProgress<CompletionResponse, CompletionTask>> {
        let Some(document) = completion_document(self, path)? else {
            return Ok(empty());
        };
        let Some(probe) = Probe::new(&document, self.parser(), position)? else {
            return Ok(empty());
        };
        let source = probe.source(&document);
        let anchor = probe.anchor(source);
        let site = probe.site(source, document.text());
        let edit = &probe.edit;
        if let Some(candidates) = candidates::syntax_candidates(
            &site,
            source,
            anchor,
            edit.prefix(document.text()),
            self.docs(),
        ) {
            return Ok(QueryProgress::Ready(response(
                self, &document, edit, candidates, caps, None,
            )?));
        }

        let Some(generation) = generation else {
            let candidates =
                candidates::collect(&document, source, anchor, site, edit, None, caps)?.candidates;
            return Ok(QueryProgress::Ready(response(
                self, &document, edit, candidates, caps, None,
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
            let mut resolver = self.resolver(workspace);
            let graph = package.map(|package| (&mut resolver, package));
            let collection =
                candidates::collect(&document, source, anchor, site, edit, graph, caps)?;
            (
                collection.candidates,
                collection.search.map(SearchKind::Symbols),
            )
        };
        if let Some(search) = search {
            return Ok(QueryProgress::Searching(CompletionTask {
                path: path.to_owned(),
                generation,
                version: document.version(),
                prefix: edit.prefix(document.text()).to_owned(),
                edit: edit.clone(),
                caps: *caps,
                search: Search::new(search, package),
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
            } else if let Some(workspace) = self.workspace()
                && let Ok(Some(source)) = self
                    .resolver(workspace)
                    .indexes
                    .reference_source(&target.file)
            {
                item.documentation = markup(source.as_ref(), target.range, caps);
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
) -> Result<Option<DocumentSnapshot<'d>>>
where
    'w: 'd,
{
    match snapshot.document(path) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
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
    version: Option<i32>,
    prefix: String,
    edit: Edit,
    caps: CompletionClientCapabilities,
    search: Search,
    candidates: Vec<ranking::Candidate>,
    result: Option<CompletionResponse>,
}

impl SemanticTask<CompletionResponse> for CompletionTask {
    fn advance(&mut self, engine: &mut Engine) -> Result<bool> {
        if engine.state.completion_graph_generation != self.generation {
            return Err(StaleCompletion.into());
        }
        let mut snapshot = engine.state.snapshot(engine.include_tests);
        let document = completion_document(&mut snapshot, &self.path)?
            .filter(|document| document.version() == self.version)
            .ok_or(StaleCompletion)?;
        if !self.search.advance(
            &mut engine.completion_imports,
            &mut snapshot,
            &document,
            self.generation,
            &self.prefix,
            &mut self.candidates,
            &self.caps,
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
        )?);
        Ok(true)
    }

    fn finish(self) -> CompletionResponse {
        self.result
            .expect("completion task must finish its last batch")
    }
}
