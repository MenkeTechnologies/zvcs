//! `get_oid_basic()`'s object-name rules, in one place.
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
//! as wrong as a missing one — [`resolve_quiet`] is the resolution that says
//! nothing, for exactly that second look.
//!
//! The same function warns a *second* time further down, for a plain name that
//! more than one ref answers to (or that is also an unambiguous abbreviated
//! object id). It carries different gates, so it is not a special case of the
//! first: see [`warn_ambiguous_operand`].
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
//! The same argument settles where a diagnostic belongs. `get_oid_basic()` warns
//! and dies *while resolving*, below the only return path `repo_get_oid()` has,
//! so no caller can hear one without the other and none can decline either.
//! [`resolve`] is this port's `repo_get_oid()`; [`reflog_diagnostics`] is
//! therefore the whole of `get_oid_basic()`'s reflog branch, raised there and
//! nowhere else, and the two commands that hold git's own `flags` —
//! `builtin/rev-parse.c` with `GET_OID_QUIETLY`, and the revision walk, which
//! diagnoses an endpoint before it resolves — reach [`reflog_reach`] and
//! [`read_ref_at_warning`] directly instead. A verb list is the one thing this
//! must not be: git's set is "every command that resolves an argv operand", and
//! that set is not enumerable by hand.
//!
//! `get_oid_hex()` accepts either case, because `hexval()` reads `A`-`F` as well
//! as `a`-`f`. So does `ObjectId::from_hex` as it stands — it decodes through
//! `faster_hex::hex_decode`, which is `hex_decode_with_case(…, CheckCase::None)`
//! (`faster-hex-0.10.0/src/decode.rs:215-217,25-26`). The fold below is
//! therefore not a fix for a rejection; it pins the case policy to this module
//! instead of to a transitive dependency's default, so an operand spelled in
//! upper case cannot start resolving differently because a crate changed its
//! mind about `CheckCase`.

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
/// else.
///
/// The body is [`warn_ambiguous_operand`] with git's default `flags` — no
/// `GET_OID_QUIETLY`, no `GET_OID_SKIP_AMBIGUITY_CHECK` — so it says everything
/// `get_oid_basic()` says about one operand, which is *both* of its ambiguity
/// warnings. This is the spelling every command that resolves an argv operand
/// calls; the two commands that pass one of those flags reach the flagged form
/// directly.
pub fn warn_ambiguous_refname(repo: &gix::Repository, name: &str) -> bool {
    warn_ambiguous_operand(repo, name, OidFlags::default())
}

/// The two bits of `get_oid_basic()`'s `flags` that decide whether it warns.
/// Everything else in `flags` picks an object type or a failure mode and is the
/// resolver's business, not this one's.
#[derive(Clone, Copy, Default)]
pub struct OidFlags {
    /// `GET_OID_QUIETLY`. Gates the *second* warning only — `git rev-parse
    /// --quiet --verify dup` is silent where `git rev-parse --verify dup` warns,
    /// while a 40-hex ref name warns under `--quiet` all the same.
    pub quiet: bool,
    /// `GET_OID_SKIP_AMBIGUITY_CHECK`, which at 2.55.0 only `builtin/update-ref.c`
    /// passes. Gates the *first* warning only, so `git update-ref refs/heads/z
    /// <40-hex-ref>` is silent while `git update-ref refs/heads/z dup` still warns.
    pub skip_ambiguity_check: bool,
}

/// `get_oid_basic()`'s two ambiguity warnings, in the order and under the gates
/// the C applies them, for a caller holding git's own `flags`.
///
/// The first branch is the full-hex one documented on
/// [`warn_ambiguous_refname`]; it ends in `return 0`, so a name that takes it
/// never reaches the second. The second is `object-name.c:750-756`, after the
/// name has resolved as a ref:
///
/// ```c
/// if (!refs_found)
///         return -1;
///
/// if (repo_settings_get_warn_ambiguous_refs(r) && !(flags & GET_OID_QUIETLY) &&
///     (refs_found > 1 ||
///      !get_short_oid(r, str, len, &tmp_oid, GET_OID_QUIETLY)))
///         warning(warn_msg, len, str);
/// ```
///
/// Three differences from the first branch, each separately observable and each
/// measured against stock 2.55.0 before being written down:
///
/// * **`warn_on_object_refname_ambiguity` is not a gate here.** The switch is
///   read only inside the full-hex branch, so the four bulk readers that clear it
///   ([`AmbiguityWarnings`]) still warn about an ambiguous *plain name*: stock
///   `printf dup | git rev-list --stdin`, `… | git cat-file --batch-check`,
///   `… | git pack-objects --revs --stdout` and `… | git bundle create f --stdin`
///   each print the line once.
/// * **`GET_OID_SKIP_AMBIGUITY_CHECK` is not a gate here either**, so
///   `git update-ref refs/heads/z dup` warns although the same command is silent
///   for a 40-hex ref name.
/// * **`GET_OID_QUIETLY` *is* a gate**, which the first branch has no test for —
///   `git rev-parse --quiet --verify dup` is silent while `git rev-parse
///   --verify dup` warns.
///
/// `refs_found` is `repo_dwim_ref()`'s count, so it is the number of distinct
/// `ref_rev_parse_rules` spellings that exist, not the number of objects they
/// point at: `refs/heads/dup` and `refs/tags/dup` is two even when both name the
/// same commit. The right-hand disjunct catches the other shape entirely — a
/// *single* ref whose name is also an unambiguous abbreviated object id, which is
/// why a branch called `ca6882fa` warns on its own.
///
/// The name in the message is [`ambiguity_base`]'s, not the operand's, for the
/// same reason as in the first branch: git warns about the `str`/`len` that
/// reached `get_oid_basic()`, so `git rev-parse dup^` and `git rev-parse
/// dup:f.txt` both say `refname 'dup' is ambiguous.` once.
pub fn warn_ambiguous_operand(repo: &gix::Repository, name: &str, flags: OidFlags) -> bool {
    let base = ambiguity_base(name);
    if base.len() == repo.object_hash().len_in_hex()
        && base.bytes().all(|b| b.is_ascii_hexdigit())
    {
        if flags.skip_ambiguity_check
            || !WARN_ON_OBJECT_REFNAME_AMBIGUITY.load(Ordering::Relaxed)
        {
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
        return true;
    }
    if flags.quiet || repo.config_snapshot().boolean("core.warnAmbiguousRefs") == Some(false) {
        return false;
    }
    // A reflog operand is counted by a different function and measured over a
    // different name. `get_oid_basic()` cuts the selector off (`len = at`) before
    // it reaches the warning, and the count it tests is `repo_dwim_log()`'s — how
    // many rules found a *log*, not how many found a ref:
    //
    // ```c
    // else if (reflog_len)
    //         refs_found = repo_dwim_log(r, str, len, oid, &real_ref);
    // …
    // if (repo_settings_get_warn_ambiguous_refs(r) && !(flags & GET_OID_QUIETLY) &&
    //     (refs_found > 1 || !get_short_oid(r, str, len, &tmp_oid, GET_OID_QUIETLY)))
    //         warning(warn_msg, len, str);
    // ```
    //
    // So `git reflog show tri@{0}` warns about `tri` when both `refs/heads/tri` and
    // `refs/remotes/tri/HEAD` have logs, while `dup@{0}` — one log, one tag without
    // one — does not.
    if let Some((reflog_base, _)) = split_reflog_selector(base) {
        if reflog_base.is_empty() {
            return false;
        }
        let logs_found = dwim_log_matches(repo, reflog_base);
        if logs_found == 0 {
            return false;
        }
        if logs_found > 1 || short_oid_unambiguous(repo, reflog_base) {
            eprintln!("warning: refname '{reflog_base}' is ambiguous.");
        }
        return false;
    }
    // `if (!refs_found) return -1;` — a name no rule matches never gets this far,
    // and that covers the revision grammar `strip_navigation` cannot reduce
    // (`<rev>^!`, `<rev>^@`) without a test of its own: `check_refname_format()`
    // bans `^`, `~`, `:` and `?` in a refname, so `repo_dwim_ref()` answers 0 for
    // anything still carrying one.
    let refs_found = crate::porcelain::rev_parse::dwim_ref_matches(repo, base).len();
    if refs_found == 0 {
        return false;
    }
    if refs_found > 1 || short_oid_unambiguous(repo, base) {
        eprintln!("warning: refname '{base}' is ambiguous.");
    }
    false
}

/// `repo_peel_to_type()`'s `error()` (`object-name.c:897-903`) for a `^{<type>}`
/// operand that cannot be peeled that far, or `None` when the operand peels or is
/// not a peel at all.
///
/// ```c
/// while (1) {
///         if (!o || (!o->parsed && !parse_object(r, &o->oid)))
///                 return NULL;
///         if (expected_type == OBJ_ANY || o->type == expected_type)
///                 return o;
///         if (o->type == OBJ_TAG)
///                 o = ((struct tag*) o)->tagged;
///         else if (o->type == OBJ_COMMIT)
///                 o = &(repo_get_commit_tree(r, ((struct commit *)o))->object);
///         else {
///                 if (name)
///                         error("%.*s: expected %s type, but the object "
///                               "dereferences to %s type",
///                               namelen, name, type_name(expected_type),
///                               type_name(o->type));
///                 return NULL;
///         }
/// }
/// ```
///
/// The dereference chain always ends at a tree — a tag peels to its target, a
/// commit peels to its tree, and a tree has nowhere left to go — so *every*
/// unreachable type reports `dereferences to tree type`, whatever the operand
/// named. Stock 2.55.0: `main^{blob}`, `v1.0^{blob}` and `lightweight^{tag}` all
/// print exactly that.
///
/// The name in the message is `peel_onion()`'s whole `name`/`len`, i.e. the
/// operand *including* its `^{…}` suffix, not the base.
///
/// Callers emit this once per resolution — *not* once per failed resolution.
/// `error()` is raised while `get_oid_1()` is still running, and `get_oid_1()`
/// carries on afterwards: it falls back to `get_oid_basic()` on the whole name
/// (`object-name.c:1128-1132`), which for a reflog operand can still answer. So
/// stock `git rev-parse 'HEAD@{<old date>}^{blob}'` prints the `error:` line and
/// then exits 0 with an id on stdout. [`resolve`] is where the emission belongs
/// for the same reason the reflog diagnostics belong there.
///
/// git resolves a *failing* operand twice on the commands that end in
/// `die_verify_filename()`, which is why stock `git rev-parse main^{blob}` prints
/// the line twice while `git cat-file -t main^{blob}` — no `verify_filename()` —
/// prints it once.
///
/// `name` is the operand as written, because the frame that reports is not
/// generally the operand's own: `peel_onion()` runs at every step of the
/// reduction that has a `^{<type>}` to cut, and the suffixes above it are cut
/// first. Stock 2.55.0 answers `HEAD^{blob}^`, `HEAD^{blob}~1` and
/// `HEAD^{blob}:f` with `error: HEAD^{blob}: …` — the *reduced* name — so the
/// walk below is `object_part()` followed by [`navigation_step`], asking
/// [`peel_onion_error`] at each frame and taking the first answer.
///
/// Outside-in is the right order even though the C recurses inside-out: an outer
/// frame whose inner peel failed cannot report, because `get_oid_1()` returned
/// non-zero and `peel_onion()` bails at `object-name.c:959-960` before
/// `repo_peel_to_type()`. Measured both ways round against stock 2.55.0 —
/// `HEAD^{tree}^{blob}` reports the whole operand, `HEAD^{blob}^{tree}` reports
/// only `HEAD^{blob}`.
pub fn peel_type_error(repo: &gix::Repository, name: &str) -> Option<String> {
    // `get_oid_with_context_1()` cuts the path arm before `get_oid_1()` is
    // entered at all, so a `<rev>:<path>` never reaches `peel_onion()` whole.
    let mut base = object_part(name);
    loop {
        if let Some(message) = peel_onion_error(repo, base) {
            return Some(message);
        }
        base = navigation_step(base)?.0;
    }
}

/// [`peel_type_error`] for one `peel_onion()` frame: the `error()` it raises for
/// the name it was handed, with no reduction of its own.
fn peel_onion_error(repo: &gix::Repository, name: &str) -> Option<String> {
    // `if (len < 4 || name[len-1] != '}') return -1;`
    if name.len() < 4 || !name.ends_with('}') {
        return None;
    }
    // `for (sp = name + len - 1; name <= sp; sp--) if (ch == '{' && name < sp && sp[-1] == '^') break;`
    let bytes = name.as_bytes();
    let open = (1..bytes.len()).rev().find(|&i| bytes[i] == b'{' && bytes[i - 1] == b'^')?;
    let sp = &name[open + 1..];
    let want = if sp.starts_with("commit}") {
        "commit"
    } else if sp.starts_with("tag}") {
        "tag"
    } else if sp.starts_with("tree}") {
        "tree"
    } else if sp.starts_with("blob}") {
        "blob"
    } else {
        // `^{object}` is OBJ_ANY, `^{}` peels tags only, `^{/<text>}` is a
        // committish search — none of the three can reach the `error()`.
        return None;
    };
    let base = &name[..open - 1];
    // `peel_onion()` resolves its base with `get_oid_1()`, which has no case for
    // the `<rev>:<path>` / `:<n>:<path>` grammar — that lives one level up, in
    // `get_oid_with_context_1()`. So `main:base.txt^{tree}` never reaches
    // `repo_peel_to_type()` at all: the base fails to resolve and the whole name
    // falls through to the path arm, where stock reports
    // `path 'base.txt^{tree}' does not exist in 'main'` with no `error:` line.
    if !matches!(crate::objpath::split(base), crate::objpath::Split::Rev) {
        return None;
    }
    let mut id = resolve_quiet(repo, base)?;
    loop {
        let object = repo.find_object(id).ok()?;
        let kind = match object.kind {
            gix::object::Kind::Commit => "commit",
            gix::object::Kind::Tag => "tag",
            gix::object::Kind::Tree => "tree",
            gix::object::Kind::Blob => "blob",
        };
        if kind == want {
            return None;
        }
        match object.kind {
            gix::object::Kind::Tag => id = object.into_tag().decode().ok()?.target(),
            gix::object::Kind::Commit => id = object.into_commit().tree_id().ok()?.detach(),
            _ => {
                return Some(format!(
                    "{name}: expected {want} type, but the object dereferences to {kind} type"
                ))
            }
        }
    }
}

/// `!get_short_oid(r, str, len, &tmp_oid, GET_OID_QUIETLY)`: whether the name is
/// also a hex prefix that picks out exactly one object.
///
/// git's floor is `minimum_abbrev`, four hex digits (`environment.c`), and an
/// ambiguous prefix makes `get_short_oid()` fail — which reads here as "not a
/// short oid", exactly as it does in the C.
fn short_oid_unambiguous(repo: &gix::Repository, name: &str) -> bool {
    if name.len() < 4
        || name.len() > repo.object_hash().len_in_hex()
        || !name.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(prefix) = gix::hash::Prefix::from_hex(name) else {
        return false;
    };
    // No candidate set: git only needs "does this name exactly one object", and
    // the early-abort form answers that — `Ok(Some(Err(())))` is the ambiguous
    // case, which `get_short_oid` also treats as a failure to resolve.
    matches!(repo.objects.lookup_prefix(prefix, None), Ok(Some(Ok(_))))
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
///
/// The exclusion mark is **not** stripped, and that is the whole distinction
/// between the two functions that read an object name. `arg++` past a leading `^`
/// belongs to `handle_revision_arg_1()` (`revision.c`), which only a *revision
/// walk* goes through:
///
/// ```c
/// if (*arg == '^') {
///         local_flags = UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// ```
///
/// `repo_get_oid()` has no such line, so a command that reaches it directly hands
/// `get_oid_basic()` the caret as well — one character too many for the full-hex
/// branch, and therefore silent. Stock 2.55.0 says nothing for
/// `git cat-file -t ^<40-hex-ref>`, `git name-rev ^<40-hex-ref>` or
/// `git cherry HEAD ^<40-hex-ref>` while `git rev-list ^<40-hex-ref>` warns once.
/// Stripping here made every one of those commands over-warn.
///
/// A walker therefore does the strip itself, at the point where the C does, and
/// asks about what is left — [`uninteresting_mark`] is that strip.
///
/// The `<rev>:<path>` cut is [`object_part`]'s bracket-aware one and **not** a
/// plain `strchr(name, ':')`, because that is the only scan `repo_get_oid()` ever
/// runs: `get_oid_with_context_1()` counts `@{`/`^{` groups so a colon inside one
/// is part of the group, not the path separator (`object-name.c:1821-1830`). A
/// plain `strchr` cut `HEAD@{2005-01-01T00:00:00+0000}` at the first colon of the
/// clock time and handed the reflog readers `HEAD@{2005-01-01T00` — no trailing
/// `}`, so not a reflog operand at all, so silent where stock 2.55.0 warns
/// `log for 'HEAD' only goes back to …`. Every `@{<date>}` spelling carrying a
/// time was affected, and so was `<40-hex-ref>^{/a:b}`, which stock warns about
/// twice and this said nothing about.
fn ambiguity_base(spec: &str) -> &str {
    strip_navigation(object_part(spec))
}

/// [`ambiguity_base`] for the two commands that hold `get_oid_basic()`'s flags
/// themselves and so reach the diagnostics directly rather than through
/// [`resolve_with_flags`].
pub fn ambiguity_base_of(spec: &str) -> &str {
    ambiguity_base(spec)
}

/// The suffix reduction `get_oid_1()` and `peel_onion()` run between them, taken
/// to a fixed point: the name `get_oid_basic()` is finally handed.
///
/// Shared by [`ambiguity_base`], which needs it to name the right thing in a
/// warning, and by [`get_oid_1_has_no_case`], which needs it to decide whether
/// git would have resolved the operand at all. The two differ only in what they
/// hand it: `ambiguity_base` reproduces `handle_revision_arg_1()`'s plain
/// `strchr(name, ':')` while [`object_part`] reproduces the bracket-aware split
/// `get_oid_with_context_1()` actually uses.
///
/// A `^{…}` group is only cut when [`peel_onion_type`] recognises what is inside
/// it. `peel_onion()` returns -1 for anything else, and `get_oid_1()` then hands
/// `get_oid_basic()` the operand **whole** — so `<40-hex>^{bogus}` and
/// `<40-hex>^{{commit}}` are measured at their full length, miss the full-hex
/// branch and are silent in stock 2.55.0. Cutting the group unconditionally made
/// them warn.
fn strip_navigation(base: &str) -> &str {
    navigation(base).0
}

/// [`strip_navigation`] together with the one thing the reduction's *shape*
/// decides: whether it went through `get_parent()`/`get_nth_ancestor()`.
///
/// Those two are the only steps that do not pass `lookup_flags` down —
/// they hand the recursion a literal `GET_OID_COMMITTISH`:
///
/// ```c
/// static enum get_oid_result get_parent(struct repository *r,
///                                       const char *name, int len,
///                                       struct object_id *result, int idx)
/// {
///         struct object_id oid;
///         enum get_oid_result ret = get_oid_1(r, name, len, &oid,
///                                             GET_OID_COMMITTISH);
/// ```
///
/// (`object-name.c:828-834`, and `get_nth_ancestor()` at `object-name.c:858-867`
/// does the same.) `peel_onion()` keeps the caller's flags — it only clears
/// `GET_OID_DISAMBIGUATORS` — and `get_oid_with_context_1()`'s `<rev>:<path>`
/// arm keeps them too, so a `~<n>`/`^<n>` anywhere in the reduction is what
/// **loses `GET_OID_QUIETLY`** before `get_oid_basic()` is reached.
///
/// That is directly observable, and it cuts both ways:
///
/// * `git rev-parse --quiet --verify 'HEAD@{<old date>}^'` prints
///   `warning: log for 'HEAD' only goes back to …` in stock 2.55.0 although
///   `--quiet` set `GET_OID_QUIETLY`, because `get_parent()` dropped it;
/// * `git rev-parse --quiet --verify 'HEAD@{<old date>}'` is silent, because
///   nothing dropped it;
/// * on the failure path, where `die_verify_filename()` resolves a second time
///   with `GET_OID_ONLY_TO_DIE | GET_OID_QUIETLY`, the warning comes out twice
///   for `HEAD@{<old date>}^` and `HEAD@{<old date>}~99` and once for
///   `HEAD@{<old date>}:nosuch`.
fn navigation(mut base: &str) -> (&str, bool) {
    let mut drops_quiet = false;
    while let Some((rest, dropped)) = navigation_step(base) {
        base = rest;
        drops_quiet |= dropped;
    }
    (base, drops_quiet)
}

/// One turn of [`navigation`]'s loop: the name the *next* `get_oid_1()` frame is
/// handed, and whether that frame is one of the two that drop `GET_OID_QUIETLY`.
///
/// `None` is the fixed point — neither `peel_onion()` nor `get_oid_1()`'s
/// `~<n>`/`^<n>` block has anything left to cut, so `get_oid_basic()` is next.
///
/// Split out of [`navigation`] because the reduction is not only a way of
/// arriving at a name: `peel_onion()` runs, and can `error()`, at *every* step
/// that has a `^{<type>}` to cut. [`peel_type_error`] walks the same chain to
/// find the one frame that reports.
fn navigation_step(base: &str) -> Option<(&str, bool)> {
    // `peel_onion()`: at least four characters, ending in `}`, and the
    // *rightmost* `{` whose predecessor is `^` opens the type name.
    if base.len() >= 4 && base.ends_with('}') {
        if let Some(at) =
            base.rfind("^{").filter(|at| *at > 0 && peel_onion_type(&base[at + 2..]))
        {
            return Some((&base[..at], false));
        }
    }
    // `get_oid_1()`: trailing digits, then one `~` or `^`, then recurse on
    // what precedes it.
    let head = base.trim_end_matches(|c: char| c.is_ascii_digit());
    head.strip_suffix(['~', '^']).filter(|rest| !rest.is_empty()).map(|rest| (rest, true))
}

/// Whether resolving `spec` reaches `get_oid_basic()` with `GET_OID_QUIETLY`
/// cleared even when the caller set it — see [`navigation`].
///
/// The `<rev>:<path>` cut runs first and keeps the flag, so this asks
/// [`navigation`] about the object half only.
pub fn quiet_lost_in_navigation(spec: &str) -> bool {
    navigation(object_part(spec)).1
}

/// `peel_onion()`'s type-name table (`object-name.c:936-951`), asked of the text
/// that follows `^{`:
///
/// ```c
/// sp++; /* beginning of type name, or closing brace for empty */
/// if (starts_with(sp, "commit}"))       expected_type = OBJ_COMMIT;
/// else if (starts_with(sp, "tag}"))     expected_type = OBJ_TAG;
/// else if (starts_with(sp, "tree}"))    expected_type = OBJ_TREE;
/// else if (starts_with(sp, "blob}"))    expected_type = OBJ_BLOB;
/// else if (starts_with(sp, "object}"))  expected_type = OBJ_ANY;
/// else if (sp[0] == '}')                expected_type = OBJ_NONE;
/// else if (sp[0] == '/')                expected_type = OBJ_COMMIT;
/// else return -1;
/// ```
///
/// `starts_with` and not an equality test, which is observable: `^{commit}}`
/// peels while `^{{commit}}` does not, because the backward scan for the opening
/// brace demands a `^` immediately before it and so lands on the outer pair.
fn peel_onion_type(after_brace: &str) -> bool {
    ["commit}", "tag}", "tree}", "blob}", "object}"]
        .iter()
        .any(|kind| after_brace.starts_with(kind))
        || after_brace.starts_with('}')
        || after_brace.starts_with('/')
}

/// The `<object>` half of an operand, as `get_oid_with_context_1()` finds it
/// (`object-name.c:1821-1830`):
///
/// ```c
/// for (cp = name, bracket_depth = 0; *cp; cp++) {
///         if (strchr("@^", *cp) && cp[1] == '{') {
///                 cp++;
///                 bracket_depth++;
///         } else if (bracket_depth && *cp == '}') {
///                 bracket_depth--;
///         } else if (!bracket_depth && *cp == ':') {
///                 break;
///         }
/// }
/// ```
///
/// The scan counts braces, so the `:` in `<rev>^{/a:b}` is part of the search
/// pattern and only an unbracketed one ends the revision — which is the one place
/// this differs from [`ambiguity_base`]'s plain `strchr`.
///
/// A leading `:` yields the empty string: `:<path>`, `:<n>:<path>` and
/// `:/<text>` are handled by `get_oid_with_context_1()` itself (the `name[0] ==
/// ':'` branch at `object-name.c:1758`), so `get_oid_1()` decides nothing about
/// them and the caret rule below must not either — `git rev-parse :/^two`
/// resolves in stock 2.55.0 despite the `^`.
fn object_part(name: &str) -> &str {
    let b = name.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b'@' | b'^') && b.get(i + 1) == Some(&b'{') {
            i += 1;
            depth += 1;
        } else if depth > 0 && b[i] == b'}' {
            depth -= 1;
        } else if depth == 0 && b[i] == b':' {
            return &name[..i];
        }
        i += 1;
    }
    name
}

/// Whether `get_oid_1()` (`object-name.c:1084-1142`) has no case for `spec` at
/// all, so `repo_get_oid()` fails on it however the repository is shaped.
///
/// `get_oid_1()` reads a `^` in exactly two ways and no others:
///
/// ```c
/// for (cp = name + len - 1; name <= cp; cp--) {
///         int ch = *cp;
///         if ('0' <= ch && ch <= '9')
///                 continue;
///         if (ch == '~' || ch == '^')
///                 has_suffix = ch;
///         break;
/// }
/// …
/// ret = peel_onion(r, name, len, oid, lookup_flags);
/// ```
///
/// — a trailing `~<n>`/`^<n>` it strips before recursing, and a `^{…}` group
/// `peel_onion()` cuts. [`strip_navigation`] takes both to a fixed point, and a
/// `^` that survives it cannot be resolved by anything `get_oid_1()` has left:
/// `get_oid_basic()`'s full-hex branch and `get_short_oid()` both want hex
/// digits, `get_describe_name()` wants a `-g<hex>` tail, and `repo_dwim_ref()`
/// cannot match because `check_refname_format()` bans `^` in a refname
/// (`refname_disposition[0x5e] == 4`, `refs.c:80-89`).
///
/// The three spellings this actually rejects are the *revision-walk* marks
/// `<rev>^!`, `<rev>^@` and `<rev>^-<n>`, which `handle_revision_arg_1()`
/// (`revision.c`) strips before it ever calls `repo_get_oid_with_context()` —
/// see [`parents_only_base`]. A command that reaches `repo_get_oid()` directly
/// never sees that strip, so stock 2.55.0 answers `fatal: Not a valid object
/// name HEAD^!` for `git cat-file -t HEAD^!` while `git rev-list HEAD^!` walks
/// the range. gitoxide draws no such line: its parser returns
/// `Spec::ExcludeParents` for `<rev>^!`, and `gix::revision::Spec::single()`
/// hands that back as an ordinary single object
/// (`src/ported/gix/src/revision/spec/mod.rs:87-97`), so every command resolving
/// an argv operand through [`resolve`] used to accept a spelling git refuses.
fn get_oid_1_has_no_case(spec: &str) -> bool {
    strip_navigation(object_part(spec)).contains('^')
}

/// [`get_oid_1_has_no_case`] for callers that resolve a name without going
/// through [`resolve_quiet`].
///
/// `builtin/rev-parse.c` is the one such caller: it reaches
/// `repo_get_oid_with_flags()` for a plain operand and `repo_get_oid_committish()`
/// for each endpoint of a range, and both land in the same `get_oid_1()` with no
/// case for `^!`, `^@` or `^-<n>`. Without this test gitoxide's wider grammar
/// answers where git refuses — `git rev-parse main..side^!` is
/// `ambiguous argument` in stock 2.55.0.
pub fn has_walk_mark(spec: &str) -> bool {
    get_oid_1_has_no_case(spec)
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
    resolve_with_flags(repo, spec, OidFlags::default())
}

/// [`resolve`] for the one command that reaches `repo_get_oid_with_flags()` with
/// a bit set: `builtin/update-ref.c`, which passes `GET_OID_SKIP_AMBIGUITY_CHECK`
/// for both of its value slots.
///
/// Only the two ambiguity warnings answer to `flags` here. `GET_OID_QUIETLY` is
/// deliberately not among them: the sole caller that sets it is
/// `builtin/rev-parse.c`, which does not come through this function at all — it
/// holds git's `flags` itself and reaches [`warn_ambiguous_operand`],
/// [`reflog_reach`] and [`read_ref_at_warning`] directly, because `--quiet`
/// changes not only what it prints but which of the two `die()` spellings it
/// ends on.
pub fn resolve_with_flags(
    repo: &gix::Repository,
    spec: &str,
    flags: OidFlags,
) -> Option<ObjectId> {
    // `repo_dwim_ref()` calls `substitute_branch_name()` before it expands
    // anything (`refs.c:795-803`), and `interpret_branch_mark()`'s `die()` fires
    // from in there — so an `@{u}`/`@{push}` mark that names no upstream ends the
    // command *inside* `get_oid_basic()` (`object-name.c:748`), ahead of the
    // ambiguity warning below it and ahead of every caller's own "not a valid
    // object name". Bolting it onto `rev-parse` and `log` alone left `cat-file`
    // (argv and `--batch`), `merge-base`, `branch --contains`, `diff`, `ls-tree`
    // and the rest answering `missing`/`Not a valid object name` where stock
    // 2.55.0 dies.
    if let Some(message) = upstream_mark_fatal(repo, spec) {
        eprintln!("fatal: {message}");
        std::process::exit(crate::fatal::EXIT_FATAL as i32);
    }
    // `repo_get_oid()` reaches `get_oid_basic()` once per operand, so this is
    // where the ambiguity warning belongs — not in `full_hex`, which the helpers
    // below call a second time to *diagnose* a name this has already resolved.
    warn_ambiguous_operand(repo, spec, flags);
    // The same argument for `read_ref_at()`'s warning, which `get_oid_basic()`
    // raises further down the very same call (`object-name.c:787`, after the
    // ambiguity check at `object-name.c:753-756` — hence this order).
    reflog_diagnostics(repo, ambiguity_base(spec));
    // `peel_onion()`'s `error()` (`object-name.c:897-903`), raised from inside
    // `get_oid_1()` and therefore before `get_oid_basic()` ever answers.
    if let Some(message) = peel_type_error(repo, spec) {
        eprintln!("error: {message}");
        // `peel_onion()` returned -1, so `get_oid_1()` hands `get_oid_basic()`
        // the **whole** name (`object-name.c:1128-1132`) — a second, differently
        // spelled trip through the reflog branch. `approxidate_careful()` does
        // not reject `2005-01-01}^{blob` as a selector (it sets `*error_ret`
        // only when nothing in the string was `isdigit()` or `isalpha()`,
        // `date.c:1409-1410`), which is how stock 2.55.0 prints
        // `log for 'HEAD' only goes back to …` twice for
        // `HEAD@{<old date>}^{blob}` and answers with the oldest entry.
        reflog_diagnostics(repo, spec);
    }
    let resolved = resolve_quiet(repo, spec);
    if resolved.is_none() {
        // `get_short_oid()` is the last thing `get_oid_1()` tries
        // (`object-name.c:1134`), so its ambiguity report belongs after every
        // other diagnostic and only for a name nothing else answered.
        short_oid_ambiguous(repo, ambiguity_base(spec), false);
    }
    resolved
}

/// Everything `get_oid_basic()`'s reflog branch says about one operand
/// (`object-name.c:787-820`), printed where the C prints it — including the
/// `die()`, which ends the process here exactly as it ends it there.
///
/// **This is the routing point.** git raises all three of these from inside
/// `get_oid_basic()`, so every command that resolves an argv operand through
/// `repo_get_oid()` gets them, and every command that does not resolve one stays
/// silent. [`resolve`] is this port's `repo_get_oid()`, which is why they belong
/// here rather than in a list of verbs: bolting the warning onto `rev-parse` and
/// `log` left `cat-file` (argv and `--batch`), `merge-base`,
/// `merge-base --is-ancestor`, `branch --contains`, `branch --merged`,
/// `tag --contains`, `for-each-ref --contains`, `diff`, `name-rev`,
/// `describe --always`, `ls-tree`, `archive`, `grep`, `blame`, `commit-tree`,
/// `ls-files --with-tree`, `read-tree`, `show-branch`, `cherry`, `bundle`,
/// `notes`, `verify-tag` and `tag <name> <rev>` silent where stock 2.55.0
/// speaks — every one of them measured, and a list nobody would have finished by
/// hand.
///
/// The four verbs that used to resolve their operands themselves now come
/// through here too — `diff-tree` at its positional classifier, `update-ref` at
/// each value slot (through [`resolve_with_flags`], for its
/// `GET_OID_SKIP_AMBIGUITY_CHECK`), `merge-tree` at all three of
/// `get_merge_parent()`/`repo_get_oid_treeish()`/`get_tree_descriptor()` — and
/// `checkout`/`switch` reach it *twice*, because
/// `setup_new_branch_info_and_source_tree()` resolves the operand again through
/// `setup_branch_path()` (`builtin/checkout.c:804-806,1311,1476`).
///
/// `range-diff` is the one that still hears less than stock: it resolves each
/// positional once where `cmd_range_diff()` resolves `argv[0]` twice (a guard at
/// `builtin/range-diff.c:106-109` and a validation at
/// `builtin/range-diff.c:112-120`) and then walks the two `<argv0>..<argvN>`
/// ranges it builds, so stock 2.55.0 prints the reach warning three times for
/// `git range-diff 'HEAD@{<old date>}' HEAD~1 HEAD` and twice for the one- and
/// two-argument spellings, against one and none here.
///
/// The `die()` is not a value some caller might forget to render:
///
/// ```c
/// if (flags & GET_OID_QUIETLY)
///         exit(128);
/// else
///         die(_("log for %s is empty"), refname);
/// ```
///
/// (`refs.c:1207-1210`, and `object-name.c:810-815` for the sibling `only has %d
/// entries`.) It fires below `repo_get_oid()`'s only return path, so a caller
/// cannot see it and cannot decline it — `git cat-file -t HEAD@{1}` and `git
/// merge-base HEAD@{1} HEAD` on an empty log both end at `fatal: log for HEAD is
/// empty`, not at each verb's own "not a valid object name". Reproducing that
/// with a return value would mean teaching all 66 call sites the same lesson;
/// exiting here is what the C does and is the only spelling that cannot drift.
///
/// The two commands that hold `get_oid_basic()`'s `flags` themselves —
/// `builtin/rev-parse.c`, which passes `GET_OID_QUIETLY` for `--quiet`, and the
/// revision walk, which diagnoses an endpoint before it resolves — do not come
/// through here; they reach [`reflog_reach`] and [`read_ref_at_warning`]
/// directly, with the gate they are entitled to.
fn reflog_diagnostics(repo: &gix::Repository, name: &str) {
    // `name` is what `get_oid_basic()` was handed, not the operand: the caller
    // reduces. [`resolve_with_flags`] calls this twice for one operand because
    // `get_oid_1()` calls `get_oid_basic()` twice when `peel_onion()` fails, and
    // the two calls do not agree on the name.
    //
    // `read_ref_at()`'s own `warning()` comes from one frame deeper than the
    // block below it, so it is printed first (`refs.c:1135`, `refs.c:1141`).
    if let Some(message) = read_ref_at_warning_of(repo, name) {
        eprintln!("warning: {message}");
    }
    match reflog_reach_of(repo, name) {
        Some(ReflogReach::Warning(message)) => eprintln!("warning: {message}"),
        Some(ReflogReach::Fatal(message)) => {
            eprintln!("fatal: {message}");
            // `die()` leaves through `exit()`, which flushes the stdio buffer —
            // `cstdio`'s `atexit` handler is registered for exactly this.
            std::process::exit(crate::fatal::EXIT_FATAL as i32);
        }
        None => {}
    }
}

/// [`resolve`] without `get_oid_basic()`'s ambiguity warning, for the callers
/// that are re-examining a name rather than resolving it for the first time.
///
/// git warns once per operand because it resolves once per operand; a zvcs
/// classifier that answers "is this a range?" and is then asked "what range?"
/// would otherwise warn twice for one argument.
///
/// The other caller is a command that has already reached
/// [`warn_ambiguous_refname`] itself because it warns on operand *shapes* this
/// function never resolves — `reflog show <ref>@{<n>}` warns about the ref and
/// then reads a log rather than an object, so the warning cannot be bolted onto
/// the resolution the way [`resolve`] bolts it on.
pub fn resolve_quiet(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    // `get_oid_1()`'s dispatch comes first, and it is a *narrower* grammar than
    // gitoxide's rev-parse: the `^!`/`^@`/`^-<n>` marks belong to
    // `handle_revision_arg_1()` and never reach `repo_get_oid()`.
    if get_oid_1_has_no_case(spec) {
        return None;
    }
    if let Some(id) = full_hex(repo, spec) {
        return Some(id);
    }
    // The rule survives the suffixes. `get_oid_1()` recurses down to
    // [`ambiguity_base`] before anything else happens, so a full-length hex there
    // takes `get_oid_basic()`'s first branch and the ref store is never consulted
    // — and the suffix operation then works on the object itself:
    //
    // ```c
    // static int peel_onion(struct repository *r, const char *name, int len,
    //                       struct object_id *oid, unsigned lookup_flags)
    // {
    //         …
    //         if (get_oid_1(r, name, sp - name - 2, &outer, lookup_flags))
    //                 return -1;
    //         o = parse_object(r, &outer);
    //         if (!o)
    //                 return -1;
    // ```
    //
    // So `<absent-40-hex>^{commit}`, `<absent-40-hex>~1` and
    // `<absent-40-hex>:<path>` all fail outright. gitoxide instead looks the id
    // up, misses, and falls back to a *ref* of that name — which in a repository
    // holding `refs/heads/<40-hex>` resolves the operand to that ref's history
    // where git reports the name as unknown.
    if full_hex(repo, ambiguity_base(spec)).is_some_and(|id| repo.find_object(id).is_err()) {
        return None;
    }
    // `get_oid_basic()` dispatches on the operand's *shape*. Once a reflog
    // selector has been cut off the end, the ref half is looked up with
    // `repo_dwim_log()` and nothing else — so a reflog operand never reaches
    // gitoxide's rev-spec parser. That matters in both directions: gitoxide gives
    // up on an ambiguous `dup@{0}` that git answers, and it answers `HEAD@{0}` off
    // a stale `logs/HEAD` that git refuses because `HEAD` resolves to nothing.
    if resolves_through_reflog(spec) {
        return reflog_spec_oid(repo, spec);
    }
    repo.rev_parse_single(canonical_spec(repo, spec).as_ref()).ok().map(|id| id.detach())
}


/// Whether the reduction `repo_get_oid()` performs lands `get_oid_basic()` on a
/// reflog operand, so that [`reflog_spec_oid`] and not gitoxide's revspec grammar
/// is what answers for `spec`.
///
/// Two tests, because `get_oid_1()` asks twice: [`ambiguity_base`] is the name the
/// reduction ends at, and the whole `spec` is what `get_oid_1()` falls back to
/// once `peel_onion()` has given up (`object-name.c:1128-1132`).
pub fn resolves_through_reflog(spec: &str) -> bool {
    is_reflog_operand(spec) || is_reflog_operand(ambiguity_base(spec))
}

/// [`reflog_oid`] reached the way `repo_get_oid()` reaches it: through the
/// reduction, not off the operand as typed.
///
/// The reduction runs *before* `get_oid_basic()` ever sees the name, and the
/// suffix then works on the object that came back — `peel_onion()` resolves
/// `sp - name - 2` characters and peels the result (`object-name.c:959-962`),
/// `get_parent()` and `get_nth_ancestor()` resolve `len1` and walk from there
/// (`object-name.c:828-834` and `object-name.c:858-867`), and `get_oid_with_context_1()`'s `<rev>:<path>`
/// arm resolves the left half and looks the path up in that tree
/// (`object-name.c:1833-1841`). So a reflog operand carrying a suffix is *not* a
/// reflog operand to `get_oid_basic()`: [`ambiguity_base`] is the name it is
/// finally handed, and only that name goes to the reader.
///
/// Substituting the resolved id back into the operand is what keeps the three
/// suffix families on one path rather than on three special cases. Gating on the
/// whole spec instead routed `HEAD@{<n>}^{commit}` and `HEAD@{<n>}^{tree}` into
/// the reader with `<n>}^{commit` as the *selector* — which
/// `approxidate_careful()` reads as a date rather than rejecting, so both
/// answered the newest entry instead of peeling — and it left
/// `HEAD@{<n>}~<n>` and `HEAD@{<n>}:<path>`, neither of which ends in `}`, to
/// gitoxide's own reflog reader, which has no [`read_ref_at`] and so failed
/// outright on the operands a `git branch -m` round trip produces.
///
/// The whole-spec attempt comes *last* for the same reason: `get_oid_1()` only
/// falls back to `get_oid_basic()` on the full name once `peel_onion()` has
/// returned -1, which is how stock answers `HEAD@{<old date>}^{blob}` at all
/// despite the peel being impossible.
pub fn reflog_spec_oid(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let base = ambiguity_base(spec);
    if base.len() < spec.len() && is_reflog_operand(base) {
        if let Some(id) = reflog_oid(repo, base) {
            let rewritten = format!("{}{}", id.to_hex(), &spec[base.len()..]);
            if let Some(id) = resolve_quiet(repo, &rewritten) {
                return Some(id);
            }
        }
    }
    if is_reflog_operand(spec) {
        return reflog_oid(repo, spec);
    }
    None
}

/// The operand as gitoxide's revspec parser needs to see it: `@{u}`-family marks
/// case-folded ([`canonical_at_marks`]) and any `./`/`../` path arm rewritten
/// root-relative ([`crate::objpath::canonical_paths`]).
///
/// Both are rewrites git performs *inside* `get_oid()` — `at_mark()` compares
/// case-insensitively and `resolve_relative_path()` runs before the index or the
/// tree is consulted — so every site that hands a raw argv operand to
/// `rev_parse_single` needs this first. A path arm that climbs out of the work
/// tree is left as written; it then fails to resolve, and
/// [`crate::objpath::misspelt_object_name`] supplies git's `is outside
/// repository` message.
pub fn canonical_spec<'a>(
    repo: &gix::Repository,
    spec: &'a str,
) -> std::borrow::Cow<'a, str> {
    // `repo_interpret_branch_name()` runs first in `get_oid_basic()`, and its
    // `@{u}` rewrite reaches an upstream gitoxide's parser cannot: with
    // `branch.<name>.remote = .` the upstream is `branch.<name>.merge` itself, a
    // *local* ref, so there is no remote-tracking ref to look up. Stock 2.55.0
    // answers `git rev-parse main@{u}` with the merge ref's id there;
    // `rev_parse_single()` refuses the operand outright.
    //
    // Applying the rewrite unconditionally is what the C does, and it is a no-op
    // for the ordinary case: the parser is handed `refs/remotes/origin/main`
    // instead of `main@{u}` and resolves the same object. The *name* the caller
    // echoes is untouched, so `--abbrev-ref main@{u}` still shortens the operand
    // git's way.
    if let Some(Ok(rewritten)) = interpret_branch_name(repo, spec) {
        return std::borrow::Cow::Owned(
            canonical_spec(repo, &rewritten).into_owned(),
        );
    }
    if let Some(rewritten) = hex_of_other_hash_as_refname(repo, spec) {
        return std::borrow::Cow::Owned(rewritten);
    }
    match crate::objpath::canonical_paths(repo, spec) {
        Ok(std::borrow::Cow::Borrowed(s)) => canonical_at_marks(s),
        Ok(std::borrow::Cow::Owned(s)) => {
            std::borrow::Cow::Owned(canonical_at_marks(&s).into_owned())
        }
        Err(_) => canonical_at_marks(spec),
    }
}

/// `get_oid_hex()` decodes with `the_hash_algo->hexsz` and nothing else
/// (`hex.c:76-84`), so a 64-hex name in a SHA-1 repository — or a 40-hex one in a
/// SHA-256 repository — never takes `get_oid_basic()`'s object-name branch. git
/// carries on to `repo_dwim_ref()` and the name is a candidate *refname*, which is
/// why stock 2.55.0 answers `fatal: Not a valid object name <64-hex>` rather than
/// producing the id.
///
/// gitoxide's rev-spec parser decodes every hash length it knows, and `gix-odb`'s
/// loose store then asserts the id's kind against the repository's
/// (`src/ported/gix-odb/src/store_impls/loose/find.rs:34`) — so handing it such a
/// name aborts the process instead of answering, which is what `cat-file -t`,
/// `-s`, `-p` and `--batch` all did. Rewriting the operand to the full ref name it
/// dwims to keeps the ref case working (the slashes stop gitoxide reading it as
/// hex) and reports the miss as a miss everywhere else.
///
/// `None` leaves the operand to the ordinary rewrites; `Some` is the spelling
/// gitoxide may safely be handed.
fn hex_of_other_hash_as_refname(repo: &gix::Repository, spec: &str) -> Option<String> {
    let base = ambiguity_base(spec);
    if !matches!(base.len(), 40 | 64)
        || base.len() == repo.object_hash().len_in_hex()
        || !base.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let suffix = spec.strip_prefix(base)?;
    // `refs_found`'s order: the first `ref_rev_parse_rules` entry that matches is
    // the ref git resolves the name to.
    let full = crate::porcelain::rev_parse::dwim_ref_matches(repo, base).into_iter().next();
    Some(match full {
        Some(name) => format!("{name}{suffix}"),
        // No ref of that name, so `get_oid_basic()` has nothing left to try. A
        // spelling that cannot decode and cannot exist makes the caller see the
        // miss rather than an abort.
        None => format!("refs/{base}{suffix}"),
    })
}

/// Whether `spec` names an object `handle_commit()` (`revision.c`) declines to
/// turn into a commit — a tree or a blob.
///
/// Both arms pend the object and `return NULL`, so the operand is *claimed*: it
/// contributes no commit, it is not an error, and it still counts towards
/// `revs->pending.nr`. That last part is what keeps `git shortlog main^{tree}`
/// from falling through to the "no revisions given, read stdin" branch.
pub fn names_non_commit(repo: &gix::Repository, spec: &str) -> bool {
    let bare = spec.strip_prefix('^').unwrap_or(spec);
    let Some(id) = resolve_quiet(repo, bare) else {
        return false;
    };
    repo.find_object(id)
        .is_ok_and(|o| matches!(o.kind, gix::object::Kind::Tree | gix::object::Kind::Blob))
}

/// `spec` with every `@{u}`/`@{upstream}`/`@{push}` mark folded to the spelling
/// gitoxide's revspec parser recognises.
///
/// ```c
/// static int at_mark(const char *string, int len, const char **suffix, int nr)
/// {
///         for (int i = 0; i < nr; i++) {
///                 int suffix_len = strlen(suffix[i]);
///                 if (suffix_len <= len && !strncasecmp(string, suffix[i], suffix_len))
///                         return suffix_len;
///         }
///         return 0;
/// }
/// ```
///
/// (`object-name.c:640-663`.) `strncasecmp`, so `main@{U}`, `main@{UpStReAm}` and
/// `main@{PUSH}` are the same operands as their lowercase spellings — all three
/// resolve in stock 2.55.0 and none of them did here. The marks are the same
/// length as their canonical form, so the fold never moves any other offset.
pub fn canonical_at_marks(spec: &str) -> std::borrow::Cow<'_, str> {
    const MARKS: [&str; 3] = ["@{upstream}", "@{u}", "@{push}"];
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < spec.len() {
        if spec.as_bytes()[i] != b'@' {
            i += 1;
            continue;
        }
        let rest = &spec[i..];
        let hit = MARKS
            .iter()
            .find(|m| rest.len() >= m.len() && rest[..m.len()].eq_ignore_ascii_case(m));
        match hit {
            Some(mark) => {
                if rest[..mark.len()] != **mark {
                    let buf = out.get_or_insert_with(|| spec.to_string());
                    buf.replace_range(i..i + mark.len(), mark);
                }
                i += mark.len();
            }
            None => i += 1,
        }
    }
    match out {
        Some(s) => std::borrow::Cow::Owned(s),
        None => std::borrow::Cow::Borrowed(spec),
    }
}

/// Whether `spec` is what `get_oid_basic()` treats as a reflog operand: a
/// `<ref>@{<selector>}` whose selector is neither `@{-<n>}` nor an
/// `@{u}`/`@{upstream}`/`@{push}` mark.
pub fn is_reflog_operand(spec: &str) -> bool {
    split_reflog_selector(spec).is_some()
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
///
/// `strstr()` finds the *first* occurrence and `!mark[2]` then demands that one be
/// the last two characters, so a second copy behind it disqualifies the operand
/// rather than being stripped: `main^!^!` has its first `^!` at index 4 with a `^`
/// after it, so it carries no mark and is resolved whole —
/// `fatal: bad revision 'main^!^!'` against git 2.55.0, where stripping the
/// trailing one would have blamed `main`'s parents instead.
///
/// Public because the mark decides how many times one operand is *resolved*, not
/// just how it is diagnosed: `add_parents_only()` opens with its own
/// `repo_get_oid_committish(arg)`, so a marked operand goes through
/// `get_oid_basic()` once for the mark and — for `^!` and `^-<n>`, which put the
/// truncated name back into `arg` and carry on — once more for what is left. A
/// caller reproducing `handle_revision_arg_1()`'s `warning: refname … is
/// ambiguous.` output has to know which of the two shapes it is looking at.
pub fn parents_only_base(word: &str) -> &str {
    for mark in ["^@", "^!"] {
        // `mark = strstr(arg, …); if (mark && !mark[2])`.
        if word.find(mark) == Some(word.len().wrapping_sub(2)) {
            return &word[..word.len() - 2];
        }
    }
    match word.find("^-") {
        Some(at) if parents_only_parent(&word[at + 2..]).is_some() => &word[..at],
        _ => word,
    }
}

/// What `handle_revision_arg_1()`'s three-mark block made of one operand, before
/// `add_parents_only()` is called.
///
/// The block itself is quoted on [`parents_only`]; this is only the *decode*,
/// which is identical for every verb that reads revisions out of argv, while the
/// queueing that follows is not (each command keeps its pending list in its own
/// shape).
pub enum ParentsOnly<'a> {
    /// No `^@`, `^!` or `^-<n>` at the end of the operand, so the mark block
    /// does nothing at all and the operand is resolved whole.
    Absent,
    /// A `^-<n>` whose `<n>` `handle_revision_arg_1()` refuses outright:
    ///
    /// ```c
    /// if (strtol_i(mark + 2, 10, &exclude_parent) || exclude_parent < 1) {
    ///         ret = -1;
    ///         goto out;
    /// }
    /// ```
    ///
    /// `add_parents_only()` is never reached, so the operand is neither resolved
    /// nor warned about, and `handle_revision_arg()` returns non-zero. For the
    /// verbs that let a failed revision become a pathspec that is
    /// indistinguishable from [`ParentsOnly::Absent`] — a marked operand never
    /// resolves either — but the two are kept apart because
    /// `setup_revisions()`'s callers with `REVARG_CANNOT_BE_FILENAME` do tell
    /// them apart, reporting `bad revision '<arg>'` here and only here.
    BadParent,
    /// A mark `add_parents_only()` is called for.
    Mark {
        /// `add_parents_only()`'s `arg_`: the operand with the mark cut off,
        /// leading `^` still attached.
        base: &'a str,
        /// `exclude_parent`: 0 for `^@` and `^!`, which take every parent, and
        /// `<n>` for `^-<n>`, which takes only the `n`th.
        nth: usize,
        /// True for `^@` alone. `handle_revision_arg_1()` `return 0`s the moment
        /// `add_parents_only()` succeeds for it, so the operand itself is never
        /// queued; `^!` and `^-<n>` instead put the truncated name back into
        /// `arg` and carry on to the single-name path, which is why
        /// `<rev>^!` is the range `<rev>^..<rev>` while `<rev>^@` is only the
        /// parents.
        replaces: bool,
    },
}

/// Decode `handle_revision_arg_1()`'s parent-mark block (`revision.c`):
///
/// ```c
/// mark = strstr(arg, "^@");
/// if (mark && !mark[2]) {
///         arg_minus_at = xmemdupz(arg, mark - arg);
///         if (add_parents_only(revs, arg_minus_at, flags, 0)) { ret = 0; goto out; }
/// }
/// mark = strstr(arg, "^!");
/// if (mark && !mark[2]) {
///         arg_minus_excl = xmemdupz(arg, mark - arg);
///         if (add_parents_only(revs, arg_minus_excl, flags ^ (UNINTERESTING | BOTTOM), 0))
///                 arg = arg_minus_excl;
/// }
/// mark = strstr(arg, "^-");
/// if (mark) {
///         int exclude_parent = 1;
///         if (mark[2]) {
///                 if (strtol_i(mark + 2, 10, &exclude_parent) || exclude_parent < 1) {
///                         ret = -1; goto out;
///                 }
///         }
///         arg_minus_dash = xmemdupz(arg, mark - arg);
///         if (add_parents_only(revs, arg_minus_dash, flags ^ (UNINTERESTING | BOTTOM), exclude_parent))
///                 arg = arg_minus_dash;
/// }
/// ```
///
/// These marks are `handle_revision_arg_1()`'s grammar and not the revision
/// parser's: `get_oid_1()` has no case for any of them, so an operand that still
/// carries one when it reaches [`resolve`] can only fail. That is what makes the
/// decode load-bearing rather than cosmetic — a command that skips this block
/// does not merely mis-name the operand, it cannot resolve it at all.
///
/// The mark is found by [`parents_only_base`], i.e. with `strstr`'s first-match
/// semantics, so `main^!^!` carries no mark and is `bad revision`.
pub fn parents_only(word: &str) -> ParentsOnly<'_> {
    let base = parents_only_base(word);
    if base.len() == word.len() {
        return ParentsOnly::Absent;
    }
    let mark = &word[base.len()..];
    match mark {
        "^@" => ParentsOnly::Mark { base, nth: 0, replaces: true },
        "^!" => ParentsOnly::Mark { base, nth: 0, replaces: false },
        // `^-` with an empty tail is parent 1; anything else is `strtol_i`, and
        // a value below one is refused before `add_parents_only()` is reached.
        _ => match parents_only_parent(&mark[2..]) {
            Some(n) if n >= 1 => {
                ParentsOnly::Mark { base, nth: n as usize, replaces: false }
            }
            _ => ParentsOnly::BadParent,
        },
    }
}

/// What `add_parents_only()` answered.
///
/// git's return is two-valued, but the function can also `die()` on its way
/// there — `get_reference()` sits inside its tag-peeling loop — and that third
/// ending is the caller's to report, because each verb quotes and exits its own
/// way.
pub enum Parents {
    /// `return 1`: the parent selection was queued.
    Queued,
    /// `return 0`: the name did not resolve, did not peel to a commit, or asked
    /// for a parent the commit does not have. `handle_revision_arg_1()` leaves
    /// `arg` alone for all three, so the operand carries its mark onward.
    None,
    /// `get_reference()`'s `die(_("bad object %s"), name)`, where `name` is the
    /// base with its leading `^` already stripped — which is why
    /// `<absent-40-hex>^!` is `fatal: bad object <absent-40-hex>` and quotes
    /// less than the operand did.
    BadObject,
}

/// `add_parents_only()` (`revision.c:2098-2140`): queue the parents of the
/// commit `arg_` names, in git's order and with git's three endings.
///
/// ```c
/// static int add_parents_only(struct rev_info *revs, const char *arg_, int flags,
///                             int exclude_parent)
/// {
///         const char *arg = arg_;
///
///         if (*arg == '^') { flags ^= UNINTERESTING | BOTTOM; arg++; }
///         if (repo_get_oid_committish(revs->repo, arg, &oid))
///                 return 0;
///         while (1) {
///                 it = get_reference(revs, arg, &oid, 0);
///                 if (!it && revs->ignore_missing) return 0;
///                 if (it->type != OBJ_TAG) break;
///                 if (!((struct tag*)it)->tagged) return 0;
///                 oidcpy(&oid, &((struct tag*)it)->tagged->oid);
///         }
///         if (it->type != OBJ_COMMIT) return 0;
///         commit = (struct commit *)it;
///         if (exclude_parent &&
///             exclude_parent > commit_list_count(commit->parents))
///                 return 0;
///         for (parents = commit->parents, parent_number = 1;
///              parents;
///              parents = parents->next, parent_number++) {
///                 if (exclude_parent && parent_number != exclude_parent)
///                         continue;
///                 it = &parents->item->object;
///                 it->flags |= flags;
///                 add_rev_cmdline(revs, it, arg_, REV_CMD_PARENTS_ONLY, flags);
///                 add_pending_object(revs, it, arg);
///         }
///         return 1;
/// }
/// ```
///
/// `exclude_parent` is 1-based and 0 means "every parent". The bounds test is
/// git's own and it is *not* an error: `<merge>^-3` is `return 0`, which leaves
/// the operand to fail its own way rather than dying here.
///
/// The `^` is a flag rather than part of the name, so the queued parents are
/// named by `arg` — the stripped base — while `add_rev_cmdline()` records
/// `arg_`. `queue` is handed the stripped name, the parent, and whether the
/// parent is UNINTERESTING; the caller keeps its own pending list in whatever
/// shape it needs, which is the only part of this that differs between verbs.
///
/// The `repo_get_oid_committish()` below is the *first* of a marked operand's
/// resolutions, so it is where `^!` and `^-<n>` earn the first of their two
/// ambiguity warnings — `^@` returns before the second, and gets one.
pub fn add_parents_only(
    repo: &gix::Repository,
    arg_: &str,
    not: bool,
    exclude_parent: usize,
    queue: &mut dyn FnMut(&str, ObjectId, bool),
) -> Parents {
    let (arg, marked) = uninteresting_mark(arg_);
    let not = not ^ marked;
    let Some(id) = resolve(repo, arg) else {
        return Parents::None;
    };
    // `get_reference()` `parse_object()`s what `repo_get_oid_committish()`
    // decoded, which for a full-length hex is the first time the object database
    // is consulted at all — and the point where an absent id becomes fatal.
    if repo.find_object(id).is_err() {
        return Parents::BadObject;
    }
    // The tag-peeling loop plus `if (it->type != OBJ_COMMIT) return 0;`.
    let crate::sequencer::Side::Commit(id) = crate::sequencer::peel_id(repo, id) else {
        return Parents::None;
    };
    let Ok(commit) = repo.find_commit(id) else {
        return Parents::None;
    };
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if exclude_parent != 0 && exclude_parent > parents.len() {
        return Parents::None;
    }
    for (n, parent) in parents.iter().enumerate() {
        if exclude_parent != 0 && n + 1 != exclude_parent {
            continue;
        }
        queue(arg, *parent, not);
    }
    Parents::Queued
}

/// `handle_revision_arg_1()`'s parent number, read off the tail of a `^-` mark:
///
/// ```c
/// mark = strstr(arg, "^-");
/// if (mark) {
///         int exclude_parent = 1;
///
///         if (mark[2]) {
///                 if (strtol_i(mark + 2, 10, &exclude_parent) ||
///                     exclude_parent < 1) {
///                         ret = -1;
///                         goto out;
///                 }
///         }
/// ```
///
/// So an empty tail is parent 1, and everything else is `strtol_i` — which skips
/// leading whitespace, takes an optional sign, demands the whole tail be consumed
/// and demands the value fit an `int`. `^-+1` and `^- 1` are therefore parent 1
/// while `^-x` and `^-99999999999999999999` are not numbers at all.
///
/// `None` is `strtol_i`'s failure. A value below 1 is *not* rejected here: git
/// rejects it a line later, in the same breath as an `exclude_parent` past the
/// commit's parent count, and both are the caller's to report.
pub fn parents_only_parent(tail: &str) -> Option<i32> {
    if tail.is_empty() {
        return Some(1);
    }
    // Rust's parser is the guard for everything but the whitespace skip: it
    // rejects trailing characters, an empty subject and anything outside `int`,
    // and accepts exactly the optional sign `strtol` does.
    tail.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).parse::<i32>().ok()
}

/// `handle_revision_arg_1()`'s exclusion mark, split off the operand
/// (`revision.c`):
///
/// ```c
/// if (*arg == '^') {
///         flags ^= UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// ```
///
/// The `^` is a *flag*, not part of the name, and the pointer advances past it
/// unconditionally — so everything downstream (`get_oid_with_context()`,
/// `verify_non_filename()`, `get_reference()`'s `die("bad object %s")`) sees the
/// shortened string. That is why an absent full-length hex is reported *without*
/// its caret while a name that does not resolve at all is reported *with* it:
/// `setup_revisions()` still holds the original `argv[i]` when it decides
///
/// ```c
/// if (seen_dashdash || *arg == '^')
///         die(_("bad revision '%s'"), arg);
/// ```
///
/// A bare `^` is not special-cased in git either: `arg++` leaves the empty
/// string, which resolves to nothing, and the operand comes back as
/// `fatal: bad revision '^'`. So the empty remainder is returned as-is rather
/// than being treated as "no mark".
///
/// The returned flag decides which of the two diagnostics above applies, and — for
/// `cmd_diff()` — one more thing: `builtin_diff_tree()` swaps the two trees when
/// the *second* one carries `UNINTERESTING` (builtin/diff.c:202), which is why
/// `git diff HEAD ^HEAD~1` diffs `HEAD~1` against `HEAD`. `cmd_diff_index()` and
/// `cmd_diff_files()` only count `revs->pending` and read the tree out of it, so
/// there the flag is invisible.
pub fn uninteresting_mark(arg: &str) -> (&str, bool) {
    match arg.strip_prefix('^') {
        Some(rest) => (rest, true),
        None => (arg, false),
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

/// The `warning: refname … is ambiguous.` half of the two
/// `repo_get_oid_with_context()` calls at the top of `handle_dotdot_1()` — the
/// half [`dotdot`] deliberately leaves out, because it is a classifier every
/// caller asks at least twice and git warns once per operand.
///
/// A command calls this exactly where git resolves the range for real, and gets
/// the C's own quirks with it:
///
/// ```c
/// if (repo_get_oid_with_context(revs->repo, a_name, oc_flags, &a_oid, a_oc) ||
///     repo_get_oid_with_context(revs->repo, b_name, oc_flags, &b_oid, b_oc))
///         return -1;
/// ```
///
/// `||` short-circuits, so a left endpoint that does not resolve means the right
/// one is never looked at and never warns — `nosuch..<40-hex-ref>` is silent in
/// stock 2.55.0 while `<40-hex-ref>..nosuch` warns once, since a full-length hex
/// resolves without the object database being consulted.
///
/// An endpoint carrying the exclusion mark is skipped: `handle_dotdot()` runs
/// before `handle_revision_arg_1()` strips the `^`, so `get_oid_basic()` sees a
/// name one character too long for its first branch and this pair fails there —
/// the warning such an operand still earns comes later, from the single-name
/// resolution of what follows the mark.
pub fn warn_dotdot_endpoints(repo: &gix::Repository, spec: &str) {
    let Some(Range { a, b, .. }) = split_range(spec) else {
        return;
    };
    for end in [a, b] {
        if end.starts_with('^') {
            return;
        }
        warn_ambiguous_refname(repo, end);
        if resolve_quiet(repo, end).is_none() {
            return;
        }
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
    /// The exact stderr bytes and exit status, for a caller that has to *hold* the
    /// diagnostic rather than raise it — a command whose single left-to-right parse
    /// can still meet an earlier failure and must report that one instead.
    pub fn rendered(&self) -> (u8, Vec<u8>) {
        let mut out = String::new();
        for note in &self.notes {
            out.push_str(&format!("error: {note}\n"));
        }
        let (prefix, code) = if self.fatal { ("fatal", 128) } else { ("error", 129) };
        out.push_str(&format!("{prefix}: {}\n", self.message));
        (code, out.into_bytes())
    }

    /// Print the diagnostic on stderr and yield git's exit status for it.
    pub fn report(&self) -> std::process::ExitCode {
        let (code, bytes) = self.rendered();
        use std::io::Write;
        let _ = std::io::stderr().lock().write_all(&bytes);
        std::process::ExitCode::from(code)
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

/// git's `diff_opt_find_object()` (`diff.c`), behind `--find-object=<object-id>`:
///
/// ```c
/// static int diff_opt_find_object(const struct option *opt,
///                                 const char *arg, int unset)
/// {
///         struct diff_options *options = opt->value;
///         struct object_id oid;
///
///         BUG_ON_OPT_NEG(unset);
///         if (repo_get_oid(the_repository, arg, &oid))
///                 return error(_("unable to resolve '%s'"), arg);
///
///         if (!options->objfind)
///                 CALLOC_ARRAY(options->objfind, 1);
///
///         options->pickaxe_opts |= DIFF_PICKAXE_KIND_OBJFIND;
///         options->flags.recursive = 1;
///         options->flags.tree_in_recursive = 1;
///         oidset_insert(options->objfind, &oid);
///         return 0;
/// }
/// ```
///
/// `repo_get_oid()` again, so the rule at the top of this module decides the
/// common case: `--find-object` compares *ids*, so an object the repository does
/// not have is a perfectly good needle that simply matches nothing, and stock
/// exits 0 with empty output. A site resolving through `rev_parse_single()` alone
/// turns that into the `error()` below and exit 129, which is what breaks a
/// script that greps a history for an id it has since gc'd.
///
/// The option repeats and each occurrence inserts into the same `oidset`, so this
/// answers *one* flag: a caller collects the ids and keeps a pair matching any of
/// them. Per-occurrence rather than per-list because the `error()` below is raised
/// from the callback, at the flag's own argv position — `--find-object=nosuch <bad
/// object>` reports the unresolvable name while `<bad object> --find-object=nosuch`
/// reports the bad object, and only a caller that resolves in argv order can do
/// both (verified against stock 2.55.0 on all three diff verbs).
///
/// `error()` from an option callback is parse-options' `PARSE_OPT_ERROR`, a bare
/// `exit(129)` with no usage block, which is [`OperandError`] with `fatal: false`.
pub fn find_object(repo: &gix::Repository, arg: &str) -> Result<ObjectId, OperandError> {
    resolve(repo, arg).ok_or_else(|| OperandError {
        notes: Vec::new(),
        message: format!("unable to resolve '{arg}'"),
        fatal: false,
    })
}

/// What `get_oid_basic()`'s reflog branch has to say about a `<ref>@{…}` operand
/// whose selector reaches past the end of the log (`object-name.c:758-822`).
///
/// The two outcomes are not interchangeable: one ends the command, the other is
/// printed and the operand still resolves.
pub enum ReflogReach {
    /// ```c
    /// die(_("log for '%.*s' only has %d entries"), len, str, co_cnt);
    /// ```
    Fatal(String),
    /// ```c
    /// warning(_("log for '%.*s' only goes back to %s"),
    ///         len, str, show_date(co_time, co_tz, DATE_MODE(RFC2822)));
    /// ```
    ///
    /// `read_ref_at()` has already filled the oid from the oldest entry, so the
    /// operand goes on to resolve after this is printed.
    Warning(String),
}

/// The `@{…}` selector of a reflog operand, as `get_oid_basic()` splits it
/// (`object-name.c:704-724`):
///
/// ```c
/// reflog_len = at = 0;
/// if (len && str[len-1] == '}') {
///         for (at = len-4; at >= 0; at--) {
///                 if (str[at] == '@' && str[at+1] == '{') {
///                         if (str[at+2] == '-') {
///                                 if (at != 0)
///                                         /* @{-N} not at start */
///                                         return -1;
///                                 nth_prior = 1;
///                                 continue;
///                         }
///                         if (!upstream_mark(str + at, len - at) &&
///                             !push_mark(str + at, len - at)) {
///                                 reflog_len = (len-1) - (at+2);
///                                 len = at;
///                         }
///                         break;
///                 }
///         }
/// }
/// ```
///
/// `None` when `str` carries no reflog selector at all — no trailing `}`, no
/// `@{`, or a suffix that `upstream_mark()`/`push_mark()` claims (`@{u}`,
/// `@{upstream}`, `@{push}`), each of which leaves the name an ordinary ref.
/// `@{-<n>}` is `interpret_nth_prior_checkout()`'s, not a reflog selector.
fn split_reflog_selector(spec: &str) -> Option<(&str, &str)> {
    let b = spec.as_bytes();
    if b.is_empty() || b[b.len() - 1] != b'}' || b.len() < 4 {
        return None;
    }
    let at = (0..=b.len() - 4).rev().find(|&i| b[i] == b'@' && b[i + 1] == b'{')?;
    let sel = &spec[at + 2..spec.len() - 1];
    // `upstream_mark()`/`push_mark()` compare with `strncasecmp`, so the marks
    // are claimed by `interpret_branch_name()` in any case and never reach the
    // reflog reader.
    if sel.starts_with('-')
        || ["u", "upstream", "push"].iter().any(|m| sel.eq_ignore_ascii_case(m))
    {
        return None;
    }
    Some((&spec[..at], sel))
}

/// git's `get_oid_basic()` reflog branch (`object-name.c:758-822`), which is the
/// only place a `<ref>@{<n>}` / `<ref>@{<date>}` operand out of range is
/// diagnosed. Without it the name simply fails to resolve and every caller
/// reports `ambiguous argument '<spec>'`, which is not what git prints.
///
/// The selector is read the way git reads it — an all-digit run is the N-th
/// entry, unless it is large enough to be an epoch, and anything else goes
/// through `approxidate_careful()`:
///
/// ```c
/// for (i = nth = 0; 0 <= nth && i < reflog_len; i++) {
///         char ch = str[at+2+i];
///         if ('0' <= ch && ch <= '9')
///                 nth = nth * 10 + ch - '0';
///         else
///                 nth = -1;
/// }
/// if (100000000 <= nth) {
///         at_time = nth;
///         nth = -1;
/// } else if (0 <= nth)
///         at_time = 0;
/// ```
///
/// `None` means git has nothing to say: the name is not a reflog operand, the ref
/// has no log at all (`repo_dwim_log()` finds nothing, and the operand then fails
/// to resolve exactly as it does today), or the selector is in range.
///
/// `spec` is the operand as written. [`ambiguity_base`] reduces it to the name
/// `get_oid_basic()` is actually handed, which is where the diagnosis belongs:
/// `HEAD@{99}^`, `HEAD@{99}~1`, `HEAD@{99}^{commit}` and `HEAD@{99}:f` all die
/// with `log for 'HEAD' only has 4 entries` in stock 2.55.0, because
/// `get_parent()`, `get_nth_ancestor()`, `peel_onion()` and
/// `get_oid_with_context_1()`'s path arm each resolve the base *first* and never
/// get to apply their suffix. Asking about the operand as typed found no `@{…}`
/// at the end of three of those four and reported the generic
/// `ambiguous argument` instead.
pub fn reflog_reach(repo: &gix::Repository, spec: &str) -> Option<ReflogReach> {
    reflog_reach_of(repo, ambiguity_base(spec))
}

/// [`reflog_reach`] for one `get_oid_basic()` call, over the name that call was
/// handed rather than over the operand.
///
/// `get_oid_1()` reaches `get_oid_basic()` twice for an operand whose
/// `peel_onion()` failed (`object-name.c:1128-1132`), and the second time it
/// passes the name **whole** — so the two calls do not agree on where the reflog
/// selector ends, and a single reduction cannot stand in for both. See
/// [`resolve_with_flags`].
pub fn reflog_reach_of(repo: &gix::Repository, name: &str) -> Option<ReflogReach> {
    let (base, sel) = split_reflog_selector(name)?;
    // `repo_dwim_log()`: which ref's log this is. Nothing found is `refs_found ==
    // 0`, i.e. `return -1` — the operand does not resolve, and the caller's
    // existing "unknown revision" message is git's.
    // `get_oid_basic()` splits here, and the two halves do NOT agree on what the
    // ref is called:
    //
    // ```c
    // if (!len && reflog_len)
    //         /* allow "@{...}" to mean the current branch reflog */
    //         refs_found = repo_dwim_ref(r, "HEAD", 4, oid, &real_ref, 0);
    // else if (reflog_len)
    //         refs_found = repo_dwim_log(r, str, len, oid, &real_ref);
    // ```
    //
    // `repo_dwim_ref("HEAD")` resolves the symref and reports its *target*, so a
    // bare `@{99}` is diagnosed as `main`; `repo_dwim_log("HEAD")` finds HEAD's own
    // log and reports `HEAD`. Detached, there is no target and both say `HEAD`.
    let full = if base.is_empty() {
        repo.head_ref()
            .ok()
            .flatten()
            .map(|r| r.name().as_bstr().to_string())
            .unwrap_or_else(|| "HEAD".to_string())
    } else {
        crate::porcelain::reflog::dwim_log(repo, base)?
    };

    let (nth, at_time) = {
        let digits = !sel.is_empty() && sel.bytes().all(|c| c.is_ascii_digit());
        // `nth * 10 + ch - '0'` overflows into a negative `int` for a long enough
        // run, which the `100000000 <= nth` test then reads as "not an epoch"; the
        // saturating parse below lands on the same side of that test.
        let n: i64 = if digits { sel.parse().unwrap_or(i64::MAX) } else { -1 };
        if n >= 100_000_000 {
            (-1i64, n)
        } else if n >= 0 {
            (n, 0i64)
        } else {
            let (t, errors) = crate::date::approxidate_careful(sel);
            if errors {
                return None;
            }
            (-1, t)
        }
    };

    // `read_ref_at()` walks the log newest-entry-first (refs.c:1181), so the
    // entries are read in that order here too.
    let entries = reflog_entries(repo, &full)?;
    if entries.is_empty() {
        // `if (!cb.reccnt)`: `<ref>@{0}` on an empty log falls back to the ref's
        // own value, anything else is a different message that names the *full*
        // ref rather than the operand (refs.c:1183-1203).
        return (nth != 0).then(|| ReflogReach::Fatal(format!("log for {full} is empty")));
    }
    // `warning(…, len, str)` prints the operand's own spelling, except for a bare
    // `@{…}`, where `get_oid_basic()` substitutes the ref it resolved:
    //
    // ```c
    // if (!len) {
    //         if (!skip_prefix(real_ref, "refs/heads/", &str))
    //                 str = "HEAD";
    //         len = strlen(str);
    // }
    // ```
    let named = if base.is_empty() {
        full.strip_prefix("refs/heads/").unwrap_or("HEAD").to_string()
    } else {
        base.to_string()
    };

    if nth >= 0 {
        let count = entries.len() as i64;
        if nth < count {
            return None;
        }
        // `} else if (nth == co_cnt && !is_null_oid(oid)) {`: asked for one entry
        // past the end, `read_ref_at_ent_oldest()` left the oldest entry's *old*
        // id in `oid`, and that is a usable answer whenever it is not the null id.
        let oldest_old = entries.last().map(|e| e.0);
        if nth == count && oldest_old.is_some_and(|id| !id.is_null()) {
            return None;
        }
        return Some(ReflogReach::Fatal(format!(
            "log for '{named}' only has {count} entries"
        )));
    }

    // `read_ref_at_ent()`: `if (timestamp <= cb->at_time || cb->cnt == 0)`. With a
    // date selector `cnt` is 0 from the start only for `@{0}`, so what decides the
    // walk is whether any entry is old enough.
    if entries.iter().any(|e| e.1.seconds <= at_time) {
        return None;
    }
    let oldest = entries.last()?;
    Some(ReflogReach::Warning(format!(
        "log for '{named}' only goes back to {}",
        crate::porcelain::log::show_date_rfc2822(oldest.1.seconds, oldest.1.offset)
    )))
}

/// [`reflog_reach`]'s [`ReflogReach::Warning`] as `get_oid_basic()` prints it, or
/// `None` when it has nothing to say about `spec`.
///
/// The two halves of the reflog diagnostic reach their callers differently. The
/// fatal ends the command, so it belongs in the message builders that already own
/// the "this operand is not a revision" text ([`crate::porcelain::log::
/// bad_revision_message_in`]). The warning is printed and the operand *still
/// resolves*, so its caller is whoever resolves an argv operand — the same place
/// [`warn_ambiguous_refname`] is called, because both warnings come out of the one
/// `get_oid_basic()` call (`object-name.c:964-967` and `object-name.c:1006-1011`).
///
/// `spec` is the operand as written; [`ambiguity_base`] reduces it to what
/// `get_oid_basic()` is actually handed, so `HEAD@{<date>}^`,
/// `HEAD@{<date>}^{commit}` and `HEAD@{<date>}:<path>` each warn once, naming the
/// reflog and not the suffix.
pub fn reflog_reach_warning(repo: &gix::Repository, spec: &str) -> Option<String> {
    match reflog_reach(repo, spec)? {
        ReflogReach::Warning(message) => Some(format!("warning: {message}\n")),
        // `read_ref_at()`'s other outcome is a `die()`, and printing it here would
        // duplicate the fatal its caller is already building.
        ReflogReach::Fatal(_) => None,
    }
}

/// The `die()` inside `interpret_branch_mark()` (`refs.c`) for an operand
/// carrying an `@{u}`/`@{upstream}` mark whose upstream cannot be named, or
/// `None` when git has nothing to die about.
///
/// ```c
/// static int interpret_branch_mark(struct repository *r,
///                                  const char *name, int namelen,
///                                  int at, struct strbuf *buf,
///                                  int (*get_mark)(const char *, int),
///                                  const char *(*get_data)(struct branch *, struct strbuf *),
///                                  const struct interpret_branch_name_options *options)
/// {
///         len = get_mark(name + at, namelen - at);
///         if (!len)
///                 return -1;
///         if (memchr(name, ':', at))
///                 return -1;
///         if (at) {
///                 char *name_str = xmemdupz(name, at);
///                 branch = branch_get(name_str);
///                 free(name_str);
///         } else
///                 branch = branch_get(NULL);
///         value = get_data(branch, &err);
///         if (!value) {
///                 if (options->nonfatal_dangling_mark) { … return -1; }
///                 else
///                         die("%s", err.buf);
///         }
/// ```
///
/// with `get_data` = `branch_get_upstream()` (`remote.c`):
///
/// ```c
/// if (!branch)
///         return error_buf(err, _("HEAD does not point to a branch"));
/// if (!branch->merge || !branch->merge[0]->dst) {
///         if (!ref_exists(branch->refname))
///                 return error_buf(err, _("no such branch: '%s'"), branch->name);
///         if (!branch->merge)
///                 return error_buf(err,
///                                  _("no upstream configured for branch '%s'"),
///                                  branch->name);
///         return error_buf(err,
///                          _("upstream branch '%s' not stored as a remote-tracking branch"),
///                          branch->merge[0]->src);
/// }
/// ```
///
/// The `die()` happens inside `get_oid()`, before the command has a failed
/// operand to report — which is why every caller's own "ambiguous argument"
/// block is *wrong* here rather than merely differently worded.
///
/// Three details the C makes and a re-derivation would not:
///
///   * `upstream_mark()` compares with `strncasecmp` and only requires the mark
///     to *start* at the `@`, so `@{U}` and `@{UpStReAm}` both match and
///     `lonely@{u}xyz` still dies — the die precedes the caller's check that the
///     mark consumed the whole name.
///   * `branch_get(NULL)` and `branch_get("HEAD")` are the same lookup, so
///     `@{u}` and `HEAD@{u}` diagnose the checked-out branch. A detached HEAD has
///     no branch at all; an unborn one has a name whose ref does not exist, which
///     is the `no such branch:` arm rather than the detached one.
///   * `interpret_branch_name()` walks the `@` positions left to right, so
///     `a@b@{u}` names the branch `a@b`.
///
/// `@{push}` reaches the same `interpret_branch_mark()` with a different
/// `get_data` — `branch_get_push()` — and is answered by [`push_mark_fatal`],
/// which this delegates to. `interpret_branch_name()` tries `upstream_mark` and
/// then `push_mark` at each `@` in turn, so the *earlier* `@` decides which of the
/// two applies and a tie goes to the upstream mark.
pub fn upstream_mark_fatal(repo: &gix::Repository, spec: &str) -> Option<String> {
    let base = ambiguity_base(spec);
    let at = match (upstream_mark_at(base), push_mark_at(base)) {
        (Some(u), Some(p)) if p < u => return push_mark_fatal(repo, spec),
        (None, Some(_)) => return push_mark_fatal(repo, spec),
        (Some(u), _) => u,
        (None, None) => return None,
    };
    let named = &base[..at];
    // `branch_get(NULL)` / `branch_get("HEAD")`: the branch HEAD points at, which
    // is `NULL` when HEAD is detached. `head_name()` keeps answering for an unborn
    // HEAD, which is what puts that case on the `no such branch:` arm below.
    let name = if named.is_empty() || named == "HEAD" {
        match repo.head_name() {
            Ok(Some(full)) => full.shorten().to_string(),
            _ => return Some("HEAD does not point to a branch".to_string()),
        }
    } else {
        named.to_string()
    };

    let full = format!("refs/heads/{name}");
    // `branch->merge[0]->dst`, which is the whole condition when it is present.
    if crate::porcelain::branch::upstream_ref(repo, full.as_str().into()).is_some() {
        return None;
    }
    if repo.try_find_reference(full.as_str()).ok().flatten().is_none() {
        return Some(format!("no such branch: '{name}'"));
    }
    match repo.config_snapshot().string(&format!("branch.{name}.merge")) {
        None => Some(format!("no upstream configured for branch '{name}'")),
        Some(src) => Some(format!(
            "upstream branch '{}' not stored as a remote-tracking branch",
            gix::bstr::ByteSlice::to_str_lossy(src.as_slice())
        )),
    }
}

/// The offset of the `@` that opens a `@{push}` mark, as
/// `interpret_branch_name()`'s left-to-right scan finds it.
///
/// `push_mark()` is `at_mark(string, len, { "@{push}" }, 1)`, i.e. the same
/// `strncasecmp` prefix test `upstream_mark()` uses (`object-name.c:659-663`), so
/// the mark need not end the operand and its case does not matter.
pub fn push_mark_at(base: &str) -> Option<usize> {
    base.bytes().enumerate().filter(|(_, b)| *b == b'@').map(|(i, _)| i).find(|&i| {
        let rest = &base[i..];
        rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("@{push}")
    })
}

/// The `die()` inside `interpret_branch_mark()` for an operand carrying a
/// `@{push}` mark whose push destination cannot be named, or `None` when git has
/// nothing to die about.
///
/// The mark reaches the same `interpret_branch_mark()` as `@{u}`
/// ([`upstream_mark_fatal`]) but with `branch_get_push()` for `get_data`, which is
/// `branch_get_push_1()` (`remote.c:1904-1966`):
///
/// ```c
/// remote = remotes_remote_get(repo, remotes_pushremote_for_branch(remote_state, branch, NULL));
/// if (!remote)
///         return error_buf(err, _("branch '%s' has no remote for pushing"), branch->name);
/// if (remote->push.nr) {
///         dst = apply_refspecs(&remote->push, branch->refname);
///         if (!dst)
///                 return error_buf(err, _("push refspecs for '%s' do not include '%s'"),
///                                  remote->name, branch->name);
///         return tracking_for_push_dest(remote, dst, err);
/// }
/// if (remote->mirror)
///         return tracking_for_push_dest(remote, branch->refname, err);
/// switch (push_default) {
/// case PUSH_DEFAULT_NOTHING:
///         return error_buf(err, _("push has no destination (push.default is 'nothing')"));
/// case PUSH_DEFAULT_MATCHING:
/// case PUSH_DEFAULT_CURRENT:
///         return tracking_for_push_dest(remote, branch->refname, err);
/// case PUSH_DEFAULT_UPSTREAM:
///         return xstrdup_or_null(branch_get_upstream(branch, err));
/// case PUSH_DEFAULT_UNSPECIFIED:
/// case PUSH_DEFAULT_SIMPLE: {
///         up = branch_get_upstream(branch, err);
///         if (!up) return NULL;
///         cur = tracking_for_push_dest(remote, branch->refname, err);
///         if (!cur) return NULL;
///         if (strcmp(cur, up))
///                 return error_buf(err, _("cannot resolve 'simple' push to a single destination"));
///         return cur;
/// }
/// }
/// ```
///
/// with `tracking_for_push_dest()` (`remote.c:1889-1901`) mapping a destination
/// back through the *fetch* refspecs:
///
/// ```c
/// ret = apply_refspecs(&remote->fetch, refname);
/// if (!ret)
///         return error_buf(err, _("push destination '%s' on remote '%s' has no local tracking branch"),
///                          refname, remote->name);
/// ```
///
/// Three outcomes git does **not** die on, which is why this cannot simply
/// answer "no push destination":
///
///   * `push.default=current` on a branch with no upstream still maps through the
///     fetch refspecs, so the operand resolves to a remote-tracking name that may
///     simply not exist yet — an ordinary `ambiguous argument`, not a fatal.
///   * a `remote.<r>.push` refspec that *does* match produces a destination whose
///     tracking ref likewise need not exist.
///   * `remotes_remote_get()` invents a remote for a name it does not know
///     (`add_url_alias()`), so `branch.<n>.pushRemote=nosuchremote` reaches the
///     "no local tracking branch" arm rather than "has no remote for pushing".
pub fn push_mark_fatal(repo: &gix::Repository, spec: &str) -> Option<String> {
    let base = ambiguity_base(spec);
    let at = push_mark_at(base)?;
    let named = &base[..at];
    if named.contains(':') {
        return None;
    }
    // `branch_get(NULL)` / `branch_get("HEAD")`.
    let name = if named.is_empty() || named == "HEAD" {
        match repo.head_name() {
            Ok(Some(full)) => full.shorten().to_string(),
            _ => return Some("HEAD does not point to a branch".to_string()),
        }
    } else {
        named.to_string()
    };
    let refname = format!("refs/heads/{name}");
    let config = repo.config_snapshot();
    let string = |key: &str| {
        config.string(key).map(|v| gix::bstr::ByteSlice::to_str_lossy(v.as_slice()).into_owned())
    };

    // `remotes_pushremote_for_branch()` then `remotes_remote_for_branch()`.
    let explicit = string(&format!("branch.{name}.pushRemote"))
        .or_else(|| string("remote.pushDefault"))
        .or_else(|| string(&format!("branch.{name}.remote")));
    // `if (remote_state->remotes_nr == 1) return remote_state->remotes[0]->name;`
    // then `return "origin";`.
    //
    // `branch_get_push_1()`'s `if (!remote)` arm — "branch '%s' has no remote for
    // pushing" — is unreachable from here and so is not produced: the name is
    // *always* handed to `remotes_remote_get()` explicitly, which makes
    // `name_given` non-zero, and `remote_get_1()` then calls `add_url_alias()` for
    // a name it does not know. That gives the invented remote a url, `valid_remote()`
    // is `!!remote->url.nr`, and the function returns non-NULL. Measured: in a
    // repository with no remotes at all, stock 2.55.0 answers
    // `fatal: no upstream configured for branch 'main'`, i.e. it fell through to
    // the `push.default` switch rather than reporting a missing remote.
    let remote = match explicit {
        Some(r) => r,
        None => {
            let names = repo.remote_names();
            match names.len() {
                1 => names
                    .iter()
                    .next()
                    .map(|n| gix::bstr::ByteSlice::to_str_lossy(n.as_slice()).into_owned())
                    .unwrap_or_else(|| "origin".to_string()),
                _ => "origin".to_string(),
            }
        }
    };

    let specs = |key: &str| -> Vec<String> {
        config
            .strings(key)
            .into_iter()
            .flatten()
            .map(|v| gix::bstr::ByteSlice::to_str_lossy(v.as_slice()).into_owned())
            .collect()
    };
    let fetch = specs(&format!("remote.{remote}.fetch"));
    let push = specs(&format!("remote.{remote}.push"));
    // `tracking_for_push_dest()`.
    let tracking = |dest: &str| -> Result<String, String> {
        apply_refspecs(&fetch, dest).ok_or_else(|| {
            format!("push destination '{dest}' on remote '{remote}' has no local tracking branch")
        })
    };

    let landed = if !push.is_empty() {
        match apply_refspecs(&push, &refname) {
            None => {
                return Some(format!(
                    "push refspecs for '{remote}' do not include '{name}'"
                ))
            }
            Some(dst) => tracking(&dst),
        }
    } else if config.boolean(&format!("remote.{remote}.mirror")).unwrap_or(false) {
        tracking(&refname)
    } else {
        match string("push.default").as_deref().unwrap_or("simple") {
            "nothing" => {
                return Some("push has no destination (push.default is 'nothing')".to_string())
            }
            "matching" | "current" => tracking(&refname),
            "upstream" | "tracking" => return upstream_mark_fatal_for(repo, &name),
            // `PUSH_DEFAULT_UNSPECIFIED` and `PUSH_DEFAULT_SIMPLE`.
            _ => {
                if let Some(message) = upstream_mark_fatal_for(repo, &name) {
                    return Some(message);
                }
                let up = crate::porcelain::branch::upstream_ref(repo, refname.as_str().into())?;
                match tracking(&refname) {
                    Err(message) => return Some(message),
                    Ok(cur) if cur.as_bytes() != up.as_bstr() => {
                        return Some(
                            "cannot resolve 'simple' push to a single destination".to_string(),
                        )
                    }
                    Ok(cur) => Ok(cur),
                }
            }
        }
    };
    landed.err()
}

/// [`upstream_mark_fatal`] asked about a branch by name rather than about an
/// operand, for `branch_get_push_1()`'s `branch_get_upstream()` arms.
fn upstream_mark_fatal_for(repo: &gix::Repository, name: &str) -> Option<String> {
    upstream_mark_fatal(repo, &format!("{name}@{{u}}"))
}

/// `apply_refspecs()` (`refspec.c:486-497`) reduced to what
/// `branch_get_push_1()` asks of it: the destination the first matching
/// `<src>:<dst>` maps `name` to.
///
/// ```c
/// if (refspec->pattern) {
///         if (match_refname_with_pattern(key, needle, value, result)) { … return 0; }
/// } else if (!strcmp(needle, key)) {
///         *result = xstrdup(value);  … return 0;
/// }
/// ```
///
/// (`refspec.c:refspec_find_match`.) A leading `+` is the force flag and is not
/// part of the source pattern; a spec with no `:` has no destination and is
/// skipped, as is a negative (`^`) one.
fn apply_refspecs(specs: &[String], name: &str) -> Option<String> {
    for spec in specs {
        let spec = spec.strip_prefix('+').unwrap_or(spec);
        if spec.starts_with('^') {
            continue;
        }
        let Some((src, dst)) = spec.split_once(':') else {
            continue;
        };
        if dst.is_empty() {
            continue;
        }
        match src.split_once('*') {
            // `match_refname_with_pattern()`: `<prefix>*<suffix>`, with the
            // matched middle spliced into the replacement's own `*`.
            Some((prefix, suffix)) => {
                let (dprefix, dsuffix) = dst.split_once('*')?;
                if name.len() >= prefix.len() + suffix.len()
                    && name.starts_with(prefix)
                    && name.ends_with(suffix)
                {
                    let middle = &name[prefix.len()..name.len() - suffix.len()];
                    return Some(format!("{dprefix}{middle}{dsuffix}"));
                }
            }
            None if src == name => return Some(dst.to_string()),
            None => {}
        }
    }
    None
}

/// `get_oid_basic()`'s reflog branch (`object-name.c:742-822`) as a *resolver*:
/// the object id `<ref>@{<n>}` / `<ref>@{<date>}` names.
///
/// gitoxide's rev-spec parser resolves the ref half through its own ref lookup and
/// gives up when the name is ambiguous, so `dup@{0}` fails there where git answers.
/// git never consults an ambiguity rule here at all — it calls `repo_dwim_log()`,
/// which walks `ref_rev_parse_rules` and takes the first candidate that both
/// resolves *and* has a log:
///
/// ```c
/// if (!len && reflog_len)
///         refs_found = repo_dwim_ref(r, "HEAD", 4, oid, &real_ref, !fatal);
/// else if (reflog_len)
///         refs_found = repo_dwim_log(r, str, len, oid, &real_ref);
/// …
/// if (read_ref_at(get_main_ref_store(r), real_ref, flags, at_time, nth, oid, NULL,
///                 &co_time, &co_tz, &co_cnt)) { … }
/// ```
///
/// `None` means git has no answer either — the name is not a reflog operand, no
/// rule found a log, or the selector is out of range (which
/// [`reflog_reach`] separately turns into git's `die()`).
pub fn reflog_oid(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let (read, nth, at_time) = reflog_read(repo, spec)?;
    if read.found {
        return Some(read.oid);
    }

    // `read_ref_at()` returned 1, so `get_oid_basic()` decides what that means
    // (`object-name.c:790-820`). Two of the three outcomes still answer; the third
    // is the `die()` [`reflog_reach`] builds, which is this `None`.
    if at_time != 0 {
        // `warning(_("log for '%.*s' only goes back to %s"), …)` — the oldest
        // entry's id is the answer and the operand resolves.
        return Some(read.oid);
    }
    // `} else if (nth == co_cnt && !is_null_oid(oid)) {`: one past the end is still
    // answerable from `read_ref_at_ent_oldest()`'s `oid`.
    (nth == read.reccnt as i64 && !read.oid.is_null()).then_some(read.oid)
}

/// The `warning()` `read_ref_at()` raises for `spec`, ready to print, or `None`
/// when the operand's log has nothing to complain about.
///
/// Kept apart from [`reflog_oid`] for the same reason
/// [`warn_ambiguous_refname`] is kept apart from the resolver: git warns once per
/// `get_oid_basic()` call, and a zvcs command that resolves a name twice — once to
/// classify it, once to use it — must not say twice what stock says once. So this
/// belongs beside [`warn_ambiguous_refname`] in [`resolve`], never in
/// [`resolve_quiet`].
///
/// [`ambiguity_base`] reduces the operand to what `get_oid_basic()` is actually
/// handed, so `HEAD@{1}^`, `HEAD@{1}^{commit}` and `HEAD@{1}:<path>` each warn
/// once and name the reflog rather than the suffix.
///
/// Unlike `object-name.c`'s two other reflog diagnostics this one has no `flags`
/// gate at all — `refs.c:1135` and `refs.c:1141` call `warning()` outright — so
/// `git rev-parse --quiet --verify <spec>` prints it just the same.
pub fn read_ref_at_warning(repo: &gix::Repository, spec: &str) -> Option<String> {
    read_ref_at_warning_of(repo, ambiguity_base(spec))
}

/// [`read_ref_at_warning`] for one `get_oid_basic()` call, over the name that
/// call was handed rather than over the operand.
///
/// The pair to [`reflog_reach_of`], and needed for the same reason: `get_oid_1()`
/// reaches `get_oid_basic()` a second time, with the name **whole**, once
/// `peel_onion()` has returned -1 (`object-name.c:1128-1132`).
pub fn read_ref_at_warning_of(repo: &gix::Repository, name: &str) -> Option<String> {
    reflog_read(repo, name)?.0.warning
}

/// `get_oid_basic()`'s reflog branch up to and including the `read_ref_at()` call
/// (`object-name.c:742-789`), returning the walk's result together with the two
/// selector values `object-name.c:790-820` still needs to interpret it.
fn reflog_read(repo: &gix::Repository, spec: &str) -> Option<(ReadRefAt, i64, i64)> {
    let (base, sel) = split_reflog_selector(spec)?;
    // `repo_dwim_ref("HEAD")` reports HEAD's *target*; `repo_dwim_log` reports the
    // ref whose log was found. Either way an unborn HEAD resolves to nothing and
    // the operand fails, which is what makes a stale `logs/HEAD` a fatal.
    let full = if base.is_empty() {
        crate::refname::resolve_ref_reading(repo, "HEAD")?
    } else {
        crate::porcelain::reflog::dwim_log(repo, base)?
    };

    // The selector, read exactly as `get_oid_basic()` reads it: an all-digit run is
    // the N-th entry unless it is large enough to be a unix timestamp.
    let digits = !sel.is_empty() && sel.bytes().all(|c| c.is_ascii_digit());
    let n: i64 = if digits { sel.parse().unwrap_or(i64::MAX) } else { -1 };
    let (nth, at_time) = if n >= 100_000_000 {
        (-1i64, n)
    } else if n >= 0 {
        (n, 0i64)
    } else {
        let (t, errors) = crate::date::approxidate_careful(sel);
        if errors {
            return None;
        }
        (-1, t)
    };

    // `repo_dwim_log()` does not merely *find* the log — it resolves the ref and
    // hands `read_ref_at()` that value in the same `oid` out-parameter the answer
    // comes back in (`refs.c:855-871`). Every branch of `read_ref_at_ent()` below
    // leans on that pre-seed, so a spelling whose ref does not resolve is
    // `refs_found == 0`, i.e. `return -1`, and the operand fails.
    let current = resolve_ref_oid(repo, &full)?;
    // `refs_for_each_reflog_ent_reverse()`: newest entry first.
    let entries = reflog_lines(repo, &full)?;
    Some((read_ref_at(&full, &entries, at_time, nth, current)?, nth, at_time))
}

/// `read_ref_at()` (`refs.c:1173-1218`) as a value rather than the pile of
/// out-parameters the C fills in.
struct ReadRefAt {
    /// `*oid` as the function leaves it. Note that it is an *in*-parameter too:
    /// the caller pre-seeds it with the ref's current value, and three of
    /// `read_ref_at_ent()`'s four paths never overwrite it.
    oid: ObjectId,
    /// The `return` value inverted — `return 0` (an entry matched) is `true`,
    /// `return 1` (the walk fell off the end) is `false`.
    found: bool,
    /// `*cutoff_cnt`, i.e. `cb.reccnt`: how many entries were consumed. The
    /// out-of-range test in `get_oid_basic()` compares `nth` against it.
    reccnt: usize,
    /// The one `warning()` `read_ref_at_ent()` can raise. Held rather than printed
    /// so the walk stays a pure function; `reflog_oid` prints it where the C does.
    warning: Option<String>,
}

/// `read_ref_at()` (`refs.c:1173-1218`) over an already-collected, newest-first
/// reflog.
///
/// The subtlety that makes this worth porting line by line is that `cb->ooid` and
/// `cb->noid` lag the walk by one entry — `read_ref_at_ent()` inspects them
/// *before* storing the current record:
///
/// ```c
/// if (timestamp <= cb->at_time || cb->cnt == 0) {
///         set_read_ref_cutoffs(cb, timestamp, tz, message);
///         /*
///          * we have not yet updated cb->[n|o]oid so they still
///          * hold the values for the previous record.
///          */
///         if (!is_null_oid(&cb->ooid)) {
///                 oidcpy(cb->oid, noid);
///                 if (!oideq(&cb->ooid, noid))
///                         warning(_("log for ref %s has gap after %s"),
///                                 refname, show_date(cb->date, cb->tz, DATE_MODE(RFC2822)));
///         }
///         else if (cb->date == cb->at_time)
///                 oidcpy(cb->oid, noid);
///         else if (!oideq(noid, cb->oid))
///                 warning(_("log for ref %s unexpectedly ended on %s"),
///                         refname, show_date(cb->date, cb->tz, DATE_MODE(RFC2822)));
///         cb->reccnt++;
///         oidcpy(&cb->ooid, ooid);
///         oidcpy(&cb->noid, noid);
///         cb->found_it = 1;
///         return 1;
/// }
/// ```
///
/// So `<ref>@{<n>}` is *not* "entry `n`'s new id". It is entry `n`'s new id only
/// when entry `n-1` — the one entry *newer* — has a non-null old id. When entry
/// `n-1` is a creation (null old id) the answer is left at the ref's current
/// value, which is what a `git branch -m` round trip exposes: the rename pair
/// writes a delete (`<id>` → null) and a create (null → `<id>`) into `logs/HEAD`,
/// and `git rev-parse HEAD@{1}` then answers the current tip with
/// `warning: log for ref HEAD unexpectedly ended on …` rather than the null id
/// that entry actually records.
///
/// `n == 0` takes the same path for the same reason — there is no newer record, so
/// `cb->ooid` is still the zeroed one `memset()` left — which is why `<ref>@{0}` is
/// always the ref's own value and warns whenever the newest entry disagrees with
/// it.
///
/// `None` is the `!cb.reccnt` arm with `cnt != 0`: an empty log, which is
/// `die(_("log for %s is empty"))` and belongs to [`reflog_reach`].
fn read_ref_at(
    refname: &str,
    entries: &[(ObjectId, ObjectId, gix::date::Time)],
    at_time: i64,
    cnt: i64,
    current: ObjectId,
) -> Option<ReadRefAt> {
    // `memset(&cb, 0, sizeof(cb))`, so the lagged old id starts out null and
    // `cb.oid` points at the value the caller pre-seeded.
    let mut prev_ooid = ObjectId::null(current.kind());
    let mut oid = current;
    let mut reccnt = 0usize;
    let mut cnt = cnt;
    let mut warning = None;
    let mut found = false;

    for (ooid, noid, time) in entries {
        // `cb->tz = tz; cb->date = timestamp;` happen for every record, matched or
        // not, so the warnings below date the record they stopped on.
        let (date, tz) = (time.seconds, time.offset);
        if date <= at_time || cnt == 0 {
            if !prev_ooid.is_null() {
                oid = *noid;
                if prev_ooid != *noid {
                    warning = Some(format!(
                        "log for ref {refname} has gap after {}",
                        crate::porcelain::log::show_date_rfc2822(date, tz)
                    ));
                }
            } else if date == at_time {
                oid = *noid;
            } else if *noid != oid {
                warning = Some(format!(
                    "log for ref {refname} unexpectedly ended on {}",
                    crate::porcelain::log::show_date_rfc2822(date, tz)
                ));
            }
            reccnt += 1;
            found = true;
            break;
        }
        reccnt += 1;
        prev_ooid = *ooid;
        // `if (cb->cnt > 0) cb->cnt--;` — a date selector passes `cnt == -1`, which
        // never reaches zero, so only the timestamp test can stop that walk.
        if cnt > 0 {
            cnt -= 1;
        }
    }

    if reccnt == 0 {
        // ```c
        // if (!cb.reccnt) {
        //         if (cnt == 0) {
        //                 set_read_ref_cutoffs(&cb, 0, 0, "empty reflog");
        //                 return 1;
        //         }
        //         …
        //         die(_("log for %s is empty"), refname);
        // }
        // ```
        //
        // `<ref>@{0}` on an empty log returns 1 with the pre-seeded value intact and
        // `co_cnt == 0`, so `get_oid_basic()`'s `nth == co_cnt` arm accepts it in
        // silence.
        return (cnt == 0).then_some(ReadRefAt {
            oid: current,
            found: false,
            reccnt: 0,
            warning: None,
        });
    }
    if found {
        return Some(ReadRefAt { oid, found: true, reccnt, warning });
    }

    // `refs_for_each_reflog_ent(refs, refname, read_ref_at_ent_oldest, &cb)` — the
    // *forward* walk, stopped at its first record, so this is the oldest entry:
    //
    // ```c
    // oidcpy(cb->oid, ooid);
    // if (cb->at_time && is_null_oid(cb->oid))
    //         oidcpy(cb->oid, noid);
    // ```
    let oldest = entries.last()?;
    let mut oid = oldest.0;
    if at_time != 0 && oid.is_null() {
        oid = oldest.1;
    }
    Some(ReadRefAt { oid, found: false, reccnt, warning: None })
}

/// `repo_dwim_log()`'s `logs_found`: how many `ref_rev_parse_rules` spellings of
/// `name` both resolve and have a reflog. It is the count `get_oid_basic()` tests
/// for the ambiguity warning, and unlike [`crate::porcelain::reflog::dwim_log`] it
/// does not stop at the first hit.
fn dwim_log_matches(repo: &gix::Repository, name: &str) -> usize {
    crate::refname::REV_PARSE_RULES
        .iter()
        .filter(|(prefix, suffix)| {
            let path = format!(
                "{}{name}{}",
                String::from_utf8_lossy(prefix),
                String::from_utf8_lossy(suffix)
            );
            let Some(resolved) = crate::refname::resolve_ref_reading(repo, &path) else {
                return false;
            };
            crate::porcelain::reflog::log_file(repo, &path).is_file()
                || (resolved != path
                    && crate::porcelain::reflog::log_file(repo, &resolved).is_file())
        })
        .count()
}

/// One ref's reflog as `(old, new, time)` triples, newest entry first.
///
/// The zone offset rides along because `read_ref_at_ent()`'s warnings render the
/// entry's own timestamp with `show_date(cb->date, cb->tz, DATE_MODE(RFC2822))`
/// (`refs.c:1136` and `refs.c:1142`), and RFC-2822 prints local time.
fn reflog_lines(
    repo: &gix::Repository,
    full: &str,
) -> Option<Vec<(ObjectId, ObjectId, gix::date::Time)>> {
    // The forward iterator reads the whole file into `buf`; the reverse one wants a
    // fixed-size chunk buffer and yields nothing when handed an empty slice.
    let mut buf: Vec<u8> = Vec::new();
    let iter = repo.refs.reflog_iter(full, &mut buf).ok().flatten()?;
    let mut out = Vec::new();
    for line in iter {
        let Ok(line) = line else { break };
        out.push((
            line.previous_oid(),
            line.new_oid(),
            line.signature.time().unwrap_or(gix::date::Time { seconds: 0, offset: 0 }),
        ));
    }
    // `refs_for_each_reflog_ent_reverse()`: newest entry first.
    out.reverse();
    Some(out)
}

/// The object a full ref name resolves to, following symrefs.
fn resolve_ref_oid(repo: &gix::Repository, full: &str) -> Option<ObjectId> {
    let name = crate::refname::resolve_ref_reading(repo, full)?;
    match repo.refs.try_find(name.as_str()).ok().flatten()?.target {
        gix::refs::Target::Object(id) => Some(id),
        gix::refs::Target::Symbolic(_) => None,
    }
}

/// `repo_interpret_branch_name()`'s two whole-operand rewrites, the pair
/// `substitute_branch_name()` (`refs.c:826-841`) applies before `repo_dwim_ref()`
/// and `repo_dwim_log()` look anything up, and `setup_revisions()` applies again
/// before `add_reflog_for_walk()`:
///
/// ```c
/// int len = repo_interpret_branch_name(the_repository, name, namelen, &buf, &options);
/// if (0 < len && len < namelen && buf.len)
///         strbuf_addstr(&buf, name + len);
/// add_reflog_for_walk(revs->reflog_info, (struct commit *)obj,
///                     buf.buf[0] ? buf.buf : name);
/// ```
/// (`revision.c:308-315`)
///
///   * `interpret_empty_at()`: a bare `@` is `HEAD`;
///   * `interpret_branch_mark()` with `branch_get_upstream()`: `<branch>@{u}` and
///     `<branch>@{upstream}` become the full name of that branch's
///     remote-tracking ref, with anything after the mark carried over.
///
/// `Some(Err(_))` is the `die()` [`upstream_mark_fatal`] words. `None` means git
/// leaves the operand exactly as typed — including `@{push}`, whose `push.default`
/// machinery has outcomes git does not die on and is a separate port.
///
/// `@{-<n>}` is `interpret_nth_prior_checkout()`'s third rewrite and is not here;
/// the reflog reader applies it separately because it needs HEAD's own log.
pub fn interpret_branch_name(
    repo: &gix::Repository,
    name: &str,
) -> Option<Result<String, String>> {
    if name == "@" {
        return Some(Ok("HEAD".to_owned()));
    }
    let at = upstream_mark_at(name)?;
    // `if (memchr(name, ':', at)) return -1;`
    if name[..at].contains(':') {
        return Some(Err(format!("unhandled upstream mark in '{name}'")));
    }
    let branch = &name[..at];
    let full = if branch.is_empty() || branch == "HEAD" {
        match repo.head_name() {
            Ok(Some(full)) => full.as_bstr().to_string(),
            _ => return Some(Err(upstream_mark_fatal(repo, name)?)),
        }
    } else {
        format!("refs/heads/{branch}")
    };
    let Some(upstream) = crate::porcelain::branch::upstream_ref(repo, full.as_str().into()) else {
        return Some(Err(upstream_mark_fatal(repo, name)?));
    };
    // `upstream_mark()` returns the length it matched, and `interpret_branch_name`
    // hands that back so the caller can append whatever followed it.
    let rest = &name[at..];
    let mark_len = ["@{upstream}", "@{u}"]
        .iter()
        .find(|m| rest.len() >= m.len() && rest[..m.len()].eq_ignore_ascii_case(m))
        .map(|m| m.len())?;
    Some(Ok(format!("{}{}", upstream.as_bstr(), &rest[mark_len..])))
}

/// The offset of the `@` that opens an `@{u}`/`@{upstream}` mark in `base`, as
/// `interpret_branch_name()`'s left-to-right scan finds it:
///
/// ```c
/// for (start = name; (at = memchr(start, '@', namelen - (start - name))); start = at + 1) {
///         …
///         len = interpret_branch_mark(r, name, namelen, at - name, buf,
///                                     upstream_mark, branch_get_upstream, options);
/// ```
///
/// with `upstream_mark()`'s own comparison:
///
/// ```c
/// static int upstream_mark(const char *string, int len)
/// {
///         const char *suffix[] = { "@{upstream}", "@{u}" };
///         for (i = 0; i < ARRAY_SIZE(suffix); i++) {
///                 int suffix_len = strlen(suffix[i]);
///                 if (suffix_len <= len && !strncasecmp(string, suffix[i], suffix_len))
///                         return suffix_len;
///         }
///         return 0;
/// }
/// ```
///
/// `suffix_len <= len` rather than `==`, so anything may follow the mark.
pub fn upstream_mark_at(base: &str) -> Option<usize> {
    base.bytes().enumerate().filter(|(_, b)| *b == b'@').map(|(i, _)| i).find(|&i| {
        let rest = &base[i..];
        ["@{upstream}", "@{u}"]
            .iter()
            .any(|mark| rest.len() >= mark.len() && rest[..mark.len()].eq_ignore_ascii_case(mark))
    })
}

/// One ref's whole reflog, newest entry first, reduced to what
/// [`reflog_reach`] needs: each entry's *old* id and its timestamp.
///
/// `None` when the ref cannot be read at all, which `repo_dwim_log()` has
/// already ruled out for every caller here.
fn reflog_entries(
    repo: &gix::Repository,
    full: &str,
) -> Option<Vec<(ObjectId, gix::date::Time)>> {
    let mut reference = repo.try_find_reference(full).ok().flatten()?;
    let mut platform = reference.log_iter();
    let iter = platform.rev().ok().flatten()?;
    let mut out = Vec::new();
    for line in iter {
        let Ok(line) = line else { break };
        out.push((line.previous_oid, line.signature.time));
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// `get_short_oid()`'s ambiguity report (object-name.c:534-566)
// ---------------------------------------------------------------------------

/// `get_short_oid()`'s report for a hex prefix that names more than one object.
///
/// ```c
/// if (!quietly && (status == SHORT_NAME_AMBIGUOUS)) {
///         struct oid_array collect = OID_ARRAY_INIT;
///
///         error(_("short object ID %s is ambiguous"), ds.hex_pfx);
///
///         /*
///          * We may still have ambiguity if we simply saw a series of
///          * candidates that did not satisfy our hint function. In
///          * that case, we still want to show them, so disable the hint
///          * function entirely.
///          */
///         if (!ds.ambiguous)
///                 ds.fn = NULL;
///
///         advise(_("The candidates are:"));
///         repo_for_each_abbrev(ds.repo, ds.hex_pfx, GET_OID_QUIETLY, collect_ambiguous, &collect);
///         QSORT_S(collect.oid, collect.nr, sort_ambiguous, ds.repo);
///         oid_array_for_each(&collect, show_ambiguous_object, &ds);
/// }
/// ```
///
/// It is the *last* thing `get_oid_1()` tries, so it speaks only for a name that
/// nothing else resolved — and it speaks before the caller's own "not a valid
/// object name"/"ambiguous argument", which is why it is a separate call rather
/// than part of a resolution result.
///
/// `ds.fn` is `core.disambiguate`'s filter. Two candidates that both pass it are
/// listed as they are; a filter that leaves *none* is switched off for the listing
/// (the comment above), so `-c core.disambiguate=commit git rev-parse <two blobs>`
/// names both blobs. A filter that leaves exactly one is not ambiguous at all and
/// never reaches here.
///
/// Returns whether anything was printed.
pub fn short_oid_ambiguous(repo: &gix::Repository, name: &str, quietly: bool) -> bool {
    // `get_short_oid()` refuses the name outright outside `[MINIMUM_ABBREV, hexsz]`
    // (`object-name.c:503-506`), so those never reach the report.
    if name.len() < crate::abbrev::MINIMUM_ABBREV
        || name.len() > repo.object_hash().len_in_hex()
        || !name.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(prefix) = gix::hash::Prefix::from_hex(&name.to_ascii_lowercase()) else {
        return false;
    };
    let mut candidates = std::collections::HashSet::new();
    if repo.objects.lookup_prefix(prefix, Some(&mut candidates)).is_err() {
        return false;
    }
    if candidates.len() < 2 {
        return false;
    }
    let kind_of = |id: &ObjectId| repo.find_header(*id).ok().map(|h| h.kind());
    let mut listed: Vec<ObjectId> = match disambiguate_filter(repo) {
        Some(want) => {
            let passed: Vec<ObjectId> =
                candidates.iter().copied().filter(|id| kind_passes(repo, id, want)).collect();
            match passed.len() {
                // `finish_object_disambiguation()` answered, so the name resolved.
                1 => return false,
                // `ds.fn = NULL`: nothing satisfied the hint, list everything.
                0 => candidates.into_iter().collect(),
                _ => passed,
            }
        }
        None => candidates.into_iter().collect(),
    };
    if quietly {
        return true;
    }
    // `sort_ambiguous()` (`object-name.c:453-484`): tags, then commits, then trees
    // and blobs; inside one type, `oidcmp()`.
    listed.sort_by_key(|id| (type_sort_order(kind_of(id)), *id));

    eprintln!("error: short object ID {name} is ambiguous");
    eprintln!("hint: The candidates are:");
    for id in &listed {
        let kind = kind_of(id);
        // `repo_find_unique_abbrev(oid, DEFAULT_ABBREV)`, which widens past a
        // collision rather than cutting at the requested width.
        let hex = crate::abbrev::unique_abbrev(repo, id, crate::abbrev::FALLBACK_DEFAULT_ABBREV);
        let type_name = match kind {
            Some(gix::object::Kind::Tag) => "tag",
            Some(gix::object::Kind::Commit) => "commit",
            Some(gix::object::Kind::Tree) => "tree",
            Some(gix::object::Kind::Blob) => "blob",
            None => "unknown type",
        };
        eprintln!("hint:   {hex} {type_name}{}", ambiguous_object_desc(repo, id, kind));
    }
    true
}

/// `show_ambiguous_object()`'s `desc` (`object-name.c:412-451`): a commit gets
/// `" %ad - %s"` rendered with `DATE_SHORT`, a tag gets `" %s"` of its tag name,
/// and everything else gets nothing.
fn ambiguous_object_desc(
    repo: &gix::Repository,
    id: &ObjectId,
    kind: Option<gix::object::Kind>,
) -> String {
    match kind {
        Some(gix::object::Kind::Commit) => {
            let Ok(commit) = repo.find_object(*id).ok().and_then(|o| o.try_into_commit().ok()).ok_or(()) else {
                return String::new();
            };
            let Ok(decoded) = commit.decode() else { return String::new() };
            let Ok(author) = decoded.author() else { return String::new() };
            let time = author.time().unwrap_or_default();
            // git's `tz` is the `[-+]HHMM` integer off the object header;
            // `gix_actor` carries the offset in seconds.
            let tz = (time.offset / 3600) * 100 + (time.offset % 3600) / 60;
            let date = crate::showdate::show_date(
                time.seconds,
                tz,
                &crate::showdate::DateMode::new(crate::showdate::DateType::Short),
                0,
            );
            let subject = decoded.message().summary().to_string();
            format!(" {date} - {subject}")
        }
        Some(gix::object::Kind::Tag) => {
            let Ok(tag) = repo.find_object(*id).ok().and_then(|o| o.try_into_tag().ok()).ok_or(()) else {
                return String::new();
            };
            match tag.decode() {
                Ok(decoded) => format!(" {}", decoded.name),
                Err(_) => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// `type_sort_order()` (`object-name.c:453-470`): tags first, then commits, then
/// trees and blobs together, then anything unreadable.
fn type_sort_order(kind: Option<gix::object::Kind>) -> u8 {
    match kind {
        Some(gix::object::Kind::Tag) => 0,
        Some(gix::object::Kind::Commit) => 1,
        Some(gix::object::Kind::Tree) => 2,
        Some(gix::object::Kind::Blob) => 3,
        None => 4,
    }
}

/// `core.disambiguate`'s value as the object kind it accepts, or `None` for
/// `none`/absent/unreadable — the settings that filter nothing.
///
/// `committish` and `treeish` peel, so they are not a kind test on their own;
/// [`kind_passes`] is what applies them.
fn disambiguate_filter(repo: &gix::Repository) -> Option<&'static str> {
    let value = repo.config_snapshot().string("core.disambiguate")?;
    let value = value.to_string();
    match value.as_str() {
        "commit" => Some("commit"),
        "committish" => Some("committish"),
        "tree" => Some("tree"),
        "treeish" => Some("treeish"),
        "blob" => Some("blob"),
        _ => None,
    }
}

/// The `disambiguate_*_only()` family (`object-name.c:340-400`): whether one
/// candidate satisfies `core.disambiguate`.
fn kind_passes(repo: &gix::Repository, id: &ObjectId, want: &str) -> bool {
    let Ok(kind) = repo.find_header(*id).map(|h| h.kind()) else { return false };
    use gix::object::Kind::*;
    match want {
        "commit" => kind == Commit,
        // A tag peels towards a commit, so it is committish too.
        "committish" => matches!(kind, Commit | Tag),
        "tree" => kind == Tree,
        "treeish" => matches!(kind, Tree | Commit | Tag),
        "blob" => kind == Blob,
        _ => true,
    }
}
