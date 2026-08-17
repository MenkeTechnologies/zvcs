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
fn ambiguity_base(spec: &str) -> &str {
    // `cp = strchr(name, ':')`, with the empty left side left alone — `:/text`
    // and `:<path>` are their own forms and never reach `get_oid_basic()`.
    let base = match spec.find(':').filter(|at| *at > 0) {
        Some(at) => &spec[..at],
        None => spec,
    };
    strip_navigation(base)
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
fn strip_navigation(mut base: &str) -> &str {
    loop {
        // `peel_onion()`: at least four characters, ending in `}`, and the
        // *rightmost* `{` whose predecessor is `^` opens the type name.
        if base.len() >= 4 && base.ends_with('}') {
            if let Some(at) =
                base.rfind("^{").filter(|at| *at > 0 && peel_onion_type(&base[at + 2..]))
            {
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
    repo.rev_parse_single(spec).ok().map(|id| id.detach())
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
    if sel.starts_with('-') || matches!(sel, "u" | "upstream" | "push") {
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
pub fn reflog_reach(repo: &gix::Repository, spec: &str) -> Option<ReflogReach> {
    let (base, sel) = split_reflog_selector(spec)?;
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
    match reflog_reach(repo, ambiguity_base(spec))? {
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
/// `@{push}` reaches the same `interpret_branch_mark()` but a different
/// `get_data`: `branch_get_push()` runs `branch_get_push_1()`'s `push.default`
/// machinery, whose outcomes include several git does *not* die on
/// (`push.default=matching` on a branch with no upstream resolves to the branch's
/// own refname and the operand then fails ordinarily). That is a separate port
/// and is deliberately not answered here.
pub fn upstream_mark_fatal(repo: &gix::Repository, spec: &str) -> Option<String> {
    let base = ambiguity_base(spec);
    let at = upstream_mark_at(base)?;
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
fn upstream_mark_at(base: &str) -> Option<usize> {
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
