use std::path::Path;

use anyhow::Result;
use lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, Location, Range, SymbolInformation, SymbolKind, Uri,
};

use super::{Engine, Source, read_source};
use crate::symbols::{OutlineKind, OutlineSymbol};
use crate::workspace::path_key;

impl Engine {
    pub fn document_symbols(
        &mut self,
        path: &Path,
        uri: &Uri,
        hierarchical: bool,
    ) -> Result<DocumentSymbolResponse> {
        let disk;
        let source = if let Some(source) = self.documents.get(&path_key(path)) {
            source
        } else if path.extension().is_some_and(|extension| extension == "go") {
            disk = read_source(&mut self.parser, path)?;
            &disk
        } else {
            return Ok(if hierarchical {
                DocumentSymbolResponse::Nested(Vec::new())
            } else {
                DocumentSymbolResponse::Flat(Vec::new())
            });
        };

        let outline = source.outline();
        if hierarchical {
            return Ok(DocumentSymbolResponse::Nested(
                outline
                    .symbols
                    .iter()
                    .map(|symbol| nested(source, symbol))
                    .collect(),
            ));
        }

        let mut entries = Vec::new();
        flatten(&outline.symbols, outline.package.as_deref(), &mut entries);
        // Multi-name fields may share an inline type and its member ranges.
        entries.sort_by_key(|(symbol, _)| symbol.selection_range.start);
        Ok(DocumentSymbolResponse::Flat(
            entries
                .into_iter()
                .map(|(symbol, container)| {
                    symbol_information(
                        symbol.name.clone(),
                        symbol.kind,
                        container.map(str::to_owned),
                        uri.clone(),
                        source.byte_range(symbol.selection_range.clone()),
                    )
                })
                .collect(),
        ))
    }
}

#[allow(deprecated)]
fn nested(source: &Source, symbol: &OutlineSymbol) -> DocumentSymbol {
    let children = symbol.children();
    DocumentSymbol {
        name: match &symbol.receiver {
            Some(receiver) => format!("({}).{}", receiver.spelling, symbol.name),
            None => symbol.name.clone(),
        },
        detail: symbol.detail.clone(),
        kind: lsp_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: source.byte_range(symbol.range.clone()),
        selection_range: source.byte_range(symbol.selection_range.clone()),
        children: (!children.is_empty())
            .then(|| children.iter().map(|child| nested(source, child)).collect()),
    }
}

fn flatten<'a>(
    symbols: &'a [OutlineSymbol],
    container: Option<&'a str>,
    entries: &mut Vec<(&'a OutlineSymbol, Option<&'a str>)>,
) {
    for symbol in symbols {
        let owner = symbol
            .receiver
            .as_ref()
            .map(|receiver| receiver.base.as_str())
            .or(container);
        entries.push((symbol, owner));
        flatten(symbol.children(), Some(&symbol.name), entries);
    }
}

#[allow(deprecated)]
pub(super) fn symbol_information(
    name: String,
    kind: OutlineKind,
    container_name: Option<String>,
    uri: Uri,
    range: Range,
) -> SymbolInformation {
    SymbolInformation {
        name,
        kind: lsp_kind(kind),
        tags: None,
        deprecated: None,
        location: Location::new(uri, range),
        container_name,
    }
}

fn lsp_kind(kind: OutlineKind) -> SymbolKind {
    match kind {
        OutlineKind::Constant => SymbolKind::CONSTANT,
        OutlineKind::Variable => SymbolKind::VARIABLE,
        OutlineKind::Class => SymbolKind::CLASS,
        OutlineKind::Struct => SymbolKind::STRUCT,
        OutlineKind::Interface | OutlineKind::EmbeddedInterface => SymbolKind::INTERFACE,
        OutlineKind::Field => SymbolKind::FIELD,
        OutlineKind::Function => SymbolKind::FUNCTION,
        OutlineKind::Method => SymbolKind::METHOD,
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, TextDocumentContentChangeEvent};

    use super::*;
    use crate::{path_uri, test_support::TestProject};

    #[test]
    fn projects_hierarchical_and_flat_ranges_in_utf16() {
        let project = TestProject::new();
        let path = project.write(
            "outline.go",
            concat!(
                "package service\r\n",
                "var Emoji = \"😀\"; var 𐐀Value int\r\n",
                "type Box[T any] struct{ A, B struct{ Child int } }\r\n",
                "func (b *Box[T]) Build() error { return nil }\r\n",
            ),
        );
        let uri = path_uri(&path).unwrap();
        let mut engine = Engine::new(project.root.clone());
        let DocumentSymbolResponse::Nested(symbols) =
            engine.document_symbols(&path, &uri, true).unwrap()
        else {
            panic!("expected hierarchical document symbols");
        };
        let unicode = &symbols[1];
        assert_eq!(unicode.name, "𐐀Value");
        assert_eq!(unicode.kind, SymbolKind::VARIABLE);
        assert_eq!(
            unicode.selection_range,
            Range::new(Position::new(1, 22), Position::new(1, 29))
        );
        assert_eq!(
            unicode.range,
            Range::new(Position::new(1, 18), Position::new(1, 33))
        );
        let ty = &symbols[2];
        assert_eq!(ty.kind, SymbolKind::STRUCT);
        let children = ty.children.as_ref().unwrap();
        for (field, name) in children.iter().zip(["A", "B"]) {
            assert_eq!(field.name, name);
            assert_eq!(field.kind, SymbolKind::FIELD);
            assert_eq!(field.children.as_ref().unwrap()[0].name, "Child");
            assert!(ty.range.start <= field.range.start && field.range.end <= ty.range.end);
        }
        let method = &symbols[3];
        assert_eq!(method.name, "(*Box[T]).Build");
        assert_eq!(method.detail.as_deref(), Some("() error"));
        assert_eq!(method.kind, SymbolKind::METHOD);
        assert_eq!(
            method.selection_range,
            Range::new(Position::new(3, 17), Position::new(3, 22))
        );

        let DocumentSymbolResponse::Flat(symbols) =
            engine.document_symbols(&path, &uri, false).unwrap()
        else {
            panic!("expected flat document symbols");
        };
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| (symbol.name.as_str(), symbol.container_name.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("Emoji", Some("service")),
                ("𐐀Value", Some("service")),
                ("Box", Some("service")),
                ("A", Some("Box")),
                ("B", Some("Box")),
                ("Child", Some("A")),
                ("Child", Some("B")),
                ("Build", Some("Box")),
            ]
        );
        assert_eq!(symbols[1].location.range, unicode.selection_range);
        assert_eq!(
            symbols.last().unwrap().location.range,
            method.selection_range
        );
        assert!(symbols.iter().all(|symbol| symbol.location.uri == uri));
        assert!(
            symbols
                .windows(2)
                .all(|two| two[0].location.range.start <= two[1].location.range.start)
        );
    }

    #[test]
    fn bare_members_enclose_identifiers_at_crlf_and_inline_boundaries() {
        let project = TestProject::new();
        let path = project.write(
            "embeddings.go",
            "package p\r\ntype T struct {\r\n\tEmbedded\r\n}\r\ntype I interface{Base}\r\n",
        );
        let uri = path_uri(&path).unwrap();
        let mut engine = Engine::new(project.root.clone());
        let DocumentSymbolResponse::Nested(symbols) =
            engine.document_symbols(&path, &uri, true).unwrap()
        else {
            panic!("expected hierarchical document symbols");
        };
        for (parent, name, selection, range) in [
            (
                &symbols[0],
                "Embedded",
                Range::new(Position::new(2, 1), Position::new(2, 9)),
                Range::new(Position::new(2, 1), Position::new(3, 0)),
            ),
            (
                &symbols[1],
                "Base",
                Range::new(Position::new(4, 17), Position::new(4, 21)),
                Range::new(Position::new(4, 17), Position::new(4, 22)),
            ),
        ] {
            let child = &parent.children.as_ref().unwrap()[0];
            assert_eq!(child.name, name);
            assert_eq!(child.selection_range, selection);
            assert_eq!(child.range, range);
            assert!(parent.range.start <= range.start && range.end <= parent.range.end);
        }
    }

    #[test]
    fn outline_is_lazy_and_invalidated_by_incremental_overlay_edits() {
        let project = TestProject::new();
        let path = project.write("outline.go", "package disk\nvar Disk int\n");
        let uri = path_uri(&path).unwrap();
        let key = path_key(&path);
        let mut engine = Engine::new(project.root.clone());
        engine
            .open(
                path.clone(),
                "package buffer\r\nvar Before int\r\nfunc Broken(\r\n".into(),
                1,
            )
            .unwrap();
        assert!(engine.documents[&key].outline.get().is_none());

        let query = |engine: &mut Engine, expected: &str| {
            let DocumentSymbolResponse::Flat(symbols) =
                engine.document_symbols(&path, &uri, false).unwrap()
            else {
                panic!("expected flat document symbols");
            };
            assert_eq!(symbols[0].name, expected);
            symbols[0].location.range
        };
        assert_eq!(
            query(&mut engine, "Before"),
            Range::new(Position::new(1, 4), Position::new(1, 10))
        );
        assert!(engine.documents[&key].outline.get().is_some());
        engine
            .edit(
                &path,
                &[TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(1, 0), Position::new(1, 10))),
                    range_length: None,
                    text: "// 🙂\r\nvar Updated".into(),
                }],
                2,
            )
            .unwrap();
        assert!(engine.documents[&key].outline.get().is_none());
        assert_eq!(
            query(&mut engine, "Updated"),
            Range::new(Position::new(2, 4), Position::new(2, 11))
        );
        engine.close(&path);
        assert_eq!(
            query(&mut engine, "Disk"),
            Range::new(Position::new(1, 4), Position::new(1, 8))
        );
    }
}
