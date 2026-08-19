//! `git verify-tag` — check the GPG signature of tag objects.
//!
//! Mirrors stock `git verify-tag` (builtin/verify-tag.c + gpg-interface.c),
//! which does not verify signatures itself: it splits the tag object into
//! payload and signature, then hands both to `gpg` and passes gpg's own output
//! straight through. This port does the same, so the human-readable text is
//! byte-identical by construction.
//!
//! Implemented:
//!   * `git verify-tag <tag>...`   → verify each, stderr carries gpg's output
//!   * `-v` / `--verbose`          → write the tag payload to stdout
//!   * `--raw`                     → emit gpg's `--status-fd` lines instead
//!   * `--format=<fmt>` / `--format <fmt>` / `--no-format` → render the tag
//!     through [`super::ref_filter::pretty_print_ref`] after a successful
//!     verification only, and suppress gpg's own output — including the `-v`
//!     payload — the way git's `GPG_VERIFY_OMIT_STATUS` does. That is the same
//!     `ref-filter.c` evaluator `for-each-ref`, `branch --format` and `tag
//!     --format` run on, so the whole atom set, the `%(if)`/`%(align)`
//!     containers and every date modifier are available. The ref it renders is
//!     built the way `pretty_print_ref()` builds one: refname is the operand
//!     *as typed*, so `%(refname)` prints `v1.0` rather than `refs/tags/v1.0`
//!     and `%(refname:lstrip=2)` prints nothing; the flag word is zero, so
//!     `%(flag)` and `%(symref)` are empty.
//!   * `gpg.minTrustLevel` → the tag is rejected unless gpg's status stream
//!     reports a trust level at or above the configured minimum, matching
//!     gpg-interface.c's `status |= sigc->trust_level < configured_min_trust_level`
//!   * `--no-verbose`, `--no-raw`, `--`, `-h`
//!   * the pre-gpg failure paths, verbatim: unresolvable name, non-tag object,
//!     and a tag carrying no signature block
//!
//! Exit codes match git: 0 when every named tag verified, 1 when any failed,
//! 129 for usage errors.
//!
//! `git tag -v` shares this checker through [`verify_resolved`] rather than
//! reimplementing it, because the two commands differ only in how a name becomes an
//! object id and in whether the payload is printed by default: `verify_tag()`
//! (builtin/tag.c:142-159) resolves `refs/tags/<name>` alone and passes
//! `GPG_VERIFY_VERBOSE`, while `cmd_verify_tag()` goes through `repo_get_oid()` and
//! passes it only under `-v`. Everything from the object-type check downwards is
//! byte-for-byte the same code.
//!
//! All three signature formats are covered, because the backend is chosen the
//! way git chooses it — off the armor header, by `get_format_by_sig()` — and all
//! three drivers live in [`crate::gitsig`]: `gpg` for `PGP SIGNATURE`/`PGP
//! MESSAGE`, `gpgsm` for `SIGNED MESSAGE`, and `ssh-keygen -Y` for `SSH
//! SIGNATURE`. Each one's own report is what reaches stderr, so the text is
//! byte-identical by construction for all three rather than only for OpenPGP.
//!
//! The format is verified once, up front, at git's own position
//! (builtin/verify-tag.c:48-53) rather than per tag, so `fatal: unknown field
//! name: <name>` at 128 and the malformed-`%(` usage block at 129 both land
//! before the first signature is checked.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::objs::Kind;

/// The parse-options usage block, byte-for-byte as git 2.55 emits it.
/// `cmd_verify_tag()`'s `struct option verify_tag_options[]`
/// (builtin/verify-tag.c), in table order, as [`super::resolve_long`] reads it.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "raw",                         neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "format",                      neg: true,  arg: super::Arg::Required },
];

const USAGE: &str = "\
usage: git verify-tag [-v | --verbose] [--format=<format>] [--raw] <tag>...

    -v, --[no-]verbose    print tag contents
    --[no-]raw            print raw gpg status output
    --[no-]format <format>
                          format to use for the output

";

/// Signature block openers git recognises, with the signing backend each implies.
const SIG_MARKERS: &[(&str, SigKind)] = &[
    ("-----BEGIN PGP SIGNATURE-----", SigKind::OpenPgp),
    ("-----BEGIN PGP MESSAGE-----", SigKind::OpenPgp),
    ("-----BEGIN SIGNED MESSAGE-----", SigKind::X509),
    ("-----BEGIN SSH SIGNATURE-----", SigKind::Ssh),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SigKind {
    OpenPgp,
    X509,
    Ssh,
}

pub fn verify_tag(args: &[String]) -> Result<ExitCode> {
    let mut verbose = false;
    let mut raw = false;
    let mut format: Option<&str> = None;
    let mut names: Vec<&str> = Vec::new();
    let mut operands_only = false;

    let mut i = 0;
    while i < args.len() {
        let typed = args[i].as_str();
        let a = typed;
        i += 1;

        if operands_only || !a.starts_with('-') || a == "-" {
            names.push(typed);
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why it is absent from [`LONG_OPTS`] and
        // matched here rather than after resolution. verify-tag's table has no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block `-h`
        // prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match a {
            "--" => operands_only = true,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--raw" => raw = true,
            "--no-raw" => raw = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `OPT_STRING`: the separate-argument spelling swallows the next
            // argv entry even when that entry looks like an operand, and
            // running out of arguments is git's own "requires a value" error.
            "--format" => match args.get(i) {
                Some(v) => {
                    format = Some(v.as_str());
                    i += 1;
                }
                None => {
                    eprintln!("error: option `format' requires a value");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            },
            "--no-format" => format = None,
            // The value is sliced out of the token as typed: the resolver copies it
            // through verbatim, and only that copy borrows from `args`, which is what
            // `format` holds on to past the loop.
            _ if a.starts_with("--format=") => {
                format = typed.split_once('=').map(|(_, v)| v)
            }
            _ => {
                // git's parse-options wording, then the usage block.
                let (kind, name) = match a.strip_prefix("--") {
                    Some(long) => ("option", long),
                    None => ("switch", &a[1..]),
                };
                eprintln!("error: unknown {kind} `{name}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    // git checks for missing operands before it validates the format, so a
    // format that is both operand-less and malformed reports only the usage.
    if names.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // `verify_ref_format` runs once, up front, and its failure is this verb's own
    // usage error rather than a per-tag one:
    //
    // ```c
    // if (format.format) {
    //         if (verify_ref_format(&format))
    //                 usage_with_options(verify_tag_usage, verify_tag_options);
    //         flags |= GPG_VERIFY_OMIT_STATUS;
    // }
    // ```
    // (builtin/verify-tag.c:48-53)
    let format = match format
        .map(|f| super::ref_filter::parse_one_format(&repo, f, USAGE))
        .transpose()
    {
        Ok(f) => f,
        Err(code) => return Ok(code),
    };

    let mut had_error = false;
    for name in names {
        if !verify_one(&repo, name, verbose, raw, format.as_deref())? {
            had_error = true;
        }
    }

    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Verify a single named tag. Returns `false` when git would count it as a
/// failure; diagnostics go to stderr exactly as git words them.
fn verify_one(
    repo: &gix::Repository,
    name: &str,
    verbose: bool,
    raw: bool,
    format: Option<&[super::for_each_ref::Item]>,
) -> Result<bool> {
    // `repo_get_oid` is `get_oid_basic()`: a full-length hex name *is* the id,
    // decoded without consulting the odb, so an absent-but-well-formed name
    // reaches `gpg_verify_tag` and fails there, not here.
    let Some(id) = crate::objname::resolve(repo, name) else {
        eprintln!("error: tag '{name}' not found.");
        return Ok(false);
    };
    verify_resolved(repo, name, id, verbose, raw, format)
}

/// `gpg_verify_tag()` (tag.c:47-74) plus the `--format` rendering its two callers
/// bolt on: everything from the object-type check downwards, for an object id the
/// caller already has.
///
/// `git verify-tag` reaches this through `repo_get_oid()`, `git tag -v` through a
/// `refs/tags/<name>` lookup — the two disagree about which names resolve, but not
/// about a single byte of what follows, which is why the split is here.
pub(crate) fn verify_resolved(
    repo: &gix::Repository,
    name: &str,
    id: gix::hash::ObjectId,
    verbose: bool,
    raw: bool,
    format: Option<&[super::for_each_ref::Item]>,
) -> Result<bool> {
    // git asks `oid_object_info` for the type first; a missing object yields no
    // type name at all, which its `error()` renders as `(null)`.
    let Ok(object) = repo.find_object(id) else {
        eprintln!("error: {name}: cannot verify a non-tag object of type (null).");
        return Ok(false);
    };
    if object.kind != Kind::Tag {
        eprintln!(
            "error: {name}: cannot verify a non-tag object of type {}.",
            object.kind
        );
        return Ok(false);
    }

    let Some((split, kind)) = split_signature(&object.data) else {
        // Unsigned: the whole object is the payload, and git still prints it
        // under -v before reporting the failure.
        if verbose {
            std::io::stdout().write_all(&object.data)?;
        }
        eprintln!("error: no signature found");
        return Ok(false);
    };
    let (payload, signature) = object.data.split_at(split);

    // `check_signature()` picks the backend off the armor header itself, so the
    // `SigKind` the split reported is only used to name the object's format —
    // every one of the three has a driver behind it.
    let _ = kind;
    // `gpg_interface_lazy_init()` is *lazy*: `gpg.minTrustLevel` is read by the
    // first `check_signature()`. A tag that is missing, is not a tag, or carries
    // no signature never reaches one, so an unparseable value stays unreported
    // and the command keeps the exit 1 those paths already earned — which is what
    // reading the key up front turned into a spurious 128.
    let min_trust = super::verify_commit::min_trust_level(repo)?;
    let sigc = crate::gitsig::verify_full(signature, payload);

    // `print_signature_buffer` runs after the check, and `--format` sets
    // GPG_VERIFY_OMIT_STATUS, which skips the whole thing — so under --format
    // even `-v` prints no payload here (an unsigned tag still does, above,
    // because that path returns before the omit-status gate).
    if format.is_none() {
        if verbose {
            std::io::stdout().write_all(payload)?;
        }
        // The checker's own report by default, or its `--status-fd` stream under
        // --raw; either way verbatim, on stderr.
        let shown = if raw { &sigc.gpg_status } else { &sigc.output };
        std::io::stderr().write_all(shown)?;
    }

    // `check_signature()`'s verdict, with `gpg.minTrustLevel` as its floor.
    let ok = sigc.verified(min_trust);

    // git renders the format only for tags that verified, through
    // `pretty_print_ref(name, &oid, NULL, &format)` — the same `ref-filter`
    // evaluator `for-each-ref`, `branch --format` and `tag --format` use, over a
    // one-item array built from the operand as typed.
    if let Some(items) = format.filter(|_| ok) {
        match super::ref_filter::pretty_print_ref(repo, name.as_bytes(), id, items)? {
            Ok(mut line) => {
                line.push(b'\n');
                std::io::stdout().write_all(&line)?;
            }
            // A formatting-stack error is `die("%s", err.buf)`, which ends the
            // whole command rather than this one tag.
            Err(_) => std::process::exit(128),
        }
    }

    Ok(ok)
}

/// git's `parse_signed_buffer`: the offset of the last line that starts a
/// signature block, or the buffer length when there is none.
pub(crate) fn parse_signed_buffer(buf: &[u8]) -> usize {
    let size = buf.len();
    let mut len = 0;
    let mut m = size;
    while len < size {
        if is_signature_start(&buf[len..]) {
            m = len;
        }
        len = match buf[len..].iter().position(|&b| b == b'\n') {
            Some(p) => len + p + 1,
            None => size,
        };
    }
    m
}

/// git's `get_format_by_sig`: whether `line` opens any known signature block.
fn is_signature_start(line: &[u8]) -> bool {
    const MARKERS: &[&[u8]] = &[
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN PGP MESSAGE-----",
        b"-----BEGIN SIGNED MESSAGE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    MARKERS.iter().any(|m| line.starts_with(m))
}

/// Byte offset at which the signature block starts, plus the backend it names.
///
/// Only a marker anchored at the start of a line counts, so a marker quoted
/// inside the tag message does not truncate the payload. The earliest such
/// marker wins, matching git's `parse_signature`.
fn split_signature(data: &[u8]) -> Option<(usize, SigKind)> {
    let mut best: Option<(usize, SigKind)> = None;
    for (marker, kind) in SIG_MARKERS {
        let Some(at) = find_at_line_start(data, marker.as_bytes()) else {
            continue;
        };
        match best {
            Some((prev, _)) if prev <= at => {}
            _ => best = Some((at, *kind)),
        }
    }
    best
}

/// First occurrence of `needle` in `haystack` that begins a line.
fn find_at_line_start(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&i| (i == 0 || haystack[i - 1] == b'\n') && &haystack[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `find_subpos` / `copy_subject` moved to the shared `ref-filter` port when
    // this module stopped carrying its own atom evaluator; the cases below still
    // pin the split, they just import it from where it now lives.
    use super::super::for_each_ref::{copy_subject, find_subpos};

    /// Wrap a message body in a minimal, well-formed annotated tag object.
    fn tag_bytes(body: &str) -> Vec<u8> {
        format!(
            "object {}\ntype commit\ntag v1\ntagger A U Thor <a@example.com> 1000000000 +0000\n\n{body}",
            "0".repeat(40),
        )
        .into_bytes()
    }

    #[test]
    fn find_subpos_splits_subject_body_signature() {
        let data = tag_bytes(
            "Subject line\n\nBody one\nBody two\n\
             -----BEGIN PGP SIGNATURE-----\nAAAA\n-----END PGP SIGNATURE-----\n",
        );
        let sub = find_subpos(&data);
        assert_eq!(
            data[sub.sub..sub.sub + sub.sub_len].to_vec(),
            b"Subject line".to_vec()
        );
        // git's contents:body keeps the newline that precedes the signature.
        assert_eq!(
            data[sub.body..sub.body + sub.nonsig_len].to_vec(),
            b"Body one\nBody two\n".to_vec()
        );
        assert_eq!(
            data[sub.sig..sub.sig + sub.sig_len].to_vec(),
            b"-----BEGIN PGP SIGNATURE-----\nAAAA\n-----END PGP SIGNATURE-----\n".to_vec()
        );
    }

    #[test]
    fn find_subpos_folds_multiline_subject() {
        let data = tag_bytes("first\nsecond\n\nbody\n-----BEGIN PGP SIGNATURE-----\nX\n");
        let sub = find_subpos(&data);
        assert_eq!(
            copy_subject(&data[sub.sub..sub.sub + sub.sub_len]),
            b"first second".to_vec()
        );
    }

    #[test]
    fn copy_subject_drops_cr_before_lf() {
        assert_eq!(copy_subject(b"a\r\nb"), b"a b".to_vec());
        assert_eq!(copy_subject(b"a\nb"), b"a b".to_vec());
    }

    #[test]
    fn parse_signed_buffer_finds_marker_line() {
        assert_eq!(parse_signed_buffer(b"hello\n-----BEGIN PGP SIGNATURE-----\nsig\n"), 6);
        assert_eq!(parse_signed_buffer(b"no marker\n"), b"no marker\n".len());
    }
}
