//! Commit history filtering shared by `log`, `rev-list` and `shortlog` — the `--grep`,
//! `--author`, `--committer` predicates and the regex-dialect handling behind
//! `-E`/`-F`/`-P`/`-i`. One implementation so the three commands agree on which
//! commits match, byte-for-byte, including git's BRE-default semantics.

use anyhow::{anyhow, Result};
use gix::bstr::{BString, ByteSlice};

/// The regex dialect git selects via `-G`/`-E`/`-F`/`-P` (default basic/BRE).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Basic,
    Extended,
    Fixed,
    Perl,
}

/// Compile each pattern to a byte regex in `dialect`.
pub fn compile_patterns(
    patterns: &[String],
    dialect: Dialect,
    ignore_case: bool,
    origin: Origin,
) -> Result<Vec<regex::bytes::Regex>> {
    patterns
        .iter()
        .map(|p| build_regex(p, dialect, ignore_case, origin))
        .collect()
}

/// Where a pattern came from, which decides how a `regcomp` failure over it is
/// worded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `p->origin = "command line"` — a `--grep`.
    CommandLine,
    /// `p->origin = "header"` — `--author`/`--committer`, which
    /// `compile_grep_patterns()` appends as header patterns.
    Header,
    /// The pickaxe, which is not a `grep_pat` at all: `diffcore_pickaxe()` calls
    /// `regcomp()` itself and dies `invalid regex: <regerror>` with no origin and
    /// no quoted pattern (diffcore-pickaxe.c).
    Pickaxe,
}

impl Origin {
    /// `compile_regexp_failed()`'s `where` prefix.
    fn describe(self, pattern: &str, text: &str) -> String {
        match self {
            Origin::CommandLine => format!("command line, '{pattern}': {text}"),
            Origin::Header => format!("header, '{pattern}': {text}"),
            Origin::Pickaxe => format!("invalid regex: {text}"),
        }
    }
}

/// `compile_regexp_failed()` (grep.c):
///
/// ```c
/// static NORETURN void compile_regexp_failed(const struct grep_pat *p, const char *error)
/// {
///         char where[1024];
///
///         if (p->no)
///                 xsnprintf(where, sizeof(where), "In '%s' at %d, ", p->origin, p->no);
///         else if (p->origin)
///                 xsnprintf(where, sizeof(where), "%s, ", p->origin);
///         else
///                 where[0] = 0;
///
///         die("%s'%s': %s", where, p->pattern, error);
/// }
/// ```
///
/// A `--grep`/`--author`/`--committer` pattern has `origin = "command line"`, so
/// the refusal reads `fatal: command line, '<pattern>': <regerror text>`. The
/// tail is the platform's `regerror()`, which [`crate::porcelain::line_log::bre_syntax_error`]
/// reproduces for the syntax errors that have a stable wording; anything else
/// keeps the `regex` crate's own text, which is this binary speaking for itself.
fn regex_failure(
    pattern: &str,
    dialect: Dialect,
    origin: Origin,
    err: regex::Error,
) -> anyhow::Error {
    let text = match dialect {
        Dialect::Basic => crate::porcelain::line_log::bre_syntax_error(pattern),
        Dialect::Extended | Dialect::Perl => crate::porcelain::line_log::ere_syntax_error(pattern),
        // `-F` escapes the pattern into a literal, so there is no syntax to fail.
        Dialect::Fixed => None,
    };
    match text {
        Some(text) => crate::fatal::Fatal(origin.describe(pattern, text)).into(),
        None => anyhow!("invalid regex: {err}"),
    }
}

/// Build one byte regex from a pattern in `dialect`, mirroring git's engine as
/// far as the `regex` crate allows: `-F` escapes to a literal, ERE/PCRE pass
/// through, BRE is translated by swapping which operators are escaped.
pub fn build_regex(
    pattern: &str,
    dialect: Dialect,
    ignore_case: bool,
    origin: Origin,
) -> Result<regex::bytes::Regex> {
    let translated = match dialect {
        Dialect::Fixed => regex::escape(pattern),
        Dialect::Extended | Dialect::Perl => pattern.to_string(),
        Dialect::Basic => bre_to_regex(pattern),
    };
    let compile = |pat: &str| {
        regex::bytes::RegexBuilder::new(pat)
            .case_insensitive(ignore_case)
            .unicode(false) // git greps bytes, not scalar values
            .build()
    };
    match compile(&translated) {
        Ok(re) => Ok(re),
        // git's POSIX engine treats a `{`/`}` that forms no valid interval as a
        // literal; the crate rejects it. Recover that leniency by literalising
        // the braces and retrying — a genuine error still surfaces.
        Err(_) => {
            let lenient = translated.replace('{', "\\{").replace('}', "\\}");
            compile(&lenient).map_err(|e| regex_failure(pattern, dialect, origin, e))
        }
    }
}

/// GNU BRE → `regex`-crate syntax. In BRE the grouping/quantifier operators are
/// the *escaped* forms (`\(` `\)` `\{` `\}` `\+` `\?` `\|`) while the bare
/// characters are literals; ERE (and this crate) are the reverse. Bytes inside a
/// `[...]` bracket expression are copied verbatim.
pub fn bre_to_regex(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut in_class = false;
    while i < b.len() {
        let c = b[i];
        if in_class {
            out.push(c as char);
            if c == b']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'[' => {
                in_class = true;
                out.push('[');
            }
            b'\\' if i + 1 < b.len() => {
                let n = b[i + 1];
                match n {
                    // BRE's escaped operators become bare operators.
                    b'(' | b')' | b'{' | b'}' | b'+' | b'?' | b'|' => out.push(n as char),
                    // Everything else keeps its backslash (`\.`, `\\`, `\b`, …).
                    _ => {
                        out.push('\\');
                        out.push(n as char);
                    }
                }
                i += 1;
            }
            // Bare operators are literals in BRE, so escape them for the crate.
            b'(' | b')' | b'{' | b'}' | b'+' | b'?' | b'|' => {
                out.push('\\');
                out.push(c as char);
            }
            _ => out.push(c as char),
        }
        i += 1;
    }
    out
}

/// The raw `author`/`committer` header value git greps against:
/// `Name <email> <seconds> <tz>`.
pub fn ident_line(sig: gix::actor::SignatureRef<'_>) -> BString {
    let mut out = BString::from(sig.name.to_vec());
    out.push(b' ');
    out.push(b'<');
    out.extend_from_slice(sig.email);
    out.push(b'>');
    out.push(b' ');
    out.extend_from_slice(sig.time.as_bytes());
    out
}

/// `get_log_output_encoding()` (environment.c:189-198): `--encoding=<enc>` when
/// given (`--encoding=none` is stored as the empty string, revision.c:2701-2707),
/// then `i18n.logOutputEncoding`, then `i18n.commitEncoding`, then UTF-8.
///
/// ```c
/// const char *get_log_output_encoding(void)
/// {
///         return git_log_output_encoding ? git_log_output_encoding
///                 : get_commit_output_encoding();
/// }
/// const char *get_commit_output_encoding(void)
/// {
///         return git_commit_encoding ? git_commit_encoding : "UTF-8";
/// }
/// ```
pub fn log_output_encoding(repo: &gix::Repository, option: Option<&str>) -> String {
    if let Some(v) = option {
        return v.to_string();
    }
    let cfg = repo.config_snapshot();
    cfg.string("i18n.logOutputEncoding")
        .or_else(|| cfg.string("i18n.commitEncoding"))
        .map(|v| v.to_string())
        .unwrap_or_else(|| "UTF-8".to_string())
}

/// Compiled `log`/`rev-list`/`shortlog` header and message predicates — the
/// `revs->grep_filter` that `commit_match()` (revision.c:4094-4157) runs.
#[derive(Default)]
pub struct CommitFilter {
    /// `revs->mailmap`: when set, the `author`/`committer` headers are rewritten
    /// through it before a header pattern is matched.
    pub mailmap: Option<std::sync::Arc<crate::mailmap::Mailmap>>,
    /// `--author` header patterns (`GREP_HEADER_AUTHOR`).
    pub author_res: Vec<regex::bytes::Regex>,
    /// `--committer` header patterns (`GREP_HEADER_COMMITTER`).
    pub committer_res: Vec<regex::bytes::Regex>,
    /// `--grep-reflog` header patterns (`GREP_HEADER_REFLOG`); any of them sets
    /// `use_reflog_filter` (grep.c:176-182).
    pub reflog_res: Vec<regex::bytes::Regex>,
    /// `--grep` body patterns (`GREP_PATTERN_BODY`).
    pub grep_res: Vec<regex::bytes::Regex>,
    /// `--all-match`.
    pub all_match: bool,
    /// `--invert-grep`: `no_body_match`.
    pub invert_grep: bool,
    /// `get_log_output_encoding()`, which the commit is re-coded into before it is
    /// grepped; see [`log_output_encoding`].
    pub output_encoding: String,
}

impl CommitFilter {
    /// `!opt->grep_filter.pattern_list && !opt->grep_filter.header_list`.
    pub fn is_empty(&self) -> bool {
        self.author_res.is_empty()
            && self.committer_res.is_empty()
            && self.reflog_res.is_empty()
            && self.grep_res.is_empty()
    }

    /// `commit_match()` for a walk that has no reflog entry and shows no notes.
    pub fn matches(&self, commit: &gix::Commit<'_>) -> Result<bool> {
        self.matches_with(commit, None, None)
    }

    /// `commit_match()`: `reflog_message` is `get_reflog_message()` for the entry
    /// being shown under `--walk-reflogs`, and `notes` produces
    /// `format_display_notes(..., raw = 1)` when `revs->show_notes` is on.
    pub fn matches_with(
        &self,
        commit: &gix::Commit<'_>,
        reflog_message: Option<&[u8]>,
        notes: Option<&dyn Fn() -> Result<Vec<u8>>>,
    ) -> Result<bool> {
        if self.is_empty() {
            return Ok(true);
        }
        let notes = notes.map(|f| f()).transpose()?;
        Ok(self.matches_buffer(&commit.data, reflog_message, notes.as_deref()))
    }

    /// The body of `commit_match()` (revision.c:4094-4157) over the stored commit
    /// object `data`:
    ///
    /// ```c
    /// if (opt->grep_filter.use_reflog_filter) {
    ///         strbuf_addstr(&buf, "reflog ");
    ///         get_reflog_message(&buf, opt->reflog_info);
    ///         strbuf_addch(&buf, '\n');
    /// }
    /// encoding = get_log_output_encoding();
    /// message = repo_logmsg_reencode(the_repository, commit, NULL, encoding);
    /// if (buf.len)
    ///         strbuf_addstr(&buf, message);
    /// if (opt->grep_filter.header_list && opt->mailmap) {
    ///         const char *commit_headers[] = { "author ", "committer ", NULL };
    ///         if (!buf.len)
    ///                 strbuf_addstr(&buf, message);
    ///         apply_mailmap_to_header(&buf, commit_headers, opt->mailmap);
    /// }
    /// if (opt->show_notes) {
    ///         if (!buf.len)
    ///                 strbuf_addstr(&buf, message);
    ///         format_display_notes(&commit->object.oid, &buf, encoding, 1);
    /// }
    /// if (buf.len)
    ///         retval = grep_buffer(&opt->grep_filter, buf.buf, buf.len);
    /// else
    ///         retval = grep_buffer(&opt->grep_filter,
    ///                              (char *)message, strlen(message));
    /// ```
    ///
    /// Both roads read the re-coded message as a C string, so it ends at its first
    /// NUL. `apply_mailmap_to_header()` rewrites the *whole* buffer's worth of
    /// header-looking lines its scan reaches — and a rewritten last header carries
    /// that scan into the message (see [`crate::mailmap::apply_mailmap_to_header`]),
    /// so a `--grep` can see a mapped `author `/`committer ` line of the body.
    pub fn matches_buffer(
        &self,
        data: &[u8],
        reflog_message: Option<&[u8]>,
        notes: Option<&[u8]>,
    ) -> bool {
        if self.is_empty() {
            return true;
        }
        let mut buf = Vec::new();
        if !self.reflog_res.is_empty() {
            buf.extend_from_slice(b"reflog ");
            buf.extend_from_slice(reflog_message.unwrap_or_default());
            buf.push(b'\n');
        }
        let mut message = data.to_vec();
        crate::porcelain::log::logmsg_reencode(&mut message, &self.output_encoding);
        let message_len = message.find_byte(0).unwrap_or(message.len());
        buf.extend_from_slice(&message[..message_len]);
        let has_header_list = !(self.author_res.is_empty()
            && self.committer_res.is_empty()
            && self.reflog_res.is_empty());
        if has_header_list {
            if let Some(mailmap) = &self.mailmap {
                crate::mailmap::apply_mailmap_to_header(
                    &mut buf,
                    &[b"author ", b"committer "],
                    mailmap,
                );
            }
        }
        if let Some(notes) = notes {
            buf.extend_from_slice(notes);
        }
        self.compile().grep_buffer(&buf)
    }

    /// `compile_grep_patterns()` (grep.c:771-815) with `prep_header_patterns()`
    /// (grep.c:705-751) and `compile_pattern_or()` (grep.c:676-690), for the
    /// pattern shapes a revision walk can produce — plain atoms, no `--and`/`--not`
    /// or parentheses.
    fn compile(&self) -> Grep<'_> {
        // `compile_pattern_or()`: `or(first, compile_pattern_or(rest))`.
        let body = self.grep_res.iter().rev().fold(None, |rest, re| {
            let atom = Expr::new(Node::Atom(None, re));
            Some(match rest {
                None => atom,
                Some(rest) => Expr::or(atom, rest),
            })
        });

        // `prep_header_patterns()`: each field's patterns OR-ed as
        // `grep_or_expr(h, header_group[p->field])`, then the fields chained in
        // enum order onto a terminating `GREP_NODE_TRUE`.
        let mut header_expr: Option<Expr<'_>> = None;
        for (field, res) in [
            (Field::Author, &self.author_res),
            (Field::Committer, &self.committer_res),
            (Field::Reflog, &self.reflog_res),
        ] {
            let group = res.iter().fold(None, |group, re| {
                let h = Expr::new(Node::Atom(Some(field), re));
                Some(match group {
                    None => h,
                    Some(group) => Expr::or(h, group),
                })
            });
            let Some(group) = group else {
                continue;
            };
            let rest = header_expr.unwrap_or_else(|| Expr::new(Node::True));
            header_expr = Some(Expr::or(group, rest));
        }

        let mut all_match = self.all_match;
        let no_body_match = self.invert_grep;
        // `if (opt->all_match || opt->no_body_match || header_expr) extended = 1;
        // else if (!extended) return;` — the unextended form leaves
        // `pattern_expression` NULL and `match_line()` tries each pattern in turn,
        // which is what the OR chain evaluates to without hit collection.
        if !(all_match || no_body_match || header_expr.is_some()) {
            return Grep { expr: body, all_match, no_body_match, body_hit: false };
        }
        let mut expr = body;
        if no_body_match {
            expr = expr.map(|x| Expr::new(Node::Not(Box::new(x))));
        }
        if let Some(mut header_expr) = header_expr {
            expr = Some(match expr {
                None => header_expr,
                Some(x) if all_match => {
                    splice_or(&mut header_expr, x);
                    header_expr
                }
                Some(x) => Expr::or(x, header_expr),
            });
            all_match = true;
        }
        Grep { expr, all_match, no_body_match, body_hit: false }
    }
}

/// `enum grep_header_field` (grep.h) and its `header_field[]` prefixes
/// (grep.c:937-944).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Author,
    Committer,
    Reflog,
}

impl Field {
    fn prefix(self) -> &'static [u8] {
        match self {
            Field::Author => b"author ",
            Field::Committer => b"committer ",
            Field::Reflog => b"reflog ",
        }
    }
}

/// `enum grep_expr_node`, restricted to the nodes a revision walk builds.
enum Node<'a> {
    True,
    /// `GREP_PATTERN_HEAD` for a field, `GREP_PATTERN_BODY` without one.
    Atom(Option<Field>, &'a regex::bytes::Regex),
    Not(Box<Expr<'a>>),
    Or(Box<Expr<'a>>, Box<Expr<'a>>),
}

/// `struct grep_expr`: a node and its `hit` marker.
struct Expr<'a> {
    node: Node<'a>,
    hit: bool,
}

impl<'a> Expr<'a> {
    fn new(node: Node<'a>) -> Self {
        Expr { node, hit: false }
    }

    /// `grep_or_expr()`.
    fn or(left: Expr<'a>, right: Expr<'a>) -> Self {
        Expr::new(Node::Or(Box::new(left), Box::new(right)))
    }
}

/// `grep_splice_or()` (grep.c:753-769): replace the `GREP_NODE_TRUE` that ends the
/// header chain's right spine with `y`.
fn splice_or<'a>(x: &mut Expr<'a>, y: Expr<'a>) {
    let Node::Or(_, right) = &mut x.node else {
        return;
    };
    if matches!(right.node, Node::True) {
        **right = y;
    } else {
        splice_or(right, y);
    }
}

/// `strip_timestamp()` (grep.c:925-935): end the ident at its last `>`, looking
/// no further left than one byte past `bol`.
fn strip_timestamp(buf: &[u8], bol: usize, mut eol: usize) -> usize {
    let mut at = eol;
    while at > bol + 1 {
        at -= 1;
        if buf[at] == b'>' {
            eol = at + 1;
            break;
        }
    }
    eol
}

/// `match_one_pattern()` + `headerless_match_one_pattern()` (grep.c:946-1036),
/// without `--word-regexp`, which a revision walk never sets.
fn match_one_pattern(
    field: Option<Field>,
    re: &regex::bytes::Regex,
    buf: &[u8],
    mut bol: usize,
    mut eol: usize,
    in_header: bool,
) -> bool {
    if let Some(field) = field {
        let prefix = field.prefix();
        if !buf[bol..].starts_with(prefix) {
            return false;
        }
        bol += prefix.len();
        if matches!(field, Field::Author | Field::Committer) {
            eol = strip_timestamp(buf, bol, eol);
        }
    }
    // A header pattern only in the header, a body pattern only in the body.
    if field.is_some() != in_header {
        return false;
    }
    re.is_match(&buf[bol..eol])
}

/// The `grep_opt` state `grep_source()` consults for one commit.
struct Grep<'a> {
    /// `pattern_expression`; `None` only for an empty pattern list.
    expr: Option<Expr<'a>>,
    all_match: bool,
    no_body_match: bool,
    body_hit: bool,
}

impl Grep<'_> {
    /// `grep_source()` (grep.c:1832-1855) with `status_only` set, as
    /// `init_revisions()` does (revision.c:1949).
    fn grep_buffer(&mut self, buf: &[u8]) -> bool {
        if !self.all_match && !self.no_body_match {
            return self.grep_source_1(buf, false);
        }
        // `clr_hit_marker()`: the expression was built for this commit, so no
        // marker is set yet.
        self.body_hit = false;
        self.grep_source_1(buf, true);
        if self.all_match && !self.expr.as_ref().is_some_and(chk_hit_marker) {
            return false;
        }
        if self.no_body_match && self.body_hit {
            return false;
        }
        self.grep_source_1(buf, false)
    }

    /// `grep_source_1()`'s line loop (grep.c:1653-1771): the context is the header
    /// until the first empty line, and every line — the empty separator included —
    /// is matched in turn. The status-only pass stops at its first hit; the
    /// hit-collecting pass always reads to the end and reports nothing.
    fn grep_source_1(&mut self, buf: &[u8], collect_hits: bool) -> bool {
        let Some(expr) = self.expr.as_mut() else {
            return false;
        };
        let mut in_header = true;
        let mut bol = 0;
        let mut left = buf.len();
        while left > 0 {
            let eol = bol + buf[bol..bol + left].find_byte(b'\n').unwrap_or(left);
            left -= eol - bol;
            if in_header && eol == bol {
                in_header = false;
            }
            let hit = match_expr_eval(expr, buf, bol, eol, in_header, &mut self.body_hit, collect_hits);
            if hit && !collect_hits {
                return true;
            }
            bol = eol + 1;
            if left == 0 {
                break;
            }
            left -= 1;
        }
        false
    }
}

/// `match_expr_eval()` (grep.c:1039-1104) without `--column`.
fn match_expr_eval(
    x: &mut Expr<'_>,
    buf: &[u8],
    bol: usize,
    eol: usize,
    in_header: bool,
    body_hit: &mut bool,
    collect_hits: bool,
) -> bool {
    let h = match &mut x.node {
        Node::True => true,
        Node::Atom(field, re) => {
            let h = match_one_pattern(*field, re, buf, bol, eol, in_header);
            if field.is_none() {
                *body_hit |= h;
            }
            h
        }
        Node::Not(inner) => !match_expr_eval(inner, buf, bol, eol, in_header, body_hit, false),
        Node::Or(left, right) => {
            if !collect_hits {
                return match_expr_eval(left, buf, bol, eol, in_header, body_hit, false)
                    || match_expr_eval(right, buf, bol, eol, in_header, body_hit, false);
            }
            let h = match_expr_eval(left, buf, bol, eol, in_header, body_hit, false);
            left.hit |= h;
            h | match_expr_eval(right, buf, bol, eol, in_header, body_hit, true)
        }
    };
    if collect_hits {
        x.hit |= h;
    }
    h
}

/// `chk_hit_marker()` (grep.c:1820-1830): every left arm of the top-level OR
/// spine, and the node that ends it, must have hit somewhere in the buffer.
fn chk_hit_marker(mut x: &Expr<'_>) -> bool {
    loop {
        let Node::Or(left, right) = &x.node else {
            return x.hit;
        };
        if !left.hit {
            return false;
        }
        x = right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(patterns: &[&str]) -> Vec<regex::bytes::Regex> {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        compile_patterns(&patterns, Dialect::Basic, false, Origin::CommandLine).unwrap()
    }

    fn filter(author: &[&str], grep: &[&str]) -> CommitFilter {
        CommitFilter {
            author_res: res(author),
            grep_res: res(grep),
            output_encoding: "UTF-8".to_string(),
            ..CommitFilter::default()
        }
    }

    fn mailmap(text: &str) -> std::sync::Arc<crate::mailmap::Mailmap> {
        let mut map = crate::mailmap::Mailmap::default();
        map.read_string(text.as_bytes());
        std::sync::Arc::new(map)
    }

    const RENAMED: &[u8] = b"tree t\nauthor Old <old@x> 1 +0000\ncommitter Old <old@x> 1 +0000\n\nfirst subject\nauthor Old <old@x> body line\n";

    #[test]
    fn mailmapped_header_scan_reaches_the_body_only_with_a_header_pattern() {
        let mut f = filter(&["."], &["^author New"]);
        f.mailmap = Some(mailmap("New <new@x> <old@x>\n"));
        assert!(f.matches_buffer(RENAMED, None, None));
        // No header pattern: commit_match() never applies the mailmap.
        f.author_res.clear();
        assert!(!f.matches_buffer(RENAMED, None, None));
    }

    #[test]
    fn a_dropped_encoding_header_lets_the_rewrite_reach_the_body() {
        let data = b"tree t\nauthor F <f@x> 1 +0000\ncommitter Old <old@x> 1 +0000\nencoding UTF-8\n\nauthor Old <old@x> body\n";
        let mut f = filter(&["F"], &["^author New"]);
        f.mailmap = Some(mailmap("New <new@x> <old@x>\n"));
        assert!(f.matches_buffer(data, None, None));
        // `--encoding=none` keeps the header, which stops the scan short of the body.
        f.output_encoding.clear();
        assert!(!f.matches_buffer(data, None, None));
        f.grep_res = res(&["^author Old"]);
        assert!(f.matches_buffer(data, None, None));
    }

    #[test]
    fn author_patterns_see_the_ident_without_its_timestamp_in_every_header_line() {
        assert!(!filter(&["0000"], &[]).matches_buffer(RENAMED, None, None));
        assert!(filter(&["x.$"], &[]).matches_buffer(RENAMED, None, None));
        // The body's `author ` line is not a header.
        assert!(!filter(&["body"], &[]).matches_buffer(RENAMED, None, None));
        let dup = b"tree t\nauthor A <a@x> 1 +0000\nauthor B <b@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nm\n";
        assert!(filter(&["^B"], &[]).matches_buffer(dup, None, None));
    }

    #[test]
    fn grep_matches_body_lines_one_at_a_time() {
        let data = b"tree t\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nsubject\nplain second\n";
        assert!(filter(&[], &["^plain"]).matches_buffer(data, None, None));
        assert!(filter(&[], &["subject$"]).matches_buffer(data, None, None));
        assert!(!filter(&[], &["^tree"]).matches_buffer(data, None, None));
        // The empty separator line is the first body line.
        assert!(filter(&[], &["^$"]).matches_buffer(data, None, None));
        let headers_only = b"tree t\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n";
        assert!(!filter(&[], &["^$"]).matches_buffer(headers_only, None, None));
    }

    #[test]
    fn invert_grep_rejects_any_body_hit_even_under_all_match() {
        let data = b"tree t\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nalpha beta\n";
        let mut f = filter(&[], &["alpha", "zzz"]);
        f.all_match = true;
        f.invert_grep = true;
        assert!(!f.matches_buffer(data, None, None));
        f.grep_res = res(&["yyy", "zzz"]);
        assert!(f.matches_buffer(data, None, None));
    }

    #[test]
    fn invert_grep_sees_the_mailmapped_body() {
        let mut f = filter(&["."], &["^author New"]);
        f.mailmap = Some(mailmap("New <new@x> <old@x>\n"));
        f.invert_grep = true;
        assert!(!f.matches_buffer(RENAMED, None, None));
    }

    #[test]
    fn all_match_requires_every_header_field_and_pattern() {
        let data = b"tree t\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 1 +0000\n\nalpha\ngamma\n";
        let mut f = filter(&["^A", "^Z"], &["alpha", "gamma"]);
        f.committer_res = res(&["^C"]);
        f.all_match = true;
        assert!(f.matches_buffer(data, None, None));
        f.grep_res = res(&["alpha", "delta"]);
        assert!(!f.matches_buffer(data, None, None));
        // Without --all-match one --grep hit is enough, but every field still must hit.
        f.all_match = false;
        assert!(f.matches_buffer(data, None, None));
        f.committer_res = res(&["^A"]);
        assert!(!f.matches_buffer(data, None, None));
    }

    #[test]
    fn reflog_patterns_match_the_prepended_reflog_line() {
        let data = b"tree t\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nm\n";
        let f = CommitFilter {
            reflog_res: res(&["^commit: m$"]),
            output_encoding: "UTF-8".to_string(),
            ..CommitFilter::default()
        };
        assert!(f.matches_buffer(data, Some(b"commit: m"), None));
        assert!(!f.matches_buffer(data, Some(b"reset: moving"), None));
    }
}
