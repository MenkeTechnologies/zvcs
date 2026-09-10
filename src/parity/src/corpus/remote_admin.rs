//! `git remote` as an **administrative** verb: the configuration it writes and
//! reads back, as opposed to the transport it configures.
//!
//! # What is measurable here, and what is not
//!
//! `corpus/clone_options.rs` records that no probe opens a *clone's* config
//! file, and that is true — but it is a statement about a repository a case
//! *produced*, not about the repository a case *ran in*. `runner::probe_state`
//! runs `config --list --local` inside the fixture itself, in file order, and
//! compares the result byte for byte. Almost every `remote` subcommand's whole
//! effect is a write to that file, so the verdict this subsystem needs is
//! already there:
//!
//! | surface | probe that reads it | reached by |
//! |---|---|---|
//! | `.git/config` content **and order** | `config --list --local` | `add`, `rename`, `remove`, `set-url`, `set-branches` |
//! | `refs/remotes/<n>/*`, incl. `HEAD` | `for-each-ref` | `set-head`, `prune`, `update`, `rename`, `remove` |
//! | the peer's own refs and objects | `probe_peer` on `.remote.git` | `update`, `prune` (which must leave the peer alone) |
//! | stdout / exit code | the comparison itself | `get-url`, `show`, `-v`, every usage error |
//! | stderr | only on [`Case::strict`] | the refusals below that opt in |
//!
//! **What is genuinely invisible.** Four things, stated rather than left to be
//! found:
//!
//!  * **The bytes of `.git/config`.** `config --list --local` prints
//!    `section.key=value` in file order; it does not print section headers,
//!    blank lines, comments or indentation. A port that writes
//!    `[remote "up"]\n\turl = x` where stock writes `[remote "up"]\nurl = x`
//!    matches here. Closing that needs a probe that reads the file itself.
//!  * **Key case.** git lower-cases section and variable names on read, so
//!    `remote.up.tagOpt` and `remote.up.tagopt` are one line in the probe. The
//!    `rename`/`remove` cases below therefore pin *which keys survive*, never
//!    how they are spelled.
//!  * **`refs/remotes/<n>/HEAD` as a *detached* ref.** `for-each-ref` prints the
//!    object a symref resolves to and prints nothing at all for a dangling one,
//!    so "symbolic vs detached" is not a distinction any probe here can draw. A
//!    case cannot write a raw ref either — the only premise a case carries is
//!    argv, environment and config. It stays unmeasured.
//!  * **A remote that is slow, or that hangs.** Every URL below answers or fails
//!    immediately. `show` against an unreachable *host* is out of scope by
//!    construction: nothing in this crate is allowed to resolve a name.
//!
//! # How the territory is divided
//!
//! Read before writing a line of this file, and what each already owns:
//!
//!  * **`branch_remote.rs`** — the largest existing `remote` holding, and the
//!    baseline this file is a delta against: `add` with `-t`/`-m`/`-f`/`--tags`/
//!    `--no-tags`/`--mirror=fetch|push`, `rename`/`remove` in their plain form,
//!    `set-url` with `--push`/`--add`/`--delete`, `set-branches` with and
//!    without `--add`, `get-url` in its three spellings, `show`/`show -n`, the
//!    bare listing, and `prune`/`update` driven by a `-c` refspec that matches
//!    nothing. It also owns `push` and `branch` outright. **Nothing in this file
//!    repeats one of those argvs on one of those shapes.**
//!  * **`refspec_algebra.rs`** — the refspec *grammar*: what
//!    `+refs/heads/*:refs/remotes/x/*` means, and the six `remote` spellings
//!    that *synthesize* one (`add -t`, `--mirror=`, `set-branches`). This file
//!    owns the config that *stores* a refspec — what `rename` does to one it did
//!    not write, what `set-branches` with no arguments does to one that exists.
//!  * **`fetch_clone.rs` / `transport_local.rs` / `wire_protocol.rs`** — the
//!    transport a remote configures: `fetch`, `clone`, `ls-remote`, the
//!    pack protocol. `remote update` here is measured for *which remotes it
//!    selects* and *what it prunes*, never for how the fetch itself negotiates.
//!  * **`clone_options.rs`** — what `clone` writes into a *new* repository, and
//!    the measurability limit quoted at the top of this header.
//!  * **`config_resolution.rs`** — the scope ladder (system/global/repo/
//!    worktree/env/`-c`) as a subject in itself. Here `ConfigScope::Repo` is
//!    used only as a *premise*: `remote rename` rewrites the file, so a premise
//!    delivered by `-c` would be read and never rewritten, and the case would
//!    measure nothing.
//!  * **`plumbing_refs.rs`** — `update-ref`, `symbolic-ref`, `show-ref`. It owns
//!    symrefs as a mechanism; `set-head` here is measured through the ref it
//!    leaves behind.
//!  * **`misc_commands.rs`** — `remote <sub> -h` and `remote <sub> --zzbogus=x`
//!    for all ten subcommands, plus `show --no-query` and `add -th`, all on
//!    `Shape::Linear`. It owns *subcommand* help; the only `-h` here is the
//!    bare verb's, which it does not have.
//!  * **`exit_codes.rs`**, **`object_format.rs`**, **`fixture_gaps2.rs`**,
//!    **`sequences.rs`** — one or two `remote` cases each (three `nosuch` exit
//!    codes on `Shape::Linear`, `show -n origin` under the SHA-256 sweep,
//!    `remote -v`/`show origin` on `Shape::Shallow`, and one add/list/remove
//!    sequence). All checked; none repeated.
//!
//! # What this file adds that none of them has
//!
//!  1. **`set-head -a` on its *success* path.** `branch_remote.rs` states that
//!     it is unreachable, and on `Shape::BehindRemote` it is: `fixture.rs:1471`
//!     builds that peer with `init --bare .remote.git` and no `-b`, so its
//!     `HEAD` is a dangling `ref: refs/heads/master`. `Shape::HooksFail` builds
//!     its peer with `init --bare -b main` (`fixture.rs:2065`) and pushes `main`
//!     into it, so the advertisement carries a resolvable `HEAD` and every
//!     `set-head` mode is reachable — including `-a`, `--auto`, `--delete` and
//!     an explicit branch, none of which had a success case anywhere.
//!  2. **The keys `rename` and `remove` rewrite *besides* `remote.<n>.*`.**
//!     `branch.<n>.pushRemote` and `remote.pushDefault` are rewritten by
//!     `rename` and deleted by `remove` — but only when they name the remote in
//!     question, which is why each appears twice below, once naming it and once
//!     naming another. No case anywhere set either key.
//!  3. **A remote whose fetch refspec `rename` refuses to touch.** Two shapes of
//!     it: sole refspec, and a second refspec beside the default one. Both print
//!     a three-line warning on stderr and leave the spec alone.
//!  4. **URL *lists*.** `remote.<n>.url` and `.pushurl` are multi-valued, and
//!     every existing case has exactly one of each. With two of each,
//!     `get-url`/`get-url --all`/`--push`/`--push --all` give four different
//!     answers, `remote -v` grows a third line, and `set-url`'s three-argument
//!     form, its `--delete`, and its refusal to leave the list empty all become
//!     reachable.
//!  5. **A remote with no `url` at all**, and one with **only a `pushurl`** —
//!     both legal config, both of which make `remote -v` print a line with an
//!     empty field and make `get-url` answer with the remote's *name*.
//!  6. **The five `remote.*` keys nothing set**: `.prune`,
//!     `.skipDefaultUpdate`, `.tagOpt` as a premise rather than as a thing `add`
//!     writes, `remotes.<group>`, and `fetch.pruneTags`.
//!  7. **The usage surface.** Every subcommand's arity error, `remote
//!     <nosuchsub>`, `remote -h`, and the two `add` flag conflicts — a family
//!     that exits 129 with a usage block and that no case reached.
//!
//! # Determinism
//!
//! Every URL is relative (`./.remote.git`, `./one.git`, `./push1.git`) or
//! absent. Nothing resolves a hostname, and no output below carries an absolute path, so no case
//! here depends on `runner::normalize`'s `<REPO>` masking — verified by hand
//! against stock 2.55.0 for every case, in a scratch replica of each shape.
//!
//! `Case::strict` is used where the refusal *is* the behaviour and the message
//! comes out of `builtin/remote.c` itself. It is deliberately **not** used for
//! the messages that come out of the transport layer (`does not appear to be a
//! git repository` and the four lines after it), which are prose about a
//! connection rather than about a remote.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    name_validation(out);
    add_stanza_edges(out);
    usage_and_arity(out);
    rename_rewrites(out);
    remove_rewrites(out);
    url_lists(out);
    listing_edges(out);
    set_head_live_peer(out);
    set_branches_edges(out);
    prune_update_config(out);
}

/// A remote defined entirely in `.git/config`, as a case premise.
///
/// `ConfigScope::Repo` rather than `-c`, and that is load-bearing rather than
/// stylistic: `rename`, `remove`, `set-url` and `set-branches` *rewrite the
/// file*. A premise delivered on the command line is read at a higher precedence
/// and is not in any file, so the rewrite would find nothing to move and the
/// case would measure the absence of its own premise.
fn repo(pairs: &[(&str, &str)]) -> Vec<ConfigEntry> {
    pairs
        .iter()
        .map(|(k, v)| ConfigEntry::set(ConfigScope::Repo, *k, *v))
        .collect()
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// What is and is not a legal remote name.
///
/// The rule is a *ref-name* rule rather than an identifier rule, which is what
/// makes two of these four surprising. Measured against stock 2.55.0 rather
/// than read out of the source:
///
/// ```text
/// remote add 'bad name' …   -> fatal: 'bad name' is not a valid remote name (128)
/// remote add '' …           -> fatal: '' is not a valid remote name          (128)
/// remote add up/stream …    -> accepted; writes remote.up/stream.fetch
///                              = +refs/heads/*:refs/remotes/up/stream/*      (0)
/// remote add -- -weird …    -> accepted; writes remote.-weird.*              (0)
/// ```
///
/// The last two are the ones worth having: a name with a slash in it produces a
/// *two-level* tracking namespace, and a name beginning with a dash is legal
/// once `--` has ended option parsing — both of which a port that validates
/// names with its own idea of "identifier" rejects while printing nothing wrong.
fn name_validation(out: &mut Vec<Case>) {
    out.push(Case::strict("remote", &["remote", "add", "bad name", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "add", "", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "add", "up/stream", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "add", "--", "-weird", "./.remote.git"], Shape::BehindRemote));

    // The same rule, reached through `rename`'s *destination*. It is checked
    // before anything is moved, so the repository must be untouched — which the
    // post-state probe is what proves.
    out.push(Case::strict("remote", &["remote", "rename", "origin", "bad name"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "rename", "origin", ""], Shape::BehindRemote));
}

// ---------------------------------------------------------------------------
// add: the stanza edges
// ---------------------------------------------------------------------------

/// `add` flag combinations that write something other than the seven stanzas
/// `branch_remote.rs` already pins.
///
/// Measured against stock 2.55.0:
///
/// ```text
/// --mirror (bare)      -> warning on stderr, and *both* fetch = +refs/*:refs/*
///                         and mirror = true                              (0)
/// --mirror=fetch       -> fetch only          (owned by branch_remote.rs)
/// --mirror=push        -> mirror only         (owned by branch_remote.rs)
/// --mirror=bogus       -> error: unknown --mirror argument: bogus       (129)
/// --no-mirror          -> the plain stanza
/// --no-tags --tags     -> tagopt = --tags     (last one wins)
/// --mirror=push -t main-> fatal: specifying branches to track makes sense
///                         only with fetch mirrors                       (128)
/// --mirror=fetch -m div-> fatal: specifying a master branch makes no sense
///                         with --mirror                                 (128)
/// ```
///
/// The bare `--mirror` is the one that matters: it is the only spelling that
/// writes *two* keys, and a port that treats it as a synonym for either
/// `--mirror=fetch` or `--mirror=push` writes one of them and matches on stdout.
fn add_stanza_edges(out: &mut Vec<Case>) {
    // Deprecated and dangerous, so the warning is the behaviour: strict.
    out.push(Case::strict("remote", &["remote", "add", "--mirror", "up", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "add", "--mirror=bogus", "up", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "add", "--no-mirror", "up", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "add", "--no-tags", "--tags", "up", "./.remote.git"], Shape::BehindRemote));
    out.push(Case::strict(
        "remote",
        &["remote", "add", "--mirror=push", "-t", "main", "up", "./.remote.git"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "remote",
        &["remote", "add", "--mirror=fetch", "-m", "div", "up", "./.remote.git"],
        Shape::BehindRemote,
    ));

    // `-f` against a URL that is not a repository. The stanza is written
    // *first* and is **not** rolled back when the fetch fails, so the post-state
    // carries `remote.gone.url` and `remote.gone.fetch` beside exit 1. A port
    // that treats the fetch as part of the transaction diverges only here — its
    // stdout ("Updating gone") and its exit code both match.
    out.push(Case::new("remote", &["remote", "add", "-f", "gone", "./no-such-peer.git"], Shape::BehindRemote));

    // "Already exists" for a remote that has **no url** — only a fetch refspec.
    // Measured: exit 3, the same code a real duplicate gets, so the existence
    // test is not "has a url".
    out.push(
        Case::strict("remote", &["remote", "add", "half", "./.remote.git"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.half.fetch", "+refs/heads/*:refs/remotes/half/*")])),
    );
}

// ---------------------------------------------------------------------------
// The usage surface
// ---------------------------------------------------------------------------

/// Arity errors, unknown subcommands and the two option-parsing traps.
///
/// All exit **129** and print a `usage:` block on stderr — a family no case in
/// the corpus reached, even though it is the first thing a script hits when it
/// gets an argument wrong. `remote update --all` is here because `--all` looks
/// like it should exist (`fetch` has it) and does not: stock answers
/// ``error: unknown option `all'`` and the `remote update` usage block.
///
/// `add -t up <url>` and `add -m up <url>` are the option-parsing trap: `-t` and
/// `-m` each take an argument, so they swallow the *name* and leave one
/// positional, which is an arity error rather than a "remote up already exists".
fn usage_and_arity(out: &mut Vec<Case>) {
    let u = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::strict("remote", args, Shape::BehindRemote));
    };
    u(&["remote", "add", "up"], out);
    u(&["remote", "add", "up", "./x.git", "extra"], out);
    u(&["remote", "add", "-t", "up", "./.remote.git"], out);
    u(&["remote", "add", "-m", "up", "./.remote.git"], out);
    u(&["remote", "rename", "origin"], out);
    u(&["remote", "rename"], out);
    u(&["remote", "remove"], out);
    u(&["remote", "get-url"], out);
    u(&["remote", "set-url"], out);
    u(&["remote", "set-head"], out);
    u(&["remote", "set-head", "origin"], out);
    u(&["remote", "prune"], out);
    u(&["remote", "update", "--all"], out);
    u(&["remote", "nosuchsub"], out);
    u(&["remote", "-h"], out);
    // `-v` after the subcommand rather than before it: the usage block says
    // "must be placed before a subcommand", and this is what happens when it is
    // not.
    u(&["remote", "show", "-v", "origin"], out);
}

// ---------------------------------------------------------------------------
// rename
// ---------------------------------------------------------------------------

/// What `rename` rewrites beyond `remote.<old>.*`, and what it refuses to.
///
/// Four separate rewrites, measured on stock 2.55.0 against a
/// `Shape::BehindRemote` replica:
///
/// ```text
/// remote.<old>.*        -> remote.<new>.*        (branch_remote.rs owns this)
/// branch.<n>.remote     -> the new name          (branch_remote.rs owns this)
/// branch.<n>.pushRemote -> the new name          (nothing owned this)
/// remote.pushDefault    -> the new name          (nothing owned this)
/// refs/remotes/<old>/*  -> refs/remotes/<new>/*  (branch_remote.rs owns this)
/// ```
///
/// The two new ones are conditional — they move only when they *name* the remote
/// being renamed — so each is written twice, once naming `origin` and once
/// naming an unrelated remote that must be left exactly as it was.
///
/// The refspec is the other half, and the premise here is *additive*:
/// `runner::install_config` appends to `.git/config`, and the fixture already
/// carries `remote.origin.fetch = +refs/heads/*:refs/remotes/origin/*`. So each
/// case below renames a remote with **two** fetch refspecs, and what is measured
/// is which of the two `rename` is willing to rewrite. Measured on stock 2.55.0:
///
/// ```text
/// second spec = +refs/heads/*:refs/remotes/elsewhere/*
///   -> warning: Not updating non-default fetch refspec  (3 lines, stderr)
///      remote.up.fetch = +refs/heads/*:refs/remotes/up/*          (rewritten)
///      remote.up.fetch = +refs/heads/*:refs/remotes/elsewhere/*   (left alone)
/// second spec = +refs/heads/main:refs/remotes/origin/main
///   -> silent
///      remote.up.fetch = +refs/heads/*:refs/remotes/up/*          (rewritten)
///      remote.up.fetch = +refs/heads/main:refs/remotes/up/main    (rewritten)
/// ```
///
/// The test is on the *destination* side, not on the source side: a narrowed
/// spec whose destination is still under `refs/remotes/origin/` is a default
/// spec and moves; one aimed anywhere else does not. Only the first is strict,
/// because only the first prints anything. The config ends up holding two
/// `remote.up.fetch` values in a defined order, and order is what
/// `config --list --local` compares.
fn rename_rewrites(out: &mut Vec<Case>) {
    // pushRemote / pushDefault naming the remote being renamed: both move.
    out.push(
        Case::new("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote).with_scoped_config(
            repo(&[("branch.main.pushRemote", "origin"), ("remote.pushDefault", "origin")]),
        ),
    );
    // …naming a different remote: both survive untouched.
    out.push(
        Case::new("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote).with_scoped_config(
            repo(&[
                ("remote.other.url", "./other.git"),
                ("branch.main.pushRemote", "other"),
                ("remote.pushDefault", "other"),
            ]),
        ),
    );
    // A `pushurl` alongside: it moves with the rest of the stanza, and it moves
    // *into a different position* in the file than the fetch refspec does.
    out.push(
        Case::new("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.origin.pushurl", "./push.git")])),
    );

    // The refspec `rename` will not touch. Strict: the warning is the whole
    // finding, and a port that silently rewrote the spec would match on stdout
    // and on the exit code, and — because it rewrote it "correctly" — look
    // better while diverging.
    out.push(
        Case::strict("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.origin.fetch", "+refs/heads/*:refs/remotes/elsewhere/*")])),
    );
    // A narrowed-but-still-default spec *is* rewritten, with no warning, so both
    // of this remote's two refspecs move and neither stream says anything.
    out.push(
        Case::new("remote", &["remote", "rename", "origin", "up"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main")])),
    );

    // Renaming onto a name that is already taken, and onto its own name — the
    // same "already exists" refusal (exit 3) from two different premises.
    out.push(
        Case::strict("remote", &["remote", "rename", "origin", "taken"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.taken.url", "./taken.git")])),
    );
    out.push(Case::strict("remote", &["remote", "rename", "origin", "origin"], Shape::BehindRemote));
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

/// `remove` deletes the same two keys `rename` rewrites — and only when they name
/// the remote going away.
///
/// With `branch.main.pushRemote = origin` and `remote.pushDefault = origin` in
/// the file, `remote remove origin` on `Shape::BehindRemote` leaves
/// `config --list --local` holding **nothing but the six `core.*` keys**: every
/// `remote.*`, every `branch.*` and both push keys are gone, and
/// `refs/remotes/origin/*` with them. Pointed at another remote, all three
/// survive. A port that deletes the stanza and stops matches stdout (`remove`
/// prints nothing) and the exit code.
fn remove_rewrites(out: &mut Vec<Case>) {
    out.push(
        Case::new("remote", &["remote", "remove", "origin"], Shape::BehindRemote).with_scoped_config(
            repo(&[("branch.main.pushRemote", "origin"), ("remote.pushDefault", "origin")]),
        ),
    );
    out.push(
        Case::new("remote", &["remote", "remove", "origin"], Shape::BehindRemote).with_scoped_config(
            repo(&[
                ("remote.other.url", "./other.git"),
                ("branch.main.pushRemote", "other"),
                ("remote.pushDefault", "other"),
            ]),
        ),
    );
    // The `rm` spelling against the second premise, so both entry points are
    // measured on the conditional half rather than only on the stanza.
    out.push(
        Case::new("remote", &["remote", "rm", "origin"], Shape::BehindRemote).with_scoped_config(repo(&[
            ("remote.other.url", "./other.git"),
            ("branch.main.pushRemote", "other"),
            ("remote.pushDefault", "other"),
        ])),
    );
}

// ---------------------------------------------------------------------------
// url lists
// ---------------------------------------------------------------------------

/// `remote.<n>.url` and `.pushurl` are multi-valued; every existing case has one
/// of each.
///
/// The premise below gives `many` two of each. Measured on stock 2.55.0:
///
/// ```text
/// get-url many              -> ./one.git
/// get-url --all many        -> ./one.git ./two.git
/// get-url --push many       -> ./push1.git
/// get-url --push --all many -> ./push1.git ./push2.git
/// remote -v                 -> one (fetch) line and *two* (push) lines
/// set-url many N ./two.git  -> ./two.git replaced in place: one.git, N
/// set-url many N ./nomatch  -> fatal: No such URL found: ./nomatch.git   (128)
/// set-url many N            -> warning: remote.many.url has multiple values
///                              fatal: could not set 'remote.many.url' …  (128)
/// set-url --delete many ./nomatch.git
///                           -> fatal: could not unset 'remote.many.url'  (128)
/// set-url --delete many '^\./.*'
///                           -> fatal: Will not delete all non-push URLs  (128)
/// set-url --delete many ./two.git       -> succeeds, one url left
/// set-url --delete --push many ./push1.git -> succeeds, one pushurl left
/// ```
///
/// Four of those are refusals whose message is the behaviour, so they are
/// strict. The two-argument `set-url` on a multi-valued key is the sharpest: it
/// is the *ordinary* spelling, and it fails — a port that implements it as "drop
/// the list, write one value" succeeds where stock refuses and leaves a remote
/// with one url instead of two.
fn url_lists(out: &mut Vec<Case>) {
    let many = || {
        repo(&[
            ("remote.many.url", "./one.git"),
            ("remote.many.url", "./two.git"),
            ("remote.many.pushurl", "./push1.git"),
            ("remote.many.pushurl", "./push2.git"),
            ("remote.many.fetch", "+refs/heads/*:refs/remotes/many/*"),
        ])
    };
    let q = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("remote", args, Shape::BehindRemote).with_scoped_config(many()));
    };
    let r = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::strict("remote", args, Shape::BehindRemote).with_scoped_config(many()));
    };
    q(&["remote", "get-url", "many"], out);
    q(&["remote", "get-url", "--all", "many"], out);
    q(&["remote", "get-url", "--push", "many"], out);
    q(&["remote", "get-url", "--push", "--all", "many"], out);
    q(&["remote", "-v"], out);
    q(&["remote", "set-url", "many", "./new.git", "./two.git"], out);
    q(&["remote", "set-url", "--delete", "many", "./two.git"], out);
    q(&["remote", "set-url", "--delete", "--push", "many", "./push1.git"], out);
    q(&["remote", "set-url", "--add", "many", "./three.git"], out);

    r(&["remote", "set-url", "many", "./new.git", "./nomatch.git"], out);
    r(&["remote", "set-url", "many", "./new.git"], out);
    r(&["remote", "set-url", "--push", "many", "./new.git", "./two.git"], out);
    r(&["remote", "set-url", "--delete", "many", "./nomatch.git"], out);
    r(&["remote", "set-url", "--delete", "--push", "many", "./nomatch.git"], out);
    // The argument is a regular expression, not a literal, and one that matches
    // everything hits the guard rather than emptying the list.
    r(&["remote", "set-url", "--delete", "many", "^\\./.*"], out);
}

// ---------------------------------------------------------------------------
// listing
// ---------------------------------------------------------------------------

/// The listing forms, against config shapes that make a column empty.
///
/// Two legal-but-partial remotes nothing else in the corpus builds:
///
///  * **fetch refspec, no url.** `remote` lists it; `remote -v` prints
///    `half\t` — the name, a tab, and *nothing* — because it has no fetch url
///    and no pushurl to print, and `show -n half` reports `Fetch URL: half`:
///    with no url configured git falls back to treating the *name* as one.
///  * **pushurl, no url.** `remote -v` prints an empty fetch line and then a
///    real push line, and `get-url pushonly` answers `pushonly`.
///
/// Plus the orderings and the spellings: the listing is sorted by name
/// regardless of file order, `--verbose` is `-v`, `-v -v` is not more verbose
/// than `-v`, and `-v` in front of `update`/`show` changes what those print
/// rather than being ignored.
fn listing_edges(out: &mut Vec<Case>) {
    let half = || repo(&[("remote.half.fetch", "+refs/heads/*:refs/remotes/half/*")]);
    let pushonly = || repo(&[("remote.pushonly.pushurl", "./push.git")]);

    for args in [
        &["remote"][..],
        &["remote", "-v"][..],
        &["remote", "show", "-n", "half"][..],
        &["remote", "get-url", "half"][..],
        &["remote", "get-url", "--all", "half"][..],
    ] {
        out.push(Case::new("remote", args, Shape::BehindRemote).with_scoped_config(half()));
    }
    for args in [&["remote", "-v"][..], &["remote", "get-url", "pushonly"][..]] {
        out.push(Case::new("remote", args, Shape::BehindRemote).with_scoped_config(pushonly()));
    }

    // Declared b-then-a in the file; listed a, b, origin.
    let unsorted = || repo(&[("remote.b.url", "./b.git"), ("remote.a.url", "./a.git")]);
    out.push(Case::new("remote", &["remote"], Shape::BehindRemote).with_scoped_config(unsorted()));
    out.push(Case::new("remote", &["remote", "-v"], Shape::BehindRemote).with_scoped_config(unsorted()));

    // Spellings of the verbosity flag, and the two subcommands it changes.
    out.push(Case::new("remote", &["remote", "--verbose"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "-v", "-v"], Shape::BehindRemote));
    // `remote -v update` prints the per-ref fetch report that plain `update`
    // does not — `= [up to date] main -> origin/main` for each tracking ref.
    out.push(Case::new("remote", &["remote", "-v", "update"], Shape::BehindRemote));
    out.push(Case::new("remote", &["remote", "--verbose", "show", "origin"], Shape::BehindRemote));
}

// ---------------------------------------------------------------------------
// set-head against a peer that has a HEAD
// ---------------------------------------------------------------------------

/// Every `set-head` mode, on the one shape whose peer can answer.
///
/// `Shape::BehindRemote`'s peer is `init --bare .remote.git` with no `-b`
/// (`fixture.rs:1471`), so its `HEAD` is `ref: refs/heads/master` and nothing is
/// there — which is why `branch_remote.rs` can only reach the refusal.
/// `Shape::HooksFail` builds its peer with `init --bare -b main`
/// (`fixture.rs:2065`) and pushes `main` and `hf-side` into it, so the
/// advertisement carries a resolvable symref and every mode works. Measured on a
/// replica of that shape:
///
/// ```text
/// set-head -a origin        -> 'origin/HEAD' is now created and points to 'main'
/// set-head --auto origin    -> the same, from the long spelling
/// set-head origin hf-side   -> silent; refs/remotes/origin/HEAD -> …/hf-side
/// set-head -d origin        -> silent; nothing to delete, still exit 0
/// set-head --delete origin  -> the same, from the long spelling
/// set-head origin nosuch    -> error: Not a valid ref: refs/remotes/origin/nosuch (1)
/// ```
///
/// `for-each-ref` in the post-state probe is the measurement: it prints the
/// object `refs/remotes/origin/HEAD` resolves to, so a port that writes the
/// symref pointing at the wrong branch is caught by the id even though the line
/// on stdout is identical.
///
/// The peer's hooks are irrelevant here and that is checked rather than assumed:
/// `fixture.rs:3001` installs one `update` hook in the peer, which is a
/// *receive* hook, and every command below is a read.
fn set_head_live_peer(out: &mut Vec<Case>) {
    for args in [
        &["remote", "set-head", "-a", "origin"][..],
        &["remote", "set-head", "--auto", "origin"][..],
        &["remote", "set-head", "origin", "hf-side"][..],
        &["remote", "set-head", "-d", "origin"][..],
        &["remote", "set-head", "--delete", "origin"][..],
    ] {
        out.push(Case::new("remote", args, Shape::HooksFail));
    }
    out.push(Case::strict("remote", &["remote", "set-head", "origin", "nosuch"], Shape::HooksFail));

    // `show` against the same peer prints `HEAD branch: main` where every
    // existing `show` case prints `(unknown)`, so the line that reads the
    // advertisement's symref is measured for the first time.
    out.push(Case::new("remote", &["remote", "show", "origin"], Shape::HooksFail));
    out.push(Case::new("remote", &["remote", "show", "-n", "origin"], Shape::HooksFail));
}

// ---------------------------------------------------------------------------
// set-branches
// ---------------------------------------------------------------------------

/// `set-branches` on premises where it does something other than rewrite a spec.
///
/// ```text
/// set-branches origin              -> exit 0, and remote.origin.fetch is *gone*
/// set-branches mir main            -> exit 1, and **nothing on either stream**
/// set-branches --add mir main      -> exit 0, writes fetch = +refs/main:refs/main
/// set-branches --add half main     -> exit 0 on a remote that had no url
/// ```
///
/// The first is the deletion nobody would guess from the name: with no branches
/// named, the whole fetch refspec is removed, which `config --list --local`
/// reports and stdout does not.
///
/// The second is a silent non-zero exit: measured on stock 2.55.0, both streams
/// are empty and the status is 1. Strict, because "exit 1 and say nothing" is
/// exactly the contract a port gets wrong by adding a message.
/// `--add` on the same mirror does *not* fail, and synthesizes
/// `+refs/main:refs/main` — the `refs/heads/` prefix that `-t` and the ordinary
/// `set-branches` add is skipped for a mirror, producing a spec that matches a
/// top-level ref rather than a branch.
fn set_branches_edges(out: &mut Vec<Case>) {
    out.push(Case::new("remote", &["remote", "set-branches", "origin"], Shape::BehindRemote));
    out.push(Case::strict("remote", &["remote", "set-branches", "--add", "origin"], Shape::BehindRemote));

    let mirror = || repo(&[("remote.mir.url", "./.remote.git"), ("remote.mir.mirror", "true")]);
    out.push(
        Case::strict("remote", &["remote", "set-branches", "mir", "main"], Shape::BehindRemote)
            .with_scoped_config(mirror()),
    );
    out.push(
        Case::new("remote", &["remote", "set-branches", "--add", "mir", "main"], Shape::BehindRemote)
            .with_scoped_config(mirror()),
    );
    out.push(
        Case::new("remote", &["remote", "set-branches", "--add", "half", "main"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("remote.half.fetch", "+refs/heads/*:refs/remotes/half/*")])),
    );
}

// ---------------------------------------------------------------------------
// prune / update, driven by configuration
// ---------------------------------------------------------------------------

/// Which remotes `update` selects, and what `prune` considers stale — both as
/// functions of `.git/config` rather than of argv.
///
/// `branch_remote.rs` reaches prune through a `-c` refspec that matches nothing
/// and reaches `update` through `--prune` and `default`. Everything below is a
/// *key* instead, and every key here was previously unset by any case:
///
/// ```text
/// remote.<n>.prune = true      -> `remote update <n>` prunes with no -p
/// remote.<n>.skipDefaultUpdate -> `remote update` and `remote update default`
///                                 become silent no-ops (not even "Fetching …")
/// remotes.<group> = origin     -> `remote update <group>` fetches origin
/// remotes.<group> =            -> fatal: no such remote or remote group: …  (1)
/// remotes.<group> = origin up  -> origin is fetched, `up` is not a remote,
///                                 error: could not fetch up                 (1)
/// fetch.pruneTags = true       -> the tag half of prune, on a peer with none
/// ```
///
/// The prune half uses a remote whose refspec maps a source namespace the peer
/// does not have onto `refs/remotes/origin/*`, so every ref under that
/// destination is unmatched and `prune` has real deletions — the two tracking
/// refs disappear from `for-each-ref`, and, because prune is a *local*
/// operation, `probe_peer` must show the peer completely unchanged.
///
/// The mirror refspec is the widest destination a remote can have
/// (`+refs/*:refs/*`), which makes prune walk the whole ref namespace rather
/// than one subtree: on stock it deletes `origin/div` and `origin/main` — which
/// the peer does not have under that name — and leaves `refs/heads/main` and
/// `refs/heads/div` alone, which it does. A port that prunes by "everything
/// under refs/remotes" gets the first half right and the second half wrong.
fn prune_update_config(out: &mut Vec<Case>) {
    let stale = || {
        repo(&[
            ("remote.st.url", "./.remote.git"),
            ("remote.st.fetch", "+refs/heads/nosuch/*:refs/remotes/origin/*"),
        ])
    };
    let stale_pruning = || {
        repo(&[
            ("remote.st.url", "./.remote.git"),
            ("remote.st.fetch", "+refs/heads/nosuch/*:refs/remotes/origin/*"),
            ("remote.st.prune", "true"),
        ])
    };
    // The pair that isolates the key: same remote, same argv, one extra line of
    // config — and two different post-states.
    out.push(Case::new("remote", &["remote", "update", "st"], Shape::BehindRemote).with_scoped_config(stale()));
    out.push(
        Case::new("remote", &["remote", "update", "st"], Shape::BehindRemote)
            .with_scoped_config(stale_pruning()),
    );
    // `show -n` reads the same refspec without contacting anything, and lists
    // the *source* side of it — `nosuch/div`, `nosuch/main` — which is a
    // rendering no other `show` case produces.
    out.push(Case::new("remote", &["remote", "show", "-n", "st"], Shape::BehindRemote).with_scoped_config(stale()));

    // skipDefaultUpdate: the remote drops out of the implicit set entirely.
    let skip = || repo(&[("remote.origin.skipDefaultUpdate", "true")]);
    out.push(Case::new("remote", &["remote", "update"], Shape::BehindRemote).with_scoped_config(skip()));
    out.push(Case::new("remote", &["remote", "update", "default"], Shape::BehindRemote).with_scoped_config(skip()));
    // Named explicitly, the key does not apply: `update origin` still fetches.
    out.push(Case::new("remote", &["remote", "update", "origin"], Shape::BehindRemote).with_scoped_config(skip()));

    // Groups.
    let groups = || {
        repo(&[
            ("remotes.grp", "origin"),
            ("remotes.empty", ""),
            ("remotes.both", "origin up"),
        ])
    };
    out.push(Case::new("remote", &["remote", "update", "grp"], Shape::BehindRemote).with_scoped_config(groups()));
    out.push(Case::new("remote", &["remote", "update", "empty"], Shape::BehindRemote).with_scoped_config(groups()));
    out.push(Case::new("remote", &["remote", "update", "both"], Shape::BehindRemote).with_scoped_config(groups()));
    out.push(
        Case::new("remote", &["remote", "update", "-p", "grp"], Shape::BehindRemote).with_scoped_config(groups()),
    );
    out.push(Case::strict("remote", &["remote", "update", "nosuchgroup"], Shape::BehindRemote));

    // Prune across the whole ref namespace.
    let mirror_spec = || repo(&[("remote.mir.url", "./.remote.git"), ("remote.mir.fetch", "+refs/*:refs/*")]);
    out.push(
        Case::new("remote", &["remote", "prune", "mir"], Shape::BehindRemote).with_scoped_config(mirror_spec()),
    );
    out.push(
        Case::new("remote", &["remote", "prune", "-n", "mir"], Shape::BehindRemote)
            .with_scoped_config(mirror_spec()),
    );

    // The tag half of prune. The peer carries no tags, so what this pins is that
    // the key is *read and acted on* without changing the answer — a port that
    // rejects the key outright, or that prunes branches for it, diverges.
    out.push(
        Case::new("remote", &["remote", "update", "origin"], Shape::BehindRemote)
            .with_scoped_config(repo(&[("fetch.pruneTags", "true"), ("fetch.prune", "true")])),
    );

    // `show -n` reads the tuning keys without contacting the peer; `prune`
    // against a name that is not a remote is treated as a *URL* and fails in the
    // transport, which is why it is not strict.
    out.push(
        Case::new("remote", &["remote", "show", "-n", "origin"], Shape::BehindRemote).with_scoped_config(repo(&[
            ("remote.origin.tagOpt", "--no-tags"),
            ("remote.origin.prune", "true"),
            ("remote.origin.skipDefaultUpdate", "true"),
        ])),
    );
    out.push(Case::new("remote", &["remote", "prune", "nosuch"], Shape::BehindRemote));
    // …while `show -n` on the same name **succeeds**, exit 0, printing the name
    // as its own URL. The two sit together because the contrast is the finding.
    out.push(Case::new("remote", &["remote", "show", "-n", "nosuch"], Shape::BehindRemote));
}
