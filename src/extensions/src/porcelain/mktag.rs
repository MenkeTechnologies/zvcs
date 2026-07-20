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
//! (`ignore`/`warn`/`error`/`info`), and finally `mktag`'s own rule that a
//! warning is reported as an error while `--strict` is in effect (the default).
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
        let Some(long) = a.strip_prefix("--") else {
            let flag = a[1..].chars().next().expect("`-` alone was handled above");
            eprintln!("error: unknown switch `{flag}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        };
        if long.is_empty() {
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

    let repo = gix::discover(".")?;

    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("could not read from stdin: {e}"))?;

    let mut reporter = Reporter::new(strict, &repo)?;
    let tagged = fsck_tag(&buf, repo.object_hash(), &mut reporter);

    if reporter.failed {
        return fatal("tag on stdin did not pass our strict fsck check");
    }
    let Some((tagged_oid, tagged_kind)) = tagged else {
        // Only reachable when config demoted one of the structural checks that
        // git nevertheless treats as fatal to the parse; git would go on to use
        // an uninitialised object id, which is not something to imitate.
        bail!("tag on stdin has no usable 'object'/'type' header after fsck demotion");
    };

    // `verify_object_in_tag`: the tagged object must exist and have that type.
    let Ok(header) = repo.find_header(tagged_oid) else {
        return fatal(&format!("could not read tagged object '{tagged_oid}'"));
    };
    if header.kind() != tagged_kind {
        return fatal(&format!(
            "object '{tagged_oid}' tagged as '{tagged_kind}', but is a '{}' type",
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

/// The fsck messages `mktag` can emit, named exactly as git's `camelcased` ids.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Msg {
    NulInHeader,
    UnterminatedHeader,
    MissingObject,
    BadObjectSha1,
    MissingTypeEntry,
    MissingType,
    BadType,
    MissingTagEntry,
    MissingTag,
    BadTagName,
    MissingTaggerEntry,
    MissingNameBeforeEmail,
    BadName,
    MissingEmail,
    MissingSpaceBeforeEmail,
    BadEmail,
    MissingSpaceBeforeDate,
    ZeroPaddedDate,
    BadDateOverflow,
    BadDate,
    BadTimezone,
    ExtraHeaderEntry,
}

impl Msg {
    /// The id git prints ahead of the message text, and the `fsck.<id>` key.
    fn id(self) -> &'static str {
        match self {
            Msg::NulInHeader => "nulInHeader",
            Msg::UnterminatedHeader => "unterminatedHeader",
            Msg::MissingObject => "missingObject",
            Msg::BadObjectSha1 => "badObjectSha1",
            Msg::MissingTypeEntry => "missingTypeEntry",
            Msg::MissingType => "missingType",
            Msg::BadType => "badType",
            Msg::MissingTagEntry => "missingTagEntry",
            Msg::MissingTag => "missingTag",
            Msg::BadTagName => "badTagName",
            Msg::MissingTaggerEntry => "missingTaggerEntry",
            Msg::MissingNameBeforeEmail => "missingNameBeforeEmail",
            Msg::BadName => "badName",
            Msg::MissingEmail => "missingEmail",
            Msg::MissingSpaceBeforeEmail => "missingSpaceBeforeEmail",
            Msg::BadEmail => "badEmail",
            Msg::MissingSpaceBeforeDate => "missingSpaceBeforeDate",
            Msg::ZeroPaddedDate => "zeroPaddedDate",
            Msg::BadDateOverflow => "badDateOverflow",
            Msg::BadDate => "badDate",
            Msg::BadTimezone => "badTimezone",
            Msg::ExtraHeaderEntry => "extraHeaderEntry",
        }
    }

    /// Git's built-in severity, before `fsck.<id>` config and before `--strict`.
    fn default_severity(self) -> Severity {
        match self {
            Msg::BadTagName | Msg::MissingTaggerEntry | Msg::ExtraHeaderEntry => Severity::Warn,
            _ => Severity::Error,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ignore,
    Warn,
    Error,
}

/// Applies severities and prints fsck messages the way `mktag` does.
struct Reporter {
    strict: bool,
    /// `fsck.<id>` overrides, keyed by git's camel-cased message id.
    overrides: Vec<(&'static str, Severity)>,
    /// Set once anything has been reported at error severity.
    failed: bool,
}

impl Reporter {
    /// Read the `fsck.<msgId>` overrides that apply to the tag messages.
    ///
    /// Config keys are matched case-insensitively by `gix-config`, as git
    /// matches them. `info` is folded into `warn`, which is what `report()`
    /// does before handing the message to the error callback.
    fn new(strict: bool, repo: &gix::Repository) -> Result<Self> {
        const ALL: [Msg; 22] = [
            Msg::NulInHeader,
            Msg::UnterminatedHeader,
            Msg::MissingObject,
            Msg::BadObjectSha1,
            Msg::MissingTypeEntry,
            Msg::MissingType,
            Msg::BadType,
            Msg::MissingTagEntry,
            Msg::MissingTag,
            Msg::BadTagName,
            Msg::MissingTaggerEntry,
            Msg::MissingNameBeforeEmail,
            Msg::BadName,
            Msg::MissingEmail,
            Msg::MissingSpaceBeforeEmail,
            Msg::BadEmail,
            Msg::MissingSpaceBeforeDate,
            Msg::ZeroPaddedDate,
            Msg::BadDateOverflow,
            Msg::BadDate,
            Msg::BadTimezone,
            Msg::ExtraHeaderEntry,
        ];

        let snapshot = repo.config_snapshot();
        let mut overrides = Vec::new();
        for msg in ALL {
            let Some(value) = snapshot.string(format!("fsck.{}", msg.id()).as_str()) else {
                continue;
            };
            let severity = match value.as_slice() {
                b"ignore" => Severity::Ignore,
                b"warn" | b"info" => Severity::Warn,
                b"error" => Severity::Error,
                other => bail!(
                    "Unknown fsck message type: '{}'",
                    String::from_utf8_lossy(other)
                ),
            };
            overrides.push((msg.id(), severity));
        }
        drop(snapshot);

        Ok(Reporter {
            strict,
            overrides,
            failed: false,
        })
    }

    fn severity(&self, msg: Msg) -> Severity {
        self.overrides
            .iter()
            .find(|(id, _)| *id == msg.id())
            .map_or_else(|| msg.default_severity(), |(_, s)| *s)
    }

    /// Emit one fsck message; `true` means "error", which aborts validation.
    ///
    /// `mktag`'s error callback turns a warning into an error whenever
    /// `--strict` is in effect, which is why a missing `tagger` line — a mere
    /// warning to `git fsck` — fails `mktag` by default.
    fn report(&mut self, msg: Msg, text: &[u8]) -> bool {
        let severity = match self.severity(msg) {
            Severity::Ignore => return false,
            Severity::Warn if !self.strict => "warning",
            _ => "error",
        };
        let mut line = format!("{severity}: tag input does not pass fsck: {}: ", msg.id())
            .into_bytes();
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
) -> Option<(ObjectId, Kind)> {
    if verify_headers(buf, reporter) {
        return None;
    }

    let Some(rest) = buf.strip_prefix(b"object ") else {
        reporter.report(Msg::MissingObject, b"invalid format - expected 'object' line");
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
            Msg::BadObjectSha1,
            b"invalid 'object' line format - bad sha1",
        );
        return None;
    };
    let rest = &rest[hex_len + 1..];

    let Some(rest) = rest.strip_prefix(b"type ") else {
        reporter.report(Msg::MissingTypeEntry, b"invalid format - expected 'type' line");
        return None;
    };
    let Some(eol) = rest.iter().position(|b| *b == b'\n') else {
        reporter.report(
            Msg::MissingType,
            b"invalid format - unexpected end after 'type' line",
        );
        return None;
    };
    let Ok(tagged_kind) = Kind::from_bytes(&rest[..eol]) else {
        reporter.report(Msg::BadType, b"invalid 'type' value");
        return None;
    };
    let rest = &rest[eol + 1..];

    // From here on the tagged object is known, so later demotions can continue.
    let found = Some((tagged_oid, tagged_kind));

    let Some(rest) = rest.strip_prefix(b"tag ") else {
        reporter.report(Msg::MissingTagEntry, b"invalid format - expected 'tag' line");
        return found;
    };
    let Some(eol) = rest.iter().position(|b| *b == b'\n') else {
        reporter.report(
            Msg::MissingTag,
            b"invalid format - unexpected end after 'tag' line",
        );
        return found;
    };
    let name = &rest[..eol];
    let mut refname = b"refs/tags/".to_vec();
    refname.extend_from_slice(name);
    if gix::validate::reference::name(refname.as_slice().into()).is_err() {
        let mut text = b"invalid 'tag' name: ".to_vec();
        text.extend_from_slice(name);
        if reporter.report(Msg::BadTagName, &text) {
            return found;
        }
    }
    let rest = &rest[eol + 1..];

    // Early tags carry no `tagger` line; git only warns, `mktag` promotes it.
    let rest = match rest.strip_prefix(b"tagger ") {
        None => {
            if reporter.report(
                Msg::MissingTaggerEntry,
                b"invalid format - expected 'tagger' line",
            ) {
                return found;
            }
            rest
        }
        Some(ident) => {
            // The line is consumed whether or not it validates.
            let after = match ident.iter().position(|b| *b == b'\n') {
                Some(nl) => &ident[nl + 1..],
                None => &ident[ident.len()..],
            };
            if let Some((msg, text)) = fsck_ident(ident) {
                if reporter.report(msg, text.as_bytes()) {
                    return found;
                }
            }
            after
        }
    };

    // A message, when present, is separated by a blank line. Anything else here
    // is a header git's `verify_headers` would have waved through, and `mktag`
    // rejects it.
    if !rest.is_empty() && !rest.starts_with(b"\n") {
        reporter.report(
            Msg::ExtraHeaderEntry,
            b"invalid format - extra header(s) after 'tagger'",
        );
    }
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
                    Msg::NulInHeader,
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
    reporter.report(Msg::UnterminatedHeader, b"unterminated header")
}

/// Port of `fsck_ident`, applied to the text following `tagger `.
///
/// Returns the first problem found, in git's order, or `None` when the identity
/// is well formed. The date is checked exactly as git checks it: no sign, no
/// leading zero unless the timestamp is a bare `0`, a value that fits `time_t`,
/// then a `[+-]HHMM` zone followed immediately by the newline.
fn fsck_ident(ident: &[u8]) -> Option<(Msg, &'static str)> {
    const NAME_END: [u8; 3] = [b'<', b'>', b'\n'];

    if ident.first() == Some(&b'<') {
        return Some((
            Msg::MissingNameBeforeEmail,
            "invalid author/committer line - missing space before email",
        ));
    }
    let at = ident
        .iter()
        .position(|b| NAME_END.contains(b))
        .unwrap_or(ident.len());
    match ident.get(at) {
        Some(b'>') => {
            return Some((Msg::BadName, "invalid author/committer line - bad name"));
        }
        Some(b'<') => {}
        _ => {
            return Some((Msg::MissingEmail, "invalid author/committer line - missing email"));
        }
    }
    if at == 0 || ident[at - 1] != b' ' {
        return Some((
            Msg::MissingSpaceBeforeEmail,
            "invalid author/committer line - missing space before email",
        ));
    }

    let rest = &ident[at + 1..];
    let end = rest
        .iter()
        .position(|b| NAME_END.contains(b))
        .unwrap_or(rest.len());
    if rest.get(end) != Some(&b'>') {
        return Some((Msg::BadEmail, "invalid author/committer line - bad email"));
    }
    let rest = &rest[end + 1..];

    if rest.first() != Some(&b' ') {
        return Some((
            Msg::MissingSpaceBeforeDate,
            "invalid author/committer line - missing space before date",
        ));
    }
    let rest = &rest[1..];

    if rest.first() == Some(&b'0') && rest.get(1) != Some(&b' ') {
        return Some((
            Msg::ZeroPaddedDate,
            "invalid author/committer line - zero-padded date",
        ));
    }
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Some((Msg::BadDate, "invalid author/committer line - bad date"));
    }
    // `date_overflows` rejects anything that will not round-trip through time_t.
    let seconds = std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    if seconds.is_none_or(|s| s > i64::MAX as u64) {
        return Some((
            Msg::BadDateOverflow,
            "invalid author/committer line - date causes integer overflow",
        ));
    }
    if rest.get(digits) != Some(&b' ') {
        return Some((Msg::BadDate, "invalid author/committer line - bad date"));
    }
    let zone = &rest[digits + 1..];

    let well_formed = matches!(zone.first(), Some(b'+' | b'-'))
        && zone.len() >= 6
        && zone[1..5].iter().all(u8::is_ascii_digit)
        && zone[5] == b'\n';
    if !well_formed {
        return Some((
            Msg::BadTimezone,
            "invalid author/committer line - bad time zone",
        ));
    }
    None
}
