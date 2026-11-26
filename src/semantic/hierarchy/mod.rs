use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use lsp_types::{Position, Range, Uri};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tree_sitter::Node;

use super::Engine;
use super::query::{QueryProgress, SemanticTask};
use super::resolver::Resolver;
use super::snapshot::{DocumentSnapshot, EngineSnapshot};
use super::source::{Source, is_declaration_name, is_test_source};
use crate::protocol::file_path;
use crate::workspace::{Package, WorkspaceLoad, path_key};

pub(crate) mod call;
pub(crate) mod type_hierarchy;

#[cfg(test)]
mod tests;

type Location = (PathBuf, Range);
const BATCH_TIME: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Hierarchy {
    Call,
    Type,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemData {
    path: PathBuf,
    selection_start: Position,
    kind: Hierarchy,
}

impl Hierarchy {
    fn data(self, uri: &Uri, selection: Range) -> Result<Value> {
        Ok(serde_json::to_value(ItemData {
            path: file_path(uri)?,
            selection_start: selection.start,
            kind: self,
        })?)
    }

    fn anchor(
        self,
        uri: &Uri,
        selection: Range,
        data: Option<&Value>,
    ) -> Result<Option<(PathBuf, Position)>> {
        let path = file_path(uri)?;
        let Some(data) = data else {
            return Ok(Some((path, selection.start)));
        };
        let Ok(data) = ItemData::deserialize(data) else {
            return Ok(None);
        };
        Ok(
            (data.kind == self && path_key(&path) == path_key(&data.path))
                .then_some((data.path, data.selection_start)),
        )
    }
}

fn document<'d>(
    snapshot: &mut EngineSnapshot<'d>,
    path: &'d Path,
) -> Result<Option<DocumentSnapshot<'d>>> {
    match snapshot.document(path) {
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        result => result,
    }
}

fn declaration_at(source: &Source, position: Position) -> Option<Node<'_>> {
    let name = source.reference_at(position)?.name();
    name.parent().filter(|_| is_declaration_name(name))
}

fn walk<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> Result<()>) -> Result<()> {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node())?;
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}

fn in_package<'w, T>(
    snapshot: &mut EngineSnapshot<'w>,
    document: &DocumentSnapshot<'_>,
    query: impl FnOnce(&mut Resolver<'w, '_>, &'w Package) -> Result<Option<T>>,
) -> Result<QueryProgress<Option<T>>> {
    match snapshot.package_for(document, is_test_source(document.path())) {
        QueryProgress::Ready(Some(package)) => {
            let workspace = snapshot
                .workspace()
                .expect("a package belongs to a workspace");
            query(&mut snapshot.resolver(workspace), package).map(QueryProgress::Ready)
        }
        QueryProgress::Ready(None) => Ok(QueryProgress::Ready(None)),
        QueryProgress::Pending(load) => Ok(QueryProgress::Pending(load)),
        QueryProgress::Searching(task) => match task {},
    }
}

struct SearchResults<T> {
    search: Option<T>,
    locations: Vec<Location>,
}

impl<T: SemanticTask<Vec<Location>>> SearchResults<T> {
    fn new(progress: QueryProgress<Vec<Location>, T>) -> std::result::Result<Self, WorkspaceLoad> {
        let (search, locations) = match progress {
            QueryProgress::Ready(locations) => (None, locations),
            QueryProgress::Searching(task) => (Some(task), Vec::new()),
            QueryProgress::Pending(load) => return Err(load),
        };
        Ok(Self { search, locations })
    }

    fn advance(&mut self, engine: &mut Engine) -> Result<()> {
        if let Some(search) = &mut self.search
            && search.advance(engine)?
        {
            self.locations = self.search.take().expect("completed search").finish();
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.search.is_none() && self.locations.is_empty()
    }
}
