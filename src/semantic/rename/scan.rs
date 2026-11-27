use super::super::index::IndexedSource;
use super::super::source::{Source, node_text};
use super::target::{Target, binding};
use super::{Engine, Location};
use crate::workspace::path_key;
use anyhow::{Context, Result, ensure};
use lsp_types::{Position, Range, TextEdit};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tree_sitter::Node;

struct Occurrence {
    range: Range,
    binding: Location,
}

#[derive(Default)]
pub(super) struct FileScan {
    pub(super) edits: Vec<TextEdit>,
    pub(super) embedded_fields: Vec<Location>,
    occurrences: Vec<Occurrence>,
    alias: Option<(Range, Range)>,
    // Cumulative UTF-16 width change at each original edit end, per line.
    columns: Vec<(Position, i64)>,
}

pub(super) fn embedded_type_use(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "field_declaration" => return parent.child_by_field_name("name").is_none(),
            "generic_type" | "qualified_type"
                if parent
                    .child_by_field_name("type")
                    .or_else(|| parent.child_by_field_name("name"))
                    == Some(node) =>
            {
                node = parent
            }
            "pointer_type" => node = parent,
            _ => break,
        }
    }
    false
}

fn offsets(source: &Source, spelling: &str) -> Vec<usize> {
    if matches!(spelling, "true" | "false" | "nil" | "iota") {
        // The grammar represents these shadowable predeclared names as distinct tokens.
        source
            .text
            .match_indices(spelling)
            .map(|(offset, _)| offset)
            .filter(|offset| {
                source.reference_at_offset(*offset).is_some_and(|r| {
                    r.name().start_byte() == *offset
                        && node_text(r.name(), &source.text) == spelling
                })
            })
            .collect()
    } else {
        source.name_offsets(spelling).to_vec()
    }
}

pub(super) fn collect(
    engine: &mut Engine,
    path: &Path,
    target: &Target,
    new_name: &str,
    family: &[Location],
) -> Result<FileScan> {
    let mut snapshot = engine.snapshot();
    let workspace = snapshot.workspace().context("missing workspace")?;
    let doc = snapshot.document(path)?.context("missing rename input")?;
    let source = doc.indexed();
    ensure!(
        !source.tree.root_node().has_error()
            || (!source.text.contains(&target.spelling) && !source.text.contains(new_name)),
        "cannot verify references in incomplete Go file {}",
        path.display()
    );
    let package = snapshot
        .active_package(workspace, source)
        .context("missing active package")?;
    let root = snapshot.root();
    let mut resolver = snapshot.resolver(workspace);
    let mut scan = FileScan::default();
    if target.implicit_import && path_key(path) == path_key(&target.location.0) {
        let origin = target.location.1;
        let alias_end = Position::new(
            origin.start.line,
            origin.start.character + new_name.encode_utf16().count() as u32,
        );
        scan.alias = Some((origin, Range::new(origin.start, alias_end)));
        scan.edits.push(TextEdit::new(
            Range::new(origin.start, origin.start),
            format!("{new_name} "),
        ));
    }
    for spelling in [target.spelling.as_str(), new_name] {
        for offset in offsets(source, spelling) {
            let Some(reference) = source.reference_at_offset(offset) else {
                continue;
            };
            let node = reference.name();
            if node.start_byte() != offset
                || node.parent().is_some_and(|p| p.kind() == "package_clause")
            {
                continue;
            }
            let range = source.range(node);
            let resolved =
                binding(&mut resolver, package, source, reference, root)?.with_context(|| {
                    format!(
                        "cannot verify the binding of {spelling} at {}:{}",
                        path.display(),
                        range.start.line + 1
                    )
                })?;
            let location = (path_key(&resolved.0), resolved.1);
            let selected = family
                .iter()
                .any(|(path, r)| path_key(path) == location.0 && *r == location.1);
            if selected {
                ensure!(
                    spelling == target.spelling,
                    "inconsistent spelling at rename target"
                );
                if embedded_type_use(node) {
                    scan.embedded_fields.push((source.path.clone(), range));
                }
                if super::super::resolver::is_exported(spelling)
                    && !super::super::resolver::is_exported(new_name)
                {
                    let owner = workspace
                        .package_of_file(&resolved.0)
                        .or_else(|| {
                            let draft = resolver.indexes.documents.get(&location.0)?;
                            super::super::index::draft_package(workspace, draft)
                        })
                        .context("missing declaration package")?;
                    ensure!(
                        owner.directory_and_name() == package.directory_and_name(),
                        "renaming {spelling} to {new_name} would make a cross-package reference inaccessible"
                    );
                }
                scan.edits.push(TextEdit::new(range, new_name.into()));
            }
            scan.occurrences.push(Occurrence {
                range,
                binding: location,
            });
        }
    }
    scan.edits.sort_by_key(|edit| edit.range.start);
    Ok(scan)
}

impl FileScan {
    pub(super) fn add_field_uses(
        &mut self,
        engine: &mut Engine,
        path: &Path,
        fields: &[Location],
        new_name: &str,
    ) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let mut snapshot = engine.snapshot();
        let workspace = snapshot.workspace().context("missing workspace")?;
        let doc = snapshot.document(path)?.context("missing usage file")?;
        let from = snapshot
            .active_package(workspace, doc.indexed())
            .context("missing usage package")?;
        for usage in &self.occurrences {
            if !fields
                .iter()
                .any(|(p, r)| path_key(p) == usage.binding.0 && *r == usage.binding.1)
            {
                continue;
            }
            if !super::super::resolver::is_exported(new_name) {
                let declaration = snapshot
                    .document(&usage.binding.0)?
                    .context("missing embedded field")?;
                let owner = snapshot
                    .active_package(workspace, declaration.indexed())
                    .context("missing embedding package")?;
                ensure!(
                    owner.directory_and_name() == from.directory_and_name(),
                    "renamed embedded field would not be visible outside its package"
                );
            }
            self.edits.push(TextEdit::new(usage.range, new_name.into()));
        }
        self.edits.sort_by_key(|e| e.range.start);
        self.edits.dedup_by_key(|e| e.range);
        Ok(())
    }

    fn position(&self, pos: Position) -> Position {
        let count = self.columns.partition_point(|(end, _)| *end <= pos);
        let shift = self.columns[..count]
            .last()
            .filter(|(end, _)| end.line == pos.line)
            .map_or(0, |(_, shift)| *shift);
        Position::new(pos.line, (i64::from(pos.character) + shift) as u32)
    }

    fn mapped(&self, range: Range) -> Range {
        if let Some((original, alias)) = self.alias
            && original == range
        {
            return alias;
        }
        Range::new(self.position(range.start), self.position(range.end))
    }

    pub(super) fn apply(&mut self, engine: &mut Engine, path: &Path) -> Result<()> {
        if self.edits.is_empty() {
            return Ok(());
        }
        let key = path_key(path);
        let source = &engine.state.documents[&key];
        let mut updated = String::with_capacity(source.text.len());
        let mut previous = 0;
        let mut line = None;
        let mut shift = 0;
        for edit in &self.edits {
            let start = source
                .lines
                .offset(&source.text, edit.range.start)
                .context("invalid edit start")?;
            let end = source
                .lines
                .offset(&source.text, edit.range.end)
                .context("invalid edit end")?;
            ensure!(
                previous <= start && start <= end,
                "overlapping rename edits in {}",
                path.display()
            );
            updated.push_str(&source.text[previous..start]);
            updated.push_str(&edit.new_text);
            previous = end;
            if line != Some(edit.range.start.line) {
                line = Some(edit.range.start.line);
                shift = 0;
            }
            shift += edit.new_text.encode_utf16().count() as i64
                - i64::from(edit.range.end.character - edit.range.start.character);
            self.columns.push((edit.range.end, shift));
        }
        updated.push_str(&source.text[previous..]);
        let version = source.document_version;
        let updated = Source::parse(&mut engine.state.parser, path.into(), updated, None)?;
        ensure!(
            !updated.tree.root_node().has_error(),
            "cannot verify the syntax of {}",
            path.display()
        );
        engine
            .state
            .documents
            .insert(key, IndexedSource::new(updated, version));
        Ok(())
    }

    pub(super) fn validate(
        &self,
        engine: &mut Engine,
        path: &Path,
        scans: &BTreeMap<PathBuf, FileScan>,
    ) -> Result<()> {
        if self.occurrences.is_empty() {
            return Ok(());
        }
        let mut snapshot = engine.snapshot();
        let workspace = snapshot.workspace().context("missing workspace")?;
        let doc = snapshot.document(path)?.context("missing rename input")?;
        let source = doc.indexed();
        let from = snapshot
            .active_package(workspace, source)
            .context("missing active package")?;
        let root = snapshot.root();
        let mut resolver = snapshot.resolver(workspace);
        for occurrence in &self.occurrences {
            let range = self.mapped(occurrence.range);
            let reference = source
                .reference_at(range.start)
                .filter(|r| source.range(r.name()) == range)
                .context("edited identifier no longer has the same syntactic role")?;
            let resolved = binding(&mut resolver, from, source, reference, root)?
                .map(|(p, r)| (path_key(&p), r));
            ensure!(
                resolved == Some(map_location(scans, &occurrence.binding)),
                "rename changes the binding of {} at {}:{}",
                node_text(reference.name(), &source.text),
                path.display(),
                range.start.line + 1
            );
        }
        Ok(())
    }
}

pub(super) fn map_location(scans: &BTreeMap<PathBuf, FileScan>, location: &Location) -> Location {
    let key = path_key(&location.0);
    let range = scans.get(&key).map_or(location.1, |s| s.mapped(location.1));
    (key, range)
}
