//! How a configuration **value is resolved**: the include graph, the scope
//! stack, the type conversions, the value selectors, and the file grammar.
//!
//! Everything else in the corpus that touches configuration asks what a *key*
//! does. This module asks what the *lookup* does — the machinery in `config.c`
//! that turns a set of files, an environment and a command line into one answer,
//! independently of which key the answer happens to be for. A port can honour
//! every key in `git help --config` and still take the value from the wrong
//! file, expand an include at the wrong point in the sequence, read `on` as a
//! string, or answer `--get-all` in the wrong order — and none of those is
//! visible from any case that sets one key and reads one verb.
//!
//! # How this divides territory with the six adjacent modules
//!
//! * **`config_cmd.rs`** is the nearest neighbour and owns `builtin/config.c` as
//!   a *program*: the subcommand spellings (`get`/`set`/`unset`/`list`/
//!   `rename-section`/`remove-section`), every mutating verb pinned through the
//!   state probe, the quoting rules `store_write_pair` applies on write,
//!   `--file`/`--blob` as alternatives to the sequence, and the repository
//!   layouts a read runs from. Where it enters this file's territory it takes
//!   **one representative** of each question and stops: one `--type=` per
//!   conversion (`t.yes`, `t.kilo`, `t.big`, `t.one`, `t.rel`, `t.stamp`,
//!   `t.color`, `t.rgb`), one three-scope stack (`system`/`global`/`repo`), one
//!   `env`+`cmdline` pair and one `repo`+`worktree` pair, one `[include]` and
//!   two `includeIf` conditions (`gitdir:**`, `onbranch:main`), four
//!   `--get-urlmatch` rows over a three-stanza `[http]`, and eight file-parsing
//!   lines. This module takes the rest of each: the *whole* boolean spelling
//!   table rather than `yes`, the *whole* documented exit-code table rather than
//!   the cells its own refusals happen to land on, the include graph rather than
//!   one edge, and the `urlmatch.c` ranking rules — user, port, scheme, case —
//!   rather than the host-vs-path axis alone. Nothing here repeats an argv or a
//!   premise from there, which the corpus-wide
//!   `no_case_id_appears_twice_in_the_corpus` test enforces: a case id carries
//!   its whole premise, so a duplicated pair fails `cargo test`.
//! * **`config_reads.rs`** owns "a setting changes what some *other* verb
//!   prints" — 90-odd `color.*`, `diff.*`, `status.*`, `log.*` keys read back
//!   through `diff`, `status`, `log`, `grep`, `blame`. Its `scoped` group is the
//!   only place it touches precedence, and it does so with two scopes and two
//!   keys. Every case here runs `config` itself, so no verb's rendering is in
//!   the way of the value.
//! * **`exit_codes.rs`** owns fifteen `config` refusals as part of a corpus-wide
//!   exit-code sweep: `--file` with no operand, `--get-all --unset`,
//!   `--list --local --global`, `--file src` read and write, `--file
//!   nosuchfile.cfg`, `--blob HEAD:README.md`, `--get-regexp ^nosuch`,
//!   `--unset`/`--unset-all nosuch.key`, `--get a`, `--get a.`,
//!   `--rename-section nosuch other`, `--get-urlmatch http nosuchurl` and
//!   `--get-color nosuch.color`. [`exit_code_table`] below is the *rest* of the
//!   table — the cells none of those reach — and the module header records the
//!   measured table in full, including the two documented codes that turn out
//!   to be unreachable.
//! * **`env_layer.rs`** owns the `GIT_*` variables that are not the config
//!   scopes, including `GIT_CONFIG_PARAMETERS` (the serialized form of `-c`).
//!   The `GIT_CONFIG_COUNT`/`KEY`/`VALUE` triple is a *scope* and is here.
//! * **`globals_layer.rs`** owns `--config-env=<key>=<var>`, which is a delivery
//!   mechanism for one pair rather than a position in the sequence.
//! * **`discovery.rs`** owns which repository git decides it is in. That
//!   decision is upstream of every lookup here; nothing below moves the working
//!   directory except where the *condition* being evaluated is about it.
//! * **`helpers_credentials.rs`** owns `credential.<url>.*`. That is the same
//!   `urlmatch.c` code reached through a different consumer: `credential.c`
//!   builds a URL from the request and calls `urlmatch_config_entry` to collect
//!   `helper`, `username` and `useHttpPath`, and the module measures it through
//!   `git credential fill` on host-match / other-host / other-scheme. It never
//!   asks the *ranking* question, because a credential lookup that matches two
//!   stanzas cannot show which one it preferred — the helper prints one answer
//!   either way. [`url_match`] asks the matcher directly, where the winning
//!   value is the output.
//!
//! # What can and cannot be put in place, and what that decides
//!
//! **A case cannot create a file.** The only files that reach a fixture copy are
//! the five [`ConfigScope`] file scopes, written by `runner::install_config`
//! from the case's own `ConfigEntry` list:
//!
//! | scope | file | in the lookup sequence? |
//! |---|---|---|
//! | `Repo` | `.git/config` | yes, and it already holds `git init`'s `[core]` stanza |
//! | `Worktree` | `.git/config.worktree` | only once `extensions.worktreeConfig` is set, which drawing the scope does |
//! | `Global` | `.git/parity-global.config`, via `$GIT_CONFIG_GLOBAL` | yes |
//! | `System` | `.git/parity-system.config`, via `$GIT_CONFIG_SYSTEM` | yes, and drawing it also clears `GIT_CONFIG_NOSYSTEM` |
//! | `Modules` | `.gitmodules` | **no** — `config.c` never reads it |
//!
//! That table is the shape of this whole module. Three consequences:
//!
//!  * **Every scope in `do_git_config_sequence` is reachable**, including the
//!    two [`crate::env::harden`] pins at `/dev/null`. A case never names either
//!    path — `runner::scope_file` computes it from the side's own fixture root —
//!    so the pin stays closed and the layering is still measurable. What is
//!    *not* reachable is `$HOME/.gitconfig` and `/etc/gitconfig` at their real
//!    paths, and nothing here needs them: the sequence position is the subject,
//!    not the filesystem location.
//!  * **`.gitmodules` is the only file that is in the worktree and out of the
//!    sequence**, which makes it the only usable *include target* and the only
//!    usable `--file` operand that a case can fill with arbitrary bytes. Every
//!    include chain below is anchored on it.
//!  * **`.git/parity-{global,system}.config` are created fresh**, so a raw entry
//!    written into one of them starts at **byte 0** of its file. That is what
//!    makes a BOM and a CRLF-only file testable at all: appended into
//!    `.git/config` a BOM is mid-file, which is a different question (and a
//!    parse error — see [`file_grammar`]).
//!
//! # `gitdir:` is a path, and only one relative spelling can ever match
//!
//! Measured against stock 2.55.0 rather than read off the documentation, because
//! the documentation does not say which end the anchor is at.
//! `config.c:prepare_include_condition_pattern` rewrites the condition three
//! ways before `wildmatch` sees it: a pattern starting `./` is spliced onto the
//! **directory of the file the condition is written in**; any other relative
//! pattern gets `**/` prepended; and a pattern ending `/` gets `**` appended.
//! The text it is matched against is `strbuf_realpath($GIT_DIR)` — absolute, and
//! different on the two sides.
//!
//! So a condition written in `.git/config` anchors at `.git/` and is matched
//! against `.git`, and **cannot match**: `gitdir:./` becomes `<repo>/.git/**`,
//! which needs the git directory to be *below* itself. Measured:
//!
//! ```text
//! $ git config --get inc.k          # [includeIf "gitdir:./"] in .git/config
//! (exit 1, no output)
//! $ git config --get inc.k          # [includeIf "gitdir:./.git/"]
//! (exit 1, no output)
//! ```
//!
//! The one relative spelling that *can* match is a condition written in a file
//! at the **worktree root**, where `./` anchors at the worktree and `<repo>/**`
//! does contain `<repo>/.git`. `.gitmodules` is the only such file a case can
//! write, and [`include_graph`] reaches it through `--file .gitmodules
//! --includes`. Conditional includes are therefore testable — root-independently,
//! with no absolute path anywhere — and the two root-independent absolute-ish
//! spellings (`gitdir:**/.git`, `gitdir/i:**/.GIT`) carry the rest.
//!
//! # Unmeasurable, and why
//!
//!  * **`--type=expiry-date` on a relative spelling.** `approxidate` resolves
//!    `2.weeks.ago` against the wall clock, so the two sides disagree whenever a
//!    second ticks between them. Only an absolute ISO timestamp is a parse
//!    rather than a clock read, and `config_cmd.rs` already pins one
//!    (`t.stamp = 2005-04-07T22:13:13`); there is no second absolute spelling
//!    that asks a different question, so this file adds none. `--type=expiry-date
//!    --default=never` is the one clock-free extra and it is [`typed_values`].
//!  * **`--show-origin` on the global and system files.** Both print an absolute
//!    path. The runner does mask the fixture root, but `config_cmd.rs` already
//!    ruled these out and re-litigating that from here would put two modules'
//!    cases on one question with only one of them documented.
//!    `--show-scope` names the scope without the path and covers the ground.
//!  * **`--type=path` on `~user/…` for a user that exists.** The answer is that
//!    machine's `/etc/passwd`. The *refusal* for a user that does not exist is
//!    clock-free and machine-independent and is pinned.
//!  * **A config file that cannot be *written*.** `git-config(1)` documents
//!    code 4 for it; reaching it needs a `chmod` no case can perform, and every
//!    unwritable path this harness can name (`/dev/null`, a missing directory)
//!    answers **255** instead. See [`exit_code_table`].
//!
//! # The documented exit-code table, measured
//!
//! `git-config(1)` lists seven codes. Measured against stock 2.55.0 in a
//! hand-built copy of [`Shape::Linear`], with the premise
//! `[demo] one = 1 / two = a / two = b`:
//!
//! | documented | invocation | measured |
//! |---|---|---|
//! | — | `--get demo.one` | **0** |
//! | — | `--get demo.missing` | **1**, both streams empty |
//! | 1 = invalid section or key | `--get invalidkey` | **1** `key does not contain a section` |
//! | 1 | `--get demo.` | **1** `key does not contain variable name` |
//! | 1 | `--get 'demo.a b'` | **1** `invalid key` |
//! | 1 | `config 'demo.a b' v` | **1** — the same message, and the same code, on the *write* |
//! | 2 = no section or name given | `config invalidkey v` | **2**, same message as the read |
//! | 2 | `config demo. v` | **2**, same message as the read |
//! | — | `--rename-section demo 'in valid'` | **255** `invalid section name` |
//! | — | `--rename-section demo demo` | **0** — renaming a section to itself is legal |
//! | 3 = invalid config file | `--file src --get a.b` | **3** (`exit_codes.rs` owns it) |
//! | 3 | a malformed file in the *sequence* | **128** `bad config line <n>` |
//! | 4 = file cannot be written | `--file /dev/null demo.k v` | **255** `could not lock config file` |
//! | 4 | `--file nosuchdir/x demo.k v` | **255**, same |
//! | 5 = unset an option that does not exist | `--unset demo.nosuch` | **5**, both streams empty |
//! | 5 = multiple lines match | `--unset demo.two` | **5** + `warning: … has multiple values` |
//! | 5 | `config demo.two z` | **5** + warning + `error: cannot overwrite multiple values` |
//! | 6 = invalid regexp | `--get demo.v '['` | **6** `invalid pattern` |
//! | 6 | `--get-regexp '['` | **6** `invalid key pattern` |
//! | — | `--remove-section nosuch` | **128** `fatal: no such section` |
//! | — | `--rename-section nosuch other` | **128**, same message |
//! | — | `--type=bogus --get demo.one` | **128** |
//! | — | `--bool --get demo.two` (`a` is not a bool) | **128** |
//! | — | `config` with no action | **129** `no action specified` |
//! | — | `--get` with no key | **129** `wrong number of arguments` |
//! | — | `--get a.b c d e` | **129**, same message |
//! | — | `--name-only --get-all` | **129** `only applicable to --list or --get-regexp` |
//! | — | `--default=zz --get-all` | **129** `--default is only applicable to --get` |
//! | — | `--fixed-value --get` with no pattern | **129** `only applies with 'value-pattern'` |
//! | — | `--system --local --get` | **129** `only one config file at a time` |
//!
//! Two documented codes are effectively unreachable from this harness: **4**
//! never appears (every unwritable path answers 255) and **3** appears only for
//! the `--file`-names-a-directory pair `exit_codes.rs` already owns — a
//! malformed file reached through the *sequence* dies 128 instead. The rest is
//! pinned below, minus the cells `exit_codes.rs` and `config_cmd.rs` hold.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

pub fn cases(out: &mut Vec<Case>) {
    include_graph(out);
    include_conditions(out);
    scope_stack(out);
    typed_values(out);
    value_selectors(out);
    url_match(out);
    file_grammar(out);
    exit_code_table(out);
}

// ---------------------------------------------------------------------------
// Case builders
// ---------------------------------------------------------------------------

/// A `config` invocation on [`Shape::Linear`] over the given scope files.
fn on(out: &mut Vec<Case>, entries: Vec<ConfigEntry>, args: &[&str]) {
    out.push(Case::new("config", args, Shape::Linear).with_scoped_config(entries));
}

/// The same with stderr compared byte for byte — for the refusals, where the
/// message and the code are both the interface.
fn on_strict(out: &mut Vec<Case>, entries: Vec<ConfigEntry>, args: &[&str]) {
    out.push(Case::strict("config", args, Shape::Linear).with_scoped_config(entries));
}

/// One raw line into `.git/config`.
fn repo(text: &str) -> ConfigEntry {
    ConfigEntry::raw(ConfigScope::Repo, text)
}

/// One raw line into `.gitmodules` — the only worktree file out of the sequence.
fn modules(text: &str) -> ConfigEntry {
    ConfigEntry::raw(ConfigScope::Modules, text)
}

/// One raw line into `.git/parity-global.config`, created fresh, so the text
/// starts at byte 0.
fn global(text: &str) -> ConfigEntry {
    ConfigEntry::raw(ConfigScope::Global, text)
}

/// One raw line into `.git/parity-system.config`, created fresh.
fn system(text: &str) -> ConfigEntry {
    ConfigEntry::raw(ConfigScope::System, text)
}

// ---------------------------------------------------------------------------
// The include graph
// ---------------------------------------------------------------------------

/// `[include] path = …`: where the path is resolved from, what a miss does, and
/// **where in the merge the included values land**.
///
/// The ordering rule is the half of includes that a port almost always gets
/// wrong, because the obvious implementation — parse the file, then expand every
/// `include.path` it collected — produces the right answer for a file whose only
/// setting is the include and the wrong one for every other file. Git expands an
/// include *at the point the directive is read* (`config.c:git_config_include`
/// recurses from inside the parser's callback), so a value set before the
/// include is overridden by it and a value set after overrides it back. Measured
/// on stock 2.55.0 with `[ord] k = before`, the include, and `[ord] k = after`
/// in one file:
///
/// ```text
/// $ git config --get ord.k          # after
/// $ git config --get-all ord.k      # before / from-include / after
/// ```
///
/// A port that expands includes last prints `from-include` for `--get` and
/// `before / after / from-include` for `--get-all`, and both are visible here.
///
/// The rest of the group is the graph's edges: a path relative to the *including
/// file's directory* rather than to the cwd, a `~` that is expanded before the
/// lookup, an absolute path, a missing file (silently skipped — not an error),
/// `include.path` with no file behind it still appearing in `--list --local`
/// because that view does not expand, an `[include]` stanza with some *other*
/// key in it, a target that parses to nothing, and a cycle.
fn include_graph(out: &mut Vec<Case>) {
    // ---- the ordering rule ----
    let ordered = || {
        vec![
            modules("[ord]\n\tk = from-include"),
            repo("[ord]\n\tk = before\n[include]\n\tpath = ../.gitmodules\n[ord]\n\tk = after"),
        ]
    };
    on(out, ordered(), &["config", "--get", "ord.k"]);
    on(out, ordered(), &["config", "--get-all", "ord.k"]);
    on(out, ordered(), &["config", "--show-origin", "--get-all", "ord.k"]);
    // Without the trailing stanza the include is last and wins, which is the
    // control: a port that expands late passes this one and fails the three
    // above, so the pair separates "wrong order" from "no includes at all".
    on(
        out,
        vec![
            modules("[ord]\n\tk = from-include"),
            repo("[ord]\n\tk = before\n[include]\n\tpath = ../.gitmodules"),
        ],
        &["config", "--get-all", "ord.k"],
    );

    // ---- what a path means, and what a miss does ----
    // Relative to `.git/`, the directory of the file holding the directive —
    // *not* to the working directory, which is the fixture root. `../` is the
    // whole test: resolved against the cwd it would name the fixture's parent.
    on(
        out,
        vec![modules("[inc]\n\tk = relative-to-including-file"), repo("[include]\n\tpath = ../.gitmodules")],
        &["config", "--show-origin", "--get", "inc.k"],
    );
    // A missing file is skipped in silence: exit 1 from the lookup, nothing on
    // stderr, and the directive itself still printed back by `--list`, which does
    // expand: the row that says the directive was read and the file was not there.
    let missing = || vec![repo("[include]\n\tpath = ../nosuch.config")];
    on_strict(out, missing(), &["config", "--get", "inc.k"]);
    on(out, missing(), &["config", "--list"]);
    // `~` is expanded before the file is opened, so this is a miss under the
    // hermetic HOME rather than a parse error — and the *unexpanded* spelling is
    // what `--list` prints back.
    on(out, vec![repo("[include]\n\tpath = ~/nosuch.config")], &["config", "--list"]);
    // An absolute path that is not there, and an `[include]` stanza whose key is
    // not `path`: both inert, both still listed.
    on(out, vec![repo("[include]\n\tpath = /nonexistent/x.config")], &["config", "--list"]);
    on(out, vec![repo("[include]\n\tnopath = x")], &["config", "--list"]);
    // A target that is a real file and parses to nothing. `README.md` is
    // `# fixture`, which is a comment.
    on(out, vec![repo("[include]\n\tpath = ../README.md")], &["config", "--list"]);

    // ---- the depth limit ----
    // A file that includes itself. `config.c` stops at MAX_INCLUDE_DEPTH and
    // dies; strict because the message names the depth and both paths, and both
    // paths print *relative* here, so the two sides agree byte for byte.
    on_strict(out, vec![repo("[include]\n\tpath = config")], &["config", "--get", "inc.k"]);

    // ---- a chain, through the one file that is not in the sequence ----
    // `--file` replaces the whole sequence and turns include expansion **off**,
    // so `--includes` is the switch that makes the chain visible at all; the
    // pair is the measurement. Three levels, each hop a different spelling: an
    // unconditional relative path out of the worktree root, then a conditional
    // one inside `.git/`.
    let chain = || {
        vec![
            modules("[includeIf \"gitdir:./\"]\n\tpath = .git/parity-global.config"),
            global("[includeIf \"onbranch:main\"]\n\tpath = parity-system.config"),
            system("[deep]\n\tk = three-levels"),
        ]
    };
    on(out, chain(), &["config", "--file", ".gitmodules", "--get", "deep.k"]);
    on(out, chain(), &["config", "--file", ".gitmodules", "--includes", "--get", "deep.k"]);
    on(
        out,
        chain(),
        &["config", "--file", ".gitmodules", "--includes", "--show-origin", "--get", "deep.k"],
    );
    on(out, chain(), &["config", "--file", ".gitmodules", "--includes", "--show-scope", "--list"]);
    // The same chain with the first condition falsified: nothing below level one
    // is read, which a port that expands every `includeIf` unconditionally fails.
    on(
        out,
        vec![
            modules("[includeIf \"gitdir:./nope/\"]\n\tpath = .git/parity-global.config"),
            global("[deep]\n\tk = should-not-be-reached"),
        ],
        &["config", "--file", ".gitmodules", "--includes", "--get", "deep.k"],
    );
    // A cycle reached through `--file`, whose diagnostic names `.gitmodules`
    // rather than `.git/config` — a different pair of paths through the same
    // depth counter.
    on_strict(
        out,
        vec![modules("[include]\n\tpath = .gitmodules")],
        &["config", "--file", ".gitmodules", "--includes", "--get", "deep.k"],
    );
}

// ---------------------------------------------------------------------------
// includeIf conditions
// ---------------------------------------------------------------------------

/// `[includeIf "<condition>"]`, one case per condition keyword and per rewrite
/// rule the pattern goes through.
///
/// Four keywords exist (`gitdir:`, `gitdir/i:`, `onbranch:`, `hasconfig:`) and
/// an unrecognised one is **not an error** — `config.c:include_condition_is_true`
/// falls through to "false", so a typo silently drops the include. That is the
/// row a port is most likely to turn into a `die`, and it is the first pair
/// below.
///
/// The `gitdir` patterns here are all root-independent by construction: `**/.git`
/// and `**/.GIT` name the one path component every fixture copy shares. The
/// `/i` pair is the whole case-folding measurement — `gitdir:**/.GIT` must miss
/// where `gitdir/i:**/.GIT` hits, and a port that folds unconditionally passes
/// one and fails the other. `~/**` cannot match because the hermetic HOME is not
/// an ancestor of the fixture, which makes it a clean test that `~` is expanded
/// *at all*: a port that leaves the tilde literal also produces a miss, so this
/// one is paired with the `--type=path` tilde case in [`typed_values`], where
/// expansion is visible in the output.
///
/// `onbranch:` is matched against the branch *short name* with `wildmatch`, so
/// `ma?n` and `m*` hit, `refs/heads/main` misses (it is not a short name),
/// `main/` misses (the trailing slash appends `**`), and `MAIN` misses (no
/// folding without `/i`, which `onbranch` does not accept).
///
/// `hasconfig:remote.*.url:` is the one condition that is a function of the
/// configuration being read rather than of the repository, and it is evaluated
/// over the values collected *so far* — which is why the `[remote]` stanza in
/// the premise comes before the `[includeIf]`.
fn include_conditions(out: &mut Vec<Case>) {
    // The include target every condition points at, and the read that shows
    // whether it fired.
    let with_condition = |cond: &str| {
        vec![
            modules("[inc]\n\tk = fired"),
            repo(&format!("[includeIf \"{cond}\"]\n\tpath = ../.gitmodules")),
        ]
    };
    for cond in [
        // gitdir: the `**/` prefix rule, and a component that is not there.
        "gitdir:**/.git",
        "gitdir:*/.git",
        "gitdir:**/nosuchdir/.git",
        // The trailing-slash rule: `/` appends `**`, so this needs the git
        // directory to have something below it and misses.
        "gitdir:**/.git/",
        // The `./` anchor, written in `.git/config`, which anchors at `.git/`
        // and therefore cannot match — see the module header.
        "gitdir:./",
        "gitdir:./.git/",
        // Case folding, on the one component every copy has.
        "gitdir:**/.GIT",
        "gitdir/i:**/.GIT",
        "gitdir/i:**/.git",
        // `~` expanded, and not an ancestor of the fixture.
        "gitdir:~/**",
        // onbranch, and the four spellings that do not match.
        "onbranch:main",
        "onbranch:ma?n",
        "onbranch:m*",
        "onbranch:MAIN",
        "onbranch:main/",
        "onbranch:refs/heads/main",
        // An unrecognised keyword, and one that is a prefix of a real one.
        "bogus:whatever",
        "gitdirx:**",
    ] {
        on(out, with_condition(cond), &["config", "--get", "inc.k"]);
    }

    // hasconfig: the condition that reads the configuration it is inside. The
    // URL is matched with the same pattern machinery, so the three rows are
    // "exact", "glob", and "a different value".
    for cond in [
        "hasconfig:remote.*.url:./peer",
        "hasconfig:remote.*.url:**/peer",
        "hasconfig:remote.*.url:./other",
    ] {
        on(
            out,
            vec![
                modules("[inc]\n\tk = fired"),
                repo(&format!(
                    "[remote \"origin\"]\n\turl = ./peer\n[includeIf \"{cond}\"]\n\tpath = ../.gitmodules"
                )),
            ],
            &["config", "--get", "inc.k"],
        );
    }
    // The same condition with no remote at all: false, not an error.
    on(
        out,
        with_condition("hasconfig:remote.*.url:./peer"),
        &["config", "--get", "inc.k"],
    );

    // An included key joins the **scope of the including file**, not a scope of
    // its own, and `--show-scope` is the only reader that says so. The pair
    // below sets the same key in the global file and in a file included from the
    // repository one, so a port that invents an `include` scope, or that labels
    // the included value by the file it came from, prints a different first
    // column for one of the two rows.
    on(
        out,
        vec![
            modules("[inc]\n\tk = from-included"),
            global("[inc]\n\tk = from-global"),
            repo("[includeIf \"gitdir:**/.git\"]\n\tpath = ../.gitmodules"),
        ],
        &["config", "--show-scope", "--get-all", "inc.k"],
    );
}

// ---------------------------------------------------------------------------
// The scope stack
// ---------------------------------------------------------------------------

/// All six layered scopes at once, then each reader that reports on them.
///
/// `config_cmd.rs` stacks three (`system`/`global`/`repo`) and, separately, two
/// (`env`/`cmdline`). Neither stack can answer the question this one can: with
/// **every** scope carrying the same key, `--get-all` has to print six values in
/// `do_git_config_sequence` order and `--show-scope` has to name six scopes, so a
/// port that has any adjacent pair swapped is caught by one line rather than by a
/// verdict. The worktree scope is the one most often placed wrongly — it is
/// *above* the repository and *below* the environment, and it is inert until
/// `extensions.worktreeConfig` is set, which drawing the scope does.
///
/// The `-z` rows are a separate question from the framing `config_cmd.rs` pins
/// with `-l -z`: `--show-scope` and `--show-origin` add fields *before* the
/// name, and `--get-regexp -z` uses `name\nvalue\0` while `--get-all -z` uses
/// `value\0` — three different frames from one flag, and a port that implements
/// `-z` as "replace the terminator" gets two of them wrong.
///
/// The scope *selectors* read one file rather than filtering the merged view,
/// which is why `--global --get` answers with the global value while the merged
/// `--get` answers with the command line's. `--worktree` without the extension
/// is not an error and is not empty: it falls back to `.git/config`.
fn scope_stack(out: &mut Vec<Case>) {
    let six = || {
        vec![
            ConfigEntry::set(ConfigScope::System, "s.k", "from-system"),
            ConfigEntry::set(ConfigScope::Global, "s.k", "from-global"),
            ConfigEntry::set(ConfigScope::Repo, "s.k", "from-repo"),
            ConfigEntry::set(ConfigScope::Worktree, "s.k", "from-worktree"),
            ConfigEntry::set(ConfigScope::Env, "s.k", "from-env"),
            ConfigEntry::set(ConfigScope::CommandLine, "s.k", "from-cmdline"),
        ]
    };
    for args in [
        &["config", "--get", "s.k"][..],
        &["config", "--get-all", "s.k"][..],
        &["config", "--show-scope", "--get-all", "s.k"][..],
        &["config", "-z", "--show-scope", "--get-all", "s.k"][..],
        &["config", "-z", "--get-all", "s.k"][..],
        &["config", "-z", "--get-regexp", "^s\\."][..],
        &["config", "--name-only", "--get-regexp", "^s\\."][..],
        // Each selector, reading its own file past everything above it.
        &["config", "--system", "--get", "s.k"][..],
        &["config", "--global", "--get", "s.k"][..],
        &["config", "--local", "--get", "s.k"][..],
        &["config", "--worktree", "--get", "s.k"][..],
        &["config", "--system", "--list"][..],
        &["config", "--global", "--list"][..],
    ] {
        on(out, six(), args);
    }

    // The same stack with the top scopes peeled off one at a time, read through
    // plain `--get`. This is the row that matters to every *other* command: a
    // port whose `--get-all` order is wrong but whose "last wins" is right still
    // answers `from-cmdline` above, and only these say which file the winner
    // came from when the command line is not in play. Three answers, three
    // scopes, one argv.
    on(
        out,
        vec![
            ConfigEntry::set(ConfigScope::System, "s.k", "from-system"),
            ConfigEntry::set(ConfigScope::Global, "s.k", "from-global"),
            ConfigEntry::set(ConfigScope::Repo, "s.k", "from-repo"),
            ConfigEntry::set(ConfigScope::Worktree, "s.k", "from-worktree"),
        ],
        &["config", "--get", "s.k"],
    );
    on(
        out,
        vec![
            ConfigEntry::set(ConfigScope::System, "s.k", "from-system"),
            ConfigEntry::set(ConfigScope::Global, "s.k", "from-global"),
            ConfigEntry::set(ConfigScope::Repo, "s.k", "from-repo"),
        ],
        &["config", "--get", "s.k"],
    );
    on(
        out,
        vec![
            ConfigEntry::set(ConfigScope::System, "s.k", "from-system"),
            ConfigEntry::set(ConfigScope::Global, "s.k", "from-global"),
        ],
        &["config", "--get", "s.k"],
    );

    // `--worktree` with the extension *unset* is the row a port gets wrong by
    // answering nothing: git falls back to `.git/config`, so this prints the
    // repository's value rather than a miss.
    on(
        out,
        vec![ConfigEntry::set(ConfigScope::Repo, "s.k", "from-repo")],
        &["config", "--worktree", "--get", "s.k"],
    );
    // The two scopes `env::harden` pins at `/dev/null` for every case that does
    // not draw them: reachable, and empty rather than an error. This is the row
    // that says which scopes this harness can and cannot see.
    on(out, vec![], &["config", "--global", "--list"]);
    on(out, vec![], &["config", "--system", "--list"]);
    on_strict(out, vec![], &["config", "--global", "--get", "core.bare"]);

    // Order *within* one scope, which is a different rule from order between
    // scopes: the last stanza in a file wins, and `--get-all` prints them in
    // file order. Delivered as one raw block so the two stanzas are adjacent in
    // one file rather than two appends.
    let twice = || vec![global("[dup]\n\tk = first\n[dup]\n\tk = second"), repo("[dup]\n\tk = repo")];
    on(out, twice(), &["config", "--get", "dup.k"]);
    on(out, twice(), &["config", "--show-scope", "--get-all", "dup.k"]);

    // Two `-c` pairs for one key: the command-line scope has its own ordering
    // and the last one given wins, which is not the same code as the file
    // parser's last-wins.
    out.push(
        Case::new("config", &["config", "--get-all", "c.k"], Shape::Linear)
            .with_config(&[("c.k", "first"), ("c.k", "second")]),
    );
    // …and the environment scope's own ordering, read back with its scope label.
    on(
        out,
        vec![
            ConfigEntry::set(ConfigScope::Env, "e.k", "first"),
            ConfigEntry::set(ConfigScope::Env, "e.k", "second"),
        ],
        &["config", "--show-scope", "--get-all", "e.k"],
    );
}

// ---------------------------------------------------------------------------
// Type conversion
// ---------------------------------------------------------------------------

/// Every spelling each `--type` accepts, and the ones it rejects.
///
/// `--type` applies to `--get-regexp` as well as to `--get`, so one invocation
/// prints the whole conversion table for one type and a diff names the exact
/// cell that moved. That is worth more than one case per value: a port with
/// `on` missing from its boolean list fails one line of one case here, and the
/// failure block shows the other eleven passing beside it.
///
/// `git_config_bool` accepts `true`/`yes`/`on`/`1`/**absent** and
/// `false`/`no`/`off`/`0`/**empty**, folds case, and — the part that is not in
/// the documentation — treats *any* non-zero integer as true, so `2` and `-1`
/// are true while `00` is false. `bool-or-int` tries bool first and falls back
/// to int, which is why `1` prints `1` and `true` prints `true` out of the same
/// converter. `git_parse_int` applies `k`/`m`/`g` (case-folded), accepts a sign,
/// and reads `0x`/`0` radix prefixes through `strtoimax`, storing into an
/// `intmax_t` — so `9223372036854775807` is exact and one more is `out of
/// range`, a different diagnostic from the `invalid unit` a non-number gets.
fn typed_values(out: &mut Vec<Case>) {
    // Every true spelling, in one file, read through one conversion.
    on(
        out,
        vec![repo(
            "[bt]\n\
             \ta = true\n\
             \tb = yes\n\
             \tc = on\n\
             \td = 1\n\
             \te\n\
             \tf = TRUE\n\
             \tg = YeS\n\
             \th = On\n\
             \ti = 2\n\
             \tj = -1",
        )],
        &["config", "--type=bool", "--get-regexp", "^bt\\."],
    );
    // Every false spelling.
    on(
        out,
        vec![repo(
            "[bf]\n\
             \ta = false\n\
             \tb = no\n\
             \tc = off\n\
             \td = 0\n\
             \te =\n\
             \tf = FALSE\n\
             \tg = Off\n\
             \th = 00",
        )],
        &["config", "--type=bool", "--get-regexp", "^bf\\."],
    );
    // The two rejections, which are the only way a valueless key and `key =`
    // stop looking alike. Strict: the message names the key and the value.
    on_strict(out, vec![repo("[bx]\n\tk = tru")], &["config", "--type=bool", "--get", "bx.k"]);
    on_strict(out, vec![repo("[bx]\n\tk = \" \"")], &["config", "--type=bool", "--get", "bx.k"]);
    on_strict(out, vec![repo("[bx]\n\tk = 1.0")], &["config", "--type=bool", "--get", "bx.k"]);

    // bool-or-int: the fallback, over the values that separate the two halves.
    on(
        out,
        vec![repo(
            "[oi]\n\
             \ta = true\n\
             \tb = 1\n\
             \tc = 0\n\
             \td = 2\n\
             \te = -1\n\
             \tf = 00\n\
             \tg\n\
             \th =\n\
             \ti = 3k",
        )],
        &["config", "--type=bool-or-int", "--get-regexp", "^oi\\."],
    );

    // int: the suffixes, the signs, the radix prefixes, and the boundary.
    on(
        out,
        vec![repo(
            "[in]\n\
             \ta = 3k\n\
             \tb = 3K\n\
             \tc = 2m\n\
             \td = 2M\n\
             \te = 1g\n\
             \tf = 1G\n\
             \tg = -4k\n\
             \th = +5\n\
             \ti = 0x10\n\
             \tj = 010\n\
             \tk = 9223372036854775807",
        )],
        &["config", "--type=int", "--get-regexp", "^in\\."],
    );
    // The two int refusals, which carry different words: a value that overflows
    // says `out of range`, a value that is not a number says `invalid unit`, and
    // an in-range value with a suffix that pushes it over says `out of range`
    // too — so the multiply happens before the check.
    for value in ["9223372036854775808", "9223372036854775807k", "5x", "abc", "\" 7 \""] {
        on_strict(
            out,
            vec![repo(&format!("[ix]\n\tk = {value}"))],
            &["config", "--type=int", "--get", "ix.k"],
        );
    }

    // path: `~` is expanded against HOME, `~user` for a user that does not exist
    // is a refusal, an absolute path is returned unchanged, and the empty value
    // stays empty. The tilde row is what proves expansion happens at all — the
    // `gitdir:~/**` miss in [`include_conditions`] cannot tell an expanded tilde
    // from a literal one.
    on(out, vec![repo("[pa]\n\tk = ~/x")], &["config", "--type=path", "--get", "pa.k"]);
    on(out, vec![repo("[pa]\n\tk = /abs/x")], &["config", "--type=path", "--get", "pa.k"]);
    on(out, vec![repo("[pa]\n\tk =")], &["config", "--type=path", "--get", "pa.k"]);
    on_strict(
        out,
        vec![repo("[pa]\n\tk = ~nosuchuser000/x")],
        &["config", "--type=path", "--get", "pa.k"],
    );

    // color: `color_parse` renders an attribute list, a 256-colour index and a
    // 24-bit hex value into three different escape sequences, and rejects a name
    // it does not know — as a *parse* error naming the config line, not as a
    // conversion error naming the key.
    on(out, vec![repo("[co]\n\tk = bold ul 202")], &["config", "--type=color", "--get", "co.k"]);
    on(out, vec![repo("[co]\n\tk = brightblue")], &["config", "--type=color", "--get", "co.k"]);
    on(out, vec![repo("[co]\n\tk = \"#00ff80\"")], &["config", "--type=color", "--get", "co.k"]);
    on(out, vec![repo("[co]\n\tk = nosuchcolour")], &["config", "--type=color", "--get", "co.k"]);

    // `--get-colorbool` is a third colour reader again: it answers with the exit
    // *code* when given one argument and prints `true`/`false` when given two,
    // and it resolves `auto` through `want_color`, which `env::harden`'s
    // `NO_COLOR=1` decides. The three values are the three branches.
    let cb = || vec![repo("[cb]\n\tauto = auto\n\tnever = never\n\talways = always")];
    on_strict(out, cb(), &["config", "--get-colorbool", "cb.auto"]);
    on_strict(out, cb(), &["config", "--get-colorbool", "cb.never"]);
    on_strict(out, cb(), &["config", "--get-colorbool", "cb.always"]);
    on(out, cb(), &["config", "--get-colorbool", "cb.auto", "true"]);
    on(out, cb(), &["config", "--get-colorbool", "cb.always", "false"]);

    // `--type=expiry-date` with no stored value at all: `--default=never` is the
    // one spelling of this conversion that is not a function of the clock.
    on(
        out,
        vec![],
        &["config", "--type=expiry-date", "--default=never", "--get", "ex.missing"],
    );
}

// ---------------------------------------------------------------------------
// Selecting among several values
// ---------------------------------------------------------------------------

/// The value-pattern machinery: which of a multi-valued key's values a reader
/// picks, and how the pattern is interpreted.
///
/// The optional second positional is a **regexp matched against the value**, and
/// it is not a filter that leaves the reader's own rule alone: `--get` still
/// returns the *last* match, `--get-all` returns every match in file order, and
/// `--get-regexp` intersects the name pattern with it. A leading `!` inverts.
/// `--fixed-value` replaces the regexp with a byte comparison, which changes
/// *which* value is selected rather than only whether one is — `a.b` as a regexp
/// also matches `a+b`, so the two spellings pick different values out of the
/// same file.
///
/// `config_cmd.rs` owns the `--get` half of that pair. The rest of the grid —
/// `--get-all`, `--get-regexp`, the inversion, and the three usage refusals that
/// say which readers each option is allowed on — is here.
fn value_selectors(out: &mut Vec<Case>) {
    // Two values that differ only by a regex metacharacter, plus a third that
    // neither pattern matches, so "every value" and "the matching values" are
    // different outputs.
    let multi = || vec![repo("[mv]\n\tv = a.b\n\tv = a+b\n\tv = xay\n\tw = 1")];

    on(out, multi(), &["config", "--get-all", "mv.v", "a.b"]);
    on(out, multi(), &["config", "--fixed-value", "--get-all", "mv.v", "a.b"]);
    on(out, multi(), &["config", "--fixed-value", "--get-all", "mv.v", "a+b"]);
    on(out, multi(), &["config", "--get-regexp", "^mv\\.", "a.b"]);
    on(out, multi(), &["config", "--fixed-value", "--get-regexp", "^mv\\.", "a.b"]);
    // Inversion, which is a prefix on the pattern rather than an option.
    on(out, multi(), &["config", "--get-all", "mv.v", "!a\\.b"]);
    on(out, multi(), &["config", "--get", "mv.v", "!a\\.b"]);
    // `get --regexp` takes the *name* as a pattern and still answers with one
    // value — the last — where `--get-regexp` answers with every row.
    on(out, multi(), &["config", "get", "--regexp", "^mv\\.v$"]);
    on(out, multi(), &["config", "get", "--all", "--regexp", "^mv\\."]);
    on(out, multi(), &["config", "get", "--all", "--show-names", "mv.v"]);
    // A type applied to a multi-valued read converts every value.
    on(out, multi(), &["config", "--type=int", "--get-all", "mv.w"]);
    // `--default` is consulted only when nothing is found, and is fed through
    // the conversion a stored value would have taken.
    on(out, multi(), &["config", "--default=zz", "--get", "mv.missing"]);
    on(out, multi(), &["config", "--default=zz", "--get", "mv.w"]);
    on(out, multi(), &["config", "--type=bool", "--default=no", "--get", "mv.missing"]);

    // ---- the three "this option is not allowed on that reader" refusals ----
    on_strict(out, multi(), &["config", "--name-only", "--get-all", "mv.v"]);
    on_strict(out, multi(), &["config", "--default=zz", "--get-all", "mv.missing"]);
    on_strict(out, multi(), &["config", "--fixed-value", "--get", "mv.v"]);
}

// ---------------------------------------------------------------------------
// URL matching
// ---------------------------------------------------------------------------

/// `urlmatch.c`'s ranking rules, one per axis, over one file that carries a
/// stanza for each.
///
/// `config_cmd.rs` asks the host-vs-path axis (bare section, host, host+path)
/// and asks it per key. The other five axes are here, and each is a rule a port
/// has to implement separately:
///
///  * **Longest path wins, and a longer URL falls back.** `/a/b` beats `/a`, and
///    a request for `/a/b/c` — which no stanza names — takes `/a/b` rather than
///    missing.
///  * **A user in the stanza must match the user in the URL**, and a request
///    carrying a *different* user falls back to the stanza with no user at all
///    rather than failing.
///  * **The port is normalised.** `https://example.com` and
///    `https://example.com:443` are the same URL, so a stanza written with the
///    explicit default port matches a request without one; a non-default port
///    matches neither.
///  * **The scheme is compared exactly.** `http://` and `https://` are different
///    URLs with the same host.
///  * **The host folds case on both sides** — the stanza's and the request's —
///    while the **path does not**.
///
/// A stanza whose subsection is not a URL at all (`[http "example.com"]`, no
/// scheme) is silently never selected, which is the row that catches a port
/// falling back to a substring match.
fn url_match(out: &mut Vec<Case>) {
    /// One `[http]` section carrying a stanza per ranking axis. `sslVerify` is
    /// the key most of them set, so one request has to rank them against each
    /// other; `proxy` and `cookieFile` are set only by the case-folded and
    /// explicit-port stanzas, so the whole-section read shows those two being
    /// taken from stanzas the `sslVerify` answer did not come from.
    const URLS: &str = "[http \"https://example.com\"]\n\
                        \tsslVerify = host\n\
                        [http \"https://EXAMPLE.COM\"]\n\
                        \tproxy = folded-host\n\
                        [http \"https://user@example.com\"]\n\
                        \tsslVerify = with-user\n\
                        [http \"https://example.com:443\"]\n\
                        \tcookieFile = ./explicit-port\n\
                        [http \"https://example.com/a\"]\n\
                        \tsslVerify = path-a\n\
                        [http \"https://example.com/a/b\"]\n\
                        \tsslVerify = path-a-b\n\
                        [http \"http://example.com\"]\n\
                        \tsslVerify = plain-http\n\
                        [http \"example.com\"]\n\
                        \tsslVerify = no-scheme";

    for url in [
        "https://example.com",
        "https://example.com/a",
        "https://example.com/a/b",
        // Longer than any stanza: the longest prefix wins rather than missing.
        "https://example.com/a/b/c",
        // The path is case-sensitive, so this falls back to the host stanza.
        "https://example.com/A",
        // User match, and a different user falling back.
        "https://user@example.com",
        "https://other@example.com",
        // The default port, explicit and absent, and a port that is neither.
        "https://example.com:443",
        "https://example.com:8443",
        // The host folds; the scheme does not.
        "https://EXAMPLE.com",
        "http://example.com",
    ] {
        on(out, vec![repo(URLS)], &["config", "--get-urlmatch", "http.sslVerify", url]);
    }
    // The whole section, for a request that has to take three keys from three
    // different stanzas at once. The key names come back lower-cased.
    on(
        out,
        vec![repo(URLS)],
        &["config", "--get-urlmatch", "http", "https://example.com:443/a/b"],
    );
    // The case-folded stanza reached from a request spelled the other way, which
    // is the second half of "the host folds on both sides".
    on(out, vec![repo(URLS)], &["config", "--get-urlmatch", "http.proxy", "https://example.com"]);
    // A section that has no stanza for this URL at all: a miss, not an error.
    on_strict(out, vec![repo(URLS)], &["config", "--get-urlmatch", "http.sslVerify", "https://nosuch.example"]);
    on_strict(out, vec![repo(URLS)], &["config", "--get-urlmatch", "nosuch.key", "https://example.com"]);
}

// ---------------------------------------------------------------------------
// The file grammar
// ---------------------------------------------------------------------------

/// The bytes a config file may and may not be made of.
///
/// Every case here is delivered through [`ConfigScope::Global`] rather than the
/// repository, because the global file is **created by the case** and the raw
/// text therefore starts at byte 0. That is load-bearing for two of them: a BOM
/// is a BOM only at the start of a file (mid-file it is a parse error, which is
/// the pair below), and a file whose *every* line ends `\r\n` is a different
/// premise from a `.git/config` whose first seven lines do not.
///
/// The reads are `--list --show-scope`, so the case shows both what parsed and
/// which file it came from — a port that fails to open the global file at all
/// prints the same `local` rows as a port that parses it to nothing.
///
/// `config_cmd.rs` owns eight parse rows delivered into `.git/config`: a
/// valueless key, `key =`, a trailing comment, a backslash continuation, a
/// section and key on one line, case folding, an escape inside a quoted value,
/// and two stanzas of one section. None of those is repeated. What is here is
/// the encoding layer underneath them — line endings, the byte-order mark,
/// subsection quoting, and the character classes a section name is allowed to be
/// made of.
fn file_grammar(out: &mut Vec<Case>) {
    let listed = |text: &str| {
        Case::new("config", &["config", "--list", "--show-scope"], Shape::Linear)
            .with_scoped_config(vec![global(text)])
    };

    // Line endings. A CRLF file parses identically to an LF one: `\r` before a
    // `\n` is dropped, in a bare value, in a quoted one and before a comment.
    out.push(listed("[eol]\r\n\tk = v\r\n\tq = \"a b\"\r\n\tc = 1 # comment\r"));
    // A **bare** CR is not a line terminator — measured, and the opposite of
    // what "strip carriage returns" would do. Stock 2.55.0 reads the whole
    // three-line-looking file below as one section and one key whose value is
    // `v\r\tj = w`:
    //
    // ```text
    // $ git config --list --show-scope
    // global  cr.k=v<CR>     j = w
    // ```
    //
    // So the rule is `\r\n` → `\n` and nothing else, and a port that strips
    // every `\r` produces two keys where git produces one.
    out.push(listed("[cr]\r\tk = v\r\tj = w\r"));
    // A trailing newline is not required.
    out.push(listed("[nonl]\n\tk = v"));

    // The byte-order mark, at byte 0 and not at byte 0. The first parses; the
    // second is `fatal: bad config line 3`, and the message names the global
    // file by absolute path, so it is compared on stdout and exit code alone.
    out.push(listed("\u{feff}[bom]\n\tk = v"));
    out.push(listed("[a]\n\tx = 1\n\u{feff}[bom]\n\tk = v"));

    // Subsections. The name is byte-compared and *not* folded, may contain a
    // space, may be empty, and accepts `\"` and `\\` — the only two escapes a
    // subsection header has, which is a shorter list than a value's.
    out.push(listed("[sec \"sub section\"]\n\tk = spaced"));
    out.push(listed("[sec \"a\\\\b\"]\n\tk = backslash"));
    out.push(listed("[sec \"a\\\"b\"]\n\tk = quote"));
    out.push(listed("[sec \"\"]\n\tk = empty-subsection"));
    out.push(listed("[Sec \"MiXeD\"]\n\tKeY = folded-around-the-subsection"));
    // Reading one back by its full name, which is where the folding rule is
    // observable from the *query* side for a name containing a space.
    out.push(
        Case::new("config", &["config", "--get", "sec.sub section.k"], Shape::Linear)
            .with_scoped_config(vec![global("[sec \"sub section\"]\n\tk = spaced")]),
    );

    // Section names. `-` and `.` are legal in a *section* header (a dot there is
    // part of the section, not a subsection separator), a leading digit is
    // legal, upper case folds — and `_` and a space are both parse errors, which
    // is the pair that stops a port from accepting any identifier.
    out.push(listed("[a-b]\n\tk = dash"));
    out.push(listed("[a.b]\n\tk = dot-in-section"));
    out.push(listed("[9a]\n\tk = leading-digit"));
    out.push(listed("[A]\n\tk = folded"));
    out.push(listed("[a_b]\n\tk = underscore"));
    out.push(listed("[a b]\n\tk = space"));

    // A file that is only comments, in both spellings, and a file that is empty.
    out.push(listed("# hash\n; semicolon\n"));
    out.push(listed(""));
}

// ---------------------------------------------------------------------------
// The documented exit codes
// ---------------------------------------------------------------------------

/// The cells of `git-config(1)`'s exit-code table that no other module reaches.
///
/// The full measured table is in the module header, including the two documented
/// codes that turn out to be unreachable from a hermetic case. What is below is
/// the remainder after `exit_codes.rs`'s twelve rows and `config_cmd.rs`'s eight
/// refusals: the read/write split on a malformed key, the two shapes of 5, the
/// two spellings of 6, and the 129s that separate "this option does not apply
/// here" from "this argument list is the wrong length".
///
/// All strict except the three that end in a usage block, per the standing
/// policy. A code with no message and a code with a two-line message are
/// different contracts, and the corpus has caught a port agreeing on one and not
/// the other.
fn exit_code_table(out: &mut Vec<Case>) {
    let rw = || vec![repo("[ec]\n\tone = 1\n\ttwo = a\n\ttwo = b")];

    // ---- 1 and 2: a key that is not a key ----
    // Three malformed spellings, two messages, and a split that is *not* the
    // obvious one. `exit_codes.rs` owns the reads `--get a` (no section) and
    // `--get a.` (no variable name), both **1**. Given a value, those same two
    // become **2** — but the third spelling, a key whose variable name contains
    // a space, is `error: invalid key` and stays **1** on both sides. So the
    // rule is not "reads are 1 and writes are 2": it is which of the three
    // diagnostics fired, measured below rather than inferred.
    on_strict(out, rw(), &["config", "--get", "ec.a b"]);
    on_strict(out, rw(), &["config", "ec.a b", "v"]);
    on_strict(out, rw(), &["config", "ec.", "v"]);

    // ---- 5: the two shapes ----
    // A key that is simply not there — no warning, both streams empty.
    on_strict(out, rw(), &["config", "--unset", "ec.nosuch"]);
    // A single-valued write over a multi-valued key: the same 5, reached from
    // the writer rather than the unsetter, and carrying two stderr lines rather
    // than one.
    on_strict(out, rw(), &["config", "ec.two", "z"]);

    // ---- 6: the two spellings ----
    // A bad *value* pattern and a bad *name* pattern are the same code and
    // different words (`invalid pattern` vs `invalid key pattern`).
    on_strict(out, rw(), &["config", "--get-regexp", "["]);
    on_strict(out, rw(), &["config", "--unset", "ec.two", "["]);

    // ---- 255: a section name that cannot be written ----
    // `--rename-section` validates the *destination* before it touches the file,
    // and answers 255 rather than the 128 `--remove-section` gives for a section
    // that is merely absent. `config_cmd.rs` names this code in its comment and
    // reaches it only through `--system p.k v`, which is a lock failure; this is
    // the name-validation path.
    on_strict(out, rw(), &["config", "--rename-section", "ec", "in valid"]);

    // ---- 128: a conversion that cannot be done ----
    on_strict(out, rw(), &["config", "--type=int", "--get", "ec.two"]);

    // ---- 129: argument-list arity, which is a different failure from all of
    // the above and prints a usage block rather than a sentence ----
    out.push(Case::new("config", &["config"], Shape::Linear));
    out.push(Case::new("config", &["config", "--get"], Shape::Linear));
    out.push(Case::new("config", &["config", "--get", "a.b", "c", "d", "e"], Shape::Linear));
    // Two file selectors at once, which is a sentence and is therefore strict.
    on_strict(out, rw(), &["config", "--system", "--local", "--get", "ec.one"]);
    on_strict(out, rw(), &["config", "--file", ".git/config", "--worktree", "--get", "ec.one"]);
}
