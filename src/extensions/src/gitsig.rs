//! Commit signature extraction and verification — the substrate behind the
//! `%G?` / `%GK` pretty-format placeholders and `git zsigs`.
//!
//! gitoxide hands us the raw signature (`CommitRef::pgp_signature`) but does not
//! reconstruct the *signed payload* or verify anything (its own source flags that
//! as "quite some work"). So this module does two things:
//!
//!  1. [`split_signed`] — split a raw commit object into `(signature, payload)`
//!     by removing the `gpgsig` header and de-folding it, exactly as git does
//!     before it hands both to the crypto tool. Pure and deterministic.
//!  2. [`verify_full`] — `check_signature()`: pick the backend the signature's
//!     own armor header names (`gpg`, `gpgsm`, or `ssh-keygen -Y`), run it with
//!     git's argument vector, and fill in git's `struct signature_check` from
//!     what it said. When the tool is absent or says nothing conclusive the
//!     result stays at `check_signature()`'s initial `N` — nothing was checked,
//!     so nothing is claimed — never a fabricated "good".
//!  3. [`Signer`] — `sign_buffer()`: the `gpg.format` table, its per-format
//!     program slots ([`gpg_programs`]), `get_signing_key()` including
//!     `gpg.ssh.defaultKeyCommand`, and the openpgp/x509/ssh signers — for the
//!     callers that produce a signature rather than check one. Success there is
//!     the backend's own report (`[GNUPG:] SIG_CREATED`, a written `.sig` file),
//!     never a zero exit status: gpg has paths that exit 0 without signing, and
//!     trusting the status code turns one into a `gpgsig` header full of
//!     whatever it printed.
//!
//! Both of the checker's streams are kept, because they are the answer for
//! several callers rather than an implementation detail: `verify-tag`,
//! `verify-commit` and `%(signature)` all print gpg's or ssh-keygen's own report
//! verbatim, so this module runs those programs with exactly the arguments git
//! runs them with and relays what comes back untouched.
//!
//! Nothing here shells out to `git`; verification is delegated to
//! gpg/gpgsm/ssh-keygen, which is what git itself does (git has no in-process
//! crypto either).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// A commit's signature status, mapped to git's `%G?` single-character codes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GStatus {
    /// `N` — no signature on the commit, and `check_signature()`'s starting
    /// value: a backend that reported nothing conclusive leaves it here.
    #[default]
    NoSignature,
    /// `G` — good signature from a fully/ultimately trusted key.
    Good,
    /// `U` — good signature, key of unknown validity.
    GoodUnknown,
    /// `X` — good signature that has expired.
    Expired,
    /// `Y` — good signature made by an expired key.
    KeyExpired,
    /// `R` — good signature made by a revoked key.
    Revoked,
    /// `B` — bad signature.
    Bad,
    /// `E` — a signature is present but could not be checked (tool missing, no
    /// public key, unsupported format). git's own fallback.
    CannotCheck,
}

impl GStatus {
    /// git's `%G?` character.
    pub fn code(self) -> char {
        match self {
            GStatus::NoSignature => 'N',
            GStatus::Good => 'G',
            GStatus::GoodUnknown => 'U',
            GStatus::Expired => 'X',
            GStatus::KeyExpired => 'Y',
            GStatus::Revoked => 'R',
            GStatus::Bad => 'B',
            GStatus::CannotCheck => 'E',
        }
    }

    /// Whether a signature is present at all (anything but `N`).
    pub fn is_signed(self) -> bool {
        self != GStatus::NoSignature
    }

    /// Whether the signature verified as good (`G` or `U`).
    pub fn is_good(self) -> bool {
        matches!(self, GStatus::Good | GStatus::GoodUnknown)
    }
}

/// Split a raw commit object into `(signature, signed_payload)`, or `None` when
/// the commit carries no `gpgsig` header.
///
/// git signs the commit object with its `gpgsig` header removed; the header value
/// is line-folded (each continuation line prefixed with one space). This removes
/// the whole `gpgsig` block from the payload and de-folds the signature, byte for
/// byte, so the pair round-trips through gpg/ssh-keygen identically to git.
pub fn split_signed(raw: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut payload = Vec::with_capacity(raw.len());
    let mut sig = Vec::new();
    let mut found = false;
    let mut in_headers = true;
    let mut i = 0;
    while i < raw.len() {
        let nl = line_end(raw, i);
        let line = &raw[i..nl]; // includes trailing '\n' if present
        let content = line.strip_suffix(b"\n").unwrap_or(line);

        if in_headers && content.is_empty() {
            // Blank line: end of headers. Copy it and everything after verbatim.
            in_headers = false;
            payload.extend_from_slice(line);
            i = nl;
            continue;
        }
        if in_headers && content.starts_with(b"gpgsig ") {
            found = true;
            sig.extend_from_slice(&content[b"gpgsig ".len()..]);
            sig.push(b'\n');
            i = nl;
            // De-fold continuation lines (leading space), excluding them from the payload.
            while i < raw.len() {
                let nl2 = line_end(raw, i);
                let l2 = &raw[i..nl2];
                if l2.first() == Some(&b' ') {
                    let c2 = l2.strip_suffix(b"\n").unwrap_or(l2);
                    sig.extend_from_slice(&c2[1..]);
                    sig.push(b'\n');
                    i = nl2;
                } else {
                    break;
                }
            }
            continue; // gpgsig block omitted from the payload
        }
        payload.extend_from_slice(line);
        i = nl;
    }
    found.then_some((sig, payload))
}

/// Index just past the newline ending the line at `start` (or end of slice).
fn line_end(raw: &[u8], start: usize) -> usize {
    raw[start..].iter().position(|&b| b == b'\n').map(|p| start + p + 1).unwrap_or(raw.len())
}

/// Evaluate a raw commit object's signature: `(status, key)`. `key` is the gpg
/// key id / fingerprint (or SSH principal) when known, else empty.
pub fn evaluate(raw: &[u8]) -> (GStatus, String) {
    match split_signed(raw) {
        None => (GStatus::NoSignature, String::new()),
        Some((sig, payload)) => verify(&sig, &payload),
    }
}

/// Verify a `(signature, payload)` pair via the same tools git uses, reporting
/// the `%G?` character.
pub fn verify(sig: &[u8], payload: &[u8]) -> (GStatus, String) {
    let check = verify_full(sig, payload);
    (check.pretty_status(), check.key)
}

/// git's `enum signature_trust_level`, in the same order — the scale
/// `gpg.minTrustLevel` and `verify_merge_signature`'s `TRUST_MARGINAL` floor are
/// compared against. `TRUST_UNDEFINED` is the value `check_signature()` starts
/// from, so an unparsed / absent `TRUST_*` status reads as undefined.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Trust {
    #[default]
    Undefined,
    Never,
    Marginal,
    Fully,
    Ultimate,
}

/// `parse_gpg_trust_level()` (gpg-interface.c:221): a level name as gpg emits it
/// in its status stream, and as git stores it after upper-casing the config
/// value.
pub fn trust_level_from_key(key: &str) -> Option<Trust> {
    match key {
        "UNDEFINED" => Some(Trust::Undefined),
        "NEVER" => Some(Trust::Never),
        "MARGINAL" => Some(Trust::Marginal),
        "FULLY" => Some(Trust::Fully),
        "ULTIMATE" => Some(Trust::Ultimate),
        _ => None,
    }
}

/// `git_gpg_config()`'s `gpg.format` check (gpg-interface.c:773-780):
///
/// ```c
/// fmt = get_format_by_name(value);
/// if (!fmt) return error(_("invalid value for '%s': '%s'"), var, value);
/// ```
///
/// `Err` carries the offending value. Like every other key the function reads,
/// this is reached through `gpg_interface_lazy_init()` — the *first* sign or
/// verify, not startup — so a repository carrying a bad value still runs
/// `git status` and `git log` without a word.
pub fn validate_format(repo: &gix::Repository) -> Result<(), String> {
    match repo.config_snapshot().string("gpg.format") {
        None => Ok(()),
        Some(value) => {
            let value = value.to_string();
            match SigFormat::from_name(&value) {
                Some(_) => Ok(()),
                None => Err(value),
            }
        }
    }
}

/// gpg-interface.c's `configured_min_trust_level` file static, as
/// `git_gpg_config()` fills it from `gpg.minTrustLevel`.
///
/// Unset is git's initial `TRUST_UNDEFINED` — the lowest level, so it never
/// rejects anything. `Err` carries the offending value for the caller's
/// `invalid value for 'gpg.mintrustlevel'` diagnostic.
pub fn configured_min_trust_level(repo: &gix::Repository) -> Result<Trust, String> {
    match repo.config_snapshot().string("gpg.minTrustLevel") {
        None => Ok(Trust::Undefined),
        Some(value) => {
            let value = value.to_string();
            trust_level_from_key(&value.to_uppercase()).ok_or(value)
        }
    }
}

/// git's `struct signature_check`.
#[derive(Default)]
pub struct SigCheck {
    /// `sigc->result` — `G`/`B`/`E`/`N` as the backend reported it. This is the
    /// raw verdict, NOT the `%G?` character: git never stores `U` here, it derives
    /// it from the trust level at format time (see [`SigCheck::pretty_status`]).
    /// `GIT_PUSH_CERT_STATUS` and `verify_merge_signature` read this field.
    ///
    /// `check_signature()` initialises it to `'N'` (gpg-interface.c:669) and only
    /// a status line the table names overwrites it, so a backend that said
    /// nothing conclusive — or that could not be run at all — leaves `'N'` here
    /// rather than `'E'`. `'E'` is reserved for what git reserves it for: an
    /// `ERRSIG` line, or two mutually exclusive `*SIG` lines in one stream.
    pub status: GStatus,
    /// `sigc->key` — the signing key's long id.
    pub key: String,
    /// `sigc->signer` — the user name gpg's `GOODSIG`/`BADSIG`/… line names after
    /// the key id. This is what git's `Commit %s has a good GPG signature by %s`
    /// diagnostics quote, not the key id.
    pub signer: String,
    /// `sigc->trust_level`, from the `TRUST_*` status line.
    pub trust: Trust,
    /// `sigc->fingerprint` — the `VALIDSIG` line's first field.
    pub fingerprint: String,
    /// `sigc->primary_key_fingerprint` — `VALIDSIG`'s tenth field, which only
    /// OpenPGP signatures carry.
    pub primary_key_fingerprint: String,
    /// `sigc->output` — the checker's own human-readable report, kept verbatim.
    /// This is gpg's/gpgsm's **stderr**, and for the ssh backend the collected
    /// `ssh-keygen` output; `print_signature_buffer()` writes it to stderr and
    /// `%(signature)` renders it.
    pub output: Vec<u8>,
    /// `sigc->gpg_status` — the machine-readable `--status-fd` stream, which
    /// `--raw` prints instead of [`output`][Self::output].
    pub gpg_status: Vec<u8>,
    /// `verify_signed_buffer()`'s return value, inverted: whether the backend
    /// itself was satisfied. `check_signature()` folds it together with the
    /// result character and the trust floor (see [`SigCheck::verified`]).
    pub backend_ok: bool,
}

impl SigCheck {
    /// The `%G?` character, which is `sigc->result` with one substitution: a good
    /// signature whose trust level is undefined or never prints `U` instead of `G`
    /// (pretty.c's `'G'` case). The fold lives here rather than in
    /// [`status`][Self::status] because git's `sigc->result` never holds `U` — a
    /// good signature by an untrusted or unknown key is still a good signature,
    /// which is what `GIT_PUSH_CERT_STATUS` reports.
    pub fn pretty_status(&self) -> GStatus {
        match self.status {
            GStatus::Good if self.trust < Trust::Marginal => GStatus::GoodUnknown,
            other => other,
        }
    }

    /// `check_signature()`'s verdict (gpg-interface.c:679-687), as a success flag:
    ///
    /// ```c
    /// status |= sigc->result != 'G' && sigc->result != 'Y';
    /// status |= sigc->trust_level < configured_min_trust_level;
    /// ```
    ///
    /// `'Y'` (a good signature by an expired key) counts as verified, which is why
    /// this is not `status.is_good()`.
    pub fn verified(&self, min_trust: Trust) -> bool {
        self.backend_ok
            && matches!(self.status, GStatus::Good | GStatus::KeyExpired)
            && self.trust >= min_trust
    }
}

/// [`evaluate`] with git's full `signature_check` result — the signer name and
/// trust level that `verify_merge_signature()` needs and `%G?` alone cannot
/// carry. Same gpg invocation as [`verify`]; no second verification path.
pub fn evaluate_full(raw: &[u8]) -> SigCheck {
    match split_signed(raw) {
        None => SigCheck::default(),
        Some((sig, payload)) => verify_full(&sig, &payload),
    }
}

/// `check_signature()` (gpg-interface.c:661): pick the backend the signature's
/// armor names and let it judge the pair.
///
/// The format comes from the *signature*, not from `gpg.format` — that setting
/// only chooses what to sign **with** — so a repository configured for ssh still
/// verifies an OpenPGP tag through gpg.
pub fn verify_full(sig: &[u8], payload: &[u8]) -> SigCheck {
    match format_by_sig(sig) {
        // `verify_ssh_signed_buffer()`.
        Some(SigFormat::Ssh) => verify_ssh(sig, payload),
        // `verify_gpg_signed_buffer()` serves openpgp and x509 alike; only the
        // program and its extra arguments differ (gpg-interface.c:92-124).
        Some(format) => verify_gpg(format, sig, payload),
        // git's `die(_("bad/incompatible signature '%s'"))`. The callers here all
        // reached this through a `gpgsig` header or a tag signature block, both of
        // which only exist because one of the markers matched, so this is the
        // unreachable arm rather than a verdict.
        None => SigCheck::default(),
    }
}

/// `get_format_by_sig()` (gpg-interface.c:135): the armor header picks the
/// backend, scanning the `gpg_format[]` table in order.
pub fn format_by_sig(sig: &[u8]) -> Option<SigFormat> {
    const SIGS: &[(&[u8], SigFormat)] = &[
        (b"-----BEGIN PGP SIGNATURE-----", SigFormat::OpenPgp),
        (b"-----BEGIN PGP MESSAGE-----", SigFormat::OpenPgp),
        (b"-----BEGIN SIGNED MESSAGE-----", SigFormat::X509),
        (b"-----BEGIN SSH SIGNATURE-----", SigFormat::Ssh),
    ];
    SIGS.iter().find(|(marker, _)| sig.starts_with(marker)).map(|&(_, f)| f)
}

/// `verify_gpg_signed_buffer()` (gpg-interface.c:349): write the signature to a
/// temp file and run `<program> [<verify-args>] --status-fd=1 --verify <file> -`
/// with the payload on stdin, keeping both of the child's streams.
///
/// The argument vector is git's exactly, because the child's stderr *is* the
/// output `verify-tag` and `%(signature)` print — a stray `--batch`/`--no-tty`
/// of our own would change the very bytes that have to match.
fn verify_gpg(format: SigFormat, sig: &[u8], payload: &[u8]) -> SigCheck {
    // `mks_tempfile_t(".git_vtag_tmpXXXXXX")`.
    let Some(sigfile) = write_temp("vtag", sig) else { return SigCheck::default() };

    let mut args: Vec<&std::ffi::OsStr> = Vec::new();
    let os = std::ffi::OsStr::new;
    // `fmt->verify_args`: only the openpgp entry has any.
    if format == SigFormat::OpenPgp {
        args.push(os("--keyid-format=long"));
    }
    args.extend([os("--status-fd=1"), os("--verify"), sigfile.as_os_str(), os("-")]);

    let out = run_with_stdin(&verify_program(format), &args, payload);
    let _ = std::fs::remove_file(&sigfile);

    let Some(out) = out else { return SigCheck::default() };

    // `ret |= !strstr(gpg_stdout.buf, "\n[GNUPG:] GOODSIG ") && !strstr(…EXPKEYSIG …)`.
    let has = |needle: &[u8]| {
        out.stdout.windows(needle.len()).any(|w| w == needle)
    };
    let backend_ok = out.status.success()
        && (has(b"\n[GNUPG:] GOODSIG ") || has(b"\n[GNUPG:] EXPKEYSIG "));

    let mut check = parse_gpg_status_full(&out.stdout);
    check.output = out.stderr;
    check.gpg_status = out.stdout;
    check.backend_ok = backend_ok;
    check
}

/// `git_gpg_config()`'s program dispatch (gpg-interface.c:788-807), which mutates
/// the `gpg_format[]` *table* rather than answering a lookup:
///
/// ```c
/// if (!strcmp(var, "gpg.program") || !strcmp(var, "gpg.openpgp.program"))
///         fmtname = "openpgp";
/// if (!strcmp(var, "gpg.x509.program"))   fmtname = "x509";
/// if (!strcmp(var, "gpg.ssh.program"))    fmtname = "ssh";
/// if (fmtname) { ... fmt->program = program; }
/// ```
///
/// Three consequences that a per-key lookup gets wrong, and that this reproduces:
///
///   * `gpg.program` names the **openpgp** entry only. An `x509` signer with just
///     `gpg.program` set still runs `gpgsm`; the key that reaches it is
///     `gpg.x509.program`.
///   * `gpg.openpgp.program` is not "more specific" than `gpg.program` — both aim
///     at the same table slot, so the one the config reader reaches *last* wins.
///     That is a positional rule, which is why this walks the config in order.
///   * every slot falls back to the table's own `.program` (`gpg`, `gpgsm`,
///     `ssh-keygen`), never to another slot's value.
///
/// Returns the three programs in `SigFormat` order: openpgp, x509, ssh.
pub fn gpg_programs(repo: &gix::Repository) -> [String; 3] {
    // Which key last claimed each slot, in config order.
    let mut winner: [Option<String>; 3] = [None, None, None];
    let snapshot = repo.config_snapshot();
    for section in snapshot.plumbing().sections() {
        let header = section.header();
        if !header.name().eq_ignore_ascii_case(b"gpg") {
            continue;
        }
        // Subsection names are the one case-sensitive part of a config key, and
        // `gpg.<fmt>.program` puts the format there.
        let sub = header.subsection_name().map(|s| s.to_string());
        for (name, _) in section.body() {
            if !name.eq_ignore_ascii_case("program") {
                continue;
            }
            let (slot, key) = match sub.as_deref() {
                None => (0, "gpg.program".to_string()),
                Some("openpgp") => (0, "gpg.openpgp.program".to_string()),
                Some("x509") => (1, "gpg.x509.program".to_string()),
                Some("ssh") => (2, "gpg.ssh.program".to_string()),
                Some(_) => continue,
            };
            winner[slot] = Some(key);
        }
    }
    let defaults = [
        SigFormat::OpenPgp.default_program(),
        SigFormat::X509.default_program(),
        SigFormat::Ssh.default_program(),
    ];
    std::array::from_fn(|i| {
        // `git_config_pathname()`: the value is a path, so `~`/`~user` expand.
        winner[i]
            .as_deref()
            .and_then(|key| snapshot.trusted_path(key).ok().flatten())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| defaults[i].to_owned())
    })
}

/// The program a *verification* runs. git holds it in the `gpg_format[]` table
/// that `git_gpg_config()` filled at startup, so it is resolved once per process
/// against the repository this one is running in.
fn verify_program(format: SigFormat) -> String {
    static PROGRAMS: OnceLock<[String; 3]> = OnceLock::new();
    let programs = PROGRAMS.get_or_init(|| match crate::setup::discover() {
        Ok(repo) => gpg_programs(&repo),
        Err(_) => [
            SigFormat::OpenPgp.default_program().to_owned(),
            SigFormat::X509.default_program().to_owned(),
            SigFormat::Ssh.default_program().to_owned(),
        ],
    });
    programs[format as usize].clone()
}

// ---------------------------------------------------------------------------
// SSH signatures (`gpg.format = ssh`)
// ---------------------------------------------------------------------------

/// git's `gpg_format` entry for `ssh` plus the two statics
/// `gpg_interface_config()` fills for it.
struct SshConfig {
    /// `gpg.ssh.program`, defaulting to git's `"ssh-keygen"`.
    program: String,
    /// `gpg.ssh.allowedSignersFile`; without it nothing can be verified.
    allowed_signers: Option<PathBuf>,
    /// `gpg.ssh.revocationFile`, passed to `ssh-keygen -Y verify -r`.
    revocation_file: Option<PathBuf>,
}

/// The ssh backend's configuration, read once per process.
///
/// git holds these in file-scope statics that `git_config()` fills during
/// startup, so a single read for the whole process is the faithful shape; the
/// repository is the one the process is running in.
fn ssh_config() -> &'static SshConfig {
    static CFG: OnceLock<SshConfig> = OnceLock::new();
    CFG.get_or_init(|| {
        let Ok(repo) = crate::setup::discover() else {
            return SshConfig {
                program: "ssh-keygen".into(),
                allowed_signers: None,
                revocation_file: None,
            };
        };
        let snapshot = repo.config_snapshot();
        // `git_config_pathname()`: `~`/`~user` expansion, which `trusted_path`
        // performs while resolving the value.
        let path = |key: &str| -> Option<PathBuf> { snapshot.trusted_path(key).ok().flatten() };
        SshConfig {
            // The ssh slot of the same `gpg_format[]` table the signer reads, so
            // `gpg.ssh.program` gets `git_config_pathname()`'s `~` expansion here
            // exactly as it does there.
            program: gpg_programs(&repo)[SigFormat::Ssh as usize].clone(),
            allowed_signers: path("gpg.ssh.allowedSignersFile"),
            revocation_file: path("gpg.ssh.revocationFile"),
        }
    })
}

/// `verify_ssh_signed_buffer()`: find the principals that could have signed the
/// payload, then let `ssh-keygen -Y verify` judge each of them.
///
/// Without `gpg.ssh.allowedSignersFile` git prints its error and leaves
/// `sigc->result` at its initial `'N'` — nothing was checked, so nothing is
/// claimed — which is what the `NoSignature` return reproduces.
fn verify_ssh(sig: &[u8], payload: &[u8]) -> SigCheck {
    // The `goto out` endings leave `sigc->output` NULL, so nothing is printed and
    // nothing is claimed — `SigCheck::default()` is that state.
    let unchecked = SigCheck::default;
    let cfg = ssh_config();
    let Some(allowed) = cfg.allowed_signers.as_deref() else {
        eprintln!(
            "error: gpg.ssh.allowedSignersFile needs to be configured and exist \
             for ssh signature verification"
        );
        return unchecked();
    };
    let Some(sigfile) = write_temp("vtag", sig) else { return unchecked() };

    // `-Overify-time=<committer date>`: the key's validity window is judged as
    // of when the payload was written, not now.
    let verify_time = payload_timestamp(payload)
        .map(|t| format!("-Overify-time={}", strftime_local(t)))
        .unwrap_or_default();

    let ssh = |args: &[&std::ffi::OsStr], stdin: &[u8]| -> Option<std::process::Output> {
        run_with_stdin(&cfg.program, args, stdin)
    };
    let os = std::ffi::OsStr::new;

    let principals = ssh(
        &[
            os("-Y"),
            os("find-principals"),
            os("-f"),
            allowed.as_os_str(),
            os("-s"),
            sigfile.as_os_str(),
            os(&verify_time),
        ],
        &[],
    );
    // A spawn failure is `pipe_command` returning non-zero with both streams
    // empty — git carries on into the check-novalidate branch below rather than
    // giving up, which is why this is not an early return.
    let (principals_ok, principals_out, principals_err) = match &principals {
        Some(o) => (o.status.success(), o.stdout.as_slice(), o.stderr.as_slice()),
        None => (false, [].as_slice(), [].as_slice()),
    };
    if !principals_ok && String::from_utf8_lossy(principals_err).contains("usage:") {
        eprintln!(
            "error: ssh-keygen -Y find-principals/verify is needed for ssh signature \
             verification (available in openssh version 8.2p1+)"
        );
        let _ = std::fs::remove_file(&sigfile);
        return unchecked();
    }

    let mut out: Vec<u8> = Vec::new();
    // ssh-keygen's *stderr*, which git appends to `sigc->output` after the
    // stdout so the user sees the tool's own errors.
    let mut err: Vec<u8> = Vec::new();
    // `ret`, tracked as its inverse: whether the backend was satisfied. Entering
    // the principal loop below, git's `ret` is find-principals' zero, so a
    // principal list of nothing but blank lines leaves the signature accepted.
    let mut backend_ok = true;
    if !principals_ok || principals_out.is_empty() {
        // No matching principal: `check-novalidate` still describes the
        // signature, but an unknown key is a failure either way.
        if let Some(o) = ssh(
            &[
                os("-Y"),
                os("check-novalidate"),
                os("-n"),
                os("git"),
                os("-s"),
                sigfile.as_os_str(),
                os(&verify_time),
            ],
            payload,
        ) {
            out = o.stdout;
            err = o.stderr;
        }
        // git's `ret = -1`: an unknown key fails whatever check-novalidate said.
        backend_ok = false;
    } else {
        // Try every principal `find-principals` reported, one per line, until
        // one of them verifies.
        for line in principals_out.split(|&b| b == b'\n') {
            let principal = line.strip_suffix(b"\r").unwrap_or(line);
            if principal.is_empty() {
                continue;
            }
            let principal = String::from_utf8_lossy(principal).into_owned();
            let mut args: Vec<&std::ffi::OsStr> = vec![
                os("-Y"),
                os("verify"),
                os("-n"),
                os("git"),
                os("-f"),
                allowed.as_os_str(),
                os("-I"),
                os(&principal),
                os("-s"),
                sigfile.as_os_str(),
                os(&verify_time),
            ];
            match cfg.revocation_file.as_deref() {
                Some(rev) if rev.exists() => {
                    args.push(os("-r"));
                    args.push(rev.as_os_str());
                }
                Some(rev) => eprintln!(
                    "warning: ssh signing revocation file configured but not found: {}",
                    rev.display()
                ),
                None => {}
            }
            let Some(o) = ssh(&args, payload) else {
                backend_ok = false;
                continue;
            };
            out = o.stdout;
            err = o.stderr;
            // `if (!ret) ret = !starts_with(ssh_keygen_out.buf, "Good");`
            backend_ok = o.status.success() && out.starts_with(b"Good");
            if backend_ok {
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&sigfile);

    // `sigc->output` is the stripspaced tool output with both stderr streams
    // appended, and `sigc->gpg_status` is a copy of it — which is why `--raw` on
    // an ssh signature prints the same text as the plain form.
    // gpg-interface.c:604-608, in order: the tool output and the *last*
    // ssh-keygen stderr are stripspaced, and find-principals' stderr is appended
    // raw between them.
    let mut output = stripspace(&out);
    output.extend_from_slice(principals_err);
    output.extend_from_slice(&stripspace(&err));

    let mut check = parse_ssh_output(&output);
    check.gpg_status = output.clone();
    check.output = output;
    check.backend_ok = backend_ok;
    check
}

/// git's `strbuf_stripspace(buf, NULL)`: trailing whitespace removed from every
/// line, runs of blank lines collapsed to one, leading/trailing blank lines
/// dropped, and a non-empty result ended with a newline.
///
/// The same port already exists privately in `porcelain/tag.rs`; both are copies
/// of one C function and should be hoisted into a shared helper.
fn stripspace(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut pending_blank = false;
    for line in input.split(|&b| b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        let trimmed = &line[..end];
        if trimmed.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank {
            out.push(b'\n');
            pending_blank = false;
        }
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
    out
}

/// `parse_ssh_output()`: read `ssh-keygen -Y verify`'s first line.
///
/// It is either `Good "git" signature for <principal> with <alg> key <fpr>` or,
/// for a valid signature by a key no allowed-signers entry claims,
/// `Good "git" signature with <alg> key <fpr>`. The principal may itself contain
/// ` with `, so the *last* occurrence delimits it. Anything else is a bad
/// signature.
fn parse_ssh_output(out: &[u8]) -> SigCheck {
    let text = String::from_utf8_lossy(out);
    let line = text.split('\n').next().unwrap_or("");
    // `sigc->result = 'B'; sigc->trust_level = TRUST_NEVER;` before the parse
    // (gpg-interface.c:534-535), which is what an unrecognised line leaves behind.
    let bad = || SigCheck {
        status: GStatus::Bad,
        trust: Trust::Never,
        ..SigCheck::default()
    };

    let (rest, signer, trust) = if let Some(after) = line.strip_prefix("Good \"git\" signature for ")
    {
        let Some(at) = after.rfind(" with ") else {
            return bad();
        };
        // `line = search + 1` leaves the cursor on the "with …" word, and the
        // signer is everything before the space that precedes it.
        (&after[at + 1..], after[..at].to_string(), Trust::Fully)
    } else if let Some(after) = line.strip_prefix("Good \"git\" signature with ") {
        (after, String::new(), Trust::Undefined)
    } else {
        return bad();
    };

    // `strstr(line, "key ")`: everything after it is the fingerprint, which is
    // also what git reports as the key (`%GK`).
    match rest.find("key ").map(|at| rest[at + 4..].to_string()) {
        Some(key) => SigCheck {
            // Both branches set `sigc->result = 'G'` (gpg-interface.c:434, :439);
            // only the trust level differs — `TRUST_FULLY` for a known principal,
            // `TRUST_UNDEFINED` for a valid signature by an unknown key. Folding
            // the latter into `U` here would be reading `%G?` back into the result.
            status: GStatus::Good,
            key,
            signer,
            trust,
            ..SigCheck::default()
        },
        // Output did not match what we expected: treat the signature as bad.
        None => bad(),
    }
}

/// Run `program` with `args`, feeding `stdin` and capturing stdout and stderr.
fn run_with_stdin(
    program: &str,
    args: &[&std::ffi::OsStr],
    stdin: &[u8],
) -> Option<std::process::Output> {
    use std::io::Write as _;

    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            // `child_err_spew()`'s `CHILD_ERR_ERRNO` arm (run-command.c:403):
            // `error_errno("cannot exec '%s'")` with the error routine swapped
            // for the die routine, so it prints `fatal:` and yet does not die —
            // the caller carries on and reports its own failure.
            eprintln!("fatal: cannot exec '{program}': {}", crate::external::strerror(&e));
            return None;
        }
    };

    // Feed the payload from a helper thread, which is what git's `pipe_command()`
    // does with poll(): the checker writes its status lines before it has drained
    // stdin, so a single-threaded write blocks on a full stdout pipe while the
    // child blocks on a full stdin pipe — a deadlock that only shows up on
    // payloads large enough to fill a pipe buffer.
    let mut sink = child.stdin.take()?;
    let payload = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        // A closed pipe is not fatal: ssh-keygen exits before reading the payload
        // on several of its error paths, which is why git ignores SIGPIPE here.
        let _ = sink.write_all(&payload);
    });

    let out = child.wait_with_output().ok();
    let _ = writer.join();
    out
}

/// `parse_payload_metadata()`: the committer (or tagger) date of a signed
/// payload, which dates the signature for `-Overify-time`.
fn payload_timestamp(payload: &[u8]) -> Option<i64> {
    for line in payload.split(|&b| b == b'\n') {
        // Headers end at the first blank line.
        if line.is_empty() {
            return None;
        }
        let rest = if let Some(r) = line.strip_prefix(b"committer ".as_slice()) {
            r
        } else if let Some(r) = line.strip_prefix(b"tagger ".as_slice()) {
            r
        } else {
            continue;
        };
        // `<name> <email> <seconds> <timezone>`: the date is the second-to-last
        // whitespace-separated field.
        let text = String::from_utf8_lossy(rest);
        let mut fields = text.split_whitespace().rev();
        let _tz = fields.next()?;
        return fields.next()?.parse().ok();
    }
    None
}

/// `show_date()` in git's `DATE_STRFTIME` mode with `%Y%m%d%H%M%S` and
/// `local = 1` — SSH key validity carries no timezone, so git uses this
/// machine's.
fn strftime_local(seconds: i64) -> String {
    let t = seconds as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `t`; both are live local
    // variables of the right types, and the call is reentrant.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// One row of git's `sigcheck_gpg_status[]` table (gpg-interface.c:186-197): the
/// status prefix, the `sigc->result` it sets (if any), and which further fields
/// the line carries.
struct StatusRow {
    check: &'static str,
    result: Option<GStatus>,
    flags: u8,
}

/// Only one status with this flag may appear per signature; a second is git's
/// `goto error`.
const EXCLUSIVE: u8 = 1 << 0;
/// The status carries a key identifier.
const KEYID: u8 = 1 << 1;
/// The status carries a user identifier after the key id.
const UID: u8 = 1 << 2;
/// The status carries the `VALIDSIG` fingerprint fields.
const FINGERPRINT: u8 = 1 << 3;
/// The status carries a trust level.
const TRUST_LEVEL: u8 = 1 << 4;

/// `GPG_STATUS_STDSIG` — the standard exclusive `*SIG` shape, with key id and uid.
const STDSIG: u8 = EXCLUSIVE | KEYID | UID;

/// `sigcheck_gpg_status[]`, in table order. Order matters: the scan takes the
/// first row whose prefix matches and stops, and `TRUST_` is deliberately last
/// so it cannot shadow a longer name.
const STATUS_ROWS: &[StatusRow] = &[
    StatusRow { check: "GOODSIG ",   result: Some(GStatus::Good),        flags: STDSIG },
    StatusRow { check: "BADSIG ",    result: Some(GStatus::Bad),         flags: STDSIG },
    StatusRow { check: "ERRSIG ",    result: Some(GStatus::CannotCheck), flags: EXCLUSIVE | KEYID },
    StatusRow { check: "EXPSIG ",    result: Some(GStatus::Expired),     flags: STDSIG },
    StatusRow { check: "EXPKEYSIG ", result: Some(GStatus::KeyExpired),  flags: STDSIG },
    StatusRow { check: "REVKEYSIG ", result: Some(GStatus::Revoked),     flags: STDSIG },
    StatusRow { check: "VALIDSIG ",  result: None,                       flags: FINGERPRINT },
    StatusRow { check: "TRUST_",     result: None,                       flags: TRUST_LEVEL },
];

/// `parse_gpg_output()` (gpg-interface.c:236-345): one sequential pass over the
/// `--status-fd` stream, filling `struct signature_check` from the rows of
/// [`STATUS_ROWS`].
///
/// Two properties are load-bearing and easy to get wrong by writing something
/// that merely looks equivalent:
///
///   * there is **no precedence ranking** between the `*SIG` statuses — each
///     assignment simply overwrites, and the last matching line wins, because
///     two of them at once is an error (below) rather than a contest;
///   * a stream with no recognised line at all leaves `sigc->result` at the `'N'`
///     `check_signature()` set, *not* at `'E'`. gpg failing to make sense of a
///     signature therefore reads as "nothing was verified", which is what git
///     reports for it.
fn parse_gpg_status_full(status: &[u8]) -> SigCheck {
    let text = String::from_utf8_lossy(status);
    let mut sigc = SigCheck::default();
    let mut seen_exclusive = false;

    for line in text.lines() {
        let Some(line) = line.strip_prefix("[GNUPG:] ") else { continue };
        let Some((row, rest)) = STATUS_ROWS
            .iter()
            .find_map(|row| line.strip_prefix(row.check).map(|rest| (row, rest)))
        else {
            continue;
        };

        if row.flags & EXCLUSIVE != 0 {
            if seen_exclusive {
                return status_parse_error();
            }
            seen_exclusive = true;
        }
        if let Some(result) = row.result {
            sigc.status = result;
        }
        if row.flags & KEYID != 0 {
            let (key, after) = match rest.split_once(' ') {
                Some((key, after)) => (key, Some(after)),
                None => (rest, None),
            };
            sigc.key = key.to_string();
            // git only takes the uid when something followed the key id.
            if let (Some(after), true) = (after, row.flags & UID != 0) {
                sigc.signer = after.to_string();
            }
        }
        if row.flags & TRUST_LEVEL != 0 {
            // `strcspn(line, " \n")`: the level name, whose absence from the
            // table is `goto error` rather than a silent `TRUST_UNDEFINED`.
            let level = rest.split(' ').next().unwrap_or("");
            match level {
                "UNDEFINED" => sigc.trust = Trust::Undefined,
                "NEVER" => sigc.trust = Trust::Never,
                "MARGINAL" => sigc.trust = Trust::Marginal,
                "FULLY" => sigc.trust = Trust::Fully,
                "ULTIMATE" => sigc.trust = Trust::Ultimate,
                _ => return status_parse_error(),
            }
        }
        if row.flags & FINGERPRINT != 0 {
            let mut fields = rest.split(' ');
            sigc.fingerprint = fields.next().unwrap_or("").to_string();
            // `for (j = 9; j > 0; j--)`: skip nine interim fields to reach the
            // primary key fingerprint, which only OpenPGP `VALIDSIG` lines have.
            // Running out of fields first leaves it unset.
            sigc.primary_key_fingerprint =
                fields.nth(8).unwrap_or("").to_string();
        }
    }
    sigc
}

/// `parse_gpg_output()`'s `error:` label: the result is `'E'` and every partial
/// field is dropped, so a confused stream cannot leave half a verdict behind.
fn status_parse_error() -> SigCheck {
    SigCheck { status: GStatus::CannotCheck, ..SigCheck::default() }
}

/// Write `data` to a uniquely-named temp file, returning its path. Unique per
/// process + a monotonic counter (no wall-clock/RNG needed).
fn write_temp(tag: &str, data: &[u8]) -> Option<std::path::PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("zvcs-{tag}-{}-{n}", std::process::id()));
    std::fs::write(&path, data).ok().map(|_| path)
}

/// `sign_buffer_gpg()` (gpg-interface.c:1010-1055): the openpgp/x509 signer.
///
/// ```c
/// strvec_pushl(&gpg.args, use_format->program, "--status-fd=2", "-bsau", signing_key, NULL);
/// ret = pipe_command(&gpg, buffer->buf, buffer->len, signature, 1024, &gpg_status, 0);
/// for (cp = gpg_status.buf; cp && (cp = strstr(cp, "[GNUPG:] SIG_CREATED ")); cp++)
///         if (cp == gpg_status.buf || cp[-1] == '\n') break; /* found */
/// ret |= !cp;
/// ```
///
/// Two things here are load-bearing rather than cosmetic:
///
///   * **the argument vector is exactly those four words.** git passes no
///     `--batch` and no `--no-tty`; both suppress the pinentry prompt a passphrase
///     -protected key needs, turning a signable key into a signing failure.
///   * **success is `SIG_CREATED`, not the exit status.** `--status-fd=2` exists so
///     that this can be read, and gpg has paths that exit 0 without producing a
///     signature. Trusting the exit status writes a commit whose `gpgsig` header
///     is whatever gpg happened to leave on stdout — a *silently unsigned commit
///     at exit 0*, which is the one failure a signing command must not have.
///
/// `Err` carries the body of git's `error(_("gpg failed to sign the data:\n%s"))`,
/// verbatim including the trailing newline gpg's own output ends with, so the
/// caller only has to prefix `error: `.
fn sign_buffer_gpg(payload: &[u8], program: &str, key: &str) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let fail = |status: &[u8]| {
        // `gpg_status.len ? gpg_status.buf : "(no gpg output)"`.
        let body = if status.is_empty() {
            "(no gpg output)".to_string()
        } else {
            String::from_utf8_lossy(status).into_owned()
        };
        format!("gpg failed to sign the data:\n{body}")
    };

    let mut cmd = Command::new(program);
    cmd.arg("--status-fd=2");
    // `-bsau <key>`: detached, sign, armor, local-user. `sign_buffer()` resolves
    // the key through `get_signing_key()` before this runs, so there is never a
    // signer here without one.
    cmd.arg("-bsau").arg(key);
    let mut child = match cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        // `start_command()` reports a failed exec from the forked child and the
        // parent then sees an empty status stream, which is `(no gpg output)`.
        Err(e) => {
            eprintln!("fatal: cannot exec '{program}': {}", crate::external::strerror(&e));
            return Err(fail(b""));
        }
    };
    // `sigchain_push(SIGPIPE, SIG_IGN)`: gpg exits without draining the payload on
    // several error paths, and the write that then fails must not kill us.
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(payload);
    }
    let out = match child.wait_with_output() {
        Ok(out) => out,
        Err(e) => return Err(fail(e.to_string().as_bytes())),
    };

    let created = out
        .stderr
        .windows(SIG_CREATED.len())
        .enumerate()
        .any(|(i, w)| w == SIG_CREATED && (i == 0 || out.stderr[i - 1] == b'\n'));
    if !out.status.success() || !created {
        return Err(fail(&out.stderr));
    }
    Ok(out.stdout)
}

/// The status line `sign_buffer_gpg()` searches for, which must start a line.
const SIG_CREATED: &[u8] = b"[GNUPG:] SIG_CREATED ";

/// `vfreportf()` (usage.c:12-38), which every `error()` / `warning()` / `die()`
/// message passes through on its way to stderr:
///
/// ```c
/// char msg[4096];
/// memcpy(msg, prefix, prefix_len);
/// vsnprintf(p, pend - p, err, params);
/// for (; p != pend - 1 && *p; p++)
///         if (iscntrl(*p) && *p != '\t' && *p != '\n')
///                 *p = '?';
/// *(p++) = '\n';
/// ```
///
/// Two effects that only show up once a *child's* bytes are relayed into a
/// message, which is exactly what a signing diagnostic does:
///
///   * every control character except tab and newline becomes `?`. `ssh-keygen`
///     ends its lines with CRLF, so git's rendering of its complaint carries a
///     literal `?` before each newline; a port that prints the bytes through
///     drops it.
///   * the whole line, prefix included, is clipped to 4096 bytes with the last
///     reserved for the newline. A chatty `gpg --status-fd=2` reaches that.
///
/// Returns the line without its trailing newline, for `eprintln!`.
pub fn report(prefix: &str, message: &str) -> String {
    const MSG: usize = 4096;
    let mut line = String::with_capacity(prefix.len() + message.len());
    line.push_str(prefix);
    line.push_str(message);
    // `vsnprintf(p, pend - p, ...)` NUL-terminates within the buffer, so the
    // body is clipped to what is left after the prefix and the final newline.
    // Truncation is by bytes; step back to a character boundary rather than
    // panicking on a split multi-byte sequence.
    if line.len() > MSG - 1 {
        let mut cut = MSG - 1;
        while cut > 0 && !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
    }
    // The sanitizing loop starts *after* the prefix, but every prefix git uses is
    // printable, so scanning the whole line is the same answer.
    line.chars()
        .map(|c| {
            if c.is_ascii_control() && c != '\t' && c != '\n' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// signing (git's `gpg_format` table, `get_signing_key[_id]` and `sign_buffer`)
// ---------------------------------------------------------------------------

/// git's `gpg_format[]` table (gpg-interface.c:93-124), selected by `gpg.format`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigFormat {
    /// `openpgp` — the default; signs with `gpg`.
    OpenPgp,
    /// `x509` — signs with `gpgsm`, otherwise identical to `openpgp`.
    X509,
    /// `ssh` — signs with `ssh-keygen -Y sign`.
    Ssh,
}

impl SigFormat {
    /// `get_format_by_name()`. `None` is git's "invalid value for gpg.format".
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "openpgp" => Some(SigFormat::OpenPgp),
            "x509" => Some(SigFormat::X509),
            "ssh" => Some(SigFormat::Ssh),
            _ => None,
        }
    }

    /// The table's built-in `.program`, which `gpg.program` / `gpg.ssh.program`
    /// override.
    fn default_program(self) -> &'static str {
        match self {
            SigFormat::OpenPgp => "gpg",
            SigFormat::X509 => "gpgsm",
            SigFormat::Ssh => "ssh-keygen",
        }
    }
}

/// The resolved signing backend for a repository: git's `use_format` plus the
/// `configured_signing_key`, program table and `gpg.ssh.defaultKeyCommand` that
/// `git_gpg_config()` fills in.
///
/// This is what every caller that needs a signature should go through: signing
/// with `gpg` while `gpg.format = ssh` is configured produces
/// `gpg: skipped …: No secret key` instead of a signature.
pub struct Signer {
    /// `use_format`.
    pub format: SigFormat,
    /// The selected format's entry in the `gpg_format[]` program table — see
    /// [`gpg_programs`] for why this is not a single config lookup.
    pub program: String,
    /// `configured_signing_key` — `user.signingKey`, unset when absent.
    pub key: Option<String>,
    /// `ssh_default_key_command` — `gpg.ssh.defaultKeyCommand`. git ships no
    /// default for it, so an unset value is what makes an ssh signer with no
    /// `user.signingKey` fatal rather than merely keyless.
    default_key_command: Option<String>,
    /// The value of a `gpg.format` that names no entry in `gpg_format[]`, held
    /// rather than reported: `git_gpg_config()` rejects it, but only once
    /// `gpg_interface_lazy_init()` runs — which is the first sign or verify, not
    /// startup. So `git status` in the same repository is unaffected, and this is
    /// raised by [`Self::sign`].
    bad_format: Option<String>,
    /// The committer ident (`Name <email>`), git's `get_signing_key()` fallback
    /// for openpgp/x509 when `user.signingKey` is unset.
    committer: Option<String>,
}

/// A signing failure in git's two flavors, because the caller has to reproduce
/// the difference: `sign_buffer_gpg`/`sign_buffer_ssh` report with `error()` and
/// leave the caller to `die()` on top ("failed to write commit object"), while
/// `get_default_ssh_signing_key()` `die()`s on the spot and nothing follows it.
#[derive(Debug)]
pub enum SignFailure {
    /// A `die()` inside the backend: print `fatal: <msg>` and stop. No second
    /// line.
    Fatal(String),
    /// An `error()` inside the backend: print `error: <msg>`, then whatever
    /// `die()` the caller wraps signing in.
    Error(String),
    /// Already reported in full; only the `die()`'s exit code is left to carry,
    /// and the caller must add no message of its own.
    Silent,
}

impl std::fmt::Display for SignFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignFailure::Fatal(m) | SignFailure::Error(m) => f.write_str(m),
            SignFailure::Silent => Ok(()),
        }
    }
}

impl Signer {
    /// Read `gpg.format`, the program table, `user.signingKey` and
    /// `gpg.ssh.defaultKeyCommand` out of `repo`.
    ///
    /// A `gpg.format` naming no entry in the table is git's
    /// `error(_("invalid value for '%s': '%s'"))` followed by the config reader's
    /// own `die()` (gpg-interface.c:775-779). It is *recorded* here rather than
    /// reported, because signing must not begin: falling back to `openpgp`, as
    /// this used to, signs with a backend the user did not ask for and exits 0
    /// where git exits 128.
    pub fn resolve(repo: &gix::Repository) -> Self {
        let snap = repo.config_snapshot();
        let configured_format = snap.string("gpg.format").map(|v| v.to_string());
        let format = configured_format
            .as_deref()
            .and_then(SigFormat::from_name)
            .unwrap_or(SigFormat::OpenPgp);
        let bad_format = configured_format.filter(|v| SigFormat::from_name(v).is_none());
        let committer = repo.committer().transpose().ok().flatten().map(|c| {
            // `git_committer_info(IDENT_STRICT | IDENT_NO_DATE)`: name and email only.
            format!("{} <{}>", c.name, c.email)
        });
        Signer {
            format,
            program: gpg_programs(repo)[format as usize].clone(),
            key: snap.string("user.signingKey").map(|v| v.to_string()),
            default_key_command: snap
                .string("gpg.ssh.defaultKeyCommand")
                .map(|v| v.to_string()),
            bad_format,
            committer,
        }
    }

    /// Raise the `gpg.format` the config reader rejected, at the moment
    /// `gpg_interface_lazy_init()` would have run.
    ///
    /// git prints `error: invalid value for 'gpg.format': '<value>'` and then dies
    /// through the config reader, whose second line names the file and line the
    /// value came from. That line is not reproduced — gix's config parser does not
    /// carry per-entry line numbers — so this reports the first and stops, rather
    /// than inventing a location.
    fn check_format(&self) -> Result<(), SignFailure> {
        match &self.bad_format {
            None => Ok(()),
            Some(value) => {
                eprintln!("error: invalid value for 'gpg.format': '{value}'");
                Err(SignFailure::Silent)
            }
        }
    }

    /// `get_signing_key()` (gpg-interface.c:950-961):
    ///
    /// ```c
    /// if (configured_signing_key)     return xstrdup(configured_signing_key);
    /// if (use_format->get_default_key) return use_format->get_default_key();
    /// return xstrdup(git_committer_info(IDENT_STRICT | IDENT_NO_DATE));
    /// ```
    ///
    /// Only the ssh entry has a `get_default_key`, so the committer ident is the
    /// openpgp/x509 fallback and *never* the ssh one; ssh goes to
    /// [`Self::default_ssh_signing_key`] instead, whose `None` leaves the caller
    /// with an empty key — which is what `sign_buffer_ssh` turns into
    /// `user.signingKey needs to be set for ssh signing`.
    pub fn signing_key(&self) -> Result<String, SignFailure> {
        if let Some(key) = &self.key {
            return Ok(key.clone());
        }
        match self.format {
            SigFormat::Ssh => self.default_ssh_signing_key()?.ok_or_else(|| {
                SignFailure::Error(
                    "user.signingKey needs to be set for ssh signing".to_string(),
                )
            }),
            _ => self.committer.clone().ok_or_else(|| {
                SignFailure::Fatal("no committer identity to sign with".to_string())
            }),
        }
    }

    /// `get_default_ssh_signing_key()` (gpg-interface.c:880-925): run
    /// `gpg.ssh.defaultKeyCommand` and take the first line of its stdout, provided
    /// that line looks like a key.
    ///
    /// Three outcomes, all of them observable:
    ///
    ///   * the command is not configured — `die()`, and it names both keys the
    ///     user could have set. This is the message an ssh signer with no
    ///     `user.signingKey` actually gets; `user.signingKey needs to be set for
    ///     ssh signing` is what comes *later*, once a command has run and
    ///     produced nothing.
    ///   * the command succeeded but its first line is not a key — `warning()`
    ///     with both of its streams, then no key.
    ///   * the command failed — `warning()` with both of its streams, then no key.
    ///
    /// Both warnings interpolate `"%s %s"` of stderr then stdout, each still
    /// carrying its own trailing newline; the resulting ragged wrapping is git's.
    fn default_ssh_signing_key(&self) -> Result<Option<String>, SignFailure> {
        let Some(command) = self.default_key_command.as_deref() else {
            return Err(SignFailure::Fatal(
                "either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured"
                    .to_string(),
            ));
        };
        let argv = match crate::alias::split_cmdline(command) {
            Ok(argv) => argv,
            Err(e) => {
                return Err(SignFailure::Fatal(format!(
                    "malformed build-time gpg.ssh.defaultKeyCommand: {e}"
                )))
            }
        };
        // `pipe_command()` failing to exec is just a non-zero return to the
        // caller, so a missing command lands in the same `failed:` warning as one
        // that ran and exited non-zero — with both streams empty.
        let failed = |stderr: &str, stdout: &str| {
            eprintln!(
                "{}",
                report(
                    "warning: ",
                    &format!("gpg.ssh.defaultKeyCommand failed: {stderr} {stdout}")
                )
            );
            Ok(None)
        };
        let Some((program, args)) = argv.split_first().filter(|(p, _)| !p.is_empty()) else {
            return failed("", "");
        };
        let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        let Some(out) = run_with_stdin(program, &args, &[]) else {
            return failed("", "");
        };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            return failed(&stderr, &stdout);
        }
        // `strchr(begin, '\n')`, so a command that prints several keys offers only
        // its first.
        let first_line = stdout.split('\n').next().unwrap_or_default();
        // `is_literal_ssh_key` is used here only to *validate*; the `key::` prefix
        // is stripped later, where the key is used.
        if literal_ssh_key(first_line).is_none() {
            eprintln!(
                "{}",
                report(
                    "warning: ",
                    &format!(
                        "gpg.ssh.defaultKeyCommand succeeded but returned no keys: {stderr} {stdout}"
                    )
                )
            );
            return Ok(None);
        }
        Ok(Some(first_line.to_string()))
    }

    /// `get_signing_key_id()` (gpg-interface.c:938): a textual but unique name for
    /// the signing key. openpgp/x509 have only the key id itself; the ssh backend
    /// turns the key (a file, or a literal `key::`/`ssh-…` blob) into its
    /// `ssh-keygen -lf` fingerprint.
    pub fn signing_key_id(&self) -> Result<String, SignFailure> {
        self.check_format()?;
        let key = self.signing_key()?;
        match self.format {
            SigFormat::Ssh => ssh_key_fingerprint(&key),
            _ => Ok(key),
        }
    }

    /// `sign_buffer()`: dispatch to the format's signer and return the detached
    /// signature — armored for openpgp/x509, an `SSH SIGNATURE` block for ssh.
    pub fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignFailure> {
        self.check_format()?;
        let key = self.signing_key()?;
        match self.format {
            SigFormat::Ssh => sign_buffer_ssh(payload, &self.program, &key),
            _ => sign_buffer_gpg(payload, &self.program, &key).map_err(SignFailure::Error),
        }
    }
}

/// `is_literal_ssh_key()` (gpg-interface.c:817): a `key::`-prefixed or bare
/// `ssh-…` value is the key itself rather than a path to it. Returns the key with
/// the prefix stripped.
fn literal_ssh_key(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix("key::") {
        return Some(rest);
    }
    value.starts_with("ssh-").then_some(value)
}

/// `get_ssh_key_fingerprint()` (gpg-interface.c:828-866): `ssh-keygen -lf` on the
/// key, whose output is `<bits> <fingerprint> <comment> (<type>)`; the
/// fingerprint is the second field. Every exit from it is a `die()`, and the two
/// messages differ by a pair of quotes that git does spell differently:
/// `die_errno` quotes the key, the two `die`s that follow do not.
fn ssh_key_fingerprint(signing_key: &str) -> Result<String, SignFailure> {
    let os = std::ffi::OsStr::new;
    let out = match literal_ssh_key(signing_key) {
        Some(literal) => run_with_stdin("ssh-keygen", &[os("-lf"), os("-")], literal.as_bytes()),
        None => run_with_stdin("ssh-keygen", &[os("-lf"), os(signing_key)], &[]),
    };
    let failed_to_run = || {
        SignFailure::Fatal(format!(
            "failed to get the ssh fingerprint for key '{signing_key}'"
        ))
    };
    let out = out.ok_or_else(failed_to_run)?;
    if !out.status.success() {
        return Err(failed_to_run());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `begin = strchr(begin, ' ') + 1` twice: the fingerprint is delimited by
    // single spaces, and a line with fewer than two of them is a `die`.
    text.split(' ').nth(1).filter(|f| !f.is_empty()).map(str::to_owned).ok_or_else(|| {
        SignFailure::Fatal(format!(
            "failed to get the ssh fingerprint for key {signing_key}"
        ))
    })
}

/// `sign_buffer_ssh()` (gpg-interface.c:1058): write the payload to a temp file,
/// run `<program> -Y sign -n git -f <key> [-U] <file>`, and read back the
/// `<file>.sig` the signer leaves next to it.
///
/// `-U` marks the key file as holding a *public* key, which is what a literal
/// `key::`/`ssh-…` value expands to; without it `ssh-keygen` expects a private key.
fn sign_buffer_ssh(
    payload: &[u8],
    program: &str,
    signing_key: &str,
) -> Result<Vec<u8>, SignFailure> {
    if signing_key.is_empty() {
        return Err(SignFailure::Error(
            "user.signingKey needs to be set for ssh signing".to_string(),
        ));
    }
    let literal = literal_ssh_key(signing_key);
    let key_file = match literal {
        Some(key) => Some(
            write_temp("sshkey", key.as_bytes()).ok_or_else(|| {
                SignFailure::Error("could not create temporary file".to_string())
            })?,
        ),
        None => None,
    };
    let key_path: PathBuf = match (&key_file, literal) {
        (Some(path), _) => path.clone(),
        // `interpolate_path(signing_key, 1)`: a plain path, `~` expanded.
        (None, _) => PathBuf::from(shellexpand_tilde(signing_key)),
    };

    let buffer_file = match write_temp("sshbuf", payload) {
        Some(p) => p,
        None => {
            if let Some(k) = &key_file {
                let _ = std::fs::remove_file(k);
            }
            return Err(SignFailure::Error(
                "could not create temporary file".to_string(),
            ));
        }
    };
    let sig_path = {
        let mut p = buffer_file.clone().into_os_string();
        p.push(".sig");
        PathBuf::from(p)
    };

    let os = std::ffi::OsStr::new;
    let mut args: Vec<&std::ffi::OsStr> = vec![
        os("-Y"),
        os("sign"),
        os("-n"),
        os("git"),
        os("-f"),
        key_path.as_os_str(),
    ];
    if literal.is_some() {
        args.push(os("-U"));
    }
    args.push(buffer_file.as_os_str());

    // `ret = error("%s", signer_stderr.buf)`: the child's stderr *is* the message,
    // untouched. Trimming it drops the trailing newline `error()` then doubles
    // into the blank line that separates ssh-keygen's complaint from what follows.
    let relay = |stderr: &[u8]| {
        let stderr = String::from_utf8_lossy(stderr).into_owned();
        SignFailure::Error(if stderr.contains("usage:") {
            "ssh-keygen -Y sign is needed for ssh signing (available in openssh version 8.2p1+)"
                .to_string()
        } else {
            stderr
        })
    };
    let result = match run_with_stdin(program, &args, &[]) {
        // A failed exec leaves `signer_stderr` empty, so git's `error("%s", "")`
        // prints a bare `error:` line after the child's own `cannot exec`.
        None => Err(SignFailure::Error(String::new())),
        Some(out) if !out.status.success() => Err(relay(&out.stderr)),
        // `error_errno`, so the reason is `strerror(errno)` with no ` (os error N)`.
        Some(_) => std::fs::read(&sig_path).map_err(|e| {
            SignFailure::Error(format!(
                "failed reading ssh signing data buffer from '{}': {}",
                sig_path.display(),
                crate::external::strerror(&e)
            ))
        }),
    };

    if let Some(k) = &key_file {
        let _ = std::fs::remove_file(k);
    }
    let _ = std::fs::remove_file(&buffer_file);
    let _ = std::fs::remove_file(&sig_path);
    result
}

/// `interpolate_path()`'s leading-`~` expansion, the only part a signing-key path
/// can use.
fn shellexpand_tilde(path: &str) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_owned();
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        // `~user/…` needs a passwd lookup git does and this does not; leave it be.
        return path.to_owned();
    }
    match std::env::var("HOME") {
        Ok(home) => format!("{home}{rest}"),
        Err(_) => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_git() {
        assert_eq!(GStatus::NoSignature.code(), 'N');
        assert_eq!(GStatus::Good.code(), 'G');
        assert_eq!(GStatus::GoodUnknown.code(), 'U');
        assert_eq!(GStatus::Bad.code(), 'B');
        assert_eq!(GStatus::Expired.code(), 'X');
        assert_eq!(GStatus::KeyExpired.code(), 'Y');
        assert_eq!(GStatus::Revoked.code(), 'R');
        assert_eq!(GStatus::CannotCheck.code(), 'E');
        assert!(!GStatus::NoSignature.is_signed());
        assert!(GStatus::CannotCheck.is_signed());
        assert!(GStatus::Good.is_good() && GStatus::GoodUnknown.is_good());
        assert!(!GStatus::Bad.is_good());
    }

    #[test]
    fn unsigned_commit_has_no_signature() {
        let raw = b"tree 1111111111111111111111111111111111111111\n\
                    author T <t@e.x> 1700000000 +0000\n\
                    committer T <t@e.x> 1700000000 +0000\n\
                    \n\
                    a message\n";
        assert!(split_signed(raw).is_none());
        assert_eq!(evaluate(raw).0, GStatus::NoSignature);
    }

    #[test]
    fn split_removes_gpgsig_and_defolds() {
        // A commit with a folded gpgsig header. The payload must be the object with
        // the whole gpgsig block gone; the signature must be de-folded verbatim.
        let raw = b"tree 1111111111111111111111111111111111111111\n\
                    author T <t@e.x> 1700000000 +0000\n\
                    committer T <t@e.x> 1700000000 +0000\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n\
                    \x20\n\
                    \x20iQIzBAABCAAd\n\
                    \x20-----END PGP SIGNATURE-----\n\
                    \n\
                    the subject\n";
        let (sig, payload) = split_signed(raw).expect("has a signature");
        // Signature de-folded: header line + blank + body + footer, no leading spaces.
        assert_eq!(
            sig,
            b"-----BEGIN PGP SIGNATURE-----\n\niQIzBAABCAAd\n-----END PGP SIGNATURE-----\n"
        );
        // Payload is the object without the gpgsig block, byte for byte.
        assert_eq!(
            payload,
            b"tree 1111111111111111111111111111111111111111\n\
              author T <t@e.x> 1700000000 +0000\n\
              committer T <t@e.x> 1700000000 +0000\n\
              \n\
              the subject\n"
                .to_vec()
        );
    }

    /// `parse_gpg_output()` is a sequential pass with no precedence ranking, and
    /// the fields it fills are exactly the ones its table flags.
    #[test]
    fn gpg_status_parses_every_flagged_field() {
        // Good + ultimate trust → G; %GK is the GOODSIG long key id (as git shows),
        // not the VALIDSIG fingerprint.
        let c = parse_gpg_status_full(
            b"[GNUPG:] GOODSIG DEADBEEF Some One\n\
              [GNUPG:] VALIDSIG ABCDEF0123 2026-01-01 1 0 4 0 22 10 00 FEEDFACE\n\
              [GNUPG:] TRUST_ULTIMATE 0\n",
        );
        assert_eq!(c.status, GStatus::Good);
        // `%GK` is the GOODSIG key id; `VALIDSIG` carries no `GPG_STATUS_KEYID`
        // flag in git's table, so it must not overwrite it.
        assert_eq!(c.key, "DEADBEEF");
        assert_eq!(c.signer, "Some One");
        assert_eq!(c.trust, Trust::Ultimate);
        // `GPG_STATUS_FINGERPRINT`: field 1, then field 10.
        assert_eq!(c.fingerprint, "ABCDEF0123");
        assert_eq!(c.primary_key_fingerprint, "FEEDFACE");

        assert_eq!(parse_gpg_status_full(b"[GNUPG:] BADSIG DEADBEEF n\n").status, GStatus::Bad);
        assert_eq!(
            parse_gpg_status_full(b"[GNUPG:] REVKEYSIG D n\n").status,
            GStatus::Revoked
        );

        // The last `TRUST_` line wins, as the sequential pass implies.
        let last = parse_gpg_status_full(b"[GNUPG:] TRUST_NEVER 0\n[GNUPG:] TRUST_FULLY 0 pgp\n");
        assert_eq!(last.trust, Trust::Fully);

        // Two exclusive statuses is git's `goto error`: 'E' with every partial
        // field dropped.
        let dup = parse_gpg_status_full(b"[GNUPG:] GOODSIG D n\n[GNUPG:] REVKEYSIG D n\n");
        assert_eq!(dup.status, GStatus::CannotCheck);
        assert!(dup.key.is_empty() && dup.signer.is_empty());

        // Nothing the table names → `check_signature()`'s initial 'N', NOT 'E'.
        // gpg failing to make sense of a signature is "nothing was verified".
        assert_eq!(parse_gpg_status_full(b"[GNUPG:] NODATA 1\n").status, GStatus::NoSignature);
    }

    /// git keeps `sigc->result` and the `%G?` character apart: a good signature by
    /// a key of unknown validity stays `'G'` in the result (which is what
    /// `GIT_PUSH_CERT_STATUS` and `verify_merge_signature` read) and only *prints*
    /// as `U`. Folding the two together made a signed push report `U` where stock
    /// git reports `G`.
    #[test]
    fn untrusted_good_signature_is_g_in_the_result_and_u_in_pretty() {
        let untrusted =
            parse_gpg_status_full(b"[GNUPG:] GOODSIG DEADBEEF Some One\n[GNUPG:] TRUST_UNDEFINED 0\n");
        assert_eq!(untrusted.status, GStatus::Good, "sigc->result is 'G'");
        assert_eq!(untrusted.trust, Trust::Undefined);
        assert_eq!(untrusted.pretty_status().code(), 'U', "%G? folds it to 'U'");

        let trusted =
            parse_gpg_status_full(b"[GNUPG:] GOODSIG DEADBEEF Some One\n[GNUPG:] TRUST_ULTIMATE 0\n");
        assert_eq!(trusted.status, GStatus::Good);
        assert_eq!(trusted.pretty_status().code(), 'G');

        // The ssh backend has the same split: a valid signature by a key no
        // allowed-signers entry claims is 'G' with TRUST_UNDEFINED
        // (gpg-interface.c:437-440), not a weaker result.
        let unknown_key = parse_ssh_output(b"Good \"git\" signature with RSA key SHA256:abc\n");
        assert_eq!(unknown_key.status, GStatus::Good);
        assert_eq!(unknown_key.trust, Trust::Undefined);
        assert_eq!(unknown_key.key, "SHA256:abc");
        assert_eq!(unknown_key.pretty_status().code(), 'U');

        let known = parse_ssh_output(b"Good \"git\" signature for a@b with RSA key SHA256:abc\n");
        assert_eq!(known.status, GStatus::Good);
        assert_eq!(known.trust, Trust::Fully);
        assert_eq!(known.signer, "a@b");
        assert_eq!(known.pretty_status().code(), 'G');
    }
}
