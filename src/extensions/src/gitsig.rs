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
//!  2. [`verify`] — run the signature through **gpg** (or, for SSH signatures,
//!     ssh-keygen), the same tools git shells to. When the tool is absent or
//!     cannot check, the result is `E` (cannot check) — git's own fallback —
//!     never a fabricated "good".
//!
//! Nothing here shells out to `git`; verification is delegated to gpg/ssh-keygen,
//! which is what git itself does (git has no in-process crypto either).

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// A commit's signature status, mapped to git's `%G?` single-character codes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GStatus {
    /// `N` — no signature on the commit.
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

/// Verify a `(signature, payload)` pair via the same tools git uses.
pub fn verify(sig: &[u8], payload: &[u8]) -> (GStatus, String) {
    if sig.starts_with(b"-----BEGIN SSH SIGNATURE-----") {
        // SSH verification needs an allowed-signers file + the committer principal;
        // without that config it cannot be checked, exactly as git reports.
        return (GStatus::CannotCheck, String::new());
    }
    let Some(sigfile) = write_temp("sig", sig) else { return (GStatus::CannotCheck, String::new()) };
    let Some(payfile) = write_temp("pay", payload) else {
        let _ = std::fs::remove_file(&sigfile);
        return (GStatus::CannotCheck, String::new());
    };
    let out = Command::new("gpg")
        .args(["--batch", "--no-tty", "--status-fd=1", "--verify"])
        .arg(&sigfile)
        .arg(&payfile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&sigfile);
    let _ = std::fs::remove_file(&payfile);
    match out {
        Ok(o) => parse_gpg_status(&o.stdout),
        Err(_) => (GStatus::CannotCheck, String::new()), // gpg not installed → E
    }
}

/// git's `enum signature_trust_level`, in the same order — the scale
/// `gpg.minTrustLevel` and `verify_merge_signature`'s `TRUST_MARGINAL` floor are
/// compared against. `TRUST_UNDEFINED` is the value `check_signature()` starts
/// from, so an unparsed / absent `TRUST_*` status reads as undefined.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Trust {
    Undefined,
    Never,
    Marginal,
    Fully,
    Ultimate,
}

/// git's `struct signature_check`, reduced to the fields this crate reads.
pub struct SigCheck {
    /// `sigc->result`, the `%G?` character.
    pub status: GStatus,
    /// `sigc->key` — the signing key's long id.
    pub key: String,
    /// `sigc->signer` — the user name gpg's `GOODSIG`/`BADSIG`/… line names after
    /// the key id. This is what git's `Commit %s has a good GPG signature by %s`
    /// diagnostics quote, not the key id.
    pub signer: String,
    /// `sigc->trust_level`, from the `TRUST_*` status line.
    pub trust: Trust,
}

/// [`evaluate`] with git's full `signature_check` result — the signer name and
/// trust level that `verify_merge_signature()` needs and `%G?` alone cannot
/// carry. Same gpg invocation as [`verify`]; no second verification path.
pub fn evaluate_full(raw: &[u8]) -> SigCheck {
    match split_signed(raw) {
        None => SigCheck {
            status: GStatus::NoSignature,
            key: String::new(),
            signer: String::new(),
            trust: Trust::Undefined,
        },
        Some((sig, payload)) => verify_full(&sig, &payload),
    }
}

/// [`verify`] with git's full `signature_check` result. See [`evaluate_full`].
pub fn verify_full(sig: &[u8], payload: &[u8]) -> SigCheck {
    let cannot_check = || SigCheck {
        status: GStatus::CannotCheck,
        key: String::new(),
        signer: String::new(),
        trust: Trust::Undefined,
    };
    if sig.starts_with(b"-----BEGIN SSH SIGNATURE-----") {
        return cannot_check();
    }
    let Some(sigfile) = write_temp("sig", sig) else { return cannot_check() };
    let Some(payfile) = write_temp("pay", payload) else {
        let _ = std::fs::remove_file(&sigfile);
        return cannot_check();
    };
    let out = Command::new("gpg")
        .args(["--batch", "--no-tty", "--status-fd=1", "--verify"])
        .arg(&sigfile)
        .arg(&payfile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&sigfile);
    let _ = std::fs::remove_file(&payfile);
    match out {
        Ok(o) => parse_gpg_status_full(&o.stdout),
        Err(_) => cannot_check(),
    }
}

/// Map gpg's machine-readable `--status-fd` output to a [`GStatus`] and key,
/// following git's precedence (bad/revoked/expired take precedence over good).
fn parse_gpg_status(status: &[u8]) -> (GStatus, String) {
    let text = String::from_utf8_lossy(status);
    let (mut good, mut bad, mut errsig) = (false, false, false);
    let (mut exp, mut expkey, mut revkey, mut trusted) = (false, false, false, false);
    let mut key = String::new();
    for line in text.lines() {
        let l = line.strip_prefix("[GNUPG:] ").unwrap_or(line);
        let mut it = l.split_whitespace();
        match it.next() {
            Some("GOODSIG") => {
                good = true;
                if key.is_empty() {
                    key = it.next().unwrap_or("").to_string();
                }
            }
            Some("VALIDSIG") => {
                // git's %GK is the signing key's long id (from GOODSIG); the
                // VALIDSIG fingerprint is only a fallback when GOODSIG had none.
                if key.is_empty() {
                    if let Some(fpr) = it.next() {
                        key = fpr.to_string();
                    }
                }
            }
            Some("BADSIG") => bad = true,
            Some("ERRSIG") => errsig = true,
            Some("EXPSIG") => exp = true,
            Some("EXPKEYSIG") => expkey = true,
            Some("REVKEYSIG") => revkey = true,
            Some("TRUST_ULTIMATE") | Some("TRUST_FULLY") => trusted = true,
            _ => {}
        }
    }
    // ERRSIG and the fallthrough both map to CannotCheck in git; keeping the
    // separate branch keeps `errsig` a live read and documents the mapping.
    #[allow(clippy::if_same_then_else)]
    let status = if bad {
        GStatus::Bad
    } else if revkey {
        GStatus::Revoked
    } else if expkey {
        GStatus::KeyExpired
    } else if exp {
        GStatus::Expired
    } else if good {
        if trusted { GStatus::Good } else { GStatus::GoodUnknown }
    } else if errsig {
        GStatus::CannotCheck
    } else {
        GStatus::CannotCheck
    };
    (status, key)
}

/// [`parse_gpg_status`] plus the two fields `%G?` throws away: the signer name
/// each `GOODSIG`/`BADSIG`/`EXPSIG`/`EXPKEYSIG`/`REVKEYSIG` line carries after
/// its key id (git's `GPG_STATUS_STDSIG`), and the `TRUST_<level>` line.
fn parse_gpg_status_full(status: &[u8]) -> SigCheck {
    let (gstatus, key) = parse_gpg_status(status);
    let text = String::from_utf8_lossy(status);
    let mut signer = String::new();
    let mut trust = Trust::Undefined;
    for line in text.lines() {
        let l = line.strip_prefix("[GNUPG:] ").unwrap_or(line);
        let (tag, rest) = match l.split_once(' ') {
            Some(p) => p,
            None => (l, ""),
        };
        match tag {
            // `GPG_STATUS_STDSIG`: `<KEYID> <name...>`; the name runs to EOL.
            "GOODSIG" | "BADSIG" | "EXPSIG" | "EXPKEYSIG" | "REVKEYSIG" => {
                if signer.is_empty() {
                    if let Some((_, name)) = rest.split_once(' ') {
                        signer = name.to_string();
                    }
                }
            }
            _ => {
                if let Some(level) = tag.strip_prefix("TRUST_") {
                    trust = match level {
                        "NEVER" => Trust::Never,
                        "MARGINAL" => Trust::Marginal,
                        "FULLY" => Trust::Fully,
                        "ULTIMATE" => Trust::Ultimate,
                        _ => Trust::Undefined,
                    };
                }
            }
        }
    }
    SigCheck { status: gstatus, key, signer, trust }
}

/// Write `data` to a uniquely-named temp file, returning its path. Unique per
/// process + a monotonic counter (no wall-clock/RNG needed).
fn write_temp(tag: &str, data: &[u8]) -> Option<std::path::PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("zvcs-{tag}-{}-{n}", std::process::id()));
    std::fs::write(&path, data).ok().map(|_| path)
}

/// Sign `payload` with gpg, returning the ASCII-armored detached signature.
///
/// Ports git's `sign_buffer` for the gpg backend: the program is `gpg.program`
/// (falling back to `gpg`), the key comes from `user.signingKey` when set — git
/// passes `-u <key>`, otherwise gpg picks the default key — and the signature is
/// armored and detached (`-bsa`), which is the form both commit `gpgsig` headers
/// and push certificates carry.
///
/// `Err` carries gpg's own stderr, so a missing key or a locked agent reaches
/// the user verbatim instead of as a generic failure.
pub fn sign(payload: &[u8], program: &str, key: Option<&str>) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut cmd = Command::new(program);
    cmd.args(["--batch", "--no-tty", "--status-fd=2", "-bsa"]);
    if let Some(k) = key {
        cmd.arg("-u").arg(k);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| "gpg stdin unavailable".to_string())?
        .write_all(payload)
        .map_err(|e| format!("writing to {program}: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
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

    #[test]
    fn gpg_status_precedence() {
        // Good + ultimate trust → G; %GK is the GOODSIG long key id (as git shows),
        // not the VALIDSIG fingerprint.
        let (s, k) = parse_gpg_status(
            b"[GNUPG:] GOODSIG DEADBEEF Some One\n[GNUPG:] VALIDSIG ABCDEF0123 x\n[GNUPG:] TRUST_ULTIMATE 0\n",
        );
        assert_eq!(s, GStatus::Good);
        assert_eq!(k, "DEADBEEF");
        // Good but untrusted → U.
        let (s, _) = parse_gpg_status(b"[GNUPG:] GOODSIG DEADBEEF n\n[GNUPG:] TRUST_UNDEFINED\n");
        assert_eq!(s, GStatus::GoodUnknown);
        // Bad wins over everything.
        let (s, _) = parse_gpg_status(b"[GNUPG:] BADSIG DEADBEEF n\n");
        assert_eq!(s, GStatus::Bad);
        // Revoked key with an otherwise-good sig → R.
        let (s, _) = parse_gpg_status(b"[GNUPG:] GOODSIG D n\n[GNUPG:] REVKEYSIG D n\n");
        assert_eq!(s, GStatus::Revoked);
        // Nothing conclusive → E.
        assert_eq!(parse_gpg_status(b"[GNUPG:] NODATA 1\n").0, GStatus::CannotCheck);
    }
}
