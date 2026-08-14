//! Differential corpus cases for the misc_commands subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! This module owns the leftovers: the stateful walker (`bisect`), the
//! submodule front-ends, the credential helpers, the small standalone plumbing
//! (`url-parse`, `last-modified`, `format-rev`, `repo`, `init-db`,
//! `sh-i18n--envsubst`, `for-each-repo`), the tool launchers (`difftool`,
//! `web--browse`, `instaweb`, `jump`), `history`, and `filter-branch`.
//!
//! # Commands that cannot be corpus cases, and why
//!
//! A parity case needs three things this harness cannot fabricate for every
//! command: a bounded run, an oracle that terminates on its own, and a result
//! that depends only on the repository. The following commands fail at least
//! one of those for their *primary* mode. They are listed here rather than
//! silently omitted, because "structurally unmeasurable" and "untested" are
//! different facts and the port report can only express the second.
//!
//! Long-running servers — the primary mode never exits, so `run_side` would
//! report a `Hang` for both implementations and measure the timeout, not the
//! port:
//!   * `daemon` — binds TCP 9418 and serves until killed.
//!   * `fsmonitor--daemon run`/`start` — watches the worktree until stopped.
//!   * `credential-cache--daemon` — listens on a unix socket until idle-timeout.
//!
//! Commands driven by a peer or a CGI environment rather than argv — with
//! `stdin` closed and no `PATH_INFO`/`GIT_URL` they exercise nothing:
//!   * `http-backend` — a CGI; behavior is a function of the request env.
//!   * `http-fetch`, `http-push`, `remote-http`, `remote-https`, `remote-ftp`,
//!     `remote-ftps` — all need a live HTTP/FTP endpoint; the harness is
//!     deliberately offline, and pointing them at a URL would make the case a
//!     network test.
//!   * `remote-ext`, `remote-fd` — transport helpers spoken to over a pipe by
//!     `fetch`/`push`; invoked directly they only reach their usage check.
//!   * `upload-archive--writer`, `checkout--worker` — internal helpers that
//!     read a packet-line/pkt stream from a parent process on stdin.
//!   * `shell` — a login shell for `authorized_keys`; its real surface is the
//!     `-c <command>` string a remote peer supplies over ssh.
//!
//! Foreign-SCM bridges the project documents as permanently unported, and which
//! additionally need a foreign server or working copy that does not exist here:
//!   * `p4` — needs a Perforce depot and the `p4` client binary.
//!   * `cvsimport`, `cvsserver`, `cvsexportcommit` — need a CVS repository and
//!     the `cvs` binary.
//!   * `archimport` — needs a GNU Arch/Bazaar archive and `tla`/`baz`.
//!   * `quiltimport` — needs a quilt `patches/` series to import.
//!
//! Host-state helpers whose answer comes from outside the repository:
//!   * `credential-cache` — answers from a daemon socket under `$XDG_CACHE_HOME`.
//!   * `credential-osxkeychain` — reads the macOS Keychain, and does not exist
//!     in stock git on Linux, so the *oracle* itself differs per platform.
//!   * `credential-netrc` — stock's copy is a Perl script that `use`s `Git.pm`;
//!     with Homebrew git 2.55.0 that module is absent, so every invocation dies
//!     in `BEGIN` before parsing argv. The oracle is broken identically for all
//!     input, so no invocation of it measures the port.
//!
//! Not a command at all:
//!   * `mergetool--lib` and the other `git-*--lib` files are shell libraries
//!     sourced by `difftool--helper`/`mergetool`; they are absent from
//!     `git --list-cmds=main` and cannot be dispatched.
//!
//! What *is* testable for all of the above is the argument surface: `-h` and an
//! unknown flag are handled before any socket is bound, any peer is contacted,
//! or any foreign tool is spawned. Those cases live in [`usage_only`] below and
//! are real coverage of code the port must still get right.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    bisect(out);
    submodule(out);
    submodule_helper(out);
    small_plumbing(out);
    tool_launchers(out);
    history_and_jump(out);
    filter_branch(out);
    usage_only(out);
}

/// `bisect` is the one genuinely stateful command in this module: `start`
/// writes `BISECT_*` refs and checks out a midpoint, so the post-state probe —
/// not stdout — is the real assertion. Every case starts from a pristine repo,
/// so multi-step sessions are expressed as the single `start <bad> <good>` form
/// that performs the whole setup in one invocation.
fn bisect(out: &mut Vec<Case>) {
    out.push(Case::new("bisect", &["bisect", "-h"], Shape::Linear));
    out.push(Case::new("bisect", &["bisect", "help"], Shape::Linear));
    out.push(Case::new("bisect", &["bisect", "no-such-subcommand"], Shape::Linear));

    // Session start: the interesting state transition.
    out.push(Case::new("bisect", &["bisect", "start"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "start"], Shape::Conflicted));
    out.push(Case::new("bisect", &["bisect", "start", "HEAD", "HEAD~2"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "start", "HEAD", "HEAD~1"], Shape::Detached));
    out.push(Case::new("bisect", &["bisect", "start", "HEAD", "HEAD~1"], Shape::Dirty));
    // A merge inside the bisect range is the case that separates a real
    // bisection from a linear walk.
    out.push(Case::new("bisect", &["bisect", "start", "HEAD", "HEAD~1"], Shape::Merged));
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--first-parent", "HEAD", "HEAD~1"],
        Shape::Merged,
    ));
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--no-checkout", "HEAD", "HEAD~2"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--term-old=works", "--term-new=broken", "HEAD", "HEAD~2"],
        Shape::Branched,
    ));
    out.push(Case::new("bisect", &["bisect", "start", "HEAD", "--", "src"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "start", "no-such-rev"], Shape::Branched));

    // Verbs that require a live session: run without one, they are error paths.
    out.push(Case::new("bisect", &["bisect", "bad"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "good"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "skip"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "next"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "terms"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "terms", "--term-good"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "terms", "--term-bad"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "visualize"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "run", "true"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "replay", "no-such-log"], Shape::Branched));
    read_only("bisect", &["bisect", "log"], out);

    // `reset` takes an optional commit-ish, which is a rev expression, not a
    // ref name — the distinction the argument parser has to get right.
    out.push(Case::new("bisect", &["bisect", "reset"], Shape::Branched));
    out.push(Case::new("bisect", &["bisect", "reset"], Shape::Detached));
    out.push(Case::new("bisect", &["bisect", "reset", "HEAD~1"], Shape::Branched));
}

/// The `Submodule` shape exists for exactly this: a parent with one real
/// submodule checked out from a local upstream. The `Linear` duplicates check
/// the no-submodule path, which is a distinct code path in every verb.
fn submodule(out: &mut Vec<Case>) {
    out.push(Case::new("submodule", &["submodule", "-h"], Shape::Linear));
    out.push(Case::new("submodule", &["submodule"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "no-such-subcommand"], Shape::Submodule));

    out.push(Case::new("submodule", &["submodule", "status"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "status"], Shape::Linear));
    out.push(Case::new("submodule", &["submodule", "status", "--recursive"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "status", "--cached"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "status", "--quiet"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "--quiet", "status"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "status", "no-such-path"], Shape::Submodule));

    out.push(Case::new("submodule", &["submodule", "init"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "init"], Shape::Linear));
    out.push(Case::new("submodule", &["submodule", "init", "sub"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "init", "no-such-path"], Shape::Submodule));

    out.push(Case::new("submodule", &["submodule", "sync"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "sync", "--recursive"], Shape::Submodule));

    out.push(Case::new("submodule", &["submodule", "update"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "update", "--init"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "update", "--checkout"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "update", "--recursive"], Shape::Submodule));
    // `--remote` re-fetches; with `protocol.file` denied the fixture's local
    // upstream is refused, so the interesting question is who reports that.
    out.push(Case::new("submodule", &["submodule", "update", "--remote"], Shape::Submodule));

    out.push(Case::new("submodule", &["submodule", "foreach", "true"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "foreach", "--quiet", "true"], Shape::Submodule));
    out.push(Case::new(
        "submodule",
        &["submodule", "foreach", "--recursive", "true"],
        Shape::Submodule,
    ));
    out.push(Case::new("submodule", &["submodule", "foreach", "pwd"], Shape::Submodule));
    // The five variables `foreach` is contracted to export into the child shell.
    out.push(Case::new(
        "submodule",
        &["submodule", "foreach", "echo $name $sm_path $displaypath $sha1 $toplevel"],
        Shape::Submodule,
    ));

    out.push(Case::new("submodule", &["submodule", "summary"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "summary", "--files"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "summary", "--cached"], Shape::Submodule));

    out.push(Case::new(
        "submodule",
        &["submodule", "set-branch", "--branch", "main", "sub"],
        Shape::Submodule,
    ));
    out.push(Case::new("submodule", &["submodule", "set-branch", "--default", "sub"], Shape::Submodule));
    out.push(Case::new(
        "submodule",
        &["submodule", "set-url", "sub", "https://example.invalid/sub.git"],
        Shape::Submodule,
    ));

    out.push(Case::new("submodule", &["submodule", "deinit", "sub"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "deinit", "--all"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "absorbgitdirs"], Shape::Submodule));
    out.push(Case::new("submodule", &["submodule", "add"], Shape::Submodule));
}

/// `submodule--helper` is the builtin the `git-submodule` shell script drives.
/// It is a separately dispatched command in `git --list-cmds=main`, and its
/// subcommand table is not the same as the front-end's — so it needs its own
/// cases rather than being assumed equivalent.
fn submodule_helper(out: &mut Vec<Case>) {
    out.push(Case::new("submodule--helper", &["submodule--helper"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "-h"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "no-such-sub"], Shape::Submodule));

    out.push(Case::new("submodule--helper", &["submodule--helper", "status"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "status"], Shape::Linear));
    // `module_foreach` parses with plain `parse_options`, so a flag *after* the
    // command is permuted out of it — one operand is left, which takes the shell
    // form and prints no `Entering` line. `git-submodule.sh` does not permute, so
    // the two entry points genuinely differ here and the helper cannot simply
    // forward. Nothing else in the corpus puts a flag after a `foreach` command.
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "foreach", "does-not-exist", "-q"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "status", "--recursive"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "status", "--cached"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "status", "no-such-path"],
        Shape::Submodule,
    ));
    out.push(Case::new("submodule--helper", &["submodule--helper", "init"], Shape::Submodule));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "foreach", "true"],
        Shape::Submodule,
    ));
    out.push(Case::new("submodule--helper", &["submodule--helper", "summary"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "sync"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "update"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "absorbgitdirs"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "deinit", "--all"], Shape::Submodule));
    out.push(Case::new("submodule--helper", &["submodule--helper", "add"], Shape::Submodule));
    out.push(Case::new(
        "submodule--helper",
        &["submodule--helper", "config", "submodule.sub.url"],
        Shape::Submodule,
    ));
}

/// Small standalone plumbing: `for-each-repo`, `init-db`, `url-parse`,
/// `sh-i18n--envsubst`, `last-modified`, `format-rev`, `repo`.
fn small_plumbing(out: &mut Vec<Case>) {
    // ---- for-each-repo: iterates a multi-valued config key ----
    out.push(Case::new("for-each-repo", &["for-each-repo"], Shape::Linear));
    out.push(Case::new("for-each-repo", &["for-each-repo", "-h"], Shape::Linear));
    out.push(Case::new("for-each-repo", &["for-each-repo", "--no-such-flag"], Shape::Linear));
    // An unset key is the empty list, not an error.
    out.push(Case::new(
        "for-each-repo",
        &["for-each-repo", "--config=parity.unset", "status", "--porcelain"],
        Shape::Linear,
    ));
    out.push(Case::new("for-each-repo", &["for-each-repo", "--config=parity.unset"], Shape::Linear));
    // `-c` supplies the list without touching the fixture, so the iteration
    // itself is exercised.
    out.push(Case::new(
        "for-each-repo",
        &["-c", "parity.list=.", "for-each-repo", "--config=parity.list", "rev-parse", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "for-each-repo",
        &["-c", "parity.list=.", "for-each-repo", "--config=parity.list", "log", "--oneline"],
        Shape::Branched,
    ));
    // A repo in the list that does not exist: the failure is the point.
    out.push(Case::new(
        "for-each-repo",
        &["-c", "parity.list=no-such-repo", "for-each-repo", "--config=parity.list", "status"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "for-each-repo",
        &[
            "-c",
            "parity.list=no-such-repo",
            "for-each-repo",
            "--config=parity.list",
            "--keep-going",
            "status",
        ],
        Shape::Linear,
    ));

    // ---- init-db: the pre-builtin spelling of `init`; same option parser ----
    out.push(Case::new("init-db", &["init-db"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "-q"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "--bare"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "-h"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "--no-such-flag"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "--template=/no/such/template"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "--object-format=bogus"], Shape::Linear));
    out.push(Case::new("init-db", &["init-db", "-b", "renamed"], Shape::Linear));

    // ---- url-parse: pure string work, so every shape would be redundant ----
    out.push(Case::new("url-parse", &["url-parse"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "-h"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "https://example.com/a/b.git"], Shape::Linear));
    out.push(Case::new(
        "url-parse",
        &["url-parse", "https://user:pw@example.com:8080/a/b.git"],
        Shape::Linear,
    ));
    out.push(Case::new("url-parse", &["url-parse", "ssh://git@example.com/x.git"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "file:///tmp/x"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "git@example.com:owner/repo.git"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "https://a/", "https://b/"], Shape::Linear));
    for component in ["scheme", "user", "host", "port", "path"] {
        let args = vec!["url-parse", "-c", component, "https://bob@example.com:99/a/b.git"];
        out.push(Case::new("url-parse", &args, Shape::Linear));
    }
    out.push(Case::new("url-parse", &["url-parse", "-c", "no-such-component", "https://h/"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "-c", "host"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "not-a-url"], Shape::Linear));
    out.push(Case::new("url-parse", &["url-parse", "--", "--looks-like-a-flag"], Shape::Linear));

    // ---- sh-i18n--envsubst: git's cut-down gettext envsubst ----
    // `--variables` is the only mode that produces output with stdin closed;
    // the substitution mode reads the template from stdin, which the harness
    // gives as /dev/null on both sides.
    out.push(Case::new("sh-i18n--envsubst", &["sh-i18n--envsubst"], Shape::Linear));
    out.push(Case::new("sh-i18n--envsubst", &["sh-i18n--envsubst", "--variables"], Shape::Linear));
    out.push(Case::new(
        "sh-i18n--envsubst",
        &["sh-i18n--envsubst", "--variables", "$HOME/$LANG"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "sh-i18n--envsubst",
        &["sh-i18n--envsubst", "--variables", "${LANG}x $UNSET_BY_HARNESS"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "sh-i18n--envsubst",
        &["sh-i18n--envsubst", "--variables", "no variables here"],
        Shape::Linear,
    ));
    out.push(Case::new("sh-i18n--envsubst", &["sh-i18n--envsubst", "$HOME"], Shape::Linear));
    out.push(Case::new("sh-i18n--envsubst", &["sh-i18n--envsubst", "a", "b"], Shape::Linear));
    out.push(Case::new("sh-i18n--envsubst", &["sh-i18n--envsubst", "--no-such-flag"], Shape::Linear));

    // ---- last-modified: per-path "commit that last touched this" ----
    read_only("last-modified", &["last-modified"], out);
    read_only("last-modified", &["last-modified", "-r"], out);
    out.push(Case::new("last-modified", &["last-modified", "--recursive"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "-t"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "-r", "-t"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "--max-depth=1"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "-z"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "HEAD"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "HEAD~1"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "--", "src"], Shape::Branched));
    out.push(Case::new("last-modified", &["last-modified", "-r"], Shape::AwkwardPaths));
    out.push(Case::new("last-modified", &["last-modified", "-r"], Shape::Submodule));
    out.push(Case::new("last-modified", &["last-modified", "-h"], Shape::Linear));
    out.push(Case::new("last-modified", &["last-modified", "--no-such-flag"], Shape::Linear));
    out.push(Case::new("last-modified", &["last-modified", "no-such-rev"], Shape::Branched));

    // ---- format-rev: stdin-driven pretty-printer; argv validation is the
    // testable half with stdin closed ----
    out.push(Case::new("format-rev", &["format-rev"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "-h"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "--format=%H"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "--stdin-mode=rev"], Shape::Branched));
    out.push(Case::new("format-rev", &["format-rev", "--stdin-mode=bogus", "--format=%H"], Shape::Branched));
    for mode in ["rev", "revs", "text"] {
        let arg = format!("--stdin-mode={mode}");
        out.push(Case::new("format-rev", &["format-rev", &arg, "--format=%H"], Shape::Branched));
    }
    out.push(Case::new(
        "format-rev",
        &["format-rev", "--stdin-mode=rev", "--format=%H", "-z"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "format-rev",
        &["format-rev", "--stdin-mode=rev", "--format=%H", "--null-input"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "format-rev",
        &["format-rev", "--stdin-mode=rev", "--format=%H", "--notes=refs/notes/commits"],
        Shape::Branched,
    ));

    // ---- repo: the object/ref census, added in 2.5x ----
    read_only("repo", &["repo", "structure"], out);
    read_only("repo", &["repo", "info", "--all"], out);
    out.push(Case::new("repo", &["repo"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "-h"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "no-such-sub"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "structure"], Shape::AwkwardPaths));
    out.push(Case::new("repo", &["repo", "structure"], Shape::Submodule));
    out.push(Case::new("repo", &["repo", "structure", "--format=lines"], Shape::Branched));
    out.push(Case::new("repo", &["repo", "structure", "-z"], Shape::Branched));
    out.push(Case::new("repo", &["repo", "structure", "--format=no-such-format"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "info"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "info", "--keys"], Shape::Linear));
    out.push(Case::new("repo", &["repo", "info", "object.format"], Shape::Linear));
    out.push(Case::new(
        "repo",
        &["repo", "info", "references.format", "layout.bare", "layout.shallow"],
        Shape::Branched,
    ));
    out.push(Case::new("repo", &["repo", "info", "-z", "--all"], Shape::Branched));
    out.push(Case::new("repo", &["repo", "info", "--format=nul", "--all"], Shape::Branched));
    out.push(Case::new("repo", &["repo", "info", "no.such.key"], Shape::Linear));
}

/// Commands whose job is to spawn something else. Cases are chosen so nothing
/// is actually launched at the network or a GUI: `--extcmd=true`/`false` is a
/// deterministic stand-in for a diff tool, `--httpd`/`--browser` are pointed at
/// names that do not exist, and no URL is ever passed to `web--browse` (stock
/// resolves one by really fetching it).
fn tool_launchers(out: &mut Vec<Case>) {
    // ---- difftool ----
    out.push(Case::new("difftool", &["difftool", "-h"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--tool-help"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--no-such-flag"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "-x", "true"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=false"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true", "--cached"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true", "--no-prompt"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true", "--", "README.md"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true", "HEAD~1", "HEAD"], Shape::Branched));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true", "main", "feature"], Shape::Branched));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true"], Shape::Conflicted));
    out.push(Case::new("difftool", &["difftool", "--extcmd=true"], Shape::AwkwardPaths));
    out.push(Case::new("difftool", &["difftool", "--dir-diff", "--extcmd=true"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--trust-exit-code", "--extcmd=false"], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "--tool=no-such-tool", "--no-prompt"], Shape::Dirty));

    // `difftool--helper` reads its tool from GIT_DIFFTOOL_EXTCMD/GIT_DIFF_TOOL,
    // which a `Case` cannot set. The seven-positional form is the one path that
    // is reachable from argv alone and whose stock stdout is reproducible.
    out.push(Case::new(
        "difftool--helper",
        &[
            "difftool--helper",
            "README.md",
            "/dev/null",
            "0000000000000000000000000000000000000000",
            "100644",
            "/dev/null",
            "0000000000000000000000000000000000000000",
            "100644",
        ],
        Shape::Dirty,
    ));

    // ---- web--browse: no URL argument, or stock really opens it ----
    out.push(Case::new("web--browse", &["web--browse"], Shape::Linear));
    out.push(Case::new("web--browse", &["web--browse", "-h"], Shape::Linear));
    out.push(Case::new("web--browse", &["web--browse", "--browser"], Shape::Linear));
    out.push(Case::new("web--browse", &["web--browse", "--no-such-flag"], Shape::Linear));

    // ---- instaweb: everything short of actually starting a server ----
    out.push(Case::new("instaweb", &["instaweb"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "-h"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "--no-such-flag"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "--stop"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "stop"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "--httpd=no-such-httpd"], Shape::Linear));
    out.push(Case::new("instaweb", &["instaweb", "--port=not-a-number"], Shape::Linear));
    out.push(Case::new(
        "instaweb",
        &["instaweb", "--httpd=no-such-httpd", "--port=1234", "restart"],
        Shape::Linear,
    ));
    out.push(Case::new("instaweb", &["instaweb", "--httpd=no-such-httpd"], Shape::Branched));

    // ---- credential front-end and the file-backed helper ----
    // stdin is /dev/null, so each of these sees an immediate EOF credential
    // description — the shared error path both implementations must agree on.
    out.push(Case::new("credential", &["credential"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "-h"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "no-such-op"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "fill"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "approve"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "reject"], Shape::Linear));
    out.push(Case::new("credential", &["credential", "capability"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store", "-h"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store", "get"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store", "store"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store", "erase"], Shape::Linear));
    out.push(Case::new("credential-store", &["credential-store", "no-such-op"], Shape::Linear));
    out.push(Case::new(
        "credential-store",
        &["credential-store", "--file=/no/such/credentials", "get"],
        Shape::Linear,
    ));
}

/// `history` (fixup/reword/split) and `jump` (quickfix generation).
fn history_and_jump(out: &mut Vec<Case>) {
    out.push(Case::new("history", &["history"], Shape::Branched));
    out.push(Case::new("history", &["history", "-h"], Shape::Branched));
    out.push(Case::new("history", &["history", "no-such-sub"], Shape::Branched));
    out.push(Case::new("history", &["history", "fixup"], Shape::Branched));
    out.push(Case::new("history", &["history", "fixup", "no-such-rev"], Shape::Branched));
    // Nothing staged: the documented refusal.
    out.push(Case::new("history", &["history", "fixup", "HEAD", "--dry-run"], Shape::Branched));
    // Something staged: the real fixup path.
    out.push(Case::new("history", &["history", "fixup", "HEAD", "--dry-run"], Shape::Dirty));
    out.push(Case::new("history", &["history", "fixup", "HEAD~1"], Shape::Branched));
    out.push(Case::new("history", &["history", "reword", "HEAD", "--dry-run"], Shape::Branched));
    out.push(Case::new("history", &["history", "reword", "HEAD"], Shape::Branched));
    out.push(Case::new(
        "history",
        &["history", "reword", "HEAD", "--dry-run", "--update-refs=branches"],
        Shape::Branched,
    ));
    out.push(Case::new("history", &["history", "split", "HEAD", "--dry-run"], Shape::Branched));
    out.push(Case::new("history", &["history", "split", "HEAD", "--dry-run", "--", "src"], Shape::Branched));

    out.push(Case::new("jump", &["jump"], Shape::Linear));
    out.push(Case::new("jump", &["jump", "no-such-mode"], Shape::Linear));
    out.push(Case::new("jump", &["jump", "--stdout"], Shape::Linear));
    out.push(Case::new("jump", &["jump", "--stdout"], Shape::Dirty));
    out.push(Case::new("jump", &["jump", "--stdout"], Shape::Conflicted));
    out.push(Case::new("jump", &["jump", "--stdout", "diff"], Shape::Dirty));
    out.push(Case::new("jump", &["jump", "--stdout", "diff"], Shape::Linear));
    out.push(Case::new("jump", &["jump", "--stdout", "ws"], Shape::Dirty));
    out.push(Case::new("jump", &["jump", "--stdout", "merge"], Shape::Conflicted));
    out.push(Case::new("jump", &["jump", "--stdout", "merge"], Shape::Linear));
    out.push(Case::new("jump", &["jump", "--stdout", "grep", "two"], Shape::Branched));
}

/// `filter-branch` gaps. Deliberately few cases: the script sleeps ten seconds
/// on its own deprecation warning before doing anything, so each case costs
/// roughly twenty seconds of wall clock across the two sides.
fn filter_branch(out: &mut Vec<Case>) {
    out.push(Case::new("filter-branch", &["filter-branch"], Shape::Branched));
    out.push(Case::new("filter-branch", &["filter-branch", "--force", "HEAD"], Shape::Branched));
    out.push(Case::new("filter-branch", &["filter-branch", "--msg-filter", "cat", "HEAD"], Shape::Branched));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "--index-filter", "git rm --cached --ignore-unmatch README.md", "--", "--all"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "--subdirectory-filter", "src", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new("filter-branch", &["filter-branch", "--tag-name-filter", "cat", "--", "--all"], Shape::Branched));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "--commit-filter", "git_commit_non_empty_tree", "HEAD"],
        Shape::Branched,
    ));
    // `export` is the filter, so `GIT_AUTHOR_NAME=x` lands in the rev argument
    // list — an argument-parsing failure inside the script, not a filter error.
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "--env-filter", "export", "GIT_AUTHOR_NAME=x", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "filter-branch",
        &["filter-branch", "--original", "refs/parity-backup", "--force", "HEAD"],
        Shape::Branched,
    ));
}

/// The argument surface of the commands documented above as structurally
/// unmeasurable. `-h` and an unknown flag are handled before a socket is bound,
/// a peer is contacted, or a foreign tool is spawned — so these terminate on
/// their own and depend only on argv, which is exactly what a corpus case
/// needs. This is the whole of what the harness can measure for these commands;
/// it is a real slice of the port's behavior, not a placeholder.
///
/// `--no-such-flag` is spelled with a leading dash on purpose: several of these
/// treat a bare word as a URL or a repository and would go looking for it.
fn usage_only(out: &mut Vec<Case>) {
    const HELP_AND_BOGUS: &[&str] = &[
        "daemon",
        "http-backend",
        "http-fetch",
        "http-push",
        "remote-http",
        "remote-https",
        "remote-ext",
        "remote-fd",
        "remote-ftp",
        "remote-ftps",
        "fsmonitor--daemon",
        "credential-cache",
        "credential-cache--daemon",
        "p4",
        "cvsimport",
        "cvsserver",
        "cvsexportcommit",
        "archimport",
        "quiltimport",
        "shell",
        "upload-archive--writer",
        "checkout--worker",
    ];
    for cmd in HELP_AND_BOGUS {
        out.push(Case::new(cmd, &[cmd, "-h"], Shape::Linear));
        out.push(Case::new(cmd, &[cmd, "--no-such-flag"], Shape::Linear));
        out.push(Case::new(cmd, &[cmd], Shape::Linear));
    }

    // `-h` on the porcelain people actually type it at.
    //
    // parse-options has one renderer and two callers that differ only in the
    // stream: `-h` goes to **stdout** at 129 with no `error:` line, a rejection
    // goes to **stderr** at 129 with one. The exit code is identical, so nothing
    // but a byte comparison of the two streams can tell them apart — which is how
    // 46 of these verbs came to answer `-h` through the rejection path unnoticed,
    // several of them also dropping the trailing blank line the renderer always
    // emits. `diff` is the deliberate odd one out: its help comes from
    // `usage(builtin_diff_usage)`, so stock puts it on stderr.
    for cmd in [
        "log", "diff", "commit", "checkout", "branch", "show", "tag", "merge", "reset", "config",
        "rm", "restore", "rev-parse", "am", "apply", "cherry-pick", "revert", "format-patch",
        "reflog", "status",
    ] {
        out.push(Case::strict(cmd, &[cmd, "-h"], Shape::Linear));
    }

    // The other `-h` family: `show_usage_if_asked` answers only when `-h` is the
    // *sole* argument (`ac == 2`), where parse-options answers wherever it appears.
    // Nothing distinguishes the two families except a second argument, so without
    // these a verb can be moved to the wrong family and no case would notice.
    for cmd in [
        "diff-index", "diff-files", "diff-tree", "rev-list", "var", "fetch-pack", "index-pack",
        "merge-index", "merge-recursive", "merge-subtree", "merge-ours", "checkout-index",
        "commit-tree", "ls-files",
    ] {
        out.push(Case::strict(cmd, &[cmd, "-h", "--quux"], Shape::Linear));
    }

    // Two verbs own `-h` as a real option, so help is the *wrong* answer here:
    // `show-ref`'s is a hidden `--head` alias (builtin/show-ref.c:317) and
    // `grep`'s is `--no-filename` (builtin/grep.c:1101). Both are one-line
    // regressions waiting to happen the next time `-h` handling is touched.
    out.push(Case::new("show-ref", &["show-ref", "-h", "HEAD"], Shape::Linear));
    out.push(Case::new("grep", &["grep", "-h", "fixture"], Shape::Linear));

    // `-h` inside a short cluster still means help for a parse-options verb,
    // which the obvious `args[0] == "-h"` implementation gets wrong.
    out.push(Case::strict("status", &["status", "-sh"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "-ah"], Shape::Linear));

    // The rejection half of the same renderer, so the shared usage text cannot
    // drift on one path while staying right on the other. Several of these were
    // themselves wrong by exactly one trailing newline.
    for cmd in ["status", "rm", "merge-base", "rev-list", "diff-files", "range-diff"] {
        out.push(Case::strict(cmd, &[cmd, "--bogus"], Shape::Linear));
    }

    // An unrecognised flag must never become an operand.
    //
    // These are not cosmetic. Before the fix, `stash clear --zzbogus` **erased
    // every stash entry**, `stash drop --zzbogus` destroyed one, `switch --orphan
    // --zzbogus` emptied the index and deleted every tracked file, `worktree add
    // -b --zzbogus` created both a ref and a checked-out worktree, and
    // `branch -m -- -bad` renamed the *current branch* — each from a typo, each
    // exiting 0. The post-state probe is doing at least as much work as stdout
    // in this group, which is the point of choosing the destructive shapes.
    //
    // `-- -foo` is here because the leading-dash rule lives in `check_branch_ref`
    // (refs.c), ahead of `check_refname_format`, and gitoxide only implements the
    // latter — so `--` was not a way to smuggle the name past it.
    out.push(Case::strict("stash", &["stash", "clear", "--zzbogus"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "clear", "-Z"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "clear", "junkarg"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "drop", "--zzbogus"], Shape::Stashed));
    out.push(Case::strict("stash", &["stash", "drop", "-Z"], Shape::Stashed));
    out.push(Case::strict("branch", &["branch", "--bogus"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "--bogus", "newbr"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "newbr", "--bogus"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "-Z"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "--", "-foo"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "--", "HEAD"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "-m", "--", "-bad"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "-c", "--", "-bad2"], Shape::Linear));
    out.push(Case::strict("checkout", &["checkout", "-b", "--zzbogus"], Shape::Linear));
    out.push(Case::strict("checkout", &["checkout", "--orphan", "--zzbogus"], Shape::Linear));
    out.push(Case::strict("switch", &["switch", "-c", "--zzbogus"], Shape::Linear));
    out.push(Case::strict("switch", &["switch", "--orphan", "--zzbogus"], Shape::Linear));
    // Unique-prefix abbreviation and negation must keep resolving as git resolves
    // them, or the rejection above would be bought by refusing valid input.
    out.push(Case::strict("branch", &["branch", "--verb"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "--no-all"], Shape::Linear));
    out.push(Case::strict("branch", &["branch", "--end-of-options", "--bogus"], Shape::Linear));

    // Column padding, on the three verbs that carry their own formatter.
    //
    // `shortlog`, `reflog` and `format-rev` were each at 100% while refusing
    // padding outright, and stayed at 100% after it was wired up — because no
    // case here had ever typed `%<`. That gap let a panic reach a commit:
    // `log --format='%<(20)%Cred%%|'` hit an `unreachable!`, since a `%C` chain
    // hands the formatter the second half of a `%%` and the validator had only
    // checked the pair. Width is in display columns, so the CJK subject in
    // `AwkwardPaths` is the shape that matters, not the ASCII one.
    for cmd in ["shortlog", "reflog", "format-rev"] {
        out.push(Case::new(cmd, &[cmd, "--format=%<(20)%s|"], Shape::Branched));
        out.push(Case::new(cmd, &[cmd, "--format=%<(10,mtrunc)%s|"], Shape::AwkwardPaths));
        out.push(Case::new(cmd, &[cmd, "--format=%>>(20)%s|"], Shape::Branched));
    }
    out.push(Case::new("log", &["log", "--format=%<(20)%Cred%%s|"], Shape::Branched));
    out.push(Case::new("log", &["log", "--format=a%x00b%<|(20)%s|"], Shape::Branched));
    out.push(Case::new("log", &["log", "--format=%<(10,mtrunc)%s|"], Shape::AwkwardPaths));
    out.push(Case::new("log", &["log", "--format=%w(20,2,4)%s %s %s"], Shape::Branched));

    // The commands that must work with **no repository at all**.
    //
    // Every fixture root is a repository and `cwd` is relative to it, so "outside
    // a repository" looks inexpressible — but a ceiling stops the upward walk, and
    // `src/` has tracked files to actually search.
    //
    // The ceiling must be a **proper ancestor** of the working directory, not the
    // directory itself: `longest_ancestor_length()` (path.c:1263-1264) only accepts
    // a prefix when `path[len]` is `/` with something after it, so a ceiling equal
    // to the cwd matches nothing and discovery walks up as though it were unset.
    // These first shipped as `{repo}/src` with cwd `src` and therefore ran *inside*
    // the repository while claiming otherwise — they passed, but measured the wrong
    // thing. Verified with stock: ceiling `{repo}/src` from `src` gives
    // `--show-toplevel` = the repo at rc 0; ceiling `{repo}` gives
    // `fatal: not a git repository`, which is the premise these cases need.
    //
    // `grep --no-index` died `fatal: not a git repository` here until the repo
    // handle became optional, which also blocked `grep.fallbackToNoIndex` — a
    // config whose entire purpose is converting a repo-less invocation. Outside a
    // repository git reads no `.gitattributes` and no `info/exclude`, so these
    // pin that the walk's inputs are rebuilt rather than borrowed from a
    // repository that is not there.
    // Unique-prefix option abbreviation, which git accepts and 68 verbs refused.
    //
    // One case per *mechanism* rather than per verb: all of them now route through
    // one port of `parse_long_opt()`, so 68 rows would test one resolver 68 times.
    // What can independently break is each rule inside it.
    //
    // A plain unique prefix resolves.
    out.push(Case::strict("ls-files", &["ls-files", "--stag"], Shape::Dirty));
    out.push(Case::strict("status", &["status", "--porcel"], Shape::Dirty));
    out.push(Case::strict("cat-file", &["cat-file", "--batch-al"], Shape::Linear));
    // An ambiguous prefix is its own error, and git splits the streams oddly:
    // the `error:` goes to stderr and the usage block to *stdout*.
    out.push(Case::strict("show-ref", &["show-ref", "--hea"], Shape::Branched));
    out.push(Case::strict("branch", &["branch", "--col"], Shape::Linear));
    // The degenerate case: `--no-` names one option twice, which only a faithful
    // `register_abbrev` (called twice per entry here) reproduces.
    out.push(Case::strict("branch", &["branch", "--no-"], Shape::Linear));
    // `PARSE_OPT_NONEG` means the negation is *unknown*, not merely rejected.
    out.push(Case::strict("grep", &["grep", "--no-before-context", "pub"], Shape::Linear));
    out.push(Case::strict("merge-base", &["merge-base", "--no-octopus", "HEAD", "HEAD"], Shape::Branched));
    // An entry literally named `no-<stem>` owns *both* spellings — `--index` is
    // that entry unset, not a missing option.
    out.push(Case::strict("check-ignore", &["check-ignore", "--index", "src/lib.rs"], Shape::Attributes));
    out.push(Case::strict("update-index", &["update-index", "--no-assume-unchanged", "README.md"], Shape::Linear));
    // Short unknowns say "switch", long ones say "option" — four verbs had it
    // backwards, and nothing else distinguishes the two words.
    for cmd in ["describe", "for-each-ref", "merge-base", "format-rev"] {
        out.push(Case::strict(cmd, &[cmd, "-Z"], Shape::Branched));
    }
    // `get_value()` names the option *being parsed* first, not the one that came
    // first on the command line.
    out.push(Case::strict(
        "merge-base",
        &["merge-base", "--octopus", "--independent", "HEAD", "HEAD"],
        Shape::Branched,
    ));
    // A usage error must print the whole block, not just its first line.
    out.push(Case::strict("mv", &["mv"], Shape::Linear));

    // `--help-all`, one verb per mechanism.
    //
    // It is matched by a bare `strcmp` in `parse_options_step()`, sitting after
    // the `--`/`--end-of-options` breaks and before `parse_long_opt()`. So it
    // never abbreviates and never takes a value, and the two families differ on
    // *where* it may appear: a `parse_options()` verb answers it anywhere, while
    // a `show_usage_with_options_if_asked()` verb answers only when it is the
    // sole argument. Nothing but a second argument distinguishes them, which is
    // why each `--help-all` here has a `--quux` twin.
    for cmd in ["log", "show-ref", "ls-remote", "blame", "cat-file", "receive-pack"] {
        out.push(Case::strict(cmd, &[cmd, "--help-all"], Shape::Linear));
        out.push(Case::strict(cmd, &[cmd, "--help-all", "--quux"], Shape::Linear));
    }
    // `rev-parse` is the `ac == 2` family: with a second argument it is *not*
    // help at all and exits 0.
    out.push(Case::strict("rev-parse", &["rev-parse", "--help-all"], Shape::Linear));
    out.push(Case::strict("rev-parse", &["rev-parse", "--help-all", "--quux"], Shape::Linear));
    // A terminator must still win, and `fast-export` had this wrong.
    out.push(Case::strict("fast-export", &["fast-export", "--", "--help-all"], Shape::Linear));

    // Nothing to commit: git prints a *status report* on stdout and exits 1.
    //
    // The port answered `zvcs: commit: nothing to commit (no changes staged)` on
    // stderr — its own voice where git runs `run_status()` (commit.c:1081-1099)
    // — and that was the sole exit for every non-amend empty-commit path, not
    // just `-p`. The `-u` variants are here because the status honours them, so
    // they prove the report is really the status engine and not a fixed string.
    for args in [
        &["commit", "-m", "msg"][..],
        &["commit", "-p", "-m", "msg"],
        &["commit", "--interactive", "-m", "msg"],
        &["commit", "-m", "msg", "-u", "no"],
        &["commit", "-m", "msg", "-uall"],
    ] {
        out.push(Case::strict("commit", args, Shape::Linear));
    }

    // `OPT_CMDMODE` is `PARSE_OPT_NONEG`, and this shipped wrong four times:
    // every one of these negations was *accepted*. A unit test now asserts the
    // property against the real tables; these pin the user-visible answer.
    out.push(Case::strict("rebase", &["rebase", "--no-abort"], Shape::Branched));
    out.push(Case::strict("rebase", &["rebase", "--no-continue"], Shape::Branched));
    out.push(Case::strict("range-diff", &["range-diff", "--no-binary", "HEAD~1...HEAD"], Shape::Branched));
    out.push(Case::strict("show-branch", &["show-branch", "--no-reflog"], Shape::Branched));
    out.push(Case::strict("fetch", &["fetch", "--no-refetch"], Shape::BehindRemote));

    // The two parse-options failure arms, paired on one verb so neither can be
    // "fixed" into the other: an unknown option prints the usage block, a missing
    // value for a known option does not.
    out.push(Case::strict("restore", &["restore", "--bogus"], Shape::Dirty));
    out.push(Case::strict("restore", &["restore", "--conflict"], Shape::Dirty));
    out.push(Case::strict("am", &["am", "--bogus"], Shape::Linear));
    // Ambiguity puts its block on stdout, which is where a usage text one newline
    // short becomes visible.
    out.push(Case::strict("verify-commit", &["verify-commit", "--n"], Shape::Branched));
    // These answered `unsupported flag` at rc 1 where stock gives `unknown
    // option` and a block at 129 — the port's voice leaking into git's contract.
    out.push(Case::strict("merge", &["merge", "--no-ff-only"], Shape::Branched));
    out.push(Case::strict("push", &["push", "--bogus"], Shape::BehindRemote));
    out.push(Case::strict("tag", &["tag", "--bogus"], Shape::Branched));
    out.push(Case::strict("clone", &["clone", "--no-ipv4", ".", "dst"], Shape::Linear));
    // Unset semantics that *clear* rather than reject, which is the half of
    // negation handling a NONEG fix can easily overshoot.
    out.push(Case::strict("commit", &["commit", "-m", "x", "--no-message"], Shape::Dirty));
    // An abbreviated value option, reached through the verb git registers as an
    // alias of another (`stage` is `cmd_add`, sharing one table).
    out.push(Case::strict("stage", &["stage", "--pathspec-from-f", "nope"], Shape::Dirty));

    const CEIL_SRC: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}")];
    for args in [
        &["grep", "--no-index", "pub"][..],
        &["grep", "--no-index", "-n", "pub"],
        &["grep", "--no-index", "-l", "pub"],
        &["grep", "--no-index", "--exclude-standard", "pub"],
        // Escaping the tree and the flag conflict are both `fatal:` at 128.
        &["grep", "--no-index", "pub", "--", "../README.md"],
        &["grep", "--no-index", "--untracked", "pub"],
        &["-c", "grep.fallbackToNoIndex=true", "grep", "pub"],
        // No repository and no `--no-index`: the plain repo-less refusal.
        &["grep", "pub"],
    ] {
        out.push(Case::strict("grep", args, Shape::Linear).in_dir("src").with_env(CEIL_SRC));
    }


    // A few argument-validation paths that go one level past the usage check
    // and still cannot reach a socket or a peer.
    out.push(Case::new("daemon", &["daemon", "--port=not-a-number"], Shape::Linear));
    out.push(Case::new("daemon", &["daemon", "--timeout"], Shape::Linear));
    // `--inetd` serves the connection on stdin, which the harness closes.
    out.push(Case::new("daemon", &["daemon", "--inetd", "--port=1234"], Shape::Linear));
    out.push(Case::new("daemon", &["daemon", "--inetd", "--user=nobody"], Shape::Linear));
    out.push(Case::new("daemon", &["daemon", "--detach", "--inetd"], Shape::Linear));
    out.push(Case::new("http-fetch", &["http-fetch", "not-a-hex-oid"], Shape::Linear));
    out.push(Case::new("remote-ext", &["remote-ext", "only-one-arg"], Shape::Linear));
    // `status` and `stop` query an existing daemon; neither starts one.
    out.push(Case::new("fsmonitor--daemon", &["fsmonitor--daemon", "status"], Shape::Linear));
    out.push(Case::new("fsmonitor--daemon", &["fsmonitor--daemon", "stop"], Shape::Linear));
    out.push(Case::new("fsmonitor--daemon", &["fsmonitor--daemon", "no-such-sub"], Shape::Linear));
    out.push(Case::new("credential-cache", &["credential-cache", "exit"], Shape::Linear));
    out.push(Case::new("credential-cache", &["credential-cache", "no-such-op"], Shape::Linear));
    out.push(Case::new("p4", &["p4", "no-such-sub"], Shape::Linear));
    out.push(Case::new("quiltimport", &["quiltimport", "--patches", "/no/such/series"], Shape::Linear));
}
