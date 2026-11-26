use std::collections::BTreeSet;
use std::iter::Peekable;

use crate::semantic::resolver::is_exported;

/// Find declaration names without building a syntax tree for every SDK file.
/// The declaration index still verifies each match on the selected build files.
pub(super) fn exported_names(text: &str) -> Vec<String> {
    let mut tokens = Tokens {
        text: text.trim_start_matches('\u{feff}'),
        offset: 0,
    }
    .peekable();
    let mut names = BTreeSet::new();
    let mut depth = 0usize;
    let mut group = None;
    let mut spec_start = false;
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(kind @ ("func" | "type" | "var" | "const")) if depth == 0 => {
                let multiple = matches!(kind, "var" | "const");
                match next_nonnewline(&mut tokens) {
                    Some(Token::Punct(b'(')) => {
                        depth = 1;
                        // For func, this parenthesis starts a method receiver.
                        group = (kind != "func").then_some(multiple);
                        spec_start = true;
                    }
                    Some(Token::Word(name)) => {
                        record(&mut tokens, &mut names, name, multiple);
                    }
                    _ => {}
                }
            }
            Token::Word(name) if depth == 1 && spec_start && group.is_some() => {
                record(&mut tokens, &mut names, name, group == Some(true));
                spec_start = false;
            }
            Token::Punct(b'(' | b'[' | b'{') => depth += 1,
            Token::Punct(b')' | b']' | b'}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    group = None;
                }
            }
            Token::Newline | Token::Punct(b';') if depth == 1 => spec_start = true,
            _ => {}
        }
    }
    names.into_iter().map(str::to_owned).collect()
}

fn record<'a>(
    tokens: &mut Peekable<Tokens<'a>>,
    names: &mut BTreeSet<&'a str>,
    mut name: &'a str,
    multiple: bool,
) {
    loop {
        if is_exported(name) {
            names.insert(name);
        }
        if !multiple || !matches!(tokens.peek(), Some(Token::Punct(b','))) {
            break;
        }
        tokens.next();
        let Some(Token::Word(next)) = next_nonnewline(tokens) else {
            break;
        };
        name = next;
    }
}

fn next_nonnewline<'a>(tokens: &mut Peekable<Tokens<'a>>) -> Option<Token<'a>> {
    tokens.find(|token| !matches!(token, Token::Newline))
}

enum Token<'a> {
    Word(&'a str),
    Punct(u8),
    Newline,
    Literal,
}

struct Tokens<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let rest = self.text.get(self.offset..)?;
            let byte = *rest.as_bytes().first()?;
            let token = match byte {
                b' ' | b'\r' | b'\t' => {
                    self.offset += rest
                        .bytes()
                        .take_while(|byte| matches!(byte, b' ' | b'\r' | b'\t'))
                        .count();
                    continue;
                }
                b'\n' => {
                    self.offset += 1;
                    Token::Newline
                }
                b'/' if rest.starts_with("//") => {
                    self.offset += rest.find('\n').unwrap_or(rest.len());
                    continue;
                }
                b'/' if rest.starts_with("/*") => {
                    let end = rest[2..].find("*/").map_or(rest.len(), |end| end + 4);
                    self.offset += end;
                    if !rest[..end].contains('\n') {
                        continue;
                    }
                    Token::Newline
                }
                b'`' => {
                    self.offset += rest[1..].find('`').map_or(rest.len(), |end| end + 2);
                    Token::Literal
                }
                quote @ (b'"' | b'\'') => {
                    let bytes = rest.as_bytes();
                    let mut end = 1;
                    while let Some(byte) = bytes.get(end).copied() {
                        end += 1;
                        if byte == quote {
                            break;
                        }
                        if byte == b'\\' && end < bytes.len() {
                            end += 1;
                        }
                    }
                    self.offset += end;
                    Token::Literal
                }
                byte if word(byte) => {
                    let end = rest.bytes().take_while(|byte| word(*byte)).count();
                    self.offset += end;
                    Token::Word(&rest[..end])
                }
                _ => {
                    self.offset += 1;
                    Token::Punct(byte)
                }
            };
            return Some(token);
        }
    }
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii()
}
