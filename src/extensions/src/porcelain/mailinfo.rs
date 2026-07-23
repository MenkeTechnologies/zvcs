//! `git mailinfo` — extract authorship, subject and patch from one e-mail.
//!
//! A direct port of git's `mailinfo.c` plus the `builtin/mailinfo.c` wrapper.
//! The command needs no object database, so nothing here depends on gitoxide
//! beyond reading configuration.
//!
//! Covered, byte-identically with stock git: the RFC 2822 header scan with
//! folding, in-body headers (`From`/`Subject`/`Date`, the `>From ` format-patch
//! separator and the `[PATCH] ` subject form), RFC 2047 `=?charset?B/Q?...?=`
//! decoding, `Content-Type` boundary/charset/`format=flowed`/`delsp` handling,
//! nested MIME multipart bodies, `base64` and `quoted-printable` transfer
//! encodings, scissors lines, the `patchbreak` split between log message and
//! patch, subject cleanup, the `From:` address/name split, and every flag:
//! `-k`, `-b`, `-m`/`--message-id`, `-u`, `-n`, `--encoding=<enc>`,
//! `--scissors`/`--no-scissors`, `--quoted-cr=<action>` and the hidden
//! `--inbody-headers`. Configuration is read for `mailinfo.scissors`,
//! `mailinfo.quotedcr` and `i18n.commitEncoding`. Usage, `error:` and
//! `warning:` text and the 0/1/129 exit codes match.
//!
//! Not covered: charset re-coding is only implemented between UTF-8, US-ASCII
//! and ISO-8859-1, because git delegates the general case to `iconv`, which has
//! no counterpart in the vendored crates. Any other pair bails rather than
//! emitting bytes git would have transliterated differently; `-n` disables
//! re-coding entirely and always works. A malformed `mailinfo.quotedcr` also
//! bails instead of reproducing git's config-callback death path.
//!
//! One structural difference with no observable effect on a successful run: the
//! `<msg>` and `<patch>` files are created and truncated up front, as git does,
//! but filled once parsing finishes rather than incrementally. A run that bails
//! on an unsupported charset therefore leaves them empty where git would have
//! left partial content.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

/// The headers git tracks, in the order `handle_info()` prints them.
const HEADER: [&str; 3] = ["From", "Subject", "Date"];

/// git's `MAX_BOUNDARIES`. The C code never uses slot 0 (`content_top` is
/// pre-incremented), so at most four boundaries actually nest.
const MAX_BOUNDARIES: usize = 5;

/// `mailinfo_usage[]` rendered by `usage_with_options()`.
const USAGE: &str = "usage: git mailinfo [<options>] <msg> <patch> < mail >info

    -k                    keep subject
    -b                    keep non patch brackets in subject
    -m, --[no-]message-id copy Message-ID to the end of commit message
    -u                    re-code metadata to i18n.commitEncoding
    -n                    disable charset re-coding of metadata
    --encoding <encoding> re-code metadata to this encoding
    --[no-]scissors       use scissors
    --quoted-cr <action>  action when quoted CR is found

";

/// `enum quoted_cr_action`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QuotedCr {
    NoWarn,
    Warn,
    Strip,
}

/// The `transfer_encoding` field of `struct mailinfo`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Te {
    DontCare,
    Qp,
    Base64,
}

/// `struct metainfo_charset`'s policy enum.
#[derive(Clone, PartialEq, Eq)]
enum CharsetPolicy {
    Default,
    NoReencode,
    Explicit(String),
}

/// Byte-oriented stand-in for the `FILE *` git reads the mail from.
///
/// It reproduces the two stdio readers `mailinfo.c` relies on, including their
/// end-of-file quirk: `strbuf_getwholeline()` returns early when the stream's
/// EOF flag is *already* set and then leaves the caller's buffer untouched,
/// while a read that discovers EOF for the first time clears it. Header parsing
/// depends on that difference, so `eof` is tracked explicitly.
struct Input {
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Input {
    fn new(buf: Vec<u8>) -> Self {
        Input {
            buf,
            pos: 0,
            eof: false,
        }
    }

    /// `fgetc()` followed by `ungetc()`: peek one byte, setting EOF when there
    /// is none. A successful peek leaves the flag clear, as `ungetc()` does.
    fn peek(&mut self) -> Option<u8> {
        match self.buf.get(self.pos) {
            Some(&b) => Some(b),
            None => {
                self.eof = true;
                None
            }
        }
    }

    /// Consume one byte; only ever called right after a successful `peek`.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// `strbuf_getwholeline(sb, fp, '\n')`: false at EOF, else `out` holds the
    /// line including its terminator when the input had one.
    fn getwholeline(&mut self, out: &mut Vec<u8>) -> bool {
        if self.eof {
            return false;
        }
        out.clear();
        if self.pos >= self.buf.len() {
            self.eof = true;
            return false;
        }
        let end = match self.buf[self.pos..].iter().position(|&b| b == b'\n') {
            Some(i) => self.pos + i + 1,
            // No terminator: the read itself ran into end-of-file.
            None => {
                self.eof = true;
                self.buf.len()
            }
        };
        out.extend_from_slice(&self.buf[self.pos..end]);
        self.pos = end;
        true
    }

    /// `strbuf_getline_lf()`: as above with the terminating newline removed.
    fn getline_lf(&mut self, out: &mut Vec<u8>) -> bool {
        if !self.getwholeline(out) {
            return false;
        }
        if out.last() == Some(&b'\n') {
            out.pop();
        }
        true
    }
}

/// `struct mailinfo`.
struct Mailinfo {
    input: Input,
    name: Vec<u8>,
    email: Vec<u8>,
    keep_subject: bool,
    keep_non_patch_brackets_in_subject: bool,
    quoted_cr: QuotedCr,
    add_message_id: bool,
    use_scissors: bool,
    use_inbody_headers: bool,
    /// `None` mirrors a NULL `metainfo_charset`, i.e. `-n`.
    metainfo_charset: Option<String>,
    /// The boundary stack; the last entry is `*content_top`.
    content: Vec<Vec<u8>>,
    charset: Vec<u8>,
    format_flowed: bool,
    delsp: bool,
    have_quoted_cr: bool,
    message_id: Option<Vec<u8>>,
    transfer_encoding: Te,
    patch_lines: u64,
    /// 0 while reading the log message, 1 once copying the patch.
    filter_stage: u8,
    header_stage: bool,
    inbody_header_accum: Vec<u8>,
    p_hdr: [Option<Vec<u8>>; 3],
    s_hdr: [Option<Vec<u8>>; 3],
    log_message: Vec<u8>,
    patch: Vec<u8>,
    input_error: i32,
}

/// `git mailinfo` — read one e-mail from stdin, write the log message to
/// `<msg>` and the patch to `<patch>`, and print the extracted `Author:`,
/// `Email:`, `Subject:` and `Date:` lines to stdout.
///
/// Exit 0 on success, 1 when git would have reported an `error:` while
/// parsing (empty patch, undecodable header, NUL in a header), 129 for a usage
/// problem.
pub fn mailinfo(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand name.
    let args = match args.first() {
        Some(a) if a == "mailinfo" => &args[1..],
        _ => args,
    };

    let mut mi = Mailinfo::new()?;
    let mut policy = CharsetPolicy::Default;
    let files = match parse_options(args, &mut mi, &mut policy)? {
        Ok(files) => files,
        Err(code) => return Ok(code),
    };

    mi.metainfo_charset = match policy {
        CharsetPolicy::Default => Some(commit_output_encoding()?),
        CharsetPolicy::NoReencode => None,
        CharsetPolicy::Explicit(enc) => Some(enc),
    };

    let mut stdin = Vec::new();
    std::io::stdin().read_to_end(&mut stdin)?;
    mi.input = Input::new(stdin);

    mi.run(&files.0, &files.1)
}

/// Parse the command line the way `parse_options()` does for this builtin.
///
/// The outer `Result` carries an I/O failure, the inner one the exit code git
/// would have used after printing usage, and success yields the two operands.
fn parse_options(
    args: &[String],
    mi: &mut Mailinfo,
    policy: &mut CharsetPolicy,
) -> Result<std::result::Result<(String, String), ExitCode>> {
    // (long name, takes a value, negatable). `--inbody-headers` is hidden from
    // the usage text but accepted, exactly as `OPT_HIDDEN_BOOL` arranges.
    const LONG: [(&str, bool, bool); 5] = [
        ("message-id", false, true),
        ("encoding", true, false),
        ("scissors", false, true),
        ("quoted-cr", true, false),
        ("inbody-headers", false, true),
    ];

    let mut operands: Vec<String> = Vec::new();
    let mut no_more_opts = false;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();
        if no_more_opts || !arg.starts_with('-') || arg == "-" {
            operands.push(arg.to_string());
            i += 1;
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (typed, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (body, None),
            };

            // Every spelling the table accepts, positive and negated.
            let mut candidates: Vec<(String, usize, bool)> = Vec::new();
            for (idx, (name, _, negatable)) in LONG.iter().enumerate() {
                candidates.push(((*name).to_string(), idx, false));
                if *negatable {
                    candidates.push((format!("no-{name}"), idx, true));
                }
            }

            let exact = candidates
                .iter()
                .find(|(name, _, _)| name.as_str() == typed);
            let hit = match exact {
                Some(hit) => hit.clone(),
                None => {
                    let matches: Vec<_> = candidates
                        .iter()
                        .filter(|(name, _, _)| name.starts_with(typed))
                        .cloned()
                        .collect();
                    match matches.len() {
                        1 => matches[0].clone(),
                        0 => {
                            eprintln!("error: unknown option `{typed}'");
                            return Ok(Err(usage(false)));
                        }
                        _ => {
                            // git keeps the first and the last match it saw.
                            let first = &matches[0];
                            let last = &matches[matches.len() - 1];
                            eprintln!(
                                "error: ambiguous option: {typed} (could be --{} or --{})",
                                first.0, last.0
                            );
                            return Ok(Err(usage(false)));
                        }
                    }
                }
            };

            let (_typed_name, idx, negated) = hit;
            let (canonical, takes_value, _) = LONG[idx];

            if takes_value {
                let value = match inline {
                    Some(v) => v.to_string(),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            // A value error stops parse_options() before it
                            // reaches the usage text, unlike an unknown option.
                            None => {
                                eprintln!("error: option `{canonical}' requires a value");
                                return Ok(Err(ExitCode::from(129)));
                            }
                        }
                    }
                };
                match canonical {
                    "encoding" => *policy = CharsetPolicy::Explicit(value),
                    "quoted-cr" => match parse_quoted_cr_action(&value) {
                        Some(action) => mi.quoted_cr = action,
                        // A failing option callback exits without usage too.
                        None => {
                            eprintln!("error: bad action '{value}' for '--quoted-cr'");
                            return Ok(Err(ExitCode::from(129)));
                        }
                    },
                    _ => unreachable!("only encoding and quoted-cr take values"),
                }
            } else {
                if inline.is_some() {
                    eprintln!("error: option `{canonical}' takes no value");
                    return Ok(Err(ExitCode::from(129)));
                }
                let on = !negated;
                match canonical {
                    "message-id" => mi.add_message_id = on,
                    "scissors" => mi.use_scissors = on,
                    "inbody-headers" => mi.use_inbody_headers = on,
                    _ => unreachable!("only boolean options reach here"),
                }
            }
            i += 1;
            continue;
        }

        // Short options, possibly grouped; none of them take a value.
        for c in arg[1..].chars() {
            match c {
                'k' => mi.keep_subject = true,
                'b' => mi.keep_non_patch_brackets_in_subject = true,
                'm' => mi.add_message_id = true,
                'u' => *policy = CharsetPolicy::Default,
                'n' => *policy = CharsetPolicy::NoReencode,
                'h' => return Ok(Err(usage(true))),
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    return Ok(Err(usage(false)));
                }
            }
        }
        i += 1;
    }

    if operands.len() != 2 {
        return Ok(Err(usage(false)));
    }
    let patch = operands.pop().expect("two operands");
    let msg = operands.pop().expect("two operands");
    Ok(Ok((msg, patch)))
}

/// Print the usage block — stdout for `-h`, stderr otherwise — and yield 129.
fn usage(to_stdout: bool) -> ExitCode {
    if to_stdout {
        print!("{USAGE}");
        let _ = std::io::stdout().flush();
    } else {
        eprint!("{USAGE}");
    }
    ExitCode::from(129)
}

/// `mailinfo_parse_quoted_cr_action()`.
fn parse_quoted_cr_action(action: &str) -> Option<QuotedCr> {
    match action {
        "nowarn" => Some(QuotedCr::NoWarn),
        "warn" => Some(QuotedCr::Warn),
        "strip" => Some(QuotedCr::Strip),
        _ => None,
    }
}

/// The configuration git reads, from the repository when there is one and from
/// the global set plus `GIT_CONFIG_*` overrides otherwise.
fn config() -> Result<gix::config::File> {
    Ok(match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => {
            let mut file = gix::config::File::from_globals()?;
            file.append(gix::config::File::from_environment_overrides()?)?;
            file
        }
    })
}

/// `get_commit_output_encoding()`: `i18n.commitEncoding`, else `UTF-8`.
fn commit_output_encoding() -> Result<String> {
    let config = config()?;
    Ok(match config.string("i18n.commitEncoding") {
        Some(v) => String::from_utf8_lossy(&v).into_owned(),
        None => "UTF-8".to_string(),
    })
}

impl Mailinfo {
    /// `setup_mailinfo()`, including `git_mailinfo_config()`.
    fn new() -> Result<Self> {
        let mut mi = Mailinfo {
            input: Input::new(Vec::new()),
            name: Vec::new(),
            email: Vec::new(),
            keep_subject: false,
            keep_non_patch_brackets_in_subject: false,
            quoted_cr: QuotedCr::Warn,
            add_message_id: false,
            use_scissors: false,
            use_inbody_headers: true,
            metainfo_charset: None,
            content: Vec::new(),
            charset: Vec::new(),
            format_flowed: false,
            delsp: false,
            have_quoted_cr: false,
            message_id: None,
            transfer_encoding: Te::DontCare,
            patch_lines: 0,
            filter_stage: 0,
            header_stage: true,
            inbody_header_accum: Vec::new(),
            p_hdr: [None, None, None],
            s_hdr: [None, None, None],
            log_message: Vec::new(),
            patch: Vec::new(),
            input_error: 0,
        };

        let config = config()?;
        if let Some(v) = config.boolean("mailinfo.scissors").transpose() {
            mi.use_scissors = v?;
        }
        if let Some(v) = config.string("mailinfo.quotedcr") {
            let value = String::from_utf8_lossy(&v).into_owned();
            match parse_quoted_cr_action(&value) {
                Some(action) => mi.quoted_cr = action,
                None => bail!("bad action '{value}' for 'mailinfo.quotedcr'"),
            }
        }
        Ok(mi)
    }

    /// `mailinfo()` plus `cmd_mailinfo()`'s `!!` on the return value.
    fn run(&mut self, msg: &str, patch: &str) -> Result<ExitCode> {
        // git opens both files for writing before reading a single byte, so an
        // early failure still leaves them truncated.
        if let Err(e) = std::fs::File::create(msg) {
            eprintln!("{msg}: {}", perror(&e));
            return Ok(ExitCode::from(1));
        }
        if let Err(e) = std::fs::File::create(patch) {
            eprintln!("{patch}: {}", perror(&e));
            return Ok(ExitCode::from(1));
        }

        // Skip leading whitespace; an input made only of it is an empty patch.
        loop {
            match self.input.peek() {
                Some(b) if is_space(b) => self.input.advance(),
                Some(_) => break,
                None => {
                    eprintln!("error: empty patch: '{patch}'");
                    return Ok(ExitCode::from(1));
                }
            }
        }

        let mut line = Vec::new();
        while self.read_one_header_line(&mut line) {
            let l = line.clone();
            self.check_header(&l, true, true)?;
        }
        self.handle_body(&mut line)?;

        std::fs::write(msg, &self.log_message)?;
        std::fs::write(patch, &self.patch)?;

        let out = self.handle_info();
        std::io::stdout().write_all(&out)?;

        Ok(if self.input_error != 0 {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }

    /// `read_one_header_line()`: read one logical header, unfolding
    /// continuations. False means "not a header"; `line` then holds the
    /// offending line with its newline restored, or is left as the previous
    /// read when the stream was already at end-of-file.
    fn read_one_header_line(&mut self, line: &mut Vec<u8>) -> bool {
        if !self.input.getline_lf(line) {
            return false;
        }
        rtrim(line);
        if line.is_empty() || !is_rfc2822_header(line) {
            line.push(b'\n');
            return false;
        }

        let mut continuation = Vec::new();
        loop {
            match self.input.peek() {
                Some(b' ') | Some(b'\t') => {}
                _ => break,
            }
            if !self.input.getline_lf(&mut continuation) {
                break;
            }
            // The peek guarantees a leading space or tab; git overwrites it.
            if let Some(first) = continuation.first_mut() {
                *first = b' ';
            }
            rtrim(&mut continuation);
            line.extend_from_slice(&continuation);
        }
        true
    }

    /// `check_header()`: store a recognised header, returning whether the line
    /// was one. `primary` selects `p_hdr_data` over `s_hdr_data`.
    fn check_header(&mut self, line: &[u8], primary: bool, overwrite: bool) -> Result<bool> {
        for i in 0..HEADER.len() {
            let taken = if primary {
                self.p_hdr[i].is_some()
            } else {
                self.s_hdr[i].is_some()
            };
            if taken && !overwrite {
                continue;
            }
            if let Some(value) = self.parse_header(line, HEADER[i])? {
                if primary {
                    self.p_hdr[i] = Some(value);
                } else {
                    self.s_hdr[i] = Some(value);
                }
                return Ok(true);
            }
        }

        if let Some(value) = self.parse_header(line, "Content-Type")? {
            self.handle_content_type(&value);
            return Ok(true);
        }
        if let Some(value) = self.parse_header(line, "Content-Transfer-Encoding")? {
            self.transfer_encoding = if icontains(&value, b"base64") {
                Te::Base64
            } else if icontains(&value, b"quoted-printable") {
                Te::Qp
            } else {
                Te::DontCare
            };
            return Ok(true);
        }
        if let Some(value) = self.parse_header(line, "Message-ID")? {
            if self.add_message_id {
                self.message_id = Some(value);
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// `parse_header()`: match `<hdr>:` case-insensitively and RFC 2047-decode
    /// the value.
    fn parse_header(&mut self, line: &[u8], hdr: &str) -> Result<Option<Vec<u8>>> {
        let Some(value) = skip_header(line, hdr) else {
            return Ok(None);
        };
        let mut value = value.to_vec();
        self.decode_header(&mut value)?;
        Ok(Some(value))
    }

    /// `decode_header()`: expand every `=?charset?B|Q?text?=` encoded word.
    ///
    /// A malformed word aborts the whole decode: the value is left as-is and
    /// `input_error` is set, exactly as the C `goto release_return` does.
    fn decode_header(&mut self, it: &mut Vec<u8>) -> Result<()> {
        let v = it.clone();
        let mut out: Vec<u8> = Vec::new();
        let mut cursor = 0usize;

        loop {
            let Some(rel) = find(&v[cursor..], b"=?") else {
                break;
            };
            let word = cursor + rel;

            if cursor != word {
                // Keep what precedes the encoded word, unless it is only the
                // linear whitespace separating two encoded words.
                let only_lws = v[cursor..word].iter().all(|&b| is_space(b));
                if !only_lws || cursor == 0 {
                    out.extend_from_slice(&v[cursor..word]);
                }
            }

            let ep = word + 2;
            if ep >= v.len() {
                self.input_error = -1;
                return Ok(());
            }
            let Some(cp) = find(&v[ep..], b"?").map(|p| ep + p) else {
                self.input_error = -1;
                return Ok(());
            };
            if cp + 3 > v.len() {
                self.input_error = -1;
                return Ok(());
            }
            let charset = v[ep..cp].to_vec();
            let encoding = v[cp + 1];
            if encoding == 0 || v[cp + 2] != b'?' {
                self.input_error = -1;
                return Ok(());
            }
            let Some(end) = find(&v[cp + 3..], b"?=").map(|p| cp + 3 + p) else {
                self.input_error = -1;
                return Ok(());
            };
            let piece = &v[cp + 3..end];

            let mut dec = match encoding.to_ascii_lowercase() {
                b'b' => decode_b_segment(piece),
                b'q' => decode_q_segment(piece, true),
                _ => {
                    self.input_error = -1;
                    return Ok(());
                }
            };
            if self.convert_to_utf8(&mut dec, &charset)? {
                return Ok(());
            }
            out.extend_from_slice(&dec);
            cursor = end + 2;
        }

        out.extend_from_slice(cstr(&v[cursor..]));
        *it = out;
        Ok(())
    }

    /// `convert_to_utf8()`. Returns true when the conversion failed, which is
    /// how the C callers read its non-zero return.
    fn convert_to_utf8(&mut self, line: &mut Vec<u8>, charset: &[u8]) -> Result<bool> {
        let Some(target) = self.metainfo_charset.clone() else {
            return Ok(false);
        };
        if charset.is_empty() {
            return Ok(false);
        }
        let source = String::from_utf8_lossy(cstr(charset)).into_owned();
        if source.is_empty() || same_encoding(&target, &source) {
            return Ok(false);
        }

        match reencode(line, &source, &target) {
            Some(converted) => {
                *line = converted;
                Ok(false)
            }
            // git hands every other pair to iconv; without it, guessing bytes
            // would be worse than stopping.
            None => bail!(
                "cannot convert from {source} to {target}: charset re-coding beyond \
                 UTF-8/US-ASCII/ISO-8859-1 needs iconv, which is not vendored (use -n)"
            ),
        }
    }

    /// `handle_content_type()`: push a boundary and pick up `charset=`,
    /// `format=flowed` and `delsp=yes`.
    fn handle_content_type(&mut self, line: &[u8]) {
        self.format_flowed = has_attr_value(line, b"format=", b"flowed");
        self.delsp = has_attr_value(line, b"delsp=", b"yes");

        if let Some(mut boundary) = slurp_attr(line, b"boundary=") {
            // `content_top` is pre-incremented from the unused slot 0.
            if self.content.len() + 1 >= MAX_BOUNDARIES {
                eprintln!("error: Too many boundaries to handle");
                self.input_error = -1;
                return;
            }
            let mut marker = b"--".to_vec();
            marker.append(&mut boundary);
            self.content.push(marker);
        }
        self.charset = slurp_attr(line, b"charset=").unwrap_or_default();
    }

    /// `handle_body()`: walk the message body, peeling MIME parts and transfer
    /// encodings before feeding lines to the log-message/patch filter.
    fn handle_body(&mut self, line: &mut Vec<u8>) -> Result<()> {
        let mut prev: Vec<u8> = Vec::new();

        if !self.content.is_empty() && !self.find_boundary(line) {
            return Ok(());
        }

        loop {
            if !self.content.is_empty() && self.is_multipart_boundary(line) {
                if !prev.is_empty() {
                    let mut flush = std::mem::take(&mut prev);
                    self.handle_filter(&mut flush)?;
                }
                self.summarize_quoted_cr();
                self.have_quoted_cr = false;
                if !self.handle_boundary(line)? {
                    return Ok(());
                }
            }

            self.decode_transfer_encoding(line);

            match self.transfer_encoding {
                Te::Base64 | Te::Qp => {
                    // A decoded chunk can hold several lines; feed them singly.
                    let mut joined = std::mem::take(&mut prev);
                    joined.extend_from_slice(line);
                    *line = joined;

                    let mut parts = split_keep(line, b'\n');
                    let last = parts.len().saturating_sub(1);
                    for idx in 0..parts.len() {
                        let mut part = std::mem::take(&mut parts[idx]);
                        if idx == last && part.last() != Some(&b'\n') {
                            prev = part;
                            break;
                        }
                        self.handle_filter_flowed(&mut part, &mut prev)?;
                    }
                }
                Te::DontCare => self.handle_filter_flowed(line, &mut prev)?,
            }

            if self.input_error != 0 {
                break;
            }
            if !self.input.getwholeline(line) {
                break;
            }
        }

        if !prev.is_empty() {
            let mut flush = prev;
            self.handle_filter(&mut flush)?;
        }
        self.summarize_quoted_cr();
        self.flush_inbody_header_accum()?;
        Ok(())
    }

    /// `find_boundary()`: read until the current boundary marker shows up.
    fn find_boundary(&mut self, line: &mut Vec<u8>) -> bool {
        while self.input.getline_lf(line) {
            if !self.content.is_empty() && self.is_multipart_boundary(line) {
                return true;
            }
        }
        false
    }

    /// `is_multipart_boundary()`.
    fn is_multipart_boundary(&self, line: &[u8]) -> bool {
        match self.content.last() {
            Some(top) => top.len() <= line.len() && line[..top.len()] == top[..],
            None => false,
        }
    }

    /// `handle_boundary()`: cross into the next MIME part, popping closed
    /// boundaries and reading the part's own headers. False means end of input.
    fn handle_boundary(&mut self, line: &mut Vec<u8>) -> Result<bool> {
        loop {
            let top_len = self.content.last().map_or(0, Vec::len);
            if line.len() >= top_len + 2 && line[top_len..].starts_with(b"--") {
                // An end boundary: pop it, emit the separating newline, and go
                // looking for the enclosing part's next boundary.
                self.content.pop();
                let mut newline = b"\n".to_vec();
                self.handle_filter(&mut newline)?;
                if self.input_error != 0 {
                    return Ok(false);
                }
                if !self.find_boundary(line) {
                    return Ok(false);
                }
                continue;
            }

            self.transfer_encoding = Te::DontCare;
            self.charset.clear();

            let mut header = Vec::new();
            while self.read_one_header_line(&mut header) {
                let h = header.clone();
                self.check_header(&h, true, false)?;
            }

            if !self.input.getline_lf(line) {
                return Ok(false);
            }
            line.push(b'\n');
            return Ok(true);
        }
    }

    /// `decode_transfer_encoding()`.
    fn decode_transfer_encoding(&self, line: &mut Vec<u8>) {
        match self.transfer_encoding {
            Te::Qp => *line = decode_q_segment(line, false),
            Te::Base64 => *line = decode_b_segment(line),
            Te::DontCare => {}
        }
    }

    /// `summarize_quoted_cr()`.
    fn summarize_quoted_cr(&self) {
        if self.have_quoted_cr && self.quoted_cr == QuotedCr::Warn {
            eprintln!("warning: quoted CRLF detected");
        }
    }

    /// `handle_filter_flowed()`: apply `format=flowed` unstuffing and soft line
    /// breaks, or just the quoted-CR policy when the message is not flowed.
    fn handle_filter_flowed(&mut self, line: &mut Vec<u8>, prev: &mut Vec<u8>) -> Result<()> {
        let mut len = line.len();

        if !self.format_flowed {
            if len >= 2 && line[len - 2] == b'\r' && line[len - 1] == b'\n' {
                self.have_quoted_cr = true;
                if self.quoted_cr == QuotedCr::Strip {
                    line.truncate(len - 2);
                    line.push(b'\n');
                }
            }
            return self.handle_filter(line);
        }

        if line.last() == Some(&b'\n') {
            len -= 1;
            if len > 0 && line[len - 1] == b'\r' {
                len -= 1;
            }
        }

        // The signature separator is never reflowed.
        if len == 3 && line.starts_with(b"-- ") {
            if !prev.is_empty() {
                let mut flush = std::mem::take(prev);
                self.handle_filter(&mut flush)?;
            }
            return self.handle_filter(line);
        }

        if len > 0 && line[0] == b' ' {
            line.remove(0);
            len -= 1;
        }

        // A trailing space is a soft line break: hold the line for the next one.
        if len > 0 && line[len - 1] == b' ' {
            let keep = len - usize::from(self.delsp);
            prev.extend_from_slice(&line[..keep]);
            return Ok(());
        }

        let mut joined = std::mem::take(prev);
        joined.extend_from_slice(line);
        *line = joined;
        self.handle_filter(line)
    }

    /// `handle_filter()`: log message until the patch starts, patch after.
    fn handle_filter(&mut self, line: &mut Vec<u8>) -> Result<()> {
        if self.filter_stage == 0 {
            if !self.handle_commit_msg(line)? {
                return Ok(());
            }
            self.filter_stage += 1;
        }
        self.patch.extend_from_slice(line);
        self.patch_lines += 1;
        Ok(())
    }

    /// `handle_commit_msg()`: returns true once `line` begins the patch.
    fn handle_commit_msg(&mut self, line: &mut Vec<u8>) -> Result<bool> {
        if self.header_stage && (line.is_empty() || line.as_slice() == b"\n") {
            if !self.inbody_header_accum.is_empty() {
                self.flush_inbody_header_accum()?;
                self.header_stage = false;
            }
            return Ok(false);
        }

        if self.use_inbody_headers && self.header_stage {
            self.header_stage = self.check_inbody_header(line)?;
            if self.header_stage {
                return Ok(false);
            }
        } else {
            // Without in-body headers only the first blank line is dropped.
            self.header_stage = false;
        }

        let charset = self.charset.clone();
        if self.convert_to_utf8(line, &charset)? {
            return Ok(false);
        }

        if self.use_scissors && is_scissors_line(cstr(line)) {
            self.log_message.clear();
            self.header_stage = true;
            // Secondary headers read before the cut do not apply after it.
            self.s_hdr = [None, None, None];
            return Ok(false);
        }

        if patchbreak(line) {
            if let Some(id) = self.message_id.clone() {
                self.log_message.extend_from_slice(b"Message-ID: ");
                self.log_message.extend_from_slice(cstr(&id));
                self.log_message.push(b'\n');
            }
            return Ok(true);
        }

        self.log_message.extend_from_slice(line);
        Ok(false)
    }

    /// `check_inbody_header()`: recognise a header written in the message body.
    fn check_inbody_header(&mut self, line: &[u8]) -> Result<bool> {
        if !self.inbody_header_accum.is_empty()
            && (line.first() == Some(&b' ') || line.first() == Some(&b'\t'))
        {
            if self.use_scissors && is_scissors_line(cstr(line)) {
                self.flush_inbody_header_accum()?;
                return Ok(false);
            }
            if self.inbody_header_accum.last() == Some(&b'\n') {
                self.inbody_header_accum.pop();
            }
            self.inbody_header_accum.extend_from_slice(line);
            return Ok(true);
        }

        self.flush_inbody_header_accum()?;

        if line.starts_with(b">From") && line.get(5).is_some_and(|&b| is_space(b)) {
            return Ok(is_format_patch_separator(&line[1..]));
        }
        if line.starts_with(b"[PATCH]") && line.get(7).is_some_and(|&b| is_space(b)) {
            for (i, name) in HEADER.iter().enumerate() {
                if *name == "Subject" {
                    self.s_hdr[i] = Some(line.to_vec());
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if self.is_inbody_header(line) {
            self.inbody_header_accum.extend_from_slice(line);
            return Ok(true);
        }
        Ok(false)
    }

    /// `is_inbody_header()`: a tracked header that the body has not set yet.
    fn is_inbody_header(&self, line: &[u8]) -> bool {
        HEADER
            .iter()
            .enumerate()
            .any(|(i, name)| self.s_hdr[i].is_none() && skip_header(line, name).is_some())
    }

    /// `flush_inbody_header_accum()`.
    fn flush_inbody_header_accum(&mut self) -> Result<()> {
        if self.inbody_header_accum.is_empty() {
            return Ok(());
        }
        let accum = std::mem::take(&mut self.inbody_header_accum);
        self.check_header(&accum, false, false)?;
        Ok(())
    }

    /// `handle_from()`: split a `From:` value into `name` and `email`.
    fn handle_from(&mut self, from: &[u8]) {
        let mut f = unquote_quoted_pair(from);

        let Some(at) = cstr(&f).iter().position(|&b| b == b'@') else {
            self.parse_bogus_from(from);
            return;
        };

        // A second address in a line we already have an address for is noise.
        if !self.email.is_empty() && cstr(&f[at + 1..]).contains(&b'@') {
            return;
        }

        // Widen leftwards to the start of the address, dropping any `<`.
        let mut start = at;
        while start > 0 {
            let c = f[start - 1];
            if is_space(c) {
                break;
            }
            if c == b'<' {
                f[start - 1] = b' ';
                break;
            }
            start -= 1;
        }

        let tail = cstr(&f[start..]);
        let el = tail
            .iter()
            .position(|b| b" \n\t\r\x0b\x0c>".contains(b))
            .unwrap_or(tail.len());
        self.email = tail[..el].to_vec();

        // Also drop the delimiter that ended the address, when there was one.
        let extra = usize::from(start + el < f.len() && f[start + el] != 0);
        f.drain(start..start + el + extra);

        cleanup_space(&mut f);
        trim(&mut f);
        if f.first() == Some(&b'(') && f.last() == Some(&b')') {
            f.pop();
            f.remove(0);
        }

        self.name = get_sane_name(&f, &self.email);
    }

    /// `parse_bogus_from()`: the `John Doe <johndoe>` fallback.
    fn parse_bogus_from(&mut self, line: &[u8]) {
        if !self.email.is_empty() {
            return;
        }
        let s = cstr(line);
        let Some(bra) = s.iter().position(|&b| b == b'<') else {
            return;
        };
        let Some(rel) = s[bra..].iter().position(|&b| b == b'>') else {
            return;
        };
        let ket = bra + rel;

        self.email = s[bra + 1..ket].to_vec();
        let mut name = s[..bra].to_vec();
        trim(&mut name);
        self.name = get_sane_name(&name, &self.email);
    }

    /// `handle_info()`: render the `Author:`/`Email:`/`Subject:`/`Date:` block.
    fn handle_info(&mut self) -> Vec<u8> {
        let mut out = Vec::new();

        for i in 0..HEADER.len() {
            // In-body headers only win once a patch was actually produced.
            let mut hdr = if self.patch_lines != 0 && self.s_hdr[i].is_some() {
                self.s_hdr[i].clone().expect("just checked")
            } else if let Some(h) = &self.p_hdr[i] {
                h.clone()
            } else {
                continue;
            };

            if hdr.contains(&0) {
                eprintln!("error: a NUL byte in '{}' is not allowed.", HEADER[i]);
                self.input_error = -1;
            }

            match HEADER[i] {
                "Subject" => {
                    if !self.keep_subject {
                        cleanup_subject(&mut hdr, self.keep_non_patch_brackets_in_subject);
                        cleanup_space(&mut hdr);
                    }
                    // `-k` can leave embedded newlines, one output line each.
                    for part in cstr(&hdr).split(|&b| b == b'\n') {
                        out.extend_from_slice(b"Subject: ");
                        out.extend_from_slice(part);
                        out.push(b'\n');
                    }
                }
                "From" => {
                    cleanup_space(&mut hdr);
                    self.handle_from(&hdr);
                    out.extend_from_slice(b"Author: ");
                    out.extend_from_slice(cstr(&self.name));
                    out.push(b'\n');
                    out.extend_from_slice(b"Email: ");
                    out.extend_from_slice(cstr(&self.email));
                    out.push(b'\n');
                }
                name => {
                    cleanup_space(&mut hdr);
                    out.extend_from_slice(name.as_bytes());
                    out.extend_from_slice(b": ");
                    out.extend_from_slice(cstr(&hdr));
                    out.push(b'\n');
                }
            }
        }

        out.push(b'\n');
        out
    }
}

/// The prefix of `v` before its first NUL, mirroring C string semantics for the
/// spots where git passes a `strbuf`'s buffer to `strchr`/`printf`.
fn cstr(v: &[u8]) -> &[u8] {
    match v.iter().position(|&b| b == 0) {
        Some(i) => &v[..i],
        None => v,
    }
}

/// C's `isspace()` in the C locale, for bytes.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strbuf_rtrim()`.
fn rtrim(v: &mut Vec<u8>) {
    while v.last().is_some_and(|&b| is_space(b)) {
        v.pop();
    }
}

/// `strbuf_trim()`.
fn trim(v: &mut Vec<u8>) {
    rtrim(v);
    let start = v.iter().position(|&b| !is_space(b)).unwrap_or(v.len());
    v.drain(..start);
}

/// `cleanup_space()`: every whitespace run becomes a single space.
fn cleanup_space(sb: &mut Vec<u8>) {
    let mut pos = 0;
    while pos < sb.len() {
        if is_space(sb[pos]) {
            sb[pos] = b' ';
            let mut cnt = 0;
            while pos + cnt + 1 < sb.len() && is_space(sb[pos + cnt + 1]) {
                cnt += 1;
            }
            sb.drain(pos + 1..pos + 1 + cnt);
        }
        pos += 1;
    }
}

/// The first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// `strcasestr()`.
fn ifind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Whether `haystack` contains `needle`, case-insensitively.
fn icontains(haystack: &[u8], needle: &[u8]) -> bool {
    ifind(haystack, needle).is_some()
}

/// `skip_header()`: match `<hdr>:` case-insensitively and skip leading spaces.
fn skip_header<'a>(line: &'a [u8], hdr: &str) -> Option<&'a [u8]> {
    let name = hdr.as_bytes();
    if line.len() <= name.len() || !line[..name.len()].eq_ignore_ascii_case(name) {
        return None;
    }
    if line[name.len()] != b':' {
        return None;
    }
    let mut at = name.len() + 1;
    while at < line.len() && is_space(line[at]) {
        at += 1;
    }
    Some(&line[at..])
}

/// `is_rfc2822_header()`.
fn is_rfc2822_header(line: &[u8]) -> bool {
    if line.starts_with(b"From ") || line.starts_with(b">From ") {
        return true;
    }
    for &ch in cstr(line) {
        if ch == b':' {
            return true;
        }
        if (33..=57).contains(&ch) || (59..=126).contains(&ch) {
            continue;
        }
        break;
    }
    false
}

/// `is_format_patch_separator()`: the `From <sha1> Mon Sep 17 ...` line.
fn is_format_patch_separator(line: &[u8]) -> bool {
    const SAMPLE: &[u8] =
        b"From e6807f3efca28b30decfecb1732a56c7db1137ee Mon Sep 17 00:00:00 2001\n";
    if line.len() != SAMPLE.len() {
        return false;
    }
    let Some(cp) = line.strip_prefix(b"From ") else {
        return false;
    };
    if cp.len() < 40 || !cp[..40].iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    let off = SAMPLE.len() - cp.len() + 40;
    line[off..] == SAMPLE[off..]
}

/// `slurp_attr()`: the value of `name` in a structured header line.
fn slurp_attr(line: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    let at = ifind(line, name)? + name.len();
    let rest = &line[at..];
    let (rest, ends): (&[u8], &[u8]) = match rest.first() {
        Some(&b'"') => (&rest[1..], b"\""),
        _ => (rest, b"; \t"),
    };
    let end = rest
        .iter()
        .position(|b| ends.contains(b) || *b == 0)
        .unwrap_or(rest.len());
    Some(rest[..end].to_vec())
}

/// `has_attr_value()`.
fn has_attr_value(line: &[u8], name: &[u8], value: &[u8]) -> bool {
    slurp_attr(line, name).is_some_and(|v| v.eq_ignore_ascii_case(value))
}

/// `decode_q_segment()`. `rfc2047` also maps `_` to a space.
fn decode_q_segment(seg: &[u8], rfc2047: bool) -> Vec<u8> {
    let s = cstr(seg);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let mut c = s[i];
        i += 1;
        if c == b'=' {
            let d = s.get(i).copied().unwrap_or(0);
            if d == b'\n' || d == 0 {
                break; // soft line break, or a trailing `=`
            }
            if let Some(ch) = hex2chr(&s[i..]) {
                out.push(ch);
                i += 2;
                continue;
            }
            // Garbage: fall through and keep the `=` verbatim.
        }
        if rfc2047 && c == b'_' {
            c = 0x20;
        }
        out.push(c);
    }
    out
}

/// Two hexadecimal digits as a byte.
fn hex2chr(s: &[u8]) -> Option<u8> {
    let (a, b) = (s.first()?, s.get(1)?);
    let v = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    Some(v(*a)? * 16 + v(*b)?)
}

/// `decode_b_segment()`: base64, silently skipping non-alphabet bytes.
fn decode_b_segment(seg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(seg.len());
    let (mut pos, mut acc) = (0u8, 0u8);
    for &c in cstr(seg) {
        let v = match c {
            b'+' => 62,
            b'/' => 63,
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            _ => continue,
        };
        match pos {
            0 => {
                acc = v << 2;
                pos = 1;
            }
            1 => {
                out.push(acc | (v >> 4));
                acc = (v & 15) << 4;
                pos = 2;
            }
            2 => {
                out.push(acc | (v >> 2));
                acc = (v & 3) << 6;
                pos = 3;
            }
            _ => {
                out.push(acc | v);
                acc = 0;
                pos = 0;
            }
        }
    }
    out
}

/// `same_utf_encoding()` folded into `same_encoding()`: `UTF-16BE` and
/// `UTF16BE` name the same encoding, and otherwise names compare case-blind.
fn same_encoding(src: &str, dst: &str) -> bool {
    // A named fn, not a closure: closure inference ties the input and output
    // lifetimes to one inference variable, which cannot outlive the call.
    fn variant(s: &str) -> Option<&str> {
        let rest = s.strip_prefix("utf").or_else(|| {
            s.get(..3)
                .filter(|p| p.eq_ignore_ascii_case("utf"))
                .map(|_| &s[3..])
        })?;
        Some(rest.strip_prefix('-').unwrap_or(rest))
    }
    if let (Some(a), Some(b)) = (variant(src), variant(dst)) {
        if a.eq_ignore_ascii_case(b) {
            return true;
        }
    }
    src.eq_ignore_ascii_case(dst)
}

/// Whether `name` spells UTF-8.
fn is_utf8_name(name: &str) -> bool {
    same_encoding("UTF-8", name)
}

/// Whether `name` spells US-ASCII.
fn is_ascii_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("us-ascii") || name.eq_ignore_ascii_case("ascii")
}

/// Whether `name` spells ISO-8859-1.
fn is_latin1_name(name: &str) -> bool {
    ["iso-8859-1", "iso8859-1", "iso88591", "latin1", "latin-1"]
        .iter()
        .any(|c| name.eq_ignore_ascii_case(c))
}

/// The subset of `reencode_string_len()` that needs no iconv: the pairs among
/// UTF-8, US-ASCII and ISO-8859-1. `None` means "unsupported or invalid".
fn reencode(data: &[u8], from: &str, to: &str) -> Option<Vec<u8>> {
    if is_ascii_name(from) {
        // Valid US-ASCII is already valid UTF-8 and valid ISO-8859-1.
        return data.iter().all(u8::is_ascii).then(|| data.to_vec());
    }
    if is_latin1_name(from) && is_utf8_name(to) {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            let mut enc = [0u8; 4];
            out.extend_from_slice((b as char).encode_utf8(&mut enc).as_bytes());
        }
        return Some(out);
    }
    if is_utf8_name(from) && is_latin1_name(to) {
        let text = std::str::from_utf8(data).ok()?;
        let mut out = Vec::with_capacity(text.len());
        for ch in text.chars() {
            out.push(u8::try_from(ch as u32).ok()?);
        }
        return Some(out);
    }
    None
}

/// `get_sane_name()`: fall back to the address when the name looks wrong.
fn get_sane_name(name: &[u8], email: &[u8]) -> Vec<u8> {
    if name.is_empty() || name.len() > 60 || cstr(name).iter().any(|b| b"@<>".contains(b)) {
        email.to_vec()
    } else {
        name.to_vec()
    }
}

/// `unquote_quoted_pair()`: unwrap quoted strings and comments in an address.
fn unquote_quoted_pair(line: &[u8]) -> Vec<u8> {
    let s = cstr(line);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        i += 1;
        match c {
            b'"' => i = unquote_quoted_string(&mut out, s, i),
            b'(' => i = unquote_comment(&mut out, s, i),
            _ => out.push(c),
        }
    }
    out
}

/// `unquote_quoted_string()`: copy until the closing quote, honouring `\`.
fn unquote_quoted_string(out: &mut Vec<u8>, s: &[u8], mut i: usize) -> usize {
    let mut literal = false;
    while i < s.len() {
        let c = s[i];
        i += 1;
        if literal {
            literal = false;
        } else {
            match c {
                b'\\' => {
                    literal = true;
                    continue;
                }
                b'"' => return i,
                _ => {}
            }
        }
        out.push(c);
    }
    i
}

/// `unquote_comment()`: copy a possibly nested `(...)` comment, parens included.
fn unquote_comment(out: &mut Vec<u8>, s: &[u8], mut i: usize) -> usize {
    let mut literal = false;
    let mut depth = 1;
    out.push(b'(');
    while i < s.len() {
        let c = s[i];
        i += 1;
        if literal {
            literal = false;
        } else {
            match c {
                b'\\' => {
                    literal = true;
                    continue;
                }
                b'(' => {
                    out.push(b'(');
                    depth += 1;
                    continue;
                }
                b')' => {
                    out.push(b')');
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    i
}

/// `cleanup_subject()`: strip `Re:`, leading punctuation and `[...]` tags.
fn cleanup_subject(subject: &mut Vec<u8>, keep_non_patch_brackets: bool) {
    let mut at = 0usize;
    while at < subject.len() {
        match subject[at] {
            b'r' | b'R' => {
                if subject.len() <= at + 3 {
                    break;
                }
                if (subject[at + 1] == b'e' || subject[at + 1] == b'E') && subject[at + 2] == b':' {
                    subject.drain(at..at + 3);
                    continue;
                }
                // A lone `r` ends the scan in git, since the switch falls out
                // of the enclosing loop.
                break;
            }
            b' ' | b'\t' | b':' => {
                subject.remove(at);
                continue;
            }
            b'[' => {
                let Some(rel) = subject[at..].iter().position(|&b| b == b']') else {
                    break;
                };
                let remove = rel + 1;
                if !keep_non_patch_brackets
                    || (remove >= 7 && find(&subject[at..at + remove], b"PATCH").is_some())
                {
                    subject.drain(at..at + remove);
                } else {
                    at += remove;
                    // Keep one space after a retained `]`; the later
                    // `cleanup_space()` normalises whatever follows.
                    if subject.get(at).is_some_and(|&b| is_space(b)) {
                        at += 1;
                    }
                }
                continue;
            }
            _ => break,
        }
    }
    trim(subject);
}

/// `is_scissors_line()`: a `-- >8 --` style cut mark.
fn is_scissors_line(line: &[u8]) -> bool {
    let (mut scissors, mut gap) = (0usize, 0usize);
    let (mut first, mut last): (Option<usize>, Option<usize>) = (None, None);
    let (mut perforation, mut in_perforation) = (0usize, false);

    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        if is_space(c) {
            if in_perforation {
                perforation += 1;
                gap += 1;
            }
            i += 1;
            continue;
        }
        last = Some(i);
        if first.is_none() {
            first = Some(i);
        }
        if c == b'-' {
            in_perforation = true;
            perforation += 1;
            i += 1;
            continue;
        }
        let pair = &line[i..(i + 2).min(line.len())];
        if pair == b">8" || pair == b"8<" || pair == b">%" || pair == b"%<" {
            in_perforation = true;
            perforation += 2;
            scissors += 2;
            i += 2;
            continue;
        }
        in_perforation = false;
        i += 1;
    }

    // At least 8 visible columns, the perforation more than a third of them,
    // and the dashes/scissors more than half the perforation.
    let visible = match (first, last) {
        (Some(f), Some(l)) => l - f + 1,
        _ => 0,
    };
    scissors != 0 && 8 <= visible && visible < perforation * 3 && gap * 2 < perforation
}

/// `patchbreak()`: the line that ends the log message and starts the patch.
fn patchbreak(line: &[u8]) -> bool {
    if line.starts_with(b"diff -") || line.starts_with(b"Index: ") {
        return true;
    }
    if line.len() < 4 {
        return false;
    }
    if !line.starts_with(b"---") {
        return false;
    }
    // `--- <filename>`; the byte past a 4-byte line is C's NUL terminator.
    if line[3] == b' ' && !is_space(line.get(4).copied().unwrap_or(0)) {
        return true;
    }
    // `---` followed only by whitespace up to the newline is a separator.
    for &c in &line[3..] {
        if c == b'\n' {
            return true;
        }
        if !is_space(c) {
            break;
        }
    }
    false
}

/// `strbuf_split()`: split after each terminator, keeping it on the chunk.
fn split_keep(data: &[u8], terminator: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let end = match data[start..].iter().position(|&b| b == terminator) {
            Some(i) => start + i + 1,
            None => data.len(),
        };
        out.push(data[start..end].to_vec());
        start = end;
    }
    out
}

/// `strerror()` text for an I/O error: Rust appends ` (os error <n>)` to the
/// system string, which `perror()` does not print.
fn perror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) if text.ends_with(')') => text[..at].to_string(),
        _ => text,
    }
}
