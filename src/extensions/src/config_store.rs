//! Byte-exact config file mutation — a port of git's `config.c` store machinery.
//!
//! `git config` never re-serializes a config file. It parses the existing bytes into a
//! stream of events carrying `[begin, end)` offsets (`store_aux_event()`, config.c:2629),
//! notes which of them are the section header and entries the key names
//! (`store_aux()`, config.c:2668), and then copies the untouched byte ranges straight
//! through, splicing the new line in between
//! (`repo_config_set_multivar_in_file_gently()`, config.c:2999-3244). Comments,
//! indentation, `[section] key = value` on one line, the original spelling of section
//! headers — all of it survives because it is never looked at.
//!
//! This module reproduces that: the tokenizer (config.c:799-1140), the store bookkeeping
//! (config.c:2613-2900) and the splicing writer, plus
//! `repo_config_copy_or_rename_section_in_file()` (config.c:3353-3505) which works on
//! whole lines instead.

use std::path::{Path, PathBuf};

/// `enum config_event_t` (config.h:66-73).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EventType {
    Section,
    Entry,
    Whitespace,
    Comment,
    Eof,
}

/// One entry of `store->parsed`: the byte span an event covers, plus the
/// `is_keys_section` flag `store_aux_event()` stamps onto section headers.
#[derive(Clone, Copy)]
struct Parsed {
    begin: usize,
    end: usize,
    ty: EventType,
    is_keys_section: bool,
}

/// The `<value-pattern>` half of `repo_config_set_multivar_in_file_gently()`.
///
/// `NULL` matches every existing value, `CONFIG_REGEX_NONE` (config.h:37) matches none —
/// which is how `--add` appends without disturbing what is there — and the remaining two
/// are the user's pattern, compared either as a POSIX ERE or, under
/// `CONFIG_FLAGS_FIXED_VALUE`, with `strcmp()`.
pub enum ValuePattern<'a> {
    /// `value_pattern == NULL`: `matches()` always answers yes.
    Any,
    /// `value_pattern == CONFIG_REGEX_NONE`: `matches()` always answers no.
    Never,
    /// `CONFIG_FLAGS_FIXED_VALUE`: literal string equality.
    Fixed(&'a str),
    /// A POSIX ERE, with a leading `!` inverting the match (`store->do_not_match`).
    Regex(&'a str),
}

/// The compiled form of [`ValuePattern`], held by the store while it walks the file.
enum Matcher {
    Any,
    Never,
    Fixed(Vec<u8>),
    Regex { re: regex::bytes::Regex, do_not_match: bool },
}

/// The outcomes `repo_config_set_multivar_in_file_gently()` reports, named after the
/// `CONFIG_*` constants in config.h:27-35 that the builtin turns into exit codes.
#[derive(Debug)]
pub enum StoreError {
    /// `CONFIG_NOTHING_SET` (5): nothing to unset, or more than one match without
    /// `--replace-all`.
    NothingSet,
    /// `CONFIG_INVALID_FILE` (3): the existing file does not parse.
    InvalidFile,
    /// `CONFIG_INVALID_PATTERN` (6): `regcomp()` refused the value-pattern.
    InvalidPattern(String),
    /// `CONFIG_NO_LOCK`/`CONFIG_NO_WRITE`: the file could not be opened or replaced.
    Io(std::io::Error),
}

// ---------------------------------------------------------------------------
// Tokenizer — config.c:77-1140
// ---------------------------------------------------------------------------

/// `strerror(errno)` — the message without Rust's trailing ` (os error N)`.
fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    }
}

/// `isspace()` over the C locale: the six bytes `strspn`/`isspace` accept.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `iskeychar()` (config.c:56): alphanumeric or `-`.
fn is_key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// `struct config_source` over a memory buffer (`config_buf_fgetc()` and friends,
/// config.c:92-114).
struct Source<'a> {
    buf: &'a [u8],
    pos: usize,
    eof: bool,
    /// `cs->var`: the section stem, `[section "Sub"]` lower-cased up to the subsection.
    var: Vec<u8>,
    /// Set by `get_base_var()`/`get_extended_base_var()`; picks `strncasecmp` vs
    /// `strncmp` when the header is compared to the key's section.
    subsection_case_sensitive: bool,
}

impl<'a> Source<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0, eof: false, var: Vec::new(), subsection_case_sensitive: true }
    }

    /// `config_buf_fgetc()`.
    fn fgetc(&mut self) -> Option<u8> {
        let c = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(c)
    }

    /// `config_buf_ungetc()`.
    fn ungetc(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    /// `config_buf_ftell()`.
    fn ftell(&self) -> usize {
        self.pos
    }

    /// `get_next_char()` (config.c:799): CRLF folding, and EOF surfaces as a synthetic
    /// `'\n'` with `cs->eof` set.
    fn next_char(&mut self) -> u8 {
        let mut c = self.fgetc();
        if c == Some(b'\r') {
            c = self.fgetc();
            if c != Some(b'\n') {
                if c.is_some() {
                    self.ungetc();
                }
                c = Some(b'\r');
            }
        }
        match c {
            Some(c) => c,
            None => {
                self.eof = true;
                b'\n'
            }
        }
    }
}

/// `struct config_store_data` (config.c:2586-2607) plus the pieces of
/// `struct parse_event_data` (config.c:1000) the event flush needs.
struct Store {
    /// The key, canonicalized by `git_config_parse_key()`.
    key: Vec<u8>,
    /// Length of the `section[.subsection]` prefix of `key`.
    baselen: usize,
    matcher: Matcher,
    multi_replace: bool,
    parsed: Vec<Parsed>,
    parsed_nr: usize,
    seen: Vec<usize>,
    seen_nr: usize,
    /// `store->seen_alloc`: whether anything was ever written into `seen`.
    seen_touched: bool,
    section_seen: bool,
    is_keys_section: bool,
    key_seen: bool,
    previous_type: EventType,
    previous_offset: usize,
}

impl Store {
    /// `matches()` (config.c:2613).
    fn matches(&self, key: &[u8], value: Option<&[u8]>) -> bool {
        if key != self.key.as_slice() {
            return false;
        }
        match &self.matcher {
            Matcher::Fixed(want) => match value {
                Some(value) => want.as_slice() == value,
                // `if (store->fixed_value && value)` — a valueless key falls through to
                // the regex arms, and with a fixed value there is no pattern, so it
                // always matches.
                None => true,
            },
            Matcher::Any => true,
            Matcher::Never => false,
            Matcher::Regex { re, do_not_match } => {
                let hit = value.is_some_and(|v| re.is_match(v));
                hit != *do_not_match
            }
        }
    }

    /// `store_aux_event()` (config.c:2629) — record one event's span, and for a section
    /// header decide whether it is the key's section.
    fn event(&mut self, ty: EventType, begin: usize, end: usize, var: &[u8], case_sensitive: bool) {
        if self.parsed.len() <= self.parsed_nr {
            self.parsed.push(Parsed { begin: 0, end: 0, ty: EventType::Eof, is_keys_section: false });
        }
        self.parsed[self.parsed_nr] = Parsed { begin, end, ty, is_keys_section: false };

        if ty == EventType::Section {
            // `cs->var` is "section[.subsection]." here — the header has been parsed by
            // the time the event is flushed.
            let is_keys_section = var.len() >= 2
                && var.last() == Some(&b'.')
                && var.len() - 1 == self.baselen
                && match case_sensitive {
                    true => var[..self.baselen].eq_ignore_ascii_case(&self.key[..self.baselen]),
                    false => var[..self.baselen] == self.key[..self.baselen],
                };
            self.is_keys_section = is_keys_section;
            self.parsed[self.parsed_nr].is_keys_section = is_keys_section;
            if is_keys_section {
                self.section_seen = true;
                self.put_seen(self.parsed_nr);
            }
        }

        self.parsed_nr += 1;
    }

    /// `ALLOC_GROW(store->seen, store->seen_nr + 1, …); store->seen[store->seen_nr] = v;`
    /// — note that `seen_nr` is a count of *confirmed* matches and is not bumped here.
    fn put_seen(&mut self, v: usize) {
        if self.seen.len() <= self.seen_nr {
            self.seen.resize(self.seen_nr + 1, 0);
        }
        self.seen[self.seen_nr] = v;
        self.seen_touched = true;
    }

    /// `store_aux()` (config.c:2668) — the key/value callback.
    fn entry(&mut self, key: &[u8], value: Option<&[u8]>) {
        if self.key_seen {
            if self.matches(key, value) {
                if self.seen_nr == 1 && !self.multi_replace {
                    eprintln!("warning: {} has multiple values", String::from_utf8_lossy(&self.key));
                }
                self.put_seen(self.parsed_nr);
                self.seen_nr += 1;
            }
        } else if self.is_keys_section {
            // Not necessarily a match, but we are in the desired section, so remember
            // where a new line could be spliced in.
            self.put_seen(self.parsed_nr);
            self.section_seen = true;
            if self.matches(key, value) {
                self.seen_nr += 1;
                self.key_seen = true;
            }
        }
    }
}

/// `do_event()` (config.c:1006): flush the previous event with its end offset, then
/// remember this one's start.
fn do_event(cs: &Source<'_>, ty: EventType, store: &mut Store) {
    if ty == EventType::Whitespace && store.previous_type == EventType::Whitespace {
        return;
    }
    let mut offset = cs.ftell();
    // At EOF the parser has "inserted" an extra '\n', so the end offset is the current
    // position; otherwise we have already advanced past the triggering byte.
    if ty != EventType::Eof {
        offset -= 1;
    }
    if store.previous_type != EventType::Eof {
        let (pt, po) = (store.previous_type, store.previous_offset);
        store.event(pt, po, offset, &cs.var, cs.subsection_case_sensitive);
    }
    store.previous_type = ty;
    store.previous_offset = offset;
}

/// `parse_value()` (config.c:835). `None` is a parse error.
fn parse_value(cs: &mut Source<'_>) -> Option<Vec<u8>> {
    let mut quote = false;
    let mut comment = false;
    let mut trim_len = 0usize;
    let mut value: Vec<u8> = Vec::new();
    loop {
        let c = cs.next_char();
        if c == b'\n' {
            if quote {
                return None;
            }
            if trim_len != 0 {
                value.truncate(trim_len);
            }
            return Some(value);
        }
        if comment {
            continue;
        }
        if is_space(c) && !quote {
            if trim_len == 0 {
                trim_len = value.len();
            }
            if !value.is_empty() {
                value.push(c);
            }
            continue;
        }
        if !quote && (c == b';' || c == b'#') {
            comment = true;
            continue;
        }
        if trim_len != 0 {
            trim_len = 0;
        }
        if c == b'\\' {
            let c = cs.next_char();
            let decoded = match c {
                b'\n' => continue,
                b't' => b'\t',
                b'b' => 0x08,
                b'n' => b'\n',
                b'\\' | b'"' => c,
                // Reject unknown escape sequences.
                _ => return None,
            };
            value.push(decoded);
            continue;
        }
        if c == b'"' {
            quote = !quote;
            continue;
        }
        value.push(c);
    }
}

/// `get_value()` (config.c:901): finish the variable name, then the optional value, then
/// hand the pair to `store_aux()`.
fn get_value(cs: &mut Source<'_>, store: &mut Store, name: &mut Vec<u8>) -> Result<(), ()> {
    let mut c;
    loop {
        c = cs.next_char();
        if cs.eof || !is_key_char(c) {
            break;
        }
        name.push(c.to_ascii_lowercase());
    }
    while c == b' ' || c == b'\t' {
        c = cs.next_char();
    }
    let mut value = None;
    if c != b'\n' {
        if c != b'=' {
            return Err(());
        }
        value = Some(parse_value(cs).ok_or(())?);
    }
    store.entry(name, value.as_deref());
    Ok(())
}

/// `get_extended_base_var()` (config.c:943): the `"subsection"` half of the header,
/// preserved verbatim.
fn get_extended_base_var(cs: &mut Source<'_>, name: &mut Vec<u8>, mut c: u8) -> Result<(), ()> {
    cs.subsection_case_sensitive = false;
    loop {
        if c == b'\n' {
            return Err(());
        }
        c = cs.next_char();
        if !is_space(c) {
            break;
        }
    }
    if c != b'"' {
        return Err(());
    }
    name.push(b'.');
    loop {
        let mut c = cs.next_char();
        if c == b'\n' {
            return Err(());
        }
        if c == b'"' {
            break;
        }
        if c == b'\\' {
            c = cs.next_char();
            if c == b'\n' {
                return Err(());
            }
        }
        name.push(c);
    }
    match cs.next_char() {
        b']' => Ok(()),
        _ => Err(()),
    }
}

/// `get_base_var()` (config.c:983).
fn get_base_var(cs: &mut Source<'_>, name: &mut Vec<u8>) -> Result<(), ()> {
    cs.subsection_case_sensitive = true;
    loop {
        let c = cs.next_char();
        if cs.eof {
            return Err(());
        }
        if c == b']' {
            return Ok(());
        }
        if is_space(c) {
            return get_extended_base_var(cs, name, c);
        }
        if !is_key_char(c) && c != b'.' {
            return Err(());
        }
        name.push(c.to_ascii_lowercase());
    }
}

/// U+FEFF encoded in UTF-8, skipped at the head of a config file (config.c:1064).
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// `git_parse_source()` (config.c:1048), driving the store callbacks. `Err` is the
/// `bad config line` path.
fn parse_source(contents: &[u8], store: &mut Store) -> Result<(), ()> {
    let mut cs = Source::new(contents);
    let mut comment = false;
    let mut baselen = 0usize;
    let mut bom = 0usize;
    let mut in_bom = true;

    loop {
        let c = cs.next_char();
        if in_bom {
            if bom < UTF8_BOM.len() {
                if c == UTF8_BOM[bom] {
                    bom += 1;
                    continue;
                }
                // Do not tolerate a partial BOM.
                if bom != 0 {
                    return Err(());
                }
                in_bom = false;
            } else {
                in_bom = false;
            }
        }
        if c == b'\n' {
            if cs.eof {
                do_event(&cs, EventType::Eof, store);
                return Ok(());
            }
            do_event(&cs, EventType::Whitespace, store);
            comment = false;
            continue;
        }
        if comment {
            continue;
        }
        if is_space(c) {
            do_event(&cs, EventType::Whitespace, store);
            continue;
        }
        if c == b'#' || c == b';' {
            do_event(&cs, EventType::Comment, store);
            comment = true;
            continue;
        }
        if c == b'[' {
            do_event(&cs, EventType::Section, store);
            let mut var = Vec::new();
            if get_base_var(&mut cs, &mut var).is_err() || var.is_empty() {
                return Err(());
            }
            var.push(b'.');
            baselen = var.len();
            cs.var = var;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            return Err(());
        }
        do_event(&cs, EventType::Entry, store);
        let mut name = cs.var[..baselen.min(cs.var.len())].to_vec();
        name.push(c.to_ascii_lowercase());
        if get_value(&mut cs, store, &mut name).is_err() {
            return Err(());
        }
    }
}

// ---------------------------------------------------------------------------
// Writer — config.c:2711-3244
// ---------------------------------------------------------------------------

/// `store_create_section()` (config.c:2711): the header line for a key that has none yet,
/// spelled with the case the caller typed.
fn store_create_section(key: &[u8], baselen: usize) -> Vec<u8> {
    let base = &key[..baselen];
    let mut sb = Vec::new();
    match base.iter().position(|&c| c == b'.') {
        Some(dot) => {
            sb.push(b'[');
            sb.extend_from_slice(&base[..dot]);
            sb.extend_from_slice(b" \"");
            for &c in &base[dot + 1..] {
                if c == b'"' || c == b'\\' {
                    sb.push(b'\\');
                }
                sb.push(c);
            }
            sb.extend_from_slice(b"\"]\n");
        }
        None => {
            sb.push(b'[');
            sb.extend_from_slice(base);
            sb.extend_from_slice(b"]\n");
        }
    }
    sb
}

/// `write_pair()` (config.c:2746): `\t<name> = <value>` with git's quoting rules, and the
/// already-prepared comment trailer appended inside the closing quote.
fn write_pair(key: &[u8], baselen: usize, value: &[u8], comment: Option<&str>) -> Vec<u8> {
    // Problematic characters are backslash-quoted below; the surrounding double quotes
    // exist to keep leading/trailing SP and comment introducers from being re-parsed away.
    let mut quote: &[u8] = b"";
    if value.first() == Some(&b' ') {
        quote = b"\"";
    }
    for &c in value {
        if c == b';' || c == b'#' || c == b'\r' {
            quote = b"\"";
        }
    }
    if value.last() == Some(&b' ') {
        quote = b"\"";
    }

    let mut sb = Vec::new();
    sb.push(b'\t');
    sb.extend_from_slice(&key[baselen + 1..]);
    sb.extend_from_slice(b" = ");
    sb.extend_from_slice(quote);
    for &c in value {
        match c {
            b'\n' => sb.extend_from_slice(b"\\n"),
            b'\t' => sb.extend_from_slice(b"\\t"),
            b'"' | b'\\' => {
                sb.push(b'\\');
                sb.push(c);
            }
            _ => sb.push(c),
        }
    }
    sb.extend_from_slice(quote);
    if let Some(comment) = comment {
        sb.extend_from_slice(comment.as_bytes());
    }
    sb.push(b'\n');
    sb
}

/// `maybe_remove_section()` (config.c:2811): widen the removal span to swallow a section
/// header that is about to be left empty, but only when no comment could belong to it.
fn maybe_remove_section(
    store: &Store,
    begin_offset: &mut usize,
    end_offset: &mut usize,
    seen_ptr: &mut usize,
) {
    let mut section_seen = false;
    let mut seen = *seen_ptr;

    // First: this must be the section's first entry, with no comment before either it or
    // the header.
    let mut i = store.seen[seen];
    while i > 0 {
        let p = store.parsed[i - 1];
        match p.ty {
            EventType::Comment => return,
            EventType::Entry => {
                if !section_seen {
                    return;
                }
                break;
            }
            EventType::Section => {
                if !p.is_keys_section {
                    break;
                }
                section_seen = true;
            }
            _ => {}
        }
        i -= 1;
    }
    let begin = store.parsed[i].begin;

    // Next: these must be the last key(s) in the section, with no trailing comment that
    // could be about it.
    let mut i = store.seen[seen] + 1;
    while i < store.parsed_nr {
        let p = store.parsed[i];
        match p.ty {
            EventType::Comment => return,
            EventType::Section => {
                if p.is_keys_section {
                    i += 1;
                    continue;
                }
                break;
            }
            EventType::Entry => {
                seen += 1;
                if seen < store.seen_nr && i == store.seen[seen] {
                    // We want to remove this entry, too.
                    i += 1;
                    continue;
                }
                // There is another entry in this section.
                return;
            }
            _ => {}
        }
        i += 1;
    }

    *seen_ptr = seen;
    *begin_offset = begin;
    *end_offset = match i < store.parsed_nr {
        true => store.parsed[i].begin,
        false => store.parsed[store.parsed_nr - 1].end,
    };
}

/// `repo_config_set_multivar_in_file_gently()` (config.c:2999).
///
/// `value == None` unsets. The file is rewritten by copying the byte ranges the change
/// does not touch and splicing the new line in between, so every comment, indentation
/// choice and section-header spelling in the untouched parts survives verbatim.
pub fn set_multivar_in_file(
    path: &Path,
    key: &str,
    canonical_key: &str,
    baselen: usize,
    value: Option<&[u8]>,
    pattern: ValuePattern<'_>,
    comment: Option<&str>,
    multi_replace: bool,
) -> Result<(), StoreError> {
    let matcher = match pattern {
        ValuePattern::Any => Matcher::Any,
        ValuePattern::Never => Matcher::Never,
        ValuePattern::Fixed(v) => Matcher::Fixed(v.as_bytes().to_vec()),
        ValuePattern::Regex(p) => {
            let (do_not_match, p) = match p.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, p),
            };
            match regex::bytes::Regex::new(p) {
                Ok(re) => Matcher::Regex { re, do_not_match },
                Err(_) => return Err(StoreError::InvalidPattern(p.to_string())),
            }
        }
    };

    let mut store = Store {
        key: canonical_key.as_bytes().to_vec(),
        baselen,
        matcher,
        multi_replace,
        parsed: vec![Parsed { begin: 0, end: 0, ty: EventType::Eof, is_keys_section: false }],
        parsed_nr: 0,
        seen: Vec::new(),
        seen_nr: 0,
        seen_touched: false,
        section_seen: false,
        is_keys_section: false,
        key_seen: false,
        previous_type: EventType::Eof,
        previous_offset: 0,
    };

    let target = resolve_symlink(path);
    let contents = match std::fs::read(&target) {
        Ok(bytes) => Some(bytes),
        // ENOENT takes the "write a minimal version" branch; every other errno is the
        // `fopen_or_warn()` warning the parse would have printed, and then
        // `error(_("invalid config file %s"))` — CONFIG_INVALID_FILE.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            eprintln!("warning: unable to access '{}': {}", target.display(), strerror(&err));
            return Err(StoreError::InvalidFile);
        }
    };

    let mut out: Vec<u8> = Vec::new();

    let Some(contents) = contents else {
        // A file that does not exist yet gets a minimal version — but there is nothing to
        // unset in it.
        let Some(value) = value else {
            return Err(StoreError::NothingSet);
        };
        out.extend_from_slice(&store_create_section(key.as_bytes(), baselen));
        out.extend_from_slice(&write_pair(key.as_bytes(), baselen, value, comment));
        write_file(&target, &out, None)?;
        return Ok(());
    };

    if parse_source(&contents, &mut store).is_err() {
        return Err(StoreError::InvalidFile);
    }

    // Nothing to unset, or too many matches to replace with one value.
    if (store.seen_nr == 0 && value.is_none()) || (store.seen_nr > 1 && !store.multi_replace) {
        return Err(StoreError::NothingSet);
    }

    if store.seen_nr == 0 {
        if !store.seen_touched {
            // Did not see key nor section.
            store.seen = vec![store.parsed_nr - usize::from(store.parsed_nr != 0)];
        }
        store.seen_nr = 1;
    }

    let mut copy_begin = 0usize;
    let mut i = 0usize;
    while i < store.seen_nr {
        let j = store.seen[i];
        let copy_end;
        let mut replace_end;
        if !store.key_seen {
            let mut end = store.parsed[j].end;
            // Include '\n' when copying a section header.
            if end > 0 && end < contents.len() && contents[end - 1] != b'\n' && contents[end] == b'\n' {
                end += 1;
            }
            copy_end = end;
            replace_end = end;
        } else {
            replace_end = store.parsed[j].end;
            let mut end = store.parsed[j].begin;
            if value.is_none() {
                maybe_remove_section(&store, &mut end, &mut replace_end, &mut i);
            }
            // Swallow preceding white-space on the same line.
            while end > 0 {
                let c = contents[end - 1];
                if is_space(c) && c != b'\n' {
                    end -= 1;
                } else {
                    break;
                }
            }
            copy_end = end;
        }

        let new_line = copy_end > 0 && contents[copy_end - 1] != b'\n';
        if copy_end > copy_begin {
            out.extend_from_slice(&contents[copy_begin..copy_end]);
            if new_line {
                out.push(b'\n');
            }
        }
        copy_begin = replace_end;
        i += 1;
    }

    if let Some(value) = value {
        if !store.section_seen {
            out.extend_from_slice(&store_create_section(key.as_bytes(), baselen));
        }
        out.extend_from_slice(&write_pair(key.as_bytes(), baselen, value, comment));
    }

    if copy_begin < contents.len() {
        out.extend_from_slice(&contents[copy_begin..]);
    }

    write_file(&target, &out, Some(&target))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Section rename / removal — config.c:3296-3505
// ---------------------------------------------------------------------------

/// `GIT_CONFIG_MAX_LINE_LEN` (config.c:3352).
const MAX_LINE_LEN: usize = 512 * 1024;

/// The outcomes of a rename/remove: git returns the number of headers it touched, or an
/// error string it already printed as `error: …`.
pub enum RenameError {
    /// `refusing to work with overly long line in '<file>' on line <n>`.
    LongLine(usize),
    Io(std::io::Error),
}

/// `section_name_match()` (config.c:3296): how many bytes of `buf` the header for
/// `name` occupies, trailing whitespace included, or 0 when it is a different section.
fn section_name_match(buf: &[u8], name: &[u8]) -> usize {
    // Both sides are NUL-terminated C strings there, so a read past the end yields 0.
    let at = |i: usize| -> u8 { buf.get(i).copied().unwrap_or(0) };
    let nat = |j: usize| -> u8 { name.get(j).copied().unwrap_or(0) };

    if at(0) != b'[' {
        return 0;
    }
    let mut i = 1usize;
    let mut j = 0usize;
    let mut dot = false;
    // `for (i = 1; buf[i] && buf[i] != ']'; i++)` — the loop's own `i++` runs on every
    // iteration that does not `break`, including the `continue` below.
    while at(i) != 0 && at(i) != b']' {
        if !dot && is_space(at(i)) {
            dot = true;
            let c = nat(j);
            j += 1;
            if c != b'.' {
                break;
            }
            i += 1;
            while is_space(at(i)) {
                i += 1;
            }
            if at(i) != b'"' {
                break;
            }
            i += 1;
            continue;
        }
        if at(i) == b'\\' && dot {
            i += 1;
        } else if at(i) == b'"' && dot {
            i += 1;
            while is_space(at(i)) {
                i += 1;
            }
            break;
        }
        let c = nat(j);
        j += 1;
        if at(i) != c {
            break;
        }
        i += 1;
    }
    if at(i) == b']' && nat(j) == 0 {
        // We match; now find the right length by gobbling up any whitespace after it.
        i += 1;
        while at(i) != 0 && is_space(at(i)) {
            i += 1;
        }
        return i;
    }
    0
}

/// `repo_config_copy_or_rename_section_in_file()` (config.c:3353). `new_name == None`
/// removes the section. Returns how many headers matched.
pub fn copy_or_rename_section_in_file(
    path: &Path,
    old_name: &str,
    new_name: Option<&str>,
    copy: bool,
) -> Result<usize, RenameError> {
    let target = resolve_symlink(path);
    let contents = match std::fs::read(&target) {
        Ok(bytes) => bytes,
        // No config file means nothing to rename, and no error.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(RenameError::Io(err)),
    };

    let mut ret = 0usize;
    let mut remove = false;
    let mut out: Vec<u8> = Vec::new();
    let mut copystr: Vec<u8> = Vec::new();
    let mut line_nr = 0usize;

    for line in whole_lines(&contents) {
        let mut output: &[u8] = line;
        line_nr += 1;

        if line.len() >= MAX_LINE_LEN {
            return Err(RenameError::LongLine(line_nr));
        }

        let mut is_section = false;
        let i = line.iter().position(|&c| !is_space(c)).unwrap_or(line.len());
        // A NUL-terminated C string stops at the first NUL; `isspace` over the whole line
        // is the same for the shapes a config file can hold.
        let mut prefixed: Vec<u8> = Vec::new();
        if line.get(i) == Some(&b'[') {
            is_section = true;

            // A new section flushes whatever `--copy` was accumulating: there can be more
            // than one `[branch "$name"]`.
            if !copystr.is_empty() {
                out.extend_from_slice(&copystr);
                copystr.clear();
            }

            let offset = section_name_match(&line[i..], old_name.as_bytes());
            if offset > 0 {
                ret += 1;
                match new_name {
                    None => {
                        remove = true;
                        continue;
                    }
                    Some(new_name) => {
                        let baselen = new_name.len();
                        if !copy {
                            out.extend_from_slice(&store_create_section(new_name.as_bytes(), baselen));
                            // The new section was written with its newline; skip the old
                            // header's bytes.
                            let rest = &line[offset + i..];
                            if rest.is_empty() {
                                output = rest;
                            } else {
                                // More content means a declaration belongs on the next
                                // line; indent it with a tab.
                                prefixed.push(b'\t');
                                prefixed.extend_from_slice(rest);
                                output = &prefixed;
                            }
                        } else {
                            copystr = store_create_section(new_name.as_bytes(), baselen);
                        }
                    }
                }
            }
            remove = false;
        }
        if remove {
            continue;
        }

        if !is_section && !copystr.is_empty() {
            copystr.extend_from_slice(output);
        }
        out.extend_from_slice(output);
    }

    // A trailing copied section is not flushed by the loop above.
    if !copystr.is_empty() {
        out.extend_from_slice(&copystr);
    }

    write_file(&target, &out, Some(&target))?;
    Ok(ret)
}

impl From<StoreError> for RenameError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Io(err) => RenameError::Io(err),
            _ => RenameError::Io(std::io::Error::other("config write failed")),
        }
    }
}

/// `strbuf_getwholeline(&buf, f, '\n')`: each line keeps its terminator, and a final
/// unterminated line is still yielded.
fn whole_lines(contents: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = contents;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = match rest.iter().position(|&c| c == b'\n') {
            Some(i) => i + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

// ---------------------------------------------------------------------------
// File replacement
// ---------------------------------------------------------------------------

/// `resolve_symlink()` (lockfile.c:83): the lock — and therefore the rewritten file — is
/// taken on the symlink's target, so `git config --file=link` edits the file the link
/// points at rather than replacing the link.
fn resolve_symlink(path: &Path) -> PathBuf {
    /// `MAXDEPTH` (lockfile.c:73).
    const MAXDEPTH: usize = 5;
    let mut path = path.to_path_buf();
    for _ in 0..MAXDEPTH {
        let Ok(link) = std::fs::read_link(&path) else {
            return path;
        };
        path = match link.is_absolute() {
            true => link,
            false => match path.parent() {
                Some(dir) => dir.join(link),
                None => link,
            },
        };
    }
    path
}

/// Replace `path` with `bytes` atomically, carrying over the permissions of
/// `mode_from` — git chmods its lock file to `st.st_mode & 07777` of the original
/// (config.c:3155) so that a `git config` write does not widen a tightened config.
fn write_file(path: &Path, bytes: &[u8], mode_from: Option<&Path>) -> Result<(), StoreError> {
    use std::io::Write;

    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(StoreError::Io)?;
        f.write_all(bytes).map_err(StoreError::Io)?;
        #[cfg(unix)]
        if let Some(from) = mode_from {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(from) {
                let mode = meta.permissions().mode() & 0o7777;
                f.set_permissions(std::fs::Permissions::from_mode(mode)).ok();
            }
        }
        f.sync_all().map_err(StoreError::Io)?;
    }
    std::fs::rename(&tmp, path).map_err(StoreError::Io)?;
    Ok(())
}
