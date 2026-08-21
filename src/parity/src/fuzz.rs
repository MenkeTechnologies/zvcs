//! Combinatorial flag fuzzing with deterministic seeding and shrinking.
//!
//! The corpus covers what a human thought to check. This covers what nobody
//! thought to check: flag combinations, argument orderings, and rev-spec forms
//! that a real caller will eventually produce.
//!
//! Determinism is a hard requirement — a parity failure nobody can reproduce is
//! not actionable. Every case is a pure function of `(seed, index)`, so a failing
//! run replays exactly from the seed printed in its report.
//!
//! # What a configuration draw costs
//!
//! Configuration used to be one dimension with one delivery mechanism: a key, a
//! value, and `-c`. It is now two — *which keys* and *which scope each one comes
//! from* — and the scope half means some draws write files into the fixture. That
//! is the only new cost in this file, and it is worth stating what it is before
//! reading the sampler that spends it.
//!
//! **The price.** A draw that lands on a file scope costs one `open`/`write`/
//! `close` of a few hundred bytes, per side, per case, into a directory tree the
//! runner has just created and deletes when the case ends. At most five such
//! writes — one per file scope — and none at all for a draw that stays on `-c`
//! or on `GIT_CONFIG_KEY_<n>`, which is most of them. No extra child process, no
//! extra fixture template, no extra comparison, and not one more case in the
//! parity denominator: a scoped case is the same single invocation it was, read
//! under a premise that was put in place before it ran.
//!
//! **Why this is the cheap shape.** The two alternatives are both more
//! expensive, and the first is also wrong:
//!
//!  * *Install each key by running `git config --file …` first.* One child
//!    process per key per side, and on the zvcs side it is the **implementation
//!    under test** writing the premise. A port with a broken config writer would
//!    corrupt its own premise, and the case would then be measuring the writer
//!    twice instead of measuring the key once — which is the one thing a
//!    differential harness must never do. Writing the bytes from Rust means both
//!    sides start from a file this crate produced, byte for byte.
//!  * *A fixture template per scope.* Twenty-two shapes crossed with the scope
//!    combinations, built at start-up and hashed, so that a four-line text file
//!    could exist. The premise is smaller than the machinery.
//!
//! The two sides live at different roots, which is the problem [`Case::cwd`] and
//! [`crate::runner::REPO_PLACEHOLDER`] already solve for directories and
//! environment values; the file scopes solve it the same way, by naming a path
//! *relative to the side's own fixture root* and letting the runner resolve it
//! per side ([`crate::runner::scope_file`]). A case names a scope and never a
//! path, which is also what keeps the two synthetic scopes compatible with the
//! hardened environment — see [`crate::runner::ConfigScope`].

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope, Sequence};

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
// Configuration: interacting key sets
// ---------------------------------------------------------------------------

/// Shared value pools for the key sets below, so a key that takes a colour and
/// a key that takes an expiry do not both end up drawing from one generic list.
///
/// Each is spelled with the forms git's own parser distinguishes rather than
/// with representatives: `never`/`always`/`auto` are three *different* branches
/// of `git_config_colorbool`, and a pool holding only `true`/`false` would
/// measure the bool branch three times and the colour branches never.
const COLORBOOL: &[&str] = &["auto", "always", "never", "true", "false", "no", "bogus", ""];
/// Colour specifications, including the ones that are not.
const COLOR: &[&str] =
    &["red", "bold blue", "normal", "reverse", "black green", "#ff0000", "12", "256", "bogus", ""];
/// Integers, including the two that are not integers and the one that overflows.
const INT: &[&str] =
    &["0", "1", "2", "3", "-1", "7", "40", "1000", "99999999999999999999999999", "abc", ""];
/// Expiry dates. `never` and `now` are special-cased before `approxidate` runs,
/// and `all`/`false`/`bogus` are rejected — three different outcomes from one
/// parser.
const EXPIRY: &[&str] =
    &["never", "now", "2.weeks.ago", "1.day.ago", "all", "bogus", "", "false", "3.months.ago"];
/// Byte sizes, which git parses with a `k`/`m`/`g` suffix.
const SIZE: &[&str] = &["0", "1", "16k", "2m", "1g", "512", "-1", "bogus", ""];

/// A set of configuration keys that only matter **together**.
///
/// The single-key sampler could never reach an interaction, and interactions are
/// where a port's configuration handling actually breaks: reading `diff.renames`
/// correctly and ignoring `diff.renameLimit` produces the right answer on every
/// case that sets one of them. Each group below is a set of keys that one
/// decision reads, so drawing two or three of them at once is what puts the
/// decision under test rather than the parse.
///
/// Two sources fed this table, and both are cited per group: git's own
/// documented interactions, and — more usefully — which keys each config
/// callback in `src/extensions/src/{default_config,diff_config,status_config,
/// log_config,cmd_config}.rs` reads *in one arm or under one guard*. The second
/// source names interactions no documentation states: `status_config.rs:208-225`
/// parses `diff.renameLimit` only while `st.rename_limit` is still `-1`, so a
/// preceding `status.renameLimit` makes a later garbage `diff.renameLimit`
/// silently acceptable — a two-key fact that no one-key draw can express.
struct ConfigGroup {
    /// Slug for the test that asserts the table is well formed. Not rendered
    /// into a case id: the id names the keys that were actually drawn, which is
    /// what a reader needs, and a group name would be one more token that does
    /// not narrow anything — which is also why the field is read only by
    /// `cargo test`, and says so rather than being silently warned about.
    #[allow(dead_code)]
    name: &'static str,
    keys: &'static [(&'static str, &'static [&'static str])],
}

/// The interacting sets.
///
/// A key may appear in more than one group; that is not duplication, it is the
/// point. `core.abbrev` interacts with `log.abbrevCommit` in one decision and
/// with `core.disambiguate` in another, and a table that listed it once would
/// have to pick which of the two interactions to leave unmeasured.
const CONFIG_GROUPS: &[ConfigGroup] = &[
    // `core.eol` is only consulted for a file `core.autocrlf` did not already
    // decide, and `core.safecrlf` decides whether an irreversible conversion is
    // a warning or a refusal — three keys, one conversion. The port validates
    // all three independently and leaves `core.eol`'s arm empty
    // (`default_config.rs:350`, `:355`, `:364`), so any disagreement here is a
    // disagreement about the conversion rather than about the parse.
    ConfigGroup {
        name: "crlf",
        keys: &[
            ("core.autocrlf", &["true", "false", "input"]),
            ("core.eol", &["lf", "crlf", "native", "bogus"]),
            ("core.safecrlf", &["true", "false", "warn"]),
            ("core.checkRoundtripEncoding", &["SHIFT-JIS", "UTF-16", "bogus", ""]),
            ("core.filemode", BOOLS),
        ],
    },
    // The four prefix keys are read as four independent strings/bools
    // (`diff_config.rs:189-201`) and resolved against each other only when a
    // diff is actually rendered: `diff.noprefix` wins over an explicit
    // `diff.srcPrefix`, and `diff.mnemonicPrefix` replaces both with a letter
    // that depends on which side of the comparison a file is. `format.noprefix`
    // is the fifth, with its own refusal path (`log_config.rs:164-173`).
    ConfigGroup {
        name: "diff-prefix",
        keys: &[
            ("diff.noprefix", BOOLS),
            ("diff.srcPrefix", &["a/", "src/", "", "x", "a b/"]),
            ("diff.dstPrefix", &["b/", "dst/", "", "y", "c d/"]),
            ("diff.mnemonicPrefix", BOOLS),
            ("format.noprefix", BOOLS),
            ("diff.relative", &["true", "false", "src/", "no/such/"]),
        ],
    },
    // `color.ui` decides whether the per-command keys are consulted at all, and
    // a per-command key decides whether its slots are. The slot tables return
    // *before* the value is parsed for a slot they do not know
    // (`diff_config.rs:283-289`, `status_config.rs:229-235`), so a bad colour is
    // only reachable by pairing a real slot with it — and `color.grep`'s table is
    // the exception that rejects the unknown slot itself
    // (`cmd_config.rs:183-189`). Both halves need two keys to reach.
    ConfigGroup {
        name: "color-cascade",
        keys: &[
            ("color.ui", COLORBOOL),
            ("color.diff", COLORBOOL),
            ("diff.color", COLORBOOL),
            ("color.status", COLORBOOL),
            ("color.branch", COLORBOOL),
            ("color.grep", COLORBOOL),
            ("color.diff.meta", COLOR),
            ("color.diff.frag", COLOR),
            ("color.diff.whitespace", COLOR),
            ("color.status.header", COLOR),
            ("color.status.bogusSlot", COLOR),
            ("color.grep.filename", COLOR),
            ("color.grep.bogusSlot", COLOR),
            ("color.decorate.branch", COLOR),
            ("color.advice.hint", COLOR),
        ],
    },
    // `--short` output has no branch header unless `status.branch` asks for one,
    // and `status.aheadBehind` decides whether that header costs a revision walk.
    // The port reads all four out of one bool arm (`status_config.rs:166-173`),
    // which is exactly why a one-key draw cannot tell a port that honours the
    // combination from one that honours each key alone.
    ConfigGroup {
        name: "status-render",
        keys: &[
            ("status.short", BOOLS),
            ("status.branch", BOOLS),
            ("status.aheadBehind", BOOLS),
            ("status.showStash", BOOLS),
            ("status.showUntrackedFiles", &["no", "normal", "all", "1", "off", "bogus"]),
            ("status.relativePaths", BOOLS),
            ("status.displayCommentPrefix", BOOLS),
            ("status.submoduleSummary", &["true", "false", "0", "5", "bogus"]),
        ],
    },
    // The `-1` guards: `diff.renames` and `diff.renameLimit` are read only while
    // the status defaults are still unset, so a `status.*` spelling drawn first
    // changes what a later `diff.*` spelling does — including making an
    // otherwise-fatal value silently acceptable (`status_config.rs:208-225`).
    ConfigGroup {
        name: "rename-detection",
        keys: &[
            ("diff.renames", &["true", "false", "copies", "copy", ""]),
            ("status.renames", &["true", "false", "copies", "copy", ""]),
            ("diff.renameLimit", INT),
            ("status.renameLimit", INT),
            ("merge.renames", BOOLS),
            ("merge.renameLimit", INT),
        ],
    },
    // `push.default=simple`/`upstream` are *defined* in terms of
    // `branch.<name>.merge` and `branch.<name>.remote`, and `remote.pushDefault`
    // overrides the branch's own remote. `push.default` alone can only ever be
    // measured against a branch with no upstream configured.
    ConfigGroup {
        name: "push-upstream",
        keys: &[
            (
                "push.default",
                &["nothing", "matching", "simple", "upstream", "current", "tracking", "bogus"],
            ),
            ("branch.main.merge", &["refs/heads/main", "refs/heads/other", "main", ""]),
            ("branch.main.remote", &["origin", "gen", ".", ""]),
            ("branch.main.pushRemote", &["origin", "gen", ""]),
            ("remote.pushDefault", &["origin", "gen", ""]),
            ("push.autoSetupRemote", BOOLS),
            ("branch.autoSetupMerge", &["true", "false", "always", "inherit", "simple"]),
        ],
    },
    // `feature.manyFiles` is not a setting, it is a *bundle*: it turns on
    // `index.skipHash`, `index.version=4` and `core.untrackedCache`, and an
    // explicit key must beat the bundle. That is only observable when both are
    // set, and the direction it must win in is the whole content of the feature.
    ConfigGroup {
        name: "index-features",
        keys: &[
            ("feature.manyFiles", BOOLS),
            ("feature.experimental", BOOLS),
            ("index.skipHash", BOOLS),
            ("index.version", &["2", "3", "4", "0", "5", "bogus"]),
            ("index.recordEndOfIndexEntries", BOOLS),
            ("index.recordOffsetTable", BOOLS),
            ("core.untrackedCache", &["true", "false", "keep", "bogus"]),
            ("core.fsmonitor", BOOLS),
        ],
    },
    // `gc.auto` decides whether the automatic run happens at all;
    // `gc.autoPackLimit` decides what it does when it does; `gc.autoDetach`
    // decides whether the caller waits for it. The port reads them as one flat
    // sequence (`cmd_config.rs:458-472`) with no key gating another, which is
    // itself worth comparing against git, where `gc.auto=0` makes the rest inert.
    ConfigGroup {
        name: "gc-auto",
        keys: &[
            ("gc.auto", INT),
            ("gc.autoPackLimit", INT),
            ("gc.autoDetach", BOOLS),
            ("gc.bigPackThreshold", SIZE),
            ("gc.cruftPacks", BOOLS),
            ("gc.maxCruftSize", SIZE),
            ("gc.aggressiveWindow", INT),
            ("gc.aggressiveDepth", INT),
            ("gc.packRefs", &["true", "false", "notbare", "bogus"]),
        ],
    },
    // Two expiry vocabularies in one command. `gc.reflogExpireUnreachable` is
    // only consulted once `gc.reflogExpire` has resolved
    // (`cmd_config.rs:453-455`), and the prune trio uses a *different* rule from
    // the reflog pair — `approxidate` strictly in the past
    // (`cmd_config.rs:479-482`, `:555-564`) — so `all` is legal to one and
    // rejected by the other.
    ConfigGroup {
        name: "gc-expiry",
        keys: &[
            ("gc.reflogExpire", EXPIRY),
            ("gc.reflogExpireUnreachable", EXPIRY),
            ("gc.pruneExpire", EXPIRY),
            ("gc.worktreePruneExpire", EXPIRY),
            ("gc.logExpiry", EXPIRY),
        ],
    },
    // `branch.<name>.rebase` overrides `pull.rebase` for one branch, and
    // `pull.ff=only` contradicts both. A single key can only ever measure the
    // fallback.
    ConfigGroup {
        name: "pull-rebase",
        keys: &[
            ("pull.rebase", &["true", "false", "merges", "interactive", "bogus"]),
            ("branch.main.rebase", &["true", "false", "merges", "interactive", "bogus"]),
            ("pull.ff", &["true", "false", "only", "bogus"]),
            ("merge.ff", &["true", "false", "only", "bogus"]),
            ("pull.twohead", &["ort", "recursive", "bogus"]),
            ("rebase.autoStash", BOOLS),
            ("merge.autoStash", BOOLS),
        ],
    },
    // `rerere.enabled` decides whether a conflict is *recorded*, and
    // `merge.conflictStyle` decides what the recorded markers look like, so the
    // two together decide what `.git/rr-cache` ends up containing — which
    // `runner::probe_rr_cache` compares. `rerere.autoUpdate` then decides whether
    // the resolution is staged, which changes the index the next step reads.
    ConfigGroup {
        name: "merge-conflict",
        keys: &[
            ("merge.conflictStyle", &["merge", "diff3", "zdiff3", "bogus"]),
            ("rerere.enabled", BOOLS),
            ("rerere.autoUpdate", BOOLS),
            ("merge.ff", &["true", "false", "only", "bogus"]),
            ("merge.verbosity", INT),
            ("merge.log", &["true", "false", "20", "bogus"]),
            ("merge.branchdesc", BOOLS),
        ],
    },
    // Every id in a `log` line is abbreviated to the same width, and the width is
    // `core.abbrev` — but only when `log.abbrevCommit` asked for abbreviation at
    // all. `log.decorate` and `log.date` then decide what else is on the line.
    // The port validates `log.decorate` nowhere (`log_config.rs:209`, an empty
    // arm) and `log.date` as a bare string (`:203`), so both are only measurable
    // through what the command prints.
    ConfigGroup {
        name: "log-render",
        keys: &[
            ("log.abbrevCommit", BOOLS),
            ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "0", "1", "64"]),
            ("log.decorate", &["short", "full", "auto", "no", "true", "false", "bogus"]),
            (
                "log.date",
                &["relative", "local", "iso", "iso-strict", "short", "raw", "human", "unix",
                  "default", "format:%Y-%m-%d", "bogus"],
            ),
            ("log.follow", BOOLS),
            ("log.showRoot", BOOLS),
            ("log.mailmap", BOOLS),
            (
                "log.diffMerges",
                &["off", "none", "on", "first-parent", "separate", "combined", "dense-combined",
                  "remerge", "bogus"],
            ),
        ],
    },
    // One shared range check with three entry points (`default_config.rs:559-566`):
    // `-1` is exempt, `0..=9` is legal, everything else is fatal — and the message
    // names whichever key was written, so a port that normalises them to one key
    // reports the wrong one.
    ConfigGroup {
        name: "zlib-levels",
        keys: &[
            ("core.compression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
            ("core.looseCompression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
            ("pack.compression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
        ],
    },
    // `diff.colorMoved` accepts the whole boolean grammar before its seven mode
    // names (`diff_config.rs:405-423`), and `diff.colorMovedWS`'s second error
    // fires only when `allow-indentation-change` co-occurs with a whitespace
    // token (`:464-470`) — a combination no single token can reach.
    ConfigGroup {
        name: "diff-moved",
        keys: &[
            (
                "diff.colorMoved",
                &["no", "plain", "blocks", "zebra", "default", "dimmed-zebra", "true", "false",
                  "bogus"],
            ),
            (
                "diff.colorMovedWS",
                &["no", "ignore-space-change", "ignore-space-at-eol", "ignore-all-space",
                  "allow-indentation-change", "allow-indentation-change,ignore-all-space", ""],
            ),
            (
                "diff.wsErrorHighlight",
                &["none", "default", "all", "new", "old", "context", "new,old", "bogus"],
            ),
            ("diff.indentHeuristic", BOOLS),
            ("diff.algorithm", &["myers", "minimal", "patience", "histogram", "default", "bogus"]),
            ("diff.context", &["0", "1", "3", "10", "-1"]),
            ("diff.interHunkContext", &["0", "1", "3", "-1"]),
        ],
    },
    // `core.whitespace` is one key whose *tokens* contradict each other — only
    // the `tab-in-indent` + `indent-with-non-tab` pair is fatal
    // (`default_config.rs:475-491`) — and `apply.whitespace` decides what `apply`
    // does about the rules `core.whitespace` set.
    ConfigGroup {
        name: "whitespace-rules",
        keys: &[
            (
                "core.whitespace",
                &["trailing-space", "space-before-tab", "indent-with-non-tab", "tab-in-indent",
                  "tab-in-indent,indent-with-non-tab", "tabwidth=4", "tabwidth=0", "tabwidth=99",
                  "-trailing-space", "blank-at-eof", "bogus"],
            ),
            ("apply.whitespace", &["nowarn", "warn", "fix", "error", "error-all", "bogus"]),
            ("apply.ignoreWhitespace", &["no", "change", "bogus"]),
            ("core.autocrlf", &["true", "false", "input"]),
        ],
    },
    // `submodule.active` is a pathspec and `submodule.<name>.active` is a bool
    // that overrides it for one submodule; `.gitmodules` supplies `path` and
    // `url` and `.git/config` overrides both. This is the group the
    // [`ConfigScope::Modules`] scope exists for — every key here is one git will
    // read out of `.gitmodules`, and no other key is.
    ConfigGroup {
        name: "submodule",
        keys: &[
            ("submodule.sub.path", &["sub", "other", "", "sub/"]),
            ("submodule.sub.url", &["./sub", "../sub", "", "https://example.invalid/x"]),
            ("submodule.sub.active", BOOLS),
            ("submodule.sub.ignore", &["all", "dirty", "untracked", "none", "bogus"]),
            ("submodule.sub.update", &["checkout", "rebase", "merge", "none", "bogus"]),
            ("submodule.sub.branch", &["main", ".", "", "bogus"]),
            ("submodule.active", &["sub", ":(glob)**", ".", "", "no/such"]),
            ("submodule.recurse", BOOLS),
            ("submodule.fetchJobs", INT),
        ],
    },
    // `diff.submodule` and `status.submoduleSummary` decide how a submodule's
    // change is *rendered*, and `diff.ignoreSubmodules` decides whether it is
    // rendered at all. The port dies on a bad `diff.ignoreSubmodules` and only
    // warns on a bad `diff.submodule` (`diff_config.rs:223-240`), and the warning
    // is latched process-wide (`:105-116`), so the pair's outcome depends on
    // which one the command reads first.
    ConfigGroup {
        name: "submodule-render",
        keys: &[
            ("diff.ignoreSubmodules", &["all", "untracked", "dirty", "none", "bogus"]),
            ("diff.submodule", &["log", "short", "diff", "bogus"]),
            ("status.submoduleSummary", &["true", "false", "0", "5", "bogus"]),
            ("submodule.recurse", BOOLS),
            ("fetch.recurseSubmodules", &["true", "false", "on-demand", "bogus"]),
        ],
    },
    // `fetch.prune` and `fetch.pruneTags` are separate switches over one refspec
    // walk, and `fetch.parallel`/`fetch.recurseSubmodules` decide how many
    // processes do it. `fetch.output` then decides whether any of it is printed.
    ConfigGroup {
        name: "fetch",
        keys: &[
            ("fetch.all", BOOLS),
            ("fetch.prune", BOOLS),
            ("fetch.pruneTags", BOOLS),
            ("fetch.showForcedUpdates", BOOLS),
            ("fetch.recurseSubmodules", &["true", "false", "on-demand", "bogus"]),
            ("fetch.parallel", INT),
            ("fetch.output", &["full", "compact", "bogus"]),
            ("remote.origin.prune", BOOLS),
            ("remote.origin.tagOpt", &["--tags", "--no-tags", "bogus"]),
        ],
    },
    // `grep.patternType` and `grep.extendedRegexp` are two spellings of one
    // choice with a documented precedence between them, and the rest decide what
    // a matching line looks like.
    ConfigGroup {
        name: "grep",
        keys: &[
            ("grep.patternType", &["default", "basic", "extended", "fixed", "perl", "bogus"]),
            ("grep.extendedRegexp", BOOLS),
            ("grep.lineNumber", BOOLS),
            ("grep.column", BOOLS),
            ("grep.fullName", BOOLS),
            ("grep.threads", INT),
            ("grep.fallbackToNoIndex", BOOLS),
        ],
    },
    // `core.abbrev` sets the width and `core.disambiguate` decides which objects
    // a short id is allowed to name, so the two together decide whether a given
    // prefix resolves at all. `core.checkStat` is in the same callback and shares
    // its fall-through (`default_config.rs:284-292`).
    ConfigGroup {
        name: "abbrev-disambiguate",
        keys: &[
            ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "false", "0", "1", "64", "true"]),
            (
                "core.disambiguate",
                &["none", "commit", "committish", "tree", "treeish", "blob", "bogus"],
            ),
            ("log.abbrevCommit", BOOLS),
            ("core.checkStat", &["default", "minimal", "bogus"]),
        ],
    },
    // Path interpretation: whether a name is folded, decomposed, quoted or
    // refused. `core.precomposeUnicode` and `core.protectHFS` disagree about the
    // same byte sequences, and `core.quotePath` decides whether the disagreement
    // is even visible in the output.
    ConfigGroup {
        name: "path-handling",
        keys: &[
            ("core.ignoreCase", BOOLS),
            ("core.precomposeUnicode", BOOLS),
            ("core.protectHFS", BOOLS),
            ("core.protectNTFS", BOOLS),
            ("core.quotePath", BOOLS),
            ("core.symlinks", BOOLS),
            ("core.fileMode", BOOLS),
        ],
    },
    // Two spellings of one setting, each in its own arm. A port that normalises
    // them to one key reports the wrong name in the error, and a port that
    // implements only one silently ignores the other — neither is visible unless
    // both spellings are drawn.
    ConfigGroup {
        name: "alias-spellings",
        keys: &[
            ("diff.suppressBlankEmpty", BOOLS),
            ("diff.suppress-blank-empty", BOOLS),
            ("pager.color", BOOLS),
            ("color.pager", BOOLS),
            ("core.commentChar", &["#", ";", "auto", "", "ab", "\n"]),
            ("core.commentString", &["#", ";", "auto", "", "--"]),
            ("repack.writeBitmaps", BOOLS),
            ("pack.writeBitmaps", BOOLS),
            ("diff.color", COLORBOOL),
            ("color.diff", COLORBOOL),
        ],
    },
    // The repository's own format, which decides whether the rest of the
    // configuration is even read: an unknown `extensions.*` at format version 1
    // makes git refuse the repository outright, and `core.bare` over a worktree
    // is a refusal `setup.c` takes before any command runs.
    ConfigGroup {
        name: "repo-format",
        keys: &[
            ("core.repositoryFormatVersion", &["0", "1", "2", "bogus", ""]),
            ("extensions.worktreeConfig", BOOLS),
            ("extensions.objectFormat", &["sha1", "sha256", "bogus"]),
            ("extensions.refStorage", &["files", "reftable", "bogus"]),
            ("extensions.noSuchExtension", BOOLS),
            ("core.bare", BOOLS),
            ("core.logAllRefUpdates", &["true", "false", "always"]),
        ],
    },
    // Ref ordering and the `versionsort.suffix` list `version:` sorting consults,
    // which is inert unless a `version:` sort is actually asked for.
    ConfigGroup {
        name: "ref-ordering",
        keys: &[
            ("tag.sort", &["refname", "-refname", "version:refname", "taggerdate", "bogus"]),
            ("branch.sort", &["refname", "-refname", "committerdate", "bogus"]),
            ("versionsort.suffix", &["-pre", "-rc", "", "-"]),
            ("versionsort.prereleaseSuffix", &["-pre", "-rc", ""]),
            ("column.ui", &["never", "always", "auto", "column", "row", "plain", "bogus"]),
            ("column.branch", &["never", "always", "auto", "bogus"]),
            ("column.tag", &["never", "always", "auto", "bogus"]),
        ],
    },
];

/// Lines written verbatim into a scope's file that git's parser **rejects**,
/// each verified against stock 2.55.0 to produce
/// `fatal: bad config line <n> in file <path>`.
///
/// Unreachable any other way. `-c` is handed a key and a value that have already
/// been split, so no `-c` can express a section header that never closes, a
/// value whose quote never closes, or a `]` with nothing in front of it — and
/// none of those produce a *line number*, which is the diagnostic a file has and
/// a command line does not.
///
/// The bad-escape entry carries its own `[core]` header because a `\q` is only a
/// bad escape once the line is being read as a value; appended to a file with no
/// section it would fail earlier, with a different message, and the case would
/// be measuring a different refusal than the one it names.
const CONFIG_BAD_LINES: &[&str] = &[
    "garbage line",
    "[core",
    "[bad section]",
    "]",
    "[]",
    "[core]]",
    "= 1",
    "x = \"unterminated",
    "[core \"a\" b]",
    "[core]\n\tabbrev = \"bad\\qescape\"",
];

/// Lines that are *legal* and that `-c` still cannot produce.
///
/// The malformed pool above measures the refusal; this one measures the parser
/// on inputs it accepts and that no other scope can deliver: a key with no value
/// at all (which is boolean **true**, while `-c key=` is the empty string — the
/// asymmetry `default_config.rs:823-830` implements), a trailing comment, a
/// backslash continuation that joins two lines into one value, a section and a
/// key on one line, and case folding of both the section and the key.
///
/// Each was checked against stock 2.55.0: the continuation yields `45` from
/// `4\`+`5`, the folded spelling yields `4`, and the valueless key yields the
/// empty rendering `--get` gives a true boolean.
const CONFIG_ODD_LINES: &[&str] = &[
    "[core]\n\tabbrev",
    "[core]\n\tabbrev = 4 # comment",
    "[core]\n\tabbrev = 4 ; comment",
    "# comment only",
    "; comment only",
    "[core]\n\tabbrev = 4\\\n5",
    "[CORE]\n\tABBREV = 4",
    "[core] abbrev = 4",
    "[branch \"a.b\"]\n\tmerge = refs/heads/main",
    "\n",
];

/// The scopes a given key may be delivered from.
///
/// `.gitmodules` is not a general configuration file: `submodule-config.c` reads
/// `submodule.*` out of it and nothing else, so putting `core.abbrev` there
/// would write a file git parses and then discards — a case that looks like it
/// set a key and set nothing. Every other key is deliverable from every scope,
/// which is what makes the scope the variable under test rather than a property
/// of the key.
fn scopes_for(key: &str) -> &'static [ConfigScope] {
    if key.starts_with("submodule.") {
        ConfigScope::ALL
    } else {
        // Every scope git layers, which for a key that is not a submodule key is
        // every scope there is: `ConfigScope::ALL` differs from this only by
        // `Modules`, which is the one this branch exists to exclude.
        ConfigScope::ORDERED
    }
}

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

/// Draw the scope one entry is delivered from.
///
/// `primary` is the scope the draw as a whole leans toward, and the two thirds
/// bias toward it is the difference between measuring two things:
///
///  * **Same scope, twice.** Two entries for one key in one *file* is the
///    last-value-wins rule, which every scope implements separately — the file
///    parser by overwriting as it reads, `-c` by appending to a parameter list
///    that is walked in order. Without the bias, a two-entry draw would land in
///    one file only about a sixth of the time and the rule would be measured a
///    sixth as often as it is worth measuring.
///  * **Different scopes.** One key in two scopes is *precedence*, which is the
///    single thing a `-c`-only harness could never see at all. The remaining
///    third is what produces it.
///
/// Both are wanted, so neither is chosen: the bias buys the first without
/// closing off the second.
fn sample_scope(rng: &mut Rng, key: &str, primary: ConfigScope) -> ConfigScope {
    let allowed = scopes_for(key);
    // The `chance` roll is taken before the membership test, so the RNG consumes
    // the same amount whether or not `primary` is admissible for this key — a
    // draw whose stream position depended on the key would make an id
    // irreproducible from its seed.
    let take_primary = rng.chance(2, 3);
    if take_primary && allowed.contains(&primary) {
        primary
    } else {
        *rng.pick(allowed)
    }
}

/// Draw the configuration one case runs under: which keys, which values, and
/// **which scope each one is delivered from**.
///
/// Most cases get none: configuration is a second axis, and crossing it into
/// every case would leave the argv axis measured only under a perturbed git.
///
/// When it fires, two thirds of the draws take their keys from one
/// [`CONFIG_GROUPS`] entry rather than independently from [`CONFIG_KEYS`]. That
/// is the whole point of the group table: two keys drawn independently out of a
/// forty-key pool are almost never the two that interact, so a sampler that only
/// ever did that would spend its whole budget proving that unrelated settings do
/// not interfere. The remaining third keeps drawing independently, because the
/// flat pool is wider than the groups and a key that belongs to no group would
/// otherwise become unreachable.
///
/// One to three keys, as before, and **repetition is allowed**: the same key
/// drawn twice with two values is the last-wins premise, and it is only a
/// duplicate when the scope, the key *and* the value all repeat, which measures
/// nothing the first one did not.
///
/// A raw line is appended on a minority of draws, always into a file scope,
/// because a line is a thing only a file has — half of them malformed (the
/// line-numbered refusal that `-c` cannot produce) and half legal-but-
/// unreachable (a valueless key, a continuation, a trailing comment).
///
/// It goes *after* the settings rather than being shuffled among them, and the
/// reason is the legal half rather than the malformed half. A legal file-only
/// line is a setting like any other, so putting it last is what makes it the one
/// that wins when it names a key an earlier stanza also named — the same
/// last-wins question, asked of the form only a file can express. The malformed
/// half aborts the whole read wherever it sits, so its position is not a choice
/// anybody has.
fn sample_config(rng: &mut Rng) -> Vec<ConfigEntry> {
    if !rng.chance(1, 3) {
        return Vec::new();
    }
    let primary = *rng.pick(ConfigScope::ALL);
    let group = if rng.chance(2, 3) { Some(rng.pick(CONFIG_GROUPS)) } else { None };

    let mut out: Vec<ConfigEntry> = Vec::new();
    for _ in 0..=rng.below(3) {
        let (key, own) = match group {
            Some(g) => *rng.pick(g.keys),
            None => *rng.pick(CONFIG_KEYS),
        };
        // Two thirds from the key's own values, one third from the generic edge
        // pool: the first measures what the setting *does*, the second measures
        // what happens when it cannot be parsed.
        let value = if rng.chance(2, 3) { *rng.pick(own) } else { *rng.pick(CONFIG_EDGE_VALUES) };
        let scope = sample_scope(rng, key, primary);
        let entry = ConfigEntry::set(scope, key, value);
        if !out.contains(&entry) {
            out.push(entry);
        }
    }

    if rng.chance(1, 5) {
        let scope = *rng.pick(ConfigScope::FILES);
        let pool = if rng.chance(1, 2) { CONFIG_BAD_LINES } else { CONFIG_ODD_LINES };
        out.push(ConfigEntry::raw(scope, *rng.pick(pool)));
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

    let args = sample_argv(rng, g, 6, 3);

    // stdin is fed only where the sampled argv (or the subcommand itself) asks
    // for it — see `wants_stdin` for the two rules that decide.
    let stdin = sample_stdin(rng, g.cmd, &args);

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

/// Draw the argv of one invocation: the subcommand, its flags, its positionals,
/// and the order they are written in.
///
/// Split out of [`sample`] because a *step* of a generated sequence needs
/// exactly this and nothing else — a [`crate::runner::Step`] carries argv and
/// stdin, and shape, config, globals, cwd and environment live on the sequence's
/// envelope. Writing a second argv sampler for sequences would mean a second
/// place where the flag-repetition, value-mutation, interleaving and `--`
/// rules live, and the two would drift; the whole reason the sequence generator
/// draws from [`Grammar`] at all is to avoid a second flag table, and a second
/// *sampler* is the same mistake one level up.
///
/// `max_flags`/`max_pos` are the only difference between the two callers.
/// [`sample`] passes the historical `6`/`3`; the sequence generator passes small
/// numbers, because a mutation buried under six stacked flags usually dies at
/// parse time, and a step that died at parse time leaves the steps after it
/// observing a repository nothing wrote to. The draw *order* is unchanged either
/// way, and [`Rng::count_upto`] consumes the same three values whatever its
/// bound, so an existing seed still produces the case ids it produced before
/// this split.
fn sample_argv(rng: &mut Rng, g: &Grammar, max_flags: usize, max_pos: usize) -> Vec<String> {
    // Up to `max_flags` flags, WITH repetition allowed. Repeats are not
    // dilution: a re-declared flag is exactly what surfaces last-wins and
    // re-parse bugs.
    let mut flag_tokens: Vec<String> = Vec::new();
    if !g.flags.is_empty() {
        for _ in 0..rng.count_upto(max_flags) {
            let flag = *rng.pick(g.flags);
            flag_tokens.push(mutate_value(rng, flag));
        }
    }

    // Up to `max_pos` positionals, repetition allowed (`git log HEAD HEAD` is
    // valid and has its own behavior). Empty positionals are dropped, not
    // emitted.
    let mut pos_tokens: Vec<String> = Vec::new();
    if !g.positionals.is_empty() {
        for _ in 0..rng.count_upto(max_pos) {
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
    args
}

/// The payload for an invocation that asks for one, and `None` for one that does
/// not. Split out of [`sample`] for the same reason [`sample_argv`] is: a
/// sequence step needs the identical rule, and a second copy of it would let a
/// step be handed input the command never reads — which is exactly what
/// [`wants_stdin`] documents as worse than a narrower rule.
fn sample_stdin(rng: &mut Rng, cmd: &str, args: &[String]) -> Option<&'static [u8]> {
    wants_stdin(cmd, args).then(|| {
        // Two thirds from the payloads shaped for this command, one third from
        // the whole pool, so the parse path and the reject path are both reached.
        let pool = if rng.chance(2, 3) { preferred_payloads(cmd) } else { STDIN_PAYLOADS };
        *rng.pick(pool)
    })
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

// ===========================================================================
// Generated sequences
// ===========================================================================
//
// Everything above generates **one** invocation against a pristine fixture.
// Everything git gets wrong twice is a *second* invocation reading what a first
// one wrote. `corpus/sequences.rs` closes that with twenty-odd hand-written
// workflows, and it closed it well — the first run of the curated sequence
// corpus found eight defects, five of them reflog messages no single case could
// see. But that corpus is exactly the artifact the single-case fuzzer exists to
// supplement: it covers what a human thought to check. This section covers what
// nobody thought to check, in the dimension where this port has historically
// broken.
//
// # Why a random chain of commands is not a sequence
//
// The naive generator draws N invocations and calls them a workflow. `git tag`
// followed by `git ls-files` is two independent cases wearing a sequence's
// clothes: it costs two invocations and two state probes per side and measures
// nothing the two cases would not have measured apart, because neither step
// reads anything the other wrote. Worse, it *looks* like coverage — the run
// prints more sequences, the invocation count goes up, and the number nobody may
// tune upward has been tuned upward with noise.
//
// So the dependency between step N and step N+1 is not left to chance; it is the
// thing being generated. Three families, and each one names the dependency it
// encodes:
//
//  * [`STOPPERS`] — **state-machine walks.** Park the repository in an
//    interrupted operation, then walk the resumption verbs. Every verb after the
//    first reads `.git/sequencer/`, `.git/rebase-merge/`, `.git/rebase-apply/`,
//    `CHERRY_PICK_HEAD`, `REVERT_HEAD` or `MERGE_HEAD` — state the entry step
//    wrote and nothing else could have. **Illegal transitions are drawn
//    deliberately**, not tolerated: `rebase --continue` with a cherry-pick in
//    progress, `--skip` after `--abort`, `--quit` twice. A port implements the
//    legal transitions because the documentation lists them; the illegal ones it
//    invents, and an invented refusal is indistinguishable from a correct one
//    until something compares it.
//  * [`MUTATORS`] — **mutate then observe.** One sampled mutating invocation,
//    then the readers whose answer it should have changed. `for-each-ref` after
//    a ref write, `stash list` after a stash push, `reflog` after anything that
//    moves `HEAD`, `ls-files --stage` after an index write. A wrong write is
//    caught by a right read, at the step that read it. This family also covers
//    the case a curated corpus never writes down: when the sampled mutation
//    *fails*, the observers assert that a failed mutation changed **nothing** —
//    which is where half-applied writes live and which no single case can see,
//    because a single case only ever looks at the repository once.
//  * [`ROUND_TRIPS`] — **an operation and its inverse.** `stash push`/`pop`,
//    `worktree add`/`remove`, `branch -m` and back, `sparse-checkout
//    set`/`disable`. The inverse operates on state the forward step created, and
//    the end state must equal the start state on both sides — so a `remove` that
//    half-cleans, or an `unset` that leaves a stanza behind, is a state
//    difference at the step that failed to clean rather than a mystery later.
//
// # What is drawn from the grammars and what cannot be
//
// Flags, positionals and shapes come from [`all_grammars`] via [`sample_argv`] —
// there is no second flag table here, and a grammar widened tomorrow widens
// these sequences the same day.
//
// Three things are stated here because no grammar encodes them, and each is a
// property of git rather than of a command line:
//
//  * **Which invocation stops.** A grammar says `cherry-pick` takes a rev; it
//    does not say that `cherry-pick theirs` on [`Shape::Conflicted`] conflicts
//    while `cherry-pick HEAD` does not. Drawing the entry step uniformly from the
//    grammar would leave nearly every walk resuming an operation that was never
//    started, which is one already-covered refusal repeated a thousand times.
//    [`STOPPERS`] therefore names premises the curated corpus has already proven
//    stop — and then decorates them with grammar-drawn flags, so the entry
//    invocation is not fixed and the walks that *do* fail to stop are reached
//    anyway.
//  * **The resumption alphabet.** `--continue`/`--skip`/`--abort`/`--quit` are
//    the transitions of `sequencer.c`'s state machine. The union of that
//    alphabet with each command's own grammar is taken rather than the
//    intersection, in both directions: the generated grammars carry `--abort`,
//    `--quit` and `--skip` for the sequencer commands but not `--continue`, so an
//    intersection would drop the single most important verb, while a fixed list
//    alone would never reach `am --retry`, which only `am`'s grammar knows about.
//  * **Which verbs mutate.** Nothing in a grammar says whether a command writes.
//    [`MUTATORS`] is a classification, and its criterion is stated there.
//
// # Cost
//
// One generated sequence costs its own step count in invocations and state
// probes **per side**, exactly as a curated one does — see
// [`crate::runner::Sequence`] for why that is the cheap shape and why the first
// divergence ends the run.
//
// The count is `--fuzz-sequences` per *entry point*, mirroring the single-case
// rule of `--fuzz` per grammar: every stopper, every mutator and every
// round-trip pair is drawn at least once at 1, so raising the knob deepens
// coverage uniformly instead of deepening whatever the RNG happened to favour.
// Entry points number [`STOPPERS`] + 1 (bisect) + [`MUTATORS`] + [`ROUND_TRIPS`],
// and steps average five to seven, so the family costs roughly five to six
// invocations per side per entry point per unit of the knob. `main` prints the
// exact sequence and invocation counts for the run it is about to do, which is
// the number to trust: a figure written here would be stale the first time an
// entry point is added.
//
// `--fuzz-sequences 0` turns the family off for a cheap argv-only sweep, and the
// knob defaults to `--fuzz` so a caller who does not care has one knob. It has
// to be a separate knob at all because the two corpora have very different unit
// prices — one invocation against one — and a reader who wants a deep argv sweep
// should not be made to buy a six-fold sequence bill to get it.
//
// # Determinism
//
// The sequence stream is seeded from `seed` mixed with [`SEQUENCE_STREAM`], so it
// is independent of the single-case stream: a sequence failure replays from its
// seed at any `--fuzz`, which it would not if both families drew from one RNG and
// `--fuzz` decided how far along it the sequences started.

/// Mixed into the seed so generated sequences draw from a stream the single-case
/// generator cannot shift. Arbitrary odd 64-bit constant; only its fixedness
/// matters.
const SEQUENCE_STREAM: u64 = 0xD1B5_4A32_D192_ED03;

/// A premise that parks a repository in an interrupted operation.
///
/// `setup` is run first and is *itself compared*, so by the time `entry` runs the
/// premise has been proven identical on both sides rather than assumed — the same
/// argument `corpus/sequences.rs` makes for doing setup in steps rather than in a
/// shape.
struct Stopper {
    /// Headline verb the whole walk is scored under, and what `--only` filters
    /// on. The entry command, not the resumption command: the finding is about
    /// the operation that stopped.
    cmd: &'static str,
    /// Slug rendered into every step id, after the family name.
    name: &'static str,
    shape: Shape,
    /// Steps that put the fixture into the state `entry` needs.
    setup: &'static [&'static [&'static str]],
    /// The invocation(s) that stop. More than one where stopping needs a
    /// predecessor to have succeeded — `am` stops on a mailbox it has already
    /// applied. Grammar-drawn flags are attached to the last of them.
    entry: &'static [&'static [&'static str]],
    /// Payload for every `entry` step. `am` reads its mailbox here; the
    /// resumption verbs that follow are fed nothing, which is the whole reason
    /// [`crate::runner::Step`] carries stdin per step.
    entry_stdin: Option<&'static [u8]>,
}

/// Premises the curated corpus has already proven stop, reused here as the
/// starting points of walks it does not take.
///
/// Deliberately the same premises rather than new ones: a premise that does not
/// actually stop turns its whole walk into resumption verbs against a clean
/// repository, which is a refusal the corpus already covers. Reusing proven ones
/// spends the budget on the transitions instead — which is what is unmeasured.
const STOPPERS: &[Stopper] = &[
    Stopper {
        cmd: "cherry-pick",
        name: "pick-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["cherry-pick", "theirs"]],
        entry_stdin: None,
    },
    // Three picks conflicting on the first, so `.git/sequencer/todo` still holds
    // two while the walk runs — the part of the sequencer a port is most likely
    // to forget entirely, and empty in every two-commit history.
    Stopper {
        cmd: "cherry-pick",
        name: "pick-todo",
        shape: Shape::Whitespace,
        setup: &[&["restore", "."], &["checkout", "-b", "side", "main~4"]],
        entry: &[&["cherry-pick", "main~2", "main~1", "main"]],
        entry_stdin: None,
    },
    // `revert` shares `sequencer.c` and writes `REVERT_HEAD` instead of
    // `CHERRY_PICK_HEAD`; a port that wires the shared engine to one filename
    // walks every cherry-pick stopper above and falls over here.
    Stopper {
        cmd: "revert",
        name: "revert-conflict",
        shape: Shape::Whitespace,
        setup: &[&["restore", "."]],
        entry: &[&["revert", "--no-edit", "main~2"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "rebase",
        name: "rebase-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["rebase", "theirs"]],
        entry_stdin: None,
    },
    // `-i` is reachable because `env::harden` pins `GIT_SEQUENCE_EDITOR=true`,
    // which accepts the generated todo unedited: the todo is written, read back
    // and executed with nothing waiting on a human.
    Stopper {
        cmd: "rebase",
        name: "rebase-i-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["rebase", "-i", "theirs"]],
        entry_stdin: None,
    },
    // A stop that is not a conflict: a failing `exec` parks the rebase with a
    // *clean* worktree and a half-consumed todo, which is the `done`/
    // `git-rebase-todo` split a conflict stop never shows.
    Stopper {
        cmd: "rebase",
        name: "rebase-exec-stop",
        shape: Shape::Renamed,
        setup: &[],
        entry: &[&["rebase", "-i", "--exec", "false", "HEAD~2"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "merge",
        name: "merge-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["merge", "theirs"]],
        entry_stdin: None,
    },
    // `.git/rebase-apply/`, which nothing else parks in. The mailbox applies
    // once and then fails against the tree it just created, so the stop needs no
    // corrupt input to manufacture.
    Stopper {
        cmd: "am",
        name: "am-mailbox-stop",
        shape: Shape::Patches,
        setup: &[],
        entry: &[&["am", "mail/one.eml"], &["am", "mail/one.eml"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "am",
        name: "am-stdin-stop",
        shape: Shape::Linear,
        setup: &[],
        entry: &[&["am"], &["am"]],
        entry_stdin: Some(crate::corpus::MBOX),
    },
    // A conflicting `stash pop`: unmerged index, `AUTO_MERGE` written, and the
    // entry *kept*. `stash` has no resumption verbs of its own, which is the
    // point — every verb the walk draws here is an illegal transition against a
    // state that is genuinely stuck, and that is the corner where a port stops
    // having documentation to copy.
    Stopper {
        cmd: "stash",
        name: "stash-pop-conflict",
        shape: Shape::Stashed,
        setup: &[
            &["stash", "push", "-m", "gen"],
            &["stash", "pop", "stash@{3}"],
            &["commit", "-am", "gen-base"],
        ],
        entry: &[&["stash", "pop"]],
        entry_stdin: None,
    },
];

/// The commands with a `--continue`/`--skip`/`--abort`/`--quit` state machine.
///
/// A fact about `sequencer.c` and `builtin/am.c`, not about any flag table: it is
/// the set a resumption verb may be *addressed to*, and drawing the verb's
/// command from here rather than from the stopper is what produces the illegal
/// cross-machine transitions this family exists for.
const STATEFUL: &[&str] = &["cherry-pick", "revert", "rebase", "merge", "am"];

/// The state machine's alphabet.
const RESUME_TOKENS: &[&str] = &["--continue", "--skip", "--abort", "--quit"];

/// Transitions and state queries only some machines have, taken from the command's
/// own grammar so a command that does not have one never sees it.
///
/// This is the half of the alphabet that *is* derivable: `--retry` belongs to
/// `am` alone and `--edit-todo` to `rebase` alone, and the grammar already knows
/// which. `--show-current-patch` is a query rather than a transition and is here
/// because reading the parked state is the cheapest way to catch a machine that
/// stopped in the wrong place.
const RESUME_EXTRA: &[&str] = &["--retry", "--edit-todo", "--show-current-patch"];

/// `git bisect`'s alphabet.
///
/// Stated rather than filtered out of the bisect grammar's positionals because
/// that list mixes verbs and revs in one flat pool with no marking of which is
/// which — a walk drawn from it uniformly spends most of its steps on
/// `bisect v0.1.0`, which is a usage error rather than a transition. The grammar
/// still supplies this family's flags, and [`REVS`] supplies the operands.
const BISECT_VERBS: &[&str] =
    &["start", "good", "bad", "skip", "next", "log", "terms", "reset", "old", "new", "help"];

/// Read-only invocations whose answer a mutation is supposed to change.
///
/// The selection criterion is that one: every entry reports a *fact about the
/// repository* rather than about its own arguments, so putting one after a
/// mutation asks whether the mutation landed. A reader whose output is a
/// function of its argv alone would pass identically before and after any write
/// and would only cost an invocation.
///
/// All of them are read-only. `status` refreshes the index as a side effect, but
/// it does so on both sides and the curated corpus already interleaves it inside
/// stateful workflows, so it is proven not to be a difference by itself.
///
/// The observer is drawn uniformly and is therefore often *not* the reader that
/// would show a given mutation — `show-ref` says nothing about a moved file.
/// That is deliberate and costs nothing, because the observer is not the oracle:
/// [`crate::runner::run_sequence`] takes the full state comparison after every
/// step regardless of what the step was, so a wrong write is caught whether or
/// not the reader beside it would have printed it. What the observer adds is a
/// *readable* surface for the difference and one more invocation of a reader
/// against a repository state no fixture describes — which is why matching
/// observers to mutators by hand would buy nothing and cost a table.
const OBSERVERS: &[&[&str]] = &[
    &["status", "--porcelain"],
    &["status", "--porcelain=v2", "--branch"],
    &["rev-parse", "HEAD"],
    &["rev-parse", "--abbrev-ref", "HEAD"],
    &["log", "--oneline", "--all"],
    &["reflog"],
    &["for-each-ref", "--format=%(refname) %(objectname) %(upstream)"],
    &["ls-files", "--stage"],
    &["diff", "--name-status"],
    &["diff", "--cached", "--name-status"],
    &["stash", "list"],
    &["branch", "--list", "-a"],
    &["tag", "--list"],
    &["worktree", "list"],
    &["show-ref", "--head"],
];

/// Verbs whose purpose is to write repository state.
///
/// No grammar carries this: a grammar describes a command line, and nothing in a
/// command line says whether running it changes anything. The criterion for
/// membership is narrower than "mutates" — it is **mutates something an
/// [`OBSERVERS`] entry or the state probe reports**. A write no reader can see
/// makes the steps after it noise, which is the failure mode this whole section
/// is built to avoid, so `var` and `check-ignore` are absent while `gc` and
/// `repack` are present (`runner::probe_storage` walks the object layout they
/// rewrite).
///
/// Every name here must have a grammar; [`every_generator_verb_has_a_grammar`]
/// asserts it, so a rename in the generated grammars fails `cargo test` instead
/// of silently dropping a family member from every future run.
const MUTATORS: &[&str] = &[
    "add", "am", "apply", "checkout", "checkout-index", "cherry-pick", "clean", "commit",
    "commit-graph", "commit-tree", "fast-import", "fetch", "filter-branch", "gc", "merge",
    "mktag", "multi-pack-index", "mv", "notes", "pack-refs", "prune", "prune-packed", "pull",
    "push", "read-tree", "rebase", "reflog", "refs", "remote", "repack", "replace", "replay",
    "rerere", "reset", "restore", "revert", "rm", "sparse-checkout", "stage", "stash",
    "submodule", "switch", "symbolic-ref", "update-index", "update-ref", "worktree",
    "write-tree",
];

/// An operation and its inverse, with the shape the pair is meaningful on.
struct RoundTrip {
    cmd: &'static str,
    name: &'static str,
    shape: Shape,
    forward: &'static [&'static [&'static str]],
    inverse: &'static [&'static [&'static str]],
}

/// The inverse pairs.
///
/// Written out rather than drawn from the grammars, and the reason is the one
/// thing this family measures: *inverseness*. A grammar-drawn flag on the
/// forward step silently destroys it — `stash push --keep-index` changes what
/// `pop` restores, `worktree add --detach` changes what `remove` has to clean —
/// and a round-trip whose inverse no longer inverts is a sequence whose premise
/// its own first step destroyed. That is the nonsense case this file must not
/// generate, so the pairs are exact and the drawn part is which pair runs, which
/// observers sit between the halves, and the envelope.
///
/// Both halves are still compared step by step like everything else, so the
/// finding is never "these differ after four commands": a `disable` that leaves
/// `.git/info/sparse-checkout` behind is a state difference at the `disable`.
const ROUND_TRIPS: &[RoundTrip] = &[
    RoundTrip {
        cmd: "stash",
        name: "stash-push-pop",
        shape: Shape::Dirty,
        forward: &[&["stash", "push", "-m", "gen"]],
        inverse: &[&["stash", "pop"]],
    },
    // The untracked half: `-u` stashes a file that was never in the index, and
    // popping it has to put it back *untracked*, which is a different code path
    // from restoring a tracked modification.
    RoundTrip {
        cmd: "stash",
        name: "stash-push-untracked-pop",
        shape: Shape::Dirty,
        forward: &[&["stash", "push", "-u", "-m", "gen"]],
        inverse: &[&["stash", "pop"]],
    },
    RoundTrip {
        cmd: "branch",
        name: "branch-rename-back",
        shape: Shape::Linear,
        forward: &[&["branch", "-m", "main", "gen-renamed"]],
        inverse: &[&["branch", "-m", "gen-renamed", "main"]],
    },
    // `add` writes `.git/worktrees/<n>/{gitdir,HEAD,commondir}` and a `.git`
    // file in the new tree; `remove` has to delete both ends of that pair.
    RoundTrip {
        cmd: "worktree",
        name: "worktree-add-remove",
        shape: Shape::Branched,
        forward: &[&["worktree", "add", "-b", "gen-wtb", "wt-gen"]],
        inverse: &[&["worktree", "remove", "wt-gen"]],
    },
    RoundTrip {
        cmd: "worktree",
        name: "worktree-lock-unlock",
        shape: Shape::Worktree,
        forward: &[&["worktree", "lock", "wt"]],
        inverse: &[&["worktree", "unlock", "wt"]],
    },
    RoundTrip {
        cmd: "sparse-checkout",
        name: "sparse-set-disable",
        shape: Shape::Sparse,
        forward: &[&["sparse-checkout", "set", "inside"]],
        inverse: &[&["sparse-checkout", "disable"]],
    },
    RoundTrip {
        cmd: "sparse-checkout",
        name: "sparse-init-disable",
        shape: Shape::Linear,
        forward: &[&["sparse-checkout", "init", "--cone"]],
        inverse: &[&["sparse-checkout", "disable"]],
    },
    RoundTrip {
        cmd: "tag",
        name: "tag-add-delete",
        shape: Shape::Branched,
        forward: &[&["tag", "gen-tag", "HEAD"]],
        inverse: &[&["tag", "-d", "gen-tag"]],
    },
    // `switch -` resolves `@{-1}` out of the reflog, so the inverse half reads
    // state the forward half wrote into a place neither command names.
    RoundTrip {
        cmd: "switch",
        name: "switch-create-back",
        shape: Shape::Branched,
        forward: &[&["switch", "-c", "gen-branch"]],
        inverse: &[&["switch", "-"], &["branch", "-D", "gen-branch"]],
    },
    RoundTrip {
        cmd: "checkout",
        name: "checkout-create-back",
        shape: Shape::Branched,
        forward: &[&["checkout", "-b", "gen-co"]],
        inverse: &[&["checkout", "main"], &["branch", "-D", "gen-co"]],
    },
    RoundTrip {
        cmd: "update-ref",
        name: "update-ref-create-delete",
        shape: Shape::Linear,
        forward: &[&["update-ref", "refs/heads/gen-ref", "HEAD"]],
        inverse: &[&["update-ref", "-d", "refs/heads/gen-ref"]],
    },
    RoundTrip {
        cmd: "remote",
        name: "remote-add-remove",
        shape: Shape::BehindRemote,
        forward: &[&["remote", "add", "gen", "./.remote.git"]],
        inverse: &[&["remote", "remove", "gen"]],
    },
    RoundTrip {
        cmd: "notes",
        name: "notes-add-remove",
        shape: Shape::Linear,
        forward: &[&["notes", "add", "-m", "gen note", "HEAD"]],
        inverse: &[&["notes", "remove", "HEAD"]],
    },
    RoundTrip {
        cmd: "commit",
        name: "commit-then-reset",
        shape: Shape::Linear,
        forward: &[&["commit", "--allow-empty", "-m", "gen"]],
        inverse: &[&["reset", "--hard", "HEAD~1"]],
    },
    RoundTrip {
        cmd: "add",
        name: "add-then-restore-staged",
        shape: Shape::Dirty,
        forward: &[&["add", "untracked.txt"]],
        inverse: &[&["restore", "--staged", "untracked.txt"]],
    },
    RoundTrip {
        cmd: "rm",
        name: "rm-cached-then-add",
        shape: Shape::Linear,
        forward: &[&["rm", "--cached", "README.md"]],
        inverse: &[&["add", "README.md"]],
    },
    RoundTrip {
        cmd: "config",
        name: "config-set-unset",
        shape: Shape::Linear,
        forward: &[&["config", "gen.key", "value"]],
        inverse: &[&["config", "--unset", "gen.key"]],
    },
    RoundTrip {
        cmd: "mv",
        name: "mv-there-and-back",
        shape: Shape::Linear,
        forward: &[&["mv", "README.md", "gen-moved.md"]],
        inverse: &[&["mv", "gen-moved.md", "README.md"]],
    },
    RoundTrip {
        cmd: "symbolic-ref",
        name: "symref-set-delete",
        shape: Shape::Linear,
        forward: &[&["symbolic-ref", "refs/gen-sym", "refs/heads/main"]],
        inverse: &[&["symbolic-ref", "-d", "refs/gen-sym"]],
    },
    // `--soft` moves only `HEAD` and records `ORIG_HEAD`; the inverse reads that
    // record back, so a port that moves the branch without writing `ORIG_HEAD`
    // fails at the inverse rather than at the step that skipped the write.
    RoundTrip {
        cmd: "reset",
        name: "reset-soft-orig-head",
        shape: Shape::Branched,
        forward: &[&["reset", "--soft", "HEAD~1"]],
        inverse: &[&["reset", "--soft", "ORIG_HEAD"]],
    },
    RoundTrip {
        cmd: "read-tree",
        name: "read-tree-back",
        shape: Shape::Branched,
        forward: &[&["read-tree", "HEAD~1"]],
        inverse: &[&["read-tree", "HEAD"]],
    },
];

/// Generate `per_entry` sequences for every entry point, from `seed`.
///
/// The three families are emitted in a fixed order and each entry point is drawn
/// `per_entry` times, so the corpus this returns — the sequences, their steps and
/// their ids — is a pure function of `(seed, per_entry)` and a reported step
/// replays exactly. That is the property this function owns. It is not the same
/// claim as "the report is byte-identical": a handful of cases carry values
/// *stock* re-rolls every run (`filter-branch`'s elapsed-seconds progress line,
/// `blame`'s wall clock on uncommitted lines, `unpack-file`'s random temp name,
/// `quiltimport`'s commit ids), and those move the report's bytes no matter what
/// any generator does. [`crate::runner::Verdict::Nondeterministic`] is where they
/// are accounted for.
pub fn generate_sequences(seed: u64, per_entry: usize) -> Vec<Sequence> {
    let mut rng = Rng::new(seed ^ SEQUENCE_STREAM);
    let grammars = all_grammars();
    let mut out = Vec::new();

    for stopper in STOPPERS {
        for n in 0..per_entry {
            out.push(walk(&mut rng, stopper, &grammars, n));
        }
    }
    for n in 0..per_entry {
        out.push(bisect_walk(&mut rng, &grammars, n));
    }
    for cmd in MUTATORS {
        // A name with no grammar is a bug in `MUTATORS`, caught at `cargo test`
        // by `every_generator_verb_has_a_grammar`. Skipped rather than panicked
        // at run time so one stale name cannot take the whole sweep down.
        let Some(g) = grammar_for(&grammars, cmd) else { continue };
        for n in 0..per_entry {
            out.push(mutate_then_observe(&mut rng, g, n));
        }
    }
    for rt in ROUND_TRIPS {
        for n in 0..per_entry {
            out.push(round_trip(&mut rng, rt, n));
        }
    }
    out
}

/// The grammar for `cmd`, if the fuzzer has one.
fn grammar_for<'a>(grammars: &'a [Grammar], cmd: &str) -> Option<&'a Grammar> {
    grammars.iter().find(|g| g.cmd == cmd)
}

/// Apply the envelope dimensions a generated sequence draws.
///
/// Only two, and both are drawn by the samplers the single-case generator
/// already uses. The working directory is here because git re-resolves which
/// repository it is in on *every* invocation, so a stateful operation resumed
/// from a subdirectory asks whether step 4 finds the repository step 3 wrote to
/// — a break the curated corpus has one case for and no more. Configuration is
/// here because settings like `merge.conflictStyle` and `rerere.enabled` change
/// what a whole workflow does rather than what one invocation prints.
///
/// Environment and global options are deliberately not drawn: `GIT_DIR`
/// redirection across steps is already curated, `-C <dir>` duplicates the
/// working directory, and every extra dimension lands in a step id that already
/// carries the whole script.
fn envelope_dims(rng: &mut Rng, seq: Sequence, shape: Shape) -> Sequence {
    let config = sample_config(rng);
    let cwd = sample_cwd(rng, shape);
    let seq = if config.is_empty() { seq } else { seq.with_scoped_config(config) };
    match cwd {
        Some(dir) => seq.in_dir(dir),
        None => seq,
    }
}

/// Append 1..=`max` observers.
fn observe(rng: &mut Rng, mut seq: Sequence, max: usize) -> Sequence {
    for _ in 0..=rng.below(max) {
        seq = seq.step(rng.pick(OBSERVERS));
    }
    seq
}

/// One resumption invocation: a command from [`STATEFUL`] and a verb from its
/// alphabet.
///
/// `own` biases the draw toward the machine that is actually running — two
/// thirds — so the legal walk is reached often while the cross-machine
/// transitions that a port has no documentation for still come up on a third of
/// the draws.
fn resume_step(rng: &mut Rng, own: &str, grammars: &[Grammar]) -> Vec<String> {
    let cmd = if rng.chance(2, 3) && STATEFUL.contains(&own) {
        own
    } else {
        *rng.pick(STATEFUL)
    };
    let mut verbs: Vec<&str> = RESUME_TOKENS.to_vec();
    if let Some(g) = grammar_for(grammars, cmd) {
        verbs.extend(g.flags.iter().copied().filter(|f| RESUME_EXTRA.contains(f)));
    }
    vec![cmd.to_string(), rng.pick(&verbs).to_string()]
}

/// A state-machine walk: setup, the invocation that stops, then resumption verbs
/// with observers between them.
fn walk(rng: &mut Rng, s: &Stopper, grammars: &[Grammar], n: usize) -> Sequence {
    let mut seq =
        Sequence::new(s.cmd, format!("gen/walk/{}#{n}", s.name), s.shape);
    for step in s.setup {
        seq = seq.step(step);
    }

    // The entry, decorated on a minority of draws with 1..=2 grammar flags on
    // its last invocation.
    //
    // Two filters and a gate. Resumption tokens are excluded because decorating
    // `cherry-pick theirs` with `--abort` defeats the premise before the walk
    // starts, and the walk draws that verb on its own terms anyway. The gate is
    // there because *any* flag can break the premise — `--strategy=bogus` dies
    // at parse time, `-n` stops the operation from starting — and a walk whose
    // entry never stopped spends every one of its steps on a refusal the corpus
    // already covers. A minority is the right rate rather than none: the entry
    // invocation should not be one of eleven fixed command lines, and a
    // decoration that turns a stop into a non-stop is itself a transition worth
    // comparing. Most walks keep the proven premise; some do not, on purpose.
    let decorations: Vec<String> = match grammar_for(grammars, s.cmd) {
        Some(g) if rng.chance(1, 3) => {
            let usable: Vec<&str> = g
                .flags
                .iter()
                .copied()
                .filter(|f| !RESUME_TOKENS.contains(f) && !RESUME_EXTRA.contains(f))
                .collect();
            let count = if usable.is_empty() { 0 } else { rng.count_upto(2) };
            (0..count).map(|_| rng.pick(&usable).to_string()).collect()
        }
        _ => Vec::new(),
    };
    for (i, step) in s.entry.iter().enumerate() {
        let mut args: Vec<String> = step.iter().map(|t| t.to_string()).collect();
        if i + 1 == s.entry.len() {
            // After the subcommand, before its operands: a flag written after a
            // rev is still parsed by git, but the id reads as an invocation
            // somebody would write.
            let tail = args.split_off(1);
            args.extend(decorations.iter().cloned());
            args.extend(tail);
        }
        seq = seq.step_argv(args, s.entry_stdin);
    }

    // The walk. Each verb is followed by an observer half the time — often
    // enough that a wrong write is attributed to the verb that made it, rarely
    // enough that the walk is mostly transitions rather than mostly reads.
    for _ in 0..=rng.below(3) {
        seq = seq.step_argv(resume_step(rng, s.cmd, grammars), None);
        if rng.chance(1, 2) {
            seq = seq.step(rng.pick(OBSERVERS));
        }
    }
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, s.shape)
}

/// `git bisect`'s own state machine: a start, then answers, then whatever the
/// walk draws — including answering a bisect that was never started and resetting
/// one twice.
fn bisect_walk(rng: &mut Rng, grammars: &[Grammar], n: usize) -> Sequence {
    let g = grammar_for(grammars, "bisect");
    let shape = match g {
        Some(g) => *rng.pick(g.shapes),
        None => Shape::Branched,
    };
    let mut seq = Sequence::new("bisect", format!("gen/bisect#{n}"), shape);

    // `start` first, decorated from the grammar — `--term-new=`/`--term-old=`
    // rename the verbs the rest of the walk uses, which is a rename a port can
    // implement for `terms` and forget for the answers.
    let mut start = vec!["bisect".to_string(), "start".to_string()];
    if let Some(g) = g {
        for _ in 0..rng.count_upto(2) {
            start.push(rng.pick(g.flags).to_string());
        }
    }
    seq = seq.step_argv(start, None);

    for _ in 0..=rng.below(5) {
        let mut args = vec!["bisect".to_string(), rng.pick(BISECT_VERBS).to_string()];
        // An operand a third of the time. `bisect bad HEAD~2` and `bisect bad`
        // are different transitions — one names a commit, the other means "the
        // one you just checked out" — and only a walk can reach the second.
        if rng.chance(1, 3) {
            let rev = *rng.pick(REVS);
            if !rev.is_empty() {
                args.push(rev.to_string());
            }
        }
        seq = seq.step_argv(args, None);
        if rng.chance(1, 3) {
            seq = seq.step(rng.pick(OBSERVERS));
        }
    }
    seq = seq.step(&["bisect", "log"]).step(&["bisect", "reset"]);
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, shape)
}

/// A sampled mutation followed by the readers whose answer it should have
/// changed, and sometimes a second mutation and a second round of readers.
///
/// The second mutation is what makes this more than a case with readers attached:
/// it runs against whatever the first one left, which for a sampled argv is a
/// state no fixture describes.
fn mutate_then_observe(rng: &mut Rng, g: &Grammar, n: usize) -> Sequence {
    let shape = *rng.pick(g.shapes);
    let mut seq = Sequence::new(g.cmd, format!("gen/observe/{}#{n}", g.cmd), shape);

    // A baseline read a third of the time. It is drawn rather than forced
    // because the two fixtures are copies of one template and are equal by
    // construction before anything runs, so its only value is proving the
    // observer itself agrees — which the single-case corpus already covers on a
    // pristine fixture. Worth an invocation sometimes, not always.
    if rng.chance(1, 3) {
        seq = seq.step(rng.pick(OBSERVERS));
    }

    for round in 0..if rng.chance(1, 3) { 2 } else { 1 } {
        let args = sample_argv(rng, g, 2, 2);
        let stdin = sample_stdin(rng, g.cmd, &args);
        seq = seq.step_argv(args, stdin);
        seq = observe(rng, seq, if round == 0 { 2 } else { 1 });
    }
    envelope_dims(rng, seq, shape)
}

/// An operation, then its inverse, with reads between the halves.
fn round_trip(rng: &mut Rng, rt: &RoundTrip, n: usize) -> Sequence {
    let mut seq =
        Sequence::new(rt.cmd, format!("gen/roundtrip/{}#{n}", rt.name), rt.shape);
    if rng.chance(1, 3) {
        seq = seq.step(rng.pick(OBSERVERS));
    }
    for step in rt.forward {
        seq = seq.step(step);
    }
    seq = observe(rng, seq, 2);
    for step in rt.inverse {
        seq = seq.step(step);
    }
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, rt.shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::split_config_key;

    /// The sampled environment may only *add* variables, never re-point one of
    /// `env::harden`'s pins. The runner asserts this per case, which catches it
    /// at run time; this catches it at `cargo test` time, before a pool entry
    /// that would abort every case gets committed.
    #[test]
    fn sampled_env_vars_are_never_pinned() {
        for (key, values) in ENV_VARS {
            assert!(!crate::env::is_pinned(key), "{key} is pinned by env::harden");
            assert!(!values.is_empty(), "{key} has no values to draw from");
            // `ConfigScope::Env` writes `GIT_CONFIG_COUNT`/`KEY_<n>`/`VALUE_<n>`
            // onto the same child, so a variable in that family drawn here would
            // be silently overwritten — and `run_side` asserts on the collision,
            // which would abort a whole sweep on whichever case drew both.
            assert!(
                !key.starts_with("GIT_CONFIG_"),
                "{key} collides with the config env scope; deliver it as a ConfigEntry instead"
            );
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
    ///
    /// The grouped pool is held to the same rules as the flat one, and to one
    /// more: a group with a single key is not a group. The whole reason
    /// [`CONFIG_GROUPS`] exists is that two keys can be drawn together, and a
    /// one-key entry would spend a two-thirds share of the budget doing what the
    /// flat pool already does.
    #[test]
    fn config_pool_is_well_formed() {
        let well_formed = |key: &str, values: &[&str], whose: &str| {
            assert!(key.contains('.'), "{whose}: {key} is not a section.key");
            assert!(!values.is_empty(), "{whose}: {key} has no values to draw from");
            assert!(
                values.iter().all(|v| !v.starts_with('/')),
                "{whose}: {key} names an absolute path"
            );
            assert!(
                split_config_key(key).is_some(),
                "{whose}: {key} cannot be written as a config file stanza"
            );
        };
        for (key, values) in CONFIG_KEYS {
            well_formed(key, values, "CONFIG_KEYS");
        }
        for group in CONFIG_GROUPS {
            assert!(group.keys.len() > 1, "group {} has nothing to interact with", group.name);
            for (key, values) in group.keys {
                well_formed(key, values, group.name);
            }
        }
    }

    /// `.gitmodules` is only read for `submodule.*`, so a key that is not one
    /// must never be routed there: git would parse the file and discard the
    /// stanza, and the case would look like it set a key while setting nothing.
    /// Every other key must reach every other scope, or the scope dimension
    /// silently collapses back to the `-c` it replaced.
    #[test]
    fn only_submodule_keys_reach_the_gitmodules_scope() {
        let every_key = CONFIG_KEYS
            .iter()
            .chain(CONFIG_GROUPS.iter().flat_map(|g| g.keys.iter()))
            .map(|(k, _)| *k);
        let (mut submodule_keys, mut other_keys) = (0, 0);
        for key in every_key {
            let scopes = scopes_for(key);
            if key.starts_with("submodule.") {
                submodule_keys += 1;
                assert!(scopes.contains(&ConfigScope::Modules), "{key} cannot reach .gitmodules");
            } else {
                other_keys += 1;
                assert!(!scopes.contains(&ConfigScope::Modules), "{key} would be discarded there");
            }
            for scope in ConfigScope::ALL {
                if *scope != ConfigScope::Modules {
                    assert!(scopes.contains(scope), "{key} cannot reach {}", scope.name());
                }
            }
        }
        assert!(submodule_keys > 0, "no key can reach the .gitmodules scope at all");
        assert!(other_keys > 0, "every key is a submodule key");
    }

    /// Every scope is actually drawn. A scope that silently rounded to zero —
    /// because a probability was wrong, or because `scopes_for` excluded it —
    /// would leave the whole point of the widening unmeasured while every number
    /// in the report stayed exactly as plausible as before.
    #[test]
    fn every_config_scope_is_drawn() {
        let cases = generate(20250821, 6);
        for scope in ConfigScope::ALL {
            assert!(
                cases.iter().any(|c| c.config.iter().any(|e| e.scope == *scope)),
                "no case delivered a setting from the {} scope",
                scope.name()
            );
        }
    }

    /// The two shapes a scope dimension exists to reach, neither of which a
    /// one-key `-c` sampler can produce:
    ///
    ///  * one key set **twice in one file**, which is last-value-wins;
    ///  * one key set **in two scopes**, which is precedence.
    ///
    /// Asserted together because the bias in [`sample_scope`] trades them off
    /// against each other: a bias of 1 would give only the first, a bias of 0
    /// only rarely the first, and either mistake leaves half of what the scope
    /// dimension is for unmeasured while the case count stays the same.
    #[test]
    fn config_draws_reach_last_wins_and_precedence() {
        let cases = generate(0xC0FFEE, 6);
        let mut repeated_in_one_file = 0;
        let mut same_key_two_scopes = 0;
        for case in &cases {
            for (i, a) in case.config.iter().enumerate() {
                for b in case.config.iter().skip(i + 1) {
                    if a.key.is_none() || a.key != b.key {
                        continue;
                    }
                    if a.scope == b.scope && a.scope.is_file() {
                        repeated_in_one_file += 1;
                    } else if a.scope != b.scope {
                        same_key_two_scopes += 1;
                    }
                }
            }
        }
        assert!(repeated_in_one_file > 0, "no draw ever set one key twice in one file");
        assert!(same_key_two_scopes > 0, "no draw ever set one key in two scopes");
    }

    /// A raw line is a thing only a file has, so it may only be drawn for a file
    /// scope — a raw entry on `-c` or in `GIT_CONFIG_KEY_<n>` would be silently
    /// dropped by [`crate::runner::install_config`], and a case id would then
    /// name a premise that never reached either side.
    ///
    /// Both pools have to be reached as well: the malformed one measures the
    /// line-numbered refusal, the legal one measures the parser on inputs `-c`
    /// cannot express, and they are drawn on one coin flip that a wrong constant
    /// could send entirely one way.
    #[test]
    fn raw_config_lines_only_land_in_file_scopes() {
        let cases = generate(31337, 8);
        let raws: Vec<&ConfigEntry> =
            cases.iter().flat_map(|c| c.config.iter()).filter(|e| e.is_raw()).collect();
        assert!(!raws.is_empty(), "no case ever drew a raw config line");
        for entry in &raws {
            assert!(
                entry.scope.is_file(),
                "raw line delivered to the non-file scope {}",
                entry.scope.name()
            );
        }
        assert!(
            raws.iter().any(|e| CONFIG_BAD_LINES.contains(&e.value.as_str())),
            "no case ever drew a malformed config line"
        );
        assert!(
            raws.iter().any(|e| CONFIG_ODD_LINES.contains(&e.value.as_str())),
            "no case ever drew a legal file-only config line"
        );
    }

    /// The `::config[…]` segment names the scope of every entry it carries, and
    /// carries every non-command-line entry. This is the property that makes a
    /// scoped failure reproducible by hand: without the scope the reader knows a
    /// key and a value and not which of six places to put them, and the case
    /// they reconstruct is a different case.
    #[test]
    fn case_ids_name_the_scope_of_every_scoped_entry() {
        let mut checked = 0;
        for case in generate(777, 6) {
            let scoped: Vec<&ConfigEntry> =
                case.config.iter().filter(|e| e.scope != ConfigScope::CommandLine).collect();
            if scoped.is_empty() {
                assert!(!case.id().contains("::config["), "empty config segment in {}", case.id());
                continue;
            }
            let id = case.id();
            let segment = id
                .split("::config[")
                .nth(1)
                .and_then(|s| s.split(']').next())
                .expect("a config segment");
            for entry in scoped {
                assert!(
                    segment.contains(&format!("{}:", entry.scope.name())),
                    "{} does not name the {} scope",
                    id,
                    entry.scope.name()
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no generated case carried a scoped config entry");
    }

    /// The group table is what makes an *interaction* reachable, so it has to
    /// actually fire: a group draw must put two keys of one group into one case
    /// often enough to be worth the two-thirds share it costs.
    #[test]
    fn grouped_draws_put_interacting_keys_in_one_case() {
        let cases = generate(2468, 6);
        let mut hits = 0;
        for case in &cases {
            let keys: Vec<&str> =
                case.config.iter().filter_map(|e| e.key.as_deref()).collect();
            if keys.len() < 2 {
                continue;
            }
            if CONFIG_GROUPS.iter().any(|g| {
                keys.iter().filter(|k| g.keys.iter().any(|(gk, _)| gk == *k)).count() >= 2
            }) {
                hits += 1;
            }
        }
        assert!(hits > 0, "no case ever drew two keys from one interacting group");
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

        // Rendered, not merely stored — and rendered in the right place for the
        // scope: a command-line entry belongs in the argv segment, everything
        // else in the `::config[…]` segment, because a reader reproducing the
        // case types the first and writes the second into a file.
        let with_cmdline = cases
            .iter()
            .find(|c| c.config.iter().any(|e| e.scope == ConfigScope::CommandLine))
            .expect("no case sampled a command-line config key");
        assert!(with_cmdline.id().contains("-c "), "config missing from {}", with_cmdline.id());
        let with_scoped = cases
            .iter()
            .find(|c| c.config.iter().any(|e| e.scope != ConfigScope::CommandLine))
            .expect("no case sampled a scoped config key");
        assert!(
            with_scoped.id().contains("::config["),
            "scoped config missing from {}",
            with_scoped.id()
        );
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
            c.config.iter().any(|e| e.key.as_deref() == Some("status.short"))
        });

        assert_eq!(
            minimal.config,
            vec![ConfigEntry::set(ConfigScope::CommandLine, "status.short", "true")]
        );
        assert!(minimal.globals.is_empty());
        assert!(minimal.env.is_empty());
        assert_eq!(minimal.cwd, None);
        assert_eq!(minimal.stdin, None);
        assert_eq!(minimal.args, vec!["status".to_string()]);
        assert_eq!(minimal.size(), 1);
    }

    // -----------------------------------------------------------------------
    // Generated sequences
    // -----------------------------------------------------------------------

    /// Every verb the sequence generator names must have a grammar, or the entry
    /// point silently vanishes from every future run while the sequence count
    /// stays plausible. Asserted against [`all_grammars`] rather than against a
    /// second list, so a rename in the generated grammars fails here instead of
    /// quietly shrinking the corpus.
    #[test]
    fn every_generator_verb_has_a_grammar() {
        let grammars = all_grammars();
        for cmd in MUTATORS {
            assert!(grammar_for(&grammars, cmd).is_some(), "MUTATORS names {cmd}, which has no grammar");
        }
        for cmd in STATEFUL {
            assert!(grammar_for(&grammars, cmd).is_some(), "STATEFUL names {cmd}, which has no grammar");
        }
        for s in STOPPERS {
            assert!(grammar_for(&grammars, s.cmd).is_some(), "stopper {} has no grammar", s.name);
        }
        assert!(grammar_for(&grammars, "bisect").is_some(), "the bisect walk has no grammar");
    }

    /// The tables are well formed in the ways a malformed entry would not be
    /// caught by anything else: an empty argv aborts a whole sequence in
    /// `Sequence::step_case`, a `git`-prefixed token would run `git git status`,
    /// and a round trip with no inverse is not a round trip.
    #[test]
    fn generator_tables_are_well_formed() {
        let check = |argv: &[&str], what: &str| {
            assert!(!argv.is_empty(), "{what} has an empty step");
            assert_ne!(argv[0], "git", "{what} repeats the binary name");
            assert!(!argv[0].starts_with('-'), "{what} starts with a flag, not a subcommand");
        };
        for s in STOPPERS {
            assert!(!s.entry.is_empty(), "stopper {} never starts anything", s.name);
            for step in s.setup.iter().chain(s.entry) {
                check(step, s.name);
            }
        }
        for rt in ROUND_TRIPS {
            assert!(!rt.forward.is_empty(), "round trip {} has no forward half", rt.name);
            assert!(!rt.inverse.is_empty(), "round trip {} has no inverse half", rt.name);
            for step in rt.forward.iter().chain(rt.inverse) {
                check(step, rt.name);
            }
        }
        for o in OBSERVERS {
            check(o, "observer");
        }
    }

    /// Generation is a pure function of `(seed, per_entry)` — the property a
    /// reported sequence failure is reproduced by, and the one every new draw is
    /// most likely to break since they all share one stream.
    ///
    /// The second half is the reason the sequence stream is seeded apart from the
    /// case stream: a sequence must replay from its seed whatever `--fuzz` was,
    /// which would not hold if `fuzz::generate` had consumed the same RNG first.
    #[test]
    fn sequence_generation_is_reproducible_from_its_seed() {
        let ids = |seed: u64, per: usize| -> Vec<String> {
            generate_sequences(seed, per)
                .iter()
                .flat_map(|s| (0..s.len()).map(|i| s.step_id(i)).collect::<Vec<_>>())
                .collect()
        };
        assert_eq!(ids(0x5A5A_C0DE, 2), ids(0x5A5A_C0DE, 2));
        assert_ne!(ids(1, 2), ids(2, 2), "different seeds must explore different workflows");

        // Independent of the case stream: draining `generate` first must not move
        // the sequences it did not produce.
        let before = ids(0x5A5A_C0DE, 1);
        let _ = generate(0x5A5A_C0DE, 3);
        assert_eq!(before, ids(0x5A5A_C0DE, 1));
    }

    /// Every entry point is drawn `per_entry` times, so raising the knob deepens
    /// coverage uniformly instead of deepening whatever the RNG favoured. A
    /// generator that silently dropped a family would still print a plausible
    /// sequence count, which is the class of lie this crate must not tell.
    #[test]
    fn every_entry_point_is_drawn() {
        let entry_points = STOPPERS.len() + 1 + MUTATORS.len() + ROUND_TRIPS.len();
        for per in [1usize, 3] {
            let seqs = generate_sequences(99, per);
            assert_eq!(seqs.len(), entry_points * per);
        }
        let seqs = generate_sequences(99, 1);
        for s in STOPPERS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/walk/{}#0", s.name)),
                "no walk generated for stopper {}",
                s.name
            );
        }
        for cmd in MUTATORS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/observe/{cmd}#0")),
                "no observe sequence generated for {cmd}"
            );
        }
        for rt in ROUND_TRIPS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/roundtrip/{}#0", rt.name)),
                "no round trip generated for {}",
                rt.name
            );
        }
        assert!(seqs.iter().any(|q| q.name == "gen/bisect#0"));
        assert!(generate_sequences(99, 0).is_empty(), "zero must generate nothing");
    }

    /// The structural property that separates a workflow from a bag of unrelated
    /// invocations: **every** generated sequence has at least two steps, and
    /// every one of them ends with a step that reads state an earlier step could
    /// have written.
    ///
    /// A generator that emitted a single-step "sequence" would be paying the
    /// sequence machinery's price for a case, and one that ended on its mutation
    /// would never look at what the mutation did — which is the entire premise of
    /// the mutate-then-observe family.
    #[test]
    fn generated_sequences_end_on_a_read() {
        let readers: Vec<&str> = OBSERVERS.iter().map(|o| o[0]).collect();
        for s in generate_sequences(7, 2) {
            assert!(s.len() >= 2, "{} is a single invocation, not a workflow", s.name);
            let last = s.step_case(s.len() - 1);
            assert!(
                readers.contains(&last.args[0].as_str()),
                "{} ends on {:?}, which reads nothing back",
                s.name,
                last.args
            );
        }
    }

    /// A walk's steps after the entry are addressed to a state machine, and the
    /// cross-machine ones — the illegal transitions a port has no documentation
    /// for — must actually be reached rather than rounded away by the 2/3 bias.
    /// A probability that silently became zero would leave the most valuable half
    /// of this family unmeasured while the sequence count stayed the same.
    #[test]
    fn walks_reach_illegal_cross_machine_transitions() {
        let seqs = generate_sequences(1234, 4);
        let (mut cross, mut own) = (0, 0);
        // Only walks whose stopper *has* a machine of its own count: the
        // stash-pop stopper is not in `STATEFUL`, so every verb it draws is
        // trivially cross and would let a broken bias pass this test.
        for s in seqs
            .iter()
            .filter(|s| s.name.starts_with("gen/walk/") && STATEFUL.contains(&s.cmd()))
        {
            for i in 0..s.len() {
                let args = s.step_case(i).args;
                let is_resume = args.len() == 2
                    && STATEFUL.contains(&args[0].as_str())
                    && RESUME_TOKENS.contains(&args[1].as_str());
                if !is_resume {
                    continue;
                }
                if args[0] == s.cmd() {
                    own += 1;
                } else {
                    cross += 1;
                }
            }
        }
        assert!(cross > 0, "no walk ever addressed a verb to another machine");
        assert!(own > 0, "no walk ever took its own machine's legal transition");
    }

    /// A round trip runs its forward half before its inverse, in order, with the
    /// reads between them. Order is the whole content of the family: reversed, it
    /// would be two independent invocations that happen to share a repository.
    #[test]
    fn round_trips_run_forward_before_inverse() {
        let seqs = generate_sequences(555, 1);
        for rt in ROUND_TRIPS {
            let name = format!("gen/roundtrip/{}#0", rt.name);
            let s = seqs.iter().find(|q| q.name == name).expect("round trip generated");
            let argvs: Vec<Vec<String>> = (0..s.len()).map(|i| s.step_case(i).args).collect();
            let position = |want: &[&str]| -> usize {
                let want: Vec<String> = want.iter().map(|t| t.to_string()).collect();
                argvs.iter().position(|a| *a == want).unwrap_or_else(|| {
                    panic!("{name} never runs {want:?}; steps were {argvs:?}")
                })
            };
            let last_forward = rt.forward.iter().map(|s| position(s)).max().unwrap();
            let first_inverse = rt.inverse.iter().map(|s| position(s)).min().unwrap();
            assert!(
                last_forward < first_inverse,
                "{name} runs its inverse before its forward half"
            );
        }
    }

    /// A step is only handed a payload where the invocation asks for one, exactly
    /// as a single case is — the sequence generator reuses [`sample_stdin`]
    /// rather than deciding for itself, and this pins that it did not grow a
    /// second rule. A payload delivered to a step that does not read it makes the
    /// step id lie about what the step does.
    #[test]
    fn generated_steps_only_get_stdin_where_it_is_read() {
        for s in generate_sequences(88, 2) {
            for i in 0..s.len() {
                let case = s.step_case(i);
                if case.stdin.is_some() {
                    assert!(
                        wants_stdin(&case.args[0], &case.args),
                        "{} step {} was fed a payload it never reads: {:?}",
                        s.name,
                        i + 1,
                        case.args
                    );
                }
            }
        }
    }

    /// The id grammar `scripts/split_failures.pl` parses survives a generated
    /// sequence: `<shape>::<cmd>::` off the front, both segments free of
    /// whitespace, and the command segment the verb the sequence is scored under.
    /// A generated failure that files under no subcommand disappears from the
    /// per-command briefs, which is worse than one that shouts.
    #[test]
    fn generated_sequence_ids_keep_the_shape_and_command_segments_first() {
        for s in generate_sequences(31337, 2) {
            for i in 0..s.len() {
                let id = s.step_id(i);
                let (shape, rest) =
                    id.trim_start_matches('!').split_once("::").expect("shape segment");
                let (cmd, rest) = rest.split_once("::").expect("command segment");
                assert!(!shape.contains(char::is_whitespace), "shape segment has a space: {id}");
                assert!(!cmd.contains(char::is_whitespace), "command segment has a space: {id}");
                assert_eq!(cmd, s.cmd());
                assert!(rest.starts_with("seq["), "sequence segment missing from {id}");
            }
        }
    }
}
