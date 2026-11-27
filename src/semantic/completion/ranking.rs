use std::{cmp::Ordering, collections::HashSet};

use lsp_types::CompletionItemKind;

use super::render::{PendingImport, Target};

pub(super) const FIELD: i32 = 45;
pub(super) const SYMBOL: i32 = 35;
pub(super) const KEYWORD: i32 = 25;
pub(super) const AUTO_IMPORT: i32 = 5;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Match {
    Exact,
    Prefix,
    FoldedPrefix,
    CamelHump,
    Substring,
}

pub(super) fn match_name(prefix: &str, name: &str) -> Option<Match> {
    match_start(prefix, name).or_else(|| {
        if prefix.chars().nth(1).is_none() {
            return None;
        }
        let contains = if prefix.is_ascii() && name.is_ascii() {
            name.as_bytes()
                .windows(prefix.len())
                .any(|substring| substring.eq_ignore_ascii_case(prefix.as_bytes()))
        } else {
            name.to_lowercase().contains(&prefix.to_lowercase())
        };
        contains.then_some(Match::Substring)
    })
}

pub(super) fn match_start(prefix: &str, name: &str) -> Option<Match> {
    if name == prefix {
        return Some(Match::Exact);
    }
    if name.starts_with(prefix) {
        return Some(Match::Prefix);
    }
    if prefix.is_ascii() && name.is_ascii() {
        if name
            .as_bytes()
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
        {
            return Some(Match::FoldedPrefix);
        }
        return camel_hump(prefix, name, true).then_some(Match::CamelHump);
    }
    let folded = prefix.to_lowercase();
    if name.to_lowercase().starts_with(&folded) {
        return Some(Match::FoldedPrefix);
    }
    camel_hump(prefix, name, true).then_some(Match::CamelHump)
}

pub(super) fn match_member_hump(prefix: &str, name: &str) -> Option<Match> {
    match_start(prefix, name)
        .or_else(|| camel_hump(prefix, name, false).then_some(Match::CamelHump))
}

fn camel_hump(prefix: &str, name: &str, anchored: bool) -> bool {
    let mut name = name.chars().peekable();
    if anchored
        && !prefix
            .chars()
            .next()
            .zip(name.peek())
            .is_some_and(|(left, right)| equal_fold(left, *right))
    {
        return false;
    }
    let pattern: Vec<_> = prefix.chars().collect();
    let case_humps = pattern.iter().any(|ch| ch.is_lowercase());
    // Keep viable fragments until the next character. A repeated hump can
    // be skipped if matching it first would prevent the rest from matching.
    let mut adjacent = vec![false; pattern.len() + 1];
    adjacent[0] = true;
    let mut longest = 0;
    let mut previous = None::<char>;
    while let Some(ch) = name.next() {
        let boundary = previous.is_none_or(|last| {
            !last.is_alphanumeric()
                || ch.is_numeric() != last.is_numeric()
                || ch.is_uppercase()
                    && (!last.is_uppercase() || name.peek().is_some_and(|next| next.is_lowercase()))
        });
        let restart = !anchored || previous.is_none();
        let bridge = boundary || !ch.is_alphanumeric();
        let mut advanced = longest;
        for index in (0..=longest).rev() {
            let expected = pattern[index];
            let case_matches =
                index == 0 || !case_humps || !expected.is_uppercase() || ch.is_uppercase();
            adjacent[index + 1] = case_matches
                && equal_fold(expected, ch)
                && (adjacent[index] || bridge && (index > 0 || restart));
            if adjacent[index + 1] {
                advanced = advanced.max(index + 1);
            }
        }
        longest = advanced;
        if longest == pattern.len() {
            return true;
        }
        adjacent[0] = false;
        previous = Some(ch);
    }
    false
}

fn equal_fold(left: char, right: char) -> bool {
    if left.is_ascii() && right.is_ascii() {
        left.eq_ignore_ascii_case(&right)
    } else {
        left.to_lowercase().eq(right.to_lowercase())
    }
}

pub(super) fn package_priority(caller: &str, path: &str) -> i32 {
    let common = caller
        .split('/')
        .zip(path.split('/'))
        .take_while(|(left, right)| left == right)
        .count() as i32;
    39 + common - path.bytes().filter(|ch| matches!(ch, b'/' | b'.')).count() as i32
}

#[derive(Default)]
pub(super) struct Weight {
    pub(super) priority: i32,
    pub(super) same_package: bool,
    pub(super) usage: usize,
    pub(super) vgo: u8,
    // The scope API counts outward from the cursor.
    pub(super) depth: Option<usize>,
}

pub(super) enum Insert {
    Name,
    Call { has_parameters: bool },
    Braces,
    Package,
    Field { nesting: Vec<(String, String)> },
    ImportPath { close: Option<char> },
}

pub(super) struct Candidate {
    pub(super) label: String,
    pub(super) filter: Option<String>,
    pub(super) kind: CompletionItemKind,
    pub(super) matched: Match,
    pub(super) weight: Weight,
    pub(super) detail: Option<String>,
    pub(super) label_suffix: Option<String>,
    pub(super) target: Option<Target>,
    pub(super) deprecated: bool,
    pub(super) insert: Insert,
    pub(super) import_path: Option<String>,
    pub(super) qualified: bool,
    pub(super) qualifier: Option<String>,
    pub(super) replace_start: Option<usize>,
    pub(super) pending_import: Option<PendingImport>,
}

impl Candidate {
    pub(super) fn new(
        label: impl Into<String>,
        kind: CompletionItemKind,
        matched: Match,
        priority: i32,
    ) -> Self {
        Self {
            label: label.into(),
            filter: None,
            kind,
            matched,
            weight: Weight {
                priority,
                ..Weight::default()
            },
            detail: None,
            label_suffix: None,
            target: None,
            deprecated: false,
            insert: Insert::Name,
            import_path: None,
            qualified: false,
            qualifier: None,
            replace_start: None,
            pending_import: None,
        }
    }

    fn score(&self) -> (Match, u16, bool, u32, u8, u16) {
        (
            self.matched,
            (499 - self.weight.priority.clamp(-499, 499)) as u16,
            !self.weight.same_package,
            u32::MAX - u32::try_from(self.weight.usage).unwrap_or(u32::MAX),
            9 - self.weight.vgo,
            self.weight
                .depth
                .and_then(|depth| u16::try_from(depth).ok())
                .unwrap_or(u16::MAX),
        )
    }

    fn sort_text(&self, index: usize) -> String {
        let (matched, priority, other_package, usage, vgo, depth) = self.score();
        format!(
            "{}{priority:03}{}{usage:010}{vgo}{depth:05}{index:010}",
            matched as u8,
            u8::from(other_package)
        )
    }
}

pub(super) fn rank(mut candidates: Vec<Candidate>) -> impl Iterator<Item = (String, Candidate)> {
    candidates.sort_by(|left, right| {
        left.score()
            .cmp(&right.score())
            .then_with(|| compare_labels(&left.label, &right.label))
            .then_with(|| left.import_path.cmp(&right.import_path))
            .then_with(|| {
                left.target
                    .as_ref()
                    .map(Target::key)
                    .cmp(&right.target.as_ref().map(Target::key))
            })
            .then_with(|| left.detail.cmp(&right.detail))
    });
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(move |candidate| {
            let path = if candidate.qualified {
                candidate.import_path.clone()
            } else {
                None
            };
            seen.insert((candidate.label.clone(), path))
        })
        .enumerate()
        .map(|(index, candidate)| (candidate.sort_text(index), candidate))
}

fn compare_labels(left: &str, right: &str) -> Ordering {
    let mut a = left.chars().flat_map(char::to_lowercase).peekable();
    let mut b = right.chars().flat_map(char::to_lowercase).peekable();
    while let (Some(&head_a), Some(&head_b)) = (a.peek(), b.peek()) {
        let order = if head_a.is_ascii_digit() && head_b.is_ascii_digit() {
            while a.next_if_eq(&'0').is_some() {}
            while b.next_if_eq(&'0').is_some() {}
            let mut digits = Ordering::Equal;
            loop {
                match (
                    a.next_if(char::is_ascii_digit),
                    b.next_if(char::is_ascii_digit),
                ) {
                    (Some(left), Some(right)) => digits = digits.then(left.cmp(&right)),
                    (Some(_), None) => break Ordering::Greater,
                    (None, Some(_)) => break Ordering::Less,
                    (None, None) => break digits,
                }
            }
        } else {
            a.next();
            b.next();
            head_a.cmp(&head_b)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.peek().cmp(&b.peek()).then_with(|| left.cmp(right))
}
