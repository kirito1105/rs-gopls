type MethodShape = (usize, usize, bool);

pub(super) struct InterfaceSpellings {
    // Missing spellings mean the scan could not rule out a match.
    methods: Option<Vec<(String, Option<MethodShape>)>>,
}

impl Default for InterfaceSpellings {
    fn default() -> Self {
        Self {
            methods: Some(Vec::new()),
        }
    }
}

impl InterfaceSpellings {
    pub(super) fn matches(&self, matches: impl Fn(&str, Option<MethodShape>) -> bool) -> bool {
        self.methods
            .as_ref()
            .is_none_or(|methods| methods.iter().any(|(name, shape)| matches(name, *shape)))
    }
}

/// Collects explicit spellings for reuse across queries. Callers looking for
/// aliases or promoted methods must also keep importers of matching packages.
pub(super) fn interface_spellings(text: &str) -> InterfaceSpellings {
    if !text.contains("interface") {
        return InterfaceSpellings::default();
    }
    InterfaceSpellings {
        methods: scan(text).ok(),
    }
}

fn scan(text: &str) -> Result<Vec<(String, Option<MethodShape>)>, ()> {
    let mut methods = Vec::new();
    let mut lexer = Lexer { rest: text };
    let mut interface_depth = 0;
    while let Some(token) = lexer.next()? {
        match token.kind {
            Kind::Word("interface") => {
                if lexer.next()?.map(|token| token.kind) != Some(Kind::Byte(b'{')) {
                    return Err(());
                }
                interface_depth += 1;
            }
            Kind::Byte(b'{') if interface_depth != 0 => interface_depth += 1,
            Kind::Byte(b'}') if interface_depth != 0 => interface_depth -= 1,
            Kind::Word(name) if interface_depth != 0 => {
                let mut signature = lexer;
                if signature.next()?.map(|token| token.kind) == Some(Kind::Byte(b'(')) {
                    methods.push((name.to_owned(), method_shape(signature)));
                }
            }
            _ => {}
        }
    }
    if interface_depth != 0 {
        return Err(());
    }
    Ok(methods)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind<'a> {
    Word(&'a str),
    Byte(u8),
    Ellipsis,
    Literal,
}

#[derive(Clone, Copy)]
struct Token<'a> {
    kind: Kind<'a>,
    newline: bool,
}

#[derive(Clone, Copy)]
struct Lexer<'a> {
    rest: &'a str,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Result<Option<Token<'a>>, ()> {
        let mut newline = false;
        loop {
            let rest = self
                .rest
                .trim_start_matches(|ch: char| ch.is_ascii_whitespace());
            newline |= self.rest[..self.rest.len() - rest.len()].contains('\n');
            self.rest = rest;
            if let Some(comment) = rest.strip_prefix("//") {
                self.rest = comment.find('\n').map_or("", |end| &comment[end..]);
            } else if let Some(comment) = rest.strip_prefix("/*") {
                let end = comment.find("*/").ok_or(())?;
                newline |= comment[..end].contains('\n');
                self.rest = &comment[end + 2..];
            } else {
                break;
            }
        }

        let Some(&first) = self.rest.as_bytes().first() else {
            return Ok(None);
        };
        let (kind, len) = match first {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let len = self
                    .rest
                    .bytes()
                    .take_while(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    .count();
                (Kind::Word(&self.rest[..len]), len)
            }
            b'`' => (Kind::Literal, self.rest[1..].find('`').ok_or(())? + 2),
            b'\'' | b'"' => (
                Kind::Literal,
                quoted_len(self.rest.as_bytes(), first).ok_or(())?,
            ),
            b'.' if self.rest.starts_with("...") => (Kind::Ellipsis, 3),
            b'!'..=b'~' => (Kind::Byte(first), 1),
            // Let the real parser handle Unicode identifiers and unusual bytes.
            _ => return Err(()),
        };
        self.rest = &self.rest[len..];
        Ok(Some(Token { kind, newline }))
    }

    fn peek(mut self) -> Result<Option<Token<'a>>, ()> {
        self.next()
    }
}

fn quoted_len(text: &[u8], quote: u8) -> Option<usize> {
    let mut escaped = false;
    for (index, &byte) in text.iter().enumerate().skip(1) {
        if byte == b'\n' {
            return None;
        }
        if byte == quote && !escaped {
            return Some(index + 1);
        }
        escaped = byte == b'\\' && !escaped;
    }
    None
}

fn method_shape(mut lexer: Lexer<'_>) -> Option<MethodShape> {
    let (parameters, variadic) = parameter_list(&mut lexer)?;
    let token = lexer.peek().ok()??;
    let results = match token.kind {
        Kind::Byte(b';' | b'}') => return Some((parameters, 0, variadic)),
        _ if token.newline => return None,
        Kind::Byte(b'(') => {
            lexer.next().ok()?;
            let (results, variadic_results) = parameter_list(&mut lexer)?;
            if variadic_results {
                return None;
            }
            results
        }
        _ if !simple_type(&mut lexer)? => 1,
        _ => return None,
    };
    let end = lexer.next().ok()??;
    (end.newline || matches!(end.kind, Kind::Byte(b';' | b'}')))
        .then_some((parameters, results, variadic))
}

// Every comma in these simple declarations adds either a parameter or a name
// sharing the following type. Complex types leave the shape unchecked.
fn parameter_list(lexer: &mut Lexer<'_>) -> Option<(usize, bool)> {
    let mut count = 0;
    let mut variadic = false;
    loop {
        if lexer.peek().ok()??.kind == Kind::Byte(b')') {
            lexer.next().ok()?;
            return Some((count, variadic));
        }
        if variadic {
            return None;
        }
        let mut item = *lexer;
        let (is_variadic, end) = parameter(&mut item).or_else(|| {
            item = *lexer;
            identifier(item.next().ok()??.kind)?;
            parameter(&mut item)
        })?;
        *lexer = item;
        count += 1;
        variadic = is_variadic;
        if end == Kind::Byte(b')') {
            return Some((count, variadic));
        }
    }
}

fn parameter<'a>(lexer: &mut Lexer<'a>) -> Option<(bool, Kind<'a>)> {
    let variadic = simple_type(lexer)?;
    let end = lexer.next().ok()??.kind;
    matches!(end, Kind::Byte(b',' | b')')).then_some((variadic, end))
}

fn simple_type(lexer: &mut Lexer<'_>) -> Option<bool> {
    let mut kind = lexer.next().ok()??.kind;
    let variadic = kind == Kind::Ellipsis;
    if variadic {
        kind = lexer.next().ok()??.kind;
    }
    loop {
        match kind {
            Kind::Byte(b'*') => {}
            Kind::Byte(b'[') if lexer.next().ok()??.kind == Kind::Byte(b']') => {}
            _ => break,
        }
        kind = lexer.next().ok()??.kind;
    }
    identifier(kind)?;
    if lexer.peek().ok()??.kind == Kind::Byte(b'.') {
        lexer.next().ok()?;
        identifier(lexer.next().ok()??.kind)?;
    }
    Some(variadic)
}

fn identifier(kind: Kind<'_>) -> Option<()> {
    let Kind::Word(name) = kind else { return None };
    (!matches!(
        name,
        "break"
            | "case"
            | "chan"
            | "const"
            | "continue"
            | "default"
            | "defer"
            | "else"
            | "fallthrough"
            | "for"
            | "func"
            | "go"
            | "goto"
            | "if"
            | "import"
            | "interface"
            | "map"
            | "package"
            | "range"
            | "return"
            | "select"
            | "struct"
            | "switch"
            | "type"
            | "var"
    ))
    .then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(spellings: &InterfaceSpellings, name: &str, shape: MethodShape) -> bool {
        spellings.matches(|candidate, fingerprint| {
            name == candidate && fingerprint.is_none_or(|candidate| shape == candidate)
        })
    }

    fn finds(source: &str, name: &str, shape: MethodShape) -> bool {
        matches(&interface_spellings(source), name, shape)
    }

    #[test]
    fn probes_interface_syntax_without_treating_comments_and_literals_as_code() {
        let source = "package p\n \
            // type I interface { Match() }\n \
            var _ = `interface { Match() }`\n \
            type I[T any] interface /* { Match() } */ {\n \
                Outer(interface { Nested() })\n \
                Match /* comment */ (\n a, b *p.T, args ...[]T,\n) (x, y error)\n \
            }";
        let spellings = interface_spellings(source);
        assert!(matches(&spellings, "Match", (3, 2, true)));
        assert!(!matches(&spellings, "Match", (0, 0, false)));
        assert!(matches(&spellings, "Nested", (0, 0, false)));
        assert!(!matches(&spellings, "Missing", (0, 0, false)));
    }

    #[test]
    fn checks_simple_result_arity_and_keeps_uncertain_syntax() {
        assert!(!finds(
            "type I interface { Close() error }",
            "Close",
            (0, 0, false)
        ));
        assert!(finds(
            "type I interface { Close() error }",
            "Close",
            (0, 1, false)
        ));
        assert!(finds(
            "type I interface { Close(); Other() }",
            "Close",
            (0, 0, false)
        ));
        assert!(finds(
            "type I interface { Use(func(int, string)) }",
            "Use",
            (0, 0, false)
        ));
        assert!(finds("type I interface { Close( ", "Close", (0, 0, false)));
        assert!(finds("type I interface { 转换() }", "转换", (0, 0, false)));
    }
}
