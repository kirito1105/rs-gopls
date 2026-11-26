use std::fmt::Write;

use lsp_types::Range;
use tree_sitter::Node;

use super::{
    Container, Package, Path, Resolver, Result, Source, TypeIdentity, TypeKind,
    is_predeclared_type, node_text, path_key,
};

pub(super) fn documentation(declaration: Node<'_>, text: &str) -> String {
    let group = declaration_group(declaration);
    let mut upper = upper_comments(declaration, text);
    if upper.is_empty()
        && let Some(group) = group
    {
        upper = upper_comments(group, text);
    }

    // In a single-spec declaration the trailing comment may be a sibling of
    // the declaration instead of the spec. A closing ')' belongs to the group.
    let tail = group
        .filter(|group| group.end_byte() == declaration.end_byte())
        .unwrap_or(declaration);
    let mut inline = trailing_comments(declaration, text);
    if inline.is_empty() && tail != declaration {
        inline = trailing_comments(tail, text);
    }
    [upper, inline]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn declaration_group(declaration: Node<'_>) -> Option<Node<'_>> {
    if !matches!(
        declaration.kind(),
        "var_spec" | "const_spec" | "type_spec" | "type_alias"
    ) {
        return None;
    }
    let mut owner = declaration.parent()?;
    if owner.kind() == "var_spec_list" {
        owner = owner.parent()?;
    }
    matches!(
        owner.kind(),
        "var_declaration" | "const_declaration" | "type_declaration"
    )
    .then_some(owner)
}

fn upper_comments(declaration: Node<'_>, text: &str) -> String {
    let mut comments = Vec::new();
    let mut next = declaration;
    while let Some(comment) = previous_sibling(next).filter(|node| node.kind() == "comment") {
        let gap = &text[comment.end_byte()..next.start_byte()];
        let line_start = text[..comment.start_byte()]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        if next.start_position().row != comment.end_position().row + 1
            || !gap.trim().is_empty()
            || !text[line_start..comment.start_byte()].trim().is_empty()
        {
            break;
        }
        if let Some(comment) = clean_comment(node_text(comment, text)) {
            comments.push(comment);
        }
        next = comment;
    }
    comments.reverse();
    comments.join("\n").trim().to_owned()
}

fn previous_sibling(node: Node<'_>) -> Option<Node<'_>> {
    node.prev_sibling().or_else(|| {
        node.parent()
            .filter(|parent| parent.kind() == "statement_list")?
            .prev_sibling()
    })
}

fn trailing_comments(declaration: Node<'_>, text: &str) -> String {
    let mut comments = Vec::new();
    let mut next = declaration.next_sibling().or_else(|| {
        declaration
            .parent()
            .filter(|parent| parent.kind() == "statement_list")?
            .next_sibling()
    });
    // Comments in a parameter list can follow the delineating comma.
    if next.is_some_and(|node| node.kind() == ",") {
        next = next.and_then(|node| node.next_sibling());
    }
    while let Some(comment) = next.filter(|node| {
        node.kind() == "comment" && node.start_position().row == declaration.end_position().row
    }) {
        if let Some(comment) = clean_comment(node_text(comment, text)) {
            comments.push(comment);
        }
        next = comment.next_sibling();
    }
    comments.join("\n").trim().to_owned()
}

fn clean_comment(comment: &str) -> Option<String> {
    if let Some(line) = comment.strip_prefix("//") {
        if directive(line) {
            return None;
        }
        return Some(line.strip_prefix(' ').unwrap_or(line).trim_end().to_owned());
    }
    let block = comment.strip_prefix("/*")?.strip_suffix("*/")?;
    let block = block.strip_prefix('*').unwrap_or(block);
    let (first, rest) = block.split_once('\n').unwrap_or((block, ""));
    let first = first.trim();
    if directive(first) {
        return None;
    }
    let mut lines = rest.lines().map(str::trim_end).collect::<Vec<_>>();
    let starred = lines.iter().any(|line| !line.is_empty())
        && lines
            .iter()
            .all(|line| line.is_empty() || line.trim_start().starts_with('*'));
    if starred {
        for line in &mut lines {
            if let Some(content) = line.trim_start().strip_prefix('*') {
                *line = content.strip_prefix(' ').unwrap_or(content);
            }
        }
    }
    let indent = lines
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    Some(
        std::iter::once(first)
            .chain(
                lines
                    .into_iter()
                    .map(|line| line.get(indent..).unwrap_or(line)),
            )
            .filter(|line| !directive(line))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
    )
}

fn directive(comment: &str) -> bool {
    let trimmed = comment.trim_start();
    let word = trimmed.split_whitespace().next().unwrap_or("");
    let line_directive = trimmed.strip_prefix("line ").is_some_and(|location| {
        location
            .trim_end()
            .rsplit_once(':')
            .is_some_and(|(_, number)| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            })
    });
    matches!(word, "+build" | "noinspection" | "region" | "endregion")
        || line_directive
        || matches!(word, "extern" | "export") && trimmed.len() == comment.len()
        || ["go:", "goland:", "language=", "formatter:", "@formatter:"]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
}

/// Render a signature from source so parameter names and comments need no
/// storage in the navigation index.
pub(super) fn function_signature(
    function: Node<'_>,
    text: &str,
    interface: Option<&str>,
) -> String {
    let mut signature = String::from("func");
    if let Some(receiver) = function.child_by_field_name("receiver") {
        let _ = write!(signature, " {}", parameter_list(receiver, text));
    } else if let Some(interface) = interface {
        let _ = write!(signature, " ({interface})");
    }
    if let Some(name) = function.child_by_field_name("name") {
        let _ = write!(signature, " {}", node_text(name, text));
    }
    if let Some(parameters) = function.child_by_field_name("type_parameters") {
        signature.push_str(&syntax_text(parameters, text));
    }
    write_parameter_and_result_lists(&mut signature, function, text);
    signature
}

pub(super) fn function_type(function: Node<'_>, text: &str) -> String {
    let mut signature = String::from("func");
    write_parameter_and_result_lists(&mut signature, function, text);
    signature
}

fn write_parameter_and_result_lists(signature: &mut String, function: Node<'_>, text: &str) {
    if let Some(parameters) = function.child_by_field_name("parameters") {
        signature.push_str(&parameter_list(parameters, text));
    }
    if let Some(result) = result_label_with(function, text, |ty| syntax_text(ty, text)) {
        let _ = write!(signature, " {result}");
    }
}

pub(super) fn parameter_labels(list: Node<'_>, text: &str) -> Vec<String> {
    parameter_labels_with(list, text, |ty| syntax_text(ty, text))
}

pub(super) fn result_label_with<'tree>(
    function: Node<'tree>,
    text: &str,
    render: impl FnMut(Node<'tree>) -> String,
) -> Option<String> {
    let result = function.child_by_field_name("result")?;
    let mut render = render;
    Some(if result.kind() == "parameter_list" {
        format!(
            "({})",
            parameter_labels_with(result, text, render).join(", ")
        )
    } else {
        render(result)
    })
}

pub(super) fn parameter_labels_with<'tree>(
    list: Node<'tree>,
    text: &str,
    mut render: impl FnMut(Node<'tree>) -> String,
) -> Vec<String> {
    let mut labels = Vec::new();
    for parameter in list.named_children(&mut list.walk()) {
        let Some(ty) = parameter.child_by_field_name("type") else {
            continue;
        };
        let ty = render(ty);
        let prefix = if parameter.kind() == "variadic_parameter_declaration" {
            "..."
        } else {
            ""
        };
        let first = labels.len();
        labels.extend(
            parameter
                .children_by_field_name("name", &mut parameter.walk())
                .filter(|name| name.is_named() && !name.is_extra())
                .map(|name| format!("{} {prefix}{ty}", node_text(name, text))),
        );
        if first == labels.len() {
            labels.push(format!("{prefix}{ty}"));
        }
    }
    labels
}

fn parameter_list(list: Node<'_>, text: &str) -> String {
    format!("({})", parameter_labels(list, text).join(", "))
}

/// Omit comment nodes from a displayed Go declaration or expression, while
/// preserving literal text (including struct tags) and Go's line breaks.
pub(super) fn syntax_text(node: Node<'_>, text: &str) -> String {
    syntax_text_with(node, text, &[])
}

pub(super) fn syntax_text_with(
    node: Node<'_>,
    text: &str,
    substitutions: &[(String, String)],
) -> String {
    let raw = node_text(node, text);
    if substitutions.is_empty() && !raw.contains("//") && !raw.contains("/*") {
        return raw.trim().to_owned();
    }

    let mut syntax = String::new();
    let mut start = node.start_byte();
    let end = node.end_byte();
    let mut cursor = node.walk();
    loop {
        let child = cursor.node();
        if child.kind() == "comment" {
            let line_start = text[start..child.start_byte()]
                .rfind('\n')
                .map_or(start, |i| start + i + 1);
            let line_end = text[child.end_byte()..end]
                .find('\n')
                .map_or(end, |i| child.end_byte() + i);
            if text[line_start..child.start_byte()].trim().is_empty()
                && text[child.end_byte()..line_end].trim().is_empty()
            {
                syntax.push_str(&text[start..line_start]);
                start = (line_end + 1).min(end);
            } else {
                syntax.push_str(&text[start..child.start_byte()]);
                let newlines = child.end_position().row - child.start_position().row;
                if newlines == 0 {
                    syntax.push(' ');
                } else {
                    syntax.extend(std::iter::repeat_n('\n', newlines));
                }
                start = child.end_byte();
            }
        } else if child.kind() == "type_identifier"
            && child
                .parent()
                .is_none_or(|parent| parent.kind() != "qualified_type")
            && let Some((_, ty)) = substitutions
                .iter()
                .find(|(parameter, _)| parameter == node_text(child, text))
        {
            syntax.push_str(&text[start..child.start_byte()]);
            syntax.push_str(ty);
            start = child.end_byte();
        } else if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                syntax.push_str(&text[start..end]);
                return syntax.trim().to_owned();
            }
        }
    }
}

pub(super) fn node_at_range(source: &Source, range: Range) -> Option<Node<'_>> {
    let start = source.lines.offset(&source.text, range.start)?;
    let end = source.lines.offset(&source.text, range.end)?;
    let node = source
        .tree
        .root_node()
        .descendant_for_byte_range(start, end)?;
    (node.byte_range() == (start..end)).then_some(node)
}

impl<'w> Resolver<'w, '_> {
    pub(super) fn type_text(
        &mut self,
        from: &'w Package,
        source: &Source,
        ty: &TypeIdentity<'w>,
    ) -> Result<Option<String>> {
        let mut text = "*".repeat(ty.pointers.into());
        match &ty.kind {
            TypeKind::Named { name, arguments } => {
                if !is_predeclared_type(name) && !std::ptr::eq(ty.package, from) {
                    let qualifier = source.imports.iter().find_map(|import| {
                        let alias = import.alias.as_deref().unwrap_or(&ty.package.name);
                        (alias != "."
                            && self
                                .workspace
                                .resolve_import(from, &import.path)
                                .is_some_and(|package| std::ptr::eq(package, ty.package)))
                        .then_some(alias)
                    });
                    let _ = write!(text, "{}.", qualifier.unwrap_or(&ty.package.name));
                }
                text.push_str(name);
                if !arguments.is_empty() {
                    let mut rendered = Vec::new();
                    for argument in arguments {
                        let Some(argument) = argument else {
                            return Ok(None);
                        };
                        let Some(argument) = self.type_text(from, source, argument)? else {
                            return Ok(None);
                        };
                        rendered.push(argument);
                    }
                    let _ = write!(text, "[{}]", rendered.join(", "));
                }
            }
            TypeKind::Container(container, _) => {
                let child = |resolver: &mut Self, ty| resolver.type_text(from, source, ty);
                let container = match container.as_ref() {
                    Container::Slice(element) => {
                        child(self, element)?.map(|element| format!("[]{element}"))
                    }
                    Container::Array { element, length } => child(self, element)?.map(|element| {
                        format!("[{}]{element}", length.as_deref().unwrap_or("..."))
                    }),
                    Container::Map { key, value } => child(self, key)?
                        .zip(child(self, value)?)
                        .map(|(key, value)| format!("map[{key}]{value}")),
                    Container::Channel {
                        element,
                        receive,
                        send,
                    } => {
                        let prefix = match (receive, send) {
                            (true, false) => "<-chan ",
                            (false, true) => "chan<- ",
                            _ => "chan ",
                        };
                        child(self, element)?.map(|element| format!("{prefix}{element}"))
                    }
                };
                let Some(container) = container else {
                    return Ok(None);
                };
                text.push_str(&container);
            }
            TypeKind::Function {
                file, signature, ..
            } => {
                let Some(origin) = signature.origin.function_type() else {
                    return Ok(None);
                };
                let Some(signature) = self.source_type_text(file, origin, source, function_type)?
                else {
                    return Ok(None);
                };
                text.push_str(&signature);
            }
            TypeKind::Inline { file, shape, .. } => {
                let Some(syntax) =
                    self.source_type_text(file, shape.origin, source, syntax_text)?
                else {
                    return Ok(None);
                };
                text.push_str(&syntax);
            }
            TypeKind::Parameter(_) => return Ok(None),
        }
        Ok(Some(text))
    }

    fn source_type_text(
        &mut self,
        path: &Path,
        origin: Range,
        source: &Source,
        format: impl FnOnce(Node<'_>, &str) -> String,
    ) -> Result<Option<String>> {
        let definition;
        let source = if path_key(path) == path_key(&source.path) {
            source
        } else if let Some(disk) = self.indexes.reference_source(path)? {
            definition = disk;
            definition.as_ref()
        } else {
            return Ok(None);
        };
        Ok(node_at_range(source, origin)
            .and_then(|token| token.parent())
            .map(|node| format(node, &source.text)))
    }
}

pub(super) fn builtin_function(name: &str) -> Option<&'static str> {
    Some(match name {
        "append" => "func append(slice []Type, elems ...Type) []Type",
        "cap" => "func cap(v Type) int",
        "clear" => "func clear[T ~[]Type | ~map[Type]Type1](t T)",
        "close" => "func close(c chan<- Type)",
        "complex" => "func complex(r FloatType, i FloatType) ComplexType",
        "copy" => "func copy(dst []Type, src []Type) int",
        "delete" => "func delete(m map[Type]Type1, key Type)",
        "imag" => "func imag(c ComplexType) FloatType",
        "len" => "func len(v Type) int",
        "make" => "func make(t Type, size ...IntegerType) Type",
        "max" => "func max[T cmp.Ordered](x T, y ...T) T",
        "min" => "func min[T cmp.Ordered](x T, y ...T) T",
        "new" => "func new(Type) *Type",
        "panic" => "func panic(v any)",
        "print" => "func print(args ...Type)",
        "println" => "func println(args ...Type)",
        "real" => "func real(c ComplexType) FloatType",
        "recover" => "func recover() any",
        _ => return None,
    })
}
