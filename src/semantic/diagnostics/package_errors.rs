use lsp_types::Position;

use super::syntax::visible_range;
use super::{
    DiagnosticCode, DiagnosticItem, DiagnosticSeverity, DocumentSnapshot, EngineSnapshot,
    QueryProgress, is_test_source,
};
use crate::workspace::path_key;

pub(super) fn collect_package_errors(
    snapshot: &EngineSnapshot<'_>,
    document: &DocumentSnapshot<'_>,
    syntax: &[DiagnosticItem],
) -> Vec<DiagnosticItem> {
    let tests = is_test_source(document.path());
    let QueryProgress::Ready(graph) = snapshot.active_graph(tests) else {
        return Vec::new();
    };
    let package_id = match snapshot.package_for(document, tests) {
        QueryProgress::Ready(Some(package)) => Some(package.id.as_str()),
        _ => None,
    };
    let key = path_key(document.path());
    let root = document.root_node();
    let fallback = root
        .named_children(&mut root.walk())
        .find(|n| n.kind() == "package_clause")
        .map_or(0..0, |n| n.byte_range());
    graph
        .package_errors()
        .iter()
        .filter(|error| match &error.path {
            Some(path) => path_key(path) == key,
            None => Some(error.package_id.as_str()) == package_id,
        })
        .filter(|error| {
            !(document.version().is_some() && syntax.is_empty() && is_parse_error(&error.message))
        })
        .map(|error| {
            let bytes = error
                .line
                .and_then(|line| {
                    let line = line.checked_sub(1)?;
                    let start = document.offset(Position::new(line, 0))?;
                    let end = document.offset(Position::new(line, u32::MAX))?;
                    let column = error.column.unwrap_or(1).saturating_sub(1) as usize;
                    let offset = document
                        .text()
                        .floor_char_boundary(start.saturating_add(column).min(end));
                    Some(offset..offset)
                })
                .unwrap_or_else(|| fallback.clone());
            DiagnosticItem::new(
                DiagnosticCode::PackageLoad,
                DiagnosticSeverity::Error,
                visible_range(document, bytes),
                error.message.clone(),
            )
        })
        .collect()
}

fn is_parse_error(message: &str) -> bool {
    message.lines().any(|line| {
        let detail = line.rsplit_once(": ").map_or(line, |(_, detail)| detail);
        [
            "expected ",
            "unexpected ",
            "invalid character",
            "illegal character",
            "illegal UTF-8",
            "string literal not terminated",
            "raw string literal not terminated",
            "rune literal not terminated",
            "comment not terminated",
            "missing import path",
        ]
        .iter()
        .any(|prefix| detail.starts_with(prefix))
            || line.contains("syntax error")
    })
}
