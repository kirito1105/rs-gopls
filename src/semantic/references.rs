use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use lsp_types::{Position, Range};
use tree_sitter::Node;

use super::Engine;
use super::index::draft_package;
use super::query::{QueryProgress, SemanticTask};
use super::resolver::{Resolver, is_exported};
use super::source::{ReferenceQuery, Source, node_text};
use crate::scope::reference_binding;
use crate::workspace::{Package, path_key};

pub(crate) struct ReferenceSearch {
    spelling: String,
    target_path: PathBuf,
    target_range: Range,
    include_declaration: bool,
    files: VecDeque<ReferenceCandidate>,
    matches: Vec<(PathBuf, Range)>,
}

struct ReferenceCandidate {
    key: PathBuf,
    path: PathBuf,
    package_id: String,
}

impl Engine {
    pub(crate) fn references(
        &mut self,
        path: &Path,
        position: Position,
        include_declaration: bool,
    ) -> Result<QueryProgress<Vec<(PathBuf, Range)>, ReferenceSearch>> {
        let mut snapshot = self.state.snapshot(self.include_tests);
        let Some(document) = snapshot.document(path)? else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };
        let source = document.indexed();
        let Some(reference) = source.reference_at(position) else {
            return Ok(QueryProgress::Ready(Vec::new()));
        };
        let mut locations = Vec::new();
        if let ReferenceQuery::Name(name) = reference
            && let Some(declaration) = reference_binding(name, &source.text)
            && let Some(scope) = local_reference_scope(declaration)
        {
            source.for_each_name(scope, node_text(declaration, &source.text), |candidate| {
                if (include_declaration || candidate != declaration)
                    && matches!(source.reference_from_leaf(candidate, candidate.start_byte()), Some(ReferenceQuery::Name(node)) if node == candidate)
                    && reference_binding(candidate, &source.text) == Some(declaration)
                {
                    locations.push((source.path.clone(), source.range(candidate)));
                }
            });
            return Ok(QueryProgress::Ready(locations));
        }

        self.include_tests = true;
        let workspace = match snapshot.active_graph(true) {
            QueryProgress::Ready(workspace) => workspace,
            QueryProgress::Pending(load) => return Ok(QueryProgress::Pending(load)),
            QueryProgress::Searching(task) => match task {},
        };
        let Some(from) = snapshot.active_package(workspace, source) else {
            return Ok(QueryProgress::Ready(locations));
        };
        let root = snapshot.root();
        let mut resolver = snapshot.resolver(workspace);
        let Some(search) =
            resolver.reference_search(from, source, reference, root, include_declaration)?
        else {
            return Ok(QueryProgress::Ready(locations));
        };
        Ok(if search.files.is_empty() {
            QueryProgress::Ready(search.matches)
        } else {
            QueryProgress::Searching(search)
        })
    }
}

impl SemanticTask<Vec<(PathBuf, Range)>> for ReferenceSearch {
    fn advance(&mut self, engine: &mut Engine) -> Result<bool> {
        let mut snapshot = engine.snapshot();
        let workspace = snapshot
            .workspace()
            .context("reference search needs a workspace")?;
        ensure!(
            matches!(snapshot.active_graph(false), QueryProgress::Ready(_)),
            "workspace changed during reference search"
        );
        let root = snapshot.root();
        let mut resolver = snapshot.resolver(workspace);
        let started = Instant::now();
        // Yield between files so pending editor requests and notifications can run.
        for _ in 0..8 {
            let Some(candidate) = self.files.pop_front() else {
                break;
            };
            resolver.scan_reference_file(self, candidate, root)?;
            if started.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        Ok(self.files.is_empty())
    }

    fn finish(self) -> Vec<(PathBuf, Range)> {
        self.matches
    }
}

fn local_reference_scope(declaration: Node<'_>) -> Option<Node<'_>> {
    let type_parameter = declaration
        .parent()
        .is_some_and(|node| node.kind() == "type_parameter_declaration");
    let mut cursor = declaration.parent();
    while let Some(node) = cursor {
        match node.kind() {
            "function_declaration" | "method_declaration" | "func_literal" => return Some(node),
            "type_spec" | "type_alias" if type_parameter => return Some(node),
            _ => cursor = node.parent(),
        }
    }
    None
}

impl<'w> Resolver<'w, '_> {
    fn reference_search(
        &mut self,
        from: &'w Package,
        source: &Source,
        reference: ReferenceQuery<'_>,
        root: &Path,
        include_declaration: bool,
    ) -> Result<Option<ReferenceSearch>> {
        let spelling = node_text(reference.name(), &source.text);
        if spelling == "_"
            || !spelling
                .chars()
                .next()
                .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
        {
            return Ok(None);
        }
        let Some((target, target_range)) =
            self.reference_location(from, source, reference, root)?
        else {
            return Ok(None);
        };
        let target_key = path_key(&target);
        let Some(owner) = self.workspace.package_of_file(&target).or_else(|| {
            self.indexes
                .documents
                .get(&target_key)
                .and_then(|source| draft_package(self.workspace, source))
        }) else {
            return Ok(None);
        };
        let transitive = if is_exported(spelling) {
            self.is_member_declaration(&target, target_range)?
        } else {
            false
        };
        let packages = self.reference_packages(owner, is_exported(spelling), transitive);
        let mut candidates = BTreeMap::new();
        for view in packages {
            for path in self.indexes.files(view).iter() {
                let context = self.workspace.package_of_file(path).unwrap_or(view);
                candidates
                    .entry(path_key(path))
                    .or_insert_with(|| (path.clone(), context));
            }
        }
        let mut matches = Vec::new();
        if include_declaration && !candidates.contains_key(&target_key) {
            matches.push((target, target_range));
        }
        Ok(Some(ReferenceSearch {
            spelling: spelling.to_owned(),
            target_path: target_key,
            target_range,
            include_declaration,
            files: candidates
                .into_iter()
                .map(|(key, (path, package))| ReferenceCandidate {
                    key,
                    path,
                    package_id: package.id.clone(),
                })
                .collect(),
            matches,
        }))
    }

    fn scan_reference_file(
        &mut self,
        search: &mut ReferenceSearch,
        file: ReferenceCandidate,
        root: &Path,
    ) -> Result<()> {
        let Some(package) = self.workspace.package(&file.package_id) else {
            return Ok(());
        };
        let Some(source) = self.indexes.reference_source(&file.path)? else {
            return Ok(());
        };
        let source = source.as_ref();
        for &offset in source.name_offsets(&search.spelling) {
            let Some(candidate) = source.reference_at_offset(offset) else {
                continue;
            };
            let range = source.range(candidate.name());
            if !search.include_declaration
                && file.key == search.target_path
                && range == search.target_range
            {
                continue;
            }
            if let Some((declaration, declaration_range)) =
                self.reference_location(package, source, candidate, root)?
                && declaration_range == search.target_range
                && path_key(&declaration) == search.target_path
            {
                search.matches.push((source.path.clone(), range));
            }
        }
        Ok(())
    }

    fn is_member_declaration(&mut self, path: &Path, range: Range) -> Result<bool> {
        let Some(source) = self.indexes.reference_source(path)? else {
            return Ok(false);
        };
        let source = source.as_ref();
        let Some(mut node) = source.reference_at(range.start).map(ReferenceQuery::name) else {
            return Ok(false);
        };
        while let Some(owner) = node.parent() {
            match owner.kind() {
                "method_declaration" | "method_elem" | "field_declaration" => return Ok(true),
                "function_declaration"
                | "type_spec"
                | "type_alias"
                | "var_spec"
                | "const_spec"
                | "source_file" => return Ok(false),
                _ => node = owner,
            }
        }
        Ok(false)
    }

    fn reference_packages(
        &self,
        owner: &'w Package,
        exported: bool,
        transitive: bool,
    ) -> Vec<&'w Package> {
        let mut queue: Vec<_> = self.workspace.same_package_variants(owner).collect();
        if queue.is_empty() {
            queue.push(owner);
        }
        let mut seen: HashSet<_> = queue.iter().map(|package| package.id.as_str()).collect();
        if exported {
            let overlay_importers = self.overlay_importers();
            let seeds = queue.len();
            let mut next = 0;
            while next < if transitive { queue.len() } else { seeds } {
                let package = queue[next];
                next += 1;
                for importer in self.workspace.direct_importers(package).chain(
                    overlay_importers
                        .get(package.id.as_str())
                        .into_iter()
                        .flatten()
                        .copied(),
                ) {
                    if seen.insert(importer.id.as_str()) {
                        queue.push(importer);
                    }
                }
            }
        }
        queue.retain(|package| self.workspace.is_project_package(package));
        queue
    }

    fn overlay_importers(&self) -> HashMap<&'w str, Vec<&'w Package>> {
        let mut importers = HashMap::<_, Vec<_>>::new();
        for source in self.indexes.documents.values() {
            let from = self.workspace.package_of_file(&source.path).or_else(|| {
                let package = draft_package(self.workspace, source)?;
                self.indexes
                    .drafts
                    .get(&package.directory_and_name())?
                    .contains(&&source.path)
                    .then_some(package)
            });
            let Some(from) = from else {
                continue;
            };
            if !self.workspace.is_project_package(from) {
                continue;
            }
            for import in &source.imports {
                if let Some(target) = self.workspace.resolve_import(from, &import.path) {
                    importers.entry(target.id.as_str()).or_default().push(from);
                }
            }
        }
        importers
    }
}
