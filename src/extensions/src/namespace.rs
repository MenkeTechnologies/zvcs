//! Git ref namespaces (`GIT_NAMESPACE` / `git --namespace=<name>`).
//!
//! A namespace lets one physical repository serve several logical ref sets over
//! the wire while sharing a single object store: with `GIT_NAMESPACE=foo`, the
//! refs a peer sees as `refs/heads/main` are stored as
//! `refs/namespaces/foo/refs/heads/main` (`Documentation/gitnamespaces.adoc`).
//!
//! # The boundary, which is the entire difficulty of the feature
//!
//! The intuition "a namespace is set, therefore every ref lookup gets a prefix"
//! is wrong, and getting it wrong is precisely the bug this module exists to
//! prevent. Git consults `GIT_NAMESPACE` **only** in the three programs that
//! serve refs to a peer, and in nothing else:
//!
//!   * `upload-pack.c:892` — `for_each_namespaced_ref_1()` builds
//!     `struct refs_for_each_ref_options opts = { .namespace = get_git_namespace() }`
//!     and iterates with that, for its own advertisement only.
//!     `upload-pack.c:1200,1220` write every advertised name through
//!     `strip_namespace(ref->name)`; `upload-pack.c:1090-1107` resolve HEAD with
//!     `refs_head_ref_namespaced()`; `upload-pack.c:1170` `parse_want_ref()`
//!     re-prefixes an incoming `want-ref` with `get_git_namespace()`.
//!   * `builtin/receive-pack.c` — `update()` builds
//!     `namespaced_name = get_git_namespace() + name` and hands *that* to the ref
//!     transaction, so a push of `refs/heads/x` writes
//!     `refs/namespaces/<ns>/refs/heads/x`. The advertisement uses
//!     `strip_namespace(ref->name)`, and `reject_updates_to_hidden()` /
//!     `check_aliased_update()` prefix likewise.
//!   * `http-backend.c:523,569,591,604` — the dumb ref routes, via
//!     `strip_namespace()`, `.namespace = get_git_namespace()` and
//!     `refs_head_ref_namespaced()`.
//!
//! Every other builtin ignores the namespace completely. That is checkable as a
//! negative rather than inferred: the substring `namespace` does not appear even
//! once in `builtin/for-each-ref.c`, `builtin/show-ref.c`, `builtin/rev-parse.c`,
//! `builtin/branch.c`, `builtin/update-ref.c`, or `builtin/ls-remote.c`. And in
//! `refs.c` the generic `refs_for_each_ref()` / `refs_resolve_ref_unsafe()` apply
//! no prefix at all — only the separately named `refs_head_ref_namespaced()`
//! (`refs.c:1053`, which composes `"%sHEAD"` from `get_git_namespace()`) does.
//!
//! So, concretely, with `GIT_NAMESPACE=ns` set:
//!
//! ```text
//! $ GIT_NAMESPACE=ns git for-each-ref          # lists refs/heads/*, refs/tags/*,
//!                                              # AND refs/namespaces/ns/* unstripped
//! $ GIT_NAMESPACE=ns git update-ref refs/heads/x $C   # writes refs/heads/x, NOT
//!                                                     # refs/namespaces/ns/refs/heads/x
//! $ GIT_NAMESPACE=ns git ls-remote .           # namespace applies: the child
//!                                              # upload-pack strips and filters
//! ```
//!
//! `ls-remote` is worth calling out because it looks like a counterexample and is
//! not: `builtin/ls-remote.c` has no namespace code either. It is a *client*. The
//! namespacing happens in the `upload-pack` it connects to, which inherits
//! `GIT_NAMESPACE` through the environment like any other child process.
//!
//! # Why this is applied here rather than at repository-open time
//!
//! The vendored gitoxide binds `GIT_NAMESPACE` to the whole ref store
//! automatically: `gitoxide.core.refsNamespace` declares
//! `.with_environment_override("GIT_NAMESPACE")`, and
//! `gix/src/open/repository.rs` then does `refs.namespace.clone_from(&config.refs_namespace)`.
//! That is a gitoxide design choice with no git counterpart, and left alone it
//! namespaces `for-each-ref`, `show-ref`, `rev-parse`, `branch` and `update-ref`
//! alike — producing an empty listing and
//! `fatal: The reference 'HEAD' did not exist` whenever the namespace happens to
//! hold no refs. `gix/src/config/cache/util.rs::query_refs_namespace` therefore
//! filters the `Source::EnvOverride` value back out, and the three programs above
//! opt in explicitly through [`apply`].
//!
//! The namespace *expansion* itself is not reimplemented here.
//! `gix_ref::namespace::expand()` already reproduces
//! `environment.c:get_git_namespace()` exactly, including the hierarchical split
//! that turns `foo/bar` into `refs/namespaces/foo/refs/namespaces/bar/` and the
//! `check_refname_format()` validation behind git's
//! `die("bad git namespace path \"%s\"")`.

use anyhow::Result;

/// The raw `GIT_NAMESPACE` value, or `None` when it is unset or empty.
///
/// `environment.c:get_git_namespace()` treats the two the same:
///
/// ```c
/// raw_namespace = getenv(GIT_NAMESPACE_ENVIRONMENT);
/// if (!raw_namespace || !*raw_namespace) {
///         namespace = "";
///         return namespace;
/// }
/// ```
///
/// An empty namespace is the *absence* of a namespace, not a namespace named "",
/// so `GIT_NAMESPACE=` must behave exactly like an unset variable rather than
/// erroring on an invalid refname.
pub fn from_env() -> Option<String> {
    match std::env::var("GIT_NAMESPACE") {
        Ok(value) if !value.is_empty() => Some(value),
        // A non-UTF-8 value cannot round-trip through `expand()`'s refname
        // validation anyway; treating it as unset matches the empty case rather
        // than inventing an error git never produces here.
        _ => None,
    }
}

/// Install the ambient `GIT_NAMESPACE` on `repo`, if one is set.
///
/// Call this only from the ref-serving programs listed in the module docs. Once
/// installed, `gix-ref` prefixes on the way in and strips on the way out —
/// `store/file/find.rs:183-206` composes the namespaced name for a lookup and
/// calls `strip_namespace()` on the result, `store/file/overlay_iter.rs:46-73`
/// does the same for iteration, and `store/file/transaction/prepare.rs:541`
/// carries the namespace into ref *writes*. That covers all three shapes git
/// needs: `strip_namespace()` on the advertisement, `refs_head_ref_namespaced()`
/// for HEAD, and receive-pack's `get_git_namespace() + name` on update.
///
/// Errors mirror `get_git_namespace()`'s `check_refname_format()` rejection,
/// which is a `die()`: `bad git namespace path "<raw>"`.
pub fn apply(repo: &mut gix::Repository) -> Result<()> {
    let Some(raw) = from_env() else { return Ok(()) };
    if repo.set_namespace(raw.as_str()).is_err() {
        crate::git_fatal!("bad git namespace path \"{raw}\"");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `get_git_namespace()` returns `""` for both an unset and an empty
    /// variable, so both must read as "no namespace" rather than as a namespace
    /// whose name is the empty string.
    #[test]
    fn empty_and_unset_are_both_no_namespace() {
        // SAFETY: single-threaded test process; the variable is restored below.
        unsafe {
            std::env::set_var("GIT_NAMESPACE", "");
        }
        assert_eq!(from_env(), None, "an empty GIT_NAMESPACE is not a namespace");
        unsafe {
            std::env::remove_var("GIT_NAMESPACE");
        }
        assert_eq!(from_env(), None, "an unset GIT_NAMESPACE is not a namespace");
    }

    /// The hierarchical expansion from `gitnamespaces.adoc`: "`GIT_NAMESPACE=foo/bar`
    /// will store refs under `refs/namespaces/foo/refs/namespaces/bar/`". This
    /// pins the vendored `expand()` we delegate to, so a change there cannot
    /// silently alter where namespaced refs land.
    #[test]
    fn slashes_expand_hierarchically() {
        let ns = gix::refs::namespace::expand("foo").expect("valid");
        assert_eq!(ns.as_bstr(), "refs/namespaces/foo/");
        let ns = gix::refs::namespace::expand("foo/bar").expect("valid");
        assert_eq!(ns.as_bstr(), "refs/namespaces/foo/refs/namespaces/bar/");
    }
}
