//! `git mktag` — read a tag object from stdin, fsck it, and write it verbatim.
//!
//! Covered: the whole command. `mktag` takes no positionals and exactly one
//! option, `--strict`/`--no-strict` (accepted in git's unambiguous-prefix forms,
//! e.g. `--stric`). Stdin is read whole, validated with the tag half of git's
//! fsck, and — on success — stored as an `OBJ_TAG` byte-for-byte as it arrived,
//! so the printed id matches stock git's. Stdout is that id plus a newline.
//!
//! The fsck port is `fsck_tag_buf` + `verify_headers` + `fsck_ident` from
//! `fsck.c`, including git's short-circuiting: the first message whose severity
//! resolves to an error stops validation, so only that one line is printed.
//! Message severities follow git's table, then `fsck.<msgId>` config
//! (`ignore`/`warn`/`error`, read in config order by `git_fsck_config()`),
//! and finally `mktag`'s own rule that a warning is reported as an error while
//! `--strict` is in effect (the default).
//! That reproduces `fsck.badTagName=warn` still failing under `--strict`, and
//! `--no-strict` demoting `missingTaggerEntry`/`badTagName`/`extraHeaderEntry`
//! to warnings that still write the tag.
//!
//! Not covered: `check_object_signature` on the tagged object. git re-hashes the
//! object it points at to catch a corrupt odb; here the object is only looked up
//! and its type compared, which is the same answer for every non-corrupt
//! repository and the same message text (`could not read tagged object '<oid>'`,
//! `object '<oid>' tagged as '<t>', but is a '<t>' type`) when it differs.
//!
//! Refname validation delegates to `gix::validate::reference::name` over
//! `refs/tags/<name>`, which implements the same rule set as
//! `check_refname_format(name, 0)`.
//!
//! Exit codes follow git: 0 on success, 129 for usage errors, 128 for a fatal
//! error (failed fsck, unreadable or mistyped tagged object).

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};

use super::fsck::{self, Msg, Severity};

/// git's usage block, printed on stderr after `error: unknown …`.
const USAGE: &str = "\
usage: git mktag

    --[no-]strict         enable more strict checking

";

/// `git mktag` — create a tag object from stdin after a strict fsck check.
///
/// Nothing but the object database is touched: no ref is created, no reflog
/// entry is written. The bytes read from stdin become the object's contents
/// unchanged, so a tag that stock git would produce hashes to the same id here.
pub fn mktag(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "mktag" => &args[1..],
        _ => args,
    };

    let mut strict = true;
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();
        // Positionals are silently ignored by git: `parse_options` leaves them
        // in argv and `cmd_mktag` never looks at what is left.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so it is matched whole here instead of going through
        // the prefix matching below. This table has no `PARSE_OPT_HIDDEN` entry,
        // so `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        let Some(long) = a.strip_prefix("--") else {
            let flag = a[1..].chars().next().expect("`-` alone was handled above");
            // parse_options_step() answers `-h` before it can be an unknown
            // switch, and on stdout — a help request is not a rejection.
            if flag == 'h' {
                return Ok(super::show_usage(USAGE));
            }
            eprintln!("error: unknown switch `{flag}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        };
        // `--` and `--end-of-options` both end the options
        // (parse-options.c:1112-1124); mktag ignores what follows.
        if long.is_empty() || long == "end-of-options" {
            no_more_opts = true;
            continue;
        }
        // `--strict=…` is rejected before abbreviation matching, as git does.
        if let Some((name, _)) = long.split_once('=') {
            let name = name.strip_prefix("no-").unwrap_or(name);
            if !name.is_empty() && "strict".starts_with(name) {
                eprintln!("error: option `strict' takes no value");
                return Ok(ExitCode::from(129));
            }
        }
        // git accepts any unambiguous prefix of an option name.
        let (negated, stem) = match long.strip_prefix("no-") {
            Some(rest) => (true, rest),
            None => (false, long),
        };
        if !stem.is_empty() && "strict".starts_with(stem) {
            strict = !negated;
            continue;
        }
        eprintln!("error: unknown option `{long}'");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = crate::setup::discover()?;

    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("could not read from stdin: {e}"))?;

    // `git_config(git_fsck_config, …)` (builtin/mktag.c:94) runs only after
    // `parse_options()` and the stdin read, and `git_fsck_config` ends in
    // `git_default_config()` — so `mktag --bogus` is a 129 under a value that
    // callback refuses, and a tag on stdin is a 128 before it is checked.
    if let Err(rejection) = crate::default_config::validate(&repo) {
        return Err(rejection.into_error());
    }

    // `Reporter::new` hands back git's `die()` text for an unusable `fsck.<id>`
    // value; git exits 128 with it, so route it through the same helper as the
    // other fatals rather than letting anyhow reformat it.
    let mut reporter = match Reporter::new(strict, &repo) {
        Ok(reporter) => reporter,
        Err(msg) => return fatal(&msg),
    };
    let tagged = fsck_tag(&buf, repo.object_hash(), &mut reporter);

    if reporter.failed {
        return fatal("tag on stdin did not pass our strict fsck check");
    }
    let Some((tagged_oid, tagged_kind)) = tagged else {
        // Only reachable when config demoted one of the structural checks that
        // git nevertheless treats as fatal to the parse; git would go on to use
        // an uninitialised object id, which is not something to imitate.
        crate::git_fatal!("tag on stdin has no usable 'object'/'type' header after fsck demotion");
    };

    // `verify_object_in_tag` (builtin/mktag.c:99) is the first object lookup, and
    // the lookup is what reads the repository settings block: measured against
    // git 2.55.0, `-c core.packedGitLimit=bogus mktag` still reports a tag that
    // fails fsck, and dies on the setting for one that passes.
    if let Err(msg) = crate::repo_settings::RepoSettings::load(&repo) {
        return fatal(&msg);
    }

    // `verify_object_in_tag`: the tagged object must exist and have that type.
    let Ok(header) = repo.find_header(tagged_oid) else {
        return fatal(&format!("could not read tagged object '{tagged_oid}'"));
    };
    if Some(header.kind()) != tagged_kind {
        // `type_name(OBJ_BAD)` is NULL, which `die()`'s printf renders as
        // `(null)` (builtin/mktag.c:62-65).
        let expected = tagged_kind.map_or("(null)".to_string(), |kind| kind.to_string());
        return fatal(&format!(
            "object '{tagged_oid}' tagged as '{expected}', but is a '{}' type",
            header.kind()
        ));
    }

    // Serialize the object write through the repo coordinator, as the other
    // writing porcelain does, so concurrent zvcs writers queue instead of racing.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let id = repo
        .objects
        .write_buf(Kind::Tag, &buf)
        .map_err(|_| anyhow::anyhow!("unable to write tag file"))?;
    println!("{id}");
    Ok(ExitCode::SUCCESS)
}

/// Report a git `fatal:` failure on stderr and yield git's exit code for it.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// The `fsck.h` rows `mktag` can report, all from the shared message table so
/// that one id has one spelling, one default and one `fsck.<id>` key across
/// `git fsck`, `git receive-pack` and here.
///
/// The first seven exist for `mktag` alone: `builtin/fsck.c` and `index-pack`
/// both run `parse_object_buffer()` before `fsck_object()` and never reach the
/// header walk that reports them. See [`fsck::Msg::receive_key`].
const ALL: [&Msg; 22] = [
    &fsck::NUL_IN_HEADER,
    &fsck::UNTERMINATED_HEADER,
    &fsck::MISSING_OBJECT,
    &fsck::BAD_OBJECT_SHA1,
    &fsck::MISSING_TYPE_ENTRY,
    &fsck::MISSING_TYPE,
    &fsck::BAD_TYPE,
    &fsck::MISSING_TAG_ENTRY,
    &fsck::MISSING_TAG,
    &fsck::BAD_TAG_NAME,
    &fsck::MISSING_TAGGER_ENTRY,
    &fsck::MISSING_NAME_BEFORE_EMAIL,
    &fsck::BAD_NAME,
    &fsck::MISSING_EMAIL,
    &fsck::MISSING_SPACE_BEFORE_EMAIL,
    &fsck::BAD_EMAIL,
    &fsck::MISSING_SPACE_BEFORE_DATE,
    &fsck::ZERO_PADDED_DATE,
    &fsck::BAD_DATE_OVERFLOW,
    &fsck::BAD_DATE,
    &fsck::BAD_TIMEZONE,
    &fsck::EXTRA_HEADER_ENTRY,
];

/// Applies severities and prints fsck messages the way `mktag` does.
struct Reporter {
    strict: bool,
    /// `fsck.<id>` overrides, keyed by git's camel-cased message id.
    overrides: Vec<(&'static str, Severity)>,
    /// Set once anything has been reported at error severity.
    failed: bool,
}

impl Reporter {
    /// `repo_config(the_repository, git_fsck_config, &fsck_options)`
    /// (builtin/mktag.c:102), keeping the overrides of the rows a tag can report.
    ///
    /// `git_fsck_config()` (fsck.c:1440-1469) is handed every variable in config
    /// order, so the first bad `fsck.*` value is the one that dies:
    ///
    /// * `fsck.skiplist` goes through `git_config_pathname()` and then
    ///   `fsck_set_msg_types("skiplist=<path>")`, whose `oidset_parse_file()`
    ///   dies on a file it cannot open or a line that is not an object id. The
    ///   list is never consulted afterwards: `fsck_tag_standalone()` is given a
    ///   NULL oid, which `object_on_skiplist()` never finds.
    /// * every other `fsck.<id>` is `fsck_set_msg_type()` (fsck.c:154-181): the
    ///   id is looked up among *all* of fsck's ids, not only the tag's —
    ///   `Unhandled message id: <id>` when none matches; the value is
    ///   `parse_msg_type()`, which knows `error`, `warn` and `ignore` and nothing
    ///   else; and a `FSCK_FATAL` row refuses to be demoted.
    ///
    /// A valueless `fsck.*` is `config_error_nonbool()`: its `error:` line is
    /// printed here and the `Err` is `configset_iter()`'s follow-up. Every other
    /// `Err` is git's `die()` text, without its `fatal: ` prefix.
    fn new(strict: bool, repo: &gix::Repository) -> Result<Self, String> {
        let mut overrides: Vec<(&'static str, Severity)> = Vec::new();
        for entry in crate::config::walk_config(repo) {
            let Some(msg_id) = entry.key.strip_prefix("fsck.") else {
                continue;
            };
            let Some(value) = entry.value.as_deref() else {
                eprintln!("error: missing value for '{}'", entry.key);
                return Err(entry.origin.die_linenr(&entry.key));
            };
            if msg_id == "skiplist" {
                let Some(path) = crate::setup::interpolate_path(value) else {
                    return Err(format!("failed to expand user dir in: '{value}'"));
                };
                fsck::read_skip_list(&path.to_string_lossy())?;
                continue;
            }
            // `parse_msg_id()` compares against each id's downcased spelling,
            // and the config parser has already lower-cased the variable name.
            let Some(msg) = fsck::MSGS.iter().find(|m| m.id.to_lowercase() == msg_id) else {
                return Err(format!("Unhandled message id: {msg_id}"));
            };
            // `largePathname` carries its limit after a colon (fsck.c:164-173).
            let mut type_str = value;
            if msg.id == fsck::LARGE_PATHNAME.id {
                if let Some((head, len)) = value.split_once(':') {
                    type_str = head;
                    if crate::optint::config_int64(len).is_err() {
                        return Err(format!("unable to parse max tree entry len: {len}"));
                    }
                }
            }
            let severity = fsck::parse_severity(type_str)?;
            if msg.default == Severity::Fatal && severity != Severity::Error {
                return Err(format!("Cannot demote {msg_id} to {type_str}"));
            }
            overrides.retain(|(id, _)| *id != msg.id);
            if ALL.iter().any(|m| m.id == msg.id) {
                overrides.push((msg.id, severity));
            }
        }

        Ok(Reporter {
            strict,
            overrides,
            failed: false,
        })
    }

    /// The severity one row is reported at.
    ///
    /// Unconfigured rows come from the table, folded the way `report()` folds
    /// them before the callback sees them: `FSCK_INFO` prints as a warning and
    /// `FSCK_FATAL` as an error. `extraHeaderEntry` is git's one explicit
    /// override — `cmd_mktag()` calls `fsck_set_msg_type_from_ids(…, FSCK_WARN)`
    /// on it, so it is a warning here rather than the table's `ignore`.
    fn severity(&self, msg: &Msg) -> Severity {
        if let Some((_, s)) = self.overrides.iter().find(|(id, _)| *id == msg.id) {
            return *s;
        }
        if msg.id == fsck::EXTRA_HEADER_ENTRY.id {
            return Severity::Warn;
        }
        match msg.default {
            Severity::Info => Severity::Warn,
            Severity::Fatal => Severity::Error,
            other => other,
        }
    }

    /// Emit one fsck message; `true` means "error", which aborts validation.
    ///
    /// `mktag`'s error callback turns a warning into an error whenever
    /// `--strict` is in effect, which is why a missing `tagger` line — a mere
    /// warning to `git fsck` — fails `mktag` by default.
    fn report(&mut self, msg: &Msg, text: &[u8]) -> bool {
        let severity = match self.severity(msg) {
            Severity::Ignore => return false,
            Severity::Warn if !self.strict => "warning",
            _ => "error",
        };
        let mut line =
            format!("{severity}: tag input does not pass fsck: {}: ", msg.id).into_bytes();
        line.extend_from_slice(text);
        line.push(b'\n');
        let _ = std::io::stderr().write_all(&line);

        if severity == "error" {
            self.failed = true;
            return true;
        }
        false
    }
}

/// Port of `fsck_tag_buf`: validate `buf`, yielding the tagged object and type.
///
/// Returns `None` when validation stopped before both the `object` and `type`
/// headers were understood. `reporter.failed` says whether the tag is rejected;
/// a `None` result with `failed == false` means a structural check was demoted
/// by config, which the caller turns into an honest error.
fn fsck_tag(
    buf: &[u8],
    hash: gix::hash::Kind,
    reporter: &mut Reporter,
) -> Option<(ObjectId, Option<Kind>)> {
    if verify_headers(buf, reporter) {
        return None;
    }

    let Some(rest) = buf.strip_prefix(b"object ") else {
        reporter.report(&fsck::MISSING_OBJECT, b"invalid format - expected 'object' line");
        return None;
    };

    // `parse_oid_hex` wants exactly one hash worth of hex — any case — and the
    // line must end right after it.
    let hex_len = hash.len_in_hex();
    let tagged_oid = match rest.get(..hex_len) {
        Some(hex) if hex.iter().all(u8::is_ascii_hexdigit) && rest.get(hex_len) == Some(&b'\n') => {
            ObjectId::from_hex(&hex.to_ascii_lowercase()).ok()
        }
        _ => None,
    };
    let Some(tagged_oid) = tagged_oid else {
        reporter.report(
            &fsck::BAD_OBJECT_SHA1,
            b"invalid 'object' line format - bad sha1",
        );
        return None;
    };
    let rest = &rest[hex_len + 1..];

    let Some(rest) = rest.strip_prefix(b"type ") else {
        reporter.report(&fsck::MISSING_TYPE_ENTRY, b"invalid format - expected 'type' line");
        return None;
    };
    let Some(eol) = rest.iter().position(|b| *b == b'\n') else {
        reporter.report(
            &fsck::MISSING_TYPE,
            b"invalid format - unexpected end after 'type' line",
        );
        return None;
    };
    // `*tagged_type = type_from_string_gently(…)` is left at `OBJ_BAD` for a word
    // that is no type, and only a `report()` answering 1 stops the parse
    // (fsck.c:1061-1065): with `fsck.badType` demoted the walk goes on, and
    // `verify_object_in_tag()` meets the bad type. `None` stands for `OBJ_BAD`.
    let tagged_kind = Kind::from_bytes(&rest[..eol]).ok();
    if tagged_kind.is_none() && reporter.report(&fsck::BAD_TYPE, b"invalid 'type' value") {
        return None;
    }
    let rest = &rest[eol + 1..];

    // From here on the tagged object is known, so later demotions can continue.
    let found = Some((tagged_oid, tagged_kind));

    let Some(rest) = rest.strip_prefix(b"tag ") else {
        reporter.report(&fsck::MISSING_TAG_ENTRY, b"invalid format - expected 'tag' line");
        return found;
    };
    let Some(eol) = rest.iter().position(|b| *b == b'\n') else {
        reporter.report(
            &fsck::MISSING_TAG,
            // git's text names the 'type' line here too (fsck.c:1074).
            b"invalid format - unexpected end after 'type' line",
        );
        return found;
    };
    let name = &rest[..eol];
    let mut refname = b"refs/tags/".to_vec();
    refname.extend_from_slice(name);
    if gix::validate::reference::name(refname.as_slice().into()).is_err() {
        let mut text = b"invalid 'tag' name: ".to_vec();
        text.extend_from_slice(name);
        if reporter.report(&fsck::BAD_TAG_NAME, &text) {
            return found;
        }
    }
    let rest = &rest[eol + 1..];

    // Early tags carry no `tagger` line; git only warns, `mktag` promotes it.
    // Every check before this one stops at its first error, so `ret` is 0 here.
    let (rest, ident_failed) = match rest.strip_prefix(b"tagger ") {
        None => {
            if reporter.report(
                &fsck::MISSING_TAGGER_ENTRY,
                b"invalid format - expected 'tagger' line",
            ) {
                return found;
            }
            (rest, false)
        }
        Some(ident) => {
            // The line is consumed whether or not it validates.
            let after = match ident.iter().position(|b| *b == b'\n') {
                Some(nl) => &ident[nl + 1..],
                None => &ident[ident.len()..],
            };
            // `else ret = fsck_ident(&buffer, …);` (fsck.c:1094-1095) is not acted on
            // there: the `gpgsig` walk and the extra-header check still run.
            let failed = match fsck_ident(ident) {
                Some((msg, text)) => reporter.report(msg, text.as_bytes()),
                None => false,
            };
            (after, failed)
        }
    };

    // A `gpgsig` / `gpgsig-sha256` header and its ` `-indented continuation lines
    // are skipped (fsck.c:1097-1112). Their `badGpgsig` / `badHeaderContinuation`
    // for a line with no newline cannot be reached: `verify_headers()` above has
    // already refused a header block that does not end in one.
    let mut rest = rest;
    fn next_line(s: &[u8]) -> &[u8] {
        s.iter().position(|b| *b == b'\n').map_or(&s[s.len()..], |nl| &s[nl + 1..])
    }
    if let Some(sig) = rest.strip_prefix(b"gpgsig ").or_else(|| rest.strip_prefix(b"gpgsig-sha256 ")) {
        rest = next_line(sig);
        while rest.starts_with(b" ") {
            rest = next_line(rest);
        }
    }

    // A message, when present, is separated by a blank line. Anything else here
    // is a header git's `verify_headers` would have waved through, and `mktag`
    // rejects it. Its `ret = report(…)` replaces the ident's verdict
    // (fsck.c:1114-1125), so under `--no-strict`, where the extra header is only a
    // warning, a bad tagger line no longer fails the tag.
    let mut failed = ident_failed;
    if !rest.is_empty() && !rest.starts_with(b"\n") {
        failed = reporter.report(
            &fsck::EXTRA_HEADER_ENTRY,
            b"invalid format - extra header(s) after 'tagger'",
        );
    }
    reporter.failed = failed;
    found
}

/// Port of `verify_headers`: no NUL anywhere, and the header block must end.
///
/// Returns `true` when validation must stop.
fn verify_headers(buf: &[u8], reporter: &mut Reporter) -> bool {
    for (i, b) in buf.iter().enumerate() {
        match b {
            0 => {
                return reporter.report(
                    &fsck::NUL_IN_HEADER,
                    format!("unterminated header: NUL at offset {i}").as_bytes(),
                )
            }
            b'\n' if buf.get(i + 1) == Some(&b'\n') => return false,
            _ => {}
        }
    }
    // No blank line means no message body, which is fine — but the last header
    // line still has to be terminated.
    if buf.last() == Some(&b'\n') {
        return false;
    }
    reporter.report(&fsck::UNTERMINATED_HEADER, b"unterminated header")
}

/// Port of `fsck_ident`, applied to the text following `tagger `.
///
/// Returns the first problem found, in git's order, or `None` when the identity
/// is well formed. The date is checked exactly as git checks it: no sign, no
/// leading zero unless the timestamp is a bare `0`, a value that fits `time_t`,
/// then a `[+-]HHMM` zone followed immediately by the newline.
fn fsck_ident(ident: &[u8]) -> Option<(&'static Msg, &'static str)> {
    const NAME_END: [u8; 3] = *b"<>\n";

    if ident.first() == Some(&b'<') {
        return Some((
            &fsck::MISSING_NAME_BEFORE_EMAIL,
            "invalid author/committer line - missing space before email",
        ));
    }
    let at = ident
        .iter()
        .position(|b| NAME_END.contains(b))
        .unwrap_or(ident.len());
    match ident.get(at) {
        Some(b'>') => {
            return Some((&fsck::BAD_NAME, "invalid author/committer line - bad name"));
        }
        Some(b'<') => {}
        _ => {
            return Some((&fsck::MISSING_EMAIL, "invalid author/committer line - missing email"));
        }
    }
    if at == 0 || ident[at - 1] != b' ' {
        return Some((
            &fsck::MISSING_SPACE_BEFORE_EMAIL,
            "invalid author/committer line - missing space before email",
        ));
    }

    let rest = &ident[at + 1..];
    let end = rest
        .iter()
        .position(|b| NAME_END.contains(b))
        .unwrap_or(rest.len());
    if rest.get(end) != Some(&b'>') {
        return Some((&fsck::BAD_EMAIL, "invalid author/committer line - bad email"));
    }
    let rest = &rest[end + 1..];

    if rest.first() != Some(&b' ') {
        return Some((
            &fsck::MISSING_SPACE_BEFORE_DATE,
            "invalid author/committer line - missing space before date",
        ));
    }
    let rest = &rest[1..];
    // "we'll continue to" allow extra linear whitespace before the timestamp, but
    // not a newline: `while (*p == ' ' || *p == '\t') p++;` (fsck.c:927-928).
    let rest = &rest[rest.iter().take_while(|b| matches!(b, b' ' | b'\t')).count()..];

    if rest.first() == Some(&b'0') && rest.get(1) != Some(&b' ') {
        return Some((
            &fsck::ZERO_PADDED_DATE,
            "invalid author/committer line - zero-padded date",
        ));
    }
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Some((&fsck::BAD_DATE, "invalid author/committer line - bad date"));
    }
    // `date_overflows` rejects anything that will not round-trip through time_t.
    let seconds = std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    if seconds.is_none_or(|s| s > i64::MAX as u64) {
        return Some((
            &fsck::BAD_DATE_OVERFLOW,
            "invalid author/committer line - date causes integer overflow",
        ));
    }
    if rest.get(digits) != Some(&b' ') {
        return Some((&fsck::BAD_DATE, "invalid author/committer line - bad date"));
    }
    let zone = &rest[digits + 1..];

    let well_formed = matches!(zone.first(), Some(b'+' | b'-'))
        && zone.len() >= 6
        && zone[1..5].iter().all(u8::is_ascii_digit)
        && zone[5] == b'\n';
    if !well_formed {
        return Some((
            &fsck::BAD_TIMEZONE,
            "invalid author/committer line - bad time zone",
        ));
    }
    None
}
