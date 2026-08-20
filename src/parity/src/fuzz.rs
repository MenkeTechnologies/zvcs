//! Combinatorial flag fuzzing with deterministic seeding and shrinking.
//!
//! The corpus covers what a human thought to check. This covers what nobody
//! thought to check: flag combinations, argument orderings, and rev-spec forms
//! that a real caller will eventually produce.
//!
//! Determinism is a hard requirement — a parity failure nobody can reproduce is
//! not actionable. Every case is a pure function of `(seed, index)`, so a failing
//! run replays exactly from the seed printed in its report.

use crate::fixture::Shape;
use crate::runner::Case;

/// xorshift64*. Chosen for being reproducible and dependency-free rather than
/// statistically excellent — case selection does not need cryptographic quality.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is absorbing for xorshift; remap it.
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, num: u64, denom: u64) -> bool {
        self.next() % denom < num
    }

    /// A count biased toward the low end but with a real tail: most draws are
    /// small, yet `max` still comes up often enough to exercise deep stacking.
    fn count_upto(&mut self, max: usize) -> usize {
        // Two rolls, take the min — triangular, tail toward 0, but the full
        // range is reachable. Deep combinations stay rare without being absent.
        let a = self.below(max + 1);
        let b = self.below(max + 1);
        a.min(b).max(if self.chance(1, 6) { max } else { 0 })
    }
}

/// What a subcommand accepts, as a grammar the generator samples from.
pub struct Grammar {
    pub cmd: &'static str,
    /// Flags safe to combine freely.
    pub flags: &'static [&'static str],
    /// Positional arguments — revs, paths, or refs depending on the command.
    pub positionals: &'static [&'static str],
    /// Shapes this command is meaningful against.
    pub shapes: &'static [Shape],
}

const REV_SHAPES: &[Shape] = &[Shape::Linear, Shape::Branched, Shape::Merged, Shape::Detached];
const ALL_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Conflicted,
    Shape::Detached,
    Shape::AwkwardPaths,
];

/// Rev-specs worth throwing at anything that resolves one. Includes forms that
/// *should* fail, because agreeing on rejection is also parity, and the hard
/// forms git's own `rev-parse` grammar allows: peels, ranges, reflog walks,
/// `:path` object specs, `:/text` searches, and raw oids.
const REVS: &[&str] = &[
    "HEAD", "HEAD^", "HEAD^^", "HEAD^2", "HEAD~1", "HEAD~2", "HEAD~3",
    "HEAD^0", "HEAD^{}", "HEAD^{tree}", "HEAD^{commit}", "HEAD^{tag}",
    "main", "@", "@~1", "@{-1}", "HEAD@{0}", "HEAD@{1}", "HEAD@{now}",
    "main..HEAD", "main...HEAD", "HEAD~2..HEAD", "^HEAD",
    "HEAD:README.md", ":/fixture", ":0:src/lib.rs", "refs/heads/main",
    "0000000000000000000000000000000000000000", "deadbeef",
    "does-not-exist", "@{999}", "HEAD~999", "",
];

/// Path arguments including magic pathspecs, which have their own parser in git
/// and are a rich source of divergence.
const PATHS: &[&str] = &[
    "README.md", "src/lib.rs", "src", "src/", ".", "./README.md", "..",
    "*.md", "**/*.rs", "no/such/path",
    ":(glob)**/*.rs", ":(icase)readme.md", ":!src", ":(exclude)*.md",
    ":(top)README.md", ":(attr:text)", "with space.txt", "üñïçødé.txt",
];

/// Replacement values for `--flag=value` mutation: empty, boundary, overflow,
/// and garbage. A parser that only ever saw well-formed values in the corpus
/// meets malformed ones here.
const VALUES: &[&str] = &[
    "", "0", "1", "-1", "999999999", "99999999999999999999999999",
    "abc", "true", "false", "v1", "=", "%H%n", "\t", "0x10",
];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Every spelling git accepts for a boolean, plus none of the ones it does not.
///
/// `config.c:git_parse_maybe_bool_text` takes `true`/`false`, `yes`/`no`,
/// `on`/`off`, `one`/`zero`, and any integer — and a port that only recognises
/// `true`/`false` reads `off` as *set*, which is the inverted-flag class of bug
/// this list exists to find. Spelled out rather than reduced to `true`/`false`
/// because the spelling is the thing under test.
const BOOLS: &[&str] = &["true", "false", "yes", "no", "on", "off", "1", "0"];

/// Real git configuration keys, each paired with the values that are meaningful
/// *for that key*.
///
/// Keys, not a grammar: `core.abbrev` takes a width, `merge.conflictStyle` takes
/// one of three names, and crossing every key with every value would spend the
/// whole budget on combinations git rejects at parse time before any behaviour
/// runs. Each key's own list is where the *behaviour* differences live; the
/// generic [`CONFIG_EDGE_VALUES`] pool is mixed in on a minority of draws so the
/// malformed path is reached too.
///
/// Invalid values are deliberately present in both lists. A key that makes
/// stock git die must make zvcs die identically — that is a pass, and excluding
/// it would leave the port's config *validation* unmeasured, which is exactly
/// the half a port skips first.
const CONFIG_KEYS: &[(&str, &[&str])] = &[
    // core.*: the settings that change what a repository even looks like.
    ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "0", "1", "64"]),
    ("core.autocrlf", &["true", "false", "input"]),
    ("core.eol", &["lf", "crlf", "native"]),
    ("core.ignorecase", BOOLS),
    ("core.precomposeUnicode", BOOLS),
    ("core.quotePath", BOOLS),
    ("core.logAllRefUpdates", &["true", "false", "always"]),
    ("core.safecrlf", &["true", "false", "warn"]),
    ("core.symlinks", BOOLS),
    // `core.bare=true` over a worktree is one of the deaths worth agreeing on:
    // `setup.c` rejects the pair rather than honouring it.
    ("core.bare", BOOLS),
    ("core.fileMode", BOOLS),
    // diff.*: rename detection and hunk shape, none of which any case could set.
    ("diff.renames", &["true", "false", "copies", "copy"]),
    ("diff.renameLimit", &["0", "1", "2", "1000", "-1", "99999999999999999999999999"]),
    ("diff.algorithm", &["myers", "minimal", "patience", "histogram", "default"]),
    ("diff.context", &["0", "1", "3", "10", "-1"]),
    ("diff.mnemonicPrefix", BOOLS),
    ("diff.noprefix", BOOLS),
    ("diff.relative", &["true", "false", "src/", "no/such/"]),
    // log.*: the defaults every `log`/`show`/`whatchanged` invocation inherits.
    ("log.abbrevCommit", BOOLS),
    (
        "log.date",
        &[
            "relative", "local", "iso", "iso-strict", "rfc", "short", "raw", "human", "unix",
            "default", "format:%Y-%m-%d", "bogus",
        ],
    ),
    ("log.decorate", &["short", "full", "auto", "no", "true", "false"]),
    ("log.follow", BOOLS),
    ("log.showSignature", BOOLS),
    // status.*: what the most-read porcelain in git decides to print.
    ("status.showUntrackedFiles", &["no", "normal", "all", "bogus"]),
    ("status.short", BOOLS),
    ("status.branch", BOOLS),
    ("status.relativePaths", BOOLS),
    ("status.renames", &["true", "false", "copies"]),
    // `color.ui` is pinned off by `NO_COLOR` in the hardened environment, so
    // `always` here is the one way a case can ask for escape sequences at all.
    ("color.ui", &["auto", "always", "never", "true", "false"]),
    ("grep.patternType", &["basic", "extended", "fixed", "perl", "default", "bogus"]),
    ("grep.lineNumber", BOOLS),
    // Ref ordering, including the `-` prefix and the `version:` sort git parses
    // out of the same string.
    ("tag.sort", &["refname", "-refname", "version:refname", "taggerdate", "bogus"]),
    ("branch.sort", &["refname", "-refname", "committerdate", "bogus"]),
    ("versionsort.suffix", &["-pre", "-rc", "", "-"]),
    ("push.default", &["nothing", "matching", "simple", "upstream", "current", "tracking", "bogus"]),
    ("merge.conflictStyle", &["merge", "diff3", "zdiff3", "bogus"]),
    ("blame.date", &["iso", "short", "raw", "relative", "bogus"]),
    // `pretty.<name>` defines a format `--pretty=<name>` then resolves, so this
    // one key reaches the whole placeholder language from the config side.
    ("pretty.custom", &["%H", "%h %s", "format:%an", "tformat:%H", "%(bogus)"]),
];

/// Values thrown at *any* key, whatever it expects: empty, whitespace, garbage,
/// overflow, and the enum names that belong to some other key.
///
/// This is where the parse-failure paths live. A key's own list exercises what
/// the setting does; this one exercises what happens when it cannot.
const CONFIG_EDGE_VALUES: &[&str] = &[
    "", " ", "auto", "abc", "-1", "0", "1", "999999999", "99999999999999999999999999",
    "true", "false", "yes", "no", "on", "off", "none", "\t", "=", "%H",
];

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// The `GIT_*` variables [`crate::env::harden`] deliberately leaves unset, with
/// the values worth setting them to.
///
/// `harden` starts from `env_clear`, so every one of these is guaranteed absent
/// on both sides unless a case sets it — which is what makes sampling one purely
/// additive and keeps the two runs symmetric. None of them may be a pin;
/// [`sampled_env_vars_are_never_pinned`] asserts that against `env::is_pinned`
/// rather than trusting this list to stay correct, and `apply_case_env` asserts
/// it again per case.
///
/// Path-shaped values are written with [`crate::runner::REPO_PLACEHOLDER`], never
/// as a literal absolute path: the two sides run against copies at different
/// roots, and a literal would name one side's repository to both.
const ENV_VARS: &[(&str, &[&str])] = &[
    // Discovery: which repository, and which worktree, before anything else.
    ("GIT_DIR", &[".git", "{repo}/.git", ".", "no-such-dir"]),
    ("GIT_WORK_TREE", &[".", "{repo}", "src", "no-such-dir"]),
    ("GIT_CEILING_DIRECTORIES", &["{repo}", "{repo}/src", "{repo}/no-such-dir", ""]),
    // Ref and object storage redirection.
    ("GIT_NAMESPACE", &["ns", "a/b", "", "refs/heads"]),
    ("GIT_INDEX_FILE", &["{repo}/.git/index", "{repo}/.git/no-such-index", ".git/index"]),
    ("GIT_OBJECT_DIRECTORY", &["{repo}/.git/objects", "{repo}/.git/no-such-objects"]),
    // Pathspec interpretation. Git reads these as "set to anything non-empty",
    // so `0` and `false` are *on* — the inverted-flag trap, from the environment
    // side this time.
    ("GIT_LITERAL_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_ICASE_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_GLOB_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_NOGLOB_PATHSPECS", &["1", "0", "", "true", "false"]),
    // Advice, attributes, replacement, locking, and flush behaviour.
    ("GIT_ADVICE", &["0", "1", "false", "true"]),
    ("GIT_ATTR_NOSYSTEM", &["1", "0"]),
    ("GIT_NO_REPLACE_OBJECTS", &["1", "0"]),
    ("GIT_OPTIONAL_LOCKS", &["0", "1", "bogus"]),
    ("GIT_FLUSH", &["0", "1"]),
];

// ---------------------------------------------------------------------------
// Global options
// ---------------------------------------------------------------------------

/// The options `git.c:handle_options` parses before it dispatches a verb.
///
/// Each entry is one whole option including its argument, so the shrinker can
/// drop `-C src` without leaving `src` behind as a positional.
///
/// `--list-cmds=main` is here even though it is not an option any porcelain
/// caller writes: it terminates argument handling and prints a list instead of
/// running the subcommand at all, which is a path nothing else in the harness
/// reaches, and it is a known gap in the port that no case could previously
/// catch.
const GLOBAL_OPTIONS: &[&[&str]] = &[
    &["--no-pager"],
    &["-P"],
    &["--no-advice"],
    &["--no-optional-locks"],
    &["--no-replace-objects"],
    &["--literal-pathspecs"],
    &["--icase-pathspecs"],
    &["--glob-pathspecs"],
    &["--noglob-pathspecs"],
    &["--namespace=ns"],
    &["--namespace=a/b"],
    &["--namespace="],
    &["-C", "src"],
    &["-C", "."],
    &["-C", "no-such-dir"],
    &["--git-dir=.git"],
    &["--work-tree=."],
    &["--attr-source=HEAD"],
    &["--attr-source=does-not-exist"],
    &["--list-cmds=main"],
];

// ---------------------------------------------------------------------------
// Working directory
// ---------------------------------------------------------------------------

/// Directories every shape contains, because `git init` and the base commit
/// create them: the git dir and four of its subdirectories, plus the one tracked
/// subdirectory the base fixture writes (`src/lib.rs`).
///
/// `Shape::Dirty` deletes `src/lib.rs` but not `src/`, and `fixture::copy_tree`
/// recreates empty directories, so the tracked subdirectory survives in every
/// shape. The runner would create a missing directory on both sides anyway; the
/// point of listing only what exists is that a directory git *finds* is a
/// different discovery situation from one that was conjured for the case.
const COMMON_DIRS: &[&str] =
    &[".git", ".git/refs", ".git/refs/heads", ".git/objects", ".git/info", ".git/hooks", "src"];

/// Directories a particular shape adds, read off `fixture::build`.
///
/// These are the layouts that make discovery interesting and that no other shape
/// can express: the `.git`-file indirection of a submodule checkout, the
/// per-worktree admin directory of a linked worktree, and a bare repository.
fn shape_dirs(shape: Shape) -> &'static [&'static str] {
    match shape {
        Shape::Worktree => &["wt", ".git/worktrees", ".git/worktrees/wt"],
        Shape::Submodule => &["sub", ".git/modules", ".git/modules/sub"],
        Shape::BehindRemote => &[".remote.git", ".remote.git/refs", ".remote.git/objects"],
        Shape::AwkwardPaths => &["nested", "nested/deep"],
        Shape::Attributes => &["docs", "vendor", "logs", "assets", "sub"],
        Shape::NoIndexTrees => &["ni", "ni/da", "ni/db", "ni/addonly_a", "ni/delonly_a"],
        // `outside/` survives only because an untracked file is written into it
        // after the cone is applied; `outside/nested` does not.
        Shape::Sparse => &["inside", "inside/nested", "outside"],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// stdin
// ---------------------------------------------------------------------------

/// A tree entry naming the empty blob, whose id is a constant of the hash
/// function rather than of any fixture — so `mktree` gets a *valid* entry
/// without the case having to read an object id off disk at run time.
const P_TREE_ENTRY: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\n";
/// The same shape with an id that is not one.
const P_TREE_ENTRY_BAD: &[u8] = b"100644 blob notanoid\tREADME.md\n";
/// A commit message with a trailer block, for `interpret-trailers`.
const P_TRAILERS: &[u8] = b"subject line\n\nbody text\n\nSigned-off-by: A U Thor <author@example.invalid>\n";
/// A well-formed unified diff against a path the base fixture tracks, for
/// `patch-id`, `apply` and `am`.
const P_PATCH: &[u8] = b"diff --git a/README.md b/README.md\nindex 0000000..1111111 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixture\n+added line\n";
/// The same patch cut off mid-hunk.
const P_PATCH_TRUNCATED: &[u8] = b"diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixt";
/// Ref transactions for `update-ref --stdin`.
const P_REF_UPDATES: &[u8] = b"create refs/heads/parity-fuzz HEAD\n";
/// The same, where the second command must fail — an implementation without a
/// real transaction leaves the first ref behind.
const P_REF_UPDATES_FAIL: &[u8] = b"create refs/heads/parity-fuzz HEAD\ncreate refs/heads/main HEAD\n";
/// Revisions and object names, for `cat-file --batch`, `rev-list --stdin` and
/// `diff-tree --stdin`. Includes the null oid, which resolves to nothing.
const P_OIDS: &[u8] = b"HEAD\nHEAD^{tree}\n0000000000000000000000000000000000000000\n";
/// Paths, for `check-ignore --stdin`, `check-attr --stdin` and
/// `update-index --stdin`.
const P_PATHS: &[u8] = b"README.md\nsrc/lib.rs\nno/such/path\n";
/// The same paths NUL-separated, which is what every `-z` mode expects and what
/// a reader that splits on newline gets wrong.
const P_PATHS_NUL: &[u8] = b"README.md\0src/lib.rs\0no/such/path\0";
/// The same paths with CRLF line endings.
const P_PATHS_CRLF: &[u8] = b"README.md\r\nsrc/lib.rs\r\n";
/// One path with no trailing newline at all — the last-line-without-EOL case
/// that a line reader drops.
const P_PATH_NO_EOL: &[u8] = b"README.md";
/// An index-info line, for `update-index --index-info`.
const P_INDEX_INFO: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\t0\tREADME.md\n";
/// Nothing at all: immediate EOF, which is a different input from closed stdin
/// for anything that distinguishes "no payload" from "no pipe".
const P_EMPTY: &[u8] = b"";
/// Bytes that are not text, including embedded NULs and invalid UTF-8.
const P_BINARY: &[u8] = b"\x00\x01\x02\xff\xfe\n\x00garbage\x00\n";

/// Every payload, as one pool. Sampling draws an **index** into this table
/// rather than generating bytes, so a case's input is a compile-time literal and
/// the case replays byte-for-byte from its seed.
const STDIN_PAYLOADS: &[&[u8]] = &[
    P_TREE_ENTRY,
    P_TREE_ENTRY_BAD,
    P_TRAILERS,
    P_PATCH,
    P_PATCH_TRUNCATED,
    P_REF_UPDATES,
    P_REF_UPDATES_FAIL,
    P_OIDS,
    P_PATHS,
    P_PATHS_NUL,
    P_PATHS_CRLF,
    P_PATH_NO_EOL,
    P_INDEX_INFO,
    P_EMPTY,
    P_BINARY,
];

/// The payloads that are the *right shape* for a given subcommand.
///
/// Without this the pool is fifteen payloads wide and `mktree` would see a real
/// tree entry once in fifteen draws, so its parse path would be measured a
/// fifteenth as often as its reject path. The sampler prefers this list and
/// falls back to the whole pool on a minority of draws, so both are reached.
fn preferred_payloads(cmd: &str) -> &'static [&'static [u8]] {
    match cmd {
        "mktree" => &[P_TREE_ENTRY, P_TREE_ENTRY_BAD],
        "interpret-trailers" | "stripspace" | "mailinfo" | "mailsplit" | "fmt-merge-msg" => {
            &[P_TRAILERS, P_PATCH, P_EMPTY]
        }
        "patch-id" | "apply" | "am" => &[P_PATCH, P_PATCH_TRUNCATED, P_EMPTY],
        "update-ref" => &[P_REF_UPDATES, P_REF_UPDATES_FAIL],
        "cat-file" | "rev-list" | "diff-tree" | "name-rev" | "pack-objects" | "for-each-ref" => {
            &[P_OIDS, P_PATHS, P_EMPTY]
        }
        "check-ignore" | "check-attr" | "check-mailmap" | "hash-object" | "ls-files" => {
            &[P_PATHS, P_PATHS_NUL, P_PATHS_CRLF, P_PATH_NO_EOL]
        }
        "update-index" => &[P_INDEX_INFO, P_PATHS, P_PATHS_NUL],
        "unpack-objects" | "index-pack" | "show-index" | "get-tar-commit-id" => {
            &[P_BINARY, P_EMPTY]
        }
        _ => STDIN_PAYLOADS,
    }
}

/// Subcommands whose *entire* input is stdin, with no flag to ask for it.
///
/// Enumerated because there is no way to derive it: `git mktree` reads stdin
/// unconditionally while `git hash-object` reads it only under `--stdin`, and
/// the difference is in each command's source, not in its argv.
const STDIN_ALWAYS: &[&str] = &[
    "mktree",
    "mktag",
    "stripspace",
    "patch-id",
    "interpret-trailers",
    "fmt-merge-msg",
    "mailinfo",
    "mailsplit",
    "unpack-objects",
    "get-tar-commit-id",
    "show-index",
    "column",
    // `apply` and `am` read stdin when they are given no file operand; when they
    // are given one, they ignore it. Both sides ignore it identically, so
    // supplying it unconditionally costs nothing and covers the no-operand form.
    "apply",
    "am",
];

/// Whether the sampled invocation actually asks for input.
///
/// Two rules, and no guessing beyond them:
///
///  * the subcommand is one of [`STDIN_ALWAYS`], which read stdin with no flag;
///  * **or** the sampled argv contains a token that means "read stdin" —
///    `--stdin` and its `--stdin-paths`/`--stdin-packs` relatives, the bare `-`
///    operand, `--annotate-stdin`, `--index-info`, the `--batch` family that
///    `cat-file` drives from a request stream, and the `…-from-file=-` forms
///    that name stdin as a file.
///
/// Anything else gets closed stdin, which is what every generated case had
/// before. Feeding a payload to a command that does not read one would not make
/// the case wrong — both sides ignore it — but it would put a payload hash in
/// the case id that means nothing, and an id that lies about what a case does is
/// worse than a narrower rule.
fn wants_stdin(cmd: &str, args: &[String]) -> bool {
    STDIN_ALWAYS.contains(&cmd)
        || args.iter().any(|a| {
            a == "-"
                || a == "--stdin"
                || a.starts_with("--stdin=")
                || a.starts_with("--stdin-")
                || a == "--annotate-stdin"
                || a == "--index-info"
                || a.starts_with("--batch")
                || a.ends_with("-from-file=-")
        })
}

/// The hand-written grammars: read-only commands, described by hand because
/// their flag sets are worth stating deliberately.
///
/// These are **not** the whole fuzz corpus — [`all_grammars`] concatenates
/// [`crate::grammars_generated::generated`], which covers eighty-odd more
/// commands including mutating ones (`init`, `cherry-pick`, `rebase`, `revert`,
/// `submodule`, `gc`, `repack`, …). That was once untrue: fuzzing a mutating
/// command used to hang on an editor or a prompt, so the corpus carried
/// read-only grammars only. `env::harden` closed that by neutralizing every
/// interactive hook, and the generated grammars followed. The comment here
/// outlived the restriction it described, which is worth remembering the next
/// time this file explains what the harness does not do.
pub fn grammars() -> Vec<Grammar> {
    vec![
        Grammar {
            cmd: "rev-parse",
            flags: &[
                "--abbrev-ref", "--short", "--verify", "--quiet", "--git-dir",
                "--show-toplevel", "--is-inside-work-tree", "--is-bare-repository",
                "--symbolic", "--symbolic-full-name", "--all", "--branches", "--tags",
            ],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "status",
            flags: &[
                "--porcelain", "--porcelain=v1", "--short", "--branch", "--long",
                "--untracked-files=all", "--untracked-files=no", "--untracked-files=normal",
                "--ignored", "--no-renames", "--find-renames",
            ],
            positionals: &[],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "log",
            flags: &[
                "--oneline", "-1", "-2", "--max-count=3", "--format=%H", "--format=%h %s",
                "--pretty=oneline", "--pretty=short", "--pretty=format:%an", "--name-only",
                "--name-status", "--stat", "--graph", "--all", "--reverse", "--no-merges",
                "--merges", "--date-order", "--topo-order",
            ],
            positionals: &["HEAD", "main", ""],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "rev-list",
            flags: &[
                "--count", "--max-count=2", "--all", "--reverse", "--no-merges",
                "--merges", "--objects", "--parents", "--topo-order",
            ],
            positionals: &["HEAD", "main"],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "cat-file",
            flags: &["-t", "-s", "-p", "-e"],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "ls-tree",
            flags: &["-r", "-t", "-d", "--name-only", "--name-status", "--full-tree", "--abbrev=7", "-z"],
            positionals: &["HEAD", "HEAD^{tree}", "main"],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "ls-files",
            flags: &[
                "--cached", "--stage", "--modified", "--deleted", "--others",
                "--unmerged", "--full-name", "-z", "--abbrev",
            ],
            positionals: PATHS,
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "diff",
            flags: &[
                "--cached", "--staged", "--stat", "--shortstat", "--numstat",
                "--name-only", "--name-status", "--raw", "--no-color", "--unified=1",
                "--ignore-all-space", "--find-renames",
            ],
            positionals: &["", "HEAD", "HEAD~1"],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "show",
            flags: &["--oneline", "--no-patch", "--stat", "--name-only", "--format=%H", "--raw"],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "branch",
            flags: &["--list", "-a", "-r", "-v", "-vv", "--show-current", "--all", "--format=%(refname)"],
            positionals: &[""],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "tag",
            flags: &["--list", "-l", "-n", "--sort=refname", "--format=%(refname:short)"],
            positionals: &["", "v0.*"],
            shapes: &[Shape::Branched, Shape::Linear],
        },
        Grammar {
            cmd: "describe",
            flags: &["--always", "--tags", "--all", "--long", "--abbrev=7", "--dirty"],
            positionals: &["", "HEAD"],
            shapes: &[Shape::Branched, Shape::Linear, Shape::Dirty],
        },
        Grammar {
            cmd: "config",
            flags: &["--list", "--get", "--get-all", "--local", "--name-only"],
            positionals: &["core.bare", "user.name", "no.such.key"],
            shapes: &[Shape::Linear],
        },
        Grammar {
            cmd: "blame",
            flags: &["--porcelain", "--line-porcelain", "-s", "-l", "--show-name"],
            positionals: &["README.md", "src/lib.rs"],
            shapes: &[Shape::Linear, Shape::Branched],
        },
    ]
}

/// Every grammar the fuzzer draws from: the hand-written ones above, plus the
/// per-command grammars generated from git's own documentation.
fn all_grammars() -> Vec<Grammar> {
    let mut all = grammars();
    all.extend(crate::grammars_generated::generated());
    all
}

/// Generate `per_cmd` cases for each grammar from `seed`.
pub fn generate(seed: u64, per_cmd: usize) -> Vec<Case> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for g in all_grammars() {
        for _ in 0..per_cmd {
            out.push(sample(&mut rng, &g));
        }
    }
    out
}

/// Replace the `=value` of a `--flag=value` token with an edge-case value, or
/// return the flag unchanged. Flags without `=` are left alone. This is how a
/// value parser meets empty / overflow / garbage inputs it never saw curated.
fn mutate_value(rng: &mut Rng, flag: &str) -> String {
    match flag.split_once('=') {
        Some((name, _)) if rng.chance(1, 3) => format!("{name}={}", rng.pick(VALUES)),
        _ => flag.to_string(),
    }
}

/// Draw `-c key=value` overrides.
///
/// Most cases get none: configuration is a second axis, and crossing it into
/// every case would leave the argv axis measured only under a perturbed git.
/// When it fires, one to three keys are drawn, which is enough for two settings
/// to interact (`diff.renames` with `diff.renameLimit`, `status.short` with
/// `status.branch`) without the shrinker having five to peel off.
fn sample_config(rng: &mut Rng) -> Vec<(String, String)> {
    if !rng.chance(1, 3) {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for _ in 0..=rng.below(3) {
        let (key, own) = *rng.pick(CONFIG_KEYS);
        // Two thirds from the key's own values, one third from the generic edge
        // pool: the first measures what the setting *does*, the second measures
        // what happens when it cannot be parsed.
        let value = if rng.chance(2, 3) { *rng.pick(own) } else { *rng.pick(CONFIG_EDGE_VALUES) };
        // A repeated key is not a duplicate — git applies `-c` in order and the
        // last one wins, which is its own parse path — but a repeated *pair* is,
        // so only exact repeats are skipped.
        let pair = (key.to_string(), value.to_string());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

/// Draw extra environment variables.
///
/// Rarer than configuration because each one redirects discovery or storage
/// wholesale — `GIT_DIR` decides which repository the command is even talking
/// about — so a high rate would drown the other dimensions in cases that all
/// fail for the same reason. At most two, and never the same variable twice:
/// a second draw of one key would silently win over the first and the case id
/// would name a setting that never applied.
fn sample_env(rng: &mut Rng) -> Vec<(String, String)> {
    if !rng.chance(1, 5) {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for _ in 0..=rng.below(2) {
        let (key, values) = *rng.pick(ENV_VARS);
        if out.iter().any(|(k, _)| k == key) {
            continue;
        }
        out.push((key.to_string(), rng.pick(values).to_string()));
    }
    out
}

/// Draw global options to place before the subcommand.
fn sample_globals(rng: &mut Rng) -> Vec<Vec<String>> {
    if !rng.chance(1, 3) {
        return Vec::new();
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    for _ in 0..=rng.below(2) {
        let opt: Vec<String> = rng.pick(GLOBAL_OPTIONS).iter().map(|t| t.to_string()).collect();
        if !out.contains(&opt) {
            out.push(opt);
        }
    }
    out
}

/// Draw a working directory the sampled shape actually contains, or the fixture
/// root.
///
/// Drawn from [`COMMON_DIRS`] plus [`shape_dirs`] as one flat range so a shape
/// that adds layouts of its own — a linked worktree, a submodule, a bare
/// repository — reaches them proportionally rather than needing a second roll.
fn sample_cwd(rng: &mut Rng, shape: Shape) -> Option<&'static str> {
    // Most cases stay at the root: the argv dimensions are what the grammars
    // describe, and running them all from inside `.git` would measure discovery
    // over and over instead of the commands.
    if !rng.chance(1, 6) {
        return None;
    }
    let extra = shape_dirs(shape);
    let i = rng.below(COMMON_DIRS.len() + extra.len());
    Some(if i < COMMON_DIRS.len() { COMMON_DIRS[i] } else { extra[i - COMMON_DIRS.len()] })
}

/// Build one invocation. Far more aggressive than a flag or two: it stacks
/// **repeated** flags (deep enough to trip re-parse and last-wins bugs), mutates
/// flag values, supplies **multiple** positionals, interleaves flags and
/// positionals in argument order, and injects a `--` separator — every degree
/// of freedom a real caller eventually exercises and none of which the corpus
/// covers. Still a pure function of the RNG, so any failure replays from its
/// seed.
///
/// argv is only one of the dimensions a git invocation has, and for a long time
/// it was the only one sampled: configuration, environment, global options,
/// working directory and stdin were fixed at "none" for every generated case, so
/// the whole of `handle_options`, of `git_config_get_*`, and of discovery was
/// reachable from curated cases alone. Each of those is now drawn here, on its
/// own probability, so a generated case is a point in the whole space rather
/// than in one axis of it.
fn sample(rng: &mut Rng, g: &Grammar) -> Case {
    // Drawn first because the working directory depends on it: only the
    // directories a shape actually contains are candidates.
    let shape = *rng.pick(g.shapes);

    // Up to 6 flags, WITH repetition allowed. Repeats are not dilution: a
    // re-declared flag is exactly what surfaces last-wins and re-parse bugs.
    let mut flag_tokens: Vec<String> = Vec::new();
    if !g.flags.is_empty() {
        for _ in 0..rng.count_upto(6) {
            let flag = *rng.pick(g.flags);
            flag_tokens.push(mutate_value(rng, flag));
        }
    }

    // Up to 3 positionals, repetition allowed (`git log HEAD HEAD` is valid and
    // has its own behavior). Empty positionals are dropped, not emitted.
    let mut pos_tokens: Vec<String> = Vec::new();
    if !g.positionals.is_empty() {
        for _ in 0..rng.count_upto(3) {
            let p = *rng.pick(g.positionals);
            if !p.is_empty() {
                pos_tokens.push(p.to_string());
            }
        }
    }

    let mut args = vec![g.cmd.to_string()];

    // Ordering: usually flags-then-positionals as a caller writes it, but a
    // fraction of the time interleave them, which tests that option parsing does
    // not depend on flags preceding operands (git's does not; a buggy port's
    // might). A `--` separator is injected before the positionals sometimes,
    // both with and without interleaving.
    let sep = !pos_tokens.is_empty() && rng.chance(1, 4);
    if rng.chance(1, 3) && !flag_tokens.is_empty() && !pos_tokens.is_empty() {
        // Interleave by draining the two lists in a random order.
        let mut fi = flag_tokens.into_iter().peekable();
        let mut pi = pos_tokens.into_iter().peekable();
        let mut sep_done = !sep;
        while fi.peek().is_some() || pi.peek().is_some() {
            let take_flag = match (fi.peek().is_some(), pi.peek().is_some()) {
                (true, false) => true,
                (false, true) => false,
                _ => rng.chance(1, 2),
            };
            if take_flag {
                args.push(fi.next().unwrap());
            } else {
                if !sep_done {
                    args.push("--".to_string());
                    sep_done = true;
                }
                args.push(pi.next().unwrap());
            }
        }
        if !sep_done {
            // No positional was emitted after all; nothing to separate.
        }
    } else {
        args.extend(flag_tokens);
        if sep {
            args.push("--".to_string());
        }
        args.extend(pos_tokens);
    }

    // stdin is fed only where the sampled argv (or the subcommand itself) asks
    // for it — see `wants_stdin` for the two rules that decide.
    let stdin = wants_stdin(g.cmd, &args).then(|| {
        // Two thirds from the payloads shaped for this command, one third from
        // the whole pool, so the parse path and the reject path are both reached.
        let pool =
            if rng.chance(2, 3) { preferred_payloads(g.cmd) } else { STDIN_PAYLOADS };
        *rng.pick(pool)
    });

    Case {
        cmd: g.cmd,
        args,
        config: sample_config(rng),
        globals: sample_globals(rng),
        shape,
        stdin,
        // stderr stays uncompared for generated cases. Opting in is a statement
        // that a particular message *is* the behaviour, which is a curated
        // judgement; asserting it across sampled argv would compare prose the
        // port is specified not to reproduce.
        compare_stderr: false,
        cwd: sample_cwd(rng, shape),
        env: sample_env(rng),
    }
}

/// Greedily drop one element at a time from a vector-valued dimension of a case,
/// keeping every drop that still fails.
///
/// `field` picks the vector out of a case; it is a plain function pointer rather
/// than a closure so the same walk serves `args`, `config`, `globals` and `env`
/// without four copies of the index bookkeeping. `from` is the first droppable
/// index — 1 for `args`, whose element 0 is the subcommand and is never dropped.
fn drop_each<T: Clone>(
    best: &mut Case,
    from: usize,
    field: fn(&mut Case) -> &mut Vec<T>,
    still_fails: &mut dyn FnMut(&Case) -> bool,
) {
    let mut i = from;
    while i < field(best).len() {
        let mut candidate = best.clone();
        field(&mut candidate).remove(i);
        if still_fails(&candidate) {
            *best = candidate; // keep index: the list shifted left under us
        } else {
            i += 1;
        }
    }
}

/// Shrink a failing case to a minimal still-failing one by greedily dropping
/// one sampled fact at a time. `still_fails` re-runs the candidate; the
/// subcommand at `args[0]` is never dropped.
///
/// Reported failures are worth far more minimized: a three-flag failure usually
/// reduces to one flag, which names the actual defect. That argument applies to
/// every dimension the fuzzer samples, not only to argv — a failure reported
/// with five config keys, two environment variables, a working directory and a
/// stdin payload attached is worth much less than the same failure reported with
/// the one of them that is responsible, and while argv was the only dimension
/// sampled it was also the only one that needed peeling.
///
/// Order is coarsest first. The whole-fact dimensions (stdin, cwd) are single
/// drops that often remove the failure's entire premise, and the environment
/// redirects discovery wholesale, so trying them before the token-by-token walk
/// through argv means the expensive walk usually runs on an already-smaller case.
pub fn shrink(case: &Case, still_fails: &mut dyn FnMut(&Case) -> bool) -> Case {
    let mut best = case.clone();

    // Closed stdin and the fixture root are the *defaults* every case had before
    // these dimensions existed, so falling back to them is a real minimization
    // and not a different case.
    if best.stdin.is_some() {
        let candidate = Case { stdin: None, ..best.clone() };
        if still_fails(&candidate) {
            best = candidate;
        }
    }
    if best.cwd.is_some() {
        let candidate = Case { cwd: None, ..best.clone() };
        if still_fails(&candidate) {
            best = candidate;
        }
    }

    drop_each(&mut best, 0, |c| &mut c.env, still_fails);
    drop_each(&mut best, 0, |c| &mut c.globals, still_fails);
    drop_each(&mut best, 0, |c| &mut c.config, still_fails);
    drop_each(&mut best, 1, |c| &mut c.args, still_fails);
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sampled environment may only *add* variables, never re-point one of
    /// `env::harden`'s pins. The runner asserts this per case, which catches it
    /// at run time; this catches it at `cargo test` time, before a pool entry
    /// that would abort every case gets committed.
    #[test]
    fn sampled_env_vars_are_never_pinned() {
        for (key, values) in ENV_VARS {
            assert!(!crate::env::is_pinned(key), "{key} is pinned by env::harden");
            assert!(!values.is_empty(), "{key} has no values to draw from");
            for value in *values {
                // The two sides run against copies at different roots, so a
                // literal absolute path would name one side's repository to both.
                assert!(
                    !value.starts_with('/'),
                    "{key}={value} must use the repo placeholder, not an absolute path"
                );
            }
        }
    }

    /// Every config key has values, and no value is an absolute path — config
    /// goes into argv unsubstituted, so a literal root would name one side's
    /// copy to both.
    #[test]
    fn config_pool_is_well_formed() {
        for (key, values) in CONFIG_KEYS {
            assert!(key.contains('.'), "{key} is not a section.key");
            assert!(!values.is_empty(), "{key} has no values to draw from");
            assert!(values.iter().all(|v| !v.starts_with('/')), "{key} names an absolute path");
        }
    }

    /// A payload is only attached where the invocation asks for it, by one of
    /// the two rules `wants_stdin` documents — and never anywhere else, because
    /// a stdin hash in a case id that the command never reads is a lie about
    /// what the case does.
    #[test]
    fn stdin_is_attached_only_where_it_is_read() {
        let argv = |ts: &[&str]| ts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert!(wants_stdin("mktree", &argv(&["mktree"])));
        assert!(wants_stdin("update-ref", &argv(&["update-ref", "--stdin"])));
        assert!(wants_stdin("cat-file", &argv(&["cat-file", "--batch-check"])));
        assert!(wants_stdin("hash-object", &argv(&["hash-object", "--stdin"])));
        assert!(wants_stdin("name-rev", &argv(&["name-rev", "--annotate-stdin"])));
        assert!(wants_stdin("update-index", &argv(&["update-index", "--index-info"])));
        assert!(wants_stdin("commit", &argv(&["commit", "--pathspec-from-file=-"])));
        assert!(wants_stdin("stripspace", &argv(&["stripspace"])));

        assert!(!wants_stdin("status", &argv(&["status", "--porcelain"])));
        assert!(!wants_stdin("hash-object", &argv(&["hash-object", "README.md"])));
        assert!(!wants_stdin("log", &argv(&["log", "--oneline"])));
    }

    /// Generation is a pure function of `(seed, per_cmd)`: the same seed must
    /// produce the same case ids, argument for argument, or a reported failure
    /// cannot be replayed. This is the property every new sampled dimension is
    /// most likely to break, since each one draws from the same stream.
    #[test]
    fn generation_is_reproducible_from_its_seed() {
        let ids = |seed: u64| -> Vec<String> {
            generate(seed, 3).iter().map(|c| c.id()).collect()
        };
        assert_eq!(ids(0x5A5A_C0DE), ids(0x5A5A_C0DE));
        assert_ne!(ids(1), ids(2), "different seeds must explore different points");
    }

    /// The new dimensions actually fire, and land in the case id where the
    /// report and `scripts/split_failures.pl` can see them. A probability that
    /// silently rounded to zero would leave the whole widening unmeasured while
    /// every number in the report stayed plausible.
    #[test]
    fn every_sampled_dimension_is_reachable() {
        let cases = generate(4242, 6);
        assert!(cases.iter().any(|c| !c.config.is_empty()), "no case sampled a config key");
        assert!(cases.iter().any(|c| !c.globals.is_empty()), "no case sampled a global option");
        assert!(cases.iter().any(|c| !c.env.is_empty()), "no case sampled an environment variable");
        assert!(cases.iter().any(|c| c.cwd.is_some()), "no case sampled a working directory");
        assert!(cases.iter().any(|c| c.stdin.is_some()), "no case sampled a stdin payload");

        // Rendered, not merely stored.
        let with_config = cases.iter().find(|c| !c.config.is_empty()).unwrap();
        assert!(with_config.id().contains("-c "), "config missing from {}", with_config.id());
        let with_cwd = cases.iter().find(|c| c.cwd.is_some()).unwrap();
        assert!(with_cwd.id().contains("::cwd["), "cwd missing from {}", with_cwd.id());
        let with_env = cases.iter().find(|c| !c.env.is_empty()).unwrap();
        assert!(with_env.id().contains("::env["), "env missing from {}", with_env.id());
    }

    /// The id grammar `scripts/split_failures.pl` parses — `<shape>::<cmd>::` off
    /// the front, both segments free of whitespace — survives every dimension,
    /// so a widened fuzzer does not silently stop being triageable.
    #[test]
    fn case_ids_keep_the_shape_and_command_segments_first() {
        for case in generate(4242, 4) {
            let id = case.id();
            let (shape, rest) = id.trim_start_matches('!').split_once("::").expect("shape segment");
            let (cmd, _) = rest.split_once("::").expect("command segment");
            assert!(!shape.contains(char::is_whitespace), "shape segment has a space: {id}");
            assert!(!cmd.contains(char::is_whitespace), "command segment has a space: {id}");
            assert_eq!(cmd, case.cmd);
        }
    }

    /// Shrinking minimizes every dimension, not only argv. The oracle here calls
    /// a case failing whenever it still carries the one config key that matters,
    /// so a shrinker that only walked `args` would report all five facts.
    #[test]
    fn shrink_minimizes_the_sampled_dimensions() {
        let case = Case::new("status", &["status", "--short", "--branch"], Shape::Linear)
            .with_config(&[("core.abbrev", "4"), ("status.short", "true")])
            .with_globals(&[&["--no-pager"], &["-C", "src"]])
            .with_env(&[("GIT_NAMESPACE", "ns")])
            .in_dir(".git");
        let case = Case { stdin: Some(P_PATHS), ..case };

        let minimal = shrink(&case, &mut |c| {
            c.config.iter().any(|(k, _)| k == "status.short")
        });

        assert_eq!(minimal.config, vec![("status.short".to_string(), "true".to_string())]);
        assert!(minimal.globals.is_empty());
        assert!(minimal.env.is_empty());
        assert_eq!(minimal.cwd, None);
        assert_eq!(minimal.stdin, None);
        assert_eq!(minimal.args, vec!["status".to_string()]);
        assert_eq!(minimal.size(), 1);
    }
}
