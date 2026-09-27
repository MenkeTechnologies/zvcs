//! `upload_pack_config()` (upload-pack.c:1334-1379), the callback
//! `get_upload_pack_config()` runs through `repo_config()` after `enter_repo()`.
//!
//! Its readers die on what they refuse: `git_config_bool()` for the
//! `uploadpack.allow*` switches, `core.precomposeunicode` and
//! `transfer.advertisesid`, `git_config_int()` for `uploadpack.keepalive`,
//! `parse_object_filter_config()` for `uploadpackfilter.*` (:1297-1332), and
//! `parse_hide_refs_config()` (refs.c:1688-1708) for a valueless
//! `uploadpack.hideRefs` / `transfer.hideRefs`. The file is named `config`
//! because `enter_repo()` chdir'd into the git directory.
//!
//! Where it runs decides what gets out first: `upload_pack()` before the v0
//! advertisement (:1408), so v1 has already written `version 1`;
//! `upload_pack_advertise()` while the v2 capability list is written (:1841),
//! so `version 2`, `agent` and `ls-refs` are out; `upload_pack_v2()` at the
//! start of a v2 `fetch` (:1779). v2 `ls-refs` reads only the hideRefs keys
//! (ls-refs.c:148-171), so `uploadpack.keepAlive=bogus` does not stop it.
//! zvcs had no callback and served every one of these.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `r`, a repository with one commit on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-upload-pack-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "r"], None, b"");
        std::fs::write(f.root.join("r/a"), "a\n").unwrap();
        f.run(&["-C", "r", "add", "a"], None, b"");
        f.run(&["-C", "r", "commit", "-q", "-m", "a"], None, b"");
        f
    }

    fn run(&self, args: &[&str], protocol: Option<&str>, stdin: &[u8]) -> (Vec<u8>, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C");
        if let Some(p) = protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn append_config(&self, text: &str) -> usize {
        let path = self.root.join("r/.git/config");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str(text);
        std::fs::write(&path, &config).unwrap();
        config.lines().count()
    }

    fn advertise(&self, protocol: Option<&str>) -> (Vec<u8>, String, i32) {
        self.run(&["upload-pack", "--advertise-refs", "r"], protocol, b"")
    }
}

const KEEPALIVE: &str =
    "fatal: bad numeric config value 'bogus' for 'uploadpack.keepalive' in file config: invalid unit\n";

#[test]
fn keepalive_dies_before_each_advertisement() {
    let f = Fixture::new("keepalive");
    f.run(&["-C", "r", "config", "uploadpack.keepAlive", "bogus"], None, b"");

    assert_eq!(f.advertise(None), (Vec::new(), KEEPALIVE.to_string(), 128));
    assert_eq!(
        f.advertise(Some("version=1")),
        (b"000eversion 1\n".to_vec(), KEEPALIVE.to_string(), 128)
    );

    // v2: the capabilities ahead of `fetch` are out, no flush.
    let (out, err, code) = f.advertise(Some("version=2"));
    assert_eq!((err.as_str(), code), (KEEPALIVE, 128));
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("000eversion 2\n"), "{out:?}");
    assert!(out.ends_with("0013ls-refs=unborn\n"), "{out:?}");
    assert!(!out.contains("fetch"), "{out:?}");

    // A v2 `fetch` request dies on it; `ls-refs` does not read the key.
    let fetch = b"0012command=fetch\n0001000ddone\n0000";
    let (out, err, code) = f.run(&["upload-pack", "--stateless-rpc", "r"], Some("version=2"), fetch);
    assert_eq!((out.as_slice(), err.as_str(), code), (&b""[..], KEEPALIVE, 128));
    let ls_refs = b"0014command=ls-refs\n00010000";
    let (out, err, code) = f.run(&["upload-pack", "--stateless-rpc", "r"], Some("version=2"), ls_refs);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(String::from_utf8(out).unwrap().ends_with(" refs/heads/main\n0000"));
}

#[test]
fn each_reader_refuses_its_own_kind_of_value() {
    for (key, message) in [
        ("uploadpack.allowFilter", "fatal: bad boolean config value 'bogus' for 'uploadpack.allowfilter'\n"),
        ("uploadpack.allowAnySHA1InWant", "fatal: bad boolean config value 'bogus' for 'uploadpack.allowanysha1inwant'\n"),
        ("transfer.advertiseSID", "fatal: bad boolean config value 'bogus' for 'transfer.advertisesid'\n"),
        ("core.precomposeUnicode", "fatal: bad boolean config value 'bogus' for 'core.precomposeunicode'\n"),
        ("uploadpackfilter.allow", "fatal: bad boolean config value 'bogus' for 'uploadpackfilter.allow'\n"),
        ("uploadpackfilter.blob:none.allow", "fatal: bad boolean config value 'bogus' for 'uploadpackfilter.blob:none.allow'\n"),
        (
            "uploadpackfilter.tree.maxDepth",
            "fatal: bad numeric config value 'bogus' for 'uploadpackfilter.tree.maxdepth' in file config: invalid unit\n",
        ),
    ] {
        let f = Fixture::new("readers");
        f.run(&["-C", "r", "config", key, "bogus"], None, b"");
        assert_eq!(f.advertise(None), (Vec::new(), message.to_string(), 128), "{key}");
    }
}

#[test]
fn a_valueless_string_is_an_error_then_the_origin() {
    for (section, key) in [
        ("[uploadpack]\n\thideRefs\n", "uploadpack.hiderefs"),
        ("[transfer]\n\thideRefs\n", "transfer.hiderefs"),
        ("[uploadpackfilter \"tree\"]\n\tmaxDepth\n", "uploadpackfilter.tree.maxdepth"),
    ] {
        let f = Fixture::new("nonbool");
        let line = f.append_config(section);
        let want = format!(
            "error: missing value for '{key}'\n\
             fatal: bad config variable '{key}' in file 'config' at line {line}\n"
        );
        assert_eq!(f.advertise(None), (Vec::new(), want.clone(), 128), "{key}");
        // `ls-refs` reads the hideRefs keys itself.
        if key.ends_with("hiderefs") {
            let ls_refs = b"0014command=ls-refs\n00010000";
            let (out, err, code) =
                f.run(&["upload-pack", "--stateless-rpc", "r"], Some("version=2"), ls_refs);
            assert_eq!((out.as_slice(), err.as_str(), code), (&b""[..], want.as_str(), 128), "{key}");
        }
    }
}
