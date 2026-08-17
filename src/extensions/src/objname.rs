//! `get_oid_basic()`'s full-length-hex rule, in one place.
//!
//! git resolves an object name through `get_oid_basic()` (`object-name.c`),
//! whose very first branch is:
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) {
//!         if (repo_settings_get_warn_ambiguous_refs(r) && warn_on_object_refname_ambiguity) { … }
//!         return 0;
//! }
//! ```
//!
//! Both halves of it live here: [`full_hex`] is the decode and the `return 0`,
//! [`warn_ambiguous_refname`] is the elided block — the `warning: refname … is
//! ambiguous.` a repository containing a ref named 40 hex digits earns, and the
//! paragraph `advice.objectNameWarning` gates. They are separate functions
//! because git reaches `get_oid_basic()` once per operand while several helpers
//! below decode a name a second time to *diagnose* it, and a doubled warning is
//! as wrong as a missing one.
//!
//! A name whose length is exactly the hash's hex length *is* the object id. git
//! decodes it and returns **without ever asking the object database whether that
//! object exists**, and before any ref/path handling — so a full-length hex wins
//! over a ref that happens to share the name, and a 39- or 41-character hex
//! string falls through to the ordinary parser instead.
//!
//! gitoxide's `rev_parse_single()` resolves through the odb, so it fails for a
//! well-formed id that is simply absent. A call site that uses it as its only
//! resolver therefore collapses git's "resolved, but the object is missing" into
//! "not a valid object name", and reports the wrong message — usually with the
//! wrong exit code, and in a few commands turning a stock exit 0 into a hard
//! failure.
//!
//! That difference is not one subcommand's problem: it reaches every command
//! that takes an object name from argv. Keeping the rule here means a call site
//! restores git's behaviour by choosing a resolver, not by re-deriving the rule
//! — the two hand-written copies this module replaced had already drifted apart
//! in the order they tried the two resolvers.
//!
//! `get_oid_hex()` accepts either case (`hexval()` is case-insensitive) while
//! `ObjectId::from_hex` wants lowercase, which is why the name is folded first.

use crate::advice::Advice;
use gix::hash::ObjectId;
use std::sync::atomic::{AtomicBool, Ordering};

/// git's `get_oid_basic()` first branch: the name decoded as an object id, or
/// `None` when it is not exactly `hexsz` hex digits.
///
/// The object database is deliberately not consulted — an absent object still
/// yields its id, which is the whole point of the rule.
///
/// Decoding only. The `warning: refname … is ambiguous.` half of that branch is
/// [`warn_ambiguous_refname`], kept separate because several helpers below ask
/// "would this decode?" about a name a caller is only *diagnosing*, and git does
/// not reach `get_oid_basic()` twice for one operand.
pub fn full_hex(repo: &gix::Repository, name: &str) -> Option<ObjectId> {
    if name.len() != repo.object_hash().len_in_hex() || !name.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    ObjectId::from_hex(name.to_ascii_lowercase().as_bytes()).ok()
}

/// `cfg->warn_on_object_refname_ambiguity` (`environment.c`, initialised to 1).
///
/// A per-process switch, distinct from `core.warnAmbiguousRefs`: the four sites
/// listed on [`AmbiguityWarnings`] clear it around a bulk read and put it back
/// afterwards, so the *user's* configuration is never what is being changed.
static WARN_ON_OBJECT_REFNAME_AMBIGUITY: AtomicBool = AtomicBool::new(true);

/// The `save_warning = cfg->warn_on_object_refname_ambiguity; cfg->… = 0; …;
/// cfg->… = save_warning;` bracket git writes by hand in four places, as a guard
/// that restores on drop.
///
/// At 2.55.0 those places — and *only* those places — are:
///
/// | site | what it covers |
/// |---|---|
/// | `read_revisions_from_stdin()` (`revision.c`) | every `--stdin` rev reader: `rev-list --stdin`, `log --stdin`, … |
/// | `batch_objects()` (`builtin/cat-file.c`) | `cat-file --batch`, `--batch-check`, `--batch-command` |
/// | `get_object_list()` (`builtin/pack-objects.c`) | the `pack-objects --revs` stdin loop |
/// | `collect_changed_submodules()` (`submodule.c`) | the internal revision walk behind `push --recurse-submodules` and `fetch`'s submodule detection |
///
/// The commands that take an object name from **argv** are not on that list and
/// do warn — `cat-file -t <id>`, `rev-list <id>`, `for-each-ref --contains=<id>`
/// and the rest. `update-ref` is silent for a different reason: it passes
/// `GET_OID_SKIP_AMBIGUITY_CHECK` (the `!(flags & …)` in the same condition), so
/// it never consults the switch at all.
///
/// The comment git leaves at the `cat-file` site is the rationale for all four:
/// they resolve a potentially very large number of names that are already object
/// ids, and the cost of asking the ref store about each one "just so we can warn"
/// dwarfs the lookups themselves.
#[must_use = "the switch is restored when this guard drops, so it must be held for the whole bulk read"]
pub struct AmbiguityWarnings {
    saved: bool,
}

impl AmbiguityWarnings {
    /// Clear `warn_on_object_refname_ambiguity` until the guard drops.
    pub fn off() -> Self {
        Self { saved: WARN_ON_OBJECT_REFNAME_AMBIGUITY.swap(false, Ordering::Relaxed) }
    }
}

impl Drop for AmbiguityWarnings {
    fn drop(&mut self) {
        WARN_ON_OBJECT_REFNAME_AMBIGUITY.store(self.saved, Ordering::Relaxed);
    }
}

/// `object_name_msg` in `object-name.c`, verbatim (git 2.55.0). git prints it
/// with a bare `fprintf(stderr, "%s\n", …)` rather than through `advise()`, so it
/// carries no `hint: ` prefix and no color.
pub(crate) const OBJECT_NAME_MSG: &str = "\
Git normally never creates a ref that ends with 40 hex characters
because it will be ignored when you just specify 40-hex. These refs
may be created by mistake. For example,

  git switch -c $br $(git rev-parse ...)

where \"$br\" is somehow empty and a 40-hex ref is created. Please
examine these refs and maybe delete them. Turn this message off by
running \"git config set advice.objectNameWarning false\"";

/// The other half of `get_oid_basic()`'s first branch — the one [`full_hex`]
/// deliberately leaves out:
///
/// ```c
/// if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) {
///         if (!(flags & GET_OID_SKIP_AMBIGUITY_CHECK) &&
///             repo_settings_get_warn_ambiguous_refs(r) &&
///             cfg->warn_on_object_refname_ambiguity) {
///                 refs_found = repo_dwim_ref(r, str, len, &tmp_oid, &real_ref, 0);
///                 if (refs_found > 0) {
///                         warning(warn_msg, len, str);
///                         if (advice_enabled(ADVICE_OBJECT_NAME_WARNING))
///                                 fprintf(stderr, "%s\n", _(object_name_msg));
///                 }
///                 free(real_ref);
///         }
///         return 0;
/// }
/// ```
///
/// Four gates, in git's order, and each one is separately observable:
///
/// 1. the name is exactly `hexsz` hex digits — otherwise this branch is not the
///    one taken and there is nothing to say;
/// 2. [`AmbiguityWarnings`] — the per-process switch the bulk readers clear;
/// 3. `core.warnAmbiguousRefs`, **default true** (`repo-settings.c` passes `1`
///    as the fallback to `repo_cfg_bool`), so silence requires setting it false;
/// 4. a ref actually answers to those 40 characters, which is the whole point:
///    the message is about a ref created by accident, not about the id.
///
/// `advice.objectNameWarning` gates only the explanatory paragraph. The
/// `warning:` line itself is not advice and survives `advice.objectNameWarning=false`.
///
/// Note what is *not* a gate: `GET_OID_QUIETLY`. `--quiet` suppresses the second,
/// unrelated ambiguity warning further down `get_oid_basic()` but not this one,
/// because this branch tests `GET_OID_SKIP_AMBIGUITY_CHECK` instead — a flag only
/// `builtin/update-ref.c` passes.
///
/// Returns whether the full-hex branch was taken at all, i.e. whether
/// `get_oid_basic()` would have `return 0`ed here without looking at anything
/// else. A caller that also implements the *later* ambiguity warning uses that
/// to stop, exactly as the `return 0` does.
pub fn warn_ambiguous_refname(repo: &gix::Repository, name: &str) -> bool {
    let base = ambiguity_base(name);
    if base.len() != repo.object_hash().len_in_hex()
        || !base.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    if !WARN_ON_OBJECT_REFNAME_AMBIGUITY.load(Ordering::Relaxed) {
        return true;
    }
    if repo.config_snapshot().boolean("core.warnAmbiguousRefs") == Some(false) {
        return true;
    }
    if crate::porcelain::rev_parse::dwim_ref_matches(repo, base).is_empty() {
        return true;
    }
    eprintln!("warning: refname '{base}' is ambiguous.");
    if Advice::ObjectNameWarning.enabled_in(repo) {
        eprintln!("{OBJECT_NAME_MSG}");
    }
    true
}

/// The substring `get_oid_basic()` is eventually handed for `spec`.
///
/// `repo_get_oid()` does not reach `get_oid_basic()` with the operand as typed:
/// `get_oid_with_context_1()` cuts a `<rev>:<path>` at the colon, then
/// `get_oid_1()` peels one `~<n>`/`^<n>` suffix and recurses, and `peel_onion()`
/// cuts a trailing `^{…}` — each ending at one `get_oid_basic()` call on what is
/// left. So `git rev-parse <40-hex>^{commit}` warns about the 40 hex characters,
/// naming them and not the operand, which is why the message is built from this
/// and not from `spec`.
///
/// `@{…}` is deliberately not peeled: a reflog spelling keeps its full length, so
/// `get_oid_basic()`'s first branch is *not* the one it takes — it falls through
/// to the reflog handling and to the second, differently-gated warning below it.
fn ambiguity_base(spec: &str) -> &str {
    // `handle_revision_arg_1()` strips the exclusion mark before resolving.
    let mut base = spec.strip_prefix('^').filter(|rest| !rest.is_empty()).unwrap_or(spec);
    // `cp = strchr(name, ':')`, with the empty left side left alone — `:/text`
    // and `:<path>` are their own forms and never reach `get_oid_basic()`.
    if let Some(at) = base.find(':').filter(|at| *at > 0) {
        base = &base[..at];
    }
    loop {
        // `peel_onion()`: at least four characters, ending in `}`, and the
        // *rightmost* `{` whose predecessor is `^` opens the type name.
        if base.len() >= 4 && base.ends_with('}') {
            if let Some(at) = base.rfind("^{").filter(|at| *at > 0) {
                base = &base[..at];
                continue;
            }
        }
        // `get_oid_1()`: trailing digits, then one `~` or `^`, then recurse on
        // what precedes it.
        let head = base.trim_end_matches(|c: char| c.is_ascii_digit());
        match head.strip_suffix(['~', '^']).filter(|rest| !rest.is_empty()) {
            Some(rest) => base = rest,
            None => return base,
        }
    }
}

/// `get_oid()` for a single object name: git's ordering, full hex first and the
/// ordinary revspec parser second.
///
/// Prefer this over a bare `rev_parse_single()` wherever the name comes from
/// argv. `None` means the name does not resolve at all, which is the only case
/// where git itself reports "not a valid object name"; a name that resolves to
/// an object the repository does not have returns `Some` here, leaving the
/// caller free to produce whatever git produces for a missing object.
pub fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    // `repo_get_oid()` reaches `get_oid_basic()` once per operand, so this is
    // where the ambiguity warning belongs — not in `full_hex`, which the helpers
    // below call a second time to *diagnose* a name this has already resolved.
    warn_ambiguous_refname(repo, spec);
    resolve_quiet(repo, spec)
}

/// [`resolve`] without `get_oid_basic()`'s ambiguity warning, for the callers
/// that are re-examining a name rather than resolving it for the first time.
///
/// git warns once per operand because it resolves once per operand; a zvcs
/// classifier that answers "is this a range?" and is then asked "what range?"
/// would otherwise warn twice for one argument.
fn resolve_quiet(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    full_hex(repo, spec).or_else(|| repo.rev_parse_single(spec).ok().map(|id| id.detach()))
}

/// Whether `spec` named an object the repository does not actually have.
///
/// The distinction git draws in several commands — `name-rev` and
/// `describe --contains` print `Could not get sha1 for %s` for a name that does
/// not resolve but `Could not get object for %s` for one that resolves to an
/// absent object — needs both answers, not just the id.
pub fn resolves_but_absent(repo: &gix::Repository, spec: &str) -> bool {
    match full_hex(repo, spec) {
        Some(id) => repo.find_object(id).is_err(),
        None => false,
    }
}

/// The name `get_reference()` (`revision.c`) would `die("bad object %s")` on,
/// or `None` when `word` is not one of the spellings that reaches it with a
/// full-length hex the repository does not have.
///
/// `setup_revisions()` is the other half of the rule at the top of this module:
/// once `get_oid_with_context()` has decoded a full-length hex without asking
/// the odb, the very next thing `handle_revision_arg_1()` does is
///
/// ```c
/// object = get_reference(revs, arg, &oid, flags ^ local_flags);
/// ```
///
/// and `get_reference()` `parse_object()`s the id and dies naming `arg`. So an
/// absent full hex is `bad object <name>` here, never `bad revision '<name>'` —
/// and `arg` is the name *after* the leading `^` was stripped and after a
/// `^@`/`^!`/`^-<n>` mark was cut off, which is why the diagnostic quotes less
/// than the operand did. The name is returned as written, uppercase included.
pub fn bad_object_name<'a>(repo: &gix::Repository, word: &'a str) -> Option<&'a str> {
    let base = parents_only_base(word);
    // `handle_revision_arg_1()` strips its `^` after the marks are handled, and
    // `add_parents_only()` strips one of its own from what it was handed.
    let base = base.strip_prefix('^').filter(|rest| !rest.is_empty()).unwrap_or(base);
    resolves_but_absent(repo, base).then_some(base)
}

/// The name `add_parents_only()` is handed for the `^@`, `^!` and `^-<n>`
/// spellings — `handle_revision_arg_1()` writes a NUL over the mark and passes
/// the truncated argument — or `word` itself when it carries no mark.
///
/// ```c
/// mark = strstr(arg, "^@");
/// if (mark && !mark[2]) { *mark = 0; if (add_parents_only(revs, arg, flags, 0)) return 0; ... }
/// ```
///
/// `^@` and `^!` are only marks at the very end (`!mark[2]`), while `^-` takes
/// the digits after it as the parent number, so a non-numeric tail is not a mark
/// at all.
fn parents_only_base(word: &str) -> &str {
    if let Some(base) = word.strip_suffix("^@").or_else(|| word.strip_suffix("^!")) {
        return base;
    }
    match word.find("^-") {
        // `strtol_i(mark + 2, 10, &num)`, with an empty tail meaning parent 1.
        Some(at) if word[at + 2..].bytes().all(|b| b.is_ascii_digit()) => &word[..at],
        _ => word,
    }
}

/// The two endpoints of a range operand, as `handle_dotdot_1()` reads them.
pub struct Range<'a> {
    /// The left endpoint, with git's `if (!*a_name) a_name = "HEAD";` applied.
    pub a: &'a str,
    /// The right endpoint, past the separator and with the same default.
    pub b: &'a str,
    /// `A...B` rather than `A..B`, which changes both the diagnostic's wording
    /// and how strict [`dotdot`] is about the endpoints.
    pub symmetric: bool,
}

/// `handle_dotdot()`'s `strstr(arg, "..")` split, or `None` when there is none.
///
/// The *first* `..` separates, so `A..B..C` is `A` against `B..C`; a third dot
/// immediately after it makes the range symmetric and is consumed.
pub fn split_range(spec: &str) -> Option<Range<'_>> {
    let cut = spec.find("..")?;
    let (a, rest) = (&spec[..cut], &spec[cut + 2..]);
    let symmetric = rest.starts_with('.');
    let b = if symmetric { &rest[1..] } else { rest };
    Some(Range {
        a: if a.is_empty() { "HEAD" } else { a },
        b: if b.is_empty() { "HEAD" } else { b },
        symmetric,
    })
}

/// git's guard *in front of* `handle_dotdot()`, which is the one token that
/// holds a `..` and is still not a range (`handle_revision_arg_1()`,
/// `revision.c`):
///
/// ```c
/// if (!cant_be_filename && !strcmp(arg, "..")) {
///         /*
///          * Just ".."?  That is not a range but the
///          * pathspec for the parent directory.
///          */
///         ret = -1;
///         goto out;
/// }
///
/// if (!handle_dotdot(arg, revs, flags, revarg_opt)) {
/// ```
///
/// The comparison is against the whole argument, so only a bare `..` qualifies;
/// `../x`, `a..`, `..b` and `...` all go on to `handle_dotdot()` as usual. The
/// `-1` return means `setup_revisions()` never sees a range at all and takes its
/// `verify_filename()` branch, where `..` lstats fine and becomes prune data —
/// so the diagnostic the user finally gets is the *pathspec* layer's
/// `die(_("%s: '%s' is outside repository at '%s'"))` (`pathspec.c`), not a
/// revision error.
///
/// `cant_be_filename` is `REVARG_CANNOT_BE_FILENAME`, which `setup_revisions()`
/// sets only for arguments standing in front of a `--` it found itself. With it
/// the guard does not fire and `..` is the ordinary `HEAD..HEAD` range, which is
/// why this is a parameter rather than a bare string comparison.
///
/// This lives here rather than in each command because it is the same question
/// [`split_range`] answers — "is this token a range?" — and the two answers have
/// to be given in one place or they drift. It is deliberately *not* folded into
/// [`split_range`] itself: that function is a pure syntactic split with no way
/// to know whether a `--` was seen, and the commands that hand it a token
/// already past a `--` still need `HEAD..HEAD`.
pub fn is_parent_directory_pathspec(spec: &str, cant_be_filename: bool) -> bool {
    !cant_be_filename && spec == ".."
}

/// What `handle_dotdot_1()` made of a range operand.
pub enum Dotdot {
    /// Both endpoints resolved to an object the repository has.
    ///
    /// For `A..B` these are the ids `parse_object()` returned, of whatever type
    /// — a blob endpoint is perfectly acceptable to `handle_dotdot_1()` and only
    /// upsets whoever consumes the pending list. For `A...B` they have already
    /// been through `lookup_commit_reference()`, so they are commits.
    Ok { a: ObjectId, b: ObjectId },
    /// `dotdot_missing()`: an endpoint resolved but its object is not in the
    /// database, or — for `A...B` only — is not commit-ish. `notes` holds the
    /// `object %s is a %s, not a %s` lines `lookup_commit_reference()` already
    /// printed.
    Missing { notes: Vec<String> },
    /// `handle_dotdot_1()` returned -1: the token holds no `..`, or an endpoint
    /// `get_oid_with_context()` could not resolve at all. The operand is not a
    /// range git could use and the caller falls back to its own diagnosis.
    NotARange,
}

/// git's `handle_dotdot_1()` (`revision.c`), which is where the full-hex rule
/// becomes visible for ranges:
///
/// ```c
/// if (repo_get_oid_with_context(revs->repo, a_name, oc_flags, &a_oid, a_oc) ||
///     repo_get_oid_with_context(revs->repo, b_name, oc_flags, &b_oid, b_oc))
///         return -1;
/// a_obj = parse_object(revs->repo, &a_oid);
/// b_obj = parse_object(revs->repo, &b_oid);
/// if (!a_obj || !b_obj)
///         return dotdot_missing(arg, dotdot, revs, symmetric);
/// ```
///
/// Both endpoints resolve *before* either is looked up, so an absent full-length
/// hex gets past the first test and fails at the second — which is why git names
/// the whole token (`Invalid revision range <a>..<b>`) rather than the endpoint
/// that failed. An endpoint that does not resolve at all returns -1 instead and
/// never reaches that message.
///
/// The symmetric form has merge bases to compute, so it additionally puts both
/// ends through `lookup_commit_reference()` and takes the same exit when either
/// is not a commit.
pub fn dotdot(repo: &gix::Repository, spec: &str) -> Dotdot {
    let Some(Range { a, b, symmetric }) = split_range(spec) else {
        return Dotdot::NotARange;
    };
    // `handle_dotdot()` runs on the argument as written, before
    // `handle_revision_arg_1()` strips a leading `^`, and `get_oid_basic()` has
    // no reading for one — so a `^`-marked endpoint fails to resolve.
    if a.starts_with('^') || b.starts_with('^') {
        return Dotdot::NotARange;
    }
    // Quiet: this is a classifier, and every caller asks it at least twice
    // ("is this a range?", then "what range?"). The ambiguity warning for the two
    // endpoints belongs to whichever resolution the command actually keeps.
    let (Some(a_oid), Some(b_oid)) = (resolve_quiet(repo, a), resolve_quiet(repo, b)) else {
        return Dotdot::NotARange;
    };
    // `parse_object()` on both, ahead of any type check: a missing object here
    // is `dotdot_missing()` with nothing printed before it.
    if repo.find_object(a_oid).is_err() || repo.find_object(b_oid).is_err() {
        return Dotdot::Missing { notes: Vec::new() };
    }
    if !symmetric {
        return Dotdot::Ok { a: a_oid, b: b_oid };
    }
    let (left, right) = (lookup_commit_reference(repo, a_oid), lookup_commit_reference(repo, b_oid));
    match (&left, &right) {
        (CommitRef::Commit(a), CommitRef::Commit(b)) => Dotdot::Ok { a: *a, b: *b },
        _ => Dotdot::Missing {
            notes: [left.type_error(), right.type_error()].into_iter().flatten().collect(),
        },
    }
}

/// `dotdot_missing()`'s wording, without its `fatal: ` prefix:
///
/// ```c
/// die(symmetric
///     ? "Invalid symmetric difference expression %s"
///     : "Invalid revision range %s", arg);
/// ```
///
/// `arg` is the operand with its separator restored — the whole token, not the
/// endpoint that failed.
pub fn dotdot_missing_message(spec: &str, symmetric: bool) -> String {
    if symmetric {
        format!("Invalid symmetric difference expression {spec}")
    } else {
        format!("Invalid revision range {spec}")
    }
}

/// Everything `handle_dotdot_1()` writes to stderr for a range operand it
/// rejected — `lookup_commit_reference()`'s `error:` notes and then
/// `dotdot_missing()`'s `fatal:`, each newline-terminated, ready to `eprint!` —
/// or `None` when the operand is not a range git would have gotten that far
/// with.
///
/// The notes travel *with* the fatal rather than being printed as a side effect
/// of asking. `setup_revisions()` prints them only on the path that then dies,
/// so a caller that asks "did this operand die here?" about a token that turns
/// out to be a pathspec must be able to get "no" without having already written
/// half a diagnostic.
pub fn dotdot_fatal(repo: &gix::Repository, spec: &str) -> Option<String> {
    let symmetric = split_range(spec)?.symmetric;
    let Dotdot::Missing { notes } = dotdot(repo, spec) else {
        return None;
    };
    let mut out = String::new();
    for note in notes {
        out.push_str(&format!("error: {note}\n"));
    }
    out.push_str(&format!("fatal: {}\n", dotdot_missing_message(spec, symmetric)));
    Some(out)
}

/// git's `lookup_commit_reference()` (`commit.c`), which is what every command
/// that took an object name and wants a *commit* calls next. It is
/// `lookup_commit_reference_gently(r, oid, 0)`, and at 2.55.0 that reads:
///
/// ```c
/// switch (peel_object_ext(r, oid, &peeled_oid, 0, &type)) {
/// case PEEL_NON_TAG: maybe_peeled = oid;         break;
/// case PEEL_PEELED:  maybe_peeled = &peeled_oid; break;
/// default: return NULL;
/// }
/// if (type != OBJ_COMMIT) {
///         if (!quiet)
///                 error(_("object %s is a %s, not a %s"),
///                       oid_to_hex(oid), type_name(type), type_name(OBJ_COMMIT));
///         return NULL;
/// }
/// ```
///
/// The three outcomes are distinguished because git reports them differently: a
/// peel that fails returns NULL silently (the caller supplies the whole message),
/// while a present object of the wrong type prints a diagnostic of its own
/// *before* the caller's — which is why the non-commit case carries an id and a
/// type rather than just failing.
pub enum CommitRef {
    /// The peel landed on a commit.
    Commit(ObjectId),
    /// The object is present but is not a commit, after tag dereferencing.
    ///
    /// `id` is the **operand**, not the peeled result, because the C above names
    /// `oid_to_hex(oid)` — the id the caller passed — while `type_name(type)` is
    /// the type `peel_object_ext()` arrived at. So a tag pointing at a tree is
    /// reported as "object <tag-id> is a tree", mixing the two; stock 2.55.0
    /// prints the tag's id and the word `tree` for `git log <tree-tag>...HEAD`.
    NotCommit { id: ObjectId, kind: gix::object::Kind },
    /// `peel_object_ext()` took the `default:` arm: the odb does not have the
    /// object, or the tag chain cannot be walked.
    Absent,
}

impl CommitRef {
    /// The `error: object %s is a %s, not a %s` line, or nothing when there is no
    /// present object to complain about.
    pub fn type_error(&self) -> Option<String> {
        match self {
            CommitRef::NotCommit { id, kind } => {
                Some(format!("object {id} is a {kind}, not a commit"))
            }
            _ => None,
        }
    }
}

/// Resolve `id` the way `lookup_commit_reference()` does. See [`CommitRef`].
pub fn lookup_commit_reference(repo: &gix::Repository, id: ObjectId) -> CommitRef {
    let Ok(object) = repo.find_object(id) else {
        return CommitRef::Absent;
    };
    // `deref_tag()` walks a tag chain to its end; a chain that cannot be walked
    // is git's NULL, i.e. indistinguishable from the object being absent.
    let Ok(peeled) = object.peel_tags_to_end() else {
        return CommitRef::Absent;
    };
    match peeled.kind {
        gix::object::Kind::Commit => CommitRef::Commit(peeled.id),
        kind => CommitRef::NotCommit { id, kind },
    }
}

/// What a pending object contributes to a commit-only walk, per git's
/// `handle_commit()` (`revision.c`), which `prepare_revision_walk()` runs over
/// every entry `setup_revisions()` left on `revs->pending`:
///
/// ```c
/// while (object->type == OBJ_TAG) {
///         struct tag *tag = (struct tag *) object;
///         if (revs->tag_objects && !(flags & UNINTERESTING))
///                 add_pending_object(revs, object, tag->tag);
///         oid = get_tagged_oid(tag);
///         object = parse_object(revs->repo, oid);
///         …
/// }
/// if (object->type == OBJ_COMMIT) { … return commit; }
/// if (object->type == OBJ_TREE) {
///         struct tree *tree = (struct tree *)object;
///         if (!revs->tree_objects)
///                 return NULL;
///         …
/// }
/// if (object->type == OBJ_BLOB) {
///         if (!revs->blob_objects)
///                 return NULL;
///         …
/// }
/// ```
///
/// The `return NULL` is the whole point: `git log`, `git show`, `git
/// format-patch` and `range-diff`'s inner logs leave `tree_objects` and
/// `blob_objects` off, so a pending tree or blob is **dropped without a word**
/// and the command exits 0. `git log HEAD^{tree}`, `git log <blob>` and
/// `git log HEAD..<tag-of-a-tree>` all print nothing and succeed in stock
/// 2.55.0 — a walk that instead insists every endpoint be a commit turns each of
/// them into a hard failure.
///
/// This is the *other* half of the range rule and is easy to mistake for it:
/// `handle_dotdot_1()` type-checks only the symmetric form (see [`dotdot`]), so
/// `A..B` hands a tree straight through to here and here is where it disappears.
pub fn walk_pending(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    match lookup_commit_reference(repo, id) {
        CommitRef::Commit(commit) => Some(commit),
        // A tree or a blob is `return NULL`; a tag chain that will not peel is
        // the `die("bad object %s")` arm, which no caller here can reach because
        // `handle_dotdot_1()`/`get_reference()` already rejected a missing
        // object before the entry was pended.
        CommitRef::NotCommit { .. } | CommitRef::Absent => None,
    }
}

/// A rejected object-name operand, as the diagnostic git printed and the status
/// it left with.
///
/// git's option callbacks have two shapes and they do not agree: most report
/// with `error()` and return -1, which `parse_options()` turns into a bare
/// `exit(129)` with no usage block, while `parse_opt_merge_filter()` reaches for
/// `die()` on its first failure and so exits 128. Carrying the choice with the
/// message is what keeps a call site from having to remember which is which.
pub struct OperandError {
    /// Lines an inner helper already printed — `object_as_type()`'s complaint —
    /// each rendered as `error: <line>` ahead of [`OperandError::message`].
    pub notes: Vec<String>,
    /// The callback's own message, without its `error: `/`fatal: ` prefix.
    pub message: String,
    /// `die()` (`fatal:`, 128) rather than `error()` + `PARSE_OPT_ERROR` (129).
    pub fatal: bool,
}

impl OperandError {
    /// Print the diagnostic on stderr and yield git's exit status for it.
    pub fn report(&self) -> std::process::ExitCode {
        for note in &self.notes {
            eprintln!("error: {note}");
        }
        if self.fatal {
            eprintln!("fatal: {}", self.message);
            std::process::ExitCode::from(128)
        } else {
            eprintln!("error: {}", self.message);
            std::process::ExitCode::from(129)
        }
    }
}

/// git's `parse_opt_commits()` (`parse-options-cb.c`), behind `OPT_CONTAINS`,
/// `OPT_NO_CONTAINS`, `OPT_WITH` and `OPT_WITHOUT`:
///
/// ```c
/// if (repo_get_oid(the_repository, arg, &oid))
///         return error("malformed object name %s", arg);
/// commit = lookup_commit_reference(the_repository, &oid);
/// if (!commit)
///         return error("no such commit %s", arg);
/// ```
///
/// Note the unquoted `%s` in both messages, and that "no such commit" — not
/// "malformed object name" — is what an absent full-length hex name produces.
pub fn parse_opt_commits(repo: &gix::Repository, arg: &str) -> Result<ObjectId, OperandError> {
    let Some(oid) = resolve(repo, arg) else {
        return Err(OperandError {
            notes: Vec::new(),
            message: format!("malformed object name {arg}"),
            fatal: false,
        });
    };
    let found = lookup_commit_reference(repo, oid);
    match found {
        CommitRef::Commit(id) => Ok(id),
        _ => Err(OperandError {
            notes: found.type_error().into_iter().collect(),
            message: format!("no such commit {arg}"),
            fatal: false,
        }),
    }
}

/// git's `parse_opt_merge_filter()` (`ref-filter.c`), behind `OPT_MERGED` and
/// `OPT_NO_MERGED`:
///
/// ```c
/// if (repo_get_oid(the_repository, arg, &oid))
///         die(_("malformed object name %s"), arg);
/// merge_commit = lookup_commit_reference_gently(the_repository, &oid, 0);
/// if (!merge_commit)
///         return error(_("option `%s' must point to a commit"), opt->long_name);
/// ```
///
/// The asymmetry is deliberate in git and observable: an unresolvable name is
/// `fatal:` at 128, while a name that resolves to something that is not a commit
/// is `error:` at 129. `long_name` is the option spelling without its dashes,
/// which git quotes with a backtick and a plain apostrophe.
pub fn parse_opt_merge_filter(
    repo: &gix::Repository,
    arg: &str,
    long_name: &str,
) -> Result<ObjectId, OperandError> {
    let Some(oid) = resolve(repo, arg) else {
        return Err(OperandError {
            notes: Vec::new(),
            message: format!("malformed object name {arg}"),
            fatal: true,
        });
    };
    let found = lookup_commit_reference(repo, oid);
    match found {
        CommitRef::Commit(id) => Ok(id),
        _ => Err(OperandError {
            notes: found.type_error().into_iter().collect(),
            message: format!("option `{long_name}' must point to a commit"),
            fatal: false,
        }),
    }
}

/// git's `parse_opt_object_name()` (`parse-options-cb.c`), behind `--points-at`:
///
/// ```c
/// if (repo_get_oid(the_repository, arg, &oid))
///         return error(_("malformed object name '%s'"), arg);
/// oid_array_append(opt->value, &oid);
/// ```
///
/// The object database is never consulted — `--points-at` compares ids, so an
/// absent full-length hex name is a perfectly good filter that simply matches
/// nothing. Turning it into an error is what breaks a script: git exits 0 with
/// empty output. The quotes around `%s` are real, and are the one place this
/// family quotes the operand.
pub fn parse_opt_object_name(repo: &gix::Repository, arg: &str) -> Result<ObjectId, OperandError> {
    resolve(repo, arg).ok_or_else(|| OperandError {
        notes: Vec::new(),
        message: format!("malformed object name '{arg}'"),
        fatal: false,
    })
}
