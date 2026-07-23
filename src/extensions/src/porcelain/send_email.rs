//! `git send-email` — send a collection of patches as email. **The sending
//! itself is not ported: every path that would compose or transmit a message
//! bails.**
//!
//! Stock `git-send-email` is a Perl script (`git-send-email.perl`, 2474 lines in
//! git 2.55.0). Almost none of what it does is git work. It drives `Net::SMTP` /
//! `Net::SMTP::SSL` (or forks a `sendmail` binary), authenticates over SASL via
//! `Authen::SASL`, encodes headers with RFC 2047 and bodies with
//! quoted-printable/base64, parses addresses with `Mail::Address`, opens
//! `$GIT_EDITOR` for `--compose`/`--annotate`, prompts interactively for
//! `--confirm`, and delegates any revision arguments to `git format-patch`.
//! None of that has a substrate in the vendored gitoxide crates under
//! `src/ported`: there is no SMTP client, no TLS-wrapped mail transport, no SASL
//! implementation, no MIME encoder and no RFC 822 address parser. Faking the
//! transmission is not an option — the whole observable result of a real
//! `send-email` run is the bytes that reach a server.
//!
//! What *is* ported is the surface that is byte-verifiable without a mail
//! server: the three-pass `Getopt::Long` scan, the usage block, and every `die`
//! the script reaches before it needs a transport — including the complete
//! `--dump-aliases` path, which is pure file parsing. All output below was
//! captured from git 2.55.0 on Darwin.
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
//! * `read_config`: `sendemail.<identity>.*` before `sendemail.*`, first prefix
//!   wins per setting, `sendemail.identity` yielding to `--identity` and
//!   `--no-identity`, list-valued keys (`aliasesfile`, `suppresscc`) taking
//!   every value, scalar keys taking the last.
//! * `--dump-aliases`: the alias files named by `sendemail.aliasesfile` are read
//!   with the parser named by `sendemail.aliasfiletype`, and the alias names are
//!   printed one per line in byte order, exit 0. All six parsers are
//!   reproduced (`mutt`, `mailrc`, `pine`, `elm`, `sendmail`, `gnus`), including
//!   sendmail's line-continuation rules and its four `warning:` lines on stderr.
//!   An unset or unrecognised `aliasfiletype` yields no aliases, as in the
//!   script. A file that cannot be opened produces `opening <file>: <errno>` and
//!   exits with the errno, as Perl's `die` does.
//!
//! ### Exit status of the `die` paths
//!
//! Perl exits a `die` with `$! || ($? >> 8) || 255`. For this script the last
//! subprocess before any of these `die`s is the `git config --null --get-regexp
//! ^sende?mail[.]` in `config_regexp`, which exits 1 when it matches nothing (or
//! when there is no repository) and 0 otherwise. So every `die` above exits 1 if
//! no `sende?mail.*` key is set anywhere and 255 if any is — which is what this
//! module computes. `usage()` calls `exit(1)` explicitly and is not affected.
//!
//! ### Not covered
//!
//! * Everything from `is_format_patch_arg` onwards: delegating to
//!   `git format-patch`, reading patches, `--compose`/`--annotate` editor
//!   sessions, `--confirm` prompting, `--validate` hook runs, header
//!   construction, MIME/RFC 2047 encoding, `--dry-run`, and the SMTP or
//!   `sendmail` transport. These bail, naming the missing substrate.
//! * `--translate-aliases`, which needs `Mail::Address` parsing and
//!   `sanitize_address_list` (RFC 822 phrase quoting against the configured
//!   sender). It bails.
//! * `--git-completion-helper`, which shells out to
//!   `git format-patch --git-completion-helper` and prints the union of both
//!   option lists. It bails.
//! * `%(prefix)` and `~user/` interpolation in `sendemail.aliasesfile`. A
//!   leading `~/` is expanded from `$HOME`; other forms are used verbatim.
//! * The `pine` parser matches on tab-delimited field structure rather than by
//!   backtracking the original regex character for character; the alias name it
//!   yields is the first field, which is all `--dump-aliases` prints.

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

    /// Every value stored for `id`, in order, as Perl's array assignment leaves it.
    fn all(&self, id: &str) -> Vec<&str> {
        self.hits.iter().filter(|h| h.id == id).filter_map(|h| h.value.as_deref()).collect()
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

/// The settings `read_config` fills in that anything reachable here depends on.
/// The rest of `%config_settings` steers composition and transport, which bail.
#[derive(Default)]
struct Config {
    alias_files: Vec<String>,
    alias_file_type: Option<String>,
    forbid_sendmail_variables: bool,
    batch_size: Option<String>,
    relogin_delay: Option<String>,
    suppress_cc: Vec<String>,
    confirm: Option<String>,
    /// Whether `sendemail.suppressfrom` or `sendemail.signedoffbycc` was set;
    /// either creates a key in `%suppress_cc` and so changes `--confirm`'s
    /// default from `auto` to `compose`.
    suppress_toggles: bool,
}

/// `read_config(\%known_config_keys, \%configured, $prefix)`, called for
/// `sendemail.<identity>` and then `sendemail`. `%configured` is what makes the
/// first prefix win.
fn read_config(cfg: &mut Config, known: &Known, prefix: &str, configured: &mut BTreeSet<&'static str>) {
    // %config_bool_settings.
    if let Some(v) = known.boolean(&format!("{prefix}.forbidsendmailvariables")) {
        if configured.insert("forbidsendmailvariables") {
            cfg.forbid_sendmail_variables = v;
        }
    }
    for setting in ["suppressfrom", "signedoffbycc", "signedoffcc"] {
        if known.exists(&format!("{prefix}.{setting}")) {
            cfg.suppress_toggles = true;
        }
    }

    // %config_path_settings: aliasesfile is list valued.
    if let Some(values) = known.0.get(&format!("{prefix}.aliasesfile")) {
        if !values.is_empty() && configured.insert("aliasesfile") {
            cfg.alias_files = values.iter().map(|v| expand_path(v)).collect();
        }
    }

    // %config_settings: scalars take the last value, lists take all of them.
    for (setting, target) in [
        ("aliasfiletype", &mut cfg.alias_file_type),
        ("smtpbatchsize", &mut cfg.batch_size),
        ("smtprelogindelay", &mut cfg.relogin_delay),
        ("confirm", &mut cfg.confirm),
    ] {
        let Some(v) = known.last(&format!("{prefix}.{setting}")) else { continue };
        if configured.insert(setting) {
            *target = Some(v.to_string());
        }
    }

    if known.exists(&format!("{prefix}.suppresscc")) && configured.insert("suppresscc") {
        cfg.suppress_cc = known.0.get(&format!("{prefix}.suppresscc")).cloned().unwrap_or_default();
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
/// the alias name.
fn mutt_alias(line: &[u8]) -> Option<String> {
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
        if skip_ws(line, end) == end {
            continue;
        }
        return Some(String::from_utf8_lossy(&line[start..end]).into_owned());
    }
    None
}

/// `/^alias\s+(\S+)\s+(.*?)\s*$/` — the mailrc parser.
fn mailrc_alias(line: &[u8]) -> Option<String> {
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
    if end == i || skip_ws(line, end) == end {
        return None;
    }
    Some(String::from_utf8_lossy(&line[i..end]).into_owned())
}

/// `/^(\S+)\s+=\s+[^=]+=\s(\S+)/` — the elm parser.
fn elm_alias(line: &[u8]) -> Option<String> {
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
    Some(String::from_utf8_lossy(&line[..end]).into_owned())
}

/// `/\(define-mail-alias\s+"(\S+?)"\s+"(\S+?)"\)/` — the gnus parser. The
/// pattern is unanchored, so it is searched for anywhere in the line.
fn gnus_alias(line: &[u8]) -> Option<String> {
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
fn gnus_at(line: &[u8], mut i: usize) -> Option<String> {
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
    let (_addr, next) = quoted_word(line, i)?;
    if line.get(next) != Some(&b')') {
        return None;
    }
    Some(name)
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
fn sendmail_alias(logical: &[u8], aliases: &mut BTreeSet<String>) {
    let text = String::from_utf8_lossy(logical);
    if text.contains('"') {
        eprintln!("warning: sendmail alias with quotes is not supported: {text}");
    } else if text.contains(":include:") {
        eprintln!("warning: `:include:` not supported: {text}");
    } else if text.contains('/') || text.contains('|') {
        eprintln!("warning: `/file` or `|pipe` redirection not supported: {text}");
    } else if let Some(name) = sendmail_name(logical) {
        aliases.insert(name);
    } else {
        eprintln!("warning: sendmail line is not recognized: {text}");
    }
}

/// `/^(\S+?)\s*:\s*(.+)$/`. `\S+?` is non-greedy, so the shortest prefix that
/// lands on a colon with a non-empty remainder wins.
fn sendmail_name(line: &[u8]) -> Option<String> {
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
        return Some(String::from_utf8_lossy(&line[..len]).into_owned());
    }
    None
}

/// `parse_sendmail_aliases` — blank and `#` lines are dropped, a trailing `\`
/// or a leading blank on the next line continues the current one.
fn parse_sendmail(text: &[u8], aliases: &mut BTreeSet<String>) {
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
/// the alias is the first field.
fn parse_pine(text: &[u8], aliases: &mut BTreeSet<String>) {
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
        aliases.insert(String::from_utf8_lossy(fields[0]).into_owned());
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
fn parse_line_oriented(text: &[u8], aliases: &mut BTreeSet<String>, f: fn(&[u8]) -> Option<String>) {
    for mut line in chomped_lines(text) {
        line.push(b'\n');
        if let Some(name) = f(&line) {
            aliases.insert(name);
        }
    }
}

/// The result of the alias-file scan: either the alias names, or the exit code
/// of a `die` raised while opening one of the files.
enum Aliases {
    Names(BTreeSet<String>),
    Died(u8),
}

/// `%parse_alias` — dispatch on `sendemail.aliasfiletype`. An unset or unknown
/// type leaves the files unread, as the
/// `if (@alias_files and $aliasfiletype and defined $parse_alias{$aliasfiletype})`
/// guard in the script does, and `%aliases` stays empty.
fn parse_aliases(cfg: &Config) -> Aliases {
    let mut aliases = BTreeSet::new();
    let Some(file_type) = cfg.alias_file_type.as_deref() else { return Aliases::Names(aliases) };
    if cfg.alias_files.is_empty() {
        return Aliases::Names(aliases);
    }
    if !matches!(file_type, "mutt" | "mailrc" | "pine" | "elm" | "sendmail" | "gnus") {
        return Aliases::Names(aliases);
    }

    for file in &cfg.alias_files {
        let text = match std::fs::read(file) {
            Ok(text) => text,
            Err(e) => {
                // Perl: `die "opening $file: $!\n"`, and `die` exits with `$!`.
                eprintln!("opening {file}: {}", errno_text(&e));
                let code = u8::try_from(e.raw_os_error().unwrap_or(255)).unwrap_or(255);
                return Aliases::Died(code);
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
    Aliases::Names(aliases)
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
// Entry point
// ---------------------------------------------------------------------------

/// `usage()` — the block on stdout, `exit(1)`.
fn usage() -> ExitCode {
    print!("{USAGE}");
    ExitCode::from(1)
}

/// Perl's `die` status here: `$? >> 8` from the last `git config --get-regexp`,
/// which is 1 when nothing matched and 0 otherwise, falling back to 255.
fn die(msg: &str, known: &Known) -> ExitCode {
    eprint!("{msg}");
    ExitCode::from(if known.0.is_empty() { 1 } else { 255 })
}

/// `git send-email` — send a collection of patches as email.
///
/// Reproduces the script's three option passes and every path that terminates
/// before a transport is needed: `-h`, the `--dump-aliases` incompatibility
/// checks, the `sendmail.*` configuration check, the `--format-patch`,
/// `--batch-size`, `--suppress-cc` and `--confirm` validations, and the whole
/// `--dump-aliases` alias-file scan. Composing or sending anything bails,
/// naming the missing substrate.
pub fn send_email(args: &[String]) -> Result<ExitCode> {
    let (config_file, in_repo) = load_config();
    let known = known_keys(config_file.as_ref());

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

    let mut cfg = Config { forbid_sendmail_variables: true, ..Config::default() };
    let mut configured = BTreeSet::new();
    if let Some(id) = identity.as_deref() {
        read_config(&mut cfg, &known, &format!("sendemail.{id}"), &mut configured);
    }
    read_config(&mut cfg, &known, "sendemail", &mut configured);

    let pass2 = getoptions(&pass1.rest, DUMP_ALIASES_OPTIONS);
    let help = pass2.seen("h");
    let dump_aliases = pass2.seen("dump-aliases");
    let translate_aliases = pass2.seen("translate-aliases");

    if !help && (dump_aliases || translate_aliases) && !pass2.rest.is_empty() {
        return Ok(die("--dump-aliases incompatible with other options\n", &known));
    }
    if !help && dump_aliases && translate_aliases {
        return Ok(die("--dump-aliases and --translate-aliases are mutually exclusive\n", &known));
    }

    let pass3 = getoptions(&pass2.rest, OPTIONS);

    if help {
        return Ok(usage());
    }
    if pass3.seen("git-completion-helper") {
        bail!(
            "unsupported flag \"--git-completion-helper\": it prints this script's option list \
             unioned with `git format-patch --git-completion-helper`, which means running \
             format-patch's own completion helper (ported: option parsing, -h, --dump-aliases)"
        );
    }

    if cfg.forbid_sendmail_variables && known.0.keys().any(|k| k.starts_with("sendmail.")) {
        return Ok(die(
            "fatal: found configuration options for 'sendmail'\n\
             git-send-email is configured with the sendemail.* options - note the 'e'.\n\
             Set sendemail.forbidSendmailVariables to false to disable this check.\n",
            &known,
        ));
    }

    // `$format_patch` is only true for `--format-patch`; `--no-format-patch`
    // sets it to 0, which is falsy and so does not trigger this.
    let format_patch = pass3.last("format-patch").map(|h| !h.negated).unwrap_or(false);
    if format_patch && !in_repo {
        return Ok(die("Cannot run git format-patch from outside a repository\n", &known));
    }

    let batch_size = pass3.last("batch-size").and_then(|h| h.value.clone()).or(cfg.batch_size.clone());
    let relogin_delay =
        pass3.last("relogin-delay").and_then(|h| h.value.clone()).or(cfg.relogin_delay.clone());
    if relogin_delay.is_some() && batch_size.is_none() {
        return Ok(die(
            "`batch-size` and `relogin` must be specified together (via command-line or \
             configuration option)\n",
            &known,
        ));
    }

    // @suppress_cc is the command line's if given, else the config's.
    let cli_suppress: Vec<String> = pass3.all("suppress-cc").into_iter().map(str::to_string).collect();
    let suppress_cc = if cli_suppress.is_empty() { cfg.suppress_cc.clone() } else { cli_suppress };
    let mut suppress_keys: BTreeSet<String> = BTreeSet::new();
    for entry in &suppress_cc {
        let ok = matches!(
            entry.as_str(),
            "all" | "cccmd" | "cc" | "author" | "self" | "sob" | "body" | "bodycc" | "misc-by"
        );
        if !ok {
            return Ok(die(&format!("Unknown --suppress-cc field: '{entry}'\n"), &known));
        }
        suppress_keys.insert(entry.clone());
    }
    // `$suppress_cc{'self'} = …` and `$suppress_cc{'sob'} = …` create their keys
    // even when the value is false, which is what makes %suppress_cc non-empty
    // and flips --confirm's default to 'compose'.
    if pass3.seen("suppress-from") || pass3.seen("signed-off-cc") || cfg.suppress_toggles {
        suppress_keys.insert("self".into());
    }

    let confirm = pass3
        .last("confirm")
        .and_then(|h| h.value.clone())
        .or(cfg.confirm.clone())
        .unwrap_or_else(|| {
            if suppress_keys.is_empty() { "auto".into() } else { "compose".into() }
        });
    // The regex is a prefix match, not an exact one: `autopilot` is accepted.
    let confirm_ok = ["auto", "cc", "compose", "always", "never"].iter().any(|p| confirm.starts_with(p));
    if !confirm_ok {
        return Ok(die(&format!("Unknown --confirm setting: '{confirm}'\n"), &known));
    }

    let aliases = match parse_aliases(&cfg) {
        Aliases::Died(code) => return Ok(ExitCode::from(code)),
        Aliases::Names(names) => names,
    };
    if dump_aliases {
        // `print "$_\n" for (sort keys %aliases); exit(0);` — Perl's default
        // sort is by byte value, which is what BTreeSet iterates in.
        for alias in &aliases {
            println!("{alias}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if translate_aliases {
        bail!(
            "unsupported flag \"--translate-aliases\": it needs RFC 822 address parsing \
             (Mail::Address) and sanitize_address_list's phrase quoting, neither of which has a \
             substrate in the vendored gitoxide crates (ported: option parsing, -h, config \
             validation, --dump-aliases)"
        );
    }

    bail!(
        "unsupported: sending patches needs an SMTP client (Net::SMTP, TLS, SASL) or a sendmail \
         binary, RFC 2047 header and MIME body encoding, Mail::Address parsing, an editor session \
         for --compose/--annotate, and delegation to git format-patch — none of which has a \
         substrate in the vendored gitoxide crates (ported: option parsing, -h, the config and \
         validation diagnostics, --dump-aliases)"
    );
}
