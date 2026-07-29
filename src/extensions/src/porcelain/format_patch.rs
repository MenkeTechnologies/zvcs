//! `git format-patch` — render commits as mbox-style e-mail patches.
//!
//! Each message is built the way stock git builds it: the fixed
//! `From <oid> Mon Sep 17 00:00:00 2001` magic line, the `From:`/`Date:`/
//! `Subject:` headers (RFC2822 date, RFC2047 q-encoded headers when needed,
//! RFC822 wrapping), the commit body, the three-dash separator, the
//! `--stat`+`--summary` block at git's 72-column mail width, the patch itself,
//! and the `-- \n<version>\n\n` signature.
//!
//! The diffstat is a faithful port of git's `show_stats()` (diff.c) including
//! `scale_linear()` graph scaling and the name-column ellipsis, and the summary
//! lines are a port of `diff_summary()`. The patch body reuses the same default
//! diff settings the rest of this crate uses: Myers with the indent (slider)
//! heuristic, three lines of context, `@@`-header function context, and the
//! `\ No newline at end of file` marker.
//!
//! Covered:
//!   * revision selection — `<since>` (implicit `<since>..HEAD`), `<a>..<b>`,
//!     `<a>...<b>`, `^<rev>`, `-<n>`, `--root`; merges are excluded as git does.
//!   * revision errors — git's own `fatal: ambiguous argument …` / `bad object`
//!     / `bad revision` / `Invalid revision range` text on stderr with exit 128,
//!     and a positional that names an existing path is a pathspec, not an error.
//!   * output — file-per-patch (default, names printed to stdout) or `--stdout`.
//!   * flags — `--stdout`, `-o`/`--output-directory`, `-<n>`/`--max-count`,
//!     `--skip`, `--reverse`, `--min-parents`/`--max-parents`/`--no-merges`,
//!     `-n`/`--numbered`, `-N`/`--no-numbered`, `--start-number`,
//!     `--numbered-files`, `--suffix`, `--subject-prefix`, `--rfc`,
//!     `-v`/`--reroll-count`, `--signature`/`--no-signature`,
//!     `--signature-file`, `--zero-commit`, `-p`/`--no-stat`, `--root`,
//!     `-q`/`--quiet`, `--filename-max-length`, `--cover-letter`,
//!     `-k`/`--keep-subject`, `--to`, `--cc`, `--add-header`, `--in-reply-to`,
//!     `-U`/`--unified`, `-a`/`--text`, `--minimal`,
//!     `--histogram`, `--diff-algorithm=myers|minimal|histogram`.
//!   * messaging — `-s`/`--signoff` (a port of `append_signoff()` with its
//!     trailer-block dedup), `--from[=<ident>]` and `--force-in-body-from` (the
//!     in-body `From:` `pp_user_info()` emits when the header identity is
//!     replaced), `--thread[=shallow|deep]` (`Message-ID:` plus the
//!     `In-Reply-To:`/`References:` chain), `--attach[=<boundary>]` /
//!     `--inline[=<boundary>]` (the `multipart/mixed` wrapper and the
//!     `text/x-patch` part `diffopt.stat_sep` hangs the patch off),
//!     `--notes[=<ref>]`/`--no-notes` (the `Notes:` commentary block, which is
//!     what collapses the `---` before the diffstat to a bare blank line), and
//!     `--base=<commit>|auto`/`--no-base` (the `base-commit:` trailer and the
//!     `prerequisite-patch-id:` list, via a port of `diff_get_patch_id()`).
//!   * cover letter — `--cover-from-description=<mode>`, `--description-file`
//!     and the `branch.<name>.description` lookup behind them, plus
//!     `--commit-list-format=shortlog|modern|log:<fmt>|<fmt>`.
//!   * alternate diffstat formats — `--stat`, `--summary`, `--numstat`,
//!     `--shortstat`, and the whole dirstat family (`--dirstat[=<params>]`,
//!     `-X<params>`, `--dirstat-by-file`, `--cumulative`), selected the way
//!     git's `diff_flush()` selects them and separated the way `log_tree_diff()`
//!     separates them (`---` only when the diffstat and the patch are both on).
//!   * width-tuned diffstat — `--stat=<width>[,<name-width>[,<count>]]`,
//!     `--stat-width`, `--stat-name-width`, `--stat-graph-width`,
//!     `--stat-count`, a port of `diff_opt_stat()`'s field parsing and the
//!     column scaling / `--stat-count` ` ...` truncation in `show_stats()`.
//!   * `-I<regex>`/`--ignore-matching-lines=<regex>`, via a vendored POSIX ERE
//!     engine (`regcomp(REG_EXTENDED | REG_NEWLINE)` semantics) and a port of
//!     xdiff's `xdl_get_hunk()` hunk selection.
//!
//! Flags git accepts that are *not* ported are recorded during parsing and
//! rejected only once it is clear a patch would actually be emitted. Rejecting
//! early would report a porting gap for an invocation git itself refuses, so the
//! two implementations would disagree about *why* they failed. Nothing is
//! silently ignored: if the commit list is non-empty the unported flag is still
//! fatal.
//!
//! Error precedence mirrors git's two passes. Format-patch's own options
//! (`--start-number`, `--thread`, `--cover-from-description`, …) are validated in
//! `parse_options` and so preempt everything, whatever their position. The diff
//! options and the revisions then share `setup_revisions`, a single
//! left-to-right pass, so a bad diff-option *value* (`--color=`, `--diff-algorithm=`,
//! `--stat=`, `--ignore-submodules=`, the `--max-parents=`/`--max-count=`/… integer
//! counts) and a bad revision race by command-line position: whichever comes
//! first wins. These value errors are therefore not emitted in place — they are
//! recorded in `Opts::opt_error` with their argument index and resolved against
//! the revisions in `select_commits`, so `format-patch --color=bad HEAD~9` is the
//! colour error (129) while `format-patch HEAD~9 --color=bad` is the revision
//! error (128). git's own exit taxonomy is preserved: 129 for an option value
//! parse-options rejects, 128 for a `die()` (bad revision, bad `--ignore-submodules`
//! word, a count that is `'not an integer'`).
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * binary files, unless `-a`/`--text` is given. format-patch implies
//!     `--binary`, i.e. a base85 `GIT binary patch` payload; that encoder is not
//!     ported.
//!   * pathspec-limited output. A pathspec is parsed and honoured to the extent
//!     that it never becomes a bogus revision error, but limiting the walk and
//!     the patch to it is not ported, so a pathspec that reaches a non-empty
//!     commit list is fatal.
//!   * interdiff and range-diff, `--ignore-if-in-upstream`,
//!     `--compact-summary`, `--encode-email-headers` as a *deferred* spelling,
//!     whitespace-insensitive diffing, patience diff (imara-diff has Myers,
//!     MyersMinimal and Histogram only), and rename/copy detection.
//!   * a glob in `notes.displayRef`/`GIT_NOTES_DISPLAY_REF`/`--notes=<glob>`:
//!     expanding a pattern over the ref store is not ported, so a pattern is
//!     refused rather than read as a literal ref name.
//!
//! ### Config
//!
//! The `format.*` keys read here are the defaults for the options above:
//! `outputDirectory`, `numbered`, `suffix`, `subjectPrefix`, `signature`,
//! `signatureFile`, `filenameMaxLength`, `coverLetter`, `to`, `cc`, `headers`,
//! `encodeEmailHeaders` (the `--[no-]encode-email-headers` default; on),
//! `noprefix` (the `--no-prefix` default), `signOff`, `from`,
//! `forceInBodyFrom`, `thread`, `attach`, `notes`, `coverFromDescription`,
//! `commitListFormat` and `useAutoBase`. `format.mboxrd` has no command-line
//! spelling at all in format-patch and is read directly. `format.pretty` belongs
//! to `log`/`show`, not here. `branch.<name>.description`, `core.notesRef` and
//! `notes.displayRef` are read through the options that consult them.
//!
//! A generated `Message-ID` embeds `time(NULL)` and the committer's address, so
//! `--thread` output is reproducible only up to that timestamp; everything the
//! id then feeds — the `In-Reply-To:` target, the `References:` chain and how
//! shallow and deep threading differ — is byte-identical to git's.
//!
//! Known deviations, stated rather than hidden: rename/copy detection is
//! disabled (as elsewhere in this crate), so a commit that renames a file
//! renders as a delete plus an add instead of git's `rename from`/`rename to`
//! and `old => new` stat line. Column widths are computed in Unicode scalar
//! values, so East-Asian wide characters in a path measure 1 rather than 2. The
//! cover letter's shortlog does not wrap long subjects at 76 columns.
//! `append_signoff()`'s trailer-block scan does not consult `trailer.<token>.key`
//! config, so only git's own generated prefixes can carry a mixed block over the
//! 25% threshold. The ERE
//! engine matches over Unicode scalar values decoded from the line (invalid
//! UTF-8 bytes decode to themselves), where a C library in a `C` locale would
//! match byte-wise; the two agree for every ASCII pattern. It is also permissive
//! about the constructs POSIX leaves undefined and the C libraries disagree on —
//! an empty alternation branch (`(a|)b`), a stacked repetition (`a**`) and a
//! dangling range (`[a-c-e]`) compile here and under glibc, while BSD `regcomp`
//! rejects all three; every pattern both accept produces the same answer.

use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The version reported in the trailing `-- \n<version>\n` signature. Stock git
/// emits its own `git_version_string` here, so this constant is what makes the
/// signature line comparable; override per-invocation with `--signature=<s>`,
/// `--no-signature`, `--signature-file`, or the
/// `format.signature`/`format.signatureFile` config keys.
const SIGNATURE_VERSION: &str = "2.55.0";

/// git's `MAIL_DEFAULT_WRAP` — the diffstat width used by format-patch.
const MAIL_DEFAULT_WRAP: i64 = 72;

/// git's `FORMAT_PATCH_NAME_MAX_DEFAULT`.
const NAME_MAX_DEFAULT: usize = 64;

/// Header wrap column for `From:`/`Subject:` (RFC2822 §2.1.1).
const HEADER_MAX_LENGTH: i64 = 78;

/// The charset name used for RFC2047 encoding and the 8-bit MIME header.
const ENCODING: &str = "UTF-8";

/// git's placeholder subject and body in a generated cover letter.
const COVER_SUBJECT: &str = "*** SUBJECT HERE ***";
const COVER_BLURB: &str = "*** BLURB HERE ***";

/// git's `DIFF_FORMAT_*` bits, restricted to the ones format-patch can emit.
/// `DIFF_FORMAT_PATCH` is not tracked: format-patch always ORs it in, so it
/// would be a constant.
const FMT_DIFFSTAT: u32 = 1 << 0;
const FMT_NUMSTAT: u32 = 1 << 1;
const FMT_SHORTSTAT: u32 = 1 << 2;
const FMT_DIRSTAT: u32 = 1 << 3;
const FMT_SUMMARY: u32 = 1 << 4;

/// git's `diff_dirstat_permille_default` — the 3.0% cut-off.
const DIRSTAT_PERMILLE_DEFAULT: u32 = 30;

/// The dirstat knobs `parse_dirstat_params()` (diff.c) sets.
#[derive(Clone, Copy)]
struct Dirstat {
    /// `lines`: damage is counted in diffstat lines rather than in bytes.
    by_line: bool,
    /// `files`: every changed file contributes exactly one unit of damage.
    by_file: bool,
    /// `cumulative`: a directory that is reported still counts toward its parent.
    cumulative: bool,
    /// The reporting cut-off, in tenths of a percent.
    permille: u32,
}

/// `--signature`/`--no-signature` state, mirroring git's `signature` pointer
/// before resolution: the version default, an explicit value, or suppressed.
enum SigCli {
    Unset,
    No,
    Value(String),
}

/// git's `enum thread_level` — `--thread[=shallow|deep]` / `format.thread`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Thread {
    Unset,
    Shallow,
    Deep,
}

/// git's `enum cover_from_description` — `--cover-from-description=<mode>` /
/// `format.coverFromDescription`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverFrom {
    None,
    Message,
    Subject,
    Auto,
}

/// git's `COVER_FROM_AUTO_MAX_SUBJECT_LEN`.
const COVER_FROM_AUTO_MAX_SUBJECT_LEN: usize = 100;

/// git's `enum auto_base_setting` — `--base=<c>|auto` / `format.useAutoBase`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoBase {
    Never,
    Always,
    WhenAble,
}

/// git's `mime_boundary_leader` (log-tree.c).
const MIME_BOUNDARY_LEADER: &str = "------------";

/// An `ident_split` reduced to what the mail formats use: the name and the
/// address, with the timestamp dropped.
#[derive(Clone)]
struct Ident {
    name: String,
    mail: String,
}

/// Port of `split_ident_line()` (ident.c) restricted to the name/mail halves:
/// the name runs to the first `<` with trailing whitespace removed, the address
/// to the first `>` after it. A line with neither is not an identity at all.
fn split_ident_line(line: &str) -> Option<Ident> {
    let b = line.as_bytes();
    let lt = b.iter().position(|&c| c == b'<')?;
    let gt = lt + 1 + b[lt + 1..].iter().position(|&c| c == b'>')?;
    let name_end = b[..lt]
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    Some(Ident {
        name: String::from_utf8_lossy(&b[..name_end]).into_owned(),
        mail: String::from_utf8_lossy(&b[lt + 1..gt]).into_owned(),
    })
}

/// Port of `ident_cmp()` (ident.c): the address decides, the name breaks the
/// tie. Only equality matters to `use_in_body_from()`.
fn ident_eq(a: &Ident, b: &Ident) -> bool {
    a.mail == b.mail && a.name == b.name
}

/// The threading state git keeps on `rev_info`: the id of the message being
/// written and the chain the `References:` header lists.
struct ThreadState {
    message_id: Option<String>,
    refs: Vec<String>,
}

struct Opts {
    // Output shape.
    to_stdout: bool,
    outdir: Option<String>,
    numbered: Option<bool>,
    start_number: usize,
    numbered_files: bool,
    suffix: String,
    subject_prefix: String,
    reroll: Option<String>,
    /// The resolved trailing signature (empty means none). git resolves this
    /// only after `setup_revisions()`, so it is filled in by `resolve_signature`
    /// once the commit list is known, not during parsing.
    signature: String,
    /// `--signature`/`--no-signature` state, git's `signature` variable.
    sig_cli: SigCli,
    /// `--signature-file <path>`, git's `signature_file_arg`; last occurrence wins.
    sig_file_arg: Option<String>,
    /// `format.signature`, git's `cfg.signature`.
    cfg_signature: Option<String>,
    /// `format.signatureFile`, git's `cfg.signature_file`.
    cfg_signature_file: Option<String>,
    zero_commit: bool,
    /// `-p`/`--no-stat`: suppress git's `DIFFSTAT|SUMMARY` default entirely.
    use_patch_format: bool,
    /// The `DIFF_FORMAT_*` bits the caller asked for, before the default fills in.
    output_format: u32,
    dirstat: Dirstat,
    /// `--stat=<w>`/`--stat-width`: the diffstat total width. 0 means git's
    /// format-patch default of `MAIL_DEFAULT_WRAP` (72).
    stat_width: i64,
    /// `--stat-name-width`: cap on the filename column. 0 leaves it uncapped.
    stat_name_width: i64,
    /// `--stat-graph-width`: cap on the `+/-` graph column. 0 leaves it uncapped
    /// (format-patch never sets git's `-1` sentinel, so the config default is
    /// never consulted).
    stat_graph_width: i64,
    /// `--stat-count`: how many files to list before a trailing ` ...` line.
    /// 0 lists every file.
    stat_count: i64,
    quiet: bool,
    name_max: usize,
    cover_letter: bool,
    /// `-k`/`--keep-subject`: keep the commit subject verbatim (newlines and
    /// all), with no `[PATCH]` prefix and no series numbering.
    keep_subject: bool,
    /// `--in-reply-to=<id>`: the cleaned inner message id (without `<`/`>`),
    /// emitted as `In-Reply-To:`/`References:` on every message and the cover.
    in_reply_to: Option<String>,
    /// `--to`/`--cc`: recipient lists, one entry per option occurrence, folded
    /// one entry per continuation line the way git emits them.
    to: Vec<String>,
    cc: Vec<String>,
    /// `--add-header`: extra header lines, emitted verbatim before `To:`/`Cc:`.
    add_header: Vec<String>,
    /// `--[no-]encode-email-headers`, defaulted by `format.encodeEmailHeaders`
    /// (git's default is on): Q-encode `From:`/`Subject:` when they carry
    /// non-ASCII. With it off the raw UTF-8 goes out, and the `MIME-Version:`/
    /// `Content-*` block a non-ASCII message triggers is unaffected.
    encode_email_headers: bool,
    /// `format.mboxrd`: with `--stdout`, escape `/^>*From /` message-body lines
    /// with one more `>` so an mbox reader cannot mistake them for a separator.
    /// git has no command-line spelling of this for format-patch.
    mboxrd: bool,
    /// `--no-prefix`/`format.noprefix`: drop the `a/`+`b/` path prefixes from
    /// `diff --git`, `---` and `+++`. `--default-prefix` puts them back.
    noprefix: bool,

    // Messaging.
    /// `-s`/`--signoff`, defaulted by `format.signOff`: append the committer's
    /// `Signed-off-by:` trailer to the message.
    signoff: bool,
    /// `--from[=<ident>]`/`format.from`: the identity the `From:` header names,
    /// with the commit's own author moved into an in-body `From:` when they
    /// differ. `None` leaves the author in the header, as git does by default.
    from: Option<Ident>,
    /// `--force-in-body-from`/`format.forceInBodyFrom`: emit the in-body
    /// `From:` even when it repeats the header identity.
    force_in_body_from: bool,
    /// `--thread[=shallow|deep]`/`format.thread`: generate `Message-ID:` and
    /// chain the series with `In-Reply-To:`/`References:`.
    thread: Thread,
    /// `--attach`/`--inline`/`format.attach`: the MIME multipart boundary.
    mime_boundary: Option<String>,
    /// `--attach`'s `Content-Disposition: attachment` (vs `--inline`'s `inline`).
    no_inline: bool,
    /// `--cover-from-description=<mode>`/`format.coverFromDescription`.
    cover_from: CoverFrom,
    /// `--description-file=<path>`: the branch description to build the cover
    /// letter from, in place of `branch.<name>.description`.
    description_file: Option<String>,
    /// The branch the series was named by, for `branch.<name>.description`.
    branch_name: Option<String>,
    /// `--notes[=<ref>]`/`--no-notes`/`format.notes`.
    notes: super::notes::DisplayOpt,
    /// `--base=<commit>|auto`/`--no-base`/`format.useAutoBase`.
    auto_base: AutoBase,
    /// The explicit `--base=<commit>` argument, if one was given.
    base_commit: Option<String>,
    /// `--commit-list-format=<spec>`/`format.commitListFormat`: how the cover
    /// letter lists the series. `None` is git's `shortlog` default.
    commit_list_format: Option<String>,

    // Revision selection.
    root: bool,
    max_count: Option<usize>,
    skip: usize,
    reverse: bool,
    min_parents: usize,
    max_parents: Option<usize>,
    revs: Vec<String>,
    paths: Vec<String>,

    // Diff rendering.
    context: u32,
    algorithm: Algorithm,
    text: bool,
    /// `-I<regex>`: change groups whose every line matches one of these are
    /// marked ignorable before hunks are assembled.
    ignore_regex: Vec<Regex>,

    /// Flags git accepts that this module has not ported, in the spelling the
    /// caller used. Reported only when a patch would actually be emitted.
    deferred: Vec<String>,

    /// `--range-diff=<range>`: git validates the range after the walk (128 on a
    /// bad revision); the range-diff render itself is not ported.
    range_diff: Option<String>,

    /// Arg index of each entry in `revs`, so a revision error can be ordered
    /// against a diff-option value error the way git's `setup_revisions()` does.
    rev_pos: Vec<usize>,

    /// The earliest diff-option value error, as `(arg index, exit code, stderr
    /// line)`. git reports these from inside `setup_revisions()`, so a revision
    /// error at an earlier position on the command line preempts it. It is
    /// recorded here during parsing and resolved against the revisions in
    /// `select_commits`, rather than emitted in place.
    opt_error: Option<(usize, u8, String)>,
}

pub fn format_patch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut opts = match parse(&repo, args)? {
        Parsed::Ready(opts) => *opts,
        Parsed::Exit(code) => return Ok(code),
    };

    // git: "Make sure 0000-$sub.patch gives non-negative length for $sub".
    let floor = "0000-".len() + opts.suffix.len();
    if opts.name_max <= floor {
        opts.name_max = floor;
    }
    if let Some(r) = &opts.reroll {
        opts.subject_prefix.push_str(&format!(" v{r}"));
    }

    let (commits, paths) = match select_commits(&repo, &opts)? {
        Selected::Commits { commits, paths } => (commits, paths),
        Selected::Exit(code) => return Ok(code),
    };
    if commits.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // git resolves the signature only here — after `setup_revisions()` and after
    // confirming the series is non-empty — so a bad revision, and an empty commit
    // list, both preempt an unreadable signature file (an empty range is exit 0,
    // not the file error).
    match resolve_signature(&opts) {
        Ok(sig) => opts.signature = sig,
        Err(code) => return Ok(code),
    }

    // git validates the `--range-diff` range after the walk
    // (`infer_range_diff_ranges`); an unresolvable side dies 128 there, before
    // any supported-but-unported diff option would matter. A range git accepts
    // still can't be rendered here, so it falls through to the unsupported-flag
    // report below.
    if let Some(rd) = opts.range_diff.clone() {
        if let Err(code) = validate_range_diff(&repo, &rd) {
            return Ok(code);
        }
        opts.deferred.push(format!("--range-diff={rd}"));
    }

    // Everything below emits bytes, so an unported flag can no longer be
    // deferred: it would change what those bytes are.
    if let Some(flag) = opts.deferred.first() {
        bail!("unsupported flag {flag:?}");
    }
    if let Some(path) = paths.first() {
        bail!("pathspec-limited format-patch is not supported (got {path:?})");
    }

    // Auto-numbering kicks in for a series; -n/-N override it. A cover letter
    // always numbers, since it is itself patch 0 of the series.
    let total = commits.len();
    let numbered = opts.numbered.unwrap_or(total > 1 || opts.cover_letter);
    let printed_total = if numbered {
        total + opts.start_number - 1
    } else {
        0
    };

    // `get_base_commit()` + `prepare_bases()` run before anything is written, so
    // an unusable base is fatal before the first message.
    let bases = match resolve_bases(&repo, &commits, &opts)? {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    let mut bases_pending = bases.is_some();

    // The notes trees `--notes`/`format.notes` selected, loaded once.
    let notes_trees = super::notes::load_display(&repo, &opts.notes)?;

    let mut stdout = std::io::stdout().lock();
    let mut buffered: Vec<u8> = Vec::new();

    let mut th = ThreadState {
        message_id: None,
        refs: opts.in_reply_to.iter().cloned().collect(),
    };

    if opts.cover_letter {
        if opts.thread != Thread::Unset {
            th.message_id = Some(gen_message_id(&repo, "cover")?);
        }
        let mut msg: Vec<u8> = Vec::new();
        // A bad `--commit-list-format` is only caught once the cover letter's
        // headers are already written, so the partial message is emitted first.
        if let Err(code) = render_cover_letter(&repo, &commits, printed_total, &opts, &th, &mut msg)?
        {
            emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
            stdout.write_all(&buffered)?;
            stdout.flush()?;
            return Ok(code);
        }
        if let Some(b) = &bases {
            print_bases(&mut msg, b);
            bases_pending = false;
        }
        write_signature(&mut msg, &opts);
        emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
    }

    for (idx, id) in commits.iter().enumerate() {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let nr = idx + opts.start_number;

        // Port of the threading block in `cmd_format_patch()`: deep threading
        // chains every mail onto the previous one, shallow threading keeps
        // replying to whatever the chain already starts with.
        if opts.thread != Thread::Unset {
            if let Some(mid) = th.message_id.take() {
                let shallow_reuses_head = opts.thread == Thread::Shallow
                    && !th.refs.is_empty()
                    && (!opts.cover_letter || nr > 1);
                if !shallow_reuses_head {
                    th.refs.push(mid);
                }
            }
            th.message_id = Some(gen_message_id(&repo, &id.to_hex().to_string())?);
        }

        let mut msg: Vec<u8> = Vec::new();
        render_message(
            &repo,
            &commit,
            nr,
            printed_total,
            &opts,
            &th,
            &notes_trees,
            &mut msg,
        )?;
        if bases_pending {
            if let Some(b) = &bases {
                print_bases(&mut msg, b);
                bases_pending = false;
            }
        }
        // With `--attach`/`--inline` the trailing MIME boundary replaces the
        // `-- \n<version>` signature entirely.
        match &opts.mime_boundary {
            Some(b) => {
                write!(msg, "\n--{MIME_BOUNDARY_LEADER}{b}--\n\n\n")?;
            }
            None => write_signature(&mut msg, &opts),
        }

        // git puts one extra blank line between patches in the mbox stream; the
        // cover letter is not separated that way.
        if opts.to_stdout && idx > 0 {
            buffered.push(b'\n');
        }
        emit_message(&mut buffered, &msg, patch_filename(&commit, nr, &opts)?, &opts)?;
    }

    match stdout.write_all(&buffered).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error; git leaves by
        // way of SIGPIPE rather than returning a status of its own.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            crate::sigpipe::exit_broken_pipe()
        }
        Err(e) => Err(e.into()),
    }
}

/// Append one rendered message to the mbox stream, or write it to its file and
/// note the name for stdout.
fn emit_message(buffered: &mut Vec<u8>, msg: &[u8], name: String, opts: &Opts) -> Result<()> {
    if opts.to_stdout {
        buffered.extend_from_slice(msg);
        return Ok(());
    }
    let path = match &opts.outdir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            format!("{dir}/{name}")
        }
        None => name.clone(),
    };
    if !opts.quiet {
        let shown = match &opts.outdir {
            Some(_) => path.clone(),
            None => name,
        };
        writeln!(buffered, "{shown}")?;
    }
    std::fs::write(&path, msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

enum Parsed {
    Ready(Box<Opts>),
    /// git refused the command line itself; the message is already on stderr.
    Exit(ExitCode),
}

/// Flags whose effect is already this module's behavior, so accepting them
/// changes nothing: the slider heuristic is what the blob diff runs, rename
/// detection is off, color and progress are never rendered, `a/`+`b/` are the
/// prefixes emitted, and format-patch implies `--binary` (binary content is
/// rejected either way).
const NO_OP: &[&str] = &[
    "--indent-heuristic",
    "--no-renames",
    "--rename-empty",
    "--no-rename-empty",
    "--no-color",
    "--no-textconv",
    "--no-ext-diff",
    "--progress",
    "--no-progress",
    "--no-relative",
    "--ita-invisible-in-index",
    "--binary",
];

/// Flags git accepts that this module has not ported. Matched as `--flag` or
/// `--flag=<value>`; see the module header for what each of them would change.
const DEFERRED: &[&str] = &[
    "--encode-email-headers",
    "--interdiff",
    "--creation-factor",
    "--always",
    "--ignore-if-in-upstream",
    "--compact-summary",
    "--patience",
    "--no-indent-heuristic",
    "--full-index",
    "--no-binary",
    "--abbrev",
    "--break-rewrites",
    "--find-renames",
    "--find-copies",
    "--find-copies-harder",
    "--irreversible-delete",
    "--skip-to",
    "--rotate-to",
    "--ignore-cr-at-eol",
    "--ignore-space-at-eol",
    "--ignore-space-change",
    "--ignore-all-space",
    "--ignore-blank-lines",
    "--inter-hunk-context",
    "--function-context",
    "--textconv",
    "--line-prefix",
    "--output-indicator-new",
    "--output-indicator-old",
    "--output-indicator-context",
    "--anchored",
    "--no-walk",
    "--first-parent",
    "--topo-order",
    "--date-order",
    "--author-date-order",
    "-b",
    "-w",
    "-D",
    "-W",
];

/// Short options that carry an attached value, e.g. `-M50%` or `-S<string>`.
const DEFERRED_SHORT: &[&str] = &["-l", "-M", "-C", "-B", "-O", "-S", "-G"];

/// True when `arg` is exactly `name` or the `name=<value>` form.
fn is_flag(arg: &str, name: &str) -> bool {
    arg == name || arg.strip_prefix(name).is_some_and(|r| r.starts_with('='))
}

fn parse(repo: &gix::Repository, args: &[String]) -> Result<Parsed> {
    // git reads the `format.*` config as the defaults for its options; the CLI
    // flags below override scalars and append to the address/header lists.
    let snap = repo.config_snapshot();
    let cfg_str = |k: &str| snap.string(k).and_then(|v| v.to_str().ok().map(str::to_owned));
    let cfg_list = |k: &str| {
        snap.plumbing()
            .values::<gix::bstr::BString>(k)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect::<Vec<String>>()
    };

    // `format.from` resolves to an identity right away, because a value that is
    // neither a boolean nor a parsable ident line is fatal (exit 128).
    let cfg_from = match cfg_str("format.from") {
        Some(v) => match maybe_bool(&v) {
            Some(true) => Some(committer_ident(repo)?),
            Some(false) => None,
            None => match split_ident_line(&v) {
                Some(id) => Some(id),
                None => return Ok(Parsed::Exit(fatal(&format!("invalid ident line: {v}")))),
            },
        },
        // A valueless `[format] from` is the implicit-true boolean.
        None if snap.boolean("format.from") == Some(true) => Some(committer_ident(repo)?),
        None => None,
    };
    let cfg_cover_from = match cfg_str("format.coverFromDescription") {
        Some(v) => match parse_cover_from_description(&v) {
            Some(m) => m,
            None => {
                return Ok(Parsed::Exit(fatal(&format!(
                    "{v}: invalid cover from description mode"
                ))))
            }
        },
        None => CoverFrom::Message,
    };

    let mut o = Opts {
        to_stdout: false,
        outdir: cfg_str("format.outputDirectory"),
        numbered: snap.boolean("format.numbered"),
        start_number: 1,
        numbered_files: false,
        suffix: cfg_str("format.suffix").unwrap_or_else(|| ".patch".to_owned()),
        subject_prefix: cfg_str("format.subjectPrefix").unwrap_or_else(|| "PATCH".to_owned()),
        reroll: None,
        signature: String::new(),
        sig_cli: SigCli::Unset,
        sig_file_arg: None,
        cfg_signature: cfg_str("format.signature"),
        cfg_signature_file: cfg_str("format.signatureFile"),
        zero_commit: false,
        use_patch_format: false,
        output_format: 0,
        dirstat: Dirstat {
            by_line: false,
            by_file: false,
            cumulative: false,
            permille: DIRSTAT_PERMILLE_DEFAULT,
        },
        stat_width: 0,
        stat_name_width: 0,
        stat_graph_width: 0,
        stat_count: 0,
        quiet: false,
        name_max: snap
            .integer("format.filenameMaxLength")
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or(NAME_MAX_DEFAULT),
        cover_letter: snap.boolean("format.coverLetter") == Some(true),
        keep_subject: false,
        in_reply_to: None,
        to: cfg_list("format.to"),
        cc: cfg_list("format.cc"),
        add_header: cfg_list("format.headers"),
        encode_email_headers: snap.boolean("format.encodeEmailHeaders").unwrap_or(true),
        mboxrd: snap.boolean("format.mboxrd") == Some(true),
        noprefix: snap.boolean("format.noprefix") == Some(true),
        signoff: snap.boolean("format.signOff") == Some(true),
        from: cfg_from,
        force_in_body_from: snap.boolean("format.forceInBodyFrom") == Some(true),
        thread: match cfg_str("format.thread") {
            Some(v) if v.eq_ignore_ascii_case("deep") => Thread::Deep,
            Some(v) if v.eq_ignore_ascii_case("shallow") => Thread::Shallow,
            Some(v) => match maybe_bool(&v) {
                Some(true) => Thread::Shallow,
                _ => Thread::Unset,
            },
            None if snap.boolean("format.thread") == Some(true) => Thread::Shallow,
            None => Thread::Unset,
        },
        // `format.attach`: a value is the boundary, an empty value turns it off,
        // and a valueless key means the version string.
        mime_boundary: match cfg_str("format.attach") {
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v),
            None if snap.plumbing().string("format.attach").is_some() => {
                Some(SIGNATURE_VERSION.to_owned())
            }
            None => None,
        },
        no_inline: snap.plumbing().string("format.attach").is_some(),
        cover_from: cfg_cover_from,
        description_file: None,
        branch_name: None,
        notes: match cfg_str("format.notes") {
            Some(v) => match maybe_bool(&v) {
                Some(true) => {
                    let mut o = super::notes::DisplayOpt::default();
                    o.enable_default();
                    o
                }
                Some(false) => super::notes::DisplayOpt::default(),
                None => {
                    let mut o = super::notes::DisplayOpt::default();
                    o.enable_ref(&v);
                    o
                }
            },
            None if snap.boolean("format.notes") == Some(true) => {
                let mut o = super::notes::DisplayOpt::default();
                o.enable_default();
                o
            }
            None => super::notes::DisplayOpt::default(),
        },
        auto_base: match cfg_str("format.useAutoBase") {
            Some(v) if v.eq_ignore_ascii_case("whenAble") => AutoBase::WhenAble,
            Some(v) => match maybe_bool(&v) {
                Some(true) => AutoBase::Always,
                _ => AutoBase::Never,
            },
            None if snap.boolean("format.useAutoBase") == Some(true) => AutoBase::Always,
            None => AutoBase::Never,
        },
        base_commit: None,
        commit_list_format: cfg_str("format.commitListFormat"),
        root: false,
        max_count: None,
        skip: 0,
        reverse: false,
        min_parents: 0,
        // format-patch sets `rev.max_parents = 1`: merges never get a patch.
        max_parents: Some(1),
        revs: Vec::new(),
        paths: Vec::new(),
        context: 3,
        algorithm: Algorithm::Myers,
        text: false,
        ignore_regex: Vec::new(),
        deferred: Vec::new(),
        range_diff: None,
        rev_pos: Vec::new(),
        opt_error: None,
    };

    let mut i = 0;
    let mut pathspec_mode = false;
    // git stores `--cover-from-description`'s value as a plain string during
    // option parsing and only validates it *after* the whole command line is
    // parsed. So an inline value error earlier or later on the line (e.g. a
    // malformed `--start-number`, which parse-options rejects in place with
    // exit 129) must win over this option's own exit-128 rejection. Capture the
    // last value here (last-wins) and validate it once the loop is done.
    let mut cover_from_desc: Option<String> = None;
    // git increments an internal `subject_prefix` counter whenever
    // `--subject-prefix`/`--rfc` is given, and later `die()`s if `-k` is also
    // set. Track only that it was given, not its value.
    let mut subject_prefix_given = false;
    // `--commit-list-format` implies a cover letter, but only when the caller
    // did not spell `--cover-letter`/`--no-cover-letter` out.
    let mut commit_list_format_given = false;
    let mut cover_letter_given = false;
    while i < args.len() {
        let a = args[i].as_str();
        if pathspec_mode {
            o.paths.push(a.to_owned());
            i += 1;
            continue;
        }
        match a {
            "--" => pathspec_mode = true,
            "--stdout" => o.to_stdout = true,
            "-o" | "--output-directory" => {
                i += 1;
                o.outdir = Some(value_at(args, i, a)?);
            }
            "-n" | "--numbered" => o.numbered = Some(true),
            "-N" | "--no-numbered" => o.numbered = Some(false),
            "--start-number" => {
                i += 1;
                match parse_start_number(&value_at(args, i, a)?) {
                    Ok(n) => o.start_number = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            "--numbered-files" => o.numbered_files = true,
            "--subject-prefix" => {
                i += 1;
                o.subject_prefix = value_at(args, i, a)?;
                subject_prefix_given = true;
            }
            "--suffix" => {
                i += 1;
                o.suffix = value_at(args, i, a)?;
            }
            "-v" | "--reroll-count" => {
                i += 1;
                o.reroll = Some(value_at(args, i, a)?);
            }
            "--signature" => {
                i += 1;
                o.sig_cli = SigCli::Value(value_at(args, i, a)?);
            }
            "--no-signature" => o.sig_cli = SigCli::No,
            "--zero-commit" => o.zero_commit = true,
            "--no-zero-commit" => o.zero_commit = false,
            "--encode-email-headers" => o.encode_email_headers = true,
            "--no-encode-email-headers" => o.encode_email_headers = false,
            "--no-prefix" => o.noprefix = true,
            "--default-prefix" => o.noprefix = false,
            "-p" | "--no-stat" => o.use_patch_format = true,
            // Each of these ORs its own `DIFF_FORMAT_*` bit in, which is what
            // makes them *replace* format-patch's `DIFFSTAT|SUMMARY` default
            // rather than add to it.
            "--stat" => o.output_format |= FMT_DIFFSTAT,
            "--summary" => o.output_format |= FMT_SUMMARY,
            "--numstat" => o.output_format |= FMT_NUMSTAT,
            "--shortstat" => o.output_format |= FMT_SHORTSTAT,
            "--cumulative" => {
                if let Err(code) = set_dirstat(&mut o, "cumulative") {
                    return Ok(Parsed::Exit(code));
                }
            }
            "-I" | "--ignore-matching-lines" => {
                i += 1;
                let pat = value_at(args, i, a)?;
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            "--root" => o.root = true,
            "-q" | "--quiet" => o.quiet = true,
            "--filename-max-length" => {
                i += 1;
                o.name_max = parse_num(&value_at(args, i, a)?)?;
            }
            "--cover-letter" => {
                o.cover_letter = true;
                cover_letter_given = true;
            }
            "--no-cover-letter" => {
                o.cover_letter = false;
                cover_letter_given = true;
            }
            "-k" | "--keep-subject" => o.keep_subject = true,
            "--to" => {
                i += 1;
                o.to.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--to=") => o.to.push(s["--to=".len()..].to_owned()),
            "--cc" => {
                i += 1;
                o.cc.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--cc=") => o.cc.push(s["--cc=".len()..].to_owned()),
            "--add-header" => {
                i += 1;
                o.add_header.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--add-header=") => {
                o.add_header.push(s["--add-header=".len()..].to_owned());
            }
            "--in-reply-to" => {
                i += 1;
                let v = value_at(args, i, a)?;
                match clean_message_id(&v) {
                    Some(id) => o.in_reply_to = Some(id),
                    None => return Ok(Parsed::Exit(fatal(&format!("insane in-reply-to: {v}")))),
                }
            }
            s if s.starts_with("--in-reply-to=") => {
                let v = &s["--in-reply-to=".len()..];
                match clean_message_id(v) {
                    Some(id) => o.in_reply_to = Some(id),
                    None => return Ok(Parsed::Exit(fatal(&format!("insane in-reply-to: {v}")))),
                }
            }
            "--signature-file" => {
                i += 1;
                o.sig_file_arg = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--signature-file=") => {
                o.sig_file_arg = Some(s["--signature-file=".len()..].to_owned());
            }
            "--rfc" => {
                o.subject_prefix = "RFC PATCH".to_owned();
                subject_prefix_given = true;
            }
            "-s" | "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "--force-in-body-from" => o.force_in_body_from = true,
            "--no-force-in-body-from" => o.force_in_body_from = false,
            // `--from` takes an *optional* value, so only the attached form
            // carries one; a bare `--from` means the committer identity.
            "--from" => o.from = Some(committer_ident(repo)?),
            "--no-from" => o.from = None,
            s if s.starts_with("--from=") => match split_ident_line(&s["--from=".len()..]) {
                Some(id) => o.from = Some(id),
                None => {
                    return Ok(Parsed::Exit(fatal(&format!(
                        "invalid ident line: {}",
                        &s["--from=".len()..]
                    ))))
                }
            },
            "--thread" => o.thread = Thread::Shallow,
            "--no-thread" => o.thread = Thread::Unset,
            // `--attach`/`--inline` differ only in the `Content-Disposition:`
            // they give the patch part; both default the boundary to git's
            // version string.
            "--attach" => {
                o.mime_boundary = Some(SIGNATURE_VERSION.to_owned());
                o.no_inline = true;
            }
            s if s.starts_with("--attach=") => {
                o.mime_boundary = Some(s["--attach=".len()..].to_owned());
                o.no_inline = true;
            }
            "--no-attach" => {
                o.mime_boundary = None;
                o.no_inline = false;
            }
            "--inline" => {
                o.mime_boundary = Some(SIGNATURE_VERSION.to_owned());
                o.no_inline = false;
            }
            s if s.starts_with("--inline=") => {
                o.mime_boundary = Some(s["--inline=".len()..].to_owned());
                o.no_inline = false;
            }
            "--cover-from-description" => {
                i += 1;
                cover_from_desc = Some(value_at(args, i, a)?);
            }
            "--description-file" => {
                i += 1;
                o.description_file = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--description-file=") => {
                o.description_file = Some(s["--description-file=".len()..].to_owned());
            }
            "--commit-list-format" => {
                i += 1;
                o.commit_list_format = Some(value_at(args, i, a)?);
                commit_list_format_given = true;
            }
            s if s.starts_with("--commit-list-format=") => {
                o.commit_list_format = Some(s["--commit-list-format=".len()..].to_owned());
                commit_list_format_given = true;
            }
            // `--notes` takes an optional ref; `--no-notes` clears everything.
            "--notes" => o.notes.enable_default(),
            s if s.starts_with("--notes=") => o.notes.enable_ref(&s["--notes=".len()..]),
            "--no-notes" => o.notes.disable(),
            // `--base` is git's `base_callback`: `auto` switches on the
            // upstream-derived base, any other value names the base itself.
            "--base" => {
                i += 1;
                set_base(&mut o, &value_at(args, i, a)?);
            }
            s if s.starts_with("--base=") => set_base(&mut o, &s["--base=".len()..]),
            "--no-base" => {
                o.auto_base = AutoBase::Never;
                o.base_commit = None;
            }
            "--reverse" => o.reverse = true,
            "--no-merges" => o.max_parents = Some(1),
            "--minimal" => o.algorithm = Algorithm::MyersMinimal,
            "--histogram" => o.algorithm = Algorithm::Histogram,
            "-a" | "--text" => o.text = true,
            // At the top of the worktree `--relative` neither strips a prefix
            // nor filters by directory, so there it is genuinely a no-op.
            "--relative" => {
                if !at_worktree_top(repo) {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--output-directory=") => {
                o.outdir = Some(s["--output-directory=".len()..].to_owned());
            }
            s if s.starts_with("--start-number=") => {
                match parse_start_number(&s["--start-number=".len()..]) {
                    Ok(n) => o.start_number = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            s if s.starts_with("--subject-prefix=") => {
                o.subject_prefix = s["--subject-prefix=".len()..].to_owned();
                subject_prefix_given = true;
            }
            s if s.starts_with("--suffix=") => o.suffix = s["--suffix=".len()..].to_owned(),
            s if s.starts_with("--reroll-count=") => {
                o.reroll = Some(s["--reroll-count=".len()..].to_owned());
            }
            s if s.starts_with("--signature=") => {
                o.sig_cli = SigCli::Value(s["--signature=".len()..].to_owned());
            }
            s if s.starts_with("--filename-max-length=") => {
                o.name_max = parse_num(&s["--filename-max-length=".len()..])?;
            }
            s if s.starts_with("--rfc=") => {
                o.subject_prefix = format!("{} PATCH", &s["--rfc=".len()..]);
                subject_prefix_given = true;
            }
            // The revision-walk counts share git's strict signed-int parser
            // (`strtol_i`, base 10): trailing junk or a non-numeral is
            // `die("'%s': not an integer")` (exit 128) from inside
            // setup_revisions, so it is recorded positionally rather than
            // emitted in place. A negative value disables the corresponding
            // bound the way revision.c's `>= 0` guards do.
            s if s.starts_with("--max-count=") => {
                let val = &s["--max-count=".len()..];
                match strtol_i(val) {
                    Some(v) => o.max_count = (v >= 0).then_some(v as usize),
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--skip=") => {
                let val = &s["--skip=".len()..];
                match strtol_i(val) {
                    Some(v) => o.skip = v.max(0) as usize,
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--min-parents=") => {
                let val = &s["--min-parents=".len()..];
                match strtol_i(val) {
                    Some(v) => o.min_parents = v.max(0) as usize,
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--max-parents=") => {
                let val = &s["--max-parents=".len()..];
                match strtol_i(val) {
                    Some(v) => o.max_parents = (v >= 0).then_some(v as usize),
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--unified=") => {
                o.context = parse_num(&s["--unified=".len()..])? as u32;
            }
            s if s.len() > 2 && s.starts_with("-U") && s[2..].bytes().all(|c| c.is_ascii_digit()) => {
                o.context = parse_num(&s[2..])? as u32;
            }
            s if s.starts_with("--diff-algorithm=") => {
                match &s["--diff-algorithm=".len()..] {
                    "myers" => o.algorithm = Algorithm::Myers,
                    "minimal" => o.algorithm = Algorithm::MyersMinimal,
                    "histogram" => o.algorithm = Algorithm::Histogram,
                    // imara-diff has no patience implementation.
                    "patience" => o.deferred.push(a.to_owned()),
                    // git's `parse_algorithm_value()` rejects this in
                    // setup_revisions (exit 129); recorded positionally so an
                    // earlier bad revision preempts it.
                    _ => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        "error: option diff-algorithm accepts \"myers\", \"minimal\", \
                         \"patience\" and \"histogram\""
                            .to_owned(),
                    ),
                }
            }
            s if s.starts_with("--thread=") => match &s["--thread=".len()..] {
                "shallow" => o.thread = Thread::Shallow,
                "deep" => o.thread = Thread::Deep,
                // git rejects the value with a bare usage exit and no message.
                _ => return Ok(Parsed::Exit(ExitCode::from(129))),
            },
            s if s.starts_with("--cover-from-description=") => {
                cover_from_desc = Some(s["--cover-from-description=".len()..].to_owned());
            }
            // git's `parse_ignore_submodules_arg()` accepts only these four
            // words; anything else is `die("bad --ignore-submodules argument")`
            // (exit 128) from setup_revisions. "none" is already this module's
            // behavior (submodule changes are shown); the other three only affect
            // an unported render, so they are deferred.
            s if s.starts_with("--ignore-submodules=") => {
                match &s["--ignore-submodules=".len()..] {
                    "none" => {}
                    "all" | "untracked" | "dirty" => o.deferred.push(a.to_owned()),
                    v => record_opt_error(
                        &mut o.opt_error,
                        i,
                        128,
                        format!("fatal: bad --ignore-submodules argument: {v}"),
                    ),
                }
            }
            // `--range-diff=<range>` / `--range-diff <range>`: the range is
            // validated after the walk (see `validate_range_diff`); the render is
            // not ported, so a range git accepts is still fatal there.
            "--range-diff" => {
                i += 1;
                o.range_diff = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--range-diff=") => {
                o.range_diff = Some(s["--range-diff=".len()..].to_owned());
            }
            s if s.starts_with("--src-prefix=") => {
                if &s["--src-prefix=".len()..] != "a/" {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--dst-prefix=") => {
                if &s["--dst-prefix=".len()..] != "b/" {
                    o.deferred.push(a.to_owned());
                }
            }
            // git's `--color=<when>` runs `git_config_colorbool(NULL, arg)`,
            // which accepts only never/always/auto (case-insensitively) and
            // otherwise `error()`s (exit 129) from setup_revisions. Colour is
            // never emitted here, so never/auto agree; "always" would colourize
            // and is not ported, so it is deferred.
            s if s.starts_with("--color=") => {
                match s["--color=".len()..].to_ascii_lowercase().as_str() {
                    "never" | "auto" => {}
                    "always" => o.deferred.push(a.to_owned()),
                    _ => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        "error: option `color' expects \"always\", \"auto\", or \"never\""
                            .to_owned(),
                    ),
                }
            }
            // `--stat=<width>[,<name-width>[,<count>]]`: git's `diff_opt_stat()`
            // parses each field with `strtoul(_, _, 10)`, keeping the previous
            // value for a field its comma never reaches, and rejects any leftover
            // (`error(_("invalid --stat value: %s"))`, exit 129) from inside
            // setup_revisions, so an earlier bad revision preempts it.
            s if s.starts_with("--stat=") => {
                let val = &s["--stat=".len()..];
                match parse_stat_value(val.as_bytes()) {
                    Some((width, name_width, count)) => {
                        o.stat_width = width;
                        if let Some(nw) = name_width {
                            o.stat_name_width = nw;
                        }
                        if let Some(c) = count {
                            o.stat_count = c;
                        }
                        o.output_format |= FMT_DIFFSTAT;
                    }
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        format!("error: invalid --stat value: {val}"),
                    ),
                }
            }
            // The four scalar width knobs all route through `diff_opt_stat()`
            // too, each rejecting trailing junk with
            // `error(_("%s expects a numerical value"))` (exit 129) and each
            // OR-ing in `DIFF_FORMAT_DIFFSTAT`. The value is a required arg, so
            // the space-separated form consumes the next token.
            "--stat-width" | "--stat-name-width" | "--stat-graph-width" | "--stat-count" => {
                let flag = i;
                i += 1;
                let v = value_at(args, i, a)?;
                parse_stat_scalar(&mut o, &a[2..], &v, flag);
            }
            s if s.starts_with("--stat-width=") => {
                parse_stat_scalar(&mut o, "stat-width", &s["--stat-width=".len()..], i);
            }
            s if s.starts_with("--stat-name-width=") => {
                parse_stat_scalar(&mut o, "stat-name-width", &s["--stat-name-width=".len()..], i);
            }
            s if s.starts_with("--stat-graph-width=") => {
                parse_stat_scalar(
                    &mut o,
                    "stat-graph-width",
                    &s["--stat-graph-width=".len()..],
                    i,
                );
            }
            s if s.starts_with("--stat-count=") => {
                parse_stat_scalar(&mut o, "stat-count", &s["--stat-count=".len()..], i);
            }
            s if s.starts_with("--relative=") => {
                o.deferred.push(a.to_owned());
            }
            // `--dirstat`, `-X` and `--dirstat-by-file` all take an *optional*
            // value, so only the attached form carries parameters; a bare
            // `-X foo` leaves `foo` to be read as a revision, as git does.
            "--dirstat" | "-X" => {
                if let Err(code) = set_dirstat(&mut o, "") {
                    return Ok(Parsed::Exit(code));
                }
            }
            "--dirstat-by-file" => {
                if let Err(code) = set_dirstat(&mut o, "files") {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--dirstat=") || s.starts_with("-X") => {
                let params = match s.strip_prefix("--dirstat=") {
                    Some(p) => p,
                    None => &s[2..],
                };
                if let Err(code) = set_dirstat(&mut o, params) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--dirstat-by-file=") => {
                if let Err(code) = set_dirstat(&mut o, "files") {
                    return Ok(Parsed::Exit(code));
                }
                if let Err(code) = set_dirstat(&mut o, &s["--dirstat-by-file=".len()..]) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--ignore-matching-lines=") => {
                let pat = s["--ignore-matching-lines=".len()..].to_owned();
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.len() > 2 && s.starts_with("-I") => {
                let pat = s[2..].to_owned();
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.len() > 2 && s.starts_with("-o") => o.outdir = Some(s[2..].to_owned()),
            s if s.len() > 2
                && s.starts_with("-v")
                && s[2..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                o.reroll = Some(s[2..].to_owned());
            }
            // `-<n>` is a commit count, unlike `-n` which means --numbered.
            s if s.len() > 1
                && s.starts_with('-')
                && s[1..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                o.max_count = Some(parse_num(&s[1..])?);
            }
            s if NO_OP.contains(&s) => {}
            s if DEFERRED.iter().any(|f| is_flag(s, f))
                || DEFERRED_SHORT.iter().any(|f| s.starts_with(f)) =>
            {
                o.deferred.push(s.to_owned());
            }
            s if s.starts_with('-') => bail!("unsupported flag {s:?}"),
            s => {
                o.revs.push(s.to_owned());
                o.rev_pos.push(i);
            }
        }
        i += 1;
    }

    // `--cover-from-description=<mode>` is validated only now, after the whole
    // command line has been parsed (git keeps it as a raw string and calls
    // `parse_cover_from_description()` once). An unrecognised mode is `die()`
    // (exit 128).
    if let Some(v) = cover_from_desc {
        match parse_cover_from_description(&v) {
            Some(m) => o.cover_from = m,
            None => {
                return Ok(Parsed::Exit(fatal(&format!(
                    "{v}: invalid cover from description mode"
                ))))
            }
        }
    }

    // builtin/log.c `cmd_format_patch()` `die()`s (exit 128) when `-k` is combined
    // with numbering or a subject prefix, since keep-subject suppresses both. The
    // numbering check comes first, so it wins when both conflicts are present.
    if o.keep_subject {
        if o.numbered == Some(true) {
            return Ok(Parsed::Exit(fatal(
                "options '-n' and '-k' cannot be used together",
            )));
        }
        if subject_prefix_given {
            return Ok(Parsed::Exit(fatal(
                "options '--subject-prefix/--rfc' and '-k' cannot be used together",
            )));
        }
    }

    // `--commit-list-format` on the command line implies `--cover-letter` when
    // the caller did not decide for themselves; `format.commitListFormat` does
    // not, because git only consults it after that check.
    if commit_list_format_given && !cover_letter_given {
        o.cover_letter = true;
    }

    o.branch_name = find_branch_name(repo, &o);

    // builtin/log.c: the stat+summary block is format-patch's default, but only
    // when the caller asked for no output format of its own — that is what makes
    // `--numstat` (and friends) *replace* it rather than add to it.
    if !o.use_patch_format && o.output_format == 0 {
        o.output_format = FMT_DIFFSTAT | FMT_SUMMARY;
    }

    Ok(Parsed::Ready(Box::new(o)))
}

/// Port of `git_parse_maybe_bool()` (config.c): the words git accepts as
/// booleans, case-insensitively. `None` means "this is not a boolean", which is
/// what makes `format.from`/`format.notes` take a value instead.
fn maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        s => s.parse::<i64>().ok().map(|n| n != 0),
    }
}

/// git's `git_committer_info(IDENT_NO_DATE)` reduced to name and address — the
/// identity `--from` and `format.from` fall back to.
fn committer_ident(repo: &gix::Repository) -> Result<Ident> {
    let sig = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("unable to auto-detect email address"))?;
    Ok(Ident {
        name: sig.name.to_str()?.to_owned(),
        mail: sig.email.to_str()?.to_owned(),
    })
}

/// Port of `parse_cover_from_description()` (builtin/log.c).
fn parse_cover_from_description(arg: &str) -> Option<CoverFrom> {
    match arg {
        "default" | "message" => Some(CoverFrom::Message),
        "none" => Some(CoverFrom::None),
        "subject" => Some(CoverFrom::Subject),
        "auto" => Some(CoverFrom::Auto),
        _ => None,
    }
}

/// Port of `base_callback()` (builtin/log.c).
fn set_base(o: &mut Opts, arg: &str) {
    if arg == "auto" {
        o.auto_base = AutoBase::Always;
        o.base_commit = None;
    } else {
        o.auto_base = AutoBase::Never;
        o.base_commit = Some(arg.to_owned());
    }
}

/// The branch whose `branch.<name>.description` the cover letter may quote.
///
/// Port of `cmd_format_patch()`'s `check_head` shortcut plus `find_branch_name()`
/// (builtin/log.c): a command line that walks HEAD implicitly — the bare
/// `<since>` shorthand, an explicit `HEAD`, or no revision at all — names the
/// current branch; otherwise the single *interesting* revision argument names a
/// branch iff it resolves to `refs/heads/<name>` at that exact tip.
fn find_branch_name(repo: &gix::Repository, o: &Opts) -> Option<String> {
    // Each argument contributes one pending object, except a range, which
    // contributes both of its sides.
    let mut cmdline: Vec<(&str, bool)> = Vec::new();
    for spec in &o.revs {
        if let Some((l, r)) = spec.split_once("...") {
            cmdline.push((l, false));
            cmdline.push((r, false));
        } else if let Some((l, r)) = spec.split_once("..") {
            cmdline.push((l, true));
            cmdline.push((r, false));
        } else if let Some(rest) = spec.strip_prefix('^') {
            cmdline.push((rest, true));
        } else {
            cmdline.push((spec, false));
        }
    }
    if cmdline.is_empty() {
        cmdline.push(("HEAD", false));
    }

    let head_branch = || {
        repo.head_ref()
            .ok()
            .flatten()
            .and_then(|r| r.name().as_bstr().to_str().ok().map(str::to_owned))
            .and_then(|n| n.strip_prefix("refs/heads/").map(str::to_owned))
            .or_else(|| Some(String::new()))
    };
    if cmdline.len() == 1 && !cmdline[0].1 {
        if o.max_count.is_none() && !o.root {
            // The `<since>` shorthand adds HEAD itself.
            return head_branch();
        }
        if cmdline[0].0 == "HEAD" {
            return head_branch();
        }
    }

    let mut interesting = cmdline.iter().filter(|(_, hidden)| !*hidden);
    let (name, _) = interesting.next()?;
    if interesting.next().is_some() {
        return None;
    }
    // `repo_dwim_ref()` hands back the ref name a symref *resolves to*, so a
    // plain `HEAD` on the command line still names the branch it points at.
    let mut r = repo.find_reference(*name).ok()?;
    while let Some(Ok(next)) = r.follow() {
        r = next;
    }
    let branch = r
        .name()
        .as_bstr()
        .to_str()
        .ok()?
        .strip_prefix("refs/heads/")?
        .to_owned();
    let tip = repo.rev_parse_single(BStr::new(*name)).ok()?;
    let head = r.into_fully_peeled_id().ok()?;
    (head.detach() == tip.detach()).then_some(branch)
}

/// Port of `parse_dirstat_params()` (diff.c), plus `parse_dirstat_opt()`'s
/// "and now DIRSTAT is one of the output formats" side effect.
///
/// git `die()`s with the accumulated message and exit 128; `bail!` would collapse
/// that to 1, so the message goes to stderr here and the code comes back as an
/// error the caller turns into an exit.
fn set_dirstat(o: &mut Opts, params: &str) -> std::result::Result<(), ExitCode> {
    let mut errmsg = String::new();
    if !params.is_empty() {
        for p in params.split(',') {
            match p {
                "changes" => {
                    o.dirstat.by_line = false;
                    o.dirstat.by_file = false;
                }
                "lines" => {
                    o.dirstat.by_line = true;
                    o.dirstat.by_file = false;
                }
                "files" => {
                    o.dirstat.by_line = false;
                    o.dirstat.by_file = true;
                }
                "noncumulative" => o.dirstat.cumulative = false,
                "cumulative" => o.dirstat.cumulative = true,
                _ if p.starts_with(|c: char| c.is_ascii_digit()) => match parse_permille(p) {
                    Some(permille) => o.dirstat.permille = permille,
                    None => errmsg.push_str(&format!(
                        "  Failed to parse dirstat cut-off percentage '{p}'\n"
                    )),
                },
                _ => errmsg.push_str(&format!("  Unknown dirstat parameter '{p}'\n")),
            }
        }
    }
    if !errmsg.is_empty() {
        return Err(fatal(&format!(
            "Failed to parse --dirstat/-X option parameter:\n{errmsg}"
        )));
    }
    o.output_format |= FMT_DIRSTAT;
    Ok(())
}

/// git's dirstat percentage grammar: whole percent, then at most one significant
/// fractional digit — `12.375` is 123 permille, and any trailing junk is fatal.
fn parse_permille(p: &str) -> Option<u32> {
    let digits = p.len() - p.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (whole, rest) = p.split_at(digits);
    let mut permille = whole.parse::<u32>().ok()?.checked_mul(10)?;
    let rest = match rest.strip_prefix('.') {
        Some(frac) if frac.starts_with(|c: char| c.is_ascii_digit()) => {
            permille += u32::from(frac.as_bytes()[0] - b'0');
            frac.trim_start_matches(|c: char| c.is_ascii_digit())
        }
        _ => rest,
    };
    rest.is_empty().then_some(permille)
}

/// Port of `diff_opt_ignore_regex()` (diff.c): `regcomp` failure is an
/// `error()`, which makes `parse_options` exit 129 with only that one line.
fn push_ignore_regex(o: &mut Opts, pattern: &str) -> std::result::Result<(), ExitCode> {
    match Regex::compile(pattern) {
        Ok(re) => {
            o.ignore_regex.push(re);
            Ok(())
        }
        Err(_) => {
            eprintln!("error: invalid regex given to -I: '{pattern}'");
            Err(ExitCode::from(129))
        }
    }
}

fn parse_num(s: &str) -> Result<usize> {
    s.parse::<usize>()
        .map_err(|_| anyhow!("invalid number `{s}`"))
}

/// Record a diff-option value error the way git would report it from inside
/// `setup_revisions()`: it is not fatal in place, because a revision error at an
/// earlier command-line position preempts it. Only the earliest such error is
/// kept — parsing is left-to-right, so the first one recorded is the earliest.
fn record_opt_error(slot: &mut Option<(usize, u8, String)>, idx: usize, code: u8, msg: String) {
    if slot.is_none() {
        *slot = Some((idx, code, msg));
    }
}

/// git's `die(_("'%s': not an integer"))` for the revision-walk counts, recorded
/// positionally (exit 128).
fn not_an_integer(slot: &mut Option<(usize, u8, String)>, idx: usize, val: &str) {
    record_opt_error(slot, idx, 128, format!("fatal: '{val}': not an integer"));
}

/// Port of git's `strtol_i(s, 10, &result)`: skip leading ASCII whitespace, an
/// optional sign, then base-10 digits, and succeed only if the whole string is
/// consumed and at least one digit was seen (`p == s` and a trailing `*p` are
/// both failures). Overflow past `i64` is a failure too, matching git's
/// `(int)ul != ul` guard closely enough for the values git accepts.
fn strtol_i(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = b.get(i) == Some(&b'-');
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digit_start = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.checked_mul(10)?.checked_add((b[i] - b'0') as i64)?;
        i += 1;
    }
    if i == digit_start || i != b.len() {
        return None;
    }
    Some(if neg { -val } else { val })
}

/// When a recorded diff-option value error is at an earlier command-line
/// position than a failing revision, git reports the option error instead.
/// Prints the stored message and returns its exit code, else `None`.
fn opt_preempts(o: &Opts, rev_pos: usize) -> Option<ExitCode> {
    match &o.opt_error {
        Some((p, code, msg)) if *p < rev_pos => {
            eprintln!("{msg}");
            Some(ExitCode::from(*code))
        }
        _ => None,
    }
}

/// git reaches a diff-option value error during `setup_revisions()` whenever no
/// earlier revision failed, so once every revision has resolved the recorded
/// error still fires. Prints the stored message and returns its exit code.
fn emit_opt_error(o: &Opts) -> Option<ExitCode> {
    o.opt_error.as_ref().map(|(_, code, msg)| {
        eprintln!("{msg}");
        ExitCode::from(*code)
    })
}

/// Port of the revision resolution `is_range_diff_range()` performs on
/// `--range-diff=<arg>`: each side of an `a..b`/`a...b` range (an empty side is
/// HEAD), or the bare argument, must resolve, else git `die()`s
/// `bad revision '<arg>'` (exit 128) after the walk. The range-diff render is not
/// ported, so a range git accepts is handled as an unsupported flag by the
/// caller; only the resolution failure is reproduced here.
fn validate_range_diff(repo: &gix::Repository, arg: &str) -> std::result::Result<(), ExitCode> {
    let ok_side = |side: &str| -> bool {
        let s = if side.is_empty() { "HEAD" } else { side };
        repo.rev_parse_single(BStr::new(s)).is_ok()
    };
    let ok = if let Some((l, r)) = arg.split_once("...") {
        ok_side(l) && ok_side(r)
    } else if let Some((l, r)) = arg.split_once("..") {
        ok_side(l) && ok_side(r)
    } else {
        ok_side(arg)
    };
    if ok {
        Ok(())
    } else {
        Err(fatal(&format!("bad revision '{arg}'")))
    }
}

/// Emulate C `strtoul(nptr, &end, 10)`, returning the accumulated base-10 value
/// and the byte offset of `end` — the first character the conversion did not
/// consume. Leading ASCII whitespace and a single optional sign are skipped;
/// when no digit is consumed there is "no conversion", so `end` is the original
/// pointer (offset 0) and the value 0, matching libc. The value saturates rather
/// than wrapping, which the callers' width arithmetic tolerates.
fn strtoul10(s: &[u8]) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digit_start = i;
    let mut val: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((s[i] - b'0') as i64);
        i += 1;
    }
    if i == digit_start {
        return (0, 0);
    }
    (if neg { -val } else { val }, i)
}

/// Port of the `--stat` branch of `diff_opt_stat()` (diff.c):
/// `width = strtoul(v); if (*end==',') name_width = strtoul(...); if
/// (*end==',') count = strtoul(...);` returning the parsed fields, or `None`
/// when anything is left over (`if (*end) return error(...)`). `width` is always
/// parsed (an empty value yields 0, git's "use the default" sentinel); the later
/// fields are `Some` only when their comma was reached, so an absent field keeps
/// git's "leave the previous value" behavior.
fn parse_stat_value(val: &[u8]) -> Option<(i64, Option<i64>, Option<i64>)> {
    let (width, mut off) = strtoul10(val);
    let mut name_width = None;
    let mut count = None;
    if val.get(off) == Some(&b',') {
        let (nw, e) = strtoul10(&val[off + 1..]);
        name_width = Some(nw);
        off = off + 1 + e;
    }
    if val.get(off) == Some(&b',') {
        let (c, e) = strtoul10(&val[off + 1..]);
        count = Some(c);
        off = off + 1 + e;
    }
    (off == val.len()).then_some((width, name_width, count))
}

/// Port of the scalar branches of `diff_opt_stat()` (diff.c) —
/// `--stat-width`/`--stat-name-width`/`--stat-graph-width`/`--stat-count`. Each
/// is `strtoul(value); if (*end) error(_("%s expects a numerical value"))`
/// (exit 129, recorded positionally like the other setup_revisions errors) and
/// each turns the diffstat on. `name` is git's dashless `opt->long_name`.
fn parse_stat_scalar(o: &mut Opts, name: &str, val: &str, idx: usize) {
    let (v, off) = strtoul10(val.as_bytes());
    if off != val.len() {
        record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            format!("error: {name} expects a numerical value"),
        );
        return;
    }
    match name {
        "stat-width" => o.stat_width = v,
        "stat-name-width" => o.stat_name_width = v,
        "stat-graph-width" => o.stat_graph_width = v,
        "stat-count" => o.stat_count = v,
        _ => unreachable!("parse_stat_scalar called with an unknown option name"),
    }
    o.output_format |= FMT_DIFFSTAT;
}

/// The three outcomes of parsing an integer-with-suffix option value.
enum IntParse {
    /// A number in the signed 32-bit range git accepts for `--start-number`.
    Ok(i64),
    /// Not a number, or trailing junk / an unrecognised unit suffix.
    Bad,
    /// A well-formed number, but outside `[i32::MIN, i32::MAX]` after the unit.
    Range,
}

/// Port of C `strtoimax(s, &end, 0)`: optional leading ASCII whitespace, an
/// optional sign, then a base-0 numeral (`0x…` hex, `0…` octal, else decimal).
/// Returns the value and the number of bytes consumed, or `None` when no digit is
/// consumed. Accumulates in `i128` so an over-long numeral saturates rather than
/// wrapping; the caller's range check turns that into git's ERANGE.
fn strtoimax0(s: &[u8]) -> Option<(i128, usize)> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let (base, start) = if s.get(i) == Some(&b'0') && matches!(s.get(i + 1), Some(b'x') | Some(b'X'))
    {
        (16i128, i + 2)
    } else if s.get(i) == Some(&b'0') {
        (8i128, i)
    } else {
        (10i128, i)
    };
    let mut j = start;
    let mut val: i128 = 0;
    // Saturate well past the i32 range the caller checks against, so an
    // arbitrarily long numeral becomes an out-of-range error rather than
    // wrapping, while every in-range value is left exact.
    let saturate = 1i128 << 40;
    while j < s.len() {
        let d = match s[j] {
            b'0'..=b'9' => (s[j] - b'0') as i128,
            b'a'..=b'f' => (s[j] - b'a' + 10) as i128,
            b'A'..=b'F' => (s[j] - b'A' + 10) as i128,
            _ => break,
        };
        if d >= base {
            break;
        }
        val = (val * base + d).min(saturate);
        j += 1;
    }
    if j == start {
        return None;
    }
    Some((if neg { -val } else { val }, j))
}

/// git parses `--start-number` as a signed integer with an optional `k`/`m`/`g`
/// unit (base-0, so hex and octal too) into a 4-byte int. This mirrors
/// parse-options.c's per-value handling: no digits after the sign is the "integer
/// value with an optional k/m/g suffix" error, an over-range magnitude is the
/// "not in range" error, and both are `error()` → exit 129.
fn parse_int_with_suffix(value: &str) -> IntParse {
    let b = value.as_bytes();
    let Some((mag, consumed)) = strtoimax0(b) else {
        return IntParse::Bad;
    };
    let factor: i128 = match &b[consumed..] {
        [] => 1,
        [c] if c.eq_ignore_ascii_case(&b'k') => 1024,
        [c] if c.eq_ignore_ascii_case(&b'm') => 1024 * 1024,
        [c] if c.eq_ignore_ascii_case(&b'g') => 1024 * 1024 * 1024,
        _ => return IntParse::Bad,
    };
    let total = mag * factor;
    if total < i32::MIN as i128 || total > i32::MAX as i128 {
        return IntParse::Range;
    }
    IntParse::Ok(total as i64)
}

/// Validate a `--start-number` value the way git's parse-options does, returning
/// the number to use or the exit code git would print for. builtin/log.c clamps a
/// negative start number to 1 after parsing, so that is folded in here.
fn parse_start_number(value: &str) -> std::result::Result<usize, ExitCode> {
    if value.is_empty() {
        eprintln!("error: option `start-number' expects a numerical value");
        return Err(ExitCode::from(129));
    }
    match parse_int_with_suffix(value) {
        IntParse::Ok(v) => Ok(if v < 0 { 1 } else { v as usize }),
        IntParse::Bad => {
            eprintln!(
                "error: option `start-number' expects an integer value with an \
                 optional k/m/g suffix"
            );
            Err(ExitCode::from(129))
        }
        IntParse::Range => {
            eprintln!(
                "error: value {value} for option `start-number' not in range \
                 [-2147483648,2147483647]"
            );
            Err(ExitCode::from(129))
        }
    }
}

/// The value slot of a two-token option, e.g. the `<dir>` in `-o <dir>`.
fn value_at(args: &[String], i: usize, name: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| anyhow!("option `{name}` requires a value"))
}

/// Port of `clean_message_id()` (builtin/log.c): skip leading whitespace and
/// `<`, then take through the last byte that is neither whitespace nor `>`. The
/// caller wraps the result back in `<`/`>`. `None` is git's
/// `die("insane in-reply-to: …")` (exit 128) when no such byte exists.
fn clean_message_id(msg_id: &str) -> Option<String> {
    let b = msg_id.as_bytes();
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let mut a = 0;
    while a < b.len() && (is_space(b[a]) || b[a] == b'<') {
        a += 1;
    }
    let mut z: Option<usize> = None;
    let mut m = a;
    while m < b.len() {
        if !is_space(b[m]) && b[m] != b'>' {
            z = Some(m);
        }
        m += 1;
    }
    z.map(|z| String::from_utf8_lossy(&b[a..=z]).into_owned())
}

/// Read a `--signature-file`, whose contents become the trailing signature
/// verbatim. git `die()`s (exit 128) `unable to read signature file '<f>': <err>`
/// when it cannot be read; the common missing-file / permission errnos are
/// reproduced from `ErrorKind`.
fn read_signature_file(path: &str) -> std::result::Result<String, ExitCode> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => {
            let reason = match e.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
                std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
                _ => e
                    .raw_os_error()
                    .map(|n| format!("os error {n}"))
                    .unwrap_or_else(|| e.to_string()),
            };
            Err(fatal(&format!(
                "unable to read signature file '{path}': {reason}"
            )))
        }
    }
}

/// Port of the signature-resolution ladder in `cmd_format_patch` (builtin/log.c),
/// run once revisions are resolved. git keeps four inputs — the `signature`
/// pointer (`--signature`/`--no-signature`, else the version default),
/// `signature_file_arg` (`--signature-file`), and the
/// `format.signature`/`format.signatureFile` config — and resolves them in this
/// order:
///   * `--no-signature` inhibits every signature;
///   * an explicit `--signature` is used verbatim (an empty value renders none);
///   * else a `--signature-file`, or a `format.signatureFile` *only when no
///     `format.signature` is set*, is read from disk — an unreadable file is
///     `die_errno` → exit 128, with the `--signature-file` argument preferred
///     over the config when both are present;
///   * else `format.signature`;
///   * else the version default.
fn resolve_signature(o: &Opts) -> std::result::Result<String, ExitCode> {
    match &o.sig_cli {
        SigCli::No => Ok(String::new()),
        SigCli::Value(s) => Ok(s.clone()),
        SigCli::Unset => {
            if o.sig_file_arg.is_some()
                || (o.cfg_signature_file.is_some() && o.cfg_signature.is_none())
            {
                let path = o
                    .sig_file_arg
                    .as_deref()
                    .or(o.cfg_signature_file.as_deref())
                    .expect("a file path is present by the condition above");
                read_signature_file(path)
            } else if let Some(s) = &o.cfg_signature {
                Ok(s.clone())
            } else {
                Ok(SIGNATURE_VERSION.to_owned())
            }
        }
    }
}

/// Whether the process runs at the root of the worktree, where a relative
/// pathspec prefix is empty.
fn at_worktree_top(repo: &gix::Repository) -> bool {
    let (Some(workdir), Ok(cwd)) = (repo.workdir(), std::env::current_dir()) else {
        return false;
    };
    match (workdir.canonicalize(), cwd.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Revision selection
// ---------------------------------------------------------------------------

/// git exits 128 on a fatal error; `anyhow::bail!` would collapse that to 1, so
/// the message goes to stderr here and the code is returned explicitly.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// git's `die_verify_filename()` wording for an argument that is neither a
/// revision nor an existing path.
fn ambiguous(spec: &str) -> ExitCode {
    fatal(&format!(
        "ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// A full-length object name is reported as a missing object rather than as an
/// ambiguous argument — git can tell it was meant to be an object id.
fn is_full_oid(spec: &str, hexsz: usize) -> bool {
    spec.len() == hexsz && spec.bytes().all(|b| b.is_ascii_hexdigit())
}

enum Selected {
    Commits {
        commits: Vec<ObjectId>,
        /// Pathspecs, including positionals that turned out to name a path.
        paths: Vec<String>,
    },
    Exit(ExitCode),
}

/// Resolve the revision arguments into the commits to format, oldest first and
/// with merges dropped (git sets `rev.max_parents = 1`).
///
/// A lone revision with neither `-<n>` nor `--root` is git's traditional
/// `format-patch <since>` shorthand for `<since>..HEAD`; anything else is an
/// ordinary walk over the given tips and exclusions. With no tips at all git
/// formats nothing unless `-<n>` or `--root` asked for HEAD implicitly.
fn select_commits(repo: &gix::Repository, o: &Opts) -> Result<Selected> {
    let hexsz = repo.object_hash().len_in_hex();
    let resolve = |spec: &str| -> Option<ObjectId> {
        repo.rev_parse_single(BStr::new(spec))
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|obj| obj.peel_to_commit().ok())
            .map(|c| c.id)
    };
    // An empty side of a range means HEAD, as in `..main` or `main..`.
    // A named fn, not a closure: closure inference unifies the input and output
    // lifetimes into one variable that cannot outlive the call.
    fn or_head(s: &str) -> &str {
        if s.is_empty() {
            "HEAD"
        } else {
            s
        }
    }

    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();
    let mut paths: Vec<String> = o.paths.clone();
    let mut plain_tips = 0usize;

    for (k, spec) in o.revs.iter().enumerate() {
        // git resolves revisions and diff options in one left-to-right pass, so
        // a recorded diff-option value error preempts this revision iff it sits
        // earlier on the command line. `rev_err` defers computing the revision
        // error (which prints its own message) until that check has passed.
        let rpos = o.rev_pos[k];
        let rev_err = |compute: &dyn Fn() -> ExitCode| -> ExitCode {
            match opt_preempts(o, rpos) {
                Some(e) => e,
                None => compute(),
            }
        };
        // A missing side of a range is reported against the whole range: as a
        // missing object when it was spelled as one, else as an ambiguous
        // argument, exactly as git's `setup_revisions()` does.
        let range_error = |side: &str| -> ExitCode {
            if is_full_oid(side, hexsz) {
                fatal(&format!("Invalid revision range {spec}"))
            } else {
                ambiguous(spec)
            }
        };

        if let Some((left, right)) = spec.split_once("...") {
            let (left, right) = (or_head(left), or_head(right));
            let Some(a) = resolve(left) else {
                return Ok(Selected::Exit(rev_err(&|| range_error(left))));
            };
            let Some(b) = resolve(right) else {
                return Ok(Selected::Exit(rev_err(&|| range_error(right))));
            };
            // `a...b` is everything reachable from either tip but not both.
            for base in repo.merge_bases_many(a, &[b])? {
                hidden.push(base.detach());
            }
            tips.push(a);
            tips.push(b);
        } else if let Some((left, right)) = spec.split_once("..") {
            let (left, right) = (or_head(left), or_head(right));
            let Some(a) = resolve(left) else {
                return Ok(Selected::Exit(rev_err(&|| range_error(left))));
            };
            let Some(b) = resolve(right) else {
                return Ok(Selected::Exit(rev_err(&|| range_error(right))));
            };
            hidden.push(a);
            tips.push(b);
        } else if let Some(rest) = spec.strip_prefix('^') {
            match resolve(rest) {
                Some(id) => hidden.push(id),
                None if is_full_oid(rest, hexsz) => {
                    return Ok(Selected::Exit(rev_err(&|| {
                        fatal(&format!("bad object {rest}"))
                    })))
                }
                // An exclusion is never retried as a filename.
                None => {
                    return Ok(Selected::Exit(rev_err(&|| {
                        fatal(&format!("bad revision '{spec}'"))
                    })))
                }
            }
        } else {
            match resolve(spec) {
                Some(id) => {
                    tips.push(id);
                    plain_tips += 1;
                }
                // git falls back to treating the argument as a pathspec when it
                // names something that exists in the worktree.
                None if std::path::Path::new(spec).exists() => paths.push(spec.clone()),
                None if is_full_oid(spec, hexsz) => {
                    return Ok(Selected::Exit(rev_err(&|| {
                        fatal(&format!("bad object {spec}"))
                    })))
                }
                None => return Ok(Selected::Exit(rev_err(&|| ambiguous(spec)))),
            }
        }
    }

    // Every revision resolved (or became a pathspec). git still reaches any
    // recorded diff-option value error during the same pass, so it fires now.
    if let Some(e) = emit_opt_error(o) {
        return Ok(Selected::Exit(e));
    }

    // `format-patch <since>` prepares what the other side does not have yet.
    if plain_tips == 1 && tips.len() == 1 && o.max_count.is_none() && !o.root {
        let since = tips.pop().expect("a single tip was just counted");
        hidden.push(since);
        tips.push(repo.head_id()?.detach());
    } else if tips.is_empty() && (o.max_count.is_some() || o.root) {
        // `format-patch -3` and `format-patch --root` walk from HEAD; with no
        // revision argument at all git formats nothing.
        tips.push(repo.head_id()?.detach());
    }

    if tips.is_empty() {
        return Ok(Selected::Commits {
            commits: Vec::new(),
            paths,
        });
    }

    let mut platform = repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    if !hidden.is_empty() {
        platform = platform.with_hidden(hidden);
    }

    let mut out: Vec<ObjectId> = Vec::new();
    let mut skipped = 0usize;
    for info in platform.all()? {
        let info = info?;
        let parents = repo
            .find_object(info.id)?
            .try_into_commit()?
            .parent_ids()
            .count();
        if parents < o.min_parents {
            continue;
        }
        if o.max_parents.is_some_and(|max| parents > max) {
            continue;
        }
        if skipped < o.skip {
            skipped += 1;
            continue;
        }
        if o.max_count.is_some_and(|max| out.len() >= max) {
            break;
        }
        out.push(info.id);
    }
    // The walk is newest-first; git emits oldest-first unless asked to reverse.
    if !o.reverse {
        out.reverse();
    }
    Ok(Selected::Commits {
        commits: out,
        paths,
    })
}

// ---------------------------------------------------------------------------
// Message rendering
// ---------------------------------------------------------------------------

/// `[v<n>-]NNNN-<sanitized subject><suffix>`, or the bare number under
/// `--numbered-files`. Port of `fmt_output_subject()` (log-tree.c).
fn patch_filename(commit: &gix::Commit<'_>, nr: usize, opts: &Opts) -> Result<String> {
    if opts.numbered_files {
        return Ok(nr.to_string());
    }
    let msg = skip_blank_lines(commit.message_raw()?);
    // git's `%f` sanitizes only the first line of the subject.
    let first_line = &msg[..one_line(msg)];
    Ok(numbered_filename(nr, trim_end_ws(first_line), opts))
}

/// The cover letter is always patch zero, whatever `--start-number` moved the
/// rest of the series to.
fn cover_filename(opts: &Opts) -> String {
    if opts.numbered_files {
        return "0".to_owned();
    }
    numbered_filename(0, b"cover letter", opts)
}

fn numbered_filename(nr: usize, subject: &[u8], opts: &Opts) -> String {
    let mut name = String::new();
    if let Some(r) = &opts.reroll {
        sanitize_subject(&mut name, format!("v{r}").as_bytes());
        name.push('-');
    }
    name.push_str(&format!("{nr:04}-"));
    sanitize_subject(&mut name, subject);

    let max = opts.name_max - (opts.suffix.len() + 1);
    if name.len() > max {
        // `sanitize_subject` only emits ASCII, so this is a char boundary.
        name.truncate(max);
    }
    name.push_str(&opts.suffix);
    name
}

/// Port of `format_sanitized_subject()` (pretty.c): collapse everything that is
/// not `[A-Za-z0-9._]` into single dashes, fold runs of dots, and trim trailing
/// `.`/`-`.
fn sanitize_subject(out: &mut String, msg: &[u8]) {
    let start_len = out.len();
    let mut space = 2u8;
    let mut i = 0;
    while i < msg.len() {
        let c = msg[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push('-');
            }
            space = 0;
            out.push(c as char);
            if c == b'.' {
                while i + 1 < msg.len() && msg[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.len() > start_len && (out.ends_with('.') || out.ends_with('-')) {
        out.pop();
    }
}

/// Render one complete mail message: magic `From` line, headers, body, and —
/// when the commit changes anything — the three-dash separator, stat/summary
/// block and patch, followed by the signature.
#[allow(clippy::too_many_arguments)]
fn render_message(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    nr: usize,
    total: usize,
    opts: &Opts,
    th: &ThreadState,
    notes_trees: &[super::notes::Tree],
    out: &mut Vec<u8>,
) -> Result<()> {
    write_from_line(out, commit.id, opts)?;
    write_message_ids(out, th)?;

    // Headers and body are built in one buffer because git's wrapping and the
    // final `strbuf_rtrim` both depend on what is already in it.
    let mut sb = String::new();
    let raw = commit.message_raw()?;

    let author = commit.author()?;
    let author = Ident {
        name: author
            .name
            .to_str()
            .map_err(|_| {
                anyhow!("author name is not valid UTF-8; RFC2047 encoding needs a known charset")
            })?
            .to_owned(),
        mail: author
            .email
            .to_str()
            .map_err(|_| {
                anyhow!("author email is not valid UTF-8; RFC2047 encoding needs a known charset")
            })?
            .to_owned(),
    };
    let date = commit
        .author()?
        .time()?
        .format(gix::date::time::format::GIT_RFC2822)?;

    // `--from`: the header names the given identity and the commit's own author
    // moves into an in-body `From:`, unless the two already agree (git's
    // `use_in_body_from()`, which `--force-in-body-from` short-circuits).
    let (header_ident, in_body_from) = match &opts.from {
        Some(from) if opts.force_in_body_from || !ident_eq(from, &author) => (
            from,
            Some(format!("From: {} <{}>\n", author.name, author.mail)),
        ),
        Some(from) => (from, None),
        None => (&author, None),
    };
    write_identity_headers(
        &mut sb,
        &header_ident.name,
        &header_ident.mail,
        &date,
        opts.encode_email_headers,
    );

    // Subject: — the first paragraph, folded onto one logical line, unless
    // `-k`/`--keep-subject` asked for the raw first paragraph (newlines and all).
    let msg = skip_blank_lines(raw);
    let (joined, rest) = format_subject(msg);
    let title = if opts.keep_subject {
        let consumed = &msg[..msg.len() - rest.len()];
        trim_end_ws(consumed)
            .to_str()
            .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
            .to_owned()
    } else {
        joined
            .to_str()
            .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
            .to_owned()
    };
    write_subject(&mut sb, &title, nr, total, opts);

    // git's `need_8bit_cte`: `-1` (never) under `--attach`/`--inline`, since the
    // multipart block declares the encoding itself; otherwise the committer
    // identity decides it when `--signoff` will append their trailer, and
    // failing that any non-ASCII byte in the message or the in-body headers.
    let signoff_needs_8bit = opts.signoff && {
        let c = committer_ident(repo)?;
        non_ascii(&c.name) || non_ascii(&c.mail)
    };
    let need_8bit = opts.mime_boundary.is_none()
        && (signoff_needs_8bit
            || raw.iter().any(|&b| b >= 0x80)
            || in_body_from.as_deref().is_some_and(non_ascii));
    if need_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str(&format!("Content-Type: text/plain; charset={ENCODING}\n"));
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    // `--add-header`, then `To:`/`Cc:`, follow the identity/MIME headers, and
    // the multipart preamble follows those (git builds one `extra_headers`
    // strbuf in that order).
    write_extra_headers(&mut sb, opts);
    write_mime_preamble(&mut sb, opts);
    sb.push('\n');
    // The in-body `From:` sits at the very top of the body, set off by a blank
    // line, and is not part of what `--signoff` treats as the message.
    if let Some(h) = &in_body_from {
        sb.push_str(h);
        sb.push('\n');
    }

    // Body — the remaining paragraphs, right-trimmed line by line.
    let beginning_of_body = sb.len();
    let mut body: Vec<u8> = Vec::new();
    pp_remainder(rest, &mut body);
    mboxrd_escape(&mut body, opts);
    sb.push_str(
        body.to_str()
            .map_err(|_| anyhow!("commit message is not valid UTF-8"))?,
    );
    while sb.ends_with([' ', '\t', '\n', '\r']) {
        sb.pop();
    }
    sb.push('\n');
    if sb.len() <= beginning_of_body {
        sb.push('\n');
    }
    // `rev.add_signoff` runs `append_signoff()` over the whole pretty-printed
    // message — headers included, since that is the buffer git hands it — with
    // `APPEND_SIGNOFF_DEDUP`, so a trailer block that already carries this
    // `Signed-off-by:` anywhere is left alone.
    if opts.signoff {
        let c = committer_ident(repo)?;
        super::commit::append_signoff(&mut sb, &format!("{} <{}>", c.name, c.mail), 0, true);
    }

    // Notes open their own commentary block, which is what makes the `---` line
    // before the diffstat collapse to a bare blank line.
    let mut shown_dashes = false;
    let notes = super::notes::format_display(repo, notes_trees, commit.id, false)?;
    if !notes.is_empty() {
        sb.push_str("---\n");
        shown_dashes = true;
        sb.push_str(
            notes
                .to_str()
                .map_err(|_| anyhow!("note is not valid UTF-8"))?,
        );
    }
    out.extend_from_slice(sb.as_bytes());

    // The patch itself.
    let new_tree = commit.tree()?;
    let parents: Vec<_> = commit.parent_ids().collect();
    let old_tree = match parents.first() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let abbrev = new_tree.id().shorten()?.hex_len();
    let changes = tree_changes(repo, old_tree.as_ref(), Some(&new_tree))?;

    if !changes.is_empty() {
        let mut patch: Vec<u8> = Vec::new();
        let mut stats: Vec<StatEntry> = Vec::new();
        for change in &changes {
            stats.push(emit_change(repo, &mut patch, change, abbrev, opts)?);
        }

        let stat_sep = mime_stat_sep(commit, nr, opts)?;
        emit_stat_blocks(
            repo,
            out,
            &changes,
            &stats,
            opts,
            shown_dashes,
            stat_sep.as_deref(),
        )?;
        out.extend_from_slice(&patch);
    }
    Ok(())
}

/// True when any byte is outside 7-bit ASCII — git's `has_non_ascii()`.
fn non_ascii(s: &str) -> bool {
    s.bytes().any(|b| b >= 0x80)
}

/// `Message-ID:` and the `In-Reply-To:`/`References:` chain, in the order
/// `log_write_email_headers()` prints them: straight after the mbox `From` line
/// and ahead of the pretty-printed identity headers.
fn write_message_ids(out: &mut Vec<u8>, th: &ThreadState) -> Result<()> {
    if let Some(id) = &th.message_id {
        writeln!(out, "Message-ID: <{id}>")?;
    }
    if let Some(last) = th.refs.last() {
        writeln!(out, "In-Reply-To: <{last}>")?;
        for (i, r) in th.refs.iter().enumerate() {
            let lead = if i > 0 { "\t" } else { "References: " };
            writeln!(out, "{lead}<{r}>")?;
        }
    }
    Ok(())
}

/// The `multipart/mixed` header block and its first (text) part, appended to the
/// extra headers exactly as `log_write_email_headers()` builds it. The cover
/// letter is written with `maybe_multipart = 0`, so it never gets this.
fn write_mime_preamble(sb: &mut String, opts: &Opts) {
    let Some(b) = &opts.mime_boundary else {
        return;
    };
    sb.push_str(&format!(
        "MIME-Version: 1.0\n\
         Content-Type: multipart/mixed; boundary=\"{MIME_BOUNDARY_LEADER}{b}\"\n\
         \n\
         This is a multi-part message in MIME format.\n\
         --{MIME_BOUNDARY_LEADER}{b}\n\
         Content-Type: text/plain; charset=UTF-8; format=fixed\n\
         Content-Transfer-Encoding: 8bit\n\n"
    ));
}

/// git's `diffopt.stat_sep` under `--attach`/`--inline`: the MIME part that
/// carries the patch, emitted after the diffstat instead of a plain blank line.
fn mime_stat_sep(commit: &gix::Commit<'_>, nr: usize, opts: &Opts) -> Result<Option<String>> {
    let Some(b) = &opts.mime_boundary else {
        return Ok(None);
    };
    let name = patch_filename(commit, nr, opts)?;
    let disposition = if opts.no_inline { "attachment" } else { "inline" };
    Ok(Some(format!(
        "\n--{MIME_BOUNDARY_LEADER}{b}\n\
         Content-Type: text/x-patch; name=\"{name}\"\n\
         Content-Transfer-Encoding: 8bit\n\
         Content-Disposition: {disposition}; filename=\"{name}\"\n\n"
    )))
}

/// Everything git prints between the commit message and the patch.
///
/// Two ports meet here. `log_tree_diff()` (log-tree.c) writes the blank line
/// that separates log from diff, prefixing it with `---` only when the diffstat
/// and the patch are *both* being shown. `diff_flush()` (diff.c) then writes the
/// selected stat blocks in a fixed order and, if any of them set its `separator`
/// counter, one more blank line before the patch. Plain (non-`lines`) dirstat is
/// deliberately outside that counter in git, which is why `--dirstat` alone
/// leaves no blank line before the patch while `--dirstat=lines` does.
fn emit_stat_blocks(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    stats: &[StatEntry],
    opts: &Opts,
    shown_dashes: bool,
    stat_sep: Option<&str>,
) -> Result<()> {
    if !shown_dashes && opts.output_format & FMT_DIFFSTAT != 0 {
        out.extend_from_slice(b"---");
    }
    out.push(b'\n');

    let dirstat_by_line = opts.output_format & FMT_DIRSTAT != 0 && opts.dirstat.by_line;
    let mut separator = false;

    if opts.output_format & (FMT_DIFFSTAT | FMT_NUMSTAT | FMT_SHORTSTAT) != 0 || dirstat_by_line {
        if opts.output_format & FMT_NUMSTAT != 0 {
            emit_numstat(out, stats)?;
        }
        if opts.output_format & FMT_DIFFSTAT != 0 {
            emit_stats(out, stats, StatWidths::from_opts(opts))?;
        }
        if opts.output_format & FMT_SHORTSTAT != 0 {
            emit_stat_summary(out, stats)?;
        }
        if dirstat_by_line {
            emit_dirstat_by_line(out, stats, &opts.dirstat)?;
        }
        separator = true;
    }
    if opts.output_format & FMT_DIRSTAT != 0 && !dirstat_by_line {
        emit_dirstat(repo, out, changes, &opts.dirstat)?;
    }
    if opts.output_format & FMT_SUMMARY != 0 && !is_summary_empty(changes) {
        emit_summary(out, changes)?;
        separator = true;
    }

    if separator {
        out.push(b'\n');
        // `--attach`/`--inline` hang the patch off its own MIME part, announced
        // here; with no stat block at all git never emits it and the patch stays
        // inside the first (text) part.
        if let Some(sep) = stat_sep {
            out.extend_from_slice(sep.as_bytes());
        }
    }
    Ok(())
}

/// Port of `make_cover_letter()` (log-tree.c): the placeholder subject and
/// blurb, a shortlog of the series, and the diffstat of the whole range.
///
/// The magic `From` line names the newest commit of the series, and the
/// identity is the committer's — the cover letter is written now, by whoever
/// runs the command, not by the author of any one patch.
fn render_cover_letter(
    repo: &gix::Repository,
    commits: &[ObjectId],
    total: usize,
    opts: &Opts,
    th: &ThreadState,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    // The series is oldest-first unless `--reverse` flipped it; either way the
    // newest commit is the one without a descendant inside the series.
    let newest = if opts.reverse {
        *commits.first().expect("a non-empty series")
    } else {
        *commits.last().expect("a non-empty series")
    };
    write_from_line(out, newest, opts)?;
    write_message_ids(out, th)?;

    let mut sb = String::new();
    // `make_cover_letter()` writes `cfg->from ? cfg->from : git_committer_info(0)`
    // through `pp_user_info()`. An identity from `--from`/`format.from` carries
    // no timestamp, so its `Date:` is the epoch — git's own behaviour, since
    // `show_ident_date()` sees an unset date field.
    let (name, mail, date) = match &opts.from {
        Some(from) => (
            from.name.clone(),
            from.mail.clone(),
            gix::date::Time::new(0, 0).format(gix::date::time::format::GIT_RFC2822)?,
        ),
        None => match repo.committer().transpose()? {
            Some(sig) => (
                sig.name.to_str()?.to_owned(),
                sig.email.to_str()?.to_owned(),
                sig.time()?.format(gix::date::time::format::GIT_RFC2822)?,
            ),
            // No committer identity configured: fall back to the series' author
            // so the cover letter is still a well-formed message.
            None => {
                let commit = repo.find_object(newest)?.try_into_commit()?;
                let author = commit.author()?;
                (
                    author.name.to_str()?.to_owned(),
                    author.email.to_str()?.to_owned(),
                    author.time()?.format(gix::date::time::format::GIT_RFC2822)?,
                )
            }
        },
    };
    write_identity_headers(&mut sb, &name, &mail, &date, opts.encode_email_headers);

    // `prepare_cover_text()`: the branch description, if there is one and
    // `--cover-from-description` did not switch it off, supplies the subject
    // and/or the blurb.
    let description = read_cover_description(repo, opts)?;
    let (subject, blurb) = cover_text(&description, opts);
    write_subject(&mut sb, &subject, 0, total, opts);
    // `make_cover_letter()` decides `need_8bit_cte` by scanning the *raw commit
    // buffers* of the whole series — the cover letter carries no message of its
    // own, so a non-ASCII byte anywhere in the series (identity lines included)
    // is what turns the 8-bit MIME block on, whether or not any header was
    // Q-encoded.
    let mut need_8bit = false;
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        if commit.data.iter().any(|&b| b >= 0x80) {
            need_8bit = true;
            break;
        }
    }
    if need_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str(&format!("Content-Type: text/plain; charset={ENCODING}\n"));
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    // The cover letter is written with `maybe_multipart = 0`, so it never gets
    // the `--attach`/`--inline` preamble the patches do.
    write_extra_headers(&mut sb, opts);
    sb.push('\n');
    let mut body: Vec<u8> = Vec::new();
    pp_remainder(&blurb, &mut body);
    sb.push_str(
        body.to_str()
            .map_err(|_| anyhow!("branch description is not valid UTF-8"))?,
    );
    // `fprintf(file, "%s\n", sb.buf)` — one more newline closes the header+blurb.
    sb.push('\n');
    out.extend_from_slice(sb.as_bytes());

    if let Err(code) = emit_commit_list(repo, commits, opts, out)? {
        return Ok(Err(code));
    }

    // The range's combined diffstat needs a base to diff against, which a root
    // commit does not have; git omits the block in that case.
    let first = repo
        .find_object(*commits.first().expect("a non-empty series"))?
        .try_into_commit()?;
    let base = match first.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    // `show_diffstat()` (builtin/log.c) builds its own `diff_options` with a
    // hard-coded `DIFF_FORMAT_SUMMARY | DIFF_FORMAT_DIFFSTAT`, so the cover
    // letter keeps the stat+summary block whatever the series was asked for.
    if let Some(base) = base {
        let newest_tree = repo.find_object(newest)?.try_into_commit()?.tree()?;
        let abbrev = newest_tree.id().shorten()?.hex_len();
        let changes = tree_changes(repo, Some(&base), Some(&newest_tree))?;
        if !changes.is_empty() {
            let mut discard: Vec<u8> = Vec::new();
            let mut stats: Vec<StatEntry> = Vec::new();
            for change in &changes {
                stats.push(emit_change(repo, &mut discard, change, abbrev, opts)?);
            }
            // `show_diffstat()` memcpy's `rev->diffopt`, keeping the width knobs,
            // so the cover letter's combined diffstat honors them too.
            emit_stats(out, &stats, StatWidths::from_opts(opts))?;
            emit_summary(out, &changes)?;
            out.push(b'\n');
        }
    }
    Ok(Ok(()))
}

/// Port of `prepare_cover_text()` (builtin/log.c): read the branch description
/// that `--cover-from-description` will draw the subject and blurb from.
///
/// `--description-file` wins over `branch.<name>.description`, and
/// `--cover-from-description=none` skips the lookup entirely.
fn read_cover_description(repo: &gix::Repository, opts: &Opts) -> Result<Vec<u8>> {
    if opts.cover_from == CoverFrom::None {
        return Ok(Vec::new());
    }
    if let Some(path) = opts.description_file.as_deref().filter(|p| !p.is_empty()) {
        return std::fs::read(path)
            .map_err(|e| anyhow!("unable to read branch description file '{path}': {e}"));
    }
    let Some(branch) = opts.branch_name.as_deref().filter(|b| !b.is_empty()) else {
        return Ok(Vec::new());
    };
    let key = format!("branch.{branch}.description");
    Ok(repo
        .config_snapshot()
        .string(&key)
        .map(|v| v.to_vec())
        .unwrap_or_default())
}

/// The subject and blurb `prepare_cover_text()` settles on for the given mode.
fn cover_text(description: &[u8], opts: &Opts) -> (String, Vec<u8>) {
    let default = (COVER_SUBJECT.to_owned(), COVER_BLURB.as_bytes().to_vec());
    if opts.cover_from == CoverFrom::None || description.is_empty() {
        return default;
    }
    let (subject, rest) = format_subject(description);
    // `message` keeps the placeholder subject and uses the whole description as
    // the blurb; `subject` splits it; `auto` splits it only when the first
    // paragraph is short enough to read as a subject line.
    let split = match opts.cover_from {
        CoverFrom::Subject => true,
        CoverFrom::Auto => subject.len() <= COVER_FROM_AUTO_MAX_SUBJECT_LEN,
        _ => false,
    };
    if !split {
        return (default.0, description.to_vec());
    }
    match String::from_utf8(subject) {
        Ok(s) => (s, rest.to_vec()),
        Err(_) => default,
    }
}

/// Port of `make_cover_letter()`'s commit-list dispatch: `shortlog` (the
/// default), `modern`, a `log:<format>` prefix, or any bare format string that
/// contains a `%`. Anything else is `die("'%s' is not a valid format string")`.
fn emit_commit_list(
    repo: &gix::Repository,
    commits: &[ObjectId],
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    let fmt = opts.commit_list_format.as_deref().unwrap_or("shortlog");
    if let Some(rest) = fmt.strip_prefix("log:") {
        generate_commit_list(repo, commits, rest, out)?;
        return Ok(Ok(()));
    }
    match fmt {
        "shortlog" => {
            emit_shortlog(repo, commits, out)?;
            out.push(b'\n');
        }
        // git spells the built-in `modern` layout as a format string, so it
        // wraps at the mail width and numbers each entry within the series.
        "modern" => {
            let n = commits.len();
            for (i, id) in commits.iter().enumerate() {
                let commit = repo.find_object(*id)?.try_into_commit()?;
                let (subject, _) = format_subject(skip_blank_lines(commit.message_raw()?));
                let line = format!(
                    "[{}/{n}] {}",
                    i + 1,
                    subject
                        .to_str()
                        .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
                );
                let mut wrapped = String::new();
                wrap_text(&mut wrapped, &line, 0, 0, MAIL_DEFAULT_WRAP);
                out.extend_from_slice(wrapped.as_bytes());
                out.push(b'\n');
            }
            out.push(b'\n');
        }
        f if f.contains('%') => generate_commit_list(repo, commits, f, out)?,
        // git validates the spec only here, after the cover letter's headers and
        // blurb have already been written, so the partial message is kept.
        f => return Ok(Err(fatal(&format!("'{f}' is not a valid format string")))),
    }
    Ok(Ok(()))
}

/// Port of `generate_commit_list_cover()` (builtin/log.c): one line per commit,
/// oldest first, rendered through a `--pretty=format:` string.
///
/// git runs this with a zeroed `pretty_print_context`, so the abbreviating
/// placeholders answer with full object names — which is what `log`'s shared
/// `format_commit()` reproduces.
fn generate_commit_list(
    repo: &gix::Repository,
    commits: &[ObjectId],
    fmt: &str,
    out: &mut Vec<u8>,
) -> Result<()> {
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        out.extend_from_slice(&super::log::format_commit(repo, &commit, fmt)?);
        out.push(b'\n');
    }
    out.push(b'\n');
    Ok(())
}

/// git's shortlog as the cover letter embeds it: one `Name (count):` group per
/// author, most commits first, each subject indented by two spaces.
fn emit_shortlog(repo: &gix::Repository, commits: &[ObjectId], out: &mut Vec<u8>) -> Result<()> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let author = commit.author()?.name.to_str()?.to_owned();
        let msg = skip_blank_lines(commit.message_raw()?);
        let (title, _) = format_subject(msg);
        let title = title.to_str()?.to_owned();
        match groups.iter_mut().find(|(name, _)| *name == author) {
            Some((_, subjects)) => subjects.push(title),
            None => groups.push((author, vec![title])),
        }
    }
    // Ties keep author order stable by name, as git's string list does.
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    for (i, (name, subjects)) in groups.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        writeln!(out, "{name} ({}):", subjects.len())?;
        for s in subjects {
            writeln!(out, "  {s}")?;
        }
    }
    Ok(())
}

/// The mbox magic line. `--zero-commit` replaces the commit name with zeroes.
fn write_from_line(out: &mut Vec<u8>, id: ObjectId, opts: &Opts) -> Result<()> {
    let name = if opts.zero_commit {
        ObjectId::null(id.kind()).to_hex().to_string()
    } else {
        id.to_hex().to_string()
    };
    writeln!(out, "From {name} Mon Sep 17 00:00:00 2001")?;
    Ok(())
}

/// `From:` — RFC2047 when non-ASCII, RFC822 quoting for specials, else wrapped —
/// followed by `Date:`.
///
/// `encode` is git's `encode_email_headers` (`--[no-]encode-email-headers`,
/// `format.encodeEmailHeaders`, default on). With it off the name goes out as raw
/// UTF-8 through the ordinary quoting/wrapping path; only the Q-encoding is
/// skipped, so the `MIME-Version:`/`Content-Transfer-Encoding: 8bit` block a
/// non-ASCII message still emits is unchanged.
fn write_identity_headers(sb: &mut String, name: &str, mail: &str, date: &str, encode: bool) {
    sb.push_str("From: ");
    let mut max_length = HEADER_MAX_LENGTH;
    if encode && needs_rfc2047_encoding(name) {
        add_rfc2047(sb, name, true);
        max_length = 76;
    } else if name.bytes().any(is_rfc822_special) {
        let quoted = rfc822_quoted(name);
        wrap_text(sb, &quoted, -6, 1, max_length);
    } else {
        wrap_text(sb, name, -6, 1, max_length);
    }
    if max_length < last_line_length(sb) + 2 + mail.len() as i64 + 1 {
        sb.push('\n');
    }
    sb.push_str(&format!(" <{mail}>\n"));
    sb.push_str(&format!("Date: {date}\n"));
}

/// `Subject: [<prefix> n/total] <title>`, with the numbering git uses. Under
/// `-k`/`--keep-subject` the prefix and numbering are dropped entirely, so the
/// bare `Subject: <title>` carries the commit's own subject.
fn write_subject(sb: &mut String, title: &str, nr: usize, total: usize, opts: &Opts) {
    if opts.keep_subject {
        sb.push_str("Subject: ");
    } else if total > 0 {
        let width = decimal_width(total as u64);
        let sep = if opts.subject_prefix.is_empty() {
            ""
        } else {
            " "
        };
        sb.push_str(&format!(
            "Subject: [{}{sep}{:0width$}/{total}] ",
            opts.subject_prefix, nr
        ));
    } else if !opts.subject_prefix.is_empty() {
        sb.push_str(&format!("Subject: [{}] ", opts.subject_prefix));
    } else {
        sb.push_str("Subject: ");
    }
    if opts.encode_email_headers && needs_rfc2047_encoding(title) {
        add_rfc2047(sb, title, false);
    } else {
        let consumed = -last_line_length(sb);
        wrap_text(sb, title, consumed, 1, HEADER_MAX_LENGTH);
    }
    sb.push('\n');
}

/// `format.mboxrd`: escape the message body's `/^>*From /` lines with one more
/// `>`, so a reader splitting an mbox on `From ` cannot mistake a body line for
/// a message separator. git models this as a distinct pretty format
/// (`CMIT_FMT_MBOXRD`) whose `pp_remainder()` prepends the `>`, which is why it
/// reaches the commit-message body only: the subject is a header, and every
/// diff line already carries a ` `/`+`/`-` prefix. The config is honoured only
/// with `--stdout`, exactly as git documents and implements it — patches written
/// to files are never concatenated into an mbox by format-patch itself.
fn mboxrd_escape(body: &mut Vec<u8>, opts: &Opts) {
    if !opts.mboxrd || !opts.to_stdout {
        return;
    }
    let mut out = Vec::with_capacity(body.len());
    for line in body.split_inclusive(|&b| b == b'\n') {
        if is_mboxrd_from(line) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    *body = out;
}

/// git's `is_mboxrd_from()`: a line matching `/^>*From /`.
fn is_mboxrd_from(line: &[u8]) -> bool {
    let rest = &line[line.iter().take_while(|&&b| b == b'>').count()..];
    rest.starts_with(b"From ")
}

/// `--add-header` lines (verbatim), then the `To:` and `Cc:` recipient lists,
/// emitted after the identity/MIME headers and before the blank line that ends
/// the header block. Each recipient list is folded one entry per continuation
/// line, aligned under the first address, the way git emits them.
fn write_extra_headers(sb: &mut String, opts: &Opts) {
    for h in &opts.add_header {
        sb.push_str(h);
        sb.push('\n');
    }
    write_recipient_list(sb, "To", &opts.to);
    write_recipient_list(sb, "Cc", &opts.cc);
}

fn write_recipient_list(sb: &mut String, name: &str, list: &[String]) {
    if list.is_empty() {
        return;
    }
    sb.push_str(name);
    sb.push_str(": ");
    let indent = " ".repeat(name.len() + 2);
    for (idx, value) in list.iter().enumerate() {
        if idx > 0 {
            sb.push_str(",\n");
            sb.push_str(&indent);
        }
        sb.push_str(value);
    }
    sb.push('\n');
}

// ---------------------------------------------------------------------------
// Threading, notes and base info
// ---------------------------------------------------------------------------

/// Port of `gen_message_id()` (builtin/log.c): `<base>.<epoch>.git.<committer
/// e-mail>`.
///
/// The timestamp is `time(NULL)` at the moment the id is minted, so a generated
/// `Message-ID` is by construction not reproducible across runs — git's own ids
/// differ between two invocations a second apart.
fn gen_message_id(repo: &gix::Repository, base: &str) -> Result<String> {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(format!("{base}.{epoch}.git.{}", committer_ident(repo)?.mail))
}

/// The `base-commit:`/`prerequisite-patch-id:` trailer block, as
/// `struct base_tree_info` holds it.
struct Bases {
    base: ObjectId,
    /// Patch ids in collection order; `print_bases()` emits them reversed.
    patch_ids: Vec<ObjectId>,
}

/// Port of `get_base_commit()` + `prepare_bases()` (builtin/log.c).
///
/// The `Err(ExitCode)` arm carries git's own `die()` message, already on stderr;
/// `Ok(None)` is "no base information was requested", which is also what
/// `format.useAutoBase=whenAble` degrades to when no base can be derived.
#[allow(clippy::type_complexity)]
fn resolve_bases(
    repo: &gix::Repository,
    commits: &[ObjectId],
    opts: &Opts,
) -> Result<std::result::Result<Option<Bases>, ExitCode>> {
    let (auto_select, die_on_failure) = match opts.auto_base {
        AutoBase::Never => match &opts.base_commit {
            Some(_) => (false, true),
            None => return Ok(Ok(None)),
        },
        AutoBase::Always => (true, true),
        AutoBase::WhenAble => (true, false),
    };
    // `die_on_failure` decides whether an underivable base is fatal or silently
    // drops the whole block.
    macro_rules! give_up {
        ($msg:expr) => {
            return Ok(if die_on_failure {
                Err(fatal($msg))
            } else {
                Ok(None)
            })
        };
    }

    let base = if !auto_select {
        let spec = opts.base_commit.as_deref().expect("a base was given");
        match repo
            .rev_parse_single(BStr::new(spec))
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|o| o.peel_to_commit().ok())
        {
            Some(c) => c.id,
            None => return Ok(Err(fatal(&format!("unknown commit {spec}")))),
        }
    } else {
        let upstream = repo
            .head_ref()?
            .map(|r| r.name().to_owned())
            .and_then(|name| repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch).and_then(|r| r.ok()));
        let Some(upstream) = upstream else {
            give_up!(
                "failed to get upstream, if you want to record base commit automatically,\n\
                 please use git branch --set-upstream-to to track a remote branch.\n\
                 Or you could specify base commit by --base=<base-commit-id> manually"
            );
        };
        let Some(tip) = repo
            .find_reference(upstream.as_ref())
            .ok()
            .and_then(|mut r| r.peel_to_id().ok())
            .map(|id| id.detach())
        else {
            give_up!(&format!("failed to resolve '{upstream}' as a valid ref"));
        };
        // "There should be one and only one merge base."
        let mut bases = repo.merge_bases_many(tip, commits)?;
        if bases.len() != 1 {
            give_up!("could not find exact merge base");
        }
        bases.pop().expect("exactly one merge base").detach()
    };

    // Reduce the series to a single merge base, pairwise, the way git does.
    let mut rev: Vec<ObjectId> = commits.to_vec();
    while rev.len() > 1 {
        let mut next = Vec::with_capacity(rev.len().div_ceil(2));
        for pair in rev.chunks(2) {
            if pair.len() == 1 {
                next.push(pair[0]);
                continue;
            }
            let mut mb = repo.merge_bases_many(pair[0], &pair[1..2])?;
            if mb.len() != 1 {
                give_up!("failed to find exact merge base");
            }
            next.push(mb.pop().expect("exactly one merge base").detach());
        }
        rev = next;
    }
    let tip = rev[0];
    let is_ancestor = repo
        .merge_bases_many(base, &[tip])?
        .iter()
        .any(|b| b.detach() == base);
    if !is_ancestor {
        give_up!("base commit should be the ancestor of revision list");
    }
    if commits.contains(&base) {
        give_up!("base commit shouldn't be in revision list");
    }

    // `prepare_bases()`: every non-merge commit between the base and the series
    // that is not itself part of the series is a prerequisite.
    let in_series: std::collections::HashSet<ObjectId> = commits.iter().copied().collect();
    let mut patch_ids = Vec::new();
    for info in repo
        .rev_walk(commits.iter().copied())
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .with_hidden(Some(base))
        .all()?
    {
        let info = info?;
        if in_series.contains(&info.id) {
            continue;
        }
        let commit = repo.find_object(info.id)?.try_into_commit()?;
        if commit.parent_ids().count() > 1 {
            continue;
        }
        patch_ids.push(commit_patch_id(repo, &commit)?);
    }
    Ok(Ok(Some(Bases { base, patch_ids })))
}

/// Port of `print_bases()` (builtin/log.c). Emitted once per series, on the
/// cover letter when there is one and otherwise on the first patch.
fn print_bases(out: &mut Vec<u8>, bases: &Bases) {
    out.extend_from_slice(format!("\nbase-commit: {}\n", bases.base).as_bytes());
    for id in bases.patch_ids.iter().rev() {
        out.extend_from_slice(format!("prerequisite-patch-id: {id}\n").as_bytes());
    }
}

/// Port of `commit_patch_id()` (patch-ids.c) plus `diff_get_patch_id()`
/// (diff.c) in its unstable form — the one `prepare_bases()` asks for.
///
/// The hash is fed a canonical, header-only rendering of each file pair
/// (`diff--git`, `a/`+`b/` paths with whitespace removed, mode words) followed
/// by the raw diff lines, and each file's digest is folded into the running
/// result with `flush_one_hunk()`'s carrying byte-wise sum.
fn commit_patch_id(repo: &gix::Repository, commit: &gix::Commit<'_>) -> Result<ObjectId> {
    let kind = repo.object_hash();
    let new_tree = commit.tree()?;
    let old_tree = match commit.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let changes = tree_changes(repo, old_tree.as_ref(), Some(&new_tree))?;

    let mut result = vec![0u8; kind.len_in_bytes()];
    let mut ctx = gix::hash::hasher(kind);
    for change in &changes {
        let path = change_path(change);
        let one = remove_space(path);
        let two = one.clone();
        let (old_id, new_id, old_mode, new_mode) = pair_info(change);

        ctx.update(b"diff--git");
        ctx.update(b"a/");
        ctx.update(&one);
        ctx.update(b"b/");
        ctx.update(&two);

        if old_mode == 0 {
            ctx.update(b"newfilemode");
            ctx.update(format!("{new_mode:06o}").as_bytes());
        } else if new_mode == 0 {
            ctx.update(b"deletedfilemode");
            ctx.update(format!("{old_mode:06o}").as_bytes());
        } else if old_mode != new_mode {
            ctx.update(b"oldmode");
            ctx.update(format!("{old_mode:06o}").as_bytes());
            ctx.update(b"newmode");
            ctx.update(format!("{new_mode:06o}").as_bytes());
        }

        let old_content = match old_id {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        let new_content = match new_id {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        if is_binary(&old_content) || is_binary(&new_content) {
            // A binary pair contributes its two object names instead of a diff.
            let hex = |o: Option<(ObjectId, bool)>| {
                o.map_or_else(|| ObjectId::null(kind), |(id, _)| id)
                    .to_hex()
                    .to_string()
            };
            ctx.update(hex(old_id).as_bytes());
            ctx.update(hex(new_id).as_bytes());
        } else {
            if old_mode == 0 {
                ctx.update(b"---/dev/null");
                ctx.update(b"+++b/");
                ctx.update(&two);
            } else if new_mode == 0 {
                ctx.update(b"---a/");
                ctx.update(&one);
                ctx.update(b"+++/dev/null");
            } else {
                ctx.update(b"---a/");
                ctx.update(&one);
                ctx.update(b"+++b/");
                ctx.update(&two);
            }
            let input = InternedInput::new(old_content.as_slice(), new_content.as_slice());
            // `xecfg.ctxlen = 3`, `xecfg.flags = XDL_EMIT_NO_HUNK_HDR`, and
            // Myers regardless of `--diff-algorithm`, because `prepare_bases()`
            // builds its own `diff_options`.
            //
            // git passes `xpp.flags = 0` here, i.e. `xdl_change_compact()`
            // without the indent heuristic, where the rendered patch uses it.
            // imara-diff exposes only the one post-processing pass, so an input
            // whose hunks the two compactions place differently would hash
            // differently; no such input turned up in differential testing.
            let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
            UnifiedDiff::new(
                &diff,
                &input,
                PatchIdWriter { ctx: &mut ctx },
                ContextSize::symmetrical(3),
            )
            .consume()?;
        }
        flush_one_hunk(&mut result, &mut ctx, kind);
    }
    Ok(ObjectId::from_bytes_or_panic(&result))
}

/// Hashes the diff body the way `patch_id_consume()` does: every line with its
/// `+`/`-`/` ` prefix and a newline, and no hunk headers
/// (`XDL_EMIT_NO_HUNK_HDR`) or `\ No newline` markers.
struct PatchIdWriter<'a> {
    ctx: &'a mut gix::hash::Hasher,
}
impl ConsumeHunk for PatchIdWriter<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        _header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        for &(kind, content) in lines {
            let mut line = Vec::with_capacity(content.len() + 2);
            line.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            line.extend_from_slice(content);
            // xdiff's incomplete-line record still ends the hashed line with a
            // newline; only the `\ No newline` marker itself is skipped.
            if !content.ends_with(b"\n") {
                line.push(b'\n');
            }
            // `patch_id_consume()` strips whitespace before hashing, which is
            // what makes a patch id insensitive to indentation-only rewrites.
            self.ctx.update(&remove_space(&line));
        }
        Ok(())
    }

    fn finish(self) {}
}

/// Port of `flush_one_hunk()` (diff.c): fold this file's digest into the
/// running result as a byte-wise sum with carry, and restart the context.
fn flush_one_hunk(result: &mut [u8], ctx: &mut gix::hash::Hasher, kind: gix::hash::Kind) {
    let digest = std::mem::replace(ctx, gix::hash::hasher(kind)).try_finalize();
    let digest = digest.expect("a hash context always finalizes").as_slice().to_vec();
    let mut carry: u16 = 0;
    for i in 0..result.len() {
        carry += u16::from(result[i]) + u16::from(digest[i]);
        result[i] = carry as u8;
        carry >>= 8;
    }
}

/// The `(pre-image, post-image, old mode, new mode)` of one change, in the
/// shape `diff_get_patch_id()` reads a `diff_filepair` — a missing side is mode
/// `0`, which is how git spells creation and deletion there.
#[allow(clippy::type_complexity)]
fn pair_info(
    change: &ChangeDetached,
) -> (
    Option<(ObjectId, bool)>,
    Option<(ObjectId, bool)>,
    u32,
    u32,
) {
    match change {
        ChangeDetached::Addition {
            entry_mode, id, ..
        } => (
            None,
            Some((*id, entry_mode.is_commit())),
            0,
            entry_mode.value().into(),
        ),
        ChangeDetached::Deletion {
            entry_mode, id, ..
        } => (
            Some((*id, entry_mode.is_commit())),
            None,
            entry_mode.value().into(),
            0,
        ),
        ChangeDetached::Modification {
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => (
            Some((*previous_id, previous_entry_mode.is_commit())),
            Some((*id, entry_mode.is_commit())),
            previous_entry_mode.value().into(),
            entry_mode.value().into(),
        ),
        // Never produced: rewrite tracking is off via Options::default().
        ChangeDetached::Rewrite { .. } => (None, None, 0, 0),
    }
}

/// git's `buffer_is_binary()`: a NUL in the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(8000)].contains(&0)
}

/// Port of `remove_space()` (diff.c): git's own `isspace` class removed from a
/// path before it is hashed.
/// git's `isspace` is `sane_ctype`'s `GIT_SPACE`, which covers only space, tab,
/// newline and carriage return — vertical tab and form feed are *kept*.
fn remove_space(path: &[u8]) -> Vec<u8> {
    path.iter()
        .copied()
        .filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        .collect()
}

fn write_signature(out: &mut Vec<u8>, opts: &Opts) {
    if opts.signature.is_empty() {
        return;
    }
    out.extend_from_slice(b"-- \n");
    out.extend_from_slice(opts.signature.as_bytes());
    if !opts.signature.ends_with('\n') {
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// The file-level changes between two trees, in path order.
///
/// `tree_with_rewrites` reports the directory entry *and* its recursed contents;
/// git's patch format only names blobs and submodules, so tree entries are
/// dropped — keeping one would render a raw tree object as a binary file.
fn tree_changes(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: Option<&gix::Tree<'_>>,
) -> Result<Vec<ChangeDetached>> {
    let mut changes =
        repo.diff_tree_to_tree(old_tree, new_tree, gix::diff::Options::default())?;
    changes.retain(|c| !is_tree_entry(c));
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));
    Ok(changes)
}

fn is_tree_entry(change: &ChangeDetached) -> bool {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. }
        | ChangeDetached::Modification { entry_mode, .. }
        | ChangeDetached::Rewrite { entry_mode, .. } => entry_mode.is_tree(),
    }
}

// ---------------------------------------------------------------------------
// Commit-message plumbing (pretty.c)
// ---------------------------------------------------------------------------

/// git's `get_one_line`: the length of the next line, newline included.
fn one_line(msg: &[u8]) -> usize {
    match msg.iter().position(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => msg.len(),
    }
}

/// git's `is_blank_line`: right-trim the line and report whether nothing is left.
/// Returns the trimmed slice alongside the verdict.
fn blank_line(line: &[u8]) -> (&[u8], bool) {
    let t = trim_end_ws(line);
    (t, t.is_empty())
}

/// Strip trailing ASCII whitespace (git's `isspace` set).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// git's `skip_blank_lines`: advance past leading blank lines.
fn skip_blank_lines(mut msg: &[u8]) -> &[u8] {
    loop {
        let len = one_line(msg);
        if len == 0 {
            return msg;
        }
        if !blank_line(&msg[..len]).1 {
            return msg;
        }
        msg = &msg[len..];
    }
}

/// git's `format_subject` with a `" "` separator: join the first paragraph into
/// one line and return it together with the rest of the message.
fn format_subject(mut msg: &[u8]) -> (Vec<u8>, &[u8]) {
    let mut title: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        if is_blank {
            break;
        }
        msg = &msg[len..];
        if !first {
            title.push(b' ');
        }
        title.extend_from_slice(trimmed);
        first = false;
    }
    (title, msg)
}

/// git's `pp_remainder` with zero indent: skip leading blank lines, then emit
/// every remaining line right-trimmed.
fn pp_remainder(mut msg: &[u8], out: &mut Vec<u8>) {
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        msg = &msg[len..];
        if is_blank && first {
            continue;
        }
        first = false;
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
}

// ---------------------------------------------------------------------------
// Header encoding and wrapping (pretty.c, utf8.c)
// ---------------------------------------------------------------------------

/// Bytes already used on the last line of `sb` (git's `last_line_length`).
fn last_line_length(sb: &str) -> i64 {
    match sb.rfind('\n') {
        Some(i) => (sb.len() - i - 1) as i64,
        None => sb.len() as i64,
    }
}

/// git's `needs_rfc2047_encoding`: any non-ASCII byte, a newline, or a literal
/// `=?` sequence forces the encoded-word form.
fn needs_rfc2047_encoding(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &ch) in b.iter().enumerate() {
        if ch >= 0x80 || ch == b'\n' {
            return true;
        }
        if i + 1 < b.len() && ch == b'=' && b[i + 1] == b'?' {
            return true;
        }
    }
    false
}

/// git's `is_rfc822_special`.
fn is_rfc822_special(ch: u8) -> bool {
    matches!(
        ch,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b':' | b';' | b'@' | b',' | b'.' | b'"' | b'\\'
    )
}

/// git's `add_rfc822_quoted`: wrap in double quotes, backslash-escaping `"`/`\`.
fn rfc822_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// git's `is_rfc2047_special`. `address` selects the stricter `phrase` rules
/// used for the `From:` display name.
fn is_rfc2047_special(ch: u8, address: bool) -> bool {
    if ch >= 0x80 || !(0x20..0x7f).contains(&ch) {
        return true;
    }
    if ch.is_ascii_whitespace() || ch == b'=' || ch == b'?' || ch == b'_' {
        return true;
    }
    if !address {
        return false;
    }
    !(ch.is_ascii_alphanumeric() || matches!(ch, b'!' | b'*' | b'+' | b'-' | b'/'))
}

/// Port of `add_rfc2047()` (pretty.c): q-encoded words, never splitting a
/// multi-byte character, folded at 76 columns.
fn add_rfc2047(sb: &mut String, line: &str, address: bool) {
    const MAX_ENCODED_LENGTH: i64 = 76;
    let mut line_len = last_line_length(sb);

    sb.push_str(&format!("=?{ENCODING}?q?"));
    line_len += ENCODING.len() as i64 + 5;

    for c in line.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let chrlen = bytes.len() as i64;
        let is_special = chrlen > 1 || is_rfc2047_special(bytes[0], address);
        let encoded_len = if is_special { 3 * chrlen } else { 1 };

        if line_len + encoded_len + 2 > MAX_ENCODED_LENGTH {
            sb.push_str(&format!("?=\n =?{ENCODING}?q?"));
            line_len = ENCODING.len() as i64 + 5 + 1;
        }
        for &b in bytes {
            if is_special {
                sb.push_str(&format!("={b:02X}"));
            } else {
                sb.push(b as char);
            }
        }
        line_len += encoded_len;
    }
    sb.push_str("?=");
}

/// Port of `strbuf_add_wrapped_text()` (utf8.c) for the ASCII inputs that reach
/// it — anything non-ASCII takes the RFC2047 path above, and neither the subject
/// (paragraph joined with spaces) nor a display name can contain a newline, so
/// the original's embedded-newline branch is unreachable here.
///
/// A negative `indent1` means that many columns are already consumed.
fn wrap_text(buf: &mut String, text: &str, indent1: i64, indent2: i64, width: i64) {
    if width <= 0 {
        buf.push_str(text);
        return;
    }
    let b = text.as_bytes();
    let mut indent = indent1;
    let mut w = indent1;
    let mut bol: usize = 0;
    let mut space: Option<usize> = None;
    let mut i: usize = 0;

    if indent < 0 {
        w = -indent;
        space = Some(0);
    }

    loop {
        let c = b.get(i).copied().unwrap_or(0);
        if c == 0 || c.is_ascii_whitespace() {
            if w <= width || space.is_none() {
                // git checks the empty-tail case against `bol`, before the
                // remembered space overrides the copy start.
                if c == 0 && i == bol {
                    return;
                }
                let start = match space {
                    Some(s) => s,
                    None => {
                        if indent > 0 {
                            buf.push_str(&" ".repeat(indent as usize));
                        }
                        bol
                    }
                };
                buf.push_str(&text[start..i]);
                if c == 0 {
                    return;
                }
                space = Some(i);
                if c == b'\t' {
                    w |= 0x07;
                }
                w += 1;
                i += 1;
            } else {
                // Break the line at the last remembered space.
                buf.push('\n');
                let s = space.expect("the else branch requires a remembered space");
                // `*space` reads the NUL terminator in git when the remembered
                // position is the end of the text; that is not whitespace.
                let at_space = b.get(s).copied().unwrap_or(0).is_ascii_whitespace();
                i = s + usize::from(at_space);
                bol = i;
                space = None;
                indent = indent2;
                w = indent2;
            }
            continue;
        }
        w += 1;
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row: the quoted path and its line counts. `raw_name` is the
/// unquoted path, which is what git's `dirstat` groups on.
struct StatEntry {
    name: String,
    raw_name: Vec<u8>,
    added: u64,
    deleted: u64,
}

/// The four `diff_options` knobs `show_stats()` reads, in git's own units (0 is
/// each field's "unset" sentinel). Carried apart from `Opts` so `show_stats()`
/// can be exercised without building the whole option set.
#[derive(Clone, Copy)]
struct StatWidths {
    width: i64,
    name_width: i64,
    graph_width: i64,
    count: i64,
}

impl StatWidths {
    fn from_opts(o: &Opts) -> StatWidths {
        StatWidths {
            width: o.stat_width,
            name_width: o.stat_name_width,
            graph_width: o.stat_graph_width,
            count: o.stat_count,
        }
    }
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (git measures terminal columns; wide
/// characters are counted as 1 here, see the module note).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// Port of `show_stats()` (diff.c). format-patch's default width is
/// `MAIL_DEFAULT_WRAP` (72), overridable by `--stat`/`--stat-width`; the filename
/// and graph columns and the list length honor `--stat-name-width`,
/// `--stat-graph-width` and `--stat-count`. Followed by
/// `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut Vec<u8>, files: &[StatEntry], sw: StatWidths) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    // git's `count = stat_count ? stat_count : nr`, then the scan loop
    // `for (i = 0; i < count && i < nr; i++)` sets `count = i`, so a non-zero
    // `--stat-count` is clamped into `[0, nr]` (a negative value shows nothing).
    // Only the shown files are scanned for the longest name / largest change, so
    // `--stat-count` narrows the columns too. Every file here is "interesting"
    // (binary is refused, no unmerged entries), so no scan step is skipped.
    let count = if sw.count != 0 {
        sw.count.clamp(0, files.len() as i64) as usize
    } else {
        files.len()
    };

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    for f in &files[..count] {
        max_len = max_len.max(display_width(&f.name));
        max_change = max_change.max((f.added + f.deleted) as i64);
    }

    // `width = stat_width ? stat_width : 80` in git, but format-patch first
    // bumps a 0 to `MAIL_DEFAULT_WRAP` (72), so 72 is the effective default and
    // the `: 80` branch is never taken here. `stat_width` is never git's `-1`
    // sentinel for format-patch, so `term_columns()` is never consulted.
    let mut width = if sw.width != 0 {
        sw.width
    } else {
        MAIL_DEFAULT_WRAP
    };
    let number_width = decimal_width(max_change as u64) as i64;
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    // bin_width is 0 (binary files are refused), so graph_width starts at
    // max_change; a non-zero `--stat-graph-width` caps it.
    let mut graph_width = max_change;
    if sw.graph_width != 0 && sw.graph_width < graph_width {
        graph_width = sw.graph_width;
    }
    let mut name_width = if sw.name_width > 0 && sw.name_width < max_len {
        sw.name_width
    } else {
        max_len
    };
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if sw.graph_width != 0 && graph_width > sw.graph_width {
            graph_width = sw.graph_width;
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in &files[..count] {
        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        write!(
            out,
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
            nw = number_width as usize,
        )?;
        for _ in 0..add.max(0) {
            out.push(b'+');
        }
        for _ in 0..del.max(0) {
            out.push(b'-');
        }
        out.push(b'\n');
    }

    // git's `DIFF_SYMBOL_STATS_SUMMARY_ABBREV`, emitted once when `--stat-count`
    // hid at least one file. The insertions/deletions summary still counts every
    // file, so it is fed the full slice.
    if count < files.len() {
        out.extend_from_slice(b" ...\n");
    }

    emit_stat_summary(out, files)
}

/// Port of `print_stat_summary_inserts_deletes()` (diff.c) — the trailing
/// ` N files changed, …` line, which is also the whole of `--shortstat`.
///
/// Every file this module reaches is "interesting" in git's sense
/// (`is_interesting = p->status != DIFF_STATUS_UNKNOWN`) and binary content is
/// refused outright, so `show_stats()` and `show_shortstats()` count the same
/// way and share this one implementation.
fn emit_stat_summary(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let adds: u64 = files.iter().map(|f| f.added).sum();
    let dels: u64 = files.iter().map(|f| f.deleted).sum();

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 {
                "insertion(+)"
            } else {
                "insertions(+)"
            }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 {
                "deletion(-)"
            } else {
                "deletions(-)"
            }
        ));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

/// Port of `show_numstat()` (diff.c): tab-separated counts and the C-quoted path.
fn emit_numstat(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    for f in files {
        writeln!(out, "{}\t{}\t{}", f.added, f.deleted, f.name)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dirstat (diff.c, diffcore-delta.c)
// ---------------------------------------------------------------------------

/// One entry of git's `struct dirstat_dir`: a path and its damage.
struct DirstatFile {
    name: Vec<u8>,
    changed: u64,
}

/// Port of `show_dirstat()` (diff.c). Damage is measured in bytes of the
/// pre-image that did not survive plus bytes that are new, except in
/// `--dirstat-by-file` mode where every changed file counts as exactly one.
fn emit_dirstat(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    cfg: &Dirstat,
) -> Result<()> {
    let mut files: Vec<DirstatFile> = Vec::new();
    let mut changed: u64 = 0;

    for change in changes {
        let damage = match change {
            // An unchanged blob id means identical content, whatever else moved.
            ChangeDetached::Modification {
                previous_id, id, ..
            } if previous_id == id => 0,
            _ if cfg.by_file => 1,
            ChangeDetached::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                let old = content_of(repo, *previous_id, previous_entry_mode.is_commit())?;
                let new = content_of(repo, *id, entry_mode.is_commit())?;
                let (copied, added) = count_changes(&old, &new);
                // Original minus copied is the removed material; `added` is the
                // new material. Both are damage done to the pre-image, and a
                // changed id always means at least one unit of it.
                ((old.len() as u64 - copied) + added).max(1)
            }
            ChangeDetached::Deletion {
                entry_mode, id, ..
            } => content_of(repo, *id, entry_mode.is_commit())?.len() as u64,
            ChangeDetached::Addition {
                entry_mode, id, ..
            } => content_of(repo, *id, entry_mode.is_commit())?.len() as u64,
            ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
        };
        files.push(DirstatFile {
            name: change_path(change).to_vec(),
            changed: damage,
        });
        changed += damage;
    }

    conclude_dirstat(out, files, changed, cfg)
}

/// Port of `show_dirstat_by_line()` (diff.c): the same report, with damage taken
/// from the diffstat's line counts instead of from the blob contents.
fn emit_dirstat_by_line(out: &mut Vec<u8>, stats: &[StatEntry], cfg: &Dirstat) -> Result<()> {
    if stats.is_empty() {
        return Ok(());
    }
    let mut changed: u64 = 0;
    let files: Vec<DirstatFile> = stats
        .iter()
        .map(|f| {
            let damage = f.added + f.deleted;
            changed += damage;
            DirstatFile {
                name: f.raw_name.clone(),
                changed: damage,
            }
        })
        .collect();
    conclude_dirstat(out, files, changed, cfg)
}

/// Port of `conclude_dirstat()` (diff.c): sort by path, then walk.
fn conclude_dirstat(
    out: &mut Vec<u8>,
    mut files: Vec<DirstatFile>,
    changed: u64,
    cfg: &Dirstat,
) -> Result<()> {
    if changed == 0 {
        return Ok(());
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let mut cursor = 0usize;
    gather_dirstat(out, &files, &mut cursor, changed, 0, cfg)?;
    Ok(())
}

/// Port of `gather_dirstat()` (diff.c).
///
/// `cursor` is git's consuming `dir->files++`/`dir->nr--`: the recursion walks
/// the sorted list once, and each level reports the directory named by the first
/// `baselen` bytes of the entry that opened it. A directory is silent at the top
/// level and whenever everything under it came from a single subdirectory
/// (`sources == 1`), which is what keeps the report to the branch points.
fn gather_dirstat(
    out: &mut Vec<u8>,
    files: &[DirstatFile],
    cursor: &mut usize,
    changed: u64,
    baselen: usize,
    cfg: &Dirstat,
) -> Result<u64> {
    let mut sum_changes: u64 = 0;
    let mut sources = 0u32;
    // The base is a prefix of the entry that opened this level; borrowing it
    // across the recursion would alias `files`, so it is captured up front.
    let base: Vec<u8> = files
        .get(*cursor)
        .map(|f| f.name[..baselen.min(f.name.len())].to_vec())
        .unwrap_or_default();

    while *cursor < files.len() {
        let name = &files[*cursor].name;
        if name.len() < baselen || name[..baselen] != base[..] {
            break;
        }
        let changes = match name[baselen..].iter().position(|&b| b == b'/') {
            Some(slash) => {
                let newbaselen = baselen + slash + 1;
                sources += 1;
                gather_dirstat(out, files, cursor, changed, newbaselen, cfg)?
            }
            None => {
                let changes = files[*cursor].changed;
                *cursor += 1;
                sources += 2;
                changes
            }
        };
        sum_changes += changes;
    }

    if baselen > 0 && sources != 1 && sum_changes > 0 {
        let permille = sum_changes * 1000 / changed;
        if permille >= u64::from(cfg.permille) {
            write!(out, "{:4}.{}% ", permille / 10, permille % 10)?;
            out.extend_from_slice(&base);
            out.push(b'\n');
            if !cfg.cumulative {
                return Ok(0);
            }
        }
    }
    Ok(sum_changes)
}

/// Port of `diffcore_count_changes()` (diffcore-delta.c): returns
/// `(src_copied, literal_added)` for the byte-level dirstat.
///
/// Both buffers are cut into chunks that end at an LF or after 64 bytes,
/// whichever comes first, and the chunks are hashed into counting buckets. A
/// chunk the destination has at least as many of as the source was copied; the
/// surplus on either side is what changed.
fn count_changes(src: &[u8], dst: &[u8]) -> (u64, u64) {
    let src_count = hash_chars(src);
    let dst_count = hash_chars(dst);

    let (mut sc, mut la) = (0u64, 0u64);
    let (mut s, mut d) = (0usize, 0usize);
    while s < src_count.len() && src_count[s].1 != 0 {
        while d < dst_count.len() && dst_count[d].1 != 0 {
            if dst_count[d].0 >= src_count[s].0 {
                break;
            }
            la += u64::from(dst_count[d].1);
            d += 1;
        }
        let src_cnt = src_count[s].1;
        let mut dst_cnt = 0u32;
        if d < dst_count.len() && dst_count[d].1 != 0 && dst_count[d].0 == src_count[s].0 {
            dst_cnt = dst_count[d].1;
            d += 1;
        }
        if src_cnt < dst_cnt {
            la += u64::from(dst_cnt - src_cnt);
            sc += u64::from(src_cnt);
        } else {
            sc += u64::from(dst_cnt);
        }
        s += 1;
    }
    while d < dst_count.len() && dst_count[d].1 != 0 {
        la += u64::from(dst_count[d].1);
        d += 1;
    }
    (sc, la)
}

/// git's `HASHBASE`: a prime chosen so the table never has to grow past 2^18.
const HASHBASE: u32 = 107_927;

/// git's `INITIAL_HASH_SIZE`.
const INITIAL_HASH_LOG2: u32 = 9;

/// git's `INITIAL_FREE`: leave proportionally more slack in a small table.
fn initial_free(log2: u32) -> i64 {
    i64::from((1u32 << log2) * (log2 - 3) / log2)
}

/// Port of `hash_chars()` (diffcore-delta.c): the chunked rolling hash, returned
/// as git leaves it — a power-of-two table sorted so that live buckets come
/// first, in hash order, and empty ones sort to the end.
fn hash_chars(buf: &[u8]) -> Vec<(u32, u32)> {
    // `is_text` only controls CRLF folding; binary content never reaches here.
    let mut table: Vec<(u32, u32)> = vec![(0, 0); 1 << INITIAL_HASH_LOG2];
    let mut log2 = INITIAL_HASH_LOG2;
    let mut free = initial_free(log2);

    let (mut accum1, mut accum2) = (0u32, 0u32);
    let mut n = 0u32;
    let mut i = 0usize;
    while i < buf.len() {
        let c = buf[i];
        i += 1;
        // Ignore CR in a CRLF sequence.
        if c == b'\r' && buf.get(i) == Some(&b'\n') {
            continue;
        }
        let old_1 = accum1;
        accum1 = (accum1 << 7) ^ (accum2 >> 25);
        accum2 = (accum2 << 7) ^ (old_1 >> 25);
        accum1 = accum1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        add_spanhash(&mut table, &mut log2, &mut free, hashval, n);
        n = 0;
        accum1 = 0;
        accum2 = 0;
    }
    if n > 0 {
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        add_spanhash(&mut table, &mut log2, &mut free, hashval, n);
    }

    // git's `spanhash_cmp`: empty buckets last, live ones by hash value.
    table.sort_by(|a, b| match (a.1 == 0, b.1 == 0) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.0.cmp(&b.0),
    });
    table
}

/// Port of `add_spanhash()` + `spanhash_rehash()` (diffcore-delta.c): linear
/// probing, and a doubling rehash once the free budget is spent.
fn add_spanhash(table: &mut Vec<(u32, u32)>, log2: &mut u32, free: &mut i64, hashval: u32, cnt: u32) {
    let lim = 1usize << *log2;
    let mut bucket = (hashval as usize) & (lim - 1);
    loop {
        let slot = &mut table[bucket];
        bucket += 1;
        if slot.1 == 0 {
            *slot = (hashval, cnt);
            *free -= 1;
            if *free < 0 {
                spanhash_rehash(table, log2, free);
            }
            return;
        }
        if slot.0 == hashval {
            slot.1 += cnt;
            return;
        }
        if lim <= bucket {
            bucket = 0;
        }
    }
}

fn spanhash_rehash(table: &mut Vec<(u32, u32)>, log2: &mut u32, free: &mut i64) {
    let sz = 1usize << (*log2 + 1);
    let mut grown: Vec<(u32, u32)> = vec![(0, 0); sz];
    *log2 += 1;
    *free = initial_free(*log2);
    for &(hashval, cnt) in table.iter() {
        if cnt == 0 {
            continue;
        }
        let mut bucket = (hashval as usize) & (sz - 1);
        loop {
            let slot = &mut grown[bucket];
            bucket += 1;
            if slot.1 == 0 {
                *slot = (hashval, cnt);
                *free -= 1;
                break;
            }
            if sz <= bucket {
                bucket = 0;
            }
        }
    }
    *table = grown;
}

/// Port of `is_summary_empty()` (diff.c): whether `--summary` would print
/// nothing, which decides whether it counts toward `diff_flush()`'s separator.
fn is_summary_empty(changes: &[ChangeDetached]) -> bool {
    !changes.iter().any(|c| match c {
        ChangeDetached::Addition { .. }
        | ChangeDetached::Deletion { .. }
        | ChangeDetached::Rewrite { .. } => true,
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } => previous_entry_mode.value() != entry_mode.value(),
    })
}

/// Port of `diff_summary()` (diff.c): the `create`/`delete`/`mode change` lines
/// that follow the diffstat. Rewrites never occur (rename detection is off).
fn emit_summary(out: &mut Vec<u8>, changes: &[ChangeDetached]) -> Result<()> {
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " create mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Deletion {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " delete mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    writeln!(
                        out,
                        " mode change {:06o} => {:06o} {}",
                        previous_entry_mode.value(),
                        entry_mode.value(),
                        quote_path(location)
                    )?;
                }
            }
            ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Patch body (shared shape with `show`)
// ---------------------------------------------------------------------------

/// Render one file-level change as a `diff --git` block, returning its stat row.
fn emit_change(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    abbrev: usize,
    opts: &Opts,
) -> Result<StatEntry> {
    let mut counts = (0u64, 0u64);
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            writeln!(out, "new file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            reject_binary(is_sub, &content, path, opts)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", "0".repeat(short.len()), short)?;
            counts = emit_body(out, None, Some(path), &[], &content, opts)?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            writeln!(out, "deleted file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            reject_binary(is_sub, &content, path, opts)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", short, "0".repeat(short.len()))?;
            counts = emit_body(out, Some(path), None, &content, &[], opts)?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            let old_mode = format!("{:o}", previous_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            let mode_changed = old_mode != new_mode;
            if mode_changed {
                writeln!(out, "old mode {old_mode}")?;
                writeln!(out, "new mode {new_mode}")?;
            }
            // A pure mode change (identical content) prints no index/hunks.
            if previous_id != id {
                let old_is_sub = previous_entry_mode.is_commit();
                let new_is_sub = entry_mode.is_commit();
                let old_content = content_of(repo, *previous_id, old_is_sub)?;
                let new_content = content_of(repo, *id, new_is_sub)?;
                reject_binary(old_is_sub, &old_content, path, opts)?;
                reject_binary(new_is_sub, &new_content, path, opts)?;
                let old_short = short_oid(repo, *previous_id, abbrev, old_is_sub)?;
                let new_short = short_oid(repo, *id, abbrev, new_is_sub)?;
                // The mode suffix is dropped when `old mode`/`new mode` said it.
                if mode_changed {
                    writeln!(out, "index {old_short}..{new_short}")?;
                } else {
                    writeln!(out, "index {old_short}..{new_short} {new_mode}")?;
                }
                counts = emit_body(
                    out,
                    Some(path),
                    Some(path),
                    &old_content,
                    &new_content,
                    opts,
                )?;
            }
        }
        // Never produced: rewrite tracking is off via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
    Ok(StatEntry {
        name: quote_path(change_path(change)),
        raw_name: change_path(change).to_vec(),
        added: counts.0,
        deleted: counts.1,
    })
}

/// format-patch implies `--binary`, whose base85 `GIT binary patch` payload is
/// not ported; refuse rather than emit a textual approximation. `-a`/`--text`
/// asks for exactly that textual rendering, so it is honoured.
fn reject_binary(is_submodule: bool, content: &[u8], path: &[u8], opts: &Opts) -> Result<()> {
    if !opts.text && !is_submodule && content.iter().take(8000).any(|&b| b == 0) {
        bail!(
            "binary file {:?}: the GIT binary patch encoding is not ported",
            path.as_bstr()
        );
    }
    Ok(())
}

/// `diff --git a/<path> b/<path>` line, with git's `quote_two()` C-quoting.
/// Under `--no-prefix`/`format.noprefix` both prefixes are empty, so git emits
/// `diff --git <path> <path>`.
fn emit_git_header(out: &mut Vec<u8>, path: &[u8], opts: &Opts) {
    let (a, b) = prefixes(opts);
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&quote_two(a, path));
    out.push(b' ');
    out.extend_from_slice(&quote_two(b, path));
    out.push(b'\n');
}

/// The `a/`+`b/` source/destination prefixes, emptied by `--no-prefix` /
/// `format.noprefix`.
fn prefixes(opts: &Opts) -> (&'static str, &'static str) {
    if opts.noprefix {
        ("", "")
    } else {
        ("a/", "b/")
    }
}

/// Emit the `---`/`+++` headers and hunks, returning `(added, deleted)` line
/// counts. An add/delete of an empty file produces no header lines, like git.
fn emit_body(
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
    opts: &Opts,
) -> Result<(u64, u64)> {
    let mut hunks: Vec<u8> = Vec::new();
    let counts = emit_text_hunks(&mut hunks, old_content, new_content, opts)?;
    if hunks.is_empty() {
        return Ok(counts);
    }

    let (a, b) = prefixes(opts);
    emit_file_header(out, b"--- ", old, a);
    emit_file_header(out, b"+++ ", new, b);
    out.extend_from_slice(&hunks);
    Ok(counts)
}

/// One `---`/`+++` line. git appends a tab when the rendered name contains a
/// space, so that a reader can tell where the name ends.
fn emit_file_header(out: &mut Vec<u8>, marker: &[u8], path: Option<&[u8]>, prefix: &str) {
    out.extend_from_slice(marker);
    let name = match path {
        Some(p) => quote_two(prefix, p),
        None => b"/dev/null".to_vec(),
    };
    out.extend_from_slice(&name);
    if name.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

/// Compute the unified diff of two blobs, returning the added/deleted line
/// counts the diffstat needs.
fn emit_text_hunks(
    out: &mut Vec<u8>,
    old: &[u8],
    new: &[u8],
    opts: &Opts,
) -> Result<(u64, u64)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(opts.algorithm, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();

    if !opts.ignore_regex.is_empty() {
        let after_lines: Vec<&[u8]> = input.after.iter().map(|&t| input.interner[t]).collect();
        return emit_hunks_with_ignorable(out, &diff, &before_lines, &after_lines, opts);
    }

    let writer = HunkWriter {
        out,
        before_lines,
        added: 0,
        deleted: 0,
    };
    let counts = UnifiedDiff::new(
        &diff,
        &input,
        writer,
        ContextSize::symmetrical(opts.context),
    )
    .consume()?;
    Ok(counts)
}

/// One entry of xdiff's edit script (`struct xdchange`).
struct Change {
    i1: u32,
    chg1: u32,
    i2: u32,
    chg2: u32,
    ignore: bool,
}

/// Port of `xdl_emit_diff()` (xdiff/xemit.c) without `XDL_EMIT_FUNCCONTEXT`,
/// used only when `-I` is in play.
///
/// gix's `UnifiedDiff` groups every change into a hunk, which is right until a
/// change can be *ignorable*: git marks those in `xdl_mark_ignorable_regex()`
/// and then lets `xdl_get_hunk()` drop the ones that no real change is holding
/// in place. An ignorable change close to a real one is still printed — the
/// regex suppresses hunks, not lines — so the two must be decided together.
fn emit_hunks_with_ignorable(
    out: &mut Vec<u8>,
    diff: &gix::diff::blob::Diff,
    before_lines: &[&[u8]],
    after_lines: &[&[u8]],
    opts: &Opts,
) -> Result<(u64, u64)> {
    let matches_all = |lines: &[&[u8]]| -> bool {
        lines
            .iter()
            .all(|line| opts.ignore_regex.iter().any(|re| re.is_match(line)))
    };
    let changes: Vec<Change> = diff
        .hunks()
        .map(|h| Change {
            i1: h.before.start,
            chg1: h.before.end - h.before.start,
            i2: h.after.start,
            chg2: h.after.end - h.after.start,
            // A group is ignorable when every line it touches, on both sides,
            // matches; `xdl_mark_ignorable_regex()` starts from `ignore = 1`, so
            // an empty side simply does not object.
            ignore: matches_all(&before_lines[h.before.start as usize..h.before.end as usize])
                && matches_all(&after_lines[h.after.start as usize..h.after.end as usize]),
        })
        .collect();

    let ctx = i64::from(opts.context);
    let nrec1 = before_lines.len() as i64;
    let nrec2 = after_lines.len() as i64;

    let mut writer = HunkWriter {
        out,
        before_lines: before_lines.to_vec(),
        added: 0,
        deleted: 0,
    };

    let mut idx = 0usize;
    while idx < changes.len() {
        let mut start = idx;
        let Some(last) = get_hunk(&changes, &mut start, ctx) else {
            break;
        };
        let (first, last) = (start, last);
        let (f, e) = (&changes[first], &changes[last]);

        let s1 = (i64::from(f.i1) - ctx).max(0);
        let s2 = (i64::from(f.i2) - ctx).max(0);
        // Trailing context stops at whichever file runs out first.
        let lctx = ctx
            .min(nrec1 - i64::from(e.i1 + e.chg1))
            .min(nrec2 - i64::from(e.i2 + e.chg2));
        let e1 = i64::from(e.i1 + e.chg1) + lctx;
        let e2 = i64::from(e.i2 + e.chg2) + lctx;

        let mut lines: Vec<(DiffLineKind, &[u8])> = Vec::new();
        // Leading context, taken from the post-image as xdiff does.
        for l in s2..i64::from(f.i2) {
            lines.push((DiffLineKind::Context, after_lines[l as usize]));
        }
        let (mut c1, mut c2) = (i64::from(f.i1), i64::from(f.i2));
        for ch in &changes[first..=last] {
            // Context bridging this change and the previous one in the hunk.
            while c1 < i64::from(ch.i1) && c2 < i64::from(ch.i2) {
                lines.push((DiffLineKind::Context, after_lines[c2 as usize]));
                c1 += 1;
                c2 += 1;
            }
            for l in ch.i1..ch.i1 + ch.chg1 {
                lines.push((DiffLineKind::Remove, before_lines[l as usize]));
            }
            for l in ch.i2..ch.i2 + ch.chg2 {
                lines.push((DiffLineKind::Add, after_lines[l as usize]));
            }
            c1 = i64::from(ch.i1 + ch.chg1);
            c2 = i64::from(ch.i2 + ch.chg2);
        }
        for l in i64::from(e.i2 + e.chg2)..e2 {
            lines.push((DiffLineKind::Context, after_lines[l as usize]));
        }

        let header = HunkHeader {
            before_hunk_start: (s1 + 1) as u32,
            before_hunk_len: (e1 - s1) as u32,
            after_hunk_start: (s2 + 1) as u32,
            after_hunk_len: (e2 - s2) as u32,
        };
        writer.consume_hunk(header, &lines)?;

        idx = last + 1;
    }

    Ok(writer.finish())
}

/// Port of `xdl_get_hunk()` (xdiff/xemit.c) with `interhunkctxlen` zero.
///
/// Advances `start` past leading ignorable changes that no following change is
/// close enough to rescue, then returns the index of the last change that
/// belongs in the same hunk — or `None` once nothing is left to show.
fn get_hunk(changes: &[Change], start: &mut usize, ctxlen: i64) -> Option<usize> {
    let max_common = ctxlen + ctxlen;
    let max_ignorable = ctxlen;
    let end_of = |i: usize| i64::from(changes[i].i1 + changes[i].chg1);

    let mut p = *start;
    while p < changes.len() && changes[p].ignore {
        let next = p + 1;
        if next >= changes.len() || i64::from(changes[next].i1) - end_of(p) >= max_ignorable {
            *start = next;
        }
        p = next;
    }
    if *start >= changes.len() {
        return None;
    }

    let mut ignored: i64 = 0;
    let mut last = *start;
    let mut prev = *start;
    let mut cur = *start + 1;
    while cur < changes.len() {
        let distance = i64::from(changes[cur].i1) - end_of(prev);
        if distance > max_common {
            break;
        }
        if distance < max_ignorable && (!changes[cur].ignore || last == prev) {
            last = cur;
            ignored = 0;
        } else if distance < max_ignorable && changes[cur].ignore {
            ignored += i64::from(changes[cur].chg2);
        } else if last != prev && i64::from(changes[cur].i1) + ignored - end_of(last) > max_common {
            break;
        } else if !changes[cur].ignore {
            last = cur;
            ignored = 0;
        } else {
            ignored += i64::from(changes[cur].chg2);
        }
        prev = cur;
        cur += 1;
    }
    Some(last)
}

/// Writes hunks in git's unified-diff style and tallies changed lines.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving each hunk header's function context.
    before_lines: Vec<&'a [u8]>,
    added: u64,
    deleted: u64,
}

impl<'a> HunkWriter<'a> {
    /// Nearest "function" line above the hunk's leading context, mirroring git's
    /// default (no `xfuncname`) heuristic: first byte is a letter, `_`, or `$`.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
        let ctx_start = before_hunk_start.saturating_sub(1);
        let mut idx = ctx_start as i64 - 1;
        while idx >= 0 {
            let line = trim_end_ws(self.before_lines[idx as usize]);
            if let Some(&first) = line.first() {
                if first.is_ascii_alphabetic() || first == b'_' || first == b'$' {
                    return Some(line);
                }
            }
            idx -= 1;
        }
        None
    }
}

impl ConsumeHunk for HunkWriter<'_> {
    type Out = (u64, u64);

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.out.extend_from_slice(b"@@ -");
        write_range(self.out, header.before_hunk_start, header.before_hunk_len);
        self.out.extend_from_slice(b" +");
        write_range(self.out, header.after_hunk_start, header.after_hunk_len);
        self.out.extend_from_slice(b" @@");
        if let Some(func) = self.find_func(header.before_hunk_start) {
            self.out.push(b' ');
            self.out.extend_from_slice(func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => {
                    self.added += 1;
                    b'+'
                }
                DiffLineKind::Remove => {
                    self.deleted += 1;
                    b'-'
                }
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> (u64, u64) {
        (self.added, self.deleted)
    }
}

/// Port of `xdl_emit_hunk_hdr()` (xdiff): the `,len` field is omitted when the
/// hunk spans exactly one line, and an empty side is anchored to the line
/// *before* the change — which is line 0 for a file that is being created.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    let start = if len == 0 { start.saturating_sub(1) } else { start };
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}

/// The bytes to diff for an entry: a blob comes from the object database; a
/// submodule (commit entry) renders as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` line. Real objects are disambiguated
/// against the odb; a submodule commit (absent here) is plainly truncated.
fn short_oid(
    repo: &gix::Repository,
    id: ObjectId,
    abbrev: usize,
    is_submodule: bool,
) -> Result<String> {
    if is_submodule {
        Ok(id.to_hex_with_len(abbrev).to_string())
    } else {
        Ok(id.attach(repo).shorten()?.to_string())
    }
}

/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// True when `quote_c_style()` would escape any byte of `bytes`.
fn needs_c_quote(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80)
}

fn c_escape_into(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`, used for
/// the stat and summary columns (`quote_c_style`).
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    if !needs_c_quote(bytes) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    c_escape_into(&mut out, bytes);
    out.push('"');
    out
}

/// git's `quote_two()`: `<prefix><path>` is quoted as a whole when either half
/// needs escaping, so `a/` stays inside the quotes.
fn quote_two(prefix: &str, path: &[u8]) -> Vec<u8> {
    if !needs_c_quote(prefix.as_bytes()) && !needs_c_quote(path) {
        let mut out = prefix.as_bytes().to_vec();
        out.extend_from_slice(path);
        return out;
    }
    let mut out = String::from("\"");
    c_escape_into(&mut out, prefix.as_bytes());
    c_escape_into(&mut out, path);
    out.push('"');
    out.into_bytes()
}

// ---------------------------------------------------------------------------
// POSIX extended regular expressions (`regcomp(REG_EXTENDED | REG_NEWLINE)`)
// ---------------------------------------------------------------------------
//
// git compiles each `-I<regex>` with `REG_EXTENDED | REG_NEWLINE` and asks only
// whether the record matches anywhere, so this engine answers a boolean and
// keeps no capture state. `REG_NEWLINE` is the part that surprises: `^` also
// matches immediately after a newline and `$` immediately before one, so on a
// record like `deep\n` the position past the newline satisfies both at once and
// the pattern `^$` matches every line that ends in a newline — which is exactly
// what stock git does with `-I^$`.
//
// The program is a Thompson NFA run as a parallel state set, so a pattern like
// `(a*)*b` costs O(len × program) instead of backtracking exponentially.

/// A parsed regular expression, before it is flattened into a program.
enum Node {
    Empty,
    /// One character drawn from a set: a literal, `.`, or a bracket expression.
    Set(CharSet),
    /// `^` — start of buffer, or just after a newline.
    Bol,
    /// `$` — end of buffer, or just before a newline.
    Eol,
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        /// `None` is an unbounded tail, as in `*`, `+` and `{n,}`.
        max: Option<u32>,
    },
}

/// A bracket expression, `.`, or a single literal.
#[derive(Clone)]
struct CharSet {
    negated: bool,
    ranges: Vec<(char, char)>,
    classes: Vec<Class>,
}

/// The POSIX character classes usable as `[:name:]`.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Xdigit,
}

impl Class {
    fn parse(name: &str) -> Option<Class> {
        Some(match name {
            "alnum" => Class::Alnum,
            "alpha" => Class::Alpha,
            "blank" => Class::Blank,
            "cntrl" => Class::Cntrl,
            "digit" => Class::Digit,
            "graph" => Class::Graph,
            "lower" => Class::Lower,
            "print" => Class::Print,
            "punct" => Class::Punct,
            "space" => Class::Space,
            "upper" => Class::Upper,
            "xdigit" => Class::Xdigit,
            _ => return None,
        })
    }

    fn matches(self, c: char) -> bool {
        match self {
            Class::Alnum => c.is_alphanumeric(),
            Class::Alpha => c.is_alphabetic(),
            Class::Blank => c == ' ' || c == '\t',
            Class::Cntrl => c.is_control(),
            Class::Digit => c.is_ascii_digit(),
            Class::Graph => !c.is_whitespace() && !c.is_control(),
            Class::Lower => c.is_lowercase(),
            Class::Print => !c.is_control(),
            Class::Punct => c.is_ascii_punctuation(),
            Class::Space => c.is_whitespace(),
            Class::Upper => c.is_uppercase(),
            Class::Xdigit => c.is_ascii_hexdigit(),
        }
    }
}

impl CharSet {
    /// A single literal character.
    fn literal(c: char) -> CharSet {
        CharSet {
            negated: false,
            ranges: vec![(c, c)],
            classes: Vec::new(),
        }
    }

    /// `.` — under `REG_NEWLINE` this is "anything but a newline", which is what
    /// an empty negated set already means.
    fn any() -> CharSet {
        CharSet {
            negated: true,
            ranges: Vec::new(),
            classes: Vec::new(),
        }
    }

    fn matches(&self, c: char) -> bool {
        let listed = self.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
            || self.classes.iter().any(|cl| cl.matches(c));
        if self.negated {
            // REG_NEWLINE: a non-matching list never matches a newline.
            !listed && c != '\n'
        } else {
            listed
        }
    }
}

/// One instruction of the compiled NFA. Every instruction that does not branch
/// falls through to the next one.
enum Inst {
    Char(CharSet),
    Split(usize, usize),
    Jump(usize),
    Bol,
    Eol,
    Match,
}

/// A compiled regular expression.
struct Regex {
    prog: Vec<Inst>,
}

/// Anything `regcomp` would reject. git only reports that it happened, never
/// which rule was broken, so the reason is not carried.
struct RegexError;

impl Regex {
    fn compile(pattern: &str) -> std::result::Result<Regex, RegexError> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parser = Parser { chars, pos: 0 };
        let node = parser.parse_alt()?;
        if parser.pos != parser.chars.len() {
            // A `)` with no `(` is all that can be left over.
            return Err(RegexError);
        }
        let mut prog = Vec::new();
        emit_node(&node, &mut prog);
        prog.push(Inst::Match);
        Ok(Regex { prog })
    }

    /// Whether the pattern matches anywhere in `text` — git's `regexec_buf()`
    /// call passes the whole record, trailing newline included, and only looks
    /// at the return code.
    fn is_match(&self, text: &[u8]) -> bool {
        let chars = decode_chars(text);
        let n = chars.len();

        let mut current: Vec<usize> = Vec::new();
        let mut next: Vec<usize> = Vec::new();
        let mut seen = vec![usize::MAX; self.prog.len()];

        for pos in 0..=n {
            // A fresh thread at every position is what makes the search
            // unanchored, as `regexec` without `REG_STARTEND` anchoring is.
            let mut stack = std::mem::take(&mut current);
            stack.push(0);

            while let Some(pc) = stack.pop() {
                if seen[pc] == pos {
                    continue;
                }
                seen[pc] = pos;
                match &self.prog[pc] {
                    Inst::Jump(t) => stack.push(*t),
                    Inst::Split(a, b) => {
                        stack.push(*a);
                        stack.push(*b);
                    }
                    Inst::Bol => {
                        if pos == 0 || chars[pos - 1] == '\n' {
                            stack.push(pc + 1);
                        }
                    }
                    Inst::Eol => {
                        if pos == n || chars[pos] == '\n' {
                            stack.push(pc + 1);
                        }
                    }
                    Inst::Match => return true,
                    Inst::Char(set) => {
                        if pos < n && set.matches(chars[pos]) {
                            next.push(pc + 1);
                        }
                    }
                }
            }
            current = std::mem::take(&mut next);
        }
        false
    }
}

/// Decode `text` into characters. Well-formed UTF-8 decodes as itself; any byte
/// that does not start a valid sequence stands for the character of the same
/// value, so no input is ever rejected.
fn decode_chars(text: &[u8]) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let width = match text[i] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 0,
        };
        let decoded = (width > 1 && i + width <= text.len())
            .then(|| std::str::from_utf8(&text[i..i + width]).ok())
            .flatten()
            .and_then(|s| s.chars().next());
        match decoded {
            Some(c) => {
                out.push(c);
                i += width;
            }
            None => {
                out.push(char::from(text[i]));
                i += 1;
            }
        }
    }
    out
}

/// Flatten the parse tree into the NFA program.
fn emit_node(node: &Node, prog: &mut Vec<Inst>) {
    match node {
        Node::Empty => {}
        Node::Set(set) => prog.push(Inst::Char(set.clone())),
        Node::Bol => prog.push(Inst::Bol),
        Node::Eol => prog.push(Inst::Eol),
        Node::Cat(parts) => {
            for part in parts {
                emit_node(part, prog);
            }
        }
        Node::Alt(branches) => {
            // Each branch gets a split that either enters it or moves on, and
            // ends with a jump to the common exit, patched once all are placed.
            let mut jumps = Vec::new();
            for (i, branch) in branches.iter().enumerate() {
                if i + 1 == branches.len() {
                    emit_node(branch, prog);
                    break;
                }
                let split = prog.len();
                prog.push(Inst::Split(0, 0));
                emit_node(branch, prog);
                jumps.push(prog.len());
                prog.push(Inst::Jump(0));
                let next = prog.len();
                prog[split] = Inst::Split(split + 1, next);
            }
            let exit = prog.len();
            for j in jumps {
                prog[j] = Inst::Jump(exit);
            }
        }
        Node::Repeat { node, min, max } => {
            for _ in 0..*min {
                emit_node(node, prog);
            }
            match max {
                None => {
                    // `X*`: split into the body or past it, and loop back.
                    let split = prog.len();
                    prog.push(Inst::Split(0, 0));
                    emit_node(node, prog);
                    prog.push(Inst::Jump(split));
                    let exit = prog.len();
                    prog[split] = Inst::Split(split + 1, exit);
                }
                Some(max) => {
                    // `X{n,m}`: the surplus copies are each independently optional.
                    let mut splits = Vec::new();
                    for _ in *min..*max {
                        splits.push(prog.len());
                        prog.push(Inst::Split(0, 0));
                        emit_node(node, prog);
                    }
                    let exit = prog.len();
                    for s in splits {
                        prog[s] = Inst::Split(s + 1, exit);
                    }
                }
            }
        }
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// `alt := concat ('|' concat)*`
    fn parse_alt(&mut self) -> std::result::Result<Node, RegexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().expect("just checked the length")
        } else {
            Node::Alt(branches)
        })
    }

    /// `concat := repeat*`, stopping at `|` or the `)` that closes a group.
    fn parse_concat(&mut self) -> std::result::Result<Node, RegexError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.parse_repeat()?);
        }
        Ok(match parts.len() {
            0 => Node::Empty,
            1 => parts.pop().expect("just checked the length"),
            _ => Node::Cat(parts),
        })
    }

    /// `repeat := atom ('*' | '+' | '?' | '{n,m}')*`
    fn parse_repeat(&mut self) -> std::result::Result<Node, RegexError> {
        let mut node = self.parse_atom()?;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => (0, None),
                Some('+') => (1, None),
                Some('?') => (0, Some(1)),
                Some('{') => match self.parse_interval()? {
                    Some(bounds) => {
                        node = Node::Repeat {
                            node: Box::new(node),
                            min: bounds.0,
                            max: bounds.1,
                        };
                        continue;
                    }
                    // Not a valid interval, so `{` was an ordinary character and
                    // `parse_interval` left the position untouched.
                    None => break,
                },
                _ => break,
            };
            self.pos += 1;
            node = Node::Repeat {
                node: Box::new(node),
                min,
                max,
            };
        }
        Ok(node)
    }

    /// `{n}`, `{n,}` or `{n,m}`. Returns `None` — without consuming anything —
    /// when what follows is not an interval, which leaves `{` an ordinary
    /// character as the C libraries treat it.
    fn parse_interval(&mut self) -> std::result::Result<Option<(u32, Option<u32>)>, RegexError> {
        let save = self.pos;
        self.pos += 1;
        let Some(min) = self.parse_bound() else {
            self.pos = save;
            return Ok(None);
        };
        let max = match self.peek() {
            Some('}') => Some(min),
            Some(',') => {
                self.pos += 1;
                if self.peek() == Some('}') {
                    None
                } else {
                    match self.parse_bound() {
                        Some(max) => Some(max),
                        None => {
                            self.pos = save;
                            return Ok(None);
                        }
                    }
                }
            }
            _ => {
                self.pos = save;
                return Ok(None);
            }
        };
        if self.peek() != Some('}') {
            self.pos = save;
            return Ok(None);
        }
        self.pos += 1;
        if max.is_some_and(|max| max < min) {
            return Err(RegexError);
        }
        Ok(Some((min, max)))
    }

    fn parse_bound(&mut self) -> Option<u32> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    fn parse_atom(&mut self) -> std::result::Result<Node, RegexError> {
        let Some(c) = self.peek() else {
            return Err(RegexError);
        };
        self.pos += 1;
        Ok(match c {
            '^' => Node::Bol,
            '$' => Node::Eol,
            '.' => Node::Set(CharSet::any()),
            '(' => {
                let inner = self.parse_alt()?;
                if self.peek() != Some(')') {
                    return Err(RegexError);
                }
                self.pos += 1;
                inner
            }
            '[' => Node::Set(self.parse_bracket()?),
            // A repetition operator with nothing to repeat is `REG_BADRPT`.
            '*' | '+' | '?' => return Err(RegexError),
            ')' => return Err(RegexError),
            '\\' => match self.peek() {
                Some(esc) => {
                    self.pos += 1;
                    Node::Set(CharSet::literal(esc))
                }
                None => return Err(RegexError),
            },
            other => Node::Set(CharSet::literal(other)),
        })
    }

    /// A bracket expression. POSIX gives backslash no special meaning in here,
    /// `]` first is a literal, and `-` first or last is a literal.
    fn parse_bracket(&mut self) -> std::result::Result<CharSet, RegexError> {
        let mut set = CharSet {
            negated: false,
            ranges: Vec::new(),
            classes: Vec::new(),
        };
        if self.peek() == Some('^') {
            set.negated = true;
            self.pos += 1;
        }
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                // Unterminated: this is what rejects `-I'['`.
                return Err(RegexError);
            };
            if c == ']' && !first {
                self.pos += 1;
                return Ok(set);
            }
            first = false;

            if c == '[' && self.chars.get(self.pos + 1) == Some(&':') {
                let rest: String = self.chars[self.pos + 2..].iter().collect();
                let Some(end) = rest.find(":]") else {
                    return Err(RegexError);
                };
                let Some(class) = Class::parse(&rest[..end]) else {
                    return Err(RegexError);
                };
                set.classes.push(class);
                self.pos += 2 + rest[..end].chars().count() + 2;
                continue;
            }

            self.pos += 1;
            // `a-z`, unless the `-` is the last character before `]`.
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&n| n != ']') {
                let Some(&hi) = self.chars.get(self.pos + 1) else {
                    return Err(RegexError);
                };
                if hi < c {
                    return Err(RegexError);
                }
                set.ranges.push((c, hi));
                self.pos += 2;
            } else {
                set.ranges.push((c, c));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, added: u64, deleted: u64) -> StatEntry {
        StatEntry {
            name: name.to_owned(),
            raw_name: name.as_bytes().to_vec(),
            added,
            deleted,
        }
    }

    fn stats(files: &[StatEntry], sw: StatWidths) -> String {
        let mut out = Vec::new();
        emit_stats(&mut out, files, sw).expect("emit_stats writes to a Vec");
        String::from_utf8(out).expect("diffstat output is UTF-8")
    }

    /// Port of `diff_opt_stat()`'s `--stat=<w>[,<nw>[,<c>]]` field parse: `width`
    /// is always taken (an empty value is 0), and each later field is `Some` only
    /// when its comma is reached, so an absent field keeps the previous value.
    #[test]
    fn stat_value_fields() {
        assert_eq!(parse_stat_value(b"20,10,3"), Some((20, Some(10), Some(3))));
        assert_eq!(parse_stat_value(b"50"), Some((50, None, None)));
        assert_eq!(parse_stat_value(b""), Some((0, None, None)));
        assert_eq!(parse_stat_value(b",5"), Some((0, Some(5), None)));
        assert_eq!(parse_stat_value(b"5,"), Some((5, Some(0), None)));
        // Trailing junk is git's `error(_("invalid --stat value: %s"))`.
        assert_eq!(parse_stat_value(b"5x"), None);
        assert_eq!(parse_stat_value(b"5,6,7,8"), None);
    }

    /// `--stat-name-width` caps the filename column, and an over-long name is
    /// elided with `...` and re-anchored, exactly as `show_stats()` does. Verified
    /// against `git format-patch --stat-name-width=5`.
    #[test]
    fn stat_name_width_elides() {
        let files = [entry("abcdefghij", 1, 0)];
        let sw = StatWidths {
            width: 0,
            name_width: 5,
            graph_width: 0,
            count: 0,
        };
        assert_eq!(
            stats(&files, sw),
            " ...ij | 1 +\n 1 file changed, 1 insertion(+)\n"
        );
    }

    /// `--stat-count` lists only the first N files, appends git's ` ...` abbrev
    /// line, scales the columns to just the shown files, yet still counts every
    /// file in the insertions/deletions summary. Verified against
    /// `git format-patch --stat-count=2`.
    #[test]
    fn stat_count_truncates_but_totals_all() {
        let files = [entry("a", 2, 0), entry("bb", 0, 2), entry("ccc", 10, 10)];
        let sw = StatWidths {
            width: 0,
            name_width: 0,
            graph_width: 0,
            count: 2,
        };
        assert_eq!(
            stats(&files, sw),
            " a  | 2 ++\n bb | 2 --\n ...\n 3 files changed, 12 insertions(+), 12 deletions(-)\n"
        );
    }

    /// The all-zero widths reproduce format-patch's default 72-column diffstat,
    /// so a small unscaled change renders unchanged from before the port.
    #[test]
    fn default_widths_unscaled() {
        let files = [entry("x", 3, 1)];
        let sw = StatWidths {
            width: 0,
            name_width: 0,
            graph_width: 0,
            count: 0,
        };
        assert_eq!(
            stats(&files, sw),
            " x | 4 +++-\n 1 file changed, 3 insertions(+), 1 deletion(-)\n"
        );
    }
}
