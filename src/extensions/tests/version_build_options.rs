//! `git version --build-options` — `get_version_info(buf, 1)`.
//!
//! The build report is the one command whose output is a set of claims about
//! the binary printing it, which makes it the easiest place in the port to lie
//! and the hardest place to notice a lie. Stock's values are known
//! (`libcurl: 8.7.1`, `zlib: 1.2.12`, `gettext: enabled`) and pasting them in
//! would turn a visibly-failing parity case into a silently-passing fabrication.
//!
//! `SHA-1: SHA1_DC` and `SHA-256: SHA256_BLK` are the two values stock and this
//! build share, and they are shared because they are true twice over, not
//! because they were copied. Both tokens name a backend *category* in `hash.h`:
//! `SHA1_DC` is the collision-detecting one (its three alternatives all read
//! `(No collision detection)`), and `SHA256_BLK` is the `#else` — a
//! self-contained block implementation, where `SHA256_NETTLE`, `SHA256_GCRYPT`
//! and `SHA256_OPENSSL` each name an external crypto library. This build's
//! `sha1-checked` is `sha1collisiondetection` in git's bail-out configuration
//! and its `sha2` is a pure-Rust block compressor linking no crypto library, so
//! both are asserted against the dependency graph below rather than as bare
//! strings: drop either crate and the assertion fails instead of going stale.
//! `SHA-256:` is additionally asserted behaviourally — the line is only
//! defensible if the format it names actually works, so a sha256 repository is
//! created and its object ids are compared against stock git's.
//!
//! So the assertions here are deliberately one-sided. They do not pin the exact
//! text of the honest lines — those may legitimately change when the build does.
//! They pin the two things that must never change:
//!
//!   * every line that *is* printed is derived from this build (the two `sizeof`
//!     lines are recomputed here from the same target types), and
//!   * no line names a C component this build does not link. If `libcurl:` ever
//!     reappears, either the binary started linking curl — in which case this
//!     test should be updated along with the code — or someone copied stock's
//!     report, which is the failure this file exists to catch.
//!
//! `git diagnose` and `git bugreport` embed the same block through the same
//! function, as `cmd_diagnose()` does in git, so the report is asserted to be
//! identical in `git bugreport`'s output too.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Components git names only when it links them. None is present in this build:
/// the transport is reqwest + rustls, there are no message catalogs, and deflate
/// is in-tree while inflate is `zlib-rs` (reported under its own name).
const NOT_LINKED: [&str; 5] = ["gettext:", "libcurl:", "OpenSSL:", "zlib:", "zlib-ng:"];

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-buildopts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TERM", "dumb")
        // `git bugreport` opens the finished report in an editor, so without a
        // pinned one the test reads the machine instead of the binary: it passes
        // wherever `EDITOR` happens to be set and dies with "Terminal is dumb,
        // but EDITOR unset" on a runner where it is not. `:` is the shell no-op
        // git's own suite uses — the report is still written, just not opened.
        .env("GIT_EDITOR", ":")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "exit {:?}", out.status.code());
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn field<'a>(report: &'a str, name: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
}

/// The workspace lockfile: the resolved dependency graph this binary was built
/// from, and the only thing that can say whether a component claim is still
/// true. `src/extensions` is a workspace member, so the lock is two up.
fn cargo_lock() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("Cargo.lock");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// "The format of this string should be kept stable for compatibility with
/// external projects that rely on the output of `git version`" — so the build
/// report opens with exactly what plain `git version` prints, and
/// `--no-build-options` leaves nothing but that line.
#[test]
fn the_report_opens_with_the_plain_version_line() {
    let dir = fixture("shape");
    let plain = stdout(&run(&dir, &["version"]));
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert!(report.starts_with(&plain), "report must begin with {plain:?}");
    assert!(report.len() > plain.len(), "--build-options added nothing");
    assert_eq!(stdout(&run(&dir, &["version", "--no-build-options"])), plain);
}

/// The two width lines are facts about the target this was compiled for, and
/// are recomputed here from the same types the port reads (`c_long`, `usize`).
/// A hardcoded `8` would pass on the machines this is usually built on and lie
/// on any other.
#[test]
fn the_sizes_are_this_targets_sizes() {
    let dir = fixture("sizes");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert_eq!(
        field(&report, "sizeof-long:"),
        Some(std::mem::size_of::<std::os::raw::c_long>().to_string().as_str())
    );
    assert_eq!(
        field(&report, "sizeof-size_t:"),
        Some(std::mem::size_of::<usize>().to_string().as_str())
    );
}

/// No line may name a C component this build does not link. This is the
/// anti-fabrication assertion: stock prints all six of these, so their absence
/// is the difference between a report about *this* binary and a copy of stock's.
#[test]
fn no_line_claims_a_component_this_build_does_not_link() {
    let dir = fixture("honest");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    for claim in NOT_LINKED {
        assert!(
            !report.lines().any(|l| l.starts_with(claim)),
            "report claims {claim} which this build does not link:\n{report}"
        );
    }
    // The lines that *are* printed say what is true here rather than what stock
    // says: this binary is Rust where stock's is not.
    assert_eq!(field(&report, "rust:"), Some("enabled"));
    // `SHA-1:` is the one field where stock's token is also the true one.
    // `SHA1_BACKEND` in `hash.h` names a backend *category* — the three
    // non-detecting spellings all carry "(No collision detection)", `SHA1_DC`
    // is the detecting one — and this build detects: `sha1-checked` implements
    // `cr-marcstevens/sha1collisiondetection`, and `gix-hash` builds it with
    // `safe_hash(false)`, git's bail-out configuration. Asserted through the
    // dependency graph rather than as a bare string so the claim dies with the
    // crate: drop `sha1-checked` and this fails instead of going stale.
    assert_eq!(field(&report, "SHA-1:"), Some("SHA1_DC"));
    assert!(
        cargo_lock().contains("name = \"sha1-checked\""),
        "SHA-1: SHA1_DC claims collision detection, but `sha1-checked` is no longer in the graph"
    );
    // `SHA-256:` is the same shape of claim. `SHA256_BLK` is `hash.h`'s `#else`
    // — the self-contained implementation, as against `SHA256_NETTLE` /
    // `SHA256_GCRYPT` / `SHA256_OPENSSL`, which each name an external crypto
    // library. `sha2` is that: a pure-Rust block compressor. Pinned to the graph
    // so turning `gix-hash`'s `sha256` feature back off fails here.
    assert_eq!(field(&report, "SHA-256:"), Some("SHA256_BLK"));
    assert!(
        cargo_lock().contains("name = \"sha2\""),
        "SHA-256: SHA256_BLK names a backend, but `sha2` is no longer in the graph"
    );
}

/// The flate line names a real crate at the version the build actually resolved,
/// not a literal in the source: `build.rs` reads it out of the lockfile, so a
/// hardcoded version could not survive a `cargo update`. Read back from the same
/// lockfile here, which is what makes the number falsifiable.
#[test]
fn the_flate_line_is_the_version_the_lockfile_resolved() {
    let dir = fixture("flate");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    let lock = cargo_lock();
    let resolved = lock
        .lines()
        .skip_while(|l| l.trim() != "name = \"zlib-rs\"")
        .take_while(|l| !l.trim_start().starts_with("[["))
        .find_map(|l| l.trim().strip_prefix("version = \"")?.strip_suffix('"').map(str::to_string))
        .expect("zlib-rs in the workspace lockfile");

    // `(inflate only)` is not decoration: `zlib_rs` is reached from `gix-zlib`'s
    // decompress path and nowhere else, while its deflate is an in-tree
    // transcription. Dropping the qualifier would claim the encoder too.
    assert_eq!(field(&report, "zlib-rs:"), Some(format!("{resolved} (inflate only)").as_str()));
}

/// The two `default-` lines are claims about defaults, and each has to agree
/// with the command that acts on them: `default-hash: sha1` means a bare
/// `git init` lays down a sha1 repository (not that sha256 is refused — see
/// [`the_sha256_line_is_backed_by_a_working_object_format`]), while
/// `default-ref-format: files` is *also* the only ref format with a backend, so
/// `--ref-format=reftable` must still be refused.
#[test]
fn the_declared_defaults_are_what_a_bare_init_produces() {
    let dir = fixture("formats");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert_eq!(field(&report, "default-ref-format:"), Some("files"));
    assert_eq!(field(&report, "default-hash:"), Some("sha1"));

    let plain = dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(run(&plain, &["init", "-q"]).status.success());
    // A sha1 repository is git's legacy layout: no `extensions.objectformat`
    // key at all, and `core.repositoryformatversion = 0`.
    let config = std::fs::read_to_string(plain.join(".git/config")).unwrap();
    assert!(
        !config.contains("objectformat"),
        "`default-hash: sha1` but a bare init recorded an object format:\n{config}"
    );
    assert!(config.contains("repositoryformatversion = 0"), "{config}");

    let out = run(&dir, &["init", "--ref-format=reftable"]);
    assert!(
        !out.status.success(),
        "--ref-format=reftable succeeded, so `files` is no longer the only supported ref format"
    );
}

/// A backend line is only true if the format it names actually works, so this
/// asserts the behaviour rather than the string: `git init --object-format=sha256`
/// must lay down the same repository stock lays down, and the objects written
/// into it must carry stock's ids. Object ids are the strongest available check
/// — they are the hash function applied to the exact bytes git would write, so
/// a matching id means the algorithm, the object encoding and the storage layout
/// all agree with stock at once.
///
/// Skipped when stock git is not on PATH, or when the stock git that is has no
/// sha256 support of its own to compare against.
#[test]
fn the_sha256_line_is_backed_by_a_working_object_format() {
    let dir = fixture("sha256");
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git installed to compare sha256 object ids against");
        return;
    };

    let zvcs_dir = dir.join("zvcs");
    let stock_dir = dir.join("stock");
    std::fs::create_dir_all(&zvcs_dir).unwrap();
    std::fs::create_dir_all(&stock_dir).unwrap();

    let init = |bin: &str, at: &Path| {
        Command::new(bin)
            .args(["init", "-q", "--object-format=sha256", "-b", "main", "."])
            .current_dir(at)
            .env("HOME", &dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    let stock_init = init(stock, &stock_dir);
    if !stock_init.status.success() {
        // This stock build has no sha256 backend, so there is nothing to compare.
        return;
    }
    assert!(init(BIN, &zvcs_dir).status.success(), "zvcs init --object-format=sha256 failed");

    // The recorded format is the extension pair git writes: the objectformat key
    // plus the repositoryformatversion bump every extension requires.
    let zvcs_config = std::fs::read_to_string(zvcs_dir.join(".git/config")).unwrap();
    assert_eq!(
        zvcs_config,
        std::fs::read_to_string(stock_dir.join(".git/config")).unwrap(),
        "sha256 init wrote a different config than stock"
    );
    assert!(zvcs_config.contains("objectformat = sha256"), "{zvcs_config}");

    // Write the same content through both binaries and compare every id the two
    // produce: the blob, the tree it hangs under, and the commit over that tree.
    let commit_ids = |bin: &str, at: &Path| -> Vec<String> {
        std::fs::write(at.join("f.txt"), "hello\n").unwrap();
        let git = |args: &[&str]| {
            let out = Command::new(bin)
                .args(args)
                .current_dir(at)
                .env("HOME", &dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "A")
                .env("GIT_AUTHOR_EMAIL", "a@example.com")
                .env("GIT_COMMITTER_NAME", "A")
                .env("GIT_COMMITTER_EMAIL", "a@example.com")
                .env("GIT_AUTHOR_DATE", "100000000 +0000")
                .env("GIT_COMMITTER_DATE", "100000000 +0000")
                .output()
                .unwrap();
            assert!(out.status.success(), "{bin} {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "one"]);
        vec![
            git(&["rev-parse", "HEAD:f.txt"]),
            git(&["rev-parse", "HEAD^{tree}"]),
            git(&["rev-parse", "HEAD"]),
        ]
    };
    let zvcs_ids = commit_ids(BIN, &zvcs_dir);
    assert_eq!(zvcs_ids, commit_ids(stock, &stock_dir), "sha256 object ids differ from stock");
    // Each id is a full sha256 digest, not a sha1 one padded or truncated.
    for id in &zvcs_ids {
        assert_eq!(id.len(), 64, "{id} is not a sha256 object id");
    }

    // And the repository this produced is internally consistent: `fsck` walks
    // every object it just wrote and must find nothing.
    let out = Command::new(BIN)
        .arg("fsck")
        .current_dir(&zvcs_dir)
        .env("HOME", &dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "fsck on a sha256 repo: {out:?}");
}

/// Where a real git lives. `git` on `PATH` is deliberately not consulted: this
/// binary shadows stock by name on any machine where zvcs is installed, so a
/// comparison test that resolved it there would drive this binary on both sides
/// and prove nothing.
const STOCK_CANDIDATES: [&str; 3] = ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"];

/// The first candidate that exists and is not this binary wearing git's name.
///
/// The probe is a superset verb run with an emptied environment: zvcs serves
/// `zverbs` itself, while a stock git looks for a `git-zverbs` on `PATH` and
/// fails. Clearing the environment is what makes it sound — zvcs's own
/// installation puts a `git-zverbs` shim on `PATH`, which a stock git would then
/// answer too. A throwaway `ZVCS_HOME` and a temp working directory keep an old
/// zvcs from writing its state into the source tree; a stock git ignores both.
fn stock_git() -> Option<&'static str> {
    let scratch = std::env::temp_dir().join(format!("zvcs-boprobe-{}", std::process::id()));
    let found = STOCK_CANDIDATES.into_iter().find(|bin| {
        Path::new(bin).exists()
            && !Command::new(bin)
                .arg("zverbs")
                .env_clear()
                .env("ZVCS_HOME", &scratch)
                .current_dir(std::env::temp_dir())
                .output()
                .map(|o| o.status.success() && !o.stdout.is_empty())
                .unwrap_or(false)
    });
    let _ = std::fs::remove_dir_all(&scratch);
    found
}

/// `cmd_diagnose()` renders its version block with `get_version_info(&buf, 1)`,
/// the same call `git version --build-options` makes. `git bugreport` prints
/// that block under its `git version:` header, so the report must appear there
/// verbatim — one implementation, three commands.
#[test]
fn bugreport_embeds_the_same_report() {
    let dir = fixture("bugreport");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    let out = run(&dir, &["bugreport", "--no-suffix"]);
    assert!(out.status.success(), "bugreport failed: {out:?}");
    let text = std::fs::read_to_string(dir.join("git-bugreport.txt")).unwrap();

    assert!(
        text.contains(&format!("git version:\n{report}")),
        "bugreport's version block is not the build report:\n{text}"
    );
}
