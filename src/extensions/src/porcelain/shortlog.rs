//! `git shortlog` — summarize `git log` output, grouped by author or committer.
//!
//! A port of git's `builtin/shortlog.c` together with the two helpers it leans
//! on for output: `strbuf_add_wrapped_text()` / `strbuf_add_indented_text()`
//! from `utf8.c` (the `-w` line wrapper) and `split_ident_line()` from
//! `ident.c` (the stdin path's `Name <email>` splitter). The commit walk and
//! the mailmap come from the vendored gitoxide crates.
//!
//! Covered, byte-for-byte against stock git:
//!   * default long format — `<ident> (<n>):` followed by one indented subject
//!     per commit, oldest first, then a blank line.
//!   * `-s`/`--summary` — `%6d\t<ident>` count lines.
//!   * `-n`/`--numbered` — stable sort by descending commit count.
//!   * `-e`/`--email` — append ` <email>` to the group key.
//!   * `-c`/`--committer` and `--group=author`/`--group=committer`.
//!   * `-w[<width>[,<indent1>[,<indent2>]]]` — the real wrap algorithm, not an
//!     approximation; `-w0` indents without wrapping.
//!   * `--no-` forms of the boolean options, short-option clustering (`-sne`),
//!     and `-h` (usage on stdout, exit 129).
//!   * `[PATCH...]` subject-prefix stripping and the `<none>` placeholder git
//!     substitutes for an empty subject.
//!   * mailmap resolution of the grouped identity.
//!   * the stdin mode git falls into when no revision is given and stdin is not
//!     a terminal (or `HEAD` is unborn): `git log --pretty=short | git shortlog`.
//!   * revision selection: `<rev>`, `^<rev>`, `<a>..<b>`, `--all`,
//!     `--max-count=<n>`, `-<n>`, `--first-parent`.
//!   * exit codes: 0 on success, 128 for an unresolvable revision, 129 for `-h`
//!     and for a malformed `-w` argument.
//!
//! Not covered — each `bail!`s rather than emitting output that would diverge:
//! `--group=trailer:<field>`, `--group=format:<fmt>`, repeated `--group`,
//! `--format`, `--date`, pathspec limiting (`-- <path>...`), and every other
//! revision-walk option.
//!
//! Known deviations, both confined to inputs stock git treats specially:
//!   * `-w` measures a code point as one display column, where git uses
//!     `wcwidth()`. Wrapping differs only for subjects containing wide (CJK) or
//!     zero-width characters, or for text that is not valid UTF-8.
//!   * mailmap lookups go through `gix_mailmap`, which case-normalises a matched
//!     email even when the matching entry supplies no replacement address; git
//!     keeps the commit's own casing. Only `-e` output against such a mailmap is
//!     affected.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The `usage_with_options` block git prints for `-h`.
const USAGE: &str = "\
usage: git shortlog [<options>] [<revision-range>] [[--] <path>...]
   or: git log --pretty=short | git shortlog [<options>]

    -c, --[no-]committer  group by committer rather than author
    -n, --[no-]numbered   sort output according to the number of commits per author
    -s, --[no-]summary    suppress commit descriptions, only provides commit count
    -e, --[no-]email      show the email address of each author
    -w[<w>[,<i1>[,<i2>]]] linewrap output
    --[no-]group <field>  group by field
";

/// git's `wrap_arg_usage`, printed verbatim when `-w`'s argument is malformed.
const WRAP_ARG_USAGE: &str = "-w[<width>[,<indent1>[,<indent2>]]]";

const DEFAULT_WRAPLEN: usize = 76;
const DEFAULT_INDENT1: usize = 6;
const DEFAULT_INDENT2: usize = 9;

/// Parsed command-line options for a single `shortlog` invocation.
struct Opts {
    summary: bool,   // -s: print counts only
    numbered: bool,  // -n: sort by descending commit count
    email: bool,     // -e: include the email in the group key
    committer: bool, // -c / --group=committer
    wrap_lines: bool,
    wrap: usize, // -w width; 0 means "indent but never wrap"
    in1: usize,  // -w indent for the first line of an entry
    in2: usize,  // -w indent for continuation lines
}

/// One grouped identity: how many commits it owns, and (unless `-s`) their
/// subjects in walk order, i.e. newest first.
#[derive(Default)]
struct Group {
    count: usize,
    onelines: Vec<BString>,
}

pub fn shortlog(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us argv with the subcommand at index 0.
    let argv: &[String] = match args.first() {
        Some(a) if a == "shortlog" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        summary: false,
        numbered: false,
        email: false,
        committer: false,
        wrap_lines: false,
        wrap: DEFAULT_WRAPLEN,
        in1: DEFAULT_INDENT1,
        in2: DEFAULT_INDENT2,
    };

    // `--group` is a bitfield in git; we only accept a single group, so track
    // whether either bit was already requested to reject the multi-group forms.
    let mut group_author = false;
    let mut group_committer = false;

    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();
    let mut saw_rev_arg = false;
    let mut use_all = false;
    let mut first_parent = false;
    let mut max_count: Option<usize> = None;
    let mut revs: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;

        if no_more_opts {
            bail!("pathspec limiting ({a:?}) is not ported");
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        if a.len() < 2 || !a.starts_with('-') {
            revs.push(a.to_string());
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            match long {
                "committer" => group_committer = true,
                "no-committer" => group_committer = false,
                "numbered" => opts.numbered = true,
                "no-numbered" => opts.numbered = false,
                "summary" => opts.summary = true,
                "no-summary" => opts.summary = false,
                "email" => opts.email = true,
                "no-email" => opts.email = false,
                "all" => {
                    use_all = true;
                    saw_rev_arg = true;
                }
                "first-parent" => first_parent = true,
                "group" | "no-group" => {
                    bail!("`--group` requires an attached `=<field>` value here")
                }
                _ if long.starts_with("group=") => match &long["group=".len()..] {
                    "author" => group_author = true,
                    "committer" => group_committer = true,
                    field => {
                        bail!("unsupported --group field {field:?} (ported: author, committer)")
                    }
                },
                _ if long.starts_with("max-count=") => {
                    max_count = Some(parse_count("--max-count", &long["max-count=".len()..])?);
                }
                _ => bail!("unsupported flag {a:?}"),
            }
            continue;
        }

        // `-<number>` is git's `--max-count` shorthand, not a short-option cluster.
        let body = &a[1..];
        if body.bytes().all(|b| b.is_ascii_digit()) {
            max_count = Some(parse_count(a, body)?);
            continue;
        }

        // A cluster of short options. `-w` takes an optional *attached* argument,
        // so it swallows whatever remains of the cluster.
        for (off, c) in body.char_indices() {
            match c {
                'c' => group_committer = true,
                'n' => opts.numbered = true,
                's' => opts.summary = true,
                'e' => opts.email = true,
                'w' => {
                    let rest = &body[off + 1..];
                    let arg = if rest.is_empty() { None } else { Some(rest) };
                    if !parse_wrap_args(&mut opts, arg) {
                        eprintln!("error: {WRAP_ARG_USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    break;
                }
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => bail!("unsupported flag {a:?}"),
            }
        }
    }

    if group_author && group_committer {
        bail!("multiple --group options are not ported");
    }
    opts.committer = group_committer;

    let repo = gix::discover(".").ok();
    let mailmap = repo
        .as_ref()
        .map(gix::Repository::open_mailmap)
        .unwrap_or_default();

    // Resolve the revision arguments now so an unknown one fails the way git's
    // `setup_revisions` does: a fatal message on stderr and exit 128.
    if !revs.is_empty() {
        let Some(repo) = repo.as_ref() else {
            bail!("not a git repository");
        };
        saw_rev_arg = true;
        for spec in &revs {
            if spec.contains("...") {
                bail!("symmetric-difference range `{spec}` is not ported");
            }
            if let Some(rest) = spec.strip_prefix('^') {
                match resolve(repo, rest) {
                    Some(id) => hidden.push(id),
                    None => return Ok(fatal_ambiguous(rest)),
                }
            } else if let Some((left, right)) = spec.split_once("..") {
                let left = if left.is_empty() { "HEAD" } else { left };
                let right = if right.is_empty() { "HEAD" } else { right };
                match resolve(repo, left) {
                    Some(id) => hidden.push(id),
                    None => return Ok(fatal_ambiguous(left)),
                }
                match resolve(repo, right) {
                    Some(id) => tips.push(id),
                    None => return Ok(fatal_ambiguous(right)),
                }
            } else {
                match resolve(repo, spec) {
                    Some(id) => tips.push(id),
                    None => return Ok(fatal_ambiguous(spec)),
                }
            }
        }
    }

    if use_all {
        let Some(repo) = repo.as_ref() else {
            bail!("not a git repository");
        };
        for reference in repo.references()?.all()? {
            let Ok(reference) = reference else { continue };
            let Ok(id) = reference.into_fully_peeled_id() else {
                continue;
            };
            let Ok(object) = id.object() else { continue };
            if let Ok(commit) = object.peel_to_commit() {
                tips.push(commit.id);
            }
        }
    }

    // git: "assume HEAD if from a tty". An unborn HEAD adds nothing, which is
    // what pushes a fresh repository onto the stdin path as well.
    if !saw_rev_arg && std::io::stdin().is_terminal() {
        if let Some(repo) = repo.as_ref() {
            if let Ok(mut head) = repo.head() {
                if let Ok(Some(id)) = head.try_peel_to_id() {
                    tips.push(id.detach());
                    saw_rev_arg = true;
                }
            }
        }
    }

    let mut groups: BTreeMap<BString, Group> = BTreeMap::new();

    if !saw_rev_arg && tips.is_empty() {
        read_from_stdin(&mut groups, &mailmap, &opts)?;
    } else if !tips.is_empty() {
        let repo = repo.as_ref().expect("tips can only come from a repository");
        let mut platform = repo
            .rev_walk(tips)
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
        if first_parent {
            platform = platform.first_parent_only();
        }
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden);
        }

        let mut seen = 0usize;
        for info in platform.all()? {
            if max_count.is_some_and(|max| seen >= max) {
                break;
            }
            seen += 1;

            let commit = info?.object()?;
            let sig = if opts.committer {
                commit.committer()?
            } else {
                commit.author()?
            };
            let ident = format_ident(sig.trim(), &mailmap, opts.email);

            // git computes the subject once and substitutes `<none>` when empty.
            let oneline = if opts.summary {
                BString::default()
            } else {
                let message = commit.message()?;
                let subject = message.summary();
                if subject.is_empty() {
                    BString::from("<none>")
                } else {
                    subject.into_owned()
                }
            };
            insert_one_record(&mut groups, &opts, ident, oneline.as_bstr());
        }
    }
    // Otherwise: revisions were named but none resolved to a positive tip
    // (e.g. only `^<rev>`), which git renders as empty output.

    let mut out: Vec<u8> = Vec::new();
    render(&groups, &opts, &mut out);
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// Peel `spec` to a commit id, or `None` when it names no commit.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// git's `setup_revisions` failure: the fatal block on stderr, exit code 128.
fn fatal_ambiguous(spec: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// Parse a positive commit count, with a git-shaped error.
fn parse_count(flag: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("invalid count `{value}` for `{flag}`"))
}

/// Port of `parse_uint()` from `builtin/shortlog.c`: read a decimal run, require
/// the terminator to be `comma` (or end of string), and fall back to `defval`
/// when the field is empty. Returns `None` on a malformed field.
fn parse_uint<'a>(arg: &mut &'a str, comma: Option<char>, defval: usize) -> Option<usize> {
    // Copy the slice out first so `rest` does not borrow through `arg`.
    let s: &'a str = *arg;
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (num, rest) = s.split_at(digits);
    if rest.chars().next().is_some_and(|c| Some(c) != comma) {
        return None;
    }
    let value = if num.is_empty() {
        defval
    } else {
        num.parse::<usize>().ok()?
    };
    *arg = if rest.is_empty() { rest } else { &rest[1..] };
    Some(value)
}

/// Port of `parse_wrap_args()`. Returns `false` when the argument is malformed,
/// which git reports as `error: -w[<width>[,<indent1>[,<indent2>]]]`.
fn parse_wrap_args(opts: &mut Opts, arg: Option<&str>) -> bool {
    opts.wrap_lines = true;
    let Some(arg) = arg else {
        opts.wrap = DEFAULT_WRAPLEN;
        opts.in1 = DEFAULT_INDENT1;
        opts.in2 = DEFAULT_INDENT2;
        return true;
    };

    let mut cursor = arg;
    let (Some(wrap), Some(in1), Some(in2)) = (
        parse_uint(&mut cursor, Some(','), DEFAULT_WRAPLEN),
        parse_uint(&mut cursor, Some(','), DEFAULT_INDENT1),
        parse_uint(&mut cursor, None, DEFAULT_INDENT2),
    ) else {
        return false;
    };
    opts.wrap = wrap;
    opts.in1 = in1;
    opts.in2 = in2;

    // git rejects a width that cannot even fit its own indent.
    if wrap != 0 && ((in1 != 0 && wrap <= in1) || (in2 != 0 && wrap <= in2)) {
        return false;
    }
    true
}

/// The group key: the mailmap-resolved name, plus ` <email>` under `-e`.
/// This is git's `%aN` / `%aN <%aE>` (or the `%c*` pair for `--committer`).
fn format_ident(
    sig: gix::actor::SignatureRef<'_>,
    mailmap: &gix::mailmap::Snapshot,
    email: bool,
) -> BString {
    // `ResolvedSignature` is not `Copy`, so read both fields out in one go.
    let (mapped_name, mapped_email) = match mailmap.try_resolve_ref(sig) {
        Some(resolved) => (resolved.name, resolved.email),
        None => (None, None),
    };

    let mut out = BString::from(mapped_name.unwrap_or(sig.name).to_vec());
    if email {
        out.push(b' ');
        out.push(b'<');
        out.extend_from_slice(mapped_email.unwrap_or(sig.email));
        out.push(b'>');
    }
    out
}

/// Port of `insert_one_record()`: strip a `[PATCH...]` prefix and any framing
/// whitespace off `oneline`, then file it under `ident`.
fn insert_one_record(
    groups: &mut BTreeMap<BString, Group>,
    opts: &Opts,
    ident: BString,
    oneline: &BStr,
) {
    let entry = groups.entry(ident).or_default();
    entry.count += 1;
    if opts.summary {
        return;
    }

    // Skip any leading whitespace, including any blank lines.
    let mut s = oneline.as_bytes();
    while s.first().is_some_and(|&b| is_space(b)) {
        s = &s[1..];
    }
    if s.starts_with(b"[PATCH") {
        let eol = s.iter().position(|&b| b == b'\n').unwrap_or(s.len());
        if let Some(eob) = s.iter().position(|&b| b == b']') {
            if eob < eol {
                s = &s[eob + 1..];
            }
        }
    }
    while s.first().is_some_and(|&b| is_space(b) && b != b'\n') {
        s = &s[1..];
    }

    entry.onelines.push(BString::from(s.to_vec()));
}

/// Port of `shortlog_output()`.
fn render(groups: &BTreeMap<BString, Group>, opts: &Opts, out: &mut Vec<u8>) {
    // The map is already in strcmp order, which is git's default. `-n` re-sorts
    // it stably by descending count, so ties keep the alphabetic order.
    let mut entries: Vec<(&BString, &Group)> = groups.iter().collect();
    if opts.numbered {
        entries.sort_by(|a, b| b.1.count.cmp(&a.1.count));
    }

    for (ident, group) in entries {
        if opts.summary {
            out.extend_from_slice(format!("{:6}\t", group.count).as_bytes());
            out.extend_from_slice(ident);
            out.push(b'\n');
            continue;
        }

        out.extend_from_slice(ident);
        out.extend_from_slice(format!(" ({}):\n", group.count).as_bytes());
        // Oldest first: git walks its per-ident list back to front.
        for msg in group.onelines.iter().rev() {
            if opts.wrap_lines {
                add_wrapped_text(out, msg, opts.in1, opts.in2, opts.wrap);
            } else {
                out.extend_from_slice(b"      ");
                out.extend_from_slice(msg);
            }
            out.push(b'\n');
        }
        out.push(b'\n');
    }
}

/// Port of `read_from_stdin()`: scan piped `git log` output for ident headers,
/// then take the first non-blank line of the message body as the subject.
fn read_from_stdin(
    groups: &mut BTreeMap<BString, Group>,
    mailmap: &gix::mailmap::Snapshot,
    opts: &Opts,
) -> Result<()> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;

    let matches: [&[u8]; 2] = if opts.committer {
        [&b"Commit: "[..], &b"committer "[..]]
    } else {
        [&b"Author: "[..], &b"author "[..]]
    };

    let mut lines = LinesLf { data: &buf, pos: 0 };
    while let Some(line) = lines.next_line() {
        let Some(ident_line) = matches.iter().find_map(|prefix| line.strip_prefix(*prefix)) else {
            continue;
        };
        let ident_line = BString::from(ident_line.to_vec());

        // Discard the remaining headers, up to the blank separator line.
        while let Some(l) = lines.next_line() {
            if l.is_empty() {
                break;
            }
        }
        // Discard blank lines; the first non-blank one is the subject.
        let mut oneline = BString::default();
        while let Some(l) = lines.next_line() {
            if !l.is_empty() {
                oneline = BString::from(l.to_vec());
                break;
            }
        }

        // git skips records whose ident it cannot split.
        let Some((name, email)) = split_ident_line(ident_line.as_bstr()) else {
            continue;
        };
        let sig = gix::actor::SignatureRef {
            name,
            email,
            time: "",
        };
        let ident = format_ident(sig, mailmap, opts.email);
        insert_one_record(groups, opts, ident, oneline.as_bstr());
    }
    Ok(())
}

/// A `strbuf_getline_lf()` equivalent: yields each `\n`-terminated record with
/// the terminator removed, and no phantom empty record after a trailing `\n`.
struct LinesLf<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LinesLf<'a> {
    fn next_line(&mut self) -> Option<&'a BStr> {
        // Copy the slice out of `self` so the result is tied to `'a`, not to
        // the `&mut self` borrow.
        let data: &'a [u8] = self.data;
        if self.pos >= data.len() {
            return None;
        }
        let rest = &data[self.pos..];
        Some(match rest.iter().position(|&b| b == b'\n') {
            Some(nl) => {
                self.pos += nl + 1;
                rest[..nl].as_bstr()
            }
            None => {
                self.pos = data.len();
                rest.as_bstr()
            }
        })
    }
}

/// Port of `split_ident_line()` reduced to what shortlog reads off it: the name
/// (trailing whitespace trimmed) and the email between the first `<` and the
/// first `>` after it. `None` when the line carries no `<...>` pair, which is
/// the `-1` git treats as "skip this record".
fn split_ident_line(line: &BStr) -> Option<(&BStr, &BStr)> {
    let bytes = line.as_bytes();
    let lt = bytes.iter().position(|&b| b == b'<')?;
    let gt = lt + 1 + bytes[lt + 1..].iter().position(|&b| b == b'>')?;

    let mut name_end = lt;
    while name_end > 0 && is_space(bytes[name_end - 1]) {
        name_end -= 1;
    }
    Some((bytes[..name_end].as_bstr(), bytes[lt + 1..gt].as_bstr()))
}

/// C's `isspace()` for the "C" locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Byte length of the UTF-8 sequence introduced by `b`; 1 for a stray byte.
fn utf8_seq_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Port of `strbuf_add_indented_text()` (`utf8.c`), git's `-w0` path.
fn add_indented_text(out: &mut Vec<u8>, text: &[u8], indent1: usize, indent2: usize) {
    let mut indent = indent1;
    let mut pos = 0;
    while pos < text.len() {
        let eol = match text[pos..].iter().position(|&b| b == b'\n') {
            Some(n) => pos + n + 1,
            None => text.len(),
        };
        out.resize(out.len() + indent, b' ');
        out.extend_from_slice(&text[pos..eol]);
        pos = eol;
        indent = indent2;
    }
}

/// Port of `strbuf_add_wrapped_text()` (`utf8.c`).
///
/// Structure follows the C loop exactly, including its habit of emitting the
/// run of whitespace that precedes a word together with that word — which is
/// why a wrapped line can keep a trailing space and why runs of spaces survive
/// wrapping. Two branches of the original are absent because the caller cannot
/// reach them: shortlog always passes a single line (no `\n` handling), and
/// subjects carry no ANSI escapes (no `display_mode_esc_sequence_len` skip).
/// Column width is counted per code point rather than via `wcwidth()`.
fn add_wrapped_text(out: &mut Vec<u8>, text: &[u8], indent1: usize, indent2: usize, width: usize) {
    if width == 0 {
        add_indented_text(out, text, indent1, indent2);
        return;
    }

    let mut bol = 0usize;
    let mut indent = indent1;
    let mut w = indent1;
    let mut space: Option<usize> = None;
    let mut i = 0usize;

    loop {
        let c = text.get(i).copied();
        let Some(byte) = c.filter(|&b| !is_space(b)) else {
            // Whitespace, or the end of the text (C's NUL terminator).
            if w <= width || space.is_none() {
                let mut start = bol;
                if c.is_none() && i == start {
                    return;
                }
                match space {
                    Some(s) => start = s,
                    None => out.resize(out.len() + indent, b' '),
                }
                out.extend_from_slice(&text[start..i]);
                let Some(c) = c else { return };
                space = Some(i);
                if c == b'\t' {
                    w |= 0x07;
                }
                w += 1;
                i += 1;
            } else {
                out.push(b'\n');
                let s = space.expect("the `||` above guarantees a break point here");
                i = s + usize::from(text.get(s).is_some_and(|&b| is_space(b)));
                bol = i;
                space = None;
                indent = indent2;
                w = indent2;
            }
            continue;
        };

        w += 1;
        i += utf8_seq_len(byte).min(text.len() - i);
    }
}
