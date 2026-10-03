//! Which stack a reference lives in: `parse_worktree_ref()` (refs.c:944-990)
//! and `backend_for()` (refs/reftable-backend.c:185-295).

use gix_object::bstr::{BStr, ByteSlice};

use super::{Backend, Error, StackRef, lock, open_stack};

/// `enum ref_worktree_type` (refs.h:1109-1117).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeType {
    /// `REF_WORKTREE_CURRENT`: implicitly per worktree, like `HEAD` or `refs/bisect/…`.
    Current,
    /// `REF_WORKTREE_MAIN`: explicitly the main worktree's, `main-worktree/HEAD`.
    Main,
    /// `REF_WORKTREE_OTHER`: explicitly a named worktree's, `worktrees/<name>/HEAD`.
    Other,
    /// `REF_WORKTREE_SHARED`: the default, like `refs/heads/main`.
    Shared,
}

/// `is_root_ref_syntax()` (refs.c:902-912): only uppercase letters, `-` and `_`.
fn is_root_ref_syntax(name: &[u8]) -> bool {
    name.iter().all(|&c| c.is_ascii_uppercase() || c == b'-' || c == b'_')
}

/// `is_per_worktree_ref()` (refs.c:880-885).
fn is_per_worktree_ref(name: &[u8]) -> bool {
    name.starts_with(b"refs/worktree/") || name.starts_with(b"refs/bisect/") || name.starts_with(b"refs/rewritten/")
}

/// `is_current_worktree_ref()` (refs.c:939-941).
fn is_current_worktree_ref(name: &[u8]) -> bool {
    is_root_ref_syntax(name) || is_per_worktree_ref(name)
}

/// `parse_worktree_ref()` (refs.c:943-990): the kind of `maybe_worktree_ref`,
/// the worktree it names for [`WorktreeType::Other`], and the reference name
/// within its stack.
///
/// `worktrees/<name>` without a reference is [`WorktreeType::Other`] with an
/// empty bare name, which callers treat as an error.
pub fn parse_worktree_ref(maybe_worktree_ref: &BStr) -> (WorktreeType, Option<&BStr>, &BStr) {
    let name = maybe_worktree_ref.as_bytes();
    if let Some(rest) = name.strip_prefix(b"worktrees/") {
        match rest.find_byte(b'/') {
            None => return (WorktreeType::Other, Some(rest.as_bstr()), b"".as_bstr()),
            Some(slash) => {
                let bare = &rest[slash + 1..];
                if is_current_worktree_ref(bare) {
                    return (WorktreeType::Other, Some(rest[..slash].as_bstr()), bare.as_bstr());
                }
            }
        }
    }

    if let Some(bare) = name.strip_prefix(b"main-worktree/") {
        if is_current_worktree_ref(bare) {
            return (WorktreeType::Main, None, bare.as_bstr());
        }
    }

    if is_current_worktree_ref(name) {
        return (WorktreeType::Current, None, maybe_worktree_ref);
    }
    (WorktreeType::Shared, None, maybe_worktree_ref)
}

impl Backend {
    /// `backend_for_worktree()` (refs/reftable-backend.c:185-215): the stack of
    /// the worktree called `worktree_name`, opened on first use.
    fn backend_for_worktree(&self, worktree_name: &BStr) -> Result<StackRef, Error> {
        let mut stacks = self
            .other_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stack) = stacks.get(worktree_name) {
            return Ok(stack.clone());
        }
        let dir = self
            .common_dir
            .join("worktrees")
            .join(gix_path::from_bstr(worktree_name))
            .join("reftable");
        match open_stack(&dir, &self.stack_options) {
            Ok(stack) => {
                self.set_err(None);
                stacks.insert(worktree_name.to_owned(), stack.clone());
                Ok(stack)
            }
            Err(err) => {
                self.set_err(Some(err));
                Err(err.into())
            }
        }
    }

    /// `backend_for()` (refs/reftable-backend.c:229-295): the stack `refname`
    /// lives in, and its name within that stack (`worktrees/wt/HEAD` is `HEAD`
    /// in the stack of `wt`). With `reload`, the stack is reloaded first, as
    /// every reading operation does.
    ///
    /// For the stack of no particular reference, git's `refname == NULL`, use
    /// [`main_stack()`](Backend::main_stack).
    ///
    /// Note that git does not check `refs->err` here; callers that must, do.
    pub fn backend_for<'a>(&self, refname: &'a BStr, reload: bool) -> Result<(StackRef, &'a BStr), Error> {
        let (kind, worktree_name, rewritten) = parse_worktree_ref(refname);
        let stack = match kind {
            // When `worktree_name` is the current worktree, its stack is opened
            // a second time; reading through both is fine, and a write through
            // both finds the stack locked already.
            WorktreeType::Other => self.backend_for_worktree(worktree_name.expect("set for Other"))?,
            // Without a worktree stack, this is the main worktree.
            WorktreeType::Current => match &self.worktree {
                Some(stack) => stack.clone(),
                None => self.main.clone().ok_or(Error::Reftable(gix_reftable::Error::Api))?,
            },
            WorktreeType::Main | WorktreeType::Shared => {
                self.main.clone().ok_or(Error::Reftable(gix_reftable::Error::Api))?
            }
        };
        if reload {
            lock(&stack).reload()?;
        }
        Ok((stack, rewritten))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A linked worktree `wt` of a repository whose stacks are empty
    /// directories: `HEAD` and `refs/bisect/*` go to the worktree's stack,
    /// shared refs and `main-worktree/HEAD` to the main one, and another
    /// worktree's refs to a stack opened under `<common dir>/worktrees/<name>`.
    #[test]
    fn backend_for_routes_by_worktree() {
        let tmp = gix_testtools::tempfile::tempdir().expect("temp dir");
        let common = tmp.path().join("repo.git");
        let wt = common.join("worktrees/wt");
        let other = common.join("worktrees/other");
        for dir in [&common, &wt, &other] {
            std::fs::create_dir_all(dir.join("reftable")).expect("create stack dir");
        }
        let backend = Backend::open(&wt, Some(&common), gix_hash::Kind::Sha1);
        backend.check().expect("stacks open");

        let main = backend.main_stack().expect("main stack");
        let worktree = backend.worktree_stack().expect("a linked worktree has its own stack");
        let route = |name: &str| {
            let (stack, rewritten) = backend.backend_for(name.into(), true).expect("routable");
            (stack, rewritten.to_string())
        };
        let (stack, name) = route("HEAD");
        assert!(Arc::ptr_eq(&stack, &worktree) && name == "HEAD");
        let (stack, _) = route("refs/bisect/bad");
        assert!(Arc::ptr_eq(&stack, &worktree));
        let (stack, _) = route("refs/heads/main");
        assert!(Arc::ptr_eq(&stack, &main));
        let (stack, name) = route("main-worktree/HEAD");
        assert!(Arc::ptr_eq(&stack, &main) && name == "HEAD");
        let (stack, name) = route("worktrees/other/HEAD");
        assert_eq!(lock(&stack).dir(), other.join("reftable"));
        assert_eq!(name, "HEAD");
        let (again, _) = route("worktrees/other/refs/bisect/x");
        assert!(Arc::ptr_eq(&stack, &again), "other worktrees' stacks are opened once");

        // The main worktree has no stack of its own.
        let backend = Backend::open(&common, None, gix_hash::Kind::Sha1);
        let (stack, _) = backend.backend_for("HEAD".into(), false).expect("routable");
        assert!(Arc::ptr_eq(&stack, &backend.main_stack().expect("main stack")));
        assert!(backend.worktree_stack().is_none());
    }

    #[test]
    fn parse_worktree_ref_kinds() {
        let parse = |name: &str| {
            let (kind, wt, bare) = parse_worktree_ref(name.into());
            (kind, wt.map(|w| w.to_string()), bare.to_string())
        };
        assert_eq!(parse("HEAD"), (WorktreeType::Current, None, "HEAD".into()));
        assert_eq!(parse("refs/bisect/bad"), (WorktreeType::Current, None, "refs/bisect/bad".into()));
        assert_eq!(parse("refs/heads/main"), (WorktreeType::Shared, None, "refs/heads/main".into()));
        assert_eq!(parse("main-worktree/HEAD"), (WorktreeType::Main, None, "HEAD".into()));
        assert_eq!(
            parse("main-worktree/refs/heads/x"),
            (WorktreeType::Shared, None, "main-worktree/refs/heads/x".into()),
            "only per-worktree refs can be addressed through main-worktree/"
        );
        assert_eq!(
            parse("worktrees/wt/refs/worktree/x"),
            (WorktreeType::Other, Some("wt".into()), "refs/worktree/x".into())
        );
        assert_eq!(
            parse("worktrees/wt"),
            (WorktreeType::Other, Some("wt".into()), String::new()),
            "a worktree without a ref is an error the caller sees as an empty name"
        );
        assert_eq!(
            parse("worktrees/wt/refs/heads/x"),
            (WorktreeType::Shared, None, "worktrees/wt/refs/heads/x".into())
        );
    }
}
