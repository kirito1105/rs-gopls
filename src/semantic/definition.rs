use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_types::{Position, Range};

use super::Engine;
use super::query::QueryProgress;
use super::snapshot::{DocumentSnapshot, EngineSnapshot};
use super::source::{ReferenceQuery, is_declaration_name, is_test_source};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefinitionKind {
    Declaration,
    Type,
}

type DefinitionProgress = QueryProgress<Option<(PathBuf, Range)>>;

impl Engine {
    pub(crate) fn definition(
        &mut self,
        path: &Path,
        position: Position,
        kind: DefinitionKind,
    ) -> Result<DefinitionProgress> {
        let mut snapshot = self.state.snapshot(self.include_tests);
        let Some(document) = snapshot.document(path)? else {
            return Ok(QueryProgress::Ready(None));
        };
        let progress = match kind {
            DefinitionKind::Declaration => snapshot.resolve_name(&document, position),
            DefinitionKind::Type => snapshot.resolve_type_definition(&document, position),
        };
        self.include_tests = snapshot.load_for(false).include_tests;
        progress
    }
}

fn local_declaration(
    document: &DocumentSnapshot<'_>,
    reference: ReferenceQuery<'_>,
) -> Option<(PathBuf, Range)> {
    let ReferenceQuery::Name(name) = reference else {
        return None;
    };
    let declaration = if is_declaration_name(name) {
        name
    } else {
        document.lexical_binding(name)?.name
    };
    Some((
        document.path().to_owned(),
        document.byte_range(declaration.byte_range()),
    ))
}

impl EngineSnapshot<'_> {
    pub(crate) fn resolve_name(
        &mut self,
        document: &DocumentSnapshot<'_>,
        position: Position,
    ) -> Result<DefinitionProgress> {
        self.resolve_definition(document, position, DefinitionKind::Declaration)
    }

    pub(crate) fn resolve_type_definition(
        &mut self,
        document: &DocumentSnapshot<'_>,
        position: Position,
    ) -> Result<DefinitionProgress> {
        self.resolve_definition(document, position, DefinitionKind::Type)
    }

    fn resolve_definition(
        &mut self,
        document: &DocumentSnapshot<'_>,
        position: Position,
        kind: DefinitionKind,
    ) -> Result<DefinitionProgress> {
        let Some(reference) = document.indexed().reference_at(position) else {
            return Ok(QueryProgress::Ready(None));
        };
        if kind == DefinitionKind::Declaration
            && let Some(location) = local_declaration(document, reference)
        {
            return Ok(QueryProgress::Ready(Some(location)));
        }
        let require_tests = is_test_source(document.path());
        self.note_test_requirement(require_tests);
        let package = match self.package_for(document, require_tests) {
            QueryProgress::Ready(Some(package)) => package,
            QueryProgress::Ready(None) => return Ok(QueryProgress::Ready(None)),
            QueryProgress::Pending(load) => return Ok(QueryProgress::Pending(load)),
            QueryProgress::Searching(task) => match task {},
        };
        let workspace = self.workspace().expect("a package belongs to a workspace");
        let root = self.root();
        let mut resolver = self.resolver(workspace);
        let source = document.indexed();
        let location = match kind {
            DefinitionKind::Declaration => {
                resolver.declaration_location(package, source, reference, root)
            }
            DefinitionKind::Type => resolver.type_definition(package, source, reference, root),
        }?;
        Ok(QueryProgress::Ready(location))
    }
}
