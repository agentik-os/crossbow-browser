//! The small languages `public-export.toml` is written in: a TOML subset,
//! path globs and deny regexes. xtask takes no new crates, so each is only
//! as large as the config needs, and anything outside it is an error.

use std::cell::Cell;

// -- TOML subset ------------------------------------------------------------------

/// A value: a string (basic or literal), an integer, a boolean, or an array
/// of strings.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<String>),
}

/// One `[table]` or `[[array-table]]` entry, with its keys in file order.
#[derive(Debug)]
pub struct Table {
    pub name: String,
    pub array: bool,
    pub line: usize,
    pub keys: Vec<(String, Value, usize)>,
}

/// Parses comments, `[table]`, `[[array]]` (names may be dotted, as in
/// `[env.prod.vars]`) and `key = value` lines with bare keys. Dotted keys,
/// inline tables, floats, dates and multi-line strings are errors.
pub fn parse_toml(text: &str) -> Result<Vec<Table>, String> {
    let mut p = Toml { s: text.as_bytes(), i: 0, line: 1 };
    let mut tables: Vec<Table> = Vec::new();
    loop {
        p.blank_lines();
        let Some(c) = p.peek() else { break };
        let line = p.line;
        if c == b'[' {
            let array = p.s.get(p.i + 1) == Some(&b'[');
            p.i += if array { 2 } else { 1 };
            let mut name = p.bare_key()?;
            while p.peek() == Some(b'.') {
                p.i += 1;
                name.push('.');
                name += &p.bare_key()?;
            }
            let close: &[u8] = if array { b"]]" } else { b"]" };
            if !p.s[p.i..].starts_with(close) {
                return Err(p.error("expected `]` after the table name"));
            }
            p.i += close.len();
            p.end_of_line()?;
            if !array && tables.iter().any(|t| t.name == name) {
                return Err(format!("line {line}: table [{name}] appears twice"));
            }
            if tables.iter().any(|t| t.name == name && t.array != array) {
                return Err(format!("line {line}: [{name}] is both a table and an array of tables"));
            }
            tables.push(Table { name, array, line, keys: Vec::new() });
        } else {
            let key = p.bare_key()?;
            p.spaces();
            if p.peek() != Some(b'=') {
                return Err(p.error("expected `=` after the key"));
            }
            p.i += 1;
            p.spaces();
            let value = p.value()?;
            p.end_of_line()?;
            let Some(table) = tables.last_mut() else {
                return Err(format!("line {line}: `{key}` is outside any table"));
            };
            if table.keys.iter().any(|(k, _, _)| *k == key) {
                return Err(format!("line {line}: `{key}` appears twice in [{}]", table.name));
            }
            table.keys.push((key, value, line));
        }
    }
    Ok(tables)
}

struct Toml<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl Toml<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn error(&self, what: &str) -> String {
        format!("line {}: {what}", self.line)
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    fn comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }

    /// Spaces, comments, and line breaks.
    fn blank_lines(&mut self) {
        loop {
            self.spaces();
            self.comment();
            match self.peek() {
                Some(b'\n') => {
                    self.i += 1;
                    self.line += 1;
                }
                Some(b'\r') if self.s.get(self.i + 1) == Some(&b'\n') => self.i += 1,
                _ => return,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.spaces();
        self.comment();
        if self.peek() == Some(b'\r') {
            self.i += 1;
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.i += 1;
                self.line += 1;
                Ok(())
            }
            Some(_) => Err(self.error("unexpected text after the value")),
        }
    }

    fn bare_key(&mut self) -> Result<String, String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
            self.i += 1;
        }
        if start == self.i {
            return Err(self.error("expected a bare key"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"' | b'\'') => Ok(Value::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.blank_lines();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        return Ok(Value::List(items));
                    }
                    if !matches!(self.peek(), Some(b'"' | b'\'')) {
                        return Err(self.error("arrays may hold only strings"));
                    }
                    items.push(self.string()?);
                    self.blank_lines();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return Err(self.error("expected `,` or `]` in the array")),
                    }
                }
            }
            _ => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'+') {
                    self.i += 1;
                }
                let word = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
                match word {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => word.replace('_', "").parse().map(Value::Int).map_err(|_| self.error("expected a string, integer, boolean or array")),
                }
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.error("unterminated string")) };
            self.i += 1;
            match c {
                b'\n' => return Err(self.error("strings may not span lines")),
                _ if c == quote => break,
                b'\\' if quote == b'"' => {
                    let Some(e) = self.peek() else { return Err(self.error("unterminated string")) };
                    self.i += 1;
                    match e {
                        b'"' | b'\\' => out.push(e),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'u' | b'U' => {
                            let len = if e == b'u' { 4 } else { 8 };
                            let hex = self.s.get(self.i..self.i + len).and_then(|h| std::str::from_utf8(h).ok());
                            let ch = hex.and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
                            let Some(ch) = ch else { return Err(self.error("bad unicode escape")) };
                            self.i += len;
                            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        _ => return Err(self.error("unknown escape in a basic string; use a 'literal string'")),
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("string is not UTF-8"))
    }
}

// -- path globs -------------------------------------------------------------------

/// A path pattern. `**` as a whole segment spans any number of folders; `*`
/// and `?` stay inside one segment. A pattern without `/` matches a file or
/// folder name at any depth; one with `/` is anchored at the repository
/// root. A pattern that matches a folder matches everything inside it.
#[derive(Debug, Clone)]
pub struct Glob {
    pub text: String,
    segments: Vec<String>,
    anywhere: bool,
}

impl Glob {
    pub fn new(text: &str) -> Result<Glob, String> {
        let anchored = text.starts_with('/');
        let body = text.trim_start_matches('/').trim_end_matches('/');
        let segments: Vec<String> = body.split('/').map(str::to_owned).collect();
        if body.is_empty() || segments.iter().any(|s| s.is_empty() || s == "." || s == "..") {
            return Err(format!("bad path pattern {text:?}"));
        }
        if segments.iter().any(|s| s.contains("**") && s != "**") {
            return Err(format!("bad path pattern {text:?}: `**` must be a whole segment"));
        }
        Ok(Glob { text: text.to_owned(), anywhere: !anchored && segments.len() == 1, segments })
    }

    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        if self.anywhere {
            return parts.iter().any(|part| segment_matches(self.segments[0].as_bytes(), part.as_bytes()));
        }
        (1..=parts.len()).any(|n| segments_match(&self.segments, &parts[..n]))
    }
}

fn segments_match(pattern: &[String], parts: &[&str]) -> bool {
    match pattern.split_first() {
        None => parts.is_empty(),
        Some((first, rest)) if first == "**" => (0..=parts.len()).any(|skip| segments_match(rest, &parts[skip..])),
        Some((first, rest)) => !parts.is_empty() && segment_matches(first.as_bytes(), parts[0].as_bytes()) && segments_match(rest, &parts[1..]),
    }
}

/// `*` and `?` within one name, by the usual backtracking over the last `*`.
fn segment_matches(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

// -- deny regexes -----------------------------------------------------------------

/// A case-insensitive regex over bytes: literals, `.`, classes with ranges
/// and negation, `\d \w \s` (and their capitals), escaped punctuation,
/// groups, `|`, `? * + {m,n}`, and `^ $` at line starts and ends. Matching
/// runs on ASCII-lowercased text.
#[derive(Debug)]
pub struct Regex {
    alternatives: Vec<Vec<Node>>,
    first: Option<Box<[bool; 256]>>,
}

#[derive(Debug)]
enum Node {
    Byte(u8),
    Set(Box<[bool; 256]>),
    Any,
    LineStart,
    LineEnd,
    Group(Vec<Vec<Node>>),
    Repeat(Box<Node>, u32, u32),
}

const UNBOUNDED: u32 = u32::MAX;
/// Backtracking steps one search may take before it gives up. A search that
/// gives up is an error, so a pathological pattern fails closed.
const BUDGET: u64 = 50_000_000;

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let mut parser = RegexParser { s: pattern.as_bytes(), i: 0 };
        let alternatives = parser.alternatives().map_err(|e| format!("regex {pattern:?}: {e}"))?;
        if parser.i != parser.s.len() {
            return Err(format!("regex {pattern:?}: unmatched `)`"));
        }
        let first = first_bytes_of_alternatives(&alternatives).map(Box::new);
        Ok(Regex { alternatives, first })
    }

    /// Where a match starts at or after `from` in already-lowercased text.
    /// `Err` means the search gave up (see `BUDGET`).
    pub fn find_from(&self, text: &[u8], from: usize) -> Result<Option<usize>, ()> {
        let run = Run { text, steps: Cell::new(0) };
        for at in from..=text.len() {
            if let Some(first) = &self.first {
                if at == text.len() || !first[text[at] as usize] {
                    continue;
                }
            }
            for alternative in &self.alternatives {
                if run.seq(alternative, at, &mut |_| true) {
                    return if run.steps.get() > BUDGET { Err(()) } else { Ok(Some(at)) };
                }
            }
        }
        Ok(None)
    }
}

struct RegexParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl RegexParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut alternatives = vec![self.sequence()?];
        while self.peek() == Some(b'|') {
            self.i += 1;
            alternatives.push(self.sequence()?);
        }
        Ok(alternatives)
    }

    fn sequence(&mut self) -> Result<Vec<Node>, String> {
        let mut nodes = Vec::new();
        while let Some(c) = self.peek() {
            if c == b'|' || c == b')' {
                break;
            }
            self.i += 1;
            let atom = match c {
                b'(' => {
                    if self.s[self.i..].starts_with(b"?:") {
                        self.i += 2;
                    } else if self.peek() == Some(b'?') {
                        return Err("only `(?:` groups are supported".into());
                    }
                    let inner = self.alternatives()?;
                    if self.peek() != Some(b')') {
                        return Err("unclosed `(`".into());
                    }
                    self.i += 1;
                    Node::Group(inner)
                }
                b'[' => self.class()?,
                b'.' => Node::Any,
                b'^' => Node::LineStart,
                b'$' => Node::LineEnd,
                b'\\' => self.escape()?,
                b'*' | b'+' | b'?' | b'{' => return Err(format!("`{}` has nothing to repeat", c as char)),
                _ => Node::Byte(c.to_ascii_lowercase()),
            };
            let atom = self.quantified(atom)?;
            nodes.push(atom);
        }
        Ok(nodes)
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some(b'*') => (0, UNBOUNDED),
            Some(b'+') => (1, UNBOUNDED),
            Some(b'?') => (0, 1),
            Some(b'{') => {
                let close = self.s[self.i..].iter().position(|&c| c == b'}').ok_or("unclosed `{`")?;
                let body = std::str::from_utf8(&self.s[self.i + 1..self.i + close]).map_err(|_| "bad `{}`")?;
                let number = |s: &str| s.parse::<u32>().map_err(|_| format!("bad repeat {{{body}}}"));
                let (min, max) = match body.split_once(',') {
                    None => (number(body)?, number(body)?),
                    Some((min, "")) => (number(min)?, UNBOUNDED),
                    Some((min, max)) => (number(min)?, number(max)?),
                };
                if min > max || (max != UNBOUNDED && max > 1000) {
                    return Err(format!("bad repeat {{{body}}}"));
                }
                self.i += close;
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        // A lazy `?` changes which match is found, never whether one exists.
        if self.peek() == Some(b'?') {
            self.i += 1;
        }
        if matches!(atom, Node::LineStart | Node::LineEnd) {
            return Err("an anchor can't repeat".into());
        }
        Ok(Node::Repeat(Box::new(atom), min, max))
    }

    fn escape(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("trailing `\\`")?;
        self.i += 1;
        if let Some(set) = escape_set(c) {
            return Ok(Node::Set(Box::new(set)));
        }
        match c {
            b'n' => Ok(Node::Byte(b'\n')),
            b't' => Ok(Node::Byte(b'\t')),
            b'r' => Ok(Node::Byte(b'\r')),
            _ if c.is_ascii_alphanumeric() => Err(format!("unsupported escape `\\{}`", c as char)),
            _ => Ok(Node::Byte(c.to_ascii_lowercase())),
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.peek() == Some(b'^');
        if negated {
            self.i += 1;
        }
        let mut set = [false; 256];
        let mut first = true;
        loop {
            let c = self.peek().ok_or("unclosed `[`")?;
            self.i += 1;
            if c == b']' && !first {
                break;
            }
            first = false;
            let low = if c == b'\\' {
                let e = self.peek().ok_or("unclosed `[`")?;
                self.i += 1;
                if let Some(escaped) = escape_set(e) {
                    for (slot, on) in set.iter_mut().zip(escaped) {
                        *slot |= on;
                    }
                    continue;
                }
                match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    _ if e.is_ascii_alphanumeric() => return Err(format!("unsupported escape `\\{}`", e as char)),
                    _ => e,
                }
            } else {
                c
            };
            if self.peek() == Some(b'-') && self.s.get(self.i + 1).is_some_and(|&n| n != b']') {
                let high = self.s[self.i + 1];
                if high == b'\\' || high < low {
                    return Err("bad range in `[...]`".into());
                }
                self.i += 2;
                for b in low..=high {
                    set[b as usize] = true;
                }
            } else {
                set[low as usize] = true;
            }
        }
        fold_case(&mut set);
        if negated {
            for slot in set.iter_mut() {
                *slot = !*slot;
            }
        }
        Ok(Node::Set(Box::new(set)))
    }
}

/// `\d \w \s` and their negations, with both letter cases set.
fn escape_set(c: u8) -> Option<[bool; 256]> {
    let test: fn(u8) -> bool = match c.to_ascii_lowercase() {
        b'd' => |b| b.is_ascii_digit(),
        b'w' => |b| b.is_ascii_alphanumeric() || b == b'_',
        b's' => |b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
        _ => return None,
    };
    let negated = c.is_ascii_uppercase();
    let mut set = [false; 256];
    for (b, slot) in set.iter_mut().enumerate() {
        *slot = test(b as u8) != negated;
    }
    Some(set)
}

fn fold_case(set: &mut [bool; 256]) {
    for upper in b'A'..=b'Z' {
        let lower = upper.to_ascii_lowercase();
        let on = set[upper as usize] || set[lower as usize];
        set[upper as usize] = on;
        set[lower as usize] = on;
    }
}

/// The bytes a match can start with, when they can be known; `None` means any
/// position must be tried.
fn first_bytes_of_alternatives(alternatives: &[Vec<Node>]) -> Option<[bool; 256]> {
    let mut all = [false; 256];
    for alternative in alternatives {
        let set = first_bytes(alternative)?;
        for (slot, on) in all.iter_mut().zip(set) {
            *slot |= on;
        }
    }
    Some(all)
}

fn first_bytes(nodes: &[Node]) -> Option<[bool; 256]> {
    for node in nodes {
        let mut set = [false; 256];
        match node {
            Node::LineStart => continue,
            Node::Byte(b) => set[*b as usize] = true,
            Node::Set(s) => set = **s,
            Node::Group(alternatives) => set = first_bytes_of_alternatives(alternatives)?,
            Node::Repeat(inner, min, _) if *min > 0 => set = first_bytes(std::slice::from_ref(&**inner))?,
            Node::Any | Node::LineEnd | Node::Repeat(..) => return None,
        }
        return Some(set);
    }
    None
}

struct Run<'a> {
    text: &'a [u8],
    steps: Cell<u64>,
}

impl Run<'_> {
    /// Whether `nodes` match at `at` with `rest` (the continuation) accepting
    /// where they end.
    fn seq(&self, nodes: &[Node], at: usize, rest: &mut dyn FnMut(usize) -> bool) -> bool {
        self.steps.set(self.steps.get() + 1);
        if self.steps.get() > BUDGET {
            return true;
        }
        let Some((node, tail)) = nodes.split_first() else { return rest(at) };
        match node {
            Node::Group(alternatives) => {
                for alternative in alternatives {
                    if self.seq(alternative, at, &mut |end| self.seq(tail, end, &mut *rest)) {
                        return true;
                    }
                }
                false
            }
            Node::Repeat(inner, min, max) if matches!(**inner, Node::Byte(_) | Node::Set(_) | Node::Any) => {
                let (min, max) = (*min as usize, *max as usize);
                let mut n = 0;
                while n < max && at + n < self.text.len() && self.one(inner, at + n).is_some() {
                    n += 1;
                }
                if n < min {
                    return false;
                }
                loop {
                    if self.seq(tail, at + n, rest) {
                        return true;
                    }
                    if n == min {
                        return false;
                    }
                    n -= 1;
                }
            }
            Node::Repeat(inner, min, max) => self.repeat(inner, *min, *max, 0, at, tail, rest),
            _ => match self.one(node, at) {
                Some(end) => self.seq(tail, end, rest),
                None => false,
            },
        }
    }

    /// Greedy repetition of a group: one more round first, then the rest.
    #[allow(clippy::too_many_arguments)]
    fn repeat(&self, inner: &Node, min: u32, max: u32, done: u32, at: usize, tail: &[Node], rest: &mut dyn FnMut(usize) -> bool) -> bool {
        if done < max
            && self.seq(std::slice::from_ref(inner), at, &mut |end| {
                (end != at || done < min) && self.repeat(inner, min, max, done + 1, end, tail, &mut *rest)
            })
        {
            return true;
        }
        done >= min && self.seq(tail, at, rest)
    }

    /// One byte or one anchor at `at`: where it ends, if it matches.
    fn one(&self, node: &Node, at: usize) -> Option<usize> {
        let byte = self.text.get(at).copied();
        let ok = match node {
            Node::Byte(b) => byte == Some(*b),
            Node::Set(set) => byte.is_some_and(|b| set[b as usize]),
            Node::Any => byte.is_some_and(|b| b != b'\n'),
            Node::LineStart => return (at == 0 || self.text[at - 1] == b'\n').then_some(at),
            Node::LineEnd => return matches!(byte, None | Some(b'\n' | b'\r')).then_some(at),
            Node::Group(_) | Node::Repeat(..) => unreachable!("handled in seq"),
        };
        ok.then_some(at + 1)
    }
}
