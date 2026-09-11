use gix_actor::Signature;
use gix_date::parse::TimeBuf;
use gix_object::bstr::ByteSlice;
use gix_testtools::tempfile::TempDir;

use super::*;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Convert a hexadecimal hash into its corresponding `ObjectId` or _panic_.
fn hex_to_id(hex: &str) -> gix_hash::ObjectId {
    gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("40 bytes hex")
}

fn empty_store(writemode: WriteReflog) -> Result<(TempDir, file::Store)> {
    let dir = TempDir::new()?;
    let store = file::Store::at(
        dir.path().into(),
        crate::store::init::Options {
            write_reflog: writemode,
            ..Default::default()
        },
    );
    Ok((dir, store))
}

fn reflog_lines(store: &file::Store, name: &str, buf: &mut Vec<u8>) -> Result<Vec<crate::log::Line>> {
    store
        .reflog_iter(name, buf)?
        .expect("existing reflog")
        .map(|l| l.map(crate::log::Line::from))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

const WRITE_MODES: &[WriteReflog] = &[WriteReflog::Normal, WriteReflog::Disable, WriteReflog::Always];

/// The write mode decides autocreation; it is not orthogonal to it.
///
/// `should_autocreate_reflog()` (refs.c:1056-1070) takes the mode as its first
/// parameter and switches on it — `LOG_REFS_ALWAYS` returns 1 for every name,
/// `LOG_REFS_NORMAL` returns 1 only for the four well-known prefixes, and
/// anything else (`LOG_REFS_NONE`, i.e. `core.logAllRefUpdates=false`) falls to
/// `default: return 0`. This test previously asserted the opposite — that
/// `WriteReflog::Disable` still autocreates for `refs/heads/*` — which is
/// upstream gitoxide's decomposition, not git's rule.
///
/// Re-measured against stock 2.55.0, one fresh ref per mode so no update is
/// short-circuited by `previous == new`:
///
/// ```text
/// core.logAllRefUpdates   refs/heads/*   refs/tags/*
/// unset                   CREATED        none
/// false                   none           none
/// true                    CREATED        none
/// always                  CREATED        CREATED
/// ```
#[test]
fn should_autocreate_follows_the_write_mode() -> Result {
    let (_keep, disabled) = empty_store(WriteReflog::Disable)?;
    for any_name in &["HEAD", "refs/heads/main", "refs/remotes/any", "refs/notes/any", "refs/tags/0.1.0"] {
        assert!(
            !disabled.should_autocreate_reflog(Path::new(any_name)),
            "`false` autocreates nothing, not even {any_name}"
        );
    }

    let (_keep, normal) = empty_store(WriteReflog::Normal)?;
    for should_create_name in &["HEAD", "refs/heads/main", "refs/remotes/any", "refs/notes/any"] {
        assert!(normal.should_autocreate_reflog(Path::new(should_create_name)));
    }
    for should_not_create_name in &["FETCH_HEAD", "SOMETHING", "refs/special/this", "refs/tags/0.1.0"] {
        assert!(!normal.should_autocreate_reflog(Path::new(should_not_create_name)));
    }

    let (_keep, always) = empty_store(WriteReflog::Always)?;
    for any_name in &["HEAD", "refs/heads/main", "refs/tags/0.1.0", "refs/special/this", "SOMETHING"] {
        assert!(
            always.should_autocreate_reflog(Path::new(any_name)),
            "`always` autocreates everything, including {any_name}"
        );
    }
    Ok(())
}

#[test]
fn missing_reflog_creates_it_even_if_similarly_named_empty_dir_exists_and_append_log_lines() -> Result {
    for mode in WRITE_MODES {
        let (_keep, store) = empty_store(*mode)?;
        let full_name_str = "refs/heads/main";
        let full_name: &FullNameRef = full_name_str.try_into()?;
        let new = hex_to_id("28ce6a8b26aa170e1de65536fe8abe1832bd3242");
        let committer = Signature {
            name: "committer".into(),
            email: "committer@example.com".into(),
            time: gix_date::parse_header("1234 +0800").unwrap(),
        };
        store.reflog_create_or_append(
            full_name,
            None,
            &new,
            committer.to_ref(&mut TimeBuf::default()).into(),
            b"the message".as_bstr(),
            false,
        )?;

        let mut buf = Vec::new();
        match mode {
            WriteReflog::Normal | WriteReflog::Always => {
                assert_eq!(
                    reflog_lines(&store, full_name_str, &mut buf)?,
                    vec![crate::log::Line {
                        previous_oid: gix_hash::Kind::Sha1.null(),
                        new_oid: new,
                        signature: committer.clone(),
                        message: "the message".into()
                    }]
                );
                let previous = hex_to_id("0000000000000000000000111111111111111111");
                buf.clear();
                store.reflog_create_or_append(
                    full_name,
                    Some(previous),
                    &new,
                    committer.to_ref(&mut TimeBuf::default()).into(),
                    b"next message".as_bstr(),
                    false,
                )?;

                let lines = reflog_lines(&store, full_name_str, &mut buf)?;
                assert_eq!(lines.len(), 2, "now there is another line");
                assert_eq!(
                    lines.last().expect("non-empty"),
                    &crate::log::Line {
                        previous_oid: previous,
                        new_oid: new,
                        signature: committer.clone(),
                        message: "next message".into()
                    }
                );
            }
            WriteReflog::Disable => {
                assert!(
                    store.reflog_iter(full_name, &mut buf)?.is_none(),
                    "there is no logs in disabled mode"
                );
            }
        }

        // create onto existing directory
        let full_name_str = "refs/heads/other";
        let full_name: &FullNameRef = full_name_str.try_into()?;
        let reflog_path = store.reflog_path(full_name_str.try_into().expect("valid"));
        let directory_in_place_of_reflog = reflog_path.join("empty-a").join("empty-b");
        std::fs::create_dir_all(directory_in_place_of_reflog)?;

        buf.clear();
        store.reflog_create_or_append(
            full_name,
            None,
            &new,
            committer.to_ref(&mut TimeBuf::default()).into(),
            b"more complicated reflog creation".as_bstr(),
            false,
        )?;

        match mode {
            WriteReflog::Normal | WriteReflog::Always => {
                assert_eq!(
                    reflog_lines(&store, full_name_str, &mut buf)?.len(),
                    1,
                    "reflog was written despite directory"
                );
                assert!(
                    reflog_path.is_file(),
                    "the empty directory was replaced with the reflog file"
                );
            }
            WriteReflog::Disable => {
                assert!(
                    store.reflog_iter(full_name_str, &mut buf)?.is_none(),
                    "reflog still doesn't exist"
                );
                assert!(
                    store.reflog_iter_rev(full_name_str, &mut buf)?.is_none(),
                    "reflog still doesn't exist"
                );
                assert!(reflog_path.is_dir(), "reflog directory wasn't touched");
            }
        }
    }
    Ok(())
}

#[test]
fn reflog_write_normalizes_committer_name_and_email_like_git() -> Result {
    let (_keep, store) = empty_store(WriteReflog::Always)?;
    let full_name_str = "refs/heads/main";
    let full_name: &FullNameRef = full_name_str.try_into()?;
    let new = hex_to_id("28ce6a8b26aa170e1de65536fe8abe1832bd3242");
    let committer = Signature {
        name: "  committer\n  ".into(),
        email: "  committer@example.com\n  ".into(),
        time: gix_date::parse_header("1234 +0800").unwrap(),
    };

    store.reflog_create_or_append(
        full_name,
        None,
        &new,
        committer.to_ref(&mut TimeBuf::default()).into(),
        b"the message".as_bstr(),
        false,
    )?;

    let mut buf = Vec::new();
    assert_eq!(
        reflog_lines(&store, full_name_str, &mut buf)?,
        vec![crate::log::Line {
            previous_oid: gix_hash::Kind::Sha1.null(),
            new_oid: new,
            signature: Signature {
                name: "committer".into(),
                email: "committer@example.com".into(),
                time: gix_date::parse_header("1234 +0800").unwrap(),
            },
            message: "the message".into()
        }],
        "it trimmed whitespace as basic fix for slightly malformed signatures"
    );
    Ok(())
}
