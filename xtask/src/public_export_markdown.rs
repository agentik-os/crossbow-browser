//! Links in Markdown files, as far as the export needs them: inline links and
//! images outside code, reference definitions, `href`/`src` attributes, and
//! the anchors headings make by GitHub's rule.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// An inline link or image: `[text](target)` or `![alt](target)`.
#[derive(Debug)]
pub struct Link {
    /// From `[` (or `!`) through `)`.
    pub span: Range<usize>,
    /// Between the brackets.
    pub text: Range<usize>,
    pub target: String,
    pub image: bool,
    pub line: usize,
}

/// A target found outside an inline link: a reference definition
/// (`[label]: target`) or an HTML `href`/`src` attribute. These can't be
/// reduced to text, so they must resolve.
#[derive(Debug)]
pub struct Reference {
    pub target: String,
    pub line: usize,
}

pub fn links(src: &str) -> Vec<Link> {
    let s = src.as_bytes();
    let (code, _) = code_mask(s);
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < s.len() {
        if s[i] == b']' && s[i + 1] == b'(' && !code[i] && !escaped(s, i) {
            if let (Some(open), Some((target, end))) = (opening_bracket(s, &code, i), destination(s, i + 2)) {
                let image = open > 0 && s[open - 1] == b'!' && !escaped(s, open - 1);
                let start = if image { open - 1 } else { open };
                found.push(Link { span: start..end, text: open + 1..i, target, image, line: line_of(s, start) });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    found
}

pub fn references(src: &str) -> Vec<Reference> {
    let s = src.as_bytes();
    let (code, fence) = code_mask(s);
    let mut found = Vec::new();
    for (start, line) in lines(s) {
        if fence[start] {
            continue;
        }
        let indent = line.iter().take_while(|&&c| c == b' ').count();
        let body = &line[indent..];
        if indent <= 3 && body.first() == Some(&b'[') && body.get(1) != Some(&b'^') {
            if let Some(close) = body.windows(2).position(|w| w == b"]:") {
                let rest = String::from_utf8_lossy(&body[close + 2..]);
                let target = rest.split_whitespace().next().unwrap_or("").trim_start_matches('<').trim_end_matches('>');
                if close > 1 && !target.is_empty() {
                    found.push(Reference { target: target.to_owned(), line: line_of(s, start) });
                }
            }
        }
    }
    let lower = s.to_ascii_lowercase();
    for attribute in [&b"href="[..], b"src="] {
        let mut from = 0;
        while let Some(at) = lower[from..].windows(attribute.len()).position(|w| w == attribute).map(|p| from + p) {
            from = at + attribute.len();
            let quoted = at > 0 && lower[at - 1].is_ascii_whitespace() && matches!(s.get(from), Some(b'"' | b'\''));
            if !quoted || code[at] {
                continue;
            }
            let quote = s[from];
            if let Some(len) = s[from + 1..].iter().position(|&c| c == quote || c == b'\n') {
                if s[from + 1 + len] == quote {
                    let target = String::from_utf8_lossy(&s[from + 1..from + 1 + len]).into_owned();
                    found.push(Reference { target, line: line_of(s, at) });
                }
            }
        }
    }
    found.sort_by_key(|r| r.line);
    found
}

/// The fragments that reach somewhere in this file: heading slugs (with
/// GitHub's `-1`, `-2` for repeats) and HTML `id`/`name` attributes.
pub fn anchors(src: &str) -> HashSet<String> {
    let s = src.as_bytes();
    let (code, fence) = code_mask(s);
    let mut found = HashSet::new();
    let mut repeats: HashMap<String, usize> = HashMap::new();
    for (start, line) in lines(s) {
        if fence[start] {
            continue;
        }
        if let Some(text) = heading(line) {
            let base = slug(&text);
            let n = repeats.entry(base.clone()).or_insert(0);
            found.insert(if *n == 0 { base.clone() } else { format!("{base}-{n}") });
            *n += 1;
        }
    }
    let lower = s.to_ascii_lowercase();
    for attribute in [&b" id=\""[..], b" name=\""] {
        let mut from = 0;
        while let Some(at) = lower[from..].windows(attribute.len()).position(|w| w == attribute).map(|p| from + p) {
            from = at + attribute.len();
            if code[at] {
                continue;
            }
            if let Some(len) = s[from..].iter().position(|&c| c == b'"' || c == b'\n') {
                found.insert(String::from_utf8_lossy(&lower[from..from + len]).into_owned());
            }
        }
    }
    found
}

/// GitHub's heading anchor: lowercase, keep letters, digits, `_`, `-` and
/// spaces, then spaces become `-`. Links count as their text, and HTML
/// tags are dropped.
pub fn slug(heading: &str) -> String {
    let mut text = String::new();
    let mut at = 0;
    for link in links(heading) {
        text.push_str(&heading[at..link.span.start]);
        text.push_str(&heading[link.text.clone()]);
        at = link.span.end;
    }
    text.push_str(&heading[at..]);
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// An ATX heading's text, without its closing `#`s.
fn heading(line: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\n', '\r']);
    let indent = line.len() - line.trim_start_matches(' ').len();
    let body = &line[indent..];
    let level = body.len() - body.trim_start_matches('#').len();
    if indent > 3 || !(1..=6).contains(&level) {
        return None;
    }
    let rest = &body[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut text = rest.trim();
    let closing = text.trim_end_matches('#');
    if closing.is_empty() || closing.ends_with([' ', '\t']) {
        text = closing.trim_end();
    }
    Some(text.to_owned())
}

/// Each line's start offset and bytes, line break included.
fn lines(s: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= s.len() {
            return None;
        }
        let end = s[start..].iter().position(|&c| c == b'\n').map_or(s.len(), |p| start + p + 1);
        let line = (start, &s[start..end]);
        start = end;
        Some(line)
    })
}

/// Which bytes are code (fenced blocks and inline spans), and which are in
/// fenced blocks.
fn code_mask(s: &[u8]) -> (Vec<bool>, Vec<bool>) {
    let mut code = vec![false; s.len()];
    let mut fence = vec![false; s.len()];
    let mut open: Option<(u8, usize)> = None;
    for (start, line) in lines(s) {
        let end = start + line.len();
        let indent = line.iter().take_while(|&&c| c == b' ').count();
        let body = &line[indent..];
        let run = |c: u8| body.iter().take_while(|&&b| b == c).count();
        match open {
            Some((c, n)) => {
                code[start..end].fill(true);
                fence[start..end].fill(true);
                if indent <= 3 && run(c) >= n && body[run(c)..].iter().all(u8::is_ascii_whitespace) {
                    open = None;
                }
            }
            None if indent <= 3 && (run(b'`') >= 3 || run(b'~') >= 3) => {
                open = Some((body[0], run(body[0])));
                code[start..end].fill(true);
                fence[start..end].fill(true);
            }
            None => inline_code(s, start, end, &mut code),
        }
    }
    (code, fence)
}

/// Backtick spans within one line: a run of n backticks up to the next run
/// of exactly n.
fn inline_code(s: &[u8], start: usize, end: usize, code: &mut [bool]) {
    let run_at = |i: usize| s[i..end].iter().take_while(|&&c| c == b'`').count();
    let mut i = start;
    while i < end {
        if s[i] != b'`' || escaped(s, i) {
            i += 1;
            continue;
        }
        let n = run_at(i);
        let mut j = i + n;
        let mut close = None;
        while j < end {
            if s[j] == b'`' {
                let m = run_at(j);
                if m == n {
                    close = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(c) => {
                code[i..c].fill(true);
                i = c;
            }
            None => i += n,
        }
    }
}

fn escaped(s: &[u8], i: usize) -> bool {
    s[..i].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 1
}

fn line_of(s: &[u8], at: usize) -> usize {
    1 + s[..at].iter().filter(|&&c| c == b'\n').count()
}

/// The `[` that opens the text ending at `close`, within the paragraph.
fn opening_bracket(s: &[u8], code: &[bool], close: usize) -> Option<usize> {
    let mut depth = 0;
    let mut blank = false;
    let mut j = close;
    while j > 0 {
        j -= 1;
        let c = s[j];
        if c == b'\n' {
            if blank {
                return None;
            }
            blank = true;
            continue;
        }
        if !c.is_ascii_whitespace() {
            blank = false;
        }
        if code[j] || escaped(s, j) {
            continue;
        }
        match c {
            b']' => depth += 1,
            b'[' if depth == 0 => return Some(j),
            b'[' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The target after `](`, an optional title, and the end past `)`.
fn destination(s: &[u8], mut p: usize) -> Option<(String, usize)> {
    let skip_space = |p: &mut usize| {
        while s.get(*p).is_some_and(u8::is_ascii_whitespace) {
            *p += 1;
        }
    };
    skip_space(&mut p);
    let target = if s.get(p) == Some(&b'<') {
        let len = s[p + 1..].iter().position(|&c| c == b'>' || c == b'\n')?;
        if s[p + 1 + len] != b'>' {
            return None;
        }
        let target = &s[p + 1..p + 1 + len];
        p += len + 2;
        target
    } else {
        let start = p;
        let mut depth = 0;
        while let Some(&c) = s.get(p) {
            if c == b'\\' && p + 1 < s.len() {
                p += 2;
                continue;
            }
            if c.is_ascii_whitespace() || c.is_ascii_control() {
                break;
            }
            if c == b'(' {
                depth += 1;
            } else if c == b')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            p += 1;
        }
        &s[start..p]
    };
    skip_space(&mut p);
    if let Some(&open) = s.get(p).filter(|&&c| matches!(c, b'"' | b'\'' | b'(')) {
        let close = if open == b'(' { b')' } else { open };
        let len = s[p + 1..].iter().position(|&c| c == close)?;
        p += len + 2;
        skip_space(&mut p);
    }
    (s.get(p) == Some(&b')')).then(|| (String::from_utf8_lossy(target).into_owned(), p + 1))
}
