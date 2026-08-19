//! The `for-each-ref` atoms whose *value* — not whose acceptance — is easy to
//! get subtly wrong, and where a wrong value is silent.
//!
//! `for-each-ref --format=…` output is almost always consumed by a script, so an
//! atom that renders the wrong bytes at exit 0 is worse than one that refuses:
//! nothing reports it. Each group below pins a distinction that a plausible
//! implementation collapses.
//!
//!   * `%(body)` is `C_BODY_DEP` and `%(contents:body)` is `C_BODY`
//!     (ref-filter.c:2064-2076). They are the same span except that the first
//!     keeps a trailing signature block and the second does not. Treating them as
//!     synonyms is the natural mistake and it silently truncates or pads a body.
//!   * `%(contents:size)` is `strlen(subpos)` — from the *subject*, not from the
//!     end of the header block — so a message with extra blank lines after its
//!     header must not count them.
//!   * `%(contents:lines=<n>)` joins with `"\n    "`, a newline **and four
//!     spaces** (`append_lines`, ref-filter.c:1943-1961).
//!   * `%(authoremail:<opts>)` options are bits, not alternatives
//!     (`person_email_atom_parser`, ref-filter.c:781-802): `trim,mailmap` means
//!     both, and `mailmap` alone still keeps the angle brackets.
//!   * `%(authordate:iso8601-strict)` writes `Z` for a zero offset, not `+00:00`
//!     (`show_date`, date.c:346-357). Every timestamp in a UTC repository goes
//!     through that branch, so getting it wrong is wrong *everywhere* and right
//!     nowhere.
//!   * `%(trailers:…)` has a verbatim fast path and a re-rendering path
//!     (`format_trailers_from_commit`, trailer.c), and they disagree about
//!     folded continuation lines and about spacing.
//!   * `%(is-base:<committish>)` picks **one** ref out of the whole array before
//!     the sort, so it cannot be computed per ref.
//!   * `%(deltabase)` is the pack's delta base, which is a real object id for a
//!     deltified object and the null oid otherwise — "always null" passes a
//!     naive fixture and is a lie in a real repository.
//!
//! Every expectation below was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, comparing stdout,
//! stderr and exit status separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn bare(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fer-atoms-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "author@example.com"]);
        f.git(&["config", "user.name", "A U Thor"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// Commit `message` verbatim, with no `-m` cleanup of blank lines.
    fn commit_message(&self, message: &str) {
        let path = self.work.join(".commit-msg");
        std::fs::write(&path, message).unwrap();
        std::fs::write(self.work.join("a"), message.as_bytes()).unwrap();
        self.git(&["add", "a"]);
        self.git(&["commit", "-q", "--cleanup=verbatim", "-F", ".commit-msg"]);
        std::fs::remove_file(&path).unwrap();
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn rev_parse(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }

    /// A stand-in `gpg` that signs with a fixed block and verifies with fixed
    /// `--status-fd` lines. No keyring, no agent, no network — so the signature
    /// atoms are testable on a headless CI box, which a real key is not.
    ///
    /// Returns `None` when the script cannot be made executable, which is the
    /// only way this can be unavailable on a unix box.
    fn install_fake_gpg(&self) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.root.join("fakegpg");
        std::fs::write(&path, FAKE_GPG).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
        self.git(&["config", "gpg.program", path.to_str().unwrap()]);
        self.git(&["config", "user.signingKey", "DEADBEEFDEADBEEF"]);
        Some(path)
    }
}

const FAKE_GPG: &str = r#"#!/bin/sh
mode=verify
for a in "$@"; do
  case "$a" in
    -bsau|--sign|-s) mode=sign ;;
    --verify) mode=verify ;;
  esac
done
if [ "$mode" = sign ]; then
  cat > /dev/null
  echo "[GNUPG:] SIG_CREATED D 1 8 00 0 0" >&2
  printf -- '-----BEGIN PGP SIGNATURE-----\n\nfakesig\n-----END PGP SIGNATURE-----\n'
  exit 0
fi
cat > /dev/null
fd=1
prev=
for a in "$@"; do
  case "$prev" in --status-fd) fd=$a ;; esac
  prev=$a
done
{
  echo "[GNUPG:] NEWSIG parity@example.invalid"
  echo "[GNUPG:] GOODSIG DEADBEEFDEADBEEF parity <parity@example.invalid>"
  echo "[GNUPG:] VALIDSIG 0000000000000000000000000000000000000042 2023-11-14 1700000000 0 4 0 1 8 00 0000000000000000000000000000000000000042"
  echo "[GNUPG:] TRUST_ULTIMATE 0 pgp"
} >&$fd
echo 'gpg: Good signature from "parity <parity@example.invalid>"' >&2
exit 0
"#;

/// A commit message with a subject, a two-paragraph body, and a trailer block
/// whose last trailer is folded across two lines. The fold is the point: it is
/// the only thing that separates the verbatim trailer path from the re-rendering
/// one, and the only thing `unfold` acts on.
const MESSAGE: &str = "\
Subject line with: a colon
Continued subject line

Body para one
Body para two

Signed-off-by: A U Thor <author@example.com>
Acked-by: Rev Iewer <rev@example.com>
Helped-by: Some
  One Folded <folded@example.com>
";

// ---------------------------------------------------------------------------
// `%(body)` vs `%(contents:body)`, and `%(contents:signature)`
// ---------------------------------------------------------------------------

/// The distinction only shows on a *signed* object, which is why it survives so
/// easily: on an unsigned one the two atoms are identical, so a fixture without
/// a signature cannot catch the collapse.
///
/// It has to be a signed **tag**, not a signed commit: a commit carries its
/// signature in a `gpgsig` *header*, which `find_subpos()` never sees, so
/// `%(contents:signature)` on a signed commit is empty. Only a tag object keeps
/// the block inline in the message. Measured on stock git 2.55.0.
#[test]
fn body_keeps_a_tags_signature_block_and_contents_body_drops_it() {
    let f = Fixture::bare("body-vs-contents-body");
    if f.install_fake_gpg().is_none() {
        eprintln!(
            "SKIPPING body_keeps_a_tags_signature_block_and_contents_body_drops_it: \
             could not install the stand-in gpg (no executable bit?)"
        );
        return;
    }
    f.commit_message("plain subject\n\nplain body\n");
    let tagged = f
        .cmd(&["tag", "-s", "-m", "signed tag subject\n\nsigned tag body", "sgn"])
        .output()
        .unwrap();
    if !tagged.status.success() {
        eprintln!(
            "SKIPPING body_keeps_a_tags_signature_block_and_contents_body_drops_it: \
             the stand-in gpg was not accepted by `tag -s`: {}",
            String::from_utf8_lossy(&tagged.stderr)
        );
        return;
    }

    let r = "refs/tags/sgn";
    let body = f.stdout(&["for-each-ref", r, "--format=%(body)"]);
    let contents_body = f.stdout(&["for-each-ref", r, "--format=%(contents:body)"]);
    let sig = f.stdout(&["for-each-ref", r, "--format=%(contents:signature)"]);
    // Each rendering carries one record newline of its own; strip it before the
    // spans are compared, or the partition below is off by three bytes.
    let (body, contents_body, sig) = (
        body.strip_suffix('\n').unwrap(),
        contents_body.strip_suffix('\n').unwrap(),
        sig.strip_suffix('\n').unwrap(),
    );

    assert_eq!(contents_body, "signed tag body\n", "C_BODY stops at the signature");
    assert_eq!(
        sig,
        "-----BEGIN PGP SIGNATURE-----\n\nfakesig\n-----END PGP SIGNATURE-----\n",
        "C_SIG is exactly the block C_BODY dropped"
    );
    assert_eq!(
        body,
        format!("{contents_body}{sig}"),
        "C_BODY_DEP is the two spans back to back — the partition that makes \
         %(body) and %(contents:body) different atoms"
    );

    // The same object under `*`-deref: the tag peels to a commit, which has no
    // inline signature at all, so all three go empty rather than repeating the
    // tag's.
    let peeled = f.stdout(&["for-each-ref", r, "--format=[%(*contents:signature)]"]);
    assert_eq!(peeled, "[]\n");
}

/// A signed **commit** keeps its signature in a `gpgsig` header, so the contents
/// atoms never see it — while `%(signature:grade)` still reports `G`. The pair
/// is asserted together because "signature atoms are empty" and "there is no
/// signature" are different states and only one of them is true here.
#[test]
fn a_signed_commit_has_a_grade_but_no_inline_signature() {
    let f = Fixture::bare("signed-commit-header");
    if f.install_fake_gpg().is_none() {
        eprintln!(
            "SKIPPING a_signed_commit_has_a_grade_but_no_inline_signature: \
             could not install the stand-in gpg (no executable bit?)"
        );
        return;
    }
    f.commit_message("first\n");
    std::fs::write(f.work.join("a"), b"signed\n").unwrap();
    f.git(&["add", "a"]);
    let signed = f.cmd(&["commit", "-q", "-S", "-m", "sc subject"]).output().unwrap();
    if !signed.status.success() {
        eprintln!(
            "SKIPPING a_signed_commit_has_a_grade_but_no_inline_signature: \
             the stand-in gpg was not accepted by `commit -S`: {}",
            String::from_utf8_lossy(&signed.stderr)
        );
        return;
    }
    let got = f.stdout(&[
        "for-each-ref",
        "refs/heads/main",
        "--format=[%(signature:grade)][%(contents:signature)]",
    ]);
    assert_eq!(got, "[G][]\n");
}

/// `%(contents:signature)` on an *unsigned* commit is empty rather than absent,
/// so a format that always prints it does not change shape when a commit happens
/// not to be signed.
#[test]
fn contents_signature_is_empty_on_an_unsigned_commit() {
    let f = Fixture::bare("contents-signature-unsigned");
    f.commit_message("plain subject\n\nplain body\n");
    let (out, err, code) =
        f.run(&["for-each-ref", "refs/heads/main", "--format=[%(contents:signature)]"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("[]\n", "", 0));
}

// ---------------------------------------------------------------------------
// `%(contents:size)` and `%(contents)`
// ---------------------------------------------------------------------------

/// `find_subpos()` skips **every** blank line after the header block, not just
/// the one that ends it, and `C_LENGTH` measures from there. A message that
/// opens with extra blank lines is the only thing that separates the two rules,
/// and `--cleanup=verbatim` is what lets one be committed.
#[test]
fn contents_size_counts_from_the_subject_not_from_the_header() {
    let f = Fixture::bare("contents-size");
    f.commit_message("\n\n\nsubject after blanks\n\nbody\n");

    let size = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(contents:size)"]);
    let contents = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(contents)"]);
    let contents = contents.strip_suffix('\n').expect("one trailing record newline");

    assert!(
        contents.starts_with("subject after blanks"),
        "%(contents) starts at the subject, past the leading blanks; got {contents:?}"
    );
    assert_eq!(
        size.trim(),
        contents.len().to_string(),
        "%(contents:size) is the length of exactly what %(contents) prints"
    );
    assert_eq!(size.trim(), "27", "measured from stock git 2.55.0");
}

// ---------------------------------------------------------------------------
// `%(contents:lines=<n>)`
// ---------------------------------------------------------------------------

/// The join is `"\n    "`. A plain `"\n"` looks right in a one-line format and
/// is wrong for every `n > 1`; the four spaces are load-bearing.
#[test]
fn contents_lines_joins_with_a_newline_and_four_spaces() {
    let f = Fixture::bare("contents-lines");
    f.commit_message(MESSAGE);

    let one = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(contents:lines=1)"]);
    assert_eq!(one, "Subject line with: a colon\n");

    let three = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(contents:lines=3)"]);
    assert_eq!(
        three, "Subject line with: a colon\n    Continued subject line\n    \n",
        "lines 2 and 3 are indented by four spaces, and line 3 is the blank one"
    );

    // `n` past the end stops at the end rather than padding, and the indent is
    // added *in front of* whatever indent the line already had — the folded
    // trailer comes out with six spaces, not four.
    let huge = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(contents:lines=99)"]);
    let indented: String = MESSAGE
        .strip_suffix('\n')
        .unwrap()
        .split('\n')
        .collect::<Vec<_>>()
        .join("\n    ");
    assert_eq!(huge, format!("{indented}\n"));

    // Zero lines is legal and renders nothing — it is not a "positive value"
    // rejection, because `strtoul_ui` accepts 0.
    let zero = f.stdout(&["for-each-ref", "refs/heads/main", "--format=[%(contents:lines=0)]"]);
    assert_eq!(zero, "[]\n");
}

/// `strtoul_ui()` refuses a `-`, trailing junk, and anything that overflows an
/// `unsigned int`, and the message names the atom rather than the option list.
#[test]
fn contents_lines_rejects_a_non_count_with_its_own_message() {
    let f = Fixture::bare("contents-lines-bad");
    f.commit_message("subject\n");
    for bad in ["", "x", "-1", "1x", "999999999999"] {
        let (out, err, code) =
            f.run(&["for-each-ref", &format!("--format=%(contents:lines={bad})")]);
        assert_eq!(out, "", "nothing is printed for contents:lines={bad}");
        assert_eq!(
            err,
            format!("fatal: positive value expected contents:lines={bad}\n"),
            "contents:lines={bad} has its own message, not the generic one"
        );
        assert_eq!(code, 128);
    }
    // The control: a value that *is* a count is accepted, so the test above is
    // not passing merely because everything is rejected.
    let (_, err, code) = f.run(&["for-each-ref", "--format=%(contents:lines=2)"]);
    assert_eq!((err.as_str(), code), ("", 0));
}

// ---------------------------------------------------------------------------
// `%(subject:sanitize)`
// ---------------------------------------------------------------------------

/// `copy_subject()` (ref-filter.c:1659-1674) turns each `\n` into **one** space
/// and drops the `\r` of a `\r\n`; it does not trim a line's trailing
/// whitespace and does not collapse runs of spaces. Folding by
/// "right-trim each line, join with a space" reads identically on a tidy
/// message and silently rewrites a scrappy one.
#[test]
fn a_multi_line_subject_is_folded_without_trimming_interior_whitespace() {
    let f = Fixture::bare("subject-fold");
    f.commit_message("first   \nsecond\t\r\nthird  \n\nbody\n");
    for atom in ["%(subject)", "%(contents:subject)"] {
        let got = f.stdout(&["for-each-ref", "refs/heads/main", &format!("--format={atom}")]);
        assert_eq!(
            got, "first    second\t third  \n",
            "{atom}: the trailing run on each line survives, the CR of the CRLF \
             does not, and each newline becomes exactly one space"
        );
    }
}

/// `format_sanitized_subject()` collapses runs of non-title characters to one
/// `-`, never *leads* with one, squeezes a run of `.` after a `.`, and trims
/// trailing `.`/`-`. The `space = 2` initial value is what stops a leading
/// separator, and dropping it is invisible until the subject starts with
/// punctuation.
#[test]
fn subject_sanitize_collapses_separators_without_leading_or_trailing_ones() {
    let f = Fixture::bare("subject-sanitize");
    for (subject, want) in [
        ("Subject line with: a colon", "Subject-line-with-a-colon"),
        ("  leading spaces", "leading-spaces"),
        ("trailing punctuation!!!", "trailing-punctuation"),
        ("dots...and.more", "dots.and.more"),
        ("under_scores kept", "under_scores-kept"),
        ("---", ""),
        ("a", "a"),
    ] {
        f.commit_message(&format!("{subject}\n\nbody\n"));
        let got = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(subject:sanitize)"]);
        assert_eq!(got, format!("{want}\n"), "sanitizing {subject:?}");
    }
}

// ---------------------------------------------------------------------------
// `%(authoremail:<opts>)` and `:mailmap`
// ---------------------------------------------------------------------------

/// The options are OR-ed bits. `mailmap` on its own keeps the angle brackets;
/// combined with `trim` or `localpart` it does not. An implementation that
/// treats them as an enum gets exactly one of these four right.
#[test]
fn email_options_are_bits_and_mailmap_alone_keeps_the_brackets() {
    let f = Fixture::bare("email-bits");
    std::fs::write(
        f.work.join(".mailmap"),
        b"Proper Name <proper@example.com> <author@example.com>\n",
    )
    .unwrap();
    f.commit_message("subject\n");
    f.git(&["config", "mailmap.file", ".mailmap"]);

    for (spec, want) in [
        ("authorname", "A U Thor"),
        ("authorname:mailmap", "Proper Name"),
        ("authoremail", "<author@example.com>"),
        ("authoremail:mailmap", "<proper@example.com>"),
        ("authoremail:trim", "author@example.com"),
        ("authoremail:trim,mailmap", "proper@example.com"),
        ("authoremail:mailmap,trim", "proper@example.com"),
        ("authoremail:localpart", "author"),
        ("authoremail:localpart,mailmap", "proper"),
        // The committer is not in the mailmap, so `:mailmap` is a no-op there —
        // the control that proves the rewrite is keyed on the identity and not
        // applied blindly.
        ("committername:mailmap", "C O Mitter"),
        ("committeremail:mailmap", "<committer@example.com>"),
    ] {
        let got = f.stdout(&["for-each-ref", "refs/heads/main", &format!("--format=%({spec})")]);
        assert_eq!(got, format!("{want}\n"), "%({spec})");
    }
}

/// `email_atom_option_parser` matches by `skip_prefix`, so the text a typo is
/// reported with is the tail *after* the options that did parse — not the whole
/// argument. Reporting the whole thing is the natural implementation and points
/// the user at the wrong character.
#[test]
fn an_email_option_typo_names_only_the_unparsed_tail() {
    let f = Fixture::bare("email-typo");
    f.commit_message("subject\n");
    for (spec, blamed) in [
        ("authoremail:trim,bogus", "bogus"),
        ("authoremail:trimx", "x"),
        ("authoremail:mailmapx", "x"),
        ("authoremail:bogus", "bogus"),
    ] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--format=%({spec})")]);
        assert_eq!(out, "");
        assert_eq!(err, format!("fatal: unrecognized %(authoremail) argument: {blamed}\n"));
        assert_eq!(code, 128);
    }
    // `%(authorname)` takes `mailmap` and nothing else, and reports the argument
    // whole — it is `strcmp`, not `skip_prefix`.
    let (_, err, _) = f.run(&["for-each-ref", "--format=%(authorname:mailmap,x)"]);
    assert_eq!(err, "fatal: unrecognized %(authorname) argument: mailmap,x\n");
}

// ---------------------------------------------------------------------------
// `%(authordate:<format>)`
// ---------------------------------------------------------------------------

/// A zero UTC offset is `Z` in iso-strict (RFC 3339), not `+00:00` — and a
/// non-zero one is `+HH:MM`. Both halves are asserted, because a fix that hard-
/// codes `Z` breaks the other half and no ordinary fixture would notice.
#[test]
fn iso_strict_writes_z_only_for_a_zero_offset() {
    let f = Fixture::bare("iso-strict");
    f.commit_message("subject\n");
    let utc = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(authordate:iso-strict)"]);
    assert_eq!(utc, "2023-11-14T22:13:20Z\n");

    // A second commit whose author and committer sit in different, non-zero
    // zones, so the two halves of the branch are exercised in one object.
    std::fs::write(f.work.join("a"), b"zoned\n").unwrap();
    f.git(&["add", "a"]);
    let out = f
        .cmd(&["commit", "-q", "-m", "zoned"])
        .env("GIT_AUTHOR_DATE", "1700000000 +0530")
        .env("GIT_COMMITTER_DATE", "1700000000 -0800")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let got = f.stdout(&[
        "for-each-ref",
        "refs/heads/main",
        "--format=%(authordate:iso8601-strict)|%(committerdate:iso-strict)",
    ]);
    assert_eq!(got, "2023-11-15T03:43:20+05:30|2023-11-14T14:13:20-08:00\n");
}

/// `format:<strftime>` computes `%s` and `%z` itself, because there is no
/// portable way to hand a zone to `strftime(3)`. `%s` in particular has to undo
/// the shift `gm_time_t()` applied, or a non-UTC zone reports the wrong epoch —
/// which is a wrong number that still *looks* like a timestamp.
#[test]
fn a_strftime_date_format_computes_percent_s_and_percent_z_itself() {
    let f = Fixture::bare("date-strftime");
    f.commit_message("subject\n");
    std::fs::write(f.work.join("a"), b"zoned\n").unwrap();
    f.git(&["add", "a"]);
    let out = f
        .cmd(&["commit", "-q", "-m", "zoned"])
        .env("GIT_AUTHOR_DATE", "1700000000 +0530")
        .env("GIT_COMMITTER_DATE", "1700000000 +0530")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let got = f.stdout(&[
        "for-each-ref",
        "refs/heads/main",
        "--format=%(authordate:format:%s|%z|%Y-%m-%d %H:%M:%S)",
    ]);
    assert_eq!(got, "1700000000|+0530|2023-11-15 03:43:20\n");

    // `%%` is a literal percent, and an empty format renders nothing.
    let escaped =
        f.stdout(&["for-each-ref", "refs/heads/main", "--format=[%(authordate:format:%%)]"]);
    assert_eq!(escaped, "[%]\n");
    let empty = f.stdout(&["for-each-ref", "refs/heads/main", "--format=[%(authordate:format:)]"]);
    assert_eq!(empty, "[]\n");
}

/// `parse_date_format()`'s two `die()`s say different things, and the one for a
/// bare `format` is the one that collapses into the generic arm.
#[test]
fn the_two_date_format_rejections_stay_distinct() {
    let f = Fixture::bare("date-format-errors");
    f.commit_message("subject\n");
    for (spec, message) in [
        ("format", "date format missing colon separator: format"),
        ("format-local", "date format missing colon separator: format-local"),
        ("bogus", "unknown date format bogus"),
        ("local-bogus", "unknown date format local-bogus"),
        ("isoX", "unknown date format isoX"),
    ] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--format=%(authordate:{spec})")]);
        assert_eq!(out, "", "nothing is printed for :{spec}");
        assert_eq!(err, format!("fatal: {message}\n"));
        assert_eq!(code, 128);
    }
    // The control: the `-local` suffix composes with a real type, and bare
    // `local` is the historical alias for `default-local`.
    for spec in ["iso-local", "short-local", "raw-local", "unix-local", "local"] {
        let (_, err, code) = f.run(&["for-each-ref", &format!("--format=%(authordate:{spec})")]);
        assert_eq!((err.as_str(), code), ("", 0), ":{spec} is a real format");
    }
}

// ---------------------------------------------------------------------------
// `%(trailers:<opts>)`
// ---------------------------------------------------------------------------

/// The verbatim fast path and the re-rendering path disagree, and the folded
/// trailer is what shows it. With no options the block is copied byte for byte,
/// fold included; `unfold` collapses the continuation to a single space.
#[test]
fn the_trailer_fast_path_keeps_a_fold_that_unfold_collapses() {
    let f = Fixture::bare("trailers-fold");
    f.commit_message(MESSAGE);
    let r = "refs/heads/main";

    let bare = f.stdout(&["for-each-ref", r, "--format=%(trailers)"]);
    assert_eq!(
        bare,
        "Signed-off-by: A U Thor <author@example.com>\n\
         Acked-by: Rev Iewer <rev@example.com>\n\
         Helped-by: Some\n  \
         One Folded <folded@example.com>\n\n",
        "the argument-less form copies the block verbatim, fold and all"
    );

    let unfolded = f.stdout(&["for-each-ref", r, "--format=%(trailers:unfold)"]);
    assert_eq!(
        unfolded,
        "Signed-off-by: A U Thor <author@example.com>\n\
         Acked-by: Rev Iewer <rev@example.com>\n\
         Helped-by: Some One Folded <folded@example.com>\n\n",
        "unfold joins the continuation with exactly one space"
    );

    // `%(contents:trailers:<opts>)` is the same parser reached through a
    // different prefix, so it must answer identically.
    let via_contents = f.stdout(&["for-each-ref", r, "--format=%(contents:trailers:unfold)"]);
    assert_eq!(via_contents, unfolded);
}

/// `key=` filters case-insensitively, tolerates the separator being spelled with
/// the key, and implies `only`; `valueonly` and `keyonly` each drop one half.
#[test]
fn trailer_key_filtering_is_case_insensitive_and_implies_only() {
    let f = Fixture::bare("trailers-key");
    f.commit_message(MESSAGE);
    let r = "refs/heads/main";

    for spec in ["key=Acked-by", "key=acked-BY", "key=Acked-by:"] {
        let got = f.stdout(&["for-each-ref", r, &format!("--format=%(trailers:{spec})")]);
        assert_eq!(
            got, "Acked-by: Rev Iewer <rev@example.com>\n\n",
            "%(trailers:{spec}) selects only that trailer"
        );
    }
    let value = f.stdout(&["for-each-ref", r, "--format=%(trailers:key=Acked-by,valueonly)"]);
    assert_eq!(value, "Rev Iewer <rev@example.com>\n\n");
    let key = f.stdout(&["for-each-ref", r, "--format=%(trailers:key=Acked-by,keyonly)"]);
    assert_eq!(key, "Acked-by\n\n");

    // A key that matches nothing prints nothing rather than falling back to the
    // whole block — the failure mode that makes a filter look like it works.
    let none = f.stdout(&["for-each-ref", r, "--format=[%(trailers:key=Nonexistent)]"]);
    assert_eq!(none, "[]\n");
}

/// `separator=` *joins* rather than terminates, so the trailing newline the
/// default form emits disappears; the value goes through `%n`/`%xNN` expansion.
#[test]
fn a_trailer_separator_joins_and_expands_its_escapes() {
    let f = Fixture::bare("trailers-separator");
    f.commit_message(MESSAGE);
    let r = "refs/heads/main";

    let comma = f.stdout(&["for-each-ref", r, "--format=%(trailers:only,unfold,separator=%x2C)"]);
    assert_eq!(
        comma,
        "Signed-off-by: A U Thor <author@example.com>,\
         Acked-by: Rev Iewer <rev@example.com>,\
         Helped-by: Some One Folded <folded@example.com>\n",
        "the separator goes between trailers only, so there is no trailing one"
    );

    let kv = f.stdout(&["for-each-ref", r, "--format=%(trailers:key=Acked-by,key_value_separator==)"]);
    assert_eq!(kv, "Acked-by=Rev Iewer <rev@example.com>\n\n");
}

/// A trailer block that also carries a **non-trailer** line is the only place
/// the separator's position shows: `format_trailers()` puts the separator in
/// front of every item but the first and right-trims afterwards, for prose lines
/// exactly as for real trailers. Appending it after the value instead is
/// invisible on a tidy block and emits a stray trailing separator on a real one.
#[test]
fn a_separator_joins_prose_lines_from_the_front_and_leaves_no_trailing_one() {
    let f = Fixture::bare("trailers-prose-separator");
    f.commit_message(
        "subj\n\nbody\n\nSigned-off-by: A <a@e.co>\nnot a trailer line\nAcked-by: B <b@e.co>\n",
    );
    let r = "refs/heads/main";

    let joined = f.stdout(&["for-each-ref", r, "--format=%(trailers:separator=%x2C)"]);
    assert_eq!(
        joined,
        "Signed-off-by: A <a@e.co>,not a trailer line,Acked-by: B <b@e.co>\n",
        "two separators for three items, and none after the last"
    );
    assert_eq!(joined.matches(',').count(), 2);

    // Without a separator each item is newline-*terminated* instead, so the
    // count differs by one — the asymmetry that makes the arm above easy to get
    // backwards.
    let terminated = f.stdout(&["for-each-ref", r, "--format=%(trailers)"]);
    assert_eq!(
        terminated,
        "Signed-off-by: A <a@e.co>\nnot a trailer line\nAcked-by: B <b@e.co>\n\n"
    );

    // `only` drops the prose line, which is the control: it proves the line above
    // really was classified as a non-trailer rather than parsed as one.
    let only = f.stdout(&["for-each-ref", r, "--format=%(trailers:only,separator=%x2C)"]);
    assert_eq!(only, "Signed-off-by: A <a@e.co>,Acked-by: B <b@e.co>\n");
}

/// The boolean options take git's full `maybe_bool` vocabulary, and a
/// recognised option given an *unparseable* boolean reports an empty argument
/// name — because `match_placeholder_arg_value` consumes the text before the
/// value is judged. The empty name looks like a bug and is the measured stock
/// behaviour.
#[test]
fn trailer_booleans_take_the_full_vocabulary_and_report_a_bad_one_bare() {
    let f = Fixture::bare("trailers-bools");
    f.commit_message(MESSAGE);
    let r = "refs/heads/main";

    let with_prose = f.stdout(&["for-each-ref", r, "--format=%(trailers:only=false)"]);
    let default = f.stdout(&["for-each-ref", r, "--format=%(trailers)"]);
    assert_eq!(with_prose, default, "only=false is the default");

    for yes in ["unfold=yes", "unfold=on", "unfold=1", "unfold=true"] {
        let got = f.stdout(&["for-each-ref", r, &format!("--format=%(trailers:{yes})")]);
        assert!(
            got.contains("Helped-by: Some One Folded"),
            "{yes} must read as true; got {got:?}"
        );
    }
    for no in ["unfold=no", "unfold=off", "unfold=0", "unfold=false"] {
        let got = f.stdout(&["for-each-ref", r, &format!("--format=%(trailers:{no})")]);
        assert!(got.contains("Helped-by: Some\n"), "{no} must read as false; got {got:?}");
    }

    let (_, err, code) = f.run(&["for-each-ref", "--format=%(trailers:only=bogus)"]);
    assert_eq!(err, "fatal: unknown %(trailers) argument: \n");
    assert_eq!(code, 128);
    let (_, err, code) = f.run(&["for-each-ref", "--format=%(trailers:bogus)"]);
    assert_eq!(err, "fatal: unknown %(trailers) argument: bogus\n");
    assert_eq!(code, 128);
    let (_, err, code) = f.run(&["for-each-ref", "--format=%(trailers:key)"]);
    assert_eq!(err, "fatal: expected %(trailers:key=<value>)\n");
    assert_eq!(code, 128);
}

// ---------------------------------------------------------------------------
// `%(is-base:<committish>)`
// ---------------------------------------------------------------------------

/// The atom names **one** ref, chosen over the whole array before the sort. A
/// per-ref implementation cannot express that, and a fixture with a single ref
/// cannot tell the difference — so this one builds two candidate branches off
/// different points and asks about a commit that is a ref tip of neither.
#[test]
fn is_base_marks_exactly_one_ref_and_is_decided_before_the_sort() {
    let f = Fixture::bare("is-base");
    f.commit_message("root\n");
    f.git(&["branch", "early"]);
    f.commit_message("second\n");
    f.commit_message("third\n");
    f.git(&["branch", "late"]);
    // A topic branched off `late`, whose tip is not itself in the ref array
    // under the `refs/heads/{early,late}` filter used below.
    f.git(&["checkout", "-q", "-b", "topic", "late"]);
    f.commit_message("topic work\n");
    let topic_tip = f.rev_parse("HEAD");
    f.git(&["checkout", "-q", "main"]);

    let got = f.stdout(&[
        "for-each-ref",
        "refs/heads/early",
        "refs/heads/late",
        &format!("--format=%(refname:short)|%(is-base:{topic_tip})"),
    ]);
    assert_eq!(
        got, "early|\nlate|({topic_tip})\n".replace("{topic_tip}", &topic_tip),
        "the branch point is `late`, and `early` — also an ancestor — must stay empty"
    );

    // The choice is made before `ref_array_sort`, so reversing the order moves
    // the marker with its ref rather than re-deciding.
    let sorted = f.stdout(&[
        "for-each-ref",
        "refs/heads/early",
        "refs/heads/late",
        "--sort=-refname",
        &format!("--format=%(refname:short)|%(is-base:{topic_tip})"),
    ]);
    assert_eq!(sorted, format!("late|({topic_tip})\nearly|\n"));

    // Two atoms are independent slots; asking twice marks the same ref twice.
    let twice = f.stdout(&[
        "for-each-ref",
        "refs/heads/late",
        &format!("--format=%(is-base:{topic_tip})%(is-base:{topic_tip})"),
    ]);
    assert_eq!(twice, format!("({topic_tip})({topic_tip})\n"));
}

/// The two parse-time rejections: a missing operand is a format error, and an
/// operand that does not name a commit is `die("failed to find '%s'")`.
#[test]
fn is_base_rejects_a_missing_and_an_unresolvable_operand() {
    let f = Fixture::bare("is-base-errors");
    f.commit_message("root\n");
    for spec in ["is-base", "is-base:"] {
        let (out, err, code) = f.run(&["for-each-ref", &format!("--format=%({spec})")]);
        assert_eq!(out, "");
        assert_eq!(err, "fatal: expected format: %(is-base:<committish>)\n");
        assert_eq!(code, 128);
    }
    let (out, err, code) = f.run(&["for-each-ref", "--format=%(is-base:nosuchthing)"]);
    assert_eq!(out, "");
    assert_eq!(err, "fatal: failed to find 'nosuchthing'\n");
    assert_eq!(code, 128);
}

// ---------------------------------------------------------------------------
// `%(deltabase)`
// ---------------------------------------------------------------------------

/// "Always the null oid" passes on any small fixture, so this one builds two
/// blobs that git really does store one against the other, and asserts the base
/// is the *other* blob. The null-oid cases are asserted alongside, because a fix
/// that reports a base for everything is just as wrong.
///
/// The direction is measured, not assumed: `pack-objects` picks the **larger**
/// object as the base and stores the smaller one as a delta against it, so it is
/// the earlier, shorter blob that ends up deltified — the opposite of the
/// chronological intuition.
#[test]
fn deltabase_names_the_real_base_of_a_deltified_object() {
    let f = Fixture::bare("deltabase");
    let big: String = (1..=4000).map(|i| format!("line {i}\n")).collect();
    std::fs::write(f.work.join("big"), big.as_bytes()).unwrap();
    f.git(&["add", "big"]);
    f.git(&["commit", "-q", "-m", "one"]);
    let shorter = f.rev_parse("HEAD:big");

    std::fs::write(f.work.join("big"), format!("{big}line 4001\n").as_bytes()).unwrap();
    f.git(&["add", "big"]);
    f.git(&["commit", "-q", "-m", "two"]);
    let longer = f.rev_parse("HEAD:big");
    assert_ne!(shorter, longer);

    f.git(&["update-ref", "refs/blobs/shorter", &shorter]);
    f.git(&["update-ref", "refs/blobs/longer", &longer]);
    f.git(&["repack", "-adq"]);

    let got = f.stdout(&["for-each-ref", "--format=%(refname:short) %(deltabase)"]);
    let null = "0".repeat(shorter.len());
    let mut lines: Vec<&str> = got.lines().collect();
    lines.sort_unstable();

    assert_eq!(
        lines,
        vec![
            format!("blobs/longer {null}").as_str(),
            format!("blobs/shorter {longer}").as_str(),
            format!("main {null}").as_str(),
        ],
        "the shorter blob is stored against the longer one; a null oid there means \
         the pack entry header was never read, and a non-null oid anywhere else \
         means a whole object was mistaken for a delta"
    );

    // A loose object is never a delta, whatever a pack said before.
    let loose = Fixture::bare("deltabase-loose");
    loose.commit_message("subject\n");
    let got = loose.stdout(&["for-each-ref", "refs/heads/main", "--format=%(deltabase)"]);
    assert_eq!(got.trim(), null);
}
