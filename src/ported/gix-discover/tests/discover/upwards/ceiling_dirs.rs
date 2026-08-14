use std::path::Path;

use gix_discover::upwards::Options;

use crate::upwards::repo_path;

fn assert_repo_is_current_workdir(path: gix_discover::repository::Path, work_dir: &Path) {
    assert_eq!(
        path.into_repository_and_work_tree_directories()
            .1
            .expect("work dir")
            .file_name(),
        work_dir.file_name()
    );
}

/// The ceiling directory is the first directory *not* searched, even when the repository is
/// exactly there. `setup_git_directory_gently_1()` stops on `offset <= ceil_offset`, where
/// `offset` is the length of the parent it is about to examine, so a parent as long as the
/// ceiling is never examined. Measured against stock 2.55.0 with the same layout:
///
/// ```text
/// $ cd <work_dir>/some/very/deeply/nested/subdir
/// $ GIT_CEILING_DIRECTORIES=<work_dir> git rev-parse --git-dir
/// fatal: not a git repository (or any of the parent directories): .git    # exit 128
/// ```
#[test]
fn git_dir_candidate_at_the_ceiling_is_not_discovered() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![work_dir.clone()],
            ..Default::default()
        },
    )
    .expect_err("the repository sits at the ceiling itself, which is never searched");
    assert!(matches!(
        err,
        gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 5, .. }
    ));

    Ok(())
}

#[test]
fn ceiling_dir_is_ignored_if_we_are_standing_on_the_ceiling_and_no_match_is_required() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    // the ceiling dir is equal to the input dir, which itself doesn't contain a repository.
    // But we can ignore that just like git does (see https://github.com/GitoxideLabs/gitoxide/pull/723 for more information)
    // and imagine us to 'stand on the ceiling', hence we are already past it.
    let (repo_path, _trust) = gix_discover::upwards_opts(
        &dir.clone(),
        Options {
            ceiling_dirs: vec![dir],
            match_ceiling_dir_or_error: false,
            ..Default::default()
        },
    )
    .expect("ceiling dir should be skipped");
    assert_repo_is_current_workdir(repo_path, &work_dir);

    Ok(())
}

#[test]
fn discovery_fails_if_we_require_a_matching_ceiling_dir_but_are_standing_on_it() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let err = gix_discover::upwards_opts(
        &dir.clone(),
        Options {
            ceiling_dirs: vec![dir],
            match_ceiling_dir_or_error: true,
            ..Default::default()
        },
    )
    .unwrap_err();

    assert!(
        matches!(err, gix_discover::upwards::Error::NoMatchingCeilingDir),
        "since standing on the ceiling dir doesn't match it, we get exactly the semantically correct error"
    );
    Ok(())
}

#[test]
fn ceiling_dir_limits_are_respected_and_prevent_discovery() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");

    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![work_dir.join("some/../some")],
            ..Default::default()
        },
    )
    .expect_err("ceiling dir prevents discovery as it ends on level too early, and they are also absolutized");
    assert!(
        matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 4, .. }
        ),
        "the search stops *at* the ceiling, so the four directories between it and the start are all it sees"
    );

    Ok(())
}

/// Ceilings that match nothing on the way up are simply ignored — that is what `git` does with
/// them (`ceil_offset` stays `-1`), and it is what `match_ceiling_dir_or_error: false` asks for
/// here. Both halves are measured against stock 2.55.0 from
/// `<work_dir>/some/very/deeply/nested/subdir`: with `<work_dir>` among the ceilings it dies
/// `not a git repository`, and with only the three non-matching ones it prints `<work_dir>/.git`.
#[test]
fn no_matching_ceiling_dir_error_can_be_suppressed() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let unrelated = || {
        vec![
            work_dir.join("some/very/deeply/nested/subdir/too-deep"),
            work_dir.join("some/very/deeply/nested/unrelated-dir"),
            work_dir.join("a/completely/unrelated/dir"),
        ]
    };

    let (repo_path, _trust) = gix_discover::upwards_opts(
        &dir,
        Options {
            match_ceiling_dir_or_error: false,
            ceiling_dirs: unrelated(),
            ..Default::default()
        },
    )
    .expect("ceilings that prefix nothing leave the search unrestricted");
    assert_repo_is_current_workdir(repo_path, &work_dir);

    let mut ceiling_dirs = vec![work_dir.canonicalize()?];
    ceiling_dirs.extend(unrelated());
    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            match_ceiling_dir_or_error: false,
            ceiling_dirs,
            ..Default::default()
        },
    )
    .expect_err("the one ceiling that does match still stops the search at the work dir");
    assert!(
        matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 5, .. }
        ),
        "suppressing the no-match error does not turn a matching ceiling into a non-matching one"
    );

    Ok(())
}

#[test]
fn more_restrictive_ceiling_dirs_overrule_less_restrictive_ones() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![work_dir.clone(), work_dir.join("some")],
            ..Default::default()
        },
    )
    .expect_err("more restrictive ceiling dirs overrule less restrictive ones");
    assert!(
        matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 4, .. }
        ),
        "`some` is the deeper of the two and is the one that stops the search, exactly as git takes \
         the *longest* ancestor in `longest_ancestor_length()`"
    );

    Ok(())
}

#[test]
fn ceiling_dirs_are_not_processed_differently_than_the_git_dir_candidate() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir/../../../../../..");
    let (repo_path, _trust) = gix_discover::upwards_opts(
        &dir,
        Options {
            match_ceiling_dir_or_error: false,
            ceiling_dirs: vec![Path::new("./some").into()],
            ..Default::default()
        },
    )
    .expect("the repo can be discovered because the relative ceiling doesn't _look_ like it has something to do with the git dir candidate");

    assert_ne!(
        &repo_path.as_ref().canonicalize()?,
        &work_dir,
        "a relative path that climbs above the test repo should yield the gitoxide repo"
    );

    Ok(())
}

/// A ceiling that follows an empty entry in `GIT_CEILING_DIRECTORIES` is compared to the search
/// directory exactly as it was spelled, so one that only *names* an ancestor after normalization
/// does not stop the search. git's `canonicalize_ceiling_entry()` returns early for those entries
/// with `/* Keep entry but do not canonicalize it */`, and `longest_ancestor_length()` then does a
/// plain `strncmp()` against them.
///
/// Measured against stock 2.55.0 with a repository at `R` and the search starting in `R/a/b`:
///
/// ```text
/// $ GIT_CEILING_DIRECTORIES=:R        git rev-parse --git-dir   # fatal: not a git repository, 128
/// $ GIT_CEILING_DIRECTORIES=:R/       git rev-parse --git-dir   # fatal: not a git repository, 128
/// $ GIT_CEILING_DIRECTORIES=:R//      git rev-parse --git-dir   # R/.git, exit 0
/// $ GIT_CEILING_DIRECTORIES=:R/.      git rev-parse --git-dir   # R/.git, exit 0
/// $ GIT_CEILING_DIRECTORIES=:R/a/..   git rev-parse --git-dir   # R/.git, exit 0
/// ```
///
/// The same five spellings in [`Options::ceiling_dirs`] would all stop the search, which is that
/// list's own documented behaviour and is what the rest of this file measures.
#[test]
fn verbatim_ceiling_dirs_are_compared_exactly_as_spelled() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let cwd = std::env::current_dir()?;
    let absolute = gix_path::realpath_opts(&work_dir, &cwd, 8)?;
    // Appended to the path's bytes rather than to a `String`, because the whole point of these
    // entries is that they are compared as the exact bytes they were spelled with.
    let spelled = |suffix: &str| -> Vec<std::path::PathBuf> {
        let mut spelling = absolute.clone().into_os_string();
        spelling.push(suffix);
        vec![spelling.into()]
    };

    for exact in ["", "/"] {
        let err = gix_discover::upwards_opts(
            &dir,
            Options {
                ceiling_dirs_verbatim: spelled(exact),
                ..Default::default()
            },
        )
        .expect_err("spelled as the ancestor itself, the ceiling matches and stops the search");
        assert!(
            matches!(
                err,
                gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 5, .. }
            ),
            "`{exact}`: a lone trailing slash is part of a root's name, so git excludes it from the \
             compared length rather than letting it fail the match"
        );
    }

    for renamed in ["//", "/.", "/some/.."] {
        let (repo_path, _trust) = gix_discover::upwards_opts(
            &dir,
            Options {
                match_ceiling_dir_or_error: false,
                ceiling_dirs_verbatim: spelled(renamed),
                ..Default::default()
            },
        )
        .unwrap_or_else(|err| panic!("`{renamed}` names the ancestor but is not spelled as it, so it must not match: {err}"));
        assert_repo_is_current_workdir(repo_path, &work_dir);
    }

    Ok(())
}

/// The two lists are one ceiling set: the deepest match out of either stops the search, which is
/// git taking the *longest* ancestor over the whole of `GIT_CEILING_DIRECTORIES` regardless of
/// where the empty entry falls. Measured against stock 2.55.0 from `R/a/b` with a repository at
/// `R`, where the verbatim half is the deeper of the two and is the one that decides:
///
/// ```text
/// $ GIT_CEILING_DIRECTORIES=<R's parent>::R/a git rev-parse --git-dir  # fatal: …, exit 128
/// ```
#[test]
fn the_deepest_ceiling_wins_across_both_lists() -> crate::Result {
    let work_dir = repo_path()?;
    let dir = work_dir.join("some/very/deeply/nested/subdir");
    let cwd = std::env::current_dir()?;
    let absolute = gix_path::realpath_opts(&work_dir, &cwd, 8)?;

    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![absolute.clone()],
            ceiling_dirs_verbatim: vec![absolute.join("some/very")],
            ..Default::default()
        },
    )
    .expect_err("a ceiling in either list stops the search");
    assert!(
        matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 3, .. }
        ),
        "the verbatim `some/very` is deeper than the normalized work dir, so it is the one that stops the search"
    );

    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![absolute.join("some/very")],
            ceiling_dirs_verbatim: vec![absolute],
            ..Default::default()
        },
    )
    .expect_err("and the same pair the other way around picks the same ceiling");
    assert!(matches!(
        err,
        gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 3, .. }
    ));

    Ok(())
}

#[test]
fn no_matching_ceiling_dirs_errors_by_default() -> crate::Result {
    let relative_work_dir = repo_path()?;
    let dir = relative_work_dir.join("some");
    let res = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec!["/something/somewhere".into()],
            ..Default::default()
        },
    );

    assert!(
        matches!(res, Err(gix_discover::upwards::Error::NoMatchingCeilingDir)),
        "the canonicalized ceiling dir doesn't have the same root as the git dir candidate, and can never match."
    );
    Ok(())
}

/// A ceiling and a search directory are compared after both have been made absolute, so the same
/// pair matches whichever of the two is spelled relatively. The match is observable as the ceiling
/// taking effect: it is the work dir, one level above the search dir, so the search sees only the
/// search dir itself and then stops. Stock 2.55.0 agrees, from `<work_dir>/some`:
///
/// ```text
/// $ GIT_CEILING_DIRECTORIES=<work_dir> git rev-parse --git-dir
/// fatal: not a git repository (or any of the parent directories): .git    # exit 128
/// ```
///
/// A ceiling that did *not* match would leave the search unrestricted and find `<work_dir>/.git`,
/// which is what the third call here shows.
#[test]
fn ceilings_are_adjusted_to_match_search_dir() -> crate::Result {
    let relative_work_dir = repo_path()?;
    let cwd = std::env::current_dir()?;
    let absolute_ceiling_dir = gix_path::realpath_opts(&relative_work_dir, &cwd, 8)?;
    let dir = relative_work_dir.join("some");
    assert!(dir.is_relative());
    let err = gix_discover::upwards_opts(
        &dir,
        Options {
            ceiling_dirs: vec![absolute_ceiling_dir],
            ..Default::default()
        },
    )
    .expect_err("the absolute ceiling matches the relative search dir, and stops it one level up");
    assert!(matches!(
        err,
        gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 1, .. }
    ));

    assert!(relative_work_dir.is_relative());
    let absolute_dir = gix_path::realpath_opts(relative_work_dir.join("some").as_ref(), &cwd, 8)?;
    let err = gix_discover::upwards_opts(
        &absolute_dir,
        Options {
            ceiling_dirs: vec![relative_work_dir.clone()],
            ..Default::default()
        },
    )
    .expect_err("and the relative ceiling matches the absolute search dir just the same");
    assert!(matches!(
        err,
        gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { ceiling_height: 1, .. }
    ));

    let (repo_path, _trust) = gix_discover::upwards_opts(
        &absolute_dir,
        Options {
            match_ceiling_dir_or_error: false,
            ceiling_dirs: vec![relative_work_dir.join("unrelated")],
            ..Default::default()
        },
    )
    .expect("a ceiling that matches nothing leaves the very same search unrestricted");
    assert_repo_is_current_workdir(repo_path, &relative_work_dir);
    Ok(())
}
