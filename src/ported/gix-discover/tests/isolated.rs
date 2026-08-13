use std::path::{Path, PathBuf};

use gix_discover::upwards::Options;
use serial_test::serial;

#[test]
#[serial]
fn in_cwd_upwards_from_nested_dir() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh")?;

    let _keep = gix_testtools::set_current_dir(repo)?;
    for dir in ["subdir", "some/very/deeply/nested/subdir"] {
        let (repo_path, _trust) = gix_discover::upwards(Path::new(dir))?;
        assert_eq!(
            repo_path.kind(),
            gix_discover::repository::Kind::WorkTree { linked_git_dir: None },
        );
        assert_eq!(repo_path.as_ref(), Path::new("."), "{dir}");
    }
    Ok(())
}

#[test]
#[serial]
fn upwards_bare_repo_with_index() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh")?;

    let _keep = gix_testtools::set_current_dir(repo.join("bare-with-index.git"))?;
    let (repo_path, _trust) = gix_discover::upwards(".".as_ref())?;
    assert_eq!(
        repo_path.kind(),
        gix_discover::repository::Kind::PossiblyBare,
        "bare stays bare, even with index, as it resolves the path as needed in this special case"
    );
    Ok(())
}

#[test]
#[serial]
fn in_cwd_upwards_bare_repo_without_index() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh")?;

    let _keep = gix_testtools::set_current_dir(repo.join("bare.git"))?;
    let (repo_path, _trust) = gix_discover::upwards(".".as_ref())?;
    assert_eq!(repo_path.kind(), gix_discover::repository::Kind::PossiblyBare);
    Ok(())
}

#[test]
#[serial]
fn in_cwd_upwards_nonbare_repo_without_index() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh")?;

    let _keep = gix_testtools::set_current_dir(repo.join("non-bare-without-index"))?;
    let (repo_path, _trust) = gix_discover::upwards(".".as_ref())?;
    assert_eq!(
        repo_path.kind(),
        gix_discover::repository::Kind::WorkTree { linked_git_dir: None },
    );
    Ok(())
}

#[test]
#[serial]
fn upwards_with_relative_directories_and_optional_ceiling() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh")?;

    let _keep = gix_testtools::set_current_dir(repo.join("some"))?;
    let cwd = std::env::current_dir()?;

    for (search_dir, ceiling_dir_component) in [
        (".", ".."),
        (".", "./.."),
        ("./.", "./.."),
        (".", "./does-not-exist/../.."),
        ("./././very/deeply/nested/subdir", ".."),
        ("very/deeply/nested/subdir", ".."),
    ] {
        let search_dir = Path::new(search_dir);
        let ceiling_dir = cwd.join(ceiling_dir_component);
        // Every spelling of `..` here names the work dir itself, which is the first directory the
        // ceiling excludes from the search — stock 2.55.0 dies `not a git repository` for the
        // same pair, e.g. `GIT_CEILING_DIRECTORIES=<work_dir> git rev-parse --git-dir` run in
        // `<work_dir>/some`.
        let err = gix_discover::upwards_opts(
            search_dir,
            Options {
                ceiling_dirs: vec![ceiling_dir],
                ..Default::default()
            },
        )
        .expect_err("however it is spelled, the ceiling is the work dir and is not searched");
        assert!(matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
        ));

        // One directory higher the very same search succeeds, so the failure above is the ceiling
        // taking effect rather than the path spelling defeating the search.
        let (repo_path, _trust) = gix_discover::upwards_opts(
            search_dir,
            Options {
                ceiling_dirs: vec![cwd.join(ceiling_dir_component).join("..")],
                ..Default::default()
            },
        )
        .expect("a ceiling above the work dir leaves the work dir itself searchable");
        assert_repo_is_current_workdir(repo_path, Path::new(".."));

        let (repo_path, _trust) =
            gix_discover::upwards_opts(search_dir, Default::default()).expect("without ceiling dir we see the same");
        assert_repo_is_current_workdir(repo_path, Path::new(".."));

        let err = gix_discover::upwards_opts(
            search_dir,
            Options {
                ceiling_dirs: vec![PathBuf::from("..")],
                ..Default::default()
            },
        )
        .expect_err("purely relative ceiling dirs work as well, and this one is the work dir too");
        assert!(matches!(
            err,
            gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
        ));

        let err = gix_discover::upwards_opts(
            search_dir,
            Options {
                ceiling_dirs: vec![PathBuf::from(".")],
                ..Default::default()
            },
        )
        .unwrap_err();

        if search_dir.parent() == Some(".".as_ref()) || search_dir.parent() == Some("".as_ref()) {
            assert!(matches!(err, gix_discover::upwards::Error::NoMatchingCeilingDir));
        } else {
            assert!(matches!(
                err,
                gix_discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
            ));
        }
    }

    Ok(())
}

#[test]
#[serial]
fn unc_paths_are_handled_on_windows() -> gix_testtools::Result {
    let repo = gix_testtools::scripted_fixture_read_only("make_basic_repo.sh").unwrap();

    let _keep = gix_testtools::set_current_dir(repo.join("some/very/deeply/nested/subdir")).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let parent = cwd.parent().unwrap();
    // all discoveries should fail, as they'll hit `parent` before finding a git repository.

    // dir: normal, ceiling: normal
    let res = gix_discover::upwards_opts(
        &cwd,
        Options {
            ceiling_dirs: vec![parent.to_path_buf()],
            match_ceiling_dir_or_error: false,
            ..Default::default()
        },
    );
    assert!(res.is_err(), "{res:?}");

    let parent = parent.canonicalize().unwrap();
    // dir: normal, ceiling: extended
    let res = gix_discover::upwards_opts(
        &cwd,
        Options {
            ceiling_dirs: vec![parent],
            match_ceiling_dir_or_error: false,
            ..Default::default()
        },
    );
    assert!(res.is_err(), "{res:?}");

    let cwd = cwd.canonicalize().unwrap();

    let parent = cwd.parent().unwrap();
    // dir: extended, ceiling: normal
    let res = gix_discover::upwards_opts(
        &cwd,
        Options {
            ceiling_dirs: vec![parent.to_path_buf()],
            match_ceiling_dir_or_error: false,
            ..Default::default()
        },
    );
    assert!(res.is_err(), "{res:?}");

    let parent = parent.canonicalize().unwrap();
    // dir: extended, ceiling: extended
    let res = gix_discover::upwards_opts(
        &cwd,
        Options {
            ceiling_dirs: vec![parent],
            match_ceiling_dir_or_error: false,
            ..Default::default()
        },
    );
    assert!(res.is_err(), "{res:?}");
    Ok(())
}

fn assert_repo_is_current_workdir(path: gix_discover::repository::Path, work_dir: &Path) {
    assert_eq!(
        path.into_repository_and_work_tree_directories().1.expect("work dir"),
        work_dir,
    );
}
