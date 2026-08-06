//! `git send-email` — send a collection of patches as email.
//!
//! Stock `git-send-email` is a Perl script (`git-send-email.perl`, 2474 lines in
//! git 2.55.0). Almost none of what it does is git work: it composes RFC 2822
//! messages out of patch files and then hands them either to a `sendmail`-style
//! program or to a `Net::SMTP` socket.
//!
//! The composing half and both transports are ported in full. Every
//! configuration variable and option that steers *which bytes get produced* —
//! the sender, the recipient lists, the alias files, the mailmap, the `Cc`
//! suppressions, the threading headers, the transfer encoding, the validation
//! hook, the confirmation prompt, the editor sessions — is live here, and the
//! bytes that reach the program are byte-identical to the ones stock produces
//! (modulo `Date:` and `Message-ID:`, which are functions of the clock and the
//! pid).
//!
//! The socket transport lives in [`super::smtp`], which ports the `Net::SMTP`
//! branch of `send_message` along with the parts of `Net::SMTP`, `Net::Cmd`,
//! `IO::Socket::SSL` and `Authen::SASL` it reaches; that module's own docs list
//! what it covers and what it does not. All output below was captured from git
//! 2.55.0 on Darwin.
//!
//! ### Covered (byte-identical stdout/stderr and exit code)
//!
//! * The three `GetOptions` passes in source order — `%identity_options`, then
//!   `%dump_aliases_options`, then `%options` — under
//!   `Getopt::Long::Configure qw/ pass_through /`. That means: `permute` (option
//!   scanning continues past positionals), `auto_abbrev` (unique prefixes such
//!   as `--dump-al`), `ignore_case` (`--DUMP-ALIASES`), single-dash long forms
//!   (`-dump-aliases`), `--opt=value` and `--opt value`, `!`-negation
//!   (`--no-thread` / `--nothread`), and pass-through of anything unknown,
//!   ambiguous, or missing a required value. `--` stops the scan and is itself
//!   left in `@ARGV`, which pass-through does not remove.
//! * `-h` (and `--h`, `-H`): the 5485-byte usage block on **stdout**, exit 1.
//!   It is checked after all three passes, and it suppresses the two
//!   `--dump-aliases` `die`s, so `-h --dump-aliases foo` prints usage.
//! * `--dump-aliases incompatible with other options` and `--dump-aliases and
//!   --translate-aliases are mutually exclusive`, both on stderr. The first
//!   fires whenever anything at all is left in `@ARGV` after the first two
//!   passes, so `--dump-aliases --from x` dies even though `--from` is a real
//!   option: it is not parsed until the third pass.
//! * `fatal: found configuration options for 'sendmail'` plus its two follow-up
//!   lines, gated by `sendemail.forbidSendmailVariables`.
//! * `Cannot run git format-patch from outside a repository`,
//!   `` `batch-size` and `relogin` must be specified together (via command-line
//!   or configuration option) ``, `Unknown --suppress-cc field: '<x>'` and
//!   `Unknown --confirm setting: '<x>'`, in that order — the order the script
//!   evaluates them.
//! * `read_config` over all three setting tables: `sendemail.<identity>.*`
//!   before `sendemail.*`, first prefix wins per setting, `sendemail.identity`
//!   yielding to `--identity` and `--no-identity`, list-valued keys
//!   (`aliasesfile`, `suppresscc`, `to`, `cc`, `bcc`, `smtpserveroption`)
//!   taking every value, scalar keys taking the last, `signedoffbycc` and
//!   `signedoffcc` feeding one variable under two setting names, and paths
//!   going through `Git::config_path`. The command line then writes the same
//!   variables, in the order the options were spelled, which is why
//!   `--suppress-cc` and `--smtp-server-option` append to what the
//!   configuration set rather than replacing it while `--to`/`--cc`/`--bcc`
//!   accumulate separately and replace it wholesale.
//! * `--dump-aliases`: the alias files named by `sendemail.aliasesfile` are read
//!   with the parser named by `sendemail.aliasfiletype`, and the alias names are
//!   printed one per line in byte order, exit 0. All six parsers are
//!   reproduced (`mutt`, `mailrc`, `pine`, `elm`, `sendmail`, `gnus`), including
//!   sendmail's line-continuation rules and its four `warning:` lines on stderr.
//!   An unset or unrecognised `aliasfiletype` yields no aliases, as in the
//!   script. A file that cannot be opened produces `opening <file>: <errno>` and
//!   exits with the errno, as Perl's `die` does.
//! * `--translate-aliases`: every line of stdin is run through
//!   `parse_address_line` (`Mail::Address`'s tokeniser, parser and `format`),
//!   `expand_aliases` (recursive, with the `alias '<x>' expands to itself` fatal
//!   for a cycle) and `sanitize_address_list`, and printed one address per line,
//!   exit 0. An address that names no alias passes through as it is. The alias
//!   values come from the same six parsers `--dump-aliases` uses, split with
//!   `Text::ParseWords::quotewords` exactly as each parser asks for it — on
//!   commas everywhere except mailrc, which splits on whitespace and drops the
//!   quotes.
//!
//!   `sanitize_address` is reproduced in full: the display name is left alone
//!   when it is already an ASCII quoted string or an RFC 2047 encoded word,
//!   otherwise its unescaped quotes are removed and it is re-quoted — as a
//!   `=?UTF-8?q?…?=` word when it holds a non-ASCII byte, in plain double quotes
//!   (backslash-escaping `\` and CR) when it holds a special or control
//!   character, and verbatim otherwise.
//! * The whole per-patch pipeline: the argument loop that turns files and
//!   directories into `@files` (with `is_format_patch_arg` disambiguating a name
//!   that is also a revision), `handle_backup_files`, the 8-bit scan that fills
//!   `%broken_encoding`, the `*** SUBJECT HERE ***` refusal, `pre_process_file`
//!   (header unfolding, `header-cmd`, the `%suppress_cc` decisions for the
//!   `From:`/`To:`/`Cc:` headers and the body's `-by:`/`Cc:` trailers,
//!   `to-cmd`/`cc-cmd`), `process_address_list` (`Mail::Address` parse, alias
//!   expansion, `sanitize_address`, `extract_valid_address` validation and
//!   `git check-mailmap`), `apply_transfer_encoding` (`MIME::QuotedPrint` and
//!   `MIME::Base64`, `auto` picking quoted-printable for a 999-byte line or a
//!   CR), `gen_header`, the `--confirm` prompt with its `inform` block, the
//!   threading of `In-Reply-To:`/`References:` across a series, `validate_patch`
//!   (the `sendemail-validate` hook through `git hook run`, the 998-character
//!   limit, and `Git::port_num` for the SMTP port), `do_edit` for `--annotate`,
//!   the `--batch-size`/`--relogin-delay` pause, and the `imap-send` copy.
//! * `--compose`: the `.gitsendemail.msg.XXXXXX` template under the git
//!   directory, the `GIT: ` comment block and per-patch subject list,
//!   `--annotate` folding the patches into the same editor session, the
//!   re-parse of the edited file into `<name>.final` (`GIT: ` stripping, the
//!   `MIME-Version:`/`Content-Type:`/`Content-Transfer-Encoding:` block a
//!   non-ASCII byte adds, `sendemail.composeEncoding` as that charset and as
//!   `quote_subject`'s label, and the headers lifted back into `$sender`,
//!   `@initial_to`, `@initial_cc`, `@initial_bcc`, `$reply_to`,
//!   `$initial_subject` and `$initial_in_reply_to`), the `Summary email is
//!   empty` skip, the composed message leading the series, and
//!   `cleanup_compose_files`. `--no-compose` is not an option in the script
//!   either: it falls through to `@rev_list_opts`.
//! * The transport for `--sendmail-cmd` and for an absolute `--smtp-server`: the
//!   `-f <envelope>`, `-i`, recipient and `--smtp-server-option` argument
//!   vector, `"$header\n$message"` on the program's stdin, and both the `quiet`
//!   (`Sent`/`Dry-Sent`) and verbose (`OK. Log says:` …) reports.
//! * The socket transport: the session opened (and reused, and closed by
//!   `--batch-size`) through [`super::smtp`], `smtp_auth_maybe`'s credential
//!   round trip, the Outlook `Message-ID` recovery and its effect on the next
//!   message's `In-Reply-To:`, and the `Result: <code> …` line the verbose
//!   report ends with.
//!
//! ### Exit status of the `die` paths
//!
//! Perl exits a `die` with `$! || ($? >> 8) || 255`. `$!` is 0 on every path
//! measured here, so the status is `$? >> 8` from the most recent `git` child
//! process, or 255 when that child succeeded. Which child that is depends on how
//! far the script got, so [`die`] tracks it rather than deriving it from the
//! configuration; the table on that function records the measurements.
//! `usage()` calls `exit(1)` explicitly and is not affected.
//!
//! Beyond the points [`die`] tracks the accounting stops holding, because `$!`
//! is whatever errno the last failed libc call in the Perl runtime happened to
//! leave behind: the same `die` observed exit 2 after the patches had been read
//! and 25 after the validation hook had run. The errno is an artefact of Perl's
//! own internal probing, not of anything the script decides, so it is not
//! reproduced; those paths report the tracked child status instead. Their
//! messages are byte-identical, except that a `die` whose
//! message does not end in a newline gets ` at <path-to-git-send-email> line
//! <n>.` appended by Perl — a path into the stock installation, which nothing
//! here can or should print. Those messages are:
//! `Send this email reply required`, `invalid transfer encoding`, `cannot send
//! message as 7bit`, `The destination IMAP folder is not properly defined.`,
//! `The required SMTP server is not properly defined.`, `No subject line in
//! <f>?`, `can't open file <f>` and the three `execute_cmd` failures.
//!
//! ### Not covered
//!
//! * The SASL mechanisms beyond `PLAIN` and `LOGIN` that `Authen::SASL` would
//!   supply, and `Authen::SASL`'s ranking between mechanisms — see
//!   [`super::smtp`].
//! * Revision arguments, which the script turns into patches by running
//!   `git format-patch -o <tmpdir>`. Naming patch files or a directory works;
//!   anything that falls through to `@rev_list_opts` bails.
//! * `--git-completion-helper`, which shells out to
//!   `git format-patch --git-completion-helper` and prints the union of both
//!   option lists. It bails.
//! * `%(prefix)` and `~user/` interpolation in `sendemail.aliasesfile`. A
//!   leading `~/` is expanded from `$HOME`; other forms are used verbatim.
//! * The `pine` parser matches on tab-delimited field structure rather than by
//!   backtracking the original regex character for character; the alias name it
//!   yields is the first field, which is all `--dump-aliases` prints.
//! * Header bytes that are not valid UTF-8. `$message` is carried and written as
//!   raw bytes, but a header line is decoded lossily before it is matched, so a
//!   display name in a legacy 8-bit charset would not survive verbatim. Patch
//!   headers out of `format-patch` are RFC 2047 encoded and unaffected.
//! * The `SIGINT`/`SIGTERM` handler, which resets the terminal attributes,
//!   re-enables echo and names the surviving `--compose` temporaries. Nothing
//!   here installs signal handlers, so an interrupted run leaves both files in
//!   the git directory without saying so.
//! * `Term::ReadLine`'s prompt decoration. With no terminal to open, `ask`
//!   returns its default without printing, which is what the script does and
//!   what every prompt path here was compared against; with a terminal the
//!   prompt is written plainly rather than with the ANSI attributes
//!   `Term::ReadLine::Perl` adds.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::config::File as ConfigFile;

/// `usage()` — the heredoc in `git-send-email.perl`, printed to stdout, then
/// `exit(1)`. 5485 bytes.
const USAGE: &str = concat!(
    "git send-email [<options>] <file|directory>\n",
    "git send-email [<options>] <format-patch options>\n",
    "git send-email --dump-aliases\n",
    "git send-email --translate-aliases\n",
    "\n",
    "  Composing:\n",
    "    --from                  <str>  * Email From:\n",
    "    --[no-]to               <str>  * Email To:\n",
    "    --[no-]cc               <str>  * Email Cc:\n",
    "    --[no-]bcc              <str>  * Email Bcc:\n",
    "    --subject               <str>  * Email \"Subject:\"\n",
    "    --reply-to              <str>  * Email \"Reply-To:\"\n",
    "    --in-reply-to           <str>  * Email \"In-Reply-To:\"\n",
    "    --[no-]outlook-id-fix          * The SMTP host is an Outlook server that munges the\n",
    "                                     Message-ID. Retrieve it from the server.\n",
    "    --[no-]xmailer                 * Add \"X-Mailer:\" header (default).\n",
    "    --[no-]annotate                * Review each patch that will be sent in an editor.\n",
    "    --compose                      * Open an editor for introduction.\n",
    "    --compose-encoding      <str>  * Encoding to assume for introduction.\n",
    "    --8bit-encoding         <str>  * Encoding to assume 8bit mails if undeclared\n",
    "    --transfer-encoding     <str>  * Transfer encoding to use (quoted-printable, 8bit, base64)\n",
    "    --[no-]mailmap                 * Use mailmap file to map all email addresses to canonical\n",
    "                                     real names and email addresses.\n",
    "\n",
    "  Sending:\n",
    "    --envelope-sender       <str>  * Email envelope sender.\n",
    "    --sendmail-cmd          <str>  * Command to run to send email.\n",
    "    --smtp-server       <str:int>  * Outgoing SMTP server to use. The port\n",
    "                                     is optional. Default 'localhost'.\n",
    "    --smtp-server-option    <str>  * Outgoing SMTP server option to use.\n",
    "    --smtp-server-port      <int>  * Outgoing SMTP server port.\n",
    "    --smtp-user             <str>  * Username for SMTP-AUTH.\n",
    "    --smtp-pass             <str>  * Password for SMTP-AUTH; not necessary.\n",
    "    --smtp-encryption       <str>  * tls or ssl; anything else disables.\n",
    "    --smtp-ssl                     * Deprecated. Use `--smtp-encryption ssl`.\n",
    "    --smtp-ssl-cert-path    <str>  * Path to ca-certificates (either directory or file).\n",
    "                                     Pass an empty string to disable certificate\n",
    "                                     verification.\n",
    "    --smtp-ssl-client-cert  <str>  * Path to the client certificate file\n",
    "    --smtp-ssl-client-key   <str>  * Path to the private key file for the client certificate\n",
    "    --smtp-domain           <str>  * The domain name sent to HELO/EHLO handshake\n",
    "    --smtp-auth             <str>  * Space-separated list of allowed AUTH mechanisms, or\n",
    "                                     \"none\" to disable authentication.\n",
    "                                     This setting forces to use one of the listed mechanisms.\n",
    "    --no-smtp-auth                 * Disable SMTP authentication. Shorthand for\n",
    "                                     `--smtp-auth=none`\n",
    "    --smtp-debug            <0|1>  * Disable, enable Net::SMTP debug.\n",
    "    --imap-sent-folder      <str>  * IMAP folder where a copy of the emails should be sent.\n",
    "                                     Make sure `git imap-send` is set up to use this feature.\n",
    "    --[no-]use-imap-only           * Only copy emails to the IMAP folder specified by\n",
    "                                     `--imap-sent-folder` instead of actually sending them.\n",
    "\n",
    "    --batch-size            <int>  * send max <int> message per connection.\n",
    "    --relogin-delay         <int>  * delay <int> seconds between two successive login.\n",
    "                                     This option can only be used with --batch-size\n",
    "\n",
    "  Automating:\n",
    "    --identity              <str>  * Use the sendemail.<id> options.\n",
    "    --to-cmd                <str>  * Email To: via `<str> $patch_path`.\n",
    "    --cc-cmd                <str>  * Email Cc: via `<str> $patch_path`.\n",
    "    --header-cmd            <str>  * Add headers via `<str> $patch_path`.\n",
    "    --no-header-cmd                * Disable any header command in use.\n",
    "    --suppress-cc           <str>  * author, self, sob, cc, cccmd, body, bodycc, misc-by, all.\n",
    "    --[no-]cc-cover                * Email Cc: addresses in the cover letter.\n",
    "    --[no-]to-cover                * Email To: addresses in the cover letter.\n",
    "    --[no-]signed-off-by-cc        * Send to Signed-off-by: addresses. Default on.\n",
    "    --[no-]suppress-from           * Send to self. Default off.\n",
    "    --[no-]chain-reply-to          * Chain In-Reply-To: fields. Default off.\n",
    "    --[no-]thread                  * Use In-Reply-To: field. Default on.\n",
    "\n",
    "  Administering:\n",
    "    --confirm               <str>  * Confirm recipients before sending;\n",
    "                                     auto, cc, compose, always, or never.\n",
    "    --quiet                        * Output one line of info per email.\n",
    "    --dry-run                      * Don't actually send the emails.\n",
    "    --[no-]validate                * Perform patch sanity checks. Default on.\n",
    "    --[no-]format-patch            * understand any non optional arguments as\n",
    "                                     `git format-patch` ones.\n",
    "    --force                        * Send even if safety checks would prevent it.\n",
    "\n",
    "  Information:\n",
    "    --dump-aliases                 * Dump configured aliases and exit.\n",
    "    --translate-aliases            * Translate aliases read from standard\n",
    "                                     input according to the configured email\n",
    "                                     alias file(s), outputting the result to\n",
    "                                     standard output.\n",
    "\n",
);

// ---------------------------------------------------------------------------
// Getopt::Long emulation
// ---------------------------------------------------------------------------

/// What an option does with the argument that follows it, mirroring the
/// `Getopt::Long` type suffixes used in the script.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No suffix: a plain switch.
    Flag,
    /// `!`: also matches `--no-<name>` and `--no<name>`.
    Negatable,
    /// `=s` / `=i`: a value is required, taken inline or from the next argument.
    Required,
    /// `:s` / `:i`: a value is optional; the next argument is taken only if it
    /// does not itself look like an option.
    Optional,
}

/// One `Getopt::Long` specification, e.g. `"sender|from=s"`.
struct Spec {
    /// The canonical name, used as the key in [`Parsed::values`].
    id: &'static str,
    /// Every spelling, in `|`-order.
    names: &'static [&'static str],
    kind: Kind,
}

const fn spec(id: &'static str, names: &'static [&'static str], kind: Kind) -> Spec {
    Spec { id, names, kind }
}

/// `%identity_options` — the first `GetOptions` pass.
const IDENTITY_OPTIONS: &[Spec] = &[
    spec("identity", &["identity"], Kind::Required),
    spec("no-identity", &["no-identity"], Kind::Flag),
];

/// `%dump_aliases_options` — the second pass.
const DUMP_ALIASES_OPTIONS: &[Spec] = &[
    spec("h", &["h"], Kind::Flag),
    spec("dump-aliases", &["dump-aliases"], Kind::Flag),
    spec("translate-aliases", &["translate-aliases"], Kind::Flag),
];

/// `%options` — the third pass, in declaration order.
const OPTIONS: &[Spec] = &[
    spec("sender", &["sender", "from"], Kind::Required),
    spec("in-reply-to", &["in-reply-to"], Kind::Required),
    spec("reply-to", &["reply-to"], Kind::Required),
    spec("subject", &["subject"], Kind::Required),
    spec("to", &["to"], Kind::Required),
    spec("to-cmd", &["to-cmd"], Kind::Required),
    spec("no-to", &["no-to"], Kind::Flag),
    spec("cc", &["cc"], Kind::Required),
    spec("no-cc", &["no-cc"], Kind::Flag),
    spec("bcc", &["bcc"], Kind::Required),
    spec("no-bcc", &["no-bcc"], Kind::Flag),
    spec("chain-reply-to", &["chain-reply-to"], Kind::Negatable),
    spec("sendmail-cmd", &["sendmail-cmd"], Kind::Required),
    spec("smtp-server", &["smtp-server"], Kind::Required),
    spec("smtp-server-option", &["smtp-server-option"], Kind::Required),
    spec("smtp-server-port", &["smtp-server-port"], Kind::Required),
    spec("smtp-user", &["smtp-user"], Kind::Required),
    spec("smtp-pass", &["smtp-pass"], Kind::Optional),
    spec("smtp-ssl", &["smtp-ssl"], Kind::Flag),
    spec("smtp-encryption", &["smtp-encryption"], Kind::Required),
    spec("smtp-ssl-cert-path", &["smtp-ssl-cert-path"], Kind::Required),
    spec("smtp-ssl-client-cert", &["smtp-ssl-client-cert"], Kind::Required),
    spec("smtp-ssl-client-key", &["smtp-ssl-client-key"], Kind::Required),
    spec("smtp-debug", &["smtp-debug"], Kind::Optional),
    spec("smtp-domain", &["smtp-domain"], Kind::Optional),
    spec("smtp-auth", &["smtp-auth"], Kind::Required),
    spec("no-smtp-auth", &["no-smtp-auth"], Kind::Flag),
    spec("imap-sent-folder", &["imap-sent-folder"], Kind::Required),
    spec("use-imap-only", &["use-imap-only"], Kind::Negatable),
    spec("annotate", &["annotate"], Kind::Negatable),
    spec("compose", &["compose"], Kind::Flag),
    spec("quiet", &["quiet"], Kind::Flag),
    spec("cc-cmd", &["cc-cmd"], Kind::Required),
    spec("header-cmd", &["header-cmd"], Kind::Required),
    spec("no-header-cmd", &["no-header-cmd"], Kind::Flag),
    spec("suppress-from", &["suppress-from"], Kind::Negatable),
    spec("suppress-cc", &["suppress-cc"], Kind::Required),
    spec("signed-off-cc", &["signed-off-cc", "signed-off-by-cc"], Kind::Negatable),
    spec("cc-cover", &["cc-cover"], Kind::Negatable),
    spec("to-cover", &["to-cover"], Kind::Negatable),
    spec("confirm", &["confirm"], Kind::Required),
    spec("dry-run", &["dry-run"], Kind::Flag),
    spec("envelope-sender", &["envelope-sender"], Kind::Required),
    spec("thread", &["thread"], Kind::Negatable),
    spec("validate", &["validate"], Kind::Negatable),
    spec("transfer-encoding", &["transfer-encoding"], Kind::Required),
    spec("mailmap", &["mailmap"], Kind::Negatable),
    spec("use-mailmap", &["use-mailmap"], Kind::Negatable),
    spec("format-patch", &["format-patch"], Kind::Negatable),
    spec("8bit-encoding", &["8bit-encoding"], Kind::Required),
    spec("compose-encoding", &["compose-encoding"], Kind::Required),
    spec("force", &["force"], Kind::Flag),
    spec("xmailer", &["xmailer"], Kind::Negatable),
    spec("batch-size", &["batch-size"], Kind::Required),
    spec("relogin-delay", &["relogin-delay"], Kind::Required),
    spec("git-completion-helper", &["git-completion-helper"], Kind::Flag),
    spec("v", &["v"], Kind::Required),
    spec("outlook-id-fix", &["outlook-id-fix"], Kind::Negatable),
];

/// One recognised occurrence: the spec's `id`, whether it was the `--no-` form,
/// and the value if the option took one.
struct Hit {
    id: &'static str,
    negated: bool,
    value: Option<String>,
}

/// The result of one `GetOptions` pass.
struct Parsed {
    hits: Vec<Hit>,
    /// `@ARGV` as the pass leaves it: positionals plus everything passed through.
    rest: Vec<String>,
}

impl Parsed {
    /// The last value stored for `id`, as Perl's scalar assignment leaves it.
    fn last(&self, id: &str) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.id == id)
    }

    /// Whether `id` was seen at all.
    fn seen(&self, id: &str) -> bool {
        self.hits.iter().any(|h| h.id == id)
    }

}

/// One candidate spelling of an option, as `Getopt::Long` registers it.
struct Candidate {
    name: String,
    id: &'static str,
    kind: Kind,
    negated: bool,
}

/// Build the lookup table for a pass. A `!` spec registers `no-<name>` and
/// `no<name>` alongside `<name>`, which is what makes those abbreviable too.
fn candidates(specs: &'static [Spec]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for s in specs {
        for name in s.names {
            out.push(Candidate { name: (*name).to_string(), id: s.id, kind: s.kind, negated: false });
            if s.kind == Kind::Negatable {
                for prefix in ["no-", "no"] {
                    out.push(Candidate {
                        name: format!("{prefix}{name}"),
                        id: s.id,
                        kind: Kind::Flag,
                        negated: true,
                    });
                }
            }
        }
    }
    out
}

/// Resolve a spelled option name against the table: exact match first (case
/// insensitively, `Getopt::Long`'s default), then unique prefix (`auto_abbrev`).
/// An ambiguous prefix resolves to nothing, which under `pass_through` means the
/// argument is left alone.
fn resolve<'a>(table: &'a [Candidate], spelled: &str) -> Option<&'a Candidate> {
    if spelled.is_empty() {
        return None;
    }
    let want = spelled.to_ascii_lowercase();
    if let Some(c) = table.iter().find(|c| c.name.eq_ignore_ascii_case(&want)) {
        return Some(c);
    }
    let mut hits = table.iter().filter(|c| c.name.to_ascii_lowercase().starts_with(&want));
    let first = hits.next()?;
    // Distinct aliases of the same option are not an ambiguity in Getopt::Long,
    // but no two names in these tables share a prefix without sharing an id.
    if hits.any(|c| c.id != first.id || c.negated != first.negated) {
        return None;
    }
    Some(first)
}

/// One `GetOptions` pass under `Getopt::Long::Configure qw/ pass_through /`.
///
/// Unknown names, ambiguous abbreviations and required values that are not
/// available all leave the argument in place rather than erroring, which is what
/// `pass_through` does and why `git send-email --bogus` ends up handing `--bogus`
/// to `git format-patch`.
fn getoptions(args: &[String], specs: &'static [Spec]) -> Parsed {
    let table = candidates(specs);
    let mut hits = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // `--` ends the scan. pass_through keeps it, unlike the default mode.
        if arg == "--" {
            rest.extend_from_slice(&args[i..]);
            break;
        }
        // A bare `-` and anything not starting with `-` is a positional; permute
        // means the scan continues past it.
        let body = match arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            Some(b) if !b.is_empty() => b,
            _ => {
                rest.push(arg.clone());
                i += 1;
                continue;
            }
        };

        let (spelled, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (body, None),
        };
        let Some(cand) = resolve(&table, spelled) else {
            rest.push(arg.clone());
            i += 1;
            continue;
        };

        match cand.kind {
            Kind::Flag | Kind::Negatable => {
                hits.push(Hit { id: cand.id, negated: cand.negated, value: inline });
                i += 1;
            }
            Kind::Required => {
                let value = match inline {
                    Some(v) => Some(v),
                    None => {
                        let next = args.get(i + 1).cloned();
                        if next.is_some() {
                            i += 1;
                        }
                        next
                    }
                };
                match value {
                    Some(v) => {
                        hits.push(Hit { id: cand.id, negated: false, value: Some(v) });
                        i += 1;
                    }
                    // Nothing left to consume: pass_through leaves the flag be.
                    None => {
                        rest.push(arg.clone());
                        i += 1;
                    }
                }
            }
            Kind::Optional => {
                let value = match inline {
                    Some(v) => v,
                    None => match args.get(i + 1) {
                        Some(next) if !next.starts_with('-') => {
                            i += 1;
                            next.clone()
                        }
                        _ => String::new(),
                    },
                };
                hits.push(Hit { id: cand.id, negated: false, value: Some(value) });
                i += 1;
            }
        }
    }

    Parsed { hits, rest }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// `%known_config_keys` — every `sende?mail.*` key that exists, lowercased,
/// mapped to its explicit values in file order. A key present with no `=` gets
/// an entry with no values, matching `--get-regexp`'s `key\0` output.
struct Known(BTreeMap<String, Vec<String>>);

impl Known {
    fn last(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.last()).map(String::as_str)
    }

    fn exists(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// `Git::config_bool`. A key with no value is true, as `git config --bool`
    /// reports it; an unparseable value is treated as unset.
    fn boolean(&self, key: &str) -> Option<bool> {
        let values = self.0.get(key)?;
        match values.last() {
            None => Some(true),
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "yes" | "on" | "true" | "1" => Some(true),
                "no" | "off" | "false" | "0" | "" => Some(false),
                _ => None,
            },
        }
    }
}

/// The config git would see: the repository in the current directory when there
/// is one, otherwise the global and system files alone. `send-email` runs
/// happily outside a repository, so both cases matter.
fn load_config() -> (Option<ConfigFile>, bool) {
    match gix::discover(".") {
        Ok(repo) => (Some(repo.config_snapshot().plumbing().clone()), true),
        Err(_) => {
            let file = ConfigFile::from_globals().ok().map(|mut f| {
                if let Ok(env) = ConfigFile::from_environment_overrides() {
                    let _ = f.append(env);
                }
                f
            });
            (file, false)
        }
    }
}

/// Collect every `sendemail.*` and `sendmail.*` key, subsections included.
fn known_keys(cfg: Option<&ConfigFile>) -> Known {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Some(cfg) = cfg else { return Known(map) };

    for section in cfg.sections() {
        let name = section.header().name().to_str_lossy().to_ascii_lowercase();
        if name != "sendemail" && name != "sendmail" {
            continue;
        }
        // Subsection names are case sensitive in git; only the section and the
        // value name are folded.
        let prefix = match section.header().subsection_name() {
            Some(sub) => format!("{name}.{}", sub.to_str_lossy()),
            None => name,
        };
        let body = section.body();
        let value_names: BTreeSet<String> =
            body.value_names().map(|n| n.to_ascii_lowercase()).collect();
        for value_name in value_names {
            let entry = map.entry(format!("{prefix}.{value_name}")).or_default();
            for v in body.values(&value_name) {
                entry.push(v.to_str_lossy().into_owned());
            }
        }
    }
    Known(map)
}

/// Where a `%config_bool_settings` entry lands in [`Settings`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoolTarget {
    Thread,
    ChainReplyTo,
    SuppressFrom,
    SignedOffByCc,
    CoverCc,
    CoverTo,
    Validate,
    MultiEdit,
    Annotate,
    XMailer,
    ForbidSendmailVariables,
    Mailmap,
    OutlookIdFix,
    UseImapOnly,
}

/// `%config_bool_settings`. Each entry is spelled as the configuration variable
/// itself for the unqualified `sendemail` prefix; [`identity_key`] rewrites it
/// for the `sendemail.<identity>` pass.
const BOOL_SETTINGS: &[(&str, BoolTarget)] = &[
    ("sendemail.thread", BoolTarget::Thread),
    ("sendemail.chainreplyto", BoolTarget::ChainReplyTo),
    ("sendemail.suppressfrom", BoolTarget::SuppressFrom),
    ("sendemail.signedoffbycc", BoolTarget::SignedOffByCc),
    ("sendemail.cccover", BoolTarget::CoverCc),
    ("sendemail.tocover", BoolTarget::CoverTo),
    ("sendemail.signedoffcc", BoolTarget::SignedOffByCc),
    ("sendemail.validate", BoolTarget::Validate),
    ("sendemail.multiedit", BoolTarget::MultiEdit),
    ("sendemail.annotate", BoolTarget::Annotate),
    ("sendemail.xmailer", BoolTarget::XMailer),
    ("sendemail.forbidsendmailvariables", BoolTarget::ForbidSendmailVariables),
    ("sendemail.mailmap", BoolTarget::Mailmap),
    ("sendemail.outlookidfix", BoolTarget::OutlookIdFix),
    ("sendemail.useimaponly", BoolTarget::UseImapOnly),
];

/// Where a `%config_path_settings` entry lands. These go through
/// `Git::config_path`, so `~/` is expanded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathTarget {
    AliasesFile,
    SmtpSslCertPath,
    SmtpSslClientCert,
    SmtpSslClientKey,
    MailmapFile,
    MailmapBlob,
}

/// `%config_path_settings`. `aliasesfile` is the only list-valued one.
const PATH_SETTINGS: &[(&str, PathTarget)] = &[
    ("sendemail.aliasesfile", PathTarget::AliasesFile),
    ("sendemail.smtpsslcertpath", PathTarget::SmtpSslCertPath),
    ("sendemail.smtpsslclientcert", PathTarget::SmtpSslClientCert),
    ("sendemail.smtpsslclientkey", PathTarget::SmtpSslClientKey),
    ("sendemail.mailmap.file", PathTarget::MailmapFile),
    ("sendemail.mailmap.blob", PathTarget::MailmapBlob),
];

/// Where a `%config_settings` entry lands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueTarget {
    SmtpEncryption,
    SmtpServer,
    SmtpServerPort,
    SmtpServerOption,
    SmtpUser,
    SmtpPass,
    SmtpDomain,
    SmtpAuth,
    BatchSize,
    ReloginDelay,
    ImapSentFolder,
    To,
    ToCmd,
    Cc,
    CcCmd,
    HeaderCmd,
    AliasFileType,
    Bcc,
    SuppressCc,
    EnvelopeSender,
    Confirm,
    From,
    Assume8BitEncoding,
    ComposeEncoding,
    TransferEncoding,
    SendmailCmd,
}

impl ValueTarget {
    /// The `ref($target) eq "ARRAY"` test: a list-valued setting takes every
    /// value the key has, a scalar one only the last.
    fn is_list(self) -> bool {
        matches!(
            self,
            ValueTarget::SmtpServerOption
                | ValueTarget::To
                | ValueTarget::Cc
                | ValueTarget::Bcc
                | ValueTarget::SuppressCc
        )
    }
}

/// `%config_settings`.
const VALUE_SETTINGS: &[(&str, ValueTarget)] = &[
    ("sendemail.smtpencryption", ValueTarget::SmtpEncryption),
    ("sendemail.smtpserver", ValueTarget::SmtpServer),
    ("sendemail.smtpserverport", ValueTarget::SmtpServerPort),
    ("sendemail.smtpserveroption", ValueTarget::SmtpServerOption),
    ("sendemail.smtpuser", ValueTarget::SmtpUser),
    ("sendemail.smtppass", ValueTarget::SmtpPass),
    ("sendemail.smtpdomain", ValueTarget::SmtpDomain),
    ("sendemail.smtpauth", ValueTarget::SmtpAuth),
    ("sendemail.smtpbatchsize", ValueTarget::BatchSize),
    ("sendemail.smtprelogindelay", ValueTarget::ReloginDelay),
    ("sendemail.imapsentfolder", ValueTarget::ImapSentFolder),
    ("sendemail.to", ValueTarget::To),
    ("sendemail.tocmd", ValueTarget::ToCmd),
    ("sendemail.cc", ValueTarget::Cc),
    ("sendemail.cccmd", ValueTarget::CcCmd),
    ("sendemail.headercmd", ValueTarget::HeaderCmd),
    ("sendemail.aliasfiletype", ValueTarget::AliasFileType),
    ("sendemail.bcc", ValueTarget::Bcc),
    ("sendemail.suppresscc", ValueTarget::SuppressCc),
    ("sendemail.envelopesender", ValueTarget::EnvelopeSender),
    ("sendemail.confirm", ValueTarget::Confirm),
    ("sendemail.from", ValueTarget::From),
    ("sendemail.assume8bitencoding", ValueTarget::Assume8BitEncoding),
    ("sendemail.composeencoding", ValueTarget::ComposeEncoding),
    ("sendemail.transferencoding", ValueTarget::TransferEncoding),
    ("sendemail.sendmailcmd", ValueTarget::SendmailCmd),
];

/// Rewrite a `sendemail.<name>` literal for the `sendemail.<identity>` pass,
/// which `read_config` runs first so that an identity subsection wins.
fn identity_key(literal: &str, identity: &str) -> String {
    format!("sendemail.{identity}.{}", &literal["sendemail.".len()..])
}

/// The script's configuration variables, with the same defaults the `my`
/// declarations give them. `Option` stands for Perl's `undef`, which several of
/// these are tested for rather than used.
struct Settings {
    thread: bool,
    chain_reply_to: bool,
    suppress_from: Option<bool>,
    signed_off_by_cc: Option<bool>,
    cover_cc: Option<bool>,
    cover_to: Option<bool>,
    validate: bool,
    multiedit: Option<bool>,
    annotate: Option<bool>,
    use_xmailer: bool,
    forbid_sendmail_variables: bool,
    mailmap: bool,
    use_imap_only: bool,

    alias_files: Vec<String>,
    mailmap_file: Option<String>,
    mailmap_blob: Option<String>,

    smtp_server: Option<String>,
    smtp_server_port: Option<String>,
    smtp_server_options: Vec<String>,
    batch_size: Option<String>,
    relogin_delay: Option<String>,
    imap_sent_folder: Option<String>,
    config_to: Vec<String>,
    to_cmd: Option<String>,
    config_cc: Vec<String>,
    cc_cmd: Option<String>,
    header_cmd: Option<String>,
    alias_file_type: Option<String>,
    config_bcc: Vec<String>,
    suppress_cc: Vec<String>,
    envelope_sender: Option<String>,
    confirm: Option<String>,
    sender: Option<String>,
    auto_8bit_encoding: Option<String>,
    /// `sendemail.composeEncoding` / `--compose-encoding`. Read by the
    /// `--compose` block: it names the `charset=` of the `Content-Type:` header
    /// the composed message gets when it holds a non-ASCII byte, and is the
    /// charset label `quote_subject` puts in the encoded word for its
    /// `Subject:`. `undef` there becomes `UTF-8`.
    compose_encoding: Option<String>,
    target_xfer_encoding: String,
    sendmail_cmd: Option<String>,

    /// The settings that steer the `Net::SMTP` conversation and nothing else.
    smtp: Smtp,
}

/// The `Net::SMTP` half of the setting tables: the variables that only
/// [`super::smtp`] reads — `Net::SMTP->new`, `ssl_verify_params`,
/// `smtp_auth_maybe` and `is_outlook`.
#[derive(Default)]
struct Smtp {
    /// `$smtp_encryption` — `ssl`, `tls`, or anything else for a session in the
    /// clear. `$smtp_encryption = '' unless defined` runs before it is used.
    encryption: Option<String>,
    /// `$smtp_authuser`.
    authuser: Option<String>,
    /// `$smtp_authpass`.
    authpass: Option<String>,
    /// `$smtp_domain` — what `EHLO` announces. `maildomain()` fills it in when
    /// nothing else does.
    domain: Option<String>,
    /// `$smtp_auth` — the allowed SASL mechanisms, or `none`.
    auth: Option<String>,
    /// `$smtp_ssl_cert_path`, `$smtp_ssl_client_cert` and
    /// `$smtp_ssl_client_key`, which are `ssl_verify_params()`'s whole input.
    ssl: super::smtp::Ssl,
    /// `$outlook_id_fix`. `None` is the script's `'auto'`, which `is_outlook`
    /// resolves against the server name on first use.
    outlook_id_fix: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            thread: true,
            chain_reply_to: false,
            suppress_from: None,
            signed_off_by_cc: None,
            cover_cc: None,
            cover_to: None,
            validate: true,
            multiedit: None,
            annotate: None,
            use_xmailer: true,
            forbid_sendmail_variables: true,
            mailmap: false,
            use_imap_only: false,
            alias_files: Vec::new(),
            mailmap_file: None,
            mailmap_blob: None,
            smtp_server: None,
            smtp_server_port: None,
            smtp_server_options: Vec::new(),
            batch_size: None,
            relogin_delay: None,
            imap_sent_folder: None,
            config_to: Vec::new(),
            to_cmd: None,
            config_cc: Vec::new(),
            cc_cmd: None,
            header_cmd: None,
            alias_file_type: None,
            config_bcc: Vec::new(),
            suppress_cc: Vec::new(),
            envelope_sender: None,
            confirm: None,
            sender: None,
            auto_8bit_encoding: None,
            compose_encoding: None,
            target_xfer_encoding: "auto".into(),
            sendmail_cmd: None,
            smtp: Smtp::default(),
        }
    }
}

impl Settings {
    fn set_bool(&mut self, target: BoolTarget, v: bool) {
        match target {
            BoolTarget::Thread => self.thread = v,
            BoolTarget::ChainReplyTo => self.chain_reply_to = v,
            BoolTarget::SuppressFrom => self.suppress_from = Some(v),
            BoolTarget::SignedOffByCc => self.signed_off_by_cc = Some(v),
            BoolTarget::CoverCc => self.cover_cc = Some(v),
            BoolTarget::CoverTo => self.cover_to = Some(v),
            BoolTarget::Validate => self.validate = v,
            BoolTarget::MultiEdit => self.multiedit = Some(v),
            BoolTarget::Annotate => self.annotate = Some(v),
            BoolTarget::XMailer => self.use_xmailer = v,
            BoolTarget::ForbidSendmailVariables => self.forbid_sendmail_variables = v,
            BoolTarget::Mailmap => self.mailmap = v,
            BoolTarget::OutlookIdFix => self.smtp.outlook_id_fix = Some(v),
            BoolTarget::UseImapOnly => self.use_imap_only = v,
        }
    }

    fn set_path(&mut self, target: PathTarget, values: &[String]) {
        let last = || values.last().cloned();
        match target {
            PathTarget::AliasesFile => self.alias_files = values.to_vec(),
            PathTarget::SmtpSslCertPath => self.smtp.ssl.cert_path = last(),
            PathTarget::SmtpSslClientCert => self.smtp.ssl.client_cert = last(),
            PathTarget::SmtpSslClientKey => self.smtp.ssl.client_key = last(),
            PathTarget::MailmapFile => self.mailmap_file = last(),
            PathTarget::MailmapBlob => self.mailmap_blob = last(),
        }
    }

    fn set_value(&mut self, target: ValueTarget, values: &[String]) {
        let last = || values.last().cloned();
        match target {
            ValueTarget::SmtpEncryption => self.smtp.encryption = last(),
            ValueTarget::SmtpUser => self.smtp.authuser = last(),
            ValueTarget::SmtpPass => self.smtp.authpass = last(),
            ValueTarget::SmtpDomain => self.smtp.domain = last(),
            ValueTarget::SmtpAuth => self.smtp.auth = last(),
            ValueTarget::SmtpServer => self.smtp_server = last(),
            ValueTarget::SmtpServerPort => self.smtp_server_port = last(),
            ValueTarget::SmtpServerOption => self.smtp_server_options = values.to_vec(),
            ValueTarget::BatchSize => self.batch_size = last(),
            ValueTarget::ReloginDelay => self.relogin_delay = last(),
            ValueTarget::ImapSentFolder => self.imap_sent_folder = last(),
            ValueTarget::To => self.config_to = values.to_vec(),
            ValueTarget::ToCmd => self.to_cmd = last(),
            ValueTarget::Cc => self.config_cc = values.to_vec(),
            ValueTarget::CcCmd => self.cc_cmd = last(),
            ValueTarget::HeaderCmd => self.header_cmd = last(),
            ValueTarget::AliasFileType => self.alias_file_type = last(),
            ValueTarget::Bcc => self.config_bcc = values.to_vec(),
            ValueTarget::SuppressCc => self.suppress_cc = values.to_vec(),
            ValueTarget::EnvelopeSender => self.envelope_sender = last(),
            ValueTarget::Confirm => self.confirm = last(),
            ValueTarget::From => self.sender = last(),
            ValueTarget::Assume8BitEncoding => self.auto_8bit_encoding = last(),
            ValueTarget::ComposeEncoding => self.compose_encoding = last(),
            ValueTarget::TransferEncoding => {
                if let Some(v) = last() {
                    self.target_xfer_encoding = v;
                }
            }
            ValueTarget::SendmailCmd => self.sendmail_cmd = last(),
        }
    }
}

/// `read_config(\%known_config_keys, \%configured, $prefix)`, called for
/// `sendemail.<identity>` and then `sendemail`. `%configured` is keyed by the
/// *setting* name, which is what makes the first prefix win — and what lets
/// `signedoffbycc` and `signedoffcc` both feed one variable.
fn read_config(
    s: &mut Settings,
    known: &Known,
    identity: Option<&str>,
    configured: &mut BTreeSet<&'static str>,
) {
    let key = |literal: &str| match identity {
        Some(id) => identity_key(literal, id),
        None => literal.to_string(),
    };
    // Every literal below is `sendemail.<setting>`; the setting name is what
    // `%configured` records.
    let setting_of = |literal: &'static str| &literal["sendemail.".len()..];

    for (literal, target) in BOOL_SETTINGS {
        let k = key(literal);
        if !known.exists(&k) {
            continue;
        }
        let Some(v) = known.boolean(&k) else { continue };
        if configured.insert(setting_of(literal)) {
            s.set_bool(*target, v);
        }
    }

    for (literal, target) in PATH_SETTINGS {
        let k = key(literal);
        let Some(values) = known.0.get(&k) else { continue };
        let values: Vec<String> = values.iter().map(|v| expand_path(v)).collect();
        // `next unless @values` for the array target, `next unless defined $v`
        // for a scalar: a key present with no value yields neither.
        if values.is_empty() {
            continue;
        }
        if configured.insert(setting_of(literal)) {
            s.set_path(*target, &values);
        }
    }

    for (literal, target) in VALUE_SETTINGS {
        let k = key(literal);
        let Some(values) = known.0.get(&k) else { continue };
        // A scalar takes `->[-1]` and skips when that is undef; a list takes
        // every defined value and is assigned even when the result is empty.
        if !target.is_list() && values.is_empty() {
            continue;
        }
        if configured.insert(setting_of(literal)) {
            s.set_value(*target, values);
        }
    }
}

/// `Git::config_path`, reduced to the `~/` case. `%(prefix)` and `~user/` are
/// left verbatim; see the module docs.
fn expand_path(value: &str) -> String {
    let Some(tail) = value.strip_prefix("~/") else { return value.to_string() };
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            let mut path = std::path::PathBuf::from(home);
            path.push(tail);
            path.to_string_lossy().into_owned()
        }
        _ => value.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Alias files
// ---------------------------------------------------------------------------

/// Perl's `\s` on a chomped line: ASCII whitespace.
fn is_ws(c: u8) -> bool {
    c.is_ascii_whitespace()
}

/// Advance past a run of whitespace, returning the new offset.
fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && is_ws(s[i]) {
        i += 1;
    }
    i
}

/// Advance past a run of non-whitespace, returning the new offset.
fn skip_non_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && !is_ws(s[i]) {
        i += 1;
    }
    i
}

/// `/^\s*alias\s+(?:-group\s+\S+\s+)*(\S+)\s+(.*)$/` — the mutt parser. Returns
/// the alias name and its addresses: `$2` with a `#` comment stripped, split on
/// commas by `split_addrs`, with `\"` unescaped so it is not escaped twice.
fn mutt_alias(line: &[u8]) -> Option<(String, Vec<String>)> {
    let mut i = skip_ws(line, 0);
    if !line[i..].starts_with(b"alias") {
        return None;
    }
    i += 5;
    let after = skip_ws(line, i);
    if after == i {
        return None;
    }
    i = after;

    // The `(?:-group …)*` group is greedy; record each position it could stop at
    // and try them from the longest match back, as backtracking would.
    let mut stops = vec![i];
    let mut j = i;
    while line[j..].starts_with(b"-group") {
        let mut k = j + 6;
        let ws = skip_ws(line, k);
        if ws == k {
            break;
        }
        k = skip_non_ws(line, ws);
        if k == ws {
            break;
        }
        let ws2 = skip_ws(line, k);
        if ws2 == k {
            break;
        }
        j = ws2;
        stops.push(j);
    }

    for &start in stops.iter().rev() {
        let end = skip_non_ws(line, start);
        if end == start {
            continue;
        }
        // `\s+` then `(.*)$`, which may be empty — a trailing newline satisfies
        // the `\s+`, so `alias bob\n` does define `bob`.
        let value_start = skip_ws(line, end);
        if value_start == end {
            continue;
        }
        // The greedy `\s+` swallows the newline when only whitespace follows,
        // leaving `(.*)` empty; otherwise `(.*)` runs to just before it.
        let value = if value_start >= line.len() {
            String::new()
        } else {
            String::from_utf8_lossy(&line[value_start..line.len() - 1]).into_owned()
        };
        // `$addr =~ s/#.*$//` — mutt allows `#` comments.
        let value = match value.find('#') {
            Some(i) => value[..i].to_string(),
            None => value,
        };
        let addrs = split_addrs(&value).into_iter().map(|a| a.replace("\\\"", "\"")).collect();
        return Some((String::from_utf8_lossy(&line[start..end]).into_owned(), addrs));
    }
    None
}

/// `/^alias\s+(\S+)\s+(.*?)\s*$/` — the mailrc parser. Its addresses are
/// whitespace separated (`quotewords('\s+', 0, $2)`), not comma separated.
fn mailrc_alias(line: &[u8]) -> Option<(String, Vec<String>)> {
    if !line.starts_with(b"alias") {
        return None;
    }
    let mut i = 5;
    let after = skip_ws(line, i);
    if after == i {
        return None;
    }
    i = after;
    let end = skip_non_ws(line, i);
    if end == i {
        return None;
    }
    let value_start = skip_ws(line, end);
    if value_start == end {
        return None;
    }
    // The lazy `(.*?)` with a trailing `\s*$` is exactly the value with its
    // surrounding whitespace (the newline included) trimmed off.
    let value = String::from_utf8_lossy(&line[value_start..]).into_owned();
    let addrs = parse_words(Delim::Space, false, value.trim_end_matches(|c: char| c.is_ascii_whitespace()));
    Some((String::from_utf8_lossy(&line[i..end]).into_owned(), addrs))
}

/// `/^(\S+)\s+=\s+[^=]+=\s(\S+)/` — the elm parser. `$2` is a single
/// whitespace-free run, still split on commas by `split_addrs`.
fn elm_alias(line: &[u8]) -> Option<(String, Vec<String>)> {
    let end = skip_non_ws(line, 0);
    if end == 0 {
        return None;
    }
    let mut i = skip_ws(line, end);
    if i == end || line.get(i) != Some(&b'=') {
        return None;
    }
    i += 1;
    let after = skip_ws(line, i);
    if after == i {
        return None;
    }
    i = after;
    // `[^=]+` cannot cross an `=`, so the next `=` is the only candidate.
    let eq = i + line[i..].iter().position(|&c| c == b'=')?;
    if eq == i {
        return None;
    }
    let ws = eq + 1;
    if !line.get(ws).is_some_and(|&c| is_ws(c)) {
        return None;
    }
    if !line.get(ws + 1).is_some_and(|&c| !is_ws(c)) {
        return None;
    }
    let value = String::from_utf8_lossy(&line[ws + 1..skip_non_ws(line, ws + 1)]).into_owned();
    Some((String::from_utf8_lossy(&line[..end]).into_owned(), split_addrs(&value)))
}

/// `/\(define-mail-alias\s+"(\S+?)"\s+"(\S+?)"\)/` — the gnus parser. The
/// pattern is unanchored, so it is searched for anywhere in the line. `$2` is
/// stored as the single address, without any comma splitting.
fn gnus_alias(line: &[u8]) -> Option<(String, Vec<String>)> {
    let needle = b"(define-mail-alias";
    let mut from = 0;
    while let Some(off) = line[from..].windows(needle.len()).position(|w| w == needle) {
        let start = from + off;
        if let Some(name) = gnus_at(line, start + needle.len()) {
            return Some(name);
        }
        from = start + 1;
    }
    None
}

/// The tail of the gnus pattern, starting just past `(define-mail-alias`.
fn gnus_at(line: &[u8], mut i: usize) -> Option<(String, Vec<String>)> {
    let after = skip_ws(line, i);
    if after == i {
        return None;
    }
    i = after;
    let (name, next) = quoted_word(line, i)?;
    i = next;
    let after = skip_ws(line, i);
    if after == i {
        return None;
    }
    i = after;
    let (addr, next) = quoted_word(line, i)?;
    if line.get(next) != Some(&b')') {
        return None;
    }
    Some((name, vec![addr]))
}

/// `"(\S+?)"` — a double-quoted run of at least one non-whitespace character.
/// The non-greedy `\S+?` stops at the first quote that lets the rest match, so
/// the content is simply everything up to the next quote.
fn quoted_word(line: &[u8], i: usize) -> Option<(String, usize)> {
    if line.get(i) != Some(&b'"') {
        return None;
    }
    let start = i + 1;
    let end = start + line[start..].iter().position(|&c| c == b'"')?;
    if end == start || line[start..end].iter().any(|&c| is_ws(c)) {
        return None;
    }
    Some((String::from_utf8_lossy(&line[start..end]).into_owned(), end + 1))
}

/// `parse_sendmail_alias` — one logical (continuation-joined) line.
fn sendmail_alias(logical: &[u8], aliases: &mut Aliases) {
    let text = String::from_utf8_lossy(logical);
    if text.contains('"') {
        eprintln!("warning: sendmail alias with quotes is not supported: {text}");
    } else if text.contains(":include:") {
        eprintln!("warning: `:include:` not supported: {text}");
    } else if text.contains('/') || text.contains('|') {
        eprintln!("warning: `/file` or `|pipe` redirection not supported: {text}");
    } else if let Some((name, addrs)) = sendmail_name(logical) {
        aliases.insert(name, addrs);
    } else {
        eprintln!("warning: sendmail line is not recognized: {text}");
    }
}

/// `/^(\S+?)\s*:\s*(.+)$/`. `\S+?` is non-greedy, so the shortest prefix that
/// lands on a colon with a non-empty remainder wins; `$2` runs to end of line
/// and is split on commas.
fn sendmail_name(line: &[u8]) -> Option<(String, Vec<String>)> {
    for len in 1..=line.len() {
        if is_ws(line[len - 1]) {
            return None;
        }
        let i = skip_ws(line, len);
        if line.get(i) != Some(&b':') {
            continue;
        }
        let rest = skip_ws(line, i + 1);
        if rest >= line.len() {
            continue;
        }
        let value = String::from_utf8_lossy(&line[rest..]).into_owned();
        return Some((
            String::from_utf8_lossy(&line[..len]).into_owned(),
            split_addrs(&value),
        ));
    }
    None
}

/// `parse_sendmail_aliases` — blank and `#` lines are dropped, a trailing `\`
/// or a leading blank on the next line continues the current one.
fn parse_sendmail(text: &[u8], aliases: &mut Aliases) {
    let mut acc: Vec<u8> = Vec::new();
    for line in chomped_lines(text) {
        if line.iter().all(|&c| is_ws(c)) {
            continue;
        }
        let trimmed = skip_ws(&line, 0);
        if line.get(trimmed) == Some(&b'#') {
            continue;
        }
        // `$s =~ s/\\$//` is tried first; only if it fails is the leading
        // whitespace of the new line stripped, and only then does that count as
        // a continuation.
        if acc.last() == Some(&b'\\') {
            acc.pop();
            acc.extend_from_slice(&line);
            continue;
        }
        if trimmed > 0 {
            acc.extend_from_slice(&line[trimmed..]);
            continue;
        }
        if !acc.is_empty() {
            sendmail_alias(&acc, aliases);
        }
        acc = line;
    }
    if acc.last() == Some(&b'\\') {
        acc.pop();
    }
    if !acc.is_empty() {
        sendmail_alias(&acc, aliases);
    }
}

/// The pine parser, whose record is a tab-delimited line plus any following
/// lines that begin with a space. The record matches when it has three to five
/// tab-separated fields, the first has no whitespace and the third is non-empty;
/// the alias is the first field and its addresses are the third, with the
/// optional parentheses of `\(?([^\t]+?)\)?` peeled off, split on commas.
fn parse_pine(text: &[u8], aliases: &mut Aliases) {
    let lines = chomped_lines(text);
    let mut i = 0;
    while i < lines.len() {
        let mut record = lines[i].clone();
        i += 1;
        while i < lines.len() && lines[i].first() == Some(&b' ') {
            let cont = &lines[i];
            let start = cont.iter().position(|&c| c != b' ').unwrap_or(cont.len());
            record.extend_from_slice(&cont[start..]);
            i += 1;
        }
        let fields: Vec<&[u8]> = record.split(|&c| c == b'\t').collect();
        if !(3..=5).contains(&fields.len()) {
            continue;
        }
        if fields[0].is_empty() || fields[0].iter().any(|&c| is_ws(c)) || fields[2].is_empty() {
            continue;
        }
        // `\(?` is greedy, and the lazy `([^\t]+?)` then stops one byte short of
        // a closing `)` because `\)?` can absorb it — but neither may leave the
        // capture empty, which is what the `+` demands.
        let mut value = fields[2];
        if value.first() == Some(&b'(') && value.len() > 1 {
            value = &value[1..];
        }
        if value.last() == Some(&b')') && value.len() > 1 {
            value = &value[..value.len() - 1];
        }
        let value = String::from_utf8_lossy(value).into_owned();
        aliases.insert(
            String::from_utf8_lossy(fields[0]).into_owned(),
            split_addrs(&value),
        );
    }
}

/// Split into lines with the terminator removed, as Perl's `<$fh>` plus `chomp`
/// does. A final line without a newline is still a line; a trailing newline does
/// not manufacture an empty one. Blank lines in the middle are preserved.
fn chomped_lines(text: &[u8]) -> Vec<Vec<u8>> {
    if text.is_empty() {
        return Vec::new();
    }
    // `chomp` removes `$/`, which is "\n" — a CR before it survives, as it does
    // in Perl.
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    body.split(|&c| c == b'\n').map(<[u8]>::to_vec).collect()
}

/// The line-oriented parsers see `$_` with its newline still attached, because
/// they never `chomp`. That matters: `\s+` in their patterns can be satisfied by
/// the newline alone.
fn parse_line_oriented(
    text: &[u8],
    aliases: &mut Aliases,
    f: fn(&[u8]) -> Option<(String, Vec<String>)>,
) {
    for mut line in chomped_lines(text) {
        line.push(b'\n');
        if let Some((name, addrs)) = f(&line) {
            aliases.insert(name, addrs);
        }
    }
}

/// `%aliases` — each alias name mapped to the address list it expands to. A
/// later definition replaces an earlier one, as assigning to a Perl hash does,
/// and the key order is the byte order `sort keys %aliases` produces.
type Aliases = BTreeMap<String, Vec<String>>;

/// The result of the alias-file scan: either `%aliases`, or the exit code of a
/// `die` raised while opening one of the files.
enum AliasScan {
    Parsed(Aliases),
    Died(u8),
}

/// `%parse_alias` — dispatch on `sendemail.aliasfiletype`. An unset or unknown
/// type leaves the files unread, as the
/// `if (@alias_files and $aliasfiletype and defined $parse_alias{$aliasfiletype})`
/// guard in the script does, and `%aliases` stays empty.
fn parse_aliases(cfg: &Settings) -> AliasScan {
    let mut aliases = Aliases::new();
    let Some(file_type) = cfg.alias_file_type.as_deref() else {
        return AliasScan::Parsed(aliases);
    };
    if cfg.alias_files.is_empty() {
        return AliasScan::Parsed(aliases);
    }
    if !matches!(file_type, "mutt" | "mailrc" | "pine" | "elm" | "sendmail" | "gnus") {
        return AliasScan::Parsed(aliases);
    }

    for file in &cfg.alias_files {
        let text = match std::fs::read(file) {
            Ok(text) => text,
            Err(e) => {
                // Perl: `die "opening $file: $!\n"`, and `die` exits with `$!`.
                eprintln!("opening {file}: {}", errno_text(&e));
                let code = u8::try_from(e.raw_os_error().unwrap_or(255)).unwrap_or(255);
                return AliasScan::Died(code);
            }
        };
        match file_type {
            "mutt" => parse_line_oriented(&text, &mut aliases, mutt_alias),
            "mailrc" => parse_line_oriented(&text, &mut aliases, mailrc_alias),
            "elm" => parse_line_oriented(&text, &mut aliases, elm_alias),
            "gnus" => parse_line_oriented(&text, &mut aliases, gnus_alias),
            "sendmail" => parse_sendmail(&text, &mut aliases),
            "pine" => parse_pine(&text, &mut aliases),
            _ => unreachable!("filtered above"),
        }
    }
    AliasScan::Parsed(aliases)
}

// ---------------------------------------------------------------------------
// Address lists: Text::ParseWords, Mail::Address and sanitize_address
// ---------------------------------------------------------------------------

/// The field delimiter `Text::ParseWords::parse_line` is handed, in the two
/// spellings `git-send-email.perl` uses.
#[derive(Clone, Copy)]
enum Delim {
    /// `'\s*,\s*'` — `split_addrs`, for a comma-separated alias value.
    Comma,
    /// `'\s+'` — the mailrc parser, whose addresses are whitespace separated.
    Space,
}

impl Delim {
    /// The length of the delimiter's match starting exactly at `at`, or `None`
    /// when it does not match there. Both spellings are greedy, so the surrounding
    /// whitespace is part of the delimiter and never part of a field.
    fn at(self, s: &[u8], at: usize) -> Option<usize> {
        let ws = skip_ws(s, at);
        match self {
            Delim::Comma if s.get(ws) == Some(&b',') => Some(skip_ws(s, ws + 1) - at),
            Delim::Space if ws > at => Some(ws - at),
            _ => None,
        }
    }
}

/// `split_addrs` — `quotewords('\s*,\s*', 1, $addr)`. `keep` is on, so quoted
/// sections come back with their quotes and escapes intact.
fn split_addrs(value: &str) -> Vec<String> {
    parse_words(Delim::Comma, true, value)
}

/// `Text::ParseWords::parse_line`, transcribed from its single alternation:
/// a double- or single-quoted section (escapes with `\`), or a lazy unquoted run
/// terminated by end of string, by the delimiter, or by a quote that opens the
/// next section. With `keep` the quotes and escapes survive; without it, `\x`
/// collapses to `x` in unquoted text and in double-quoted sections.
///
/// A run that cannot be tokenised at all — an unterminated quote — makes Perl's
/// `parse_line` return the empty list, and `quotewords` then discards whatever it
/// had; that is what the empty vector here means. Perl's trailing `undef` piece
/// (produced when the string ends on a delimiter) becomes an empty string, which
/// expands and prints identically.
fn parse_words(delim: Delim, keep: bool, line: &str) -> Vec<String> {
    let s = line.as_bytes();
    let mut pieces: Vec<String> = Vec::new();
    let mut word: Option<String> = None;
    let mut i = 0;

    while i < s.len() {
        let (text, delim_len) = if s[i] == b'"' || s[i] == b'\'' {
            let quote = s[i];
            let Some(end) = quoted_section(s, i, quote) else { return Vec::new() };
            let inner = String::from_utf8_lossy(&s[i + 1..end]).into_owned();
            let text = if keep {
                format!("{q}{inner}{q}", q = quote as char)
            } else if quote == b'"' {
                unescape(&inner)
            } else {
                inner
            };
            i = end + 1;
            (text, 0)
        } else {
            let start = i;
            let mut p = i;
            let delim_len = loop {
                if p == s.len() {
                    break 0;
                }
                if let Some(n) = delim.at(s, p) {
                    break n;
                }
                // `(?!^)(?=["'])`: a quote ends the unquoted run, but never at
                // the very start of what is left of the line.
                if p > start && (s[p] == b'"' || s[p] == b'\'') {
                    break 0;
                }
                match s[p] {
                    b'\\' if p + 1 < s.len() => p += 2,
                    b'\\' | b'"' | b'\'' => return Vec::new(),
                    _ => p += 1,
                }
            };
            let raw = String::from_utf8_lossy(&s[start..p]).into_owned();
            i = p + delim_len;
            (if keep { raw } else { unescape(&raw) }, delim_len)
        };

        word.get_or_insert_with(String::new).push_str(&text);
        if delim_len > 0 {
            pieces.push(word.take().unwrap_or_default());
        }
        if i >= s.len() {
            pieces.push(word.take().unwrap_or_default());
        }
    }
    pieces
}

/// The end offset of the closing quote of the section that starts at `at`, per
/// `(?>[^\\<q>]*(?:\\.[^\\<q>]*)*)<q>`, or `None` when it is never closed.
fn quoted_section(s: &[u8], at: usize, quote: u8) -> Option<usize> {
    let mut j = at + 1;
    loop {
        while j < s.len() && s[j] != quote && s[j] != b'\\' {
            j += 1;
        }
        match s.get(j) {
            Some(&b'\\') if j + 1 < s.len() => j += 2,
            Some(&c) if c == quote => return Some(j),
            _ => return None,
        }
    }
}

/// Perl's `s/\\(.)/$1/sg`: drop the backslash of every escape pair.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            out.push(b[i + 1]);
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `parse_address_line` — `map { $_->format } Mail::Address->parse($line)`.
fn parse_address_line(line: &str) -> Vec<String> {
    mail_address_parse(line).iter().map(format_address).collect()
}

/// One `Mail::Address` object: `[phrase, address, comment]`.
struct MailAddress {
    phrase: String,
    address: String,
    comment: String,
}

/// `Mail::Address::_tokenise`. Every token is either a parenthesised comment, a
/// quoted string or bracketed domain literal (delimiters included), an atom, or
/// one of RFC 822's single-character specials; a trailing `,` is appended so the
/// last address is completed by the same code path as the rest.
fn mail_tokenise(line: &str) -> Vec<String> {
    // `s/\A\s+//` then `s/[\r\n]+/ /g`.
    let collapsed: Vec<u8> = {
        let trimmed = line.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let mut out: Vec<u8> = Vec::with_capacity(trimmed.len());
        let mut in_break = false;
        for &b in trimmed.as_bytes() {
            if b == b'\r' || b == b'\n' {
                if !in_break {
                    out.push(b' ');
                }
                in_break = true;
            } else {
                out.push(b);
                in_break = false;
            }
        }
        out
    };
    let s = &collapsed[..];

    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        // `s/^\s*\(/(/` — a comment, with any whitespace ahead of it dropped.
        let after_ws = skip_ws(s, i);
        if s.get(after_ws) == Some(&b'(') {
            let (field, next) = mail_comment(s, after_ws);
            words.push(field);
            i = next;
            continue;
        }
        match s[i] {
            b'"' | b'[' => {
                let close = if s[i] == b'"' { b'"' } else { b']' };
                // `"(?:[^"\\]+|\\.)*"` / `\[(?:[^\]\\]+|\\.)*\]`, which keep
                // their delimiters in the token.
                match quoted_section(s, i, close) {
                    Some(end) => {
                        words.push(String::from_utf8_lossy(&s[i..=end]).into_owned());
                        i = skip_ws(s, end + 1);
                        continue;
                    }
                    // Unterminated: the alternation falls through to the
                    // single-special branch, which matches the delimiter alone.
                    None => {}
                }
                words.push(String::from_utf8_lossy(&s[i..=i]).into_owned());
                i = skip_ws(s, i + 1);
            }
            b if is_special(b) => {
                words.push(String::from_utf8_lossy(&s[i..=i]).into_owned());
                i = skip_ws(s, i + 1);
            }
            b if is_ws(b) => i = skip_ws(s, i),
            _ => {
                let end = i + s[i..]
                    .iter()
                    .position(|&b| is_ws(b) || is_special(b))
                    .unwrap_or(s.len() - i);
                words.push(String::from_utf8_lossy(&s[i..end]).into_owned());
                i = skip_ws(s, end);
            }
        }
    }
    words.push(",".into());
    words
}

/// The `[^\s()<>\@,;:\\".[\]]` complement: RFC 822's specials, which are each
/// their own token.
fn is_special(b: u8) -> bool {
    b"()<>@,;:\\\".[]".contains(&b)
}

/// The nested-comment scanner of `_tokenise`'s `PAREN` loop, returning the token
/// (with its trailing whitespace trimmed) and the offset just past it.
fn mail_comment(s: &[u8], mut i: usize) -> (String, usize) {
    // A run of `([^()\\]|\\.)*` — text that is neither a parenthesis nor an
    // unfinished escape.
    let run = |s: &[u8], mut j: usize| -> usize {
        while j < s.len() {
            match s[j] {
                b'\\' if j + 1 < s.len() => j += 2,
                b'\\' | b'(' | b')' => break,
                _ => j += 1,
            }
        }
        j
    };

    let mut field: Vec<u8> = Vec::new();
    let mut depth = 0usize;
    'paren: while s.get(i) == Some(&b'(') {
        let start = i;
        i = run(s, i + 1);
        field.extend_from_slice(&s[start..i]);
        depth += 1;
        loop {
            let end = run(s, i);
            if s.get(end) != Some(&b')') {
                break;
            }
            let after = skip_ws(s, end + 1);
            field.extend_from_slice(&s[i..after]);
            i = after;
            depth -= 1;
            if depth == 0 {
                break 'paren;
            }
            let more = run(s, i);
            if more > i {
                field.extend_from_slice(&s[i..more]);
                i = more;
            }
        }
    }
    while field.last().is_some_and(|&b| is_ws(b)) {
        field.pop();
    }
    (String::from_utf8_lossy(&field).into_owned(), i)
}

/// `Mail::Address::parse` over the token stream: `<`/`>` switch into and out of
/// the address, a `,` or `;` completes the address being built, and outside the
/// angle brackets a token joins the phrase when one is coming and the address
/// otherwise.
fn mail_address_parse(line: &str) -> Vec<MailAddress> {
    let tokens = mail_tokenise(line);
    let mut phrase: Vec<String> = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    let mut address: Vec<String> = Vec::new();
    let mut objs: Vec<MailAddress> = Vec::new();
    let mut depth = 0usize;
    let mut next = find_next(0, &tokens);

    // `_complete`: emit an object unless all three parts are empty.
    fn complete(
        phrase: &mut Vec<String>,
        address: &mut Vec<String>,
        comment: &mut Vec<String>,
        objs: &mut Vec<MailAddress>,
    ) {
        if !(phrase.is_empty() && address.is_empty() && comment.is_empty()) {
            objs.push(MailAddress {
                phrase: phrase.join(" "),
                address: address.concat(),
                comment: comment.join(" "),
            });
        }
        phrase.clear();
        address.clear();
        comment.clear();
    }

    for idx in 0..tokens.len() {
        let t = tokens[idx].as_str();
        if t.starts_with('(') {
            comment.push(t.into());
        } else if t == "<" {
            depth += 1;
        } else if t == ">" {
            depth = depth.saturating_sub(1);
        } else if t == "," || t == ";" {
            complete(&mut phrase, &mut address, &mut comment, &mut objs);
            depth = 0;
            next = find_next(idx + 1, &tokens);
        } else if depth > 0 {
            address.push(t.into());
        } else if next == "<" {
            phrase.push(t.into());
        } else if matches!(t, "." | "@" | ":" | ";")
            || address.is_empty()
            || matches!(address[address.len() - 1].as_str(), "." | "@" | ":" | ";")
        {
            address.push(t.into());
        } else {
            complete(&mut phrase, &mut address, &mut comment, &mut objs);
            depth = 0;
            address.push(t.into());
        }
    }
    objs
}

/// `_find_next`: the first `,`, `;` or `<` at or after `idx`, or the empty
/// string. A `<` ahead means the tokens up to it are a display phrase.
fn find_next(idx: usize, tokens: &[String]) -> &str {
    tokens[idx..]
        .iter()
        .map(String::as_str)
        .find(|t| matches!(*t, "," | ";" | "<"))
        .unwrap_or("")
}

/// `Mail::Address::format` for one address: the phrase (quoted unless it is
/// already quoted or consists only of `atext`), the address in angle brackets,
/// and any comment forced into parentheses.
fn format_address(a: &MailAddress) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !a.phrase.is_empty() {
        if is_all_atext(&a.phrase) || has_unescaped_quote(&a.phrase) {
            parts.push(a.phrase.clone());
        } else {
            parts.push(format!("\"{}\"", a.phrase));
        }
        if !a.address.is_empty() {
            parts.push(format!("<{}>", a.address));
        }
    } else if !a.address.is_empty() {
        parts.push(a.address.clone());
    }

    let mut comment = a.comment.clone();
    if comment.bytes().any(|b| !is_ws(b)) {
        // `s/^\s*\(?/(/` and `s/\)?\s*$/)/`.
        let body = comment.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let body = body.strip_prefix('(').unwrap_or(body);
        let body = body.trim_end_matches(|c: char| c.is_ascii_whitespace());
        let body = body.strip_suffix(')').unwrap_or(body);
        comment = format!("({body})");
    }
    if !comment.is_empty() {
        parts.push(comment);
    }
    parts.join(" ")
}

/// `/^(?:\s*[-\w !#$%&'*+\/=?^`{|}~]\s*)+$/`: every byte is whitespace or one of
/// RFC 2822's `atext` characters, and at least one is `atext`.
fn is_all_atext(s: &str) -> bool {
    let atext = |b: u8| {
        b.is_ascii_alphanumeric() || b"-_ !#$%&'*+/=?^`{|}~".contains(&b)
    };
    let bytes = s.as_bytes();
    !bytes.is_empty() && bytes.iter().all(|&b| atext(b) || is_ws(b)) && bytes.iter().any(|&b| atext(b))
}

/// `/(?<!\\)"/`: a double quote that is not backslash-escaped.
fn has_unescaped_quote(s: &str) -> bool {
    let b = s.as_bytes();
    b.iter().enumerate().any(|(i, &c)| c == b'"' && (i == 0 || b[i - 1] != b'\\'))
}

/// `sanitize_address`: keep an already-quoted or encoded display name as it is,
/// otherwise strip its stray quotes and re-quote it — RFC 2047 for non-ASCII,
/// plain double quotes when it carries a special or control character.
fn sanitize_address(recipient: &str) -> String {
    // `s/(.*>).*$/$1/` — drop whatever trails the last `>`.
    let recipient = match recipient.rfind('>') {
        Some(i) => &recipient[..=i],
        None => recipient,
    };
    // `/^(.*?)\s*(<.*)/` — the lazy name stops at the first `<`.
    let Some(angle) = recipient.find('<') else {
        return recipient.to_string();
    };
    let name = recipient[..angle].trim_end_matches(|c: char| c.is_ascii_whitespace());
    let addr = &recipient[angle..];
    // Perl's falsiness: an empty name — and the literal `0` — take this branch.
    if name.is_empty() || name == "0" {
        return recipient.to_string();
    }
    if is_rfc2047_quoted(name) {
        return recipient.to_string();
    }

    // `s/(^|[^\\])"/$1/g` — remove every quote that is not escaped.
    let b = name.as_bytes();
    let mut stripped: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if i == 0 && b[0] == b'"' {
            i = 1;
        } else if i + 1 < b.len() && b[i] != b'\\' && b[i + 1] == b'"' {
            stripped.push(b[i]);
            i += 2;
        } else {
            stripped.push(b[i]);
            i += 1;
        }
    }

    let name = if stripped.iter().any(|&c| c >= 0x80) {
        quote_rfc2047(&stripped)
    } else if stripped.iter().any(|&c| b"[]()<>@,;:\\\".".contains(&c) || c < 0x20 || c == 0x7f) {
        // `s/([\\\r])/\\$1/g` then wrap in double quotes.
        let mut escaped = String::from("\"");
        for &c in &stripped {
            if c == b'\\' || c == b'\r' {
                escaped.push('\\');
            }
            escaped.push(c as char);
        }
        escaped.push('"');
        escaped
    } else {
        String::from_utf8_lossy(&stripped).into_owned()
    };
    format!("{name} {addr}")
}

/// `is_rfc2047_quoted`: at most 75 characters, and either an entirely ASCII
/// double-quoted string or a single RFC 2047 encoded word.
fn is_rfc2047_quoted(s: &str) -> bool {
    if s.chars().count() > 75 {
        return false;
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') && s.is_ascii() {
        return true;
    }
    is_encoded_word(s)
}

/// `$re_encoded_word` — `=?<token>?<token>?<encoded-text>?=` anchored to the
/// whole string.
fn is_encoded_word(s: &str) -> bool {
    // `$re_token`: no specials, no `=`, `/`, `?`, `.`, space, control or 8-bit.
    let token = |b: u8| !b"][()<>@,;:\\\"/?.= ".contains(&b) && (0x21..0x7f).contains(&b);
    let text = |b: u8| b != b'?' && (0x21..0x7f).contains(&b);
    let Some(body) = s.strip_prefix("=?").and_then(|r| r.strip_suffix("?=")) else {
        return false;
    };
    let mut parts = body.splitn(3, '?');
    let (Some(charset), Some(encoding), Some(encoded)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !charset.is_empty()
        && charset.bytes().all(token)
        && !encoding.is_empty()
        && encoding.bytes().all(token)
        && !encoded.is_empty()
        && encoded.bytes().all(text)
}

/// `quote_rfc2047($_, 'UTF-8')`: every byte outside `-a-zA-Z0-9!*+/` becomes
/// `=<HH>`, and the result is wrapped as a `q`-encoded word.
fn quote_rfc2047(bytes: &[u8]) -> String {
    let mut out = String::from("=?UTF-8?q?");
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-!*+/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("={b:02X}"));
        }
    }
    out.push_str("?=");
    out
}

/// `expand_aliases`: replace each address that names an alias with that alias's
/// own addresses, recursively. An alias reached while it is already being
/// expanded is the `expands to itself` fatal.
fn expand_aliases(addrs: &[String], aliases: &Aliases) -> Result<Vec<String>, String> {
    fn one(
        alias: &str,
        aliases: &Aliases,
        active: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> Result<(), String> {
        if active.iter().any(|a| a == alias) {
            return Err(alias.to_string());
        }
        match aliases.get(alias) {
            Some(expansion) => {
                active.push(alias.to_string());
                for next in expansion {
                    one(next, aliases, active, out)?;
                }
                active.pop();
            }
            None => out.push(alias.to_string()),
        }
        Ok(())
    }

    let mut out = Vec::new();
    let mut active = Vec::new();
    for addr in addrs {
        one(addr, aliases, &mut active, &mut out)?;
    }
    Ok(out)
}

/// Perl's `$!` stringification for the errnos an `open` can raise here.
fn errno_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(2) => "No such file or directory".into(),
        Some(13) => "Permission denied".into(),
        Some(21) => "Is a directory".into(),
        _ => e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Seconds since the epoch, as Perl's `time`.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `localtime`/`gmtime` for one instant.
fn broken_down(t: i64, local: bool) -> libc::tm {
    let tt = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        if local {
            libc::localtime_r(&tt, &mut tm);
        } else {
            libc::gmtime_r(&tt, &mut tm);
        }
    }
    tm
}

const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `format_2822_time` — the `Date:` header. The zone offset is derived by
/// differencing local and GMT the way the script does, rather than read out of
/// `tm_gmtoff`, so the two `die`s it can raise are reachable.
fn format_2822_time(t: i64) -> Result<String, &'static str> {
    let l = broken_down(t, true);
    let g = broken_down(t, false);
    if l.tm_sec != g.tm_sec {
        return Err("local zone differs from GMT by a non-minute interval\n");
    }
    let mut localmin = l.tm_min + l.tm_hour * 60;
    let gmtmin = g.tm_min + g.tm_hour * 60;
    if (g.tm_wday + 1).rem_euclid(7) == l.tm_wday {
        localmin += 1440;
    } else if (g.tm_wday - 1).rem_euclid(7) == l.tm_wday {
        localmin -= 1440;
    } else if g.tm_wday != l.tm_wday {
        return Err("local time offset greater than or equal to 24 hours\n");
    }
    let offset = localmin - gmtmin;
    let offhour = offset / 60;
    let offmin = offset.rem_euclid(60).abs();
    if offhour.abs() >= 24 {
        return Err("local time offset greater than or equal to 24 hours\n");
    }
    Ok(format!(
        "{}, {:2} {} {} {:02}:{:02}:{:02} {}{:02}{:02}",
        WDAYS[l.tm_wday as usize % 7],
        l.tm_mday,
        MONTHS[l.tm_mon as usize % 12],
        l.tm_year + 1900,
        l.tm_hour,
        l.tm_min,
        l.tm_sec,
        if offset >= 0 { '+' } else { '-' },
        offhour.abs(),
        offmin
    ))
}

// ---------------------------------------------------------------------------
// Prompting
// ---------------------------------------------------------------------------

/// `ask` — `Term::ReadLine` over the controlling terminal.
///
/// The script bails out to `$default` `unless defined $term->IN and defined
/// fileno($term->IN) …`, which is what happens whenever there is no terminal to
/// attach to; that is the whole of the non-interactive behaviour and nothing is
/// printed on the way. With a terminal the prompt is written to it and one line
/// is read back, up to ten times.
///
/// `Term::ReadLine::Perl` decorates the prompt with ANSI attributes when it is
/// the chosen back end; that decoration is a property of the Perl module and is
/// not reproduced.
fn ask(
    prompt: &str,
    valid: Option<&dyn Fn(&str) -> bool>,
    default: Option<&str>,
    confirm_only: bool,
) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};

    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty");
    let Ok(tty) = tty else { return default.map(str::to_string) };
    let Ok(mut out) = tty.try_clone() else { return default.map(str::to_string) };
    let mut reader = BufReader::new(tty);

    let mut read_line = |out: &mut std::fs::File, prompt: &str| -> Option<String> {
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    };

    for _ in 0..10 {
        let Some(resp) = read_line(&mut out, prompt) else {
            // EOF: `print "\n"` goes to stdout, not to the terminal handle.
            println!();
            return default.map(str::to_string);
        };
        if resp.is_empty() {
            if let Some(d) = default {
                return Some(d.to_string());
            }
        }
        if valid.is_none_or(|f| f(&resp)) {
            return Some(resp);
        }
        if confirm_only {
            let q = format!("Are you sure you want to use <{resp}> [y/N]? ");
            if let Some(yesno) = read_line(&mut out, &q) {
                if yesno.to_ascii_lowercase().contains('y') {
                    return Some(resp);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// `$local_part_regexp` — `[^<>"\s@]`.
fn is_local_char(b: u8) -> bool {
    !matches!(b, b'<' | b'>' | b'"' | b'@') && !is_ws(b)
}

/// `$domain_regexp`'s label character class — `[^.<>"\s@]`.
fn is_label_char(b: u8) -> bool {
    is_local_char(b) && b != b'.'
}

/// `extract_valid_address`. `Email::Valid` is not a core module and is absent
/// from the Perl the stock script runs under, so the documented fallback — a
/// leftmost `local-part@domain` search — is the live path.
fn extract_valid_address(address: &str) -> Option<String> {
    let b = address.as_bytes();
    if !b.is_empty() && b.iter().all(|&c| is_local_char(c)) {
        return Some(address.to_string());
    }
    // `s/^\s*<(.*)>\s*$/$1/` — greedy, so the last `>` closes it.
    let trimmed = address.trim_matches(|c: char| c.is_ascii_whitespace());
    let inner = match (trimmed.strip_prefix('<'), trimmed.strip_suffix('>')) {
        (Some(_), Some(_)) if trimmed.len() >= 2 => &trimmed[1..trimmed.len() - 1],
        _ => address,
    };

    let s = inner.as_bytes();
    for start in 0..s.len() {
        if !is_local_char(s[start]) {
            continue;
        }
        // The local part is a maximal run: a shorter one would end on an allowed
        // character, never on the `@` the pattern needs next.
        let mut i = start;
        while i < s.len() && is_local_char(s[i]) {
            i += 1;
        }
        if s.get(i) != Some(&b'@') {
            continue;
        }
        let mut j = i + 1;
        let label = |s: &[u8], mut k: usize| {
            while k < s.len() && is_label_char(s[k]) {
                k += 1;
            }
            k
        };
        let first = label(s, j);
        if first == j {
            continue;
        }
        j = first;
        let mut labels = 0;
        while s.get(j) == Some(&b'.') {
            let next = label(s, j + 1);
            if next == j + 1 {
                break;
            }
            j = next;
            labels += 1;
        }
        if labels == 0 {
            continue;
        }
        return Some(inner[start..j].to_string());
    }
    None
}

/// `unique_email_list` — dedupe on the extracted address, keep the entry.
fn unique_email_list(entries: &[String]) -> Result<Vec<String>, String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for e in entries {
        let clean = extract_valid_address(e).ok_or_else(|| e.clone())?;
        if seen.insert(clean) {
            out.push(e.clone());
        }
    }
    Ok(out)
}

/// `strip_garbage_one_address` — trim whatever trails a body address.
fn strip_garbage_one_address(addr: &str) -> String {
    let addr = addr.trim_end_matches('\n');
    // `^(("[^"]*"|[^"<]*)? *<[^>]*>).*`
    if let Some(open) = addr.find('<') {
        let head = &addr[..open];
        let head_ok = if head.starts_with('"') {
            // `"[^"]*"` then optional spaces.
            match head[1..].find('"') {
                Some(i) => head[2 + i..].bytes().all(|c| c == b' '),
                None => false,
            }
        } else {
            !head.contains('"')
        };
        if head_ok {
            if let Some(close) = addr[open..].find('>') {
                return addr[..open + close + 1].to_string();
            }
        }
    }
    // `^([^"#,\s]*)`
    let end = addr
        .bytes()
        .position(|c| matches!(c, b'"' | b'#' | b',') || is_ws(c))
        .unwrap_or(addr.len());
    addr[..end].to_string()
}

// ---------------------------------------------------------------------------
// RFC 2047 subjects and transfer encodings
// ---------------------------------------------------------------------------

/// `unquote_rfc2047` — decode every `q`-encoded word, returning the text and the
/// charset of the last word decoded.
fn unquote_rfc2047(s: &str) -> (String, Option<String>) {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut charset = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && bytes.get(i + 1) == Some(&b'?') {
            if let Some(end) = encoded_word_end(bytes, i) {
                let word = &s[i..end];
                let body = &word[2..word.len() - 2];
                let mut parts = body.splitn(3, '?');
                let (cs, enc, text) =
                    (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                charset = Some(cs.to_string());
                if enc.eq_ignore_ascii_case("q") {
                    let t = text.as_bytes();
                    let mut k = 0;
                    while k < t.len() {
                        match t[k] {
                            b'_' => {
                                out.push(b' ');
                                k += 1;
                            }
                            b'=' if k + 2 < t.len() => {
                                match u8::from_str_radix(&text[k + 1..k + 3], 16) {
                                    Ok(v) => {
                                        out.push(v);
                                        k += 3;
                                    }
                                    Err(_) => {
                                        out.push(t[k]);
                                        k += 1;
                                    }
                                }
                            }
                            c => {
                                out.push(c);
                                k += 1;
                            }
                        }
                    }
                } else {
                    out.extend_from_slice(text.as_bytes());
                }
                i = end;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    (String::from_utf8_lossy(&out).into_owned(), charset)
}

/// The end offset of the `$re_encoded_word` starting at `at`, if there is one.
fn encoded_word_end(s: &[u8], at: usize) -> Option<usize> {
    let token = |b: u8| !b"][()<>@,;:\\\"/?.= ".contains(&b) && (0x21..0x7f).contains(&b);
    let text = |b: u8| b != b'?' && (0x21..0x7f).contains(&b);
    let mut i = at + 2;
    let cs = i;
    while i < s.len() && token(s[i]) {
        i += 1;
    }
    if i == cs || s.get(i) != Some(&b'?') {
        return None;
    }
    i += 1;
    let en = i;
    while i < s.len() && token(s[i]) {
        i += 1;
    }
    if i == en || s.get(i) != Some(&b'?') {
        return None;
    }
    i += 1;
    let tx = i;
    while i < s.len() && text(s[i]) {
        i += 1;
    }
    if i == tx || s.get(i) != Some(&b'?') || s.get(i + 1) != Some(&b'=') {
        return None;
    }
    Some(i + 2)
}

/// `subject_needs_rfc2047_quoting` then `quote_rfc2047`.
fn quote_subject(subject: &str, encoding: &str) -> String {
    if subject.is_ascii() && !subject.contains("=?") {
        return subject.to_string();
    }
    let mut out = format!("=?{encoding}?q?");
    for &b in subject.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-!*+/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("={b:02X}"));
        }
    }
    out.push_str("?=");
    out
}

/// `MIME::QuotedPrint::encode($message, "\n", 0)` — text mode, so a newline is a
/// line break rather than an escape, trailing blanks on a line are escaped, and
/// no output line exceeds 76 bytes including the soft-break `=`.
fn encode_qp(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    for line in input.split_inclusive(|&b| b == b'\n') {
        let (body, eol): (&[u8], &[u8]) = match line.strip_suffix(b"\n") {
            Some(b) => (b, b"\n"),
            None => (line, b""),
        };
        let mut column = 0usize;
        let mut pending: Vec<u8> = Vec::new();
        let emit = |out: &mut Vec<u8>, column: &mut usize, tok: &[u8]| {
            if *column + tok.len() > 75 {
                out.push(b'=');
                out.push(b'\n');
                *column = 0;
            }
            out.extend_from_slice(tok);
            *column += tok.len();
        };
        for &b in body {
            if b == b' ' || b == b'\t' {
                // Rule 3: blanks are literal unless they end the line.
                pending.push(b);
                continue;
            }
            for &p in &pending {
                emit(&mut out, &mut column, &[p]);
            }
            pending.clear();
            if (33..=60).contains(&b) || (62..=126).contains(&b) {
                emit(&mut out, &mut column, &[b]);
            } else {
                emit(&mut out, &mut column, format!("={b:02X}").as_bytes());
            }
        }
        for &p in &pending {
            emit(&mut out, &mut column, format!("={p:02X}").as_bytes());
        }
        out.extend_from_slice(eol);
    }
    out
}

/// `MIME::QuotedPrint::decode`.
fn decode_qp(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for line in input.split_inclusive(|&b| b == b'\n') {
        let (body, had_eol) = match line.strip_suffix(b"\n") {
            Some(b) => (b, true),
            None => (line, false),
        };
        // Trailing whitespace on an encoded line is not significant.
        let body = {
            let mut end = body.len();
            while end > 0 && (body[end - 1] == b' ' || body[end - 1] == b'\t' || body[end - 1] == b'\r') {
                end -= 1;
            }
            &body[..end]
        };
        let soft = body.ends_with(b"=");
        let body = if soft { &body[..body.len() - 1] } else { body };
        let mut i = 0;
        while i < body.len() {
            if body[i] == b'=' && i + 2 < body.len() + 1 && i + 2 <= body.len() {
                let hex = std::str::from_utf8(&body[i + 1..i + 3]).ok();
                if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(body[i]);
            i += 1;
        }
        if had_eol && !soft {
            out.push(b'\n');
        }
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `MIME::Base64::encode($message, "\n")` — 76 characters per line.
pub(crate) fn encode_base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut column = 0;
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        let quad = [
            B64[(n >> 18) as usize & 63],
            B64[(n >> 12) as usize & 63],
            if chunk.len() > 1 { B64[(n >> 6) as usize & 63] } else { b'=' },
            if chunk.len() > 2 { B64[n as usize & 63] } else { b'=' },
        ];
        out.extend_from_slice(&quad);
        column += 4;
        if column >= 76 {
            out.push(b'\n');
            column = 0;
        }
    }
    if column > 0 {
        out.push(b'\n');
    }
    out
}

/// `MIME::Base64::decode`.
fn decode_base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in input {
        let Some(v) = B64.iter().position(|&x| x == c) else { continue };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// `apply_transfer_encoding($message, $from, $to)`.
fn apply_transfer_encoding(
    message: Vec<u8>,
    from: &str,
    to: &str,
) -> Result<(Vec<u8>, String), &'static str> {
    if from == to && from != "7bit" {
        return Ok((message, to.to_string()));
    }
    let message = match from {
        "quoted-printable" => decode_qp(&message),
        "base64" => decode_base64(&message),
        _ => message,
    };
    let to = if to == "auto" {
        // `/(?:.{999,}|\r)/` — a line of 999 or more bytes, or any CR.
        let long = message.split(|&b| b == b'\n').any(|l| l.len() >= 999);
        if long || message.contains(&b'\r') {
            "quoted-printable"
        } else {
            "8bit"
        }
    } else {
        to
    };
    match to {
        "7bit" if message.iter().any(|&b| b >= 0x80) => Err("cannot send message as 7bit"),
        "7bit" | "8bit" => Ok((message, to.to_string())),
        "quoted-printable" => Ok((encode_qp(&message), to.to_string())),
        "base64" => Ok((encode_base64(&message), to.to_string())),
        _ => Err("invalid transfer encoding"),
    }
}

// ---------------------------------------------------------------------------
// Subprocesses
// ---------------------------------------------------------------------------

/// Perl's `\Q…\E` — backslash-escape every byte that is not a word character,
/// which is how the file name reaches the shell in `execute_cmd`.
fn quotemeta(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `execute_cmd($prefix, $cmd, $file)` — run `<cmd> <file>` through the shell
/// and collect its lines. A blank line ends the output; anything after one is an
/// error.
fn execute_cmd(prefix: &str, cmd: &str, file: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} {}", quotemeta(file)))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| format!("({prefix}) Could not execute '{cmd}'"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = Vec::new();
    let mut seen_blank = false;
    for line in text.split_inclusive('\n') {
        if seen_blank {
            return Err(format!("({prefix}) Malformed output from '{cmd}'"));
        }
        if line == "\n" || line.is_empty() {
            seen_blank = true;
            continue;
        }
        lines.push(line.to_string());
    }
    if !out.status.success() {
        return Err(format!("({prefix}) failed to close pipe to '{cmd}'"));
    }
    Ok(lines)
}

/// This binary, so the script's `Git::command('check-mailmap', …)` and
/// `Git::command_input_pipe(['imap-send', …])` reach the ported subcommands
/// rather than whatever `git` happens to be on `PATH`.
pub(crate) fn self_exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "git".into())
}

/// `unfold_headers` — RFC 2822 continuation lines are folded into their header.
fn unfold_headers(lines: &[String]) -> Vec<String> {
    let mut headers: Vec<String> = Vec::new();
    for line in lines {
        if line.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty() {
            break;
        }
        let continuation = line
            .strip_prefix(|c: char| c.is_ascii_whitespace())
            .is_some()
            && line.trim_start_matches(|c: char| c.is_ascii_whitespace()).starts_with(|c: char| !c.is_ascii_whitespace());
        if continuation && !headers.is_empty() {
            let last = headers.last_mut().expect("non-empty");
            while last.ends_with('\n') {
                last.pop();
            }
            last.push(' ');
            last.push_str(line.trim_start_matches(|c: char| c.is_ascii_whitespace()));
        } else {
            headers.push(line.clone());
        }
    }
    headers
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// `usage()` — the block on stdout, `exit(1)`.
fn usage() -> ExitCode {
    print!("{USAGE}");
    ExitCode::from(1)
}

/// The exit status of the most recent `git` child process the script ran, which
/// is what Perl's `$?` holds when a `die` is raised. See [`die`].
type LastChild = std::cell::Cell<u8>;

/// Perl exits a `die` with `$! || ($? >> 8) || 255`. `$!` is 0 on every path
/// measured here, so the status is `$? >> 8` from the last child git process,
/// falling back to 255 when that child succeeded.
///
/// The children the script runs before the `die`s this port can reach, in order:
///   1. `git rev-parse --show-prefix` (repository discovery, exit 0).
///   2. `git config --null --get-regexp '^sende?mail[.]'` in `config_regexp` —
///      exit 1 when nothing matched, 0 otherwise. `read_config` only shells out
///      for keys `config_regexp` already found, and only for the `bool` and
///      `path` tables, so a plain-string key such as `sendemail.to` adds no
///      child of its own.
///   3. `git rev-parse --verify --quiet <arg>` per file/directory operand in
///      `is_format_patch_arg` — exit 1 for a name that is not a revision.
///   4. `git var GIT_AUTHOR_IDENT` via `Git::ident_person`, but only when
///      `$sender` is still undefined at line 838, i.e. when neither `--from`
///      nor `sendemail.from` supplied one. Exit 0.
///
/// That ordering is what makes the status of `No subject line in <file>?` depend
/// on the sender rather than on the config, which is the opposite of what this
/// module previously assumed. Measured against git 2.55.0 on a repository whose
/// only operand is a non-revision file:
///
/// | invocation                              | last child                  | exit |
/// |-----------------------------------------|-----------------------------|------|
/// | `send-email … README.md`                | `git var GIT_AUTHOR_IDENT`  | 255  |
/// | `-c sendemail.to=… send-email … `       | `git var GIT_AUTHOR_IDENT`  | 255  |
/// | `-c sendemail.smtpserver=… send-email …`| `git var GIT_AUTHOR_IDENT`  | 255  |
/// | `-c sendemail.from=x@y send-email …`    | `rev-parse --verify README.md` | 1 |
///
/// A `die` raised before the operand loop still reports `config_regexp`'s
/// status, which is why [`run`] seeds the cell from it.
fn die(msg: &str, last_child: &LastChild) -> ExitCode {
    eprint!("{msg}");
    let status = last_child.get();
    ExitCode::from(if status == 0 { 255 } else { status })
}

/// A `die` or an `exit` raised on the way to sending.
enum Stop {
    /// The message exactly as the script writes it, newline included.
    Die(String),
    /// `exit(<code>)`, which the prompting paths reach.
    Exit(u8),
    /// A path whose substrate is genuinely absent; reported through `anyhow`
    /// like every other unported surface in this tree.
    Unported(String),
}

type Step<T> = std::result::Result<T, Stop>;

fn died(msg: impl Into<String>) -> Stop {
    Stop::Die(msg.into())
}

/// Whether `send_message` must prompt before sending.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Needs {
    No,
    Yes,
    /// The Cc list grew past what the user asked for; the long explanation is
    /// printed once ahead of the prompt.
    Inform,
}

/// What the user chose at the `--confirm` prompt.
enum Sent {
    Yes,
    No,
    Edit,
}

/// The script's file-scoped state, from `read_config` through the send loop.
struct Mailer {
    repo: Option<gix::Repository>,
    s: Settings,
    aliases: Aliases,

    quiet: bool,
    dry_run: bool,
    force: bool,
    no_header_cmd: bool,
    annotate: bool,
    initial_subject: Option<String>,
    initial_in_reply_to: Option<String>,
    reply_to: Option<String>,
    initial_to: Vec<String>,
    initial_cc: Vec<String>,
    initial_bcc: Vec<String>,
    /// `%suppress_cc`. A key with a false value still counts as present, which
    /// is what flips `--confirm`'s default.
    suppress: BTreeMap<String, bool>,
    confirm: String,
    confirm_unconfigured: bool,

    sender: String,
    files: Vec<String>,
    broken_encoding: BTreeSet<String>,
    time: i64,

    message_id: Option<String>,
    subject: Option<String>,
    in_reply_to: Option<String>,
    references: String,
    message: Vec<u8>,
    to: Vec<String>,
    cc: Vec<String>,
    xh: Vec<String>,
    needs_confirm: Needs,
    message_num: i64,
    ask_default: Option<String>,
    num_sent: u64,
    id_stamp: Option<String>,
    id_serial: u64,
    prompting: bool,
    editor: Option<String>,
    imap_copy: Vec<Vec<u8>>,
    /// `$smtp` — the live `Net::SMTP` session, kept across messages and torn
    /// down by `--batch-size`.
    smtp: Option<super::smtp::Session>,
    /// `$debug_net_smtp`.
    smtp_debug: bool,

    /// `$compose`: 0 when `--compose` was not given, 1 while the composed
    /// message is part of the series, and -1 once an empty summary has taken it
    /// back out. `-1` is still true in Perl, so it keeps driving the
    /// `--confirm` default and the temporary-file cleanup.
    compose: i32,
    /// `$compose_filename`.
    compose_filename: Option<String>,
}

impl Mailer {
    /// `cleanup_compose_files` — both temporaries go, including on the `-1` path.
    fn cleanup_compose_files(&self) {
        if self.compose == 0 {
            return;
        }
        if let Some(f) = &self.compose_filename {
            std::fs::remove_file(f).ok();
            std::fs::remove_file(format!("{f}.final")).ok();
        }
    }

    fn suppressed(&self, key: &str) -> bool {
        self.suppress.get(key).copied().unwrap_or(false)
    }

    /// `Git::ident_person(@repo, $what)` — `"$name <$email>"`.
    fn ident_person(&self, author: bool) -> Option<String> {
        let repo = self.repo.as_ref()?;
        let sig = if author { repo.author() } else { repo.committer() }?.ok()?;
        Some(format!("{} <{}>", sig.name, sig.email))
    }

    /// `make_message_id` — `<stamp.pid-serial-address>`.
    fn make_message_id(&mut self) {
        if self.id_stamp.is_none() {
            let tm = broken_down(now_seconds(), false);
            self.id_stamp = Some(format!(
                "{:04}{:02}{:02}{:02}{:02}{:02}.{}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec,
                std::process::id()
            ));
            self.id_serial = 0;
        }
        self.id_serial += 1;
        let uniq = format!("{}-{}", self.id_stamp.as_deref().unwrap_or(""), self.id_serial);

        let mut du_part = String::new();
        let candidates = [
            Some(self.sender.clone()),
            self.ident_person(false),
            self.ident_person(true),
        ];
        for c in candidates.into_iter().flatten() {
            if let Some(v) = extract_valid_address(&sanitize_address(&c)) {
                if !v.is_empty() {
                    du_part = v;
                    break;
                }
            }
        }
        if du_part.is_empty() {
            du_part = format!("user@{}", hostname());
        }
        self.message_id = Some(format!("<{uniq}-{du_part}>"));
    }

    /// `expand_aliases`, then the `expands to itself` fatal.
    fn expand(&self, addrs: &[String]) -> Step<Vec<String>> {
        expand_aliases(addrs, &self.aliases)
            .map_err(|a| died(format!("fatal: alias '{a}' expands to itself\n")))
    }

    /// `validate_address_list` — an address no `local@domain` can be pulled out
    /// of is reported and then dropped, edited or quit on.
    fn validate_address_list(&self, list: Vec<String>) -> Step<Vec<String>> {
        let mut out = Vec::new();
        for addr in list {
            let mut addr = addr;
            loop {
                if extract_valid_address(&addr).is_some() {
                    out.push(addr);
                    break;
                }
                eprintln!("error: unable to extract a valid address from: {addr}");
                let answer = ask(
                    "What to do with this address? ([q]uit|[d]rop|[e]dit): ",
                    Some(&|r: &str| {
                        let r = r.to_ascii_lowercase();
                        ["quit", "q", "drop", "d", "edit", "e"].iter().any(|p| r.starts_with(p))
                    }),
                    Some("q"),
                    false,
                )
                .unwrap_or_default()
                .to_ascii_lowercase();
                if answer.starts_with('d') {
                    break;
                }
                if answer.starts_with('q') {
                    self.cleanup_compose_files();
                    return Err(Stop::Exit(0));
                }
                addr = ask(
                    "To whom should the emails be sent (if anyone)? ",
                    Some(&|r: &str| r.contains('@') && r.split('@').nth(1).is_some_and(|d| d.contains('.'))),
                    Some(""),
                    true,
                )
                .unwrap_or_default();
            }
        }
        Ok(out)
    }

    /// `mailmap_address_list` — `git check-mailmap` with the configured file and
    /// blob, then the `<>` wrapping a bare address comes back in is removed.
    fn mailmap_address_list(&self, list: Vec<String>) -> Step<Vec<String>> {
        if list.is_empty() || !self.s.mailmap {
            return Ok(list);
        }
        let mut cmd = std::process::Command::new(self_exe());
        cmd.arg("check-mailmap");
        if let Some(f) = &self.s.mailmap_file {
            cmd.arg(format!("--mailmap-file={f}"));
        }
        if let Some(b) = &self.s.mailmap_blob {
            cmd.arg(format!("--mailmap-blob={b}"));
        }
        cmd.args(&list);
        let out = cmd
            .output()
            .map_err(|e| died(format!("fatal: cannot run check-mailmap: {e}\n")))?;
        if !out.status.success() {
            std::io::Write::write_all(&mut std::io::stderr(), &out.stderr).ok();
            return Err(Stop::Exit(u8::try_from(out.status.code().unwrap_or(1)).unwrap_or(1)));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| match (l.strip_prefix('<'), l.strip_suffix('>')) {
                (Some(_), Some(_)) if l.len() >= 2 => l[1..l.len() - 1].to_string(),
                _ => l.to_string(),
            })
            .collect())
    }

    /// `process_address_list`.
    fn process_address_list(&self, list: &[String]) -> Step<Vec<String>> {
        let parsed: Vec<String> = list.iter().flat_map(|l| parse_address_line(l)).collect();
        let expanded = self.expand(&parsed)?;
        let sanitized: Vec<String> = expanded.iter().map(|a| sanitize_address(a)).collect();
        let validated = self.validate_address_list(sanitized)?;
        self.mailmap_address_list(validated)
    }

    /// `do_edit` — `sendemail.multiEdit` decides whether the editor sees every
    /// file at once or one per invocation.
    fn do_edit(&mut self, files: &[String]) -> Step<()> {
        if self.editor.is_none() {
            let out = std::process::Command::new(self_exe())
                .args(["var", "GIT_EDITOR"])
                .output()
                .map_err(|e| died(format!("fatal: cannot run git var: {e}\n")))?;
            self.editor =
                Some(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string());
        }
        let editor = self.editor.clone().unwrap_or_default();
        let die_msg = "the editor exited uncleanly, aborting everything\n";
        let run = |batch: &[String]| -> Step<()> {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$@\""))
                .arg(&editor)
                .args(batch)
                .status()
                .map_err(|_| died(die_msg))?;
            if status.success() {
                Ok(())
            } else {
                Err(died(die_msg))
            }
        };
        // `if (defined($multiedit) && !$multiedit)` — one editor per file only
        // when the setting is present and false.
        if self.s.multiedit == Some(false) {
            for f in files {
                run(std::slice::from_ref(f))?;
            }
            Ok(())
        } else {
            run(files)
        }
    }

    /// `gen_header` — the recipient list and the header block that goes with it.
    fn gen_header(&mut self) -> Step<(Vec<String>, String)> {
        let recipients = unique_email_list(&self.to)
            .map_err(|a| died(format!("error: unable to extract a valid address from: {a}\n")))?;
        // Drop from Cc anything already on the To line.
        let mut kept = Vec::new();
        for entry in &self.cc {
            let cc = extract_valid_address(entry).ok_or_else(|| {
                died(format!("error: unable to extract a valid address from: {entry}\n"))
            })?;
            let dup = recipients.iter().any(|r| *r == cc || r.ends_with(&format!("<{cc}>")));
            if !dup {
                kept.push(entry.clone());
            }
        }
        self.cc = kept;

        let to = recipients.join(",\n\t");
        let mut all = recipients.clone();
        all.extend(self.cc.iter().cloned());
        all.extend(self.initial_bcc.iter().cloned());
        let all = unique_email_list(&all)
            .map_err(|a| died(format!("error: unable to extract a valid address from: {a}\n")))?;
        let envelope: Vec<String> = all
            .iter()
            .map(|e| {
                extract_valid_address(e).ok_or_else(|| {
                    died(format!("error: unable to extract a valid address from: {e}\n"))
                })
            })
            .collect::<Step<Vec<String>>>()?;

        let date = format_2822_time(self.time).map_err(died)?;
        self.time += 1;

        let cc = unique_email_list(&self.cc)
            .map_err(|a| died(format!("error: unable to extract a valid address from: {a}\n")))?
            .join(",\n\t");
        let ccline = if cc.is_empty() { String::new() } else { format!("\nCc: {cc}") };
        if self.message_id.is_none() {
            self.make_message_id();
        }

        let mut header = format!(
            "From: {}\nTo: {to}{ccline}\nSubject: {}\nDate: {date}\nMessage-ID: {}\n",
            self.sender,
            self.subject.clone().unwrap_or_default(),
            self.message_id.clone().unwrap_or_default(),
        );
        if self.s.use_xmailer {
            header.push_str(&format!("X-Mailer: git-send-email {GIT_VERSION}\n"));
        }
        if self.in_reply_to.as_deref().is_some_and(|s| !s.is_empty()) {
            header.push_str(&format!("In-Reply-To: {}\n", self.in_reply_to.clone().unwrap_or_default()));
            header.push_str(&format!("References: {}\n", self.references));
        }
        if self.reply_to.as_deref().is_some_and(|s| !s.is_empty()) {
            header.push_str(&format!("Reply-To: {}\n", self.reply_to.clone().unwrap_or_default()));
        }
        if !self.xh.is_empty() {
            header.push_str(&self.xh.join("\n"));
            header.push('\n');
        }
        Ok((envelope, header))
    }

    /// `smtp_host_string()` — the server as the credential helper is told
    /// about it.
    fn smtp_host_string(&self) -> String {
        let server = self.s.smtp_server.clone().unwrap_or_default();
        match &self.s.smtp_server_port {
            Some(port) => format!("{server}:{port}"),
            None => server,
        }
    }

    /// `is_outlook($host)`. The script resolves `'auto'` into 1 or 0 on the
    /// first call and keeps the answer for the rest of the run, so this does
    /// too.
    fn is_outlook(&mut self, host: &str) -> bool {
        let fixed = *self.s.smtp.outlook_id_fix.get_or_insert_with(|| {
            host == "smtp.office365.com" || host == "smtp-mail.outlook.com"
        });
        fixed
    }

    /// The `Net::SMTP` branch of `send_message`: open (or reuse) the session,
    /// authenticate, and hand over the envelope and the message.
    fn smtp_send(
        &mut self,
        raw_from: &str,
        recipients: &[String],
        header: &mut String,
    ) -> Step<()> {
        let server = self.s.smtp_server.clone().unwrap_or_default();
        // `$smtp_domain ||= maildomain();`
        if self.s.smtp.domain.as_deref().unwrap_or_default().is_empty() {
            self.s.smtp.domain = Some(super::smtp::maildomain());
        }
        let encryption = self.s.smtp.encryption.clone().unwrap_or_default();
        let port_unset = self.s.smtp_server_port.as_deref().unwrap_or_default().is_empty();
        if encryption == "ssl" {
            // `$smtp_server_port ||= 465; # ssmtp`
            if port_unset {
                self.s.smtp_server_port = Some("465".into());
            }
        } else if self.smtp.is_none() && port_unset {
            self.s.smtp_server_port = Some("25".into());
        }

        if self.smtp.is_none() {
            let domain = self.s.smtp.domain.clone().unwrap_or_default();
            let port = self.s.smtp_server_port.clone().unwrap_or_default();
            let cfg = super::smtp::Connect {
                server: &server,
                port: &port,
                domain: &domain,
                encryption: &encryption,
                ssl: &self.s.smtp.ssl,
                debug: self.smtp_debug,
            };
            match super::smtp::Session::connect(&cfg) {
                Ok(session) => self.smtp = Some(session),
                Err(super::smtp::ConnectError::Die(msg)) => return Err(died(msg)),
                Err(super::smtp::ConnectError::Undef) => {}
            }
        }
        if self.smtp.is_none() {
            let port = match &self.s.smtp_server_port {
                Some(p) => format!(" port={p}"),
                None => String::new(),
            };
            return Err(died(format!(
                "Unable to initialize SMTP properly. Check config and use --smtp-debug. \
                 VALUES: server={server} encryption={encryption} hello={}{port}\n",
                self.s.smtp.domain.clone().unwrap_or_default()
            )));
        }

        // `is_outlook` only ever consults the server name, so resolving it here
        // rather than after `dataend` cannot change the answer.
        let outlook = self.is_outlook(&server);
        let debug = self.smtp_debug;
        let host = self.smtp_host_string();
        let auth = super::smtp::Auth {
            user: self.s.smtp.authuser.as_deref(),
            pass: self.s.smtp.authpass.as_deref(),
            mechanisms: self.s.smtp.auth.as_deref(),
            host,
        };

        // `"$header\n$message"`, which the script feeds to `datasend` one line
        // at a time; the framing `Net::Cmd` applies is per line either way.
        let mut body = header.clone().into_bytes();
        body.push(b'\n');
        body.extend_from_slice(&self.message);

        let session = self.smtp.as_mut().expect("connected just above");
        // `smtp_auth_maybe or die $smtp->message;`
        if !session.auth_maybe(&auth).map_err(died)? {
            return Err(died(session.message()));
        }
        if !session.mail(raw_from) {
            return Err(died(session.message()));
        }
        if !session.recipients(recipients) {
            return Err(died(session.message()));
        }
        if !session.data() {
            return Err(died(session.message()));
        }
        if !session.datasend(&body) {
            return Err(died(session.message()));
        }
        if !session.dataend() {
            return Err(died(session.message()));
        }

        // Outlook discards the Message-ID it was given and assigns its own, so
        // the one in the final reply is what a follow-up must thread against.
        let reassigned = if outlook {
            match angle_addr(&session.message()) {
                Some(id) => Some(format!("<{id}>")),
                None => {
                    eprint!("Warning: Could not retrieve Message-ID from server response.\n");
                    None
                }
            }
        } else {
            None
        };
        let code = session.code();
        let reply = session.message();

        if let Some(id) = reassigned {
            *header = replace_message_id(header, &id);
            self.message_id = Some(id.clone());
            if debug {
                println!("Outlook reassigned Message-ID to: {id}");
            }
        }
        // `$smtp->code =~ /250|200/`.
        let code = code.to_string();
        if !(code.contains("250") || code.contains("200")) {
            let subject = self.subject.clone().unwrap_or_default();
            return Err(died(format!("Failed to send {subject}\n{reply}")));
        }
        Ok(())
    }

    /// `send_message` — prompt if asked to, then hand the bytes to the sendmail
    /// program, to an SMTP session, or to nothing at all under `--dry-run`.
    fn send_message(&mut self) -> Step<Sent> {
        let (recipients, mut header) = self.gen_header()?;

        let mut params: Vec<String> = vec!["-i".into()];
        params.extend(recipients.iter().cloned());
        let mut raw_from = self.sender.clone();
        if let Some(es) = &self.s.envelope_sender {
            if es != "auto" {
                raw_from = es.clone();
            }
        }
        raw_from = extract_valid_address(&raw_from).unwrap_or_default();
        if self.s.envelope_sender.is_some() {
            params.splice(0..0, ["-f".to_string(), raw_from.clone()]);
        }

        if self.needs_confirm != Needs::No && !self.dry_run {
            println!("\n{header}");
            if self.needs_confirm == Needs::Inform {
                self.confirm_unconfigured = false;
                self.ask_default = Some("y".into());
                print!("{INFORM}");
            }
            let answer = ask(
                "Send this email? ([y]es|[n]o|[e]dit|[q]uit|[a]ll): ",
                Some(&|r: &str| {
                    let r = r.to_ascii_lowercase();
                    ["yes", "y", "no", "n", "edit", "e", "quit", "q", "all", "a"]
                        .iter()
                        .any(|p| r.starts_with(p))
                }),
                self.ask_default.as_deref(),
                false,
            );
            let Some(answer) = answer else {
                return Err(died("Send this email reply required\n"));
            };
            let answer = answer.to_ascii_lowercase();
            if answer.starts_with('n') {
                self.message_num -= 1;
                return Ok(Sent::No);
            } else if answer.starts_with('e') {
                self.message_num -= 1;
                return Ok(Sent::Edit);
            } else if answer.starts_with('q') {
                self.cleanup_compose_files();
                return Err(Stop::Exit(0));
            } else if answer.starts_with('a') {
                self.confirm = "never".into();
            }
        }

        params.splice(0..0, self.s.smtp_server_options.iter().cloned());

        let absolute_server =
            self.s.smtp_server.as_deref().is_some_and(|s| std::path::Path::new(s).is_absolute());
        if self.dry_run {
            // Nothing leaves the process.
        } else if self.s.use_imap_only {
            if self.s.imap_sent_folder.is_none() {
                return Err(died("The destination IMAP folder is not properly defined.\n"));
            }
        } else if self.s.sendmail_cmd.is_some() || absolute_server {
            let mut cmd = match &self.s.sendmail_cmd {
                Some(sc) => {
                    let mut c = std::process::Command::new("sh");
                    c.arg("-c").arg(format!("{sc} \"$@\"")).arg("-").args(&params);
                    c
                }
                None => {
                    let mut c =
                        std::process::Command::new(self.s.smtp_server.clone().unwrap_or_default());
                    c.args(&params);
                    c
                }
            };
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| died(format!("{e}\n")))?;
            let mut body = header.clone().into_bytes();
            body.push(b'\n');
            body.extend_from_slice(&self.message);
            {
                let stdin = child.stdin.as_mut().expect("piped");
                std::io::Write::write_all(stdin, &body).map_err(|e| died(format!("{e}\n")))?;
            }
            let status = child.wait().map_err(|e| died(format!("{e}\n")))?;
            if !status.success() {
                return Err(died("Broken pipe\n"));
            }
        } else if self.s.smtp_server.is_none() {
            return Err(died("The required SMTP server is not properly defined.\n"));
        } else {
            self.smtp_send(&raw_from, &recipients, &mut header)?;
        }

        let subject = self.subject.clone().unwrap_or_default();
        if self.quiet {
            println!("{} {subject}", if self.dry_run { "Dry-Sent" } else { "Sent" });
        } else {
            println!("{}", if self.dry_run { "Dry-OK. Log says:" } else { "OK. Log says:" });
            if self.s.sendmail_cmd.is_none() && !absolute_server {
                println!("Server: {}", self.s.smtp_server.clone().unwrap_or_default());
                println!("MAIL FROM:<{raw_from}>");
                for e in &recipients {
                    println!("RCPT TO:<{e}>");
                }
            } else {
                let sm = self
                    .s
                    .sendmail_cmd
                    .clone()
                    .unwrap_or_else(|| self.s.smtp_server.clone().unwrap_or_default());
                println!("Sendmail: {sm} {}", params.join(" "));
            }
            print!("{header}");
            println!();
            match &self.smtp {
                // `print "Result: ", $smtp->code, ' ', ($smtp->message =~
                // /\n([^\n]+\n)$/s);` — the trailing line of a multi-line
                // reply, and nothing at all when the reply had a single line.
                Some(session) => {
                    print!("Result: {} {}", session.code(), last_reply_line(&session.message()));
                }
                None => print!("Result: OK"),
            }
            println!();
        }

        if self.s.imap_sent_folder.is_some() && !self.dry_run {
            if !self.initial_bcc.is_empty() {
                header.push_str(&format!("Bcc: {}\n", self.initial_bcc.join(", ")));
            }
            let mut copy = format!("From git-send-email\n{header}\n").into_bytes();
            copy.extend_from_slice(&self.message);
            self.imap_copy.push(copy);
        }
        Ok(Sent::Yes)
    }
}

/// `$smtp->message =~ /<([^>]+)>/` — the first angle-bracketed run of the
/// server's reply, which is where Outlook puts the Message-ID it assigned.
fn angle_addr(reply: &str) -> Option<&str> {
    let open = reply.find('<')?;
    let rest = &reply[open + 1..];
    let close = rest.find('>')?;
    (close > 0).then(|| &rest[..close])
}

/// `$header =~ s/^(Message-ID:\s*).*\n/${1}$message_id\n/m` — the first
/// `Message-ID:` line takes the identifier the server assigned, keeping the
/// spacing the header was written with.
fn replace_message_id(header: &str, message_id: &str) -> String {
    let mut out = String::with_capacity(header.len());
    let mut replaced = false;
    for line in header.split_inclusive('\n') {
        match line.strip_prefix("Message-ID:") {
            Some(rest) if !replaced => {
                replaced = true;
                let spacing: String =
                    rest.chars().take_while(|c| c.is_ascii_whitespace() && *c != '\n').collect();
                out.push_str("Message-ID:");
                out.push_str(&spacing);
                out.push_str(message_id);
                out.push('\n');
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// `$smtp->message =~ /\n([^\n]+\n)$/s` — the last line of a multi-line reply,
/// and the empty string when the reply had only one.
fn last_reply_line(message: &str) -> &str {
    let body = message.strip_suffix('\n').unwrap_or(message);
    match body.rfind('\n') {
        Some(pos) if !body[pos + 1..].is_empty() => &message[pos + 1..],
        _ => "",
    }
}

/// The `X-Mailer:` version. The script substitutes git's own version here.
const GIT_VERSION: &str = "2.55.0";

/// The block printed once when the Cc list grew on its own.
const INFORM: &str = concat!(
    "    The Cc list above has been expanded by additional\n",
    "    addresses found in the patch commit message. By default\n",
    "    send-email prompts before sending whenever this occurs.\n",
    "    This behavior is controlled by the sendemail.confirm\n",
    "    configuration setting.\n",
    "\n",
    "    For additional information, run 'git send-email --help'.\n",
    "    To retain the current behavior, but squelch this message,\n",
    "    run 'git config --global sendemail.confirm auto'.\n",
    "\n",
);

/// `Sys::Hostname::hostname()`, for the fallback message-id domain.
pub(crate) fn hostname() -> String {
    // `c_char`, not `i8`: it is unsigned on aarch64 Linux and signed on x86_64
    // and Darwin, so naming the concrete type builds on one and not the other.
    let mut buf = [0 as libc::c_char; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return "localhost".into();
    }
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `git send-email` — send a collection of patches as email.
///
/// Reproduces the script's three option passes, `read_config` over all three
/// setting tables, `--dump-aliases`/`--translate-aliases`, and the whole compose
/// and send path down to a sendmail-style program (`--sendmail-cmd`, or an
/// absolute `--smtp-server`), to an SMTP server over a socket, and to
/// `--dry-run`. Delegating a revision range to `git format-patch` bails.
pub fn send_email(args: &[String]) -> Result<ExitCode> {
    let (config_file, in_repo) = load_config();
    let known = known_keys(config_file.as_ref());
    // Seeded with `config_regexp`'s own status; `run` advances it as it reaches
    // the points where the script spawns another child. See [`die`].
    let last_child = LastChild::new(u8::from(known.0.is_empty()));
    match run(args, &known, in_repo, &last_child) {
        Ok(code) => Ok(code),
        Err(Stop::Die(msg)) => Ok(die(&msg, &last_child)),
        Err(Stop::Exit(code)) => Ok(ExitCode::from(code)),
        Err(Stop::Unported(msg)) => anyhow::bail!("{msg}"),
    }
}

fn run(args: &[String], known: &Known, in_repo: bool, last_child: &LastChild) -> Step<ExitCode> {
    // sendemail.identity is read before anything else, then overridden by
    // --identity and cleared by --no-identity.
    let mut identity = known.last("sendemail.identity").map(str::to_string);
    let pass1 = getoptions(args, IDENTITY_OPTIONS);
    if let Some(hit) = pass1.last("identity") {
        identity = hit.value.clone();
    }
    if pass1.seen("no-identity") {
        identity = None;
    }

    let mut s = Settings::default();
    let mut configured = BTreeSet::new();
    if let Some(id) = identity.as_deref() {
        read_config(&mut s, known, Some(id), &mut configured);
    }
    read_config(&mut s, known, None, &mut configured);

    let pass2 = getoptions(&pass1.rest, DUMP_ALIASES_OPTIONS);
    let help = pass2.seen("h");
    let dump_aliases = pass2.seen("dump-aliases");
    let translate_aliases = pass2.seen("translate-aliases");

    if !help && (dump_aliases || translate_aliases) && !pass2.rest.is_empty() {
        return Err(died("--dump-aliases incompatible with other options\n"));
    }
    if !help && dump_aliases && translate_aliases {
        return Err(died("--dump-aliases and --translate-aliases are mutually exclusive\n"));
    }

    let pass3 = getoptions(&pass2.rest, OPTIONS);

    // The command line writes the same variables `read_config` filled, in the
    // order the options were spelled.
    let mut getopt_to: Vec<String> = Vec::new();
    let mut getopt_cc: Vec<String> = Vec::new();
    let mut getopt_bcc: Vec<String> = Vec::new();
    let mut no_to = false;
    let mut no_cc = false;
    let mut no_bcc = false;
    let mut no_header_cmd = false;
    let mut quiet = false;
    let mut dry_run = false;
    let mut force = false;
    // `$debug_net_smtp`, which `Net::SMTP` takes as its `Debug` level.
    let mut debug_net_smtp = false;
    let mut compose = false;
    let mut format_patch: Option<bool> = None;
    let mut initial_subject: Option<String> = None;
    let mut initial_in_reply_to: Option<String> = None;
    let mut reply_to: Option<String> = None;

    for hit in &pass3.hits {
        let val = || hit.value.clone();
        let on = !hit.negated;
        match hit.id {
            "sender" => s.sender = val(),
            "in-reply-to" => initial_in_reply_to = val(),
            "reply-to" => reply_to = val(),
            "subject" => initial_subject = val(),
            "to" => getopt_to.extend(val()),
            "to-cmd" => s.to_cmd = val(),
            "no-to" => no_to = true,
            "cc" => getopt_cc.extend(val()),
            "no-cc" => no_cc = true,
            "bcc" => getopt_bcc.extend(val()),
            "no-bcc" => no_bcc = true,
            "chain-reply-to" => s.chain_reply_to = on,
            "sendmail-cmd" => s.sendmail_cmd = val(),
            "smtp-server" => s.smtp_server = val(),
            "smtp-server-option" => s.smtp_server_options.extend(val()),
            "smtp-server-port" => s.smtp_server_port = val(),
            "smtp-user" => s.smtp.authuser = val(),
            "smtp-pass" => s.smtp.authpass = val(),
            "smtp-ssl" => s.smtp.encryption = Some("ssl".into()),
            "smtp-encryption" => s.smtp.encryption = val(),
            "smtp-ssl-cert-path" => s.smtp.ssl.cert_path = val(),
            "smtp-ssl-client-cert" => s.smtp.ssl.client_cert = val(),
            "smtp-ssl-client-key" => s.smtp.ssl.client_key = val(),
            "smtp-domain" => s.smtp.domain = val(),
            "smtp-auth" => s.smtp.auth = val(),
            "no-smtp-auth" => s.smtp.auth = Some("none".into()),
            // `"smtp-debug:i"`: the value is optional, and an omitted one is 0.
            "smtp-debug" => {
                debug_net_smtp = val().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) != 0;
            }
            "imap-sent-folder" => s.imap_sent_folder = val(),
            "use-imap-only" => s.use_imap_only = on,
            "annotate" => s.annotate = Some(on),
            "compose" => compose = true,
            "compose-encoding" => s.compose_encoding = val(),
            "quiet" => quiet = true,
            "cc-cmd" => s.cc_cmd = val(),
            "header-cmd" => s.header_cmd = val(),
            "no-header-cmd" => no_header_cmd = true,
            "suppress-from" => s.suppress_from = Some(on),
            "suppress-cc" => s.suppress_cc.extend(val()),
            "signed-off-cc" => s.signed_off_by_cc = Some(on),
            "cc-cover" => s.cover_cc = Some(on),
            "to-cover" => s.cover_to = Some(on),
            "confirm" => s.confirm = val(),
            "dry-run" => dry_run = true,
            "envelope-sender" => s.envelope_sender = val(),
            "thread" => s.thread = on,
            "validate" => s.validate = on,
            "transfer-encoding" => s.target_xfer_encoding = val().unwrap_or_default(),
            "mailmap" | "use-mailmap" => s.mailmap = on,
            "format-patch" => format_patch = Some(on),
            "8bit-encoding" => s.auto_8bit_encoding = val(),
            "force" => force = true,
            "xmailer" => s.use_xmailer = on,
            "batch-size" => s.batch_size = val(),
            "relogin-delay" => s.relogin_delay = val(),
            "outlook-id-fix" => s.smtp.outlook_id_fix = Some(on),
            _ => {}
        }
    }

    // "Munge any either config or getopt, not both variables".
    let initial_to = if !getopt_to.is_empty() {
        getopt_to
    } else if no_to {
        Vec::new()
    } else {
        s.config_to.clone()
    };
    let initial_cc = if !getopt_cc.is_empty() {
        getopt_cc
    } else if no_cc {
        Vec::new()
    } else {
        s.config_cc.clone()
    };
    let initial_bcc = if !getopt_bcc.is_empty() {
        getopt_bcc
    } else if no_bcc {
        Vec::new()
    } else {
        s.config_bcc.clone()
    };

    if help {
        return Ok(usage());
    }
    if pass3.seen("git-completion-helper") {
        return Err(Stop::Unported(
            "unsupported flag \"--git-completion-helper\": it prints this script's option list \
             unioned with `git format-patch --git-completion-helper`, which means running \
             format-patch's own completion helper"
                .into(),
        ));
    }

    if s.forbid_sendmail_variables && known.0.keys().any(|k| k.starts_with("sendmail.")) {
        return Err(died(
            "fatal: found configuration options for 'sendmail'\n\
             git-send-email is configured with the sendemail.* options - note the 'e'.\n\
             Set sendemail.forbidSendmailVariables to false to disable this check.\n",
        ));
    }

    if format_patch == Some(true) && !in_repo {
        return Err(died("Cannot run git format-patch from outside a repository\n"));
    }

    if s.relogin_delay.is_some() && s.batch_size.is_none() {
        return Err(died(
            "`batch-size` and `relogin` must be specified together (via command-line or \
             configuration option)\n",
        ));
    }

    // Set CC suppressions.
    let mut suppress: BTreeMap<String, bool> = BTreeMap::new();
    for entry in &s.suppress_cc {
        let ok = matches!(
            entry.as_str(),
            "all" | "cccmd" | "cc" | "author" | "self" | "sob" | "body" | "bodycc" | "misc-by"
        );
        if !ok {
            return Err(died(format!("Unknown --suppress-cc field: '{entry}'\n")));
        }
        suppress.insert(entry.clone(), true);
    }
    if suppress.remove("all").is_some() {
        for e in ["cccmd", "cc", "author", "self", "sob", "body", "bodycc", "misc-by"] {
            suppress.insert(e.into(), true);
        }
    }
    // The explicit old-style toggles trump --suppress-cc, and create their key
    // even when false — which is what makes %suppress_cc non-empty.
    if let Some(v) = s.suppress_from {
        suppress.insert("self".into(), v);
    }
    if let Some(v) = s.signed_off_by_cc {
        suppress.insert("sob".into(), !v);
    }
    if suppress.get("body").copied().unwrap_or(false) {
        for e in ["sob", "bodycc", "misc-by"] {
            suppress.insert(e.into(), true);
        }
    }
    suppress.remove("body");

    let confirm_unconfigured = s.confirm.is_none();
    let confirm = s
        .confirm
        .clone()
        .unwrap_or_else(|| if suppress.is_empty() { "auto".into() } else { "compose".into() });
    // The regex is a prefix match, not an exact one: `autopilot` is accepted.
    if !["auto", "cc", "compose", "always", "never"].iter().any(|p| confirm.starts_with(p)) {
        return Err(died(format!("Unknown --confirm setting: '{confirm}'\n")));
    }

    let aliases = match parse_aliases(&s) {
        AliasScan::Died(code) => return Err(Stop::Exit(code)),
        AliasScan::Parsed(aliases) => aliases,
    };
    if dump_aliases {
        // `print "$_\n" for (sort keys %aliases); exit(0);` — Perl's default
        // sort is by byte value, which is what BTreeMap iterates in.
        for alias in aliases.keys() {
            println!("{alias}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if translate_aliases {
        // `while (<STDIN>) { … }` — each line is parsed as an address list,
        // expanded through the aliases, sanitized, and printed one per line.
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
            .map_err(|e| died(format!("{e}\n")))?;
        let input = String::from_utf8_lossy(&bytes);
        for line in input.split_inclusive('\n') {
            let parsed = parse_address_line(line);
            let expanded = expand_aliases(&parsed, &aliases)
                .map_err(|a| died(format!("fatal: alias '{a}' expands to itself\n")))?;
            for addr in expanded {
                println!("{}", sanitize_address(&addr));
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".").ok();
    let mut m = Mailer {
        repo,
        s,
        aliases,
        quiet,
        dry_run,
        force,
        no_header_cmd,
        annotate: false,
        initial_subject,
        initial_in_reply_to,
        reply_to,
        initial_to,
        initial_cc,
        initial_bcc,
        suppress,
        confirm,
        confirm_unconfigured,
        sender: String::new(),
        files: Vec::new(),
        broken_encoding: BTreeSet::new(),
        time: 0,
        message_id: None,
        subject: None,
        in_reply_to: None,
        references: String::new(),
        message: Vec::new(),
        to: Vec::new(),
        cc: Vec::new(),
        xh: Vec::new(),
        needs_confirm: Needs::No,
        message_num: 0,
        ask_default: None,
        num_sent: 0,
        id_stamp: None,
        id_serial: 0,
        prompting: false,
        editor: None,
        imap_copy: Vec::new(),
        smtp: None,
        smtp_debug: debug_net_smtp,
        compose: i32::from(compose),
        compose_filename: None,
    };
    m.annotate = m.s.annotate.unwrap_or(false);

    m.files = collect_files(m.repo.as_ref(), &pass3.rest, format_patch, last_child)?;
    resolve_sender(&mut m, last_child)?;
    m.time = now_seconds() - (m.files.len() as i64 - 1);
    handle_backup_files(&mut m);

    if m.files.is_empty() {
        eprint!("\nNo patch files specified!\n\n");
        return Ok(usage());
    }
    if !m.quiet {
        for f in &m.files {
            println!("{f}");
        }
    }

    if m.compose > 0 {
        compose_message(&mut m)?;
    } else if m.annotate {
        let files = m.files.clone();
        m.do_edit(&files)?;
    }

    scan_broken_encoding(&mut m)?;

    if !m.force {
        for f in &m.files {
            let subject = get_patch_subject(f)?;
            if subject.contains("*** SUBJECT HERE ***") {
                return Err(died(format!(
                    "Refusing to send because the patch\n\t{f}\nhas the template subject \
                     '*** SUBJECT HERE ***'. Pass --force if you really want to send.\n"
                )));
            }
        }
    }

    if m.initial_to.is_empty() && m.s.to_cmd.is_none() {
        let to = ask(
            "To whom should the emails be sent (if anyone)? ",
            Some(&|r: &str| r.contains('@') && r.split('@').nth(1).is_some_and(|d| d.contains('.'))),
            Some(""),
            true,
        );
        if let Some(to) = to {
            m.initial_to.extend(parse_address_line(&to));
        }
        m.prompting = true;
    }

    let to = m.process_address_list(&m.initial_to.clone())?;
    m.initial_to = to;
    let cc = m.process_address_list(&m.initial_cc.clone())?;
    m.initial_cc = cc;
    let bcc = m.process_address_list(&m.initial_bcc.clone())?;
    m.initial_bcc = bcc;

    if m.s.thread && m.initial_in_reply_to.is_none() && m.prompting {
        m.initial_in_reply_to = ask(
            "Message-ID to be used as In-Reply-To for the first email (if any)? ",
            Some(&|r: &str| r.contains('@') && r.split('@').nth(1).is_some_and(|d| d.contains('.'))),
            Some(""),
            true,
        );
    }
    if let Some(irt) = m.initial_in_reply_to.clone() {
        let t = irt.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let t = t.strip_prefix('<').unwrap_or(t);
        let t = t.trim_end_matches(|c: char| c.is_ascii_whitespace());
        let t = t.strip_suffix('>').unwrap_or(t);
        let t = t.trim_end_matches(|c: char| c.is_ascii_whitespace());
        m.initial_in_reply_to = Some(if t.is_empty() { String::new() } else { format!("<{t}>") });
    }

    if let Some(rt) = m.reply_to.clone() {
        let rt = rt.trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
        let expanded = m.expand(&[rt])?;
        m.reply_to = Some(sanitize_address(expanded.first().map(String::as_str).unwrap_or("")));
    }

    if m.s.sendmail_cmd.is_none() && m.s.smtp_server.is_none() {
        let mut paths: Vec<String> =
            vec!["/usr/sbin/sendmail".into(), "/usr/lib/sendmail".into()];
        if let Ok(path) = std::env::var("PATH") {
            paths.extend(path.split(':').map(|d| format!("{d}/sendmail")));
        }
        m.s.sendmail_cmd = paths.into_iter().find(|p| is_executable(p));
        if m.s.sendmail_cmd.is_none() {
            m.s.smtp_server = Some("localhost".into());
        }
    }

    // `@files = ($compose_filename . ".final", @files)` — the composed message
    // leads the series, so it is what `--validate` sees first and what the rest
    // of the patches answer with `In-Reply-To:`.
    if m.compose > 0 {
        if let Some(f) = m.compose_filename.clone() {
            m.files.insert(0, format!("{f}.final"));
        }
    }

    if m.s.validate {
        validate_all(&mut m)?;
    }

    m.in_reply_to = m.initial_in_reply_to.clone();
    m.references = m.initial_in_reply_to.clone().unwrap_or_default();
    m.message_num = 0;
    for t in m.files.clone() {
        loop {
            let quiet = m.quiet;
            pre_process_file(&mut m, &t, quiet)?;
            let sent = m.send_message()?;
            if let Sent::Edit = sent {
                m.do_edit(std::slice::from_ref(&t))?;
                continue;
            }
            let was_sent = matches!(sent, Sent::Yes);
            if m.s.thread {
                if was_sent
                    && (m.s.chain_reply_to
                        || m.in_reply_to.as_deref().unwrap_or("").is_empty()
                        || m.message_num == 1)
                {
                    m.in_reply_to = m.message_id.clone();
                    let id = m.message_id.clone().unwrap_or_default();
                    if m.references.is_empty() {
                        m.references = id;
                    } else {
                        m.references.push_str(&format!("\n {id}"));
                    }
                }
            } else if m.initial_in_reply_to.is_none() {
                m.in_reply_to = None;
                m.references = String::new();
            }
            m.message_id = None;
            m.num_sent += 1;
            if let Some(bs) = m.s.batch_size.as_deref().and_then(|v| v.parse::<u64>().ok()) {
                if m.num_sent == bs {
                    // `$num_sent = 0; $smtp->quit if defined $smtp; undef $smtp;
                    // undef $auth; sleep($relogin_delay) if defined`.
                    m.num_sent = 0;
                    if let Some(session) = m.smtp.take() {
                        session.quit();
                    }
                    if let Some(d) = m.s.relogin_delay.as_deref().and_then(|v| v.parse::<u64>().ok())
                    {
                        std::thread::sleep(std::time::Duration::from_secs(d));
                    }
                }
            }
            break;
        }
    }

    m.cleanup_compose_files();

    // `$smtp->quit if $smtp;`
    if let Some(session) = m.smtp.take() {
        session.quit();
    }

    if let Some(folder) = m.s.imap_sent_folder.clone() {
        if !m.imap_copy.is_empty() && !m.dry_run {
            println!("\nStarting git imap-send...");
            let mut input: Vec<u8> = Vec::new();
            for (i, c) in m.imap_copy.iter().enumerate() {
                if i > 0 {
                    input.push(b'\n');
                }
                input.extend_from_slice(c);
            }
            let child = std::process::Command::new(self_exe())
                .args(["imap-send", "-f", &folder])
                .stdin(std::process::Stdio::piped())
                .spawn();
            let ok = match child {
                Ok(mut child) => {
                    if let Some(stdin) = child.stdin.as_mut() {
                        std::io::Write::write_all(stdin, &input).ok();
                    }
                    drop(child.stdin.take());
                    child.wait().map(|st| st.success()).unwrap_or(false)
                }
                Err(_) => false,
            };
            if !ok {
                eprintln!("Warning: failed to send messages to IMAP folder {folder}");
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Whether the path names a file with any execute bit, as Perl's `-x` reports.
fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|md| md.is_file() && md.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// `is_format_patch_arg` plus the argument loop that fills `@files`.
fn collect_files(
    repo: Option<&gix::Repository>,
    rest: &[String],
    format_patch: Option<bool>,
    last_child: &LastChild,
) -> Step<Vec<String>> {
    use std::os::unix::fs::FileTypeExt;

    let is_format_patch_arg = |f: &str| -> Step<bool> {
        // `return unless $repo`, then a `rev-parse --verify --quiet` that only
        // succeeds for something that names an object.
        let Some(repo) = repo else { return Ok(false) };
        let resolved = repo.rev_parse_single(f).is_ok();
        // That `rev-parse` is a child process, so it sets `$?`: 0 when the name
        // resolved, 1 (`--verify` failing quietly) when it did not. See [`die`].
        last_child.set(u8::from(!resolved));
        if !resolved {
            return Ok(false);
        }
        match format_patch {
            Some(v) => Ok(v),
            None => Err(died(format!(
                "File '{f}' exists but it could also be the range of commits\n\
                 to produce patches for.  Please disambiguate by...\n\
                 \n    * Saying \"./{f}\" if you mean a file; or\n\
                 \x20   * Giving --format-patch option if you mean a range.\n"
            ))),
        }
    };

    let mut files: Vec<String> = Vec::new();
    let mut rev_list_opts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let f = &rest[i];
        i += 1;
        if f == "--" {
            rev_list_opts.push(f.clone());
            rev_list_opts.extend_from_slice(&rest[i..]);
            break;
        }
        let md = std::fs::metadata(f).ok();
        if md.as_ref().is_some_and(std::fs::Metadata::is_dir) && !is_format_patch_arg(f)? {
            let mut entries: Vec<String> = std::fs::read_dir(f)
                .map_err(|e| died(format!("Failed to opendir {f}: {e}\n")))?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            entries.sort();
            for e in entries {
                let p = std::path::Path::new(f).join(&e);
                if p.is_file() {
                    files.push(p.to_string_lossy().into_owned());
                }
            }
        } else if md.as_ref().is_some_and(|md| md.is_file() || md.file_type().is_fifo())
            && !is_format_patch_arg(f)?
        {
            files.push(f.clone());
        } else {
            rev_list_opts.push(f.clone());
        }
    }

    if !rev_list_opts.is_empty() {
        if repo.is_none() {
            return Err(died("Cannot run git format-patch from outside a repository\n"));
        }
        return Err(Stop::Unported(format!(
            "unsupported: \"{}\" is a revision specification, which send-email turns into patches \
             by running `git format-patch -o <tmpdir>` and then mailing the result — pass the \
             patch files themselves instead",
            rev_list_opts.join(" ")
        )));
    }
    Ok(files)
}

/// `$sender` — `sendemail.from`/`--from`, else the repository identity, then
/// `sanitize_address`.
fn resolve_sender(m: &mut Mailer, last_child: &LastChild) -> Step<()> {
    let raw = match m.s.sender.clone() {
        Some(v) => {
            let v = v.trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
            let expanded = m.expand(&[v])?;
            expanded.first().cloned().unwrap_or_default()
        }
        None => {
            let ident = m
                .ident_person(true)
                .filter(|v| !v.is_empty())
                .or_else(|| m.ident_person(false));
            // Only this branch runs `git var GIT_AUTHOR_IDENT`
            // (`Git::ident_person`). It exits 0 when an identity is available
            // and 128 when git cannot build one. See [`die`].
            last_child.set(if ident.is_some() { 0 } else { 128 });
            ident.unwrap_or_default()
        }
    };
    m.sender = sanitize_address(&raw);
    Ok(())
}

/// `handle_backup_files` — consecutive names that differ only by a non-alnum
/// suffix are editor backups, and are confirmed once per suffix.
fn handle_backup_files(m: &mut Mailer) {
    let mut result = Vec::new();
    let mut last: Option<String> = None;
    let mut known_suffix: Option<String> = None;
    for file in m.files.clone() {
        let mut skip = false;
        if let Some(prev) = &last {
            if prev.len() < file.len() && file.starts_with(prev.as_str()) {
                let suffix = &file[prev.len()..];
                if !suffix.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                    if known_suffix.as_deref() == Some(suffix) {
                        println!(
                            "Skipping {file} with backup suffix '{}'.",
                            known_suffix.clone().unwrap_or_default()
                        );
                        skip = true;
                    } else {
                        let answer = ask(
                            &format!("Do you really want to send {file}? [y|N]: "),
                            Some(&|r: &str| {
                                let r = r.to_ascii_lowercase();
                                r.starts_with('y') || r.starts_with('n')
                            }),
                            Some("n"),
                            false,
                        )
                        .unwrap_or_default();
                        skip = answer != "y";
                        if skip {
                            known_suffix = Some(suffix.to_string());
                        }
                    }
                }
            }
        }
        last = Some(file.clone());
        if !skip {
            result.push(file);
        }
    }
    m.files = result;
}

/// `get_patch_subject` — the first `Subject:` line, `GIT: `-prefixed.
fn get_patch_subject(fname: &str) -> Step<String> {
    let text = std::fs::read(fname).unwrap_or_default();
    for line in text.split_inclusive(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(line);
        if let Some(rest) = line.strip_prefix("Subject: ") {
            return Ok(format!("GIT: {}", rest.trim_end_matches('\n')));
        }
    }
    Err(died(format!("No subject line in {fname}?\n")))
}

/// The body of the `GIT: ` comment block, before `Git::prefix_lines` runs over
/// it. Kept as the script spells it so the prefixing stays visible.
const COMPOSE_HELP: &str = "Lines beginning in \"GIT:\" will be removed.\n\
                            Consider including an overall diffstat or table of contents\n\
                            for the patch you are writing.\n\
                            \n\
                            Clear the body content if you don't wish to send a summary.\n";

/// `Git::prefix_lines` — `s/^/$prefix/mg`, which does not fire at the position
/// after a newline that ends the string.
fn prefix_lines(prefix: &str, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + prefix.len());
    for line in s.split_inclusive('\n') {
        out.push_str(prefix);
        out.push_str(line);
    }
    out
}

/// `/^<name>:\s*(.+)\s*$/i` on a line that still carries its newline.
///
/// `(.+)` cannot match a newline and needs at least one character, so `\s*`
/// gives one back when everything after the colon is whitespace: `"Cc: \n"`
/// captures a single space rather than failing. `(.+)` is greedy, so trailing
/// whitespace stays in the capture while leading whitespace does not.
fn compose_header_value(line: &str, name: &str) -> Option<String> {
    let rest = prefix_ci(line, &format!("{name}:"))?;
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }
    Some(rest.chars().next_back()?.to_string())
}

/// A `File::Temp::tempfile(".gitsendemail.msg.XXXXXX", DIR => $dir)` name.
/// Uniqueness comes from the exclusive create, not from the entropy.
fn compose_temp_file(dir: &std::path::Path) -> Step<(String, std::fs::File)> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut seed = u64::from(std::process::id())
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
    let mut last = None;
    for _ in 0..1000 {
        let mut suffix = String::new();
        for _ in 0..6 {
            // xorshift64: enough spread for six characters.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            suffix.push(ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char);
        }
        let path = dir.join(format!(".gitsendemail.msg.{suffix}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path.to_string_lossy().into_owned(), f)),
            Err(e) => last = Some((path, e)),
        }
    }
    let (path, e) = last.expect("the loop runs at least once");
    Err(died(format!(
        "Failed to open for writing {}: {}\n",
        path.display(),
        errno_text(&e)
    )))
}

/// The `if ($compose)` block: write the summary template into a temporary file
/// under the git directory, hand it to the editor, then read it back into
/// `<name>.final` with the `GIT: ` lines removed and the headers it carries
/// lifted back out into `$sender`, `@initial_to`, `@initial_cc`,
/// `@initial_bcc`, `$reply_to`, `$initial_subject` and `$initial_in_reply_to`.
///
/// An all-blank body leaves `$compose` at -1, which keeps the composed message
/// out of the series without switching `--compose` back off.
fn compose_message(m: &mut Mailer) -> Step<()> {
    let dir = match m.repo.as_ref() {
        Some(r) => r.path().to_path_buf(),
        None => std::path::PathBuf::from("."),
    };
    let (filename, mut handle) = compose_temp_file(&dir)?;
    m.compose_filename = Some(filename.clone());

    // `$sender || $repoauthor->() || $repocommitter->() || ''`. `$sender` is
    // already resolved and sanitized by this point, so it is normally the one
    // that wins.
    let tpl_sender = if m.sender.is_empty() {
        m.ident_person(true).or_else(|| m.ident_person(false)).unwrap_or_default()
    } else {
        m.sender.clone()
    };
    let tpl_subject = m.initial_subject.clone().unwrap_or_default();
    let tpl_in_reply_to = m.initial_in_reply_to.clone().unwrap_or_default();
    let tpl_reply_to = m.reply_to.clone().unwrap_or_default();
    let tpl_to = m.initial_to.join(",");
    let tpl_cc = m.initial_cc.join(",");
    let tpl_bcc = m.initial_bcc.join(", ");

    let mut template = format!("From {tpl_sender} # This line is ignored.\n");
    template.push_str(&prefix_lines("GIT: ", COMPOSE_HELP));
    template.push_str(&format!(
        "From: {tpl_sender}\n\
         To: {tpl_to}\n\
         Cc: {tpl_cc}\n\
         Bcc: {tpl_bcc}\n\
         Reply-To: {tpl_reply_to}\n\
         Subject: {tpl_subject}\n\
         In-Reply-To: {tpl_in_reply_to}\n\
         \n"
    ));
    for f in &m.files {
        template.push_str(&get_patch_subject(f)?);
        template.push('\n');
    }
    std::io::Write::write_all(&mut handle, template.as_bytes())
        .map_err(|e| died(format!("Failed to open for writing {filename}: {}\n", errno_text(&e))))?;
    drop(handle);

    // `--annotate` puts the patches in front of the same editor session.
    let mut batch = vec![filename.clone()];
    if m.annotate {
        batch.extend(m.files.iter().cloned());
    }
    m.do_edit(&batch)?;

    let final_name = format!("{filename}.final");
    let mut final_handle = std::fs::File::create(&final_name)
        .map_err(|e| died(format!("Failed to open {filename}.final: {}\n", errno_text(&e))))?;
    let composed = std::fs::read(&filename)
        .map_err(|e| died(format!("Failed to open {filename}: {}\n", errno_text(&e))))?;

    // `file_has_nonascii` over the whole file, before any line is dropped.
    let mut need_8bit_cte = composed.iter().any(|&b| b >= 0x80);
    let compose_encoding = m.s.compose_encoding.clone().unwrap_or_else(|| "UTF-8".into());
    let mut in_body = false;
    let mut summary_empty = true;
    let mut final_body: Vec<u8> = Vec::with_capacity(composed.len());

    for raw in composed.split_inclusive(|&b| b == b'\n') {
        if raw.starts_with(b"GIT:") {
            continue;
        }
        let line = String::from_utf8_lossy(raw).into_owned();
        if in_body {
            if raw != b"\n" {
                summary_empty = false;
            }
        } else if raw == b"\n" {
            in_body = true;
            if need_8bit_cte {
                final_body.extend_from_slice(
                    format!(
                        "MIME-Version: 1.0\n\
                         Content-Type: text/plain; charset={compose_encoding}\n\
                         Content-Transfer-Encoding: 8bit\n"
                    )
                    .as_bytes(),
                );
            }
        } else if starts_ci(&line, "MIME-Version:") {
            need_8bit_cte = false;
        } else if let Some(v) = compose_header_value(&line, "Subject") {
            m.initial_subject = Some(v.clone());
            // `quote_subject`'s own `shift || 'UTF-8'` means an empty setting
            // still labels the encoded word `UTF-8`, unlike the `charset=`
            // above, which takes it verbatim.
            let label =
                if compose_encoding.is_empty() { "UTF-8" } else { compose_encoding.as_str() };
            final_body
                .extend_from_slice(format!("Subject: {}\n", quote_subject(&v, label)).as_bytes());
            continue;
        } else if let Some(v) = compose_header_value(&line, "In-Reply-To") {
            m.initial_in_reply_to = Some(v);
            continue;
        } else if let Some(v) = compose_header_value(&line, "Reply-To") {
            m.reply_to = Some(v);
        } else if let Some(v) = compose_header_value(&line, "From") {
            m.sender = v;
            continue;
        } else if let Some(v) = compose_header_value(&line, "To") {
            m.initial_to = parse_address_line(&v);
            continue;
        } else if let Some(v) = compose_header_value(&line, "Cc") {
            m.initial_cc = parse_address_line(&v);
            continue;
        } else if starts_ci(&line, "Bcc:") {
            // `/^Bcc:/i` captures nothing, so the `parse_address_line($1)` the
            // script runs here is handed an undef — capture variables are scoped
            // to the loop body, so nothing an earlier line matched survives into
            // this iteration. The list is emptied, and a `--bcc` given on the
            // command line is dropped by any `Bcc:` line in the template.
            m.initial_bcc = Vec::new();
            continue;
        }
        final_body.extend_from_slice(raw);
    }

    std::io::Write::write_all(&mut final_handle, &final_body)
        .map_err(|e| died(format!("Failed to open {final_name}: {}\n", errno_text(&e))))?;
    drop(final_handle);

    if summary_empty {
        println!("Summary email is empty, skipping it");
        m.compose = -1;
    }
    Ok(())
}

/// `%broken_encoding` and the `sendemail.assume8bitEncoding` prompt.
fn scan_broken_encoding(m: &mut Mailer) -> Step<()> {
    for f in &m.files {
        let text = std::fs::read(f).map_err(|e| died(format!("unable to open {f}: {e}\n")))?;
        let mut lines = text.split_inclusive(|&b| b == b'\n');
        let mut nonascii = false;
        let mut declares_8bit = false;
        for line in lines.by_ref() {
            if line == b"\n" || line.is_empty() {
                break;
            }
            if line.starts_with(b"Subject") && line.iter().any(|&b| b >= 0x80) {
                nonascii = true;
            }
            let l = String::from_utf8_lossy(line);
            if l.starts_with("Content-Transfer-Encoding: ") && l.contains("8bit") {
                declares_8bit = true;
            }
        }
        if !nonascii {
            nonascii = lines.flatten().any(|&b| b >= 0x80);
        }
        if nonascii && !declares_8bit {
            m.broken_encoding.insert(f.clone());
        }
    }
    if m.s.auto_8bit_encoding.is_none() && !m.broken_encoding.is_empty() {
        println!("The following files are 8bit, but do not declare a Content-Transfer-Encoding.");
        for f in &m.broken_encoding {
            println!("    {f}");
        }
        // With no terminal `ask` hands back the default straight away, and
        // "UTF-8" is a charset, so the loop runs exactly once.
        let encoding = ask(
            "Declare which 8bit encoding to use [default: UTF-8]? ",
            Some(&|r: &str| !r.is_empty() && !r.bytes().any(is_ws)),
            Some("UTF-8"),
            false,
        );
        m.s.auto_8bit_encoding = encoding.or(Some("UTF-8".into()));
    }
    Ok(())
}

/// The `if ($validate)` block: the SMTP port check, then the hook and the
/// 998-character line check for every file that is not a FIFO.
fn validate_all(m: &mut Mailer) -> Step<()> {
    use std::os::unix::fs::FileTypeExt;

    let real: Vec<String> = m
        .files
        .iter()
        .filter(|f| !std::fs::metadata(f).map(|md| md.file_type().is_fifo()).unwrap_or(false))
        .cloned()
        .collect();

    if let Some(port) = m.s.smtp_server_port.clone() {
        match port_num(&port) {
            Some(p) => m.s.smtp_server_port = Some(p),
            None => return Err(died(format!("error: invalid SMTP port '{port}'\n"))),
        }
    }

    std::env::set_var("GIT_SENDEMAIL_FILE_TOTAL", real.len().to_string());
    m.in_reply_to = m.initial_in_reply_to.clone();
    m.references = m.initial_in_reply_to.clone().unwrap_or_default();
    m.message_num = 0;
    let mut result = Ok(());
    for (i, r) in real.iter().enumerate() {
        std::env::set_var("GIT_SENDEMAIL_FILE_COUNTER", (i + 1).to_string());
        result = pre_process_file(m, r, true).and_then(|()| validate_patch(m, r));
        if result.is_err() {
            break;
        }
    }
    std::env::remove_var("GIT_SENDEMAIL_FILE_COUNTER");
    std::env::remove_var("GIT_SENDEMAIL_FILE_TOTAL");
    result
}

/// `Git::port_num` — a 16-bit number, or a service name `getservbyname` knows.
pub(crate) fn port_num(port: &str) -> Option<String> {
    if let Ok(n) = port.parse::<u32>() {
        if port.bytes().all(|b| b.is_ascii_digit()) && n > 0 && n <= 65535 {
            return Some(port.to_string());
        }
    }
    let name = std::ffi::CString::new(port).ok()?;
    let ent = unsafe { libc::getservbyname(name.as_ptr(), std::ptr::null()) };
    if ent.is_null() {
        return None;
    }
    // `s_port` is in network byte order; Perl's getservbyname returns it in host
    // order, which is what the caller compares and prints.
    let port = u16::from_be(unsafe { (*ent).s_port } as u16);
    Some(port.to_string())
}

/// `validate_patch` — the `sendemail-validate` hook, then the line-length limit.
fn validate_patch(m: &mut Mailer, fname: &str) -> Step<()> {
    if let Some(repo) = &m.repo {
        let hooks_path = match repo.config_snapshot().string("core.hooksPath") {
            Some(p) => std::path::PathBuf::from(p.to_string()),
            None => repo.git_dir().join("hooks"),
        };
        let hook = hooks_path.join("sendemail-validate");
        if is_executable(&hook.to_string_lossy()) {
            let target = std::fs::canonicalize(fname)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| fname.to_string());
            let git_dir = repo.git_dir().to_path_buf();
            let work_dir = repo.workdir().unwrap_or(&git_dir).to_path_buf();
            let cwd_save = std::env::current_dir().ok();

            let (_, header) = m.gen_header()?;
            let header_file = git_dir.join(format!(".gitsendemail.header.{}", std::process::id()));
            std::fs::write(&header_file, &header)
                .map_err(|e| died(format!("Failed to open {}: {e}\n", header_file.display())))?;

            std::env::set_current_dir(&work_dir).map_err(|e| died(format!("chdir: {e}\n")))?;
            let status = std::process::Command::new(self_exe())
                .args(["hook", "run", "--ignore-missing", "sendemail-validate", "--"])
                .arg(&target)
                .arg(&header_file)
                .env("GIT_DIR", &git_dir)
                .status();
            if let Some(cwd) = cwd_save {
                std::env::set_current_dir(cwd).map_err(|e| died(format!("chdir: {e}\n")))?;
            }
            std::fs::remove_file(&header_file).ok();

            let code = match status {
                Ok(st) if st.success() => None,
                Ok(st) => Some(st.code().unwrap_or(0)),
                Err(_) => Some(1),
            };
            if let Some(code) = code {
                let cmd_msg = "git hook run --ignore-missing sendemail-validate -- <patch> <header>";
                return Err(died(format!(
                    "fatal: {fname}: rejected by sendemail-validate hook\n\
                     fatal: command '{cmd_msg}' died with exit code {code}\n\
                     warning: no patches were sent\n"
                )));
            }
        }
    }

    if !matches!(m.s.target_xfer_encoding.as_str(), "auto" | "quoted-printable" | "base64") {
        let text = std::fs::read(fname)
            .map_err(|e| died(format!("unable to open {fname}: {e}\n")))?;
        for (n, line) in text.split_inclusive(|&b| b == b'\n').enumerate() {
            if line.len() > 998 {
                return Err(died(format!(
                    "fatal: {fname}:{} is longer than 998 characters\nwarning: no patches were sent\n",
                    n + 1
                )));
            }
        }
    }
    Ok(())
}

/// `pre_process_file` — read one patch, work out its recipients, its extra
/// headers and its transfer encoding.
fn pre_process_file(m: &mut Mailer, t: &str, quiet: bool) -> Step<()> {
    let raw = std::fs::read(t).map_err(|_| died(format!("can't open file {t}\n")))?;
    let mut author: Option<String> = None;
    let mut sauthor: Option<String> = None;
    let mut author_encoding: Option<String> = None;
    let mut has_content_type = false;
    let mut body_encoding: Option<String> = None;
    let mut xfer_encoding: Option<String> = None;
    let mut has_mime_version = false;
    m.to.clear();
    m.cc.clear();
    m.xh.clear();
    m.subject = m.initial_subject.clone();
    m.message = Vec::new();
    m.message_num += 1;
    m.message_id = None;

    let mut lines = raw.split_inclusive(|&b| b == b'\n');
    let mut header_lines: Vec<String> = Vec::new();
    for line in lines.by_ref() {
        let text = String::from_utf8_lossy(line).into_owned();
        if text.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty() {
            break;
        }
        header_lines.push(text);
    }
    let mut header = unfold_headers(&header_lines);
    if !m.no_header_cmd {
        if let Some(cmd) = m.s.header_cmd.clone() {
            let extra = execute_cmd("header-cmd", &cmd, t).map_err(|e| died(format!("{e}\n")))?;
            header.extend(unfold_headers(&extra));
        }
    }

    let mut input_format: Option<&str> = None;
    for raw_line in &header {
        if raw_line.starts_with("From ") {
            input_format = Some("mbox");
            continue;
        }
        let line = raw_line.trim_end_matches('\n');
        if input_format.is_none() && is_header_line(line) {
            input_format = Some("mbox");
        }
        if input_format == Some("mbox") {
            if let Some(v) = header_value(line, "Subject") {
                m.subject = Some(v);
            } else if let Some(v) = header_value(line, "From") {
                let (a, enc) = unquote_rfc2047(&v);
                let sa = sanitize_address(&a);
                author = Some(a);
                author_encoding = enc;
                let skip = m.suppressed("author")
                    || (m.suppressed("self") && sa == m.sender);
                sauthor = Some(sa);
                if !skip {
                    if !quiet {
                        println!("(mbox) Adding cc: {v} from line '{line}'");
                    }
                    m.cc.push(v);
                }
            } else if let Some(v) = header_value(line, "To") {
                for addr in parse_address_line(&v) {
                    if !quiet {
                        println!("(mbox) Adding to: {addr} from line '{line}'");
                    }
                    m.to.push(addr);
                }
            } else if let Some(v) = header_value(line, "Cc") {
                for addr in parse_address_line(&v) {
                    let (q, _) = unquote_rfc2047(&addr);
                    let sa = sanitize_address(&q);
                    let skip =
                        if sa == m.sender { m.suppressed("self") } else { m.suppressed("cc") };
                    if skip {
                        continue;
                    }
                    if !quiet {
                        println!("(mbox) Adding cc: {addr} from line '{line}'");
                    }
                    m.cc.push(addr);
                }
            } else if starts_ci(line, "Content-type:") {
                has_content_type = true;
                if let Some(i) = line.to_ascii_lowercase().find("charset=") {
                    let rest = &line[i + 8..];
                    let rest = rest.strip_prefix('"').unwrap_or(rest);
                    let end = rest.find(['"', ' ']).unwrap_or(rest.len());
                    body_encoding = Some(rest[..end].to_string());
                }
                m.xh.push(line.to_string());
            } else if starts_ci(line, "MIME-Version") {
                has_mime_version = true;
                m.xh.push(line.to_string());
            } else if let Some(v) = prefix_ci(line, "Message-ID: ") {
                m.message_id = Some(v.to_string());
            } else if let Some(v) = prefix_ci(line, "Content-Transfer-Encoding: ") {
                xfer_encoding.get_or_insert_with(|| v.to_string());
            } else if let Some(v) = prefix_ci(line, "In-Reply-To: ") {
                if m.initial_in_reply_to.as_deref().unwrap_or("").is_empty() || m.s.thread {
                    m.in_reply_to = Some(v.to_string());
                }
            } else if let Some(v) = prefix_ci(line, "Reply-To: ") {
                m.reply_to = Some(v.to_string());
            } else if let Some(v) = prefix_ci(line, "References: ") {
                if m.initial_in_reply_to.as_deref().unwrap_or("").is_empty() || m.s.thread {
                    m.references = v.to_string();
                }
            } else if !starts_ci(line, "Date:") && is_header_line_with_value(line) {
                m.xh.push(line.to_string());
            }
        } else {
            // The traditional format: line 1 is a Cc, line 2 the subject.
            input_format = Some("lots");
            if m.cc.is_empty() && !m.suppressed("cc") {
                if !quiet {
                    println!("(non-mbox) Adding cc: {line} from line '{line}'");
                }
                m.cc.push(line.to_string());
            } else if m.subject.is_none() {
                m.subject = Some(line.to_string());
            }
        }
    }

    for line in lines {
        m.message.extend_from_slice(line);
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\n');
        let Some((what, c)) = body_cc_line(text) else { continue };
        let c = strip_garbage_one_address(&c);
        let sc = sanitize_address(&c);
        if sc == m.sender {
            if m.suppressed("self") {
                continue;
            }
        } else if what.eq_ignore_ascii_case("signed-off-by") {
            if m.suppressed("sob") {
                continue;
            }
        } else if what.to_ascii_lowercase().ends_with("-by") {
            if m.suppressed("misc-by") {
                continue;
            }
        } else if what.eq_ignore_ascii_case("cc") && m.suppressed("bodycc") {
            continue;
        }
        if !(c.contains('@') || (c.contains('<') && c.contains('>'))) {
            if !quiet {
                println!("(body) Ignoring {what} from line '{text}'");
            }
            continue;
        }
        m.cc.push(sc.clone());
        if !quiet {
            println!("(body) Adding cc: {sc} from line '{text}'");
        }
    }

    if let Some(cmd) = m.s.to_cmd.clone() {
        let more = recipients_cmd(m, "to-cmd", "to", &cmd, t, quiet)?;
        m.to.extend(more);
    }
    if let Some(cmd) = m.s.cc_cmd.clone() {
        if !m.suppressed("cccmd") {
            let more = recipients_cmd(m, "cc-cmd", "cc", &cmd, t, quiet)?;
            m.cc.extend(more);
        }
    }

    let broken = m.broken_encoding.contains(t);
    let auto8 = m.s.auto_8bit_encoding.clone().unwrap_or_default();
    if broken && !has_content_type {
        xfer_encoding.get_or_insert_with(|| "8bit".into());
        has_content_type = true;
        m.xh.push(format!("Content-Type: text/plain; charset={auto8}"));
        body_encoding = Some(auto8.clone());
    }
    if broken {
        let subject = m.subject.clone().unwrap_or_default();
        if !is_rfc2047_quoted(&subject) {
            m.subject = Some(quote_subject(&subject, &auto8));
        }
    }

    if let Some(sa) = &sauthor {
        if *sa != m.sender {
            let mut msg = format!("From: {}\n\n", author.clone().unwrap_or_default()).into_bytes();
            msg.extend_from_slice(&m.message);
            m.message = msg;
            if let Some(enc) = &author_encoding {
                if !has_content_type {
                    xfer_encoding.get_or_insert_with(|| "8bit".into());
                    m.xh.push(format!("Content-Type: text/plain; charset={enc}"));
                } else if body_encoding.as_deref() != Some(enc.as_str()) {
                    // The script notes it should re-encode here and does not.
                }
            }
        }
    }

    let from = xfer_encoding.unwrap_or_else(|| "8bit".into());
    let target = m.s.target_xfer_encoding.clone();
    let (message, encoding) =
        apply_transfer_encoding(std::mem::take(&mut m.message), &from, &target)
            .map_err(|e| died(format!("{e}\n")))?;
    m.message = message;
    m.xh.push(format!("Content-Transfer-Encoding: {encoding}"));
    if !has_mime_version {
        m.xh.insert(0, "MIME-Version: 1.0".into());
    }

    let confirm = m.confirm.as_str();
    let needs = confirm == "always"
        || (matches!(confirm, "auto" | "cc") && !m.cc.is_empty())
        || (matches!(confirm, "auto" | "compose") && m.compose != 0 && m.message_num == 1);
    m.needs_confirm = if needs {
        if m.confirm_unconfigured && !m.cc.is_empty() {
            Needs::Inform
        } else {
            Needs::Yes
        }
    } else {
        Needs::No
    };

    let processed_to = m.process_address_list(&m.to.clone())?;
    let processed_cc = m.process_address_list(&m.cc.clone())?;
    let mut to = m.initial_to.clone();
    to.extend(processed_to);
    m.to = to;
    let mut cc = m.initial_cc.clone();
    cc.extend(processed_cc);
    m.cc = cc;

    if m.message_num == 1 {
        if m.s.cover_cc == Some(true) {
            m.initial_cc = m.cc.clone();
        }
        if m.s.cover_to == Some(true) {
            m.initial_to = m.to.clone();
        }
    }
    Ok(())
}

/// `recipients_cmd` — run `sendemail.toCmd`/`sendemail.ccCmd` for one patch.
fn recipients_cmd(
    m: &Mailer,
    prefix: &str,
    what: &str,
    cmd: &str,
    file: &str,
    quiet: bool,
) -> Step<Vec<String>> {
    let lines = execute_cmd(prefix, cmd, file).map_err(|e| died(format!("{e}\n")))?;
    let mut out = Vec::new();
    for line in lines {
        let address = sanitize_address(line.trim_matches(|c: char| c.is_ascii_whitespace()));
        if address == m.sender && m.suppressed("self") {
            continue;
        }
        if !quiet {
            println!("({prefix}) Adding {what}: {address} from: '{cmd}'");
        }
        out.push(address);
    }
    Ok(out)
}

/// `/^[-A-Za-z]+:\s/`.
fn is_header_line(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b'-' || b[i].is_ascii_alphabetic()) {
        i += 1;
    }
    i > 0 && b.get(i) == Some(&b':') && b.get(i + 1).is_some_and(|&c| is_ws(c))
}

/// `/^[-A-Za-z]+:\s+\S/` — a header that carries a value.
fn is_header_line_with_value(line: &str) -> bool {
    if !is_header_line(line) {
        return false;
    }
    let rest = &line[line.find(':').unwrap_or(0) + 1..];
    rest.starts_with(|c: char| c.is_ascii_whitespace())
        && rest.trim_start_matches(|c: char| c.is_ascii_whitespace()).chars().next().is_some()
}

fn starts_ci(line: &str, prefix: &str) -> bool {
    line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    starts_ci(line, prefix).then(|| &line[prefix.len()..])
}

/// `/^(?:Subject|From|To|Cc):\s+(.*)$/i` — the value with its leading run of
/// whitespace removed.
fn header_value(line: &str, name: &str) -> Option<String> {
    let rest = prefix_ci(line, &format!("{name}:"))?;
    let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.len() == rest.len() {
        return None;
    }
    Some(trimmed.to_string())
}

/// `/^([a-z][a-z-]*-by|Cc): (.*)/i` over a body line.
fn body_cc_line(line: &str) -> Option<(String, String)> {
    let colon = line.find(": ")?;
    let name = &line[..colon];
    let value = &line[colon + 2..];
    if name.eq_ignore_ascii_case("cc") {
        return Some((name.to_string(), value.to_string()));
    }
    let b = name.as_bytes();
    if !b.first().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if !b.iter().all(|&c| c.is_ascii_alphabetic() || c == b'-') {
        return None;
    }
    name.to_ascii_lowercase().ends_with("-by").then(|| (name.to_string(), value.to_string()))
}
