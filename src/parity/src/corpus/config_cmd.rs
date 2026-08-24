//! Differential corpus cases for the `git config` **command** — the reader and
//! the writer — including its 2.46 subcommand spellings `get`, `set`, `unset`,
//! `list`, `rename-section`, `remove-section` and `edit`.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # How this differs from the config *premise* dimension
//!
//! [`crate::runner::ConfigScope`] exists so any case in the harness can be given
//! configuration — `-c`, a scope file, `GIT_CONFIG_KEY_<n>` — and `crate::fuzz`
//! samples that dimension across every subcommand. That measures whether `log`,
//! `status` and `diff` *honour* a setting: the consumer is some other command
//! reading an already-parsed value out of the config cache, and the code that
//! produced the value is never the thing under test.
//!
//! This module points the same delivery machinery at `builtin/config.c` itself.
//! The premise is still a file (or a `-c`, or an environment pair); the
//! *subject* is the reporting and rewriting of it — which value wins, which
//! scope it is labelled with, what a `--type` conversion prints, what
//! `.git/config` looks like afterwards, and which documented exit code comes
//! back. A port can pass every fuzzed premise case, because its config cache is
//! right, and still frame `--get-regexp -z` wrongly, write `two = a#b` without
//! quotes, or answer `--show-scope` with `local` for a worktree value.
//!
//! # What the state probe adds here
//!
//! `runner::probe_state` ends with `config --list --local`, run by **stock** git
//! against whatever the case left behind, and compares content *and order*.
//! Every writer case below is therefore pinned even though its stdout is empty:
//! a `set` that opens a second `[demo]` stanza instead of using the existing
//! one, that drops the quotes off `" leading"`, or that reorders a multi-valued
//! key, diverges on state with both sides printing nothing.
//!
//! # Fixture constraints these cases work around
//!
//! * **A case cannot create a file**, so every config file read here is either
//!   delivered by [`ConfigEntry::raw`] into a scope (`runner::install_config`
//!   writes it) or one the shape already has — `.gitmodules` on
//!   [`Shape::Submodule`], `.git/config` everywhere.
//! * **`--show-origin` prints an absolute path for a file outside the worktree**:
//!   the `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` files the runner points at, and
//!   `.git/config` itself when the cwd is a *linked* worktree. The two sides run
//!   at different fixture roots, so those combinations are unmeasurable and are
//!   absent; `--show-scope` prints only the scope name and covers that ground.
//! * **`--type=path` expands a leading `~` to `$HOME`**, which is the side's own
//!   temporary directory, so every path-typed value here is relative.
//! * **The `<n>` in `bad config line <n> in file <path>`** depends on how many
//!   keys `git init` auto-detected on this platform. That is fine: both sides
//!   read the same file on the same machine, and nothing here is compared
//!   against a literal.
//! * **`--global`/`--system` writes** land where `probe_state` does not look, so
//!   those cases are pinned by stdout and exit code alone.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    readers(out);
    subcommand_spellings(out);
    types(out);
    writers(out);
    quoting(out);
    scopes(out);
    file_parsing(out);
    malformed(out);
    files_and_blobs(out);
    includes(out);
    urlmatch(out);
    layouts(out);
}

// ---------------------------------------------------------------------------
// Premises
// ---------------------------------------------------------------------------

/// The reader premise: a multi-valued key, a valueless key (boolean true), a key
/// set to the empty string, and a subsection whose name carries a dot and whose
/// spelling is mixed case.
///
/// Section and variable names fold to lower case and a subsection does not
/// (`config.c:get_base_var` lower-cases only outside the quotes), so one file
/// carries both halves and a single `--list` catches a port that folds
/// everything or nothing.
const DEMO: &str = "[demo]\n\
                    \tone = 1\n\
                    \ttwo = a\n\
                    \ttwo = b\n\
                    \tflag\n\
                    \tempty =\n\
                    [Demo \"Sub.Section\"]\n\
                    \tKey = V";

/// The writer premise: one single-valued key and one two-valued key, so `--add`,
/// `--replace-all`, `--unset`, `--unset-all` and the value-pattern forms each
/// have something to hit and something to leave alone.
const RW: &str = "[demo]\n\tone = 1\n\ttwo = a\n\ttwo = b";

/// One value per `--type` conversion, spelled the way that conversion's own
/// parser accepts it.
///
/// `rel` is relative on purpose (see the module doc on `--type=path`), `stamp`
/// is an absolute ISO timestamp rather than an `approxidate` phrase so
/// `--type=expiry-date` is a parse and not a clock read, and `rgb` is quoted
/// because an unquoted `#` starts a comment.
const TYPED: &str = "[t]\n\
                     \tyes = yes\n\
                     \tzero = 0\n\
                     \tone = 1\n\
                     \tempty =\n\
                     \tflag\n\
                     \tkilo = 3k\n\
                     \tbig = 4294967296\n\
                     \tnotnum = abc\n\
                     \trel = ./rel\n\
                     \tcolor = red bold\n\
                     \trgb = \"#ff0000\"\n\
                     \tstamp = 2005-04-07T22:13:13";

/// A `[http]` stanza with a bare section, a host subsection and a
/// host-plus-path subsection — the three specificities `urlmatch.c` ranks
/// against each other, spread over two different keys so one URL has to take
/// them from two different stanzas.
const URLS: &str = "[http]\n\
                    \tsslVerify = true\n\
                    \tcookieFile = ./jar\n\
                    [http \"https://example.com\"]\n\
                    \tsslVerify = false\n\
                    [http \"https://example.com/path\"]\n\
                    \tcookieFile = ./p";

/// Two values differing only by a regex metacharacter, so `--fixed-value`
/// changes *which* value a pattern selects rather than only whether it matches.
const FIXED: &str = "[demo]\n\tv = a.b\n\tv = a+b";

// ---------------------------------------------------------------------------
// Case builders
// ---------------------------------------------------------------------------

/// A `config` invocation on [`Shape::Linear`] over a repository file holding
/// `premise` verbatim.
fn on(out: &mut Vec<Case>, premise: &str, args: &[&str]) {
    out.push(
        Case::new("config", args, Shape::Linear)
            .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Repo, premise)]),
    );
}

/// The same with stderr compared byte for byte, for the refusals — where the
/// exit code and the message are both the documented interface.
fn on_strict(out: &mut Vec<Case>, premise: &str, args: &[&str]) {
    out.push(
        Case::strict("config", args, Shape::Linear)
            .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Repo, premise)]),
    );
}

/// A `config` invocation on [`Shape::Linear`] with no premise beyond what
/// `git init` wrote.
fn bare(out: &mut Vec<Case>, args: &[&str]) {
    out.push(Case::new("config", args, Shape::Linear));
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// The reporting paths, over one file containing each awkward shape a value can
/// have.
///
/// What a port gets wrong without these: `--get` on a multi-valued key returns
/// the **last** value rather than the first or an error; a valueless key and a
/// key set to the empty string both print as an empty line here and are told
/// apart only by `--type=bool` (see [`types`]); `--get-regexp` prints
/// `name<SP>value` and prints the name *alone* for a valueless key; `-z` frames
/// as `name\nvalue\0`, a different arrangement from `--list`'s `name=value` and
/// not merely a different terminator.
fn readers(out: &mut Vec<Case>) {
    on(out, DEMO, &["config", "--get", "demo.two"]);
    on(out, DEMO, &["config", "--get-all", "demo.two"]);
    on(out, DEMO, &["config", "--get", "demo.flag"]);
    on(out, DEMO, &["config", "--get", "demo.empty"]);
    on(out, DEMO, &["config", "--get-regexp", "^demo\\."]);
    on(out, DEMO, &["config", "-l", "-z"]);
    on(out, DEMO, &["config", "--name-only", "--list"]);
    on(out, DEMO, &["config", "--show-origin", "--show-scope", "--list"]);
    // Folding from the query side: the section and the variable fold, the
    // subsection between them is byte-compared.
    on(out, DEMO, &["config", "--get", "DEMO.ONE"]);

    // ---- refusals ----
    // A miss is exit 1 with both streams empty — not a message.
    on_strict(out, DEMO, &["config", "--get", "demo.missing"]);
    // Two readers that disagree about the same file cannot be combined.
    on_strict(out, DEMO, &["config", "--get-all", "--get", "demo.one"]);
}

// ---------------------------------------------------------------------------
// The 2.46 subcommand spellings
// ---------------------------------------------------------------------------

/// `git config get|set|unset|list|rename-section|remove-section|edit`.
///
/// Not aliases of the old options. `get --regexp` takes the *name* as a pattern
/// and still prints one value without `--all`, where `--get-regexp` printed
/// every match; `get --value=<pattern>` is the old second positional promoted to
/// an option; `set --append` is `--add`; and `--show-names` has no
/// old-interface spelling at all. A port that implements the subcommands by
/// rewriting them into the old options gets the `--regexp` and `--show-names`
/// rows wrong.
fn subcommand_spellings(out: &mut Vec<Case>) {
    on(out, DEMO, &["config", "get", "--all", "--show-names", "demo.two"]);
    on(out, DEMO, &["config", "get", "--regexp", "^demo\\."]);
    on(out, DEMO, &["config", "get", "--regexp", "--all", "--show-names", "^demo\\."]);
    on(out, DEMO, &["config", "get", "--default=zz", "demo.missing"]);

    on(out, RW, &["config", "set", "demo.three", "3"]);
    on(out, RW, &["config", "set", "--append", "demo.two", "c"]);
    // A comment becomes a trailing `#` on the line it was written with.
    on(out, RW, &["config", "set", "--comment", "why", "demo.three", "3"]);
    on(out, RW, &["config", "unset", "--value=a", "demo.two"]);
    on(out, RW, &["config", "remove-section", "demo"]);
    // `GIT_EDITOR=true` accepts the file unchanged, so this measures the
    // copy-out/copy-back path alone: a port that rewrites the file through its
    // own serializer leaves different bytes and the state probe says so.
    on(out, RW, &["config", "edit"]);

    // ---- refusals ----
    // The comment becomes a `#` line, so a newline in it would leave the second
    // half of the message as configuration.
    on_strict(out, RW, &["config", "set", "--comment", "bad\nnewline", "demo.k", "v"]);
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The `--type` conversions, in the modern spelling and the legacy
/// one-flag-per-type spelling, against values chosen to separate them.
///
/// `git_config_bool` reads `yes`/`on`/`1`/**an absent value** as true and
/// `no`/`off`/`0`/**the empty string** as false — the one place a valueless key
/// and `key =` finally differ. `git_parse_int` applies the `k`/`m`/`g` factors
/// and stores into an `intmax_t`, so `4294967296` is not a truncation.
/// `bool-or-int` tries bool first and falls back to int, so `1` prints `1` while
/// `yes` prints `true`. `--type=color` renders through `color_parse`, which
/// emits `1;31` for `red bold` and a 24-bit sequence for `#ff0000`. A type also
/// applies on the way *in*: writing `3k` through `--type=int` stores `3072`,
/// which only the state probe can see.
fn types(out: &mut Vec<Case>) {
    on(out, TYPED, &["config", "--type=bool", "--get", "t.yes"]);
    on(out, TYPED, &["config", "--type=int", "--get", "t.kilo"]);
    on(out, TYPED, &["config", "--type=int", "--get", "t.big"]);
    on(out, TYPED, &["config", "--type=bool-or-int", "--get", "t.one"]);
    on(out, TYPED, &["config", "--type=path", "--get", "t.rel"]);
    on(out, TYPED, &["config", "--type=expiry-date", "--get", "t.stamp"]);
    on(out, TYPED, &["config", "--type=color", "--get", "t.color"]);
    on(out, TYPED, &["config", "--type=color", "--get", "t.rgb"]);
    // The legacy spellings reach the same conversions through a different option
    // table entry; a port that implements only `--type=` fails here.
    on(out, TYPED, &["config", "--bool", "--get", "t.yes"]);
    // The default is fed through the conversion a stored value would have taken.
    on(out, TYPED, &["config", "--type=bool", "--default=yes", "--get", "t.missing"]);
    // `--get-color` is a separate reader from `--type=color`: it falls back to
    // its second argument for an unset key and emits no trailing newline.
    on(out, TYPED, &["config", "--get-color", "nosuch.color", "blue"]);
    // Conversion on write.
    on(out, RW, &["config", "--type=int", "demo.n", "3k"]);

    // ---- refusals ----
    on_strict(out, TYPED, &["config", "--type=bool", "--get", "t.notnum"]);
    on_strict(out, TYPED, &["config", "--type=bogus", "--get", "t.one"]);
    // A rejected conversion on a write must leave the file untouched.
    on_strict(out, RW, &["config", "--type=int", "demo.n", "notanint"]);
}

// ---------------------------------------------------------------------------
// Writers
// ---------------------------------------------------------------------------

/// The mutating verbs, pinned by the `.git/config` the state probe reads back
/// rather than by stdout, which is empty for all of them.
///
/// `config.c:git_config_set_multivar_in_file_gently` rewrites the file in place
/// by byte offset: a new key goes *inside* the existing section when there is
/// one, `--add` appends after the last existing value rather than at the end of
/// the file, `--replace-all` collapses the run to one line at the position of
/// the first, and `--remove-section` takes the header with it while leaving the
/// rest of the file byte-identical. A port that regenerates the file from its
/// parsed config map gets all of that wrong while printing the same nothing.
fn writers(out: &mut Vec<Case>) {
    on(out, RW, &["config", "demo.three", "3"]);
    on(out, RW, &["config", "--add", "demo.two", "c"]);
    on(out, RW, &["config", "--replace-all", "demo.two", "z"]);
    on(out, RW, &["config", "--unset", "demo.one"]);
    on(out, RW, &["config", "--unset-all", "demo.two"]);
    on(out, RW, &["config", "--rename-section", "demo", "other"]);
    on(out, RW, &["config", "--remove-section", "demo"]);
    // A key whose written form is a header the writer has to synthesize.
    on(out, RW, &["config", "remote.odd name.url", "./x"]);
    // `--fixed-value` selects a different value, not merely a different verdict:
    // `a.b` as a regex also matches `a+b`.
    on(out, FIXED, &["config", "--get", "demo.v", "a.b"]);
    on(out, FIXED, &["config", "--fixed-value", "--get", "demo.v", "a.b"]);

    // ---- refusals ----
    // The documented exit codes, each on the path that produces it: multiple
    // lines match (5, with a warning), nothing to unset (5), no such section
    // (128), an unusable section name (255), a key with no section (2), a key
    // whose variable name is not an identifier (1), an unusable regexp (6), and
    // a config file that cannot be locked (255) — the last reached without a
    // chmod a case could not perform, because `env::harden` pins the system
    // scope at `/dev/null`.
    on_strict(out, RW, &["config", "--unset", "demo.two"]);
    on_strict(out, RW, &["config", "--remove-section", "nosuch"]);
    on_strict(out, RW, &["config", "invalidkey", "v"]);
    on_strict(out, FIXED, &["config", "--get", "demo.v", "["]);
    on_strict(out, RW, &["config", "--system", "p.k", "v"]);
}

// ---------------------------------------------------------------------------
// Quoting on write
// ---------------------------------------------------------------------------

/// Values whose stored spelling is not the value.
///
/// `config.c:store_write_pair` quotes the whole value when it begins or ends
/// with a space or contains `#` or `;`, and escapes `"`, `\`, a newline and a
/// tab *individually* whether or not the value ends up quoted. The two rules are
/// independent, so `a"b` is stored unquoted with a backslash while `a#b` is
/// stored quoted without one. Nothing here prints anything: the state probe is
/// the whole measurement, and a port that quotes uniformly — or that writes a
/// tab as a literal tab — leaves a file stock git reads back as a different
/// value.
fn quoting(out: &mut Vec<Case>) {
    for (key, value) in [
        ("w.lead", " leading"),
        ("w.hash", "a#b"),
        ("w.quote", "a\"b"),
        ("w.back", "a\\b"),
        ("w.newline", "a\nb"),
        ("w.tab", "a\tb"),
    ] {
        bare(out, &["config", key, value]);
    }
    // A subsection header is always quoted, and only `"` and `\` are escaped
    // inside it — a different rule from the one above.
    bare(out, &["config", "remote.odd \"name\".url", "./x"]);
}

// ---------------------------------------------------------------------------
// Scopes and precedence
// ---------------------------------------------------------------------------

/// One key set in several scopes at once, read with and without a scope
/// restriction.
///
/// The axis `-c` alone cannot reach. `--get` answers with the
/// highest-precedence value; `--get-all` prints every one **lowest precedence
/// first**; `--show-scope` names the file each came from; and a scope selector
/// reads that file *alone* rather than filtering the merged view. The
/// environment scope and `-c` are both labelled `command`, and the environment
/// is read first, so with both present the `-c` value wins and the two are
/// distinguishable only by order. A port that stores one value per key passes
/// `--get` and fails every other row here.
fn scopes(out: &mut Vec<Case>) {
    let layered = || {
        vec![
            ConfigEntry::set(ConfigScope::System, "p.k", "system"),
            ConfigEntry::set(ConfigScope::Global, "p.k", "global"),
            ConfigEntry::set(ConfigScope::Repo, "p.k", "repo"),
        ]
    };
    for args in [
        &["config", "--get", "p.k"][..],
        &["config", "--show-scope", "--get-all", "p.k"][..],
        &["config", "--local", "--get", "p.k"][..],
    ] {
        out.push(Case::new("config", args, Shape::Linear).with_scoped_config(layered()));
    }
    out.push(
        Case::new("config", &["config", "--show-scope", "--get-all", "p.k"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "p.k", "repo"),
                ConfigEntry::set(ConfigScope::Env, "p.k", "envv"),
                ConfigEntry::set(ConfigScope::CommandLine, "p.k", "cmdline"),
            ]),
    );
    // The same pairs written by hand, which is the spelling a user's shell
    // produces and the one that has to survive `--list`.
    out.push(
        Case::new("config", &["config", "--list", "--show-scope"], Shape::Linear).with_env(&[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "e.one"),
            ("GIT_CONFIG_VALUE_0", "1"),
            ("GIT_CONFIG_KEY_1", "e.two"),
            ("GIT_CONFIG_VALUE_1", "2"),
        ]),
    );
    out.push(
        Case::new("config", &["config", "--show-scope", "--get", "c.k"], Shape::Linear)
            .with_config(&[("c.k", "cmdline")]),
    );

    // `.git/config.worktree` is inert until `extensions.worktreeConfig` is set,
    // so drawing this scope writes two files. The worktree value then outranks
    // the local one and `--local` still reads past it. `--show-origin` is usable
    // here because both files are inside the worktree and print relative.
    let wt = || {
        vec![
            ConfigEntry::set(ConfigScope::Repo, "wt.k", "fromlocal"),
            ConfigEntry::set(ConfigScope::Worktree, "wt.k", "fromworktree"),
        ]
    };
    for args in [
        &["config", "--get", "wt.k"][..],
        &["config", "--show-scope", "--get-all", "wt.k"][..],
        &["config", "--show-origin", "--get-all", "wt.k"][..],
        // Written to `.git/config.worktree`, which `--list --local` does *not*
        // show — so a port that writes it into `.git/config` instead is caught
        // by the probe printing one key too many.
        &["config", "--worktree", "wt.new", "v"][..],
    ] {
        out.push(Case::new("config", args, Shape::Linear).with_scoped_config(wt()));
    }
}

// ---------------------------------------------------------------------------
// File parsing corners
// ---------------------------------------------------------------------------

/// Legal file content no `-c` and no `GIT_CONFIG_KEY_<n>` can express, read back
/// through the reader that shows what the parser produced.
///
/// `-c key=value` hands the parser an already-split pair. A file has section
/// headers, continuation, comments, quoting and escapes, and
/// `config.c:parse_value` resolves all of them before any consumer sees a
/// value. The asymmetry that matters most is the first two rows: a key with no
/// `=` is boolean **true** while `key =` is the empty string, and `--get` prints
/// an empty line for both.
fn file_parsing(out: &mut Vec<Case>) {
    on(out, "[core]\n\tabbrev", &["config", "--type=bool", "--get", "core.abbrev"]);
    on(out, "[core]\n\tabbrev =", &["config", "--type=bool", "--get", "core.abbrev"]);
    // A trailing comment is not part of the value.
    on(out, "[demo]\n\tv = 4 # comment", &["config", "--get", "demo.v"]);
    // A backslash at end of line joins the next line into one value.
    on(out, "[demo]\n\tv = 4\\\n5", &["config", "--get", "demo.v"]);
    // Section and key on one line.
    on(out, "[demo] v = 4", &["config", "--get", "demo.v"]);
    // Folding from the file side rather than the query side.
    on(out, "[DEMO]\n\tV = 4", &["config", "--list", "--local"]);
    // Escapes inside a quoted value, and a quote that opens mid-value.
    on(out, "[demo]\n\tv = \"a\\tb\"", &["config", "--get", "demo.v"]);
    // Two stanzas of one section in one file: last value wins and both survive
    // for `--get-all`; a write has to pick one of the two places to land in.
    on(out, "[demo]\n\tv = 4\n[demo]\n\tv = 5", &["config", "--get-all", "demo.v"]);
    on(out, "[demo]\n\tv = 4\n[demo]\n\tw = 5", &["config", "demo.x", "9"]);
}

// ---------------------------------------------------------------------------
// Malformed files
// ---------------------------------------------------------------------------

/// The refusals that name a *line* — the one diagnostic a command line cannot
/// produce, because only a file has line numbers.
///
/// All strict. `fatal: bad config line <n> in file <path>` is the whole answer
/// (these abort before any output), and the number is the part a port most
/// easily gets wrong: git reports the line the offending token started on, which
/// a continuation makes differ from the line the parser noticed it on.
fn malformed(out: &mut Vec<Case>) {
    for line in [
        "garbage line",
        "[core",
        "x = \"unterminated",
        // A bad escape is only bad once the line is read as a value, so this
        // form carries its own section header.
        "[core]\n\tabbrev = \"bad\\qescape\"",
    ] {
        on_strict(out, line, &["config", "--get", "demo.one"]);
    }
    // The same file reached by a *writer*: the refusal has to come before the
    // rewrite, and the file has to survive it unchanged.
    on_strict(out, "garbage line", &["config", "demo.k", "v"]);
}

// ---------------------------------------------------------------------------
// --file and --blob
// ---------------------------------------------------------------------------

/// Reading a config file that is not one of the layered scopes.
///
/// `--file` replaces the whole sequence with one file, so `git init`'s `core.*`
/// keys are visible only when that file *is* `.git/config`, and `--show-scope`
/// then reports the whole thing as `command`. `--blob` does the same out of the
/// object store and cannot be written to at all. Both echo their origin as the
/// string they were given, so every path here is repository-relative and the two
/// sides agree.
///
/// `.gitmodules` is the one file that is both a config file and a tracked blob,
/// which is what makes `--file` and `--blob` comparable on identical bytes. Only
/// the *names* are read from it: the `url` the fixture wrote is an absolute path
/// into that side's own copy.
fn files_and_blobs(out: &mut Vec<Case>) {
    on(out, DEMO, &["config", "--file", ".git/config", "--show-origin", "--get", "demo.one"]);
    // A file that exists and is not configuration: `# fixture` is a comment, so
    // it parses to nothing rather than failing.
    bare(out, &["config", "--file", "README.md", "--list"]);
    // Writing through `--file` creates the file, which the status probe sees.
    bare(out, &["config", "--file", "written.config", "demo.k", "v"]);

    out.push(Case::new(
        "config",
        &["config", "--file", ".gitmodules", "--name-only", "--list"],
        Shape::Submodule,
    ));
    out.push(Case::new(
        "config",
        &["config", "--blob", "HEAD:.gitmodules", "--name-only", "--list"],
        Shape::Submodule,
    ));
    // The same key through the normal sequence, which does not read
    // `.gitmodules` at all: `submodule-config.c` does, `config.c` does not.
    out.push(Case::new("config", &["config", "--get", "submodule.sub.path"], Shape::Submodule));

    // ---- refusals ----
    on_strict(out, DEMO, &["config", "--file", "no/such/file", "--list"]);
    // A tracked blob that is not parseable configuration, and a write through
    // `--blob`, which is unsupported outright.
    out.push(Case::strict("config", &["config", "--blob", "HEAD:src/lib.rs", "--list"], Shape::Linear));
    out.push(Case::strict("config", &["config", "--blob", "HEAD:README.md", "k", "v"], Shape::Linear));
    on_strict(out, DEMO, &["config", "--file", ".git/config", "--local", "--list"]);
}

// ---------------------------------------------------------------------------
// Includes
// ---------------------------------------------------------------------------

/// `[include]` and `[includeIf]`, delivered as a repository file pointing at a
/// second file the same case writes.
///
/// Two facts a port needs separately: an included key joins the scope of the
/// *including* file, so `--show-scope` says `local` while `--show-origin` names
/// the included file; and `--list --local` does **not** expand includes, so the
/// same repository answers `include.path` there and `inc.k` through the full
/// sequence. `--no-includes` turns expansion off for the sequence too, and a
/// write always lands in the including file rather than the included one.
///
/// `.gitmodules` is the include target because it is the only non-`.git` file a
/// case can write through a scope; the path is relative to `.git/config`'s own
/// directory, which is how `config.c:handle_path_include` resolves it. The
/// `gitdir:` condition is the root-independent glob — the two sides live at
/// different paths, so a condition naming a directory could not be written here.
fn includes(out: &mut Vec<Case>) {
    let inc = |cond: &str| {
        vec![
            ConfigEntry::raw(ConfigScope::Modules, "[inc]\n\tk = from-gitmodules"),
            ConfigEntry::raw(ConfigScope::Repo, &format!("[{cond}]\n\tpath = ../.gitmodules")),
        ]
    };
    for args in [
        &["config", "--get", "inc.k"][..],
        &["config", "--list", "--local"][..],
        &["config", "--show-origin", "--get", "inc.k"][..],
        &["config", "--no-includes", "--get", "inc.k"][..],
        &["config", "inc.k", "written"][..],
    ] {
        out.push(Case::new("config", args, Shape::Linear).with_scoped_config(inc("include")));
    }
    for cond in [
        "includeIf \"gitdir:**\"",
        "includeIf \"onbranch:main\"",
    ] {
        out.push(
            Case::new("config", &["config", "--get", "inc.k"], Shape::Linear)
                .with_scoped_config(inc(cond)),
        );
    }
}

// ---------------------------------------------------------------------------
// URL matching
// ---------------------------------------------------------------------------

/// `--get-urlmatch` and its subcommand spelling `get --url=`.
///
/// `urlmatch.c` ranks candidate subsections by how much of the URL each matches
/// — host beats bare section, host-plus-path beats host — and does it **per
/// key**, so one URL takes `sslVerify` from the host stanza and `cookieFile`
/// from the path stanza in a single answer. Asking for a whole section prints
/// `key value` rows with the key lower-cased; asking for one key prints the
/// value alone. A port that picks the single most specific *stanza* and reads
/// every key out of it produces the right answer for the one-key form and the
/// wrong one for the section form.
fn urlmatch(out: &mut Vec<Case>) {
    on(out, URLS, &["config", "--get-urlmatch", "http", "https://example.com"]);
    on(out, URLS, &["config", "--get-urlmatch", "http", "https://example.com/path/x"]);
    on(out, URLS, &["config", "--get-urlmatch", "http.sslVerify", "https://other.example"]);
    on(out, URLS, &["config", "get", "--url=https://example.com", "http.sslVerify"]);

    // ---- refusals ----
    on_strict(out, URLS, &["config", "--get-urlmatch", "http", "not-a-url"]);
}

// ---------------------------------------------------------------------------
// Repository layouts
// ---------------------------------------------------------------------------

/// The same reads from somewhere other than the top of a normal worktree.
///
/// A subdirectory must not change the origin string: `.git/config` stays
/// relative to the *worktree* root and not to the cwd. A bare repository has no
/// worktree and its config still resolves — and it is reached here without a
/// premise, because a scope file would be installed into the *outer* repository
/// rather than into this one. A **linked** worktree reads the common
/// `.git/config` and its own `config.worktree`, never the main worktree's, which
/// is the row that catches a port resolving the worktree config off the common
/// directory.
fn layouts(out: &mut Vec<Case>) {
    out.push(
        Case::new("config", &["config", "--show-origin", "--get", "demo.one"], Shape::Linear)
            .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Repo, DEMO)])
            .in_dir("src"),
    );
    // Writing from a subdirectory: the file rewritten is the repository's, and
    // the probe reads it back from the root.
    out.push(
        Case::new("config", &["config", "demo.k", "v"], Shape::Linear)
            .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Repo, RW)])
            .in_dir("src"),
    );

    // The bare repository `BehindRemote` keeps as its remote.
    out.push(Case::new("config", &["config", "--list", "--local"], Shape::BehindRemote)
        .in_dir(".remote.git"));

    // A linked worktree: the main worktree's `config.worktree` must not apply
    // here, so `wt.k` resolves to the local value and `--show-scope` says so.
    let wt = || {
        vec![
            ConfigEntry::set(ConfigScope::Repo, "wt.k", "fromlocal"),
            ConfigEntry::set(ConfigScope::Worktree, "wt.k", "fromworktree"),
        ]
    };
    for args in [
        &["config", "--get", "wt.k"][..],
        &["config", "--show-scope", "--get-all", "wt.k"][..],
    ] {
        out.push(Case::new("config", args, Shape::Worktree).with_scoped_config(wt()).in_dir("wt"));
    }
    // `--worktree` with more than one working tree and the extension off is a
    // refusal whose message names no path — so it is comparable where the
    // enabled case's message, which prints an absolute one, would not be.
    out.push(Case::strict("config", &["config", "--worktree", "--list"], Shape::Worktree).in_dir("wt"));
}
