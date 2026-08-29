//! Commit history filtering shared by `log` and `shortlog` — the `--grep`,
//! `--author`, `--committer` predicates and the regex-dialect handling behind
//! `-E`/`-F`/`-P`/`-i`. One implementation so the two commands agree on which
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

/// Rewrites an identity the way a mailmap would, given its recorded name and
/// address. `commit_match()` applies this to the `author`/`committer` headers before
/// grepping them, so `--author=<canonical>` finds a commit recorded under an alias.
pub type IdentMapper = std::sync::Arc<dyn Fn(&[u8], &[u8]) -> (Vec<u8>, Vec<u8>) + Send + Sync>;

/// Compiled `log`/`shortlog` header/message predicates.
#[derive(Default)]
pub struct CommitFilter {
    /// The mailmap rewrite applied to the two identity headers before matching;
    /// `None` leaves them as the commit recorded them.
    pub ident_map: Option<IdentMapper>,
    pub author_res: Vec<regex::bytes::Regex>,
    pub committer_res: Vec<regex::bytes::Regex>,
    pub grep_res: Vec<regex::bytes::Regex>,
    /// `--all-match`: every `--grep` must match, not just one.
    pub all_match: bool,
    /// `--invert-grep`: keep commits whose message does NOT match.
    pub invert_grep: bool,
}

impl CommitFilter {
    pub fn is_empty(&self) -> bool {
        self.author_res.is_empty() && self.committer_res.is_empty() && self.grep_res.is_empty()
    }

    /// One identity header as it is matched against: rewritten through the mailmap
    /// when there is one, otherwise exactly as recorded.
    fn mapped_ident_line(&self, sig: gix::actor::SignatureRef<'_>) -> BString {
        let Some(map) = &self.ident_map else {
            return ident_line(sig);
        };
        let (name, email) = map(sig.name, sig.email);
        ident_line(gix::actor::SignatureRef {
            name: name.as_slice().into(),
            email: email.as_slice().into(),
            time: sig.time,
        })
    }

    /// git's header-and-message match: `--author` AND `--committer` AND the
    /// `--grep` result (OR of patterns, or AND under `--all-match`, then flipped
    /// by `--invert-grep`). An empty predicate set matches everything.
    pub fn matches(&self, commit: &gix::Commit<'_>) -> Result<bool> {
        if !self.author_res.is_empty() {
            let line = self.mapped_ident_line(commit.author()?);
            if !self.author_res.iter().any(|re| re.is_match(line.as_bytes())) {
                return Ok(false);
            }
        }
        if !self.committer_res.is_empty() {
            let line = self.mapped_ident_line(commit.committer()?);
            if !self
                .committer_res
                .iter()
                .any(|re| re.is_match(line.as_bytes()))
            {
                return Ok(false);
            }
        }
        if self.grep_res.is_empty() {
            return Ok(true);
        }
        let message = commit.message_raw()?;
        let hit = if self.all_match {
            self.grep_res.iter().all(|re| re.is_match(message))
        } else {
            self.grep_res.iter().any(|re| re.is_match(message))
        };
        Ok(hit != self.invert_grep)
    }
}
