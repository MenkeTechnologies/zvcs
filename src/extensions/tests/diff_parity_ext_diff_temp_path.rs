//! The temporary file an external diff program is handed.
//!
//! `prep_temp_blob()` (diff.c:4666-4693) writes the blob with
//! `mks_tempfile_dt("git-blob-XXXXXX", basename)`, which builds the directory
//! as `"%s/%s"` over `getenv("TMPDIR")` — `/tmp` when unset — and hands it to
//! `mkdtemp()` (tempfile.c:205-230): six characters of `[a-zA-Z0-9]`, mode
//! 0700. So a `TMPDIR` ending in `/` reaches the program as `…//git-blob-…`.
//! zvcs built the directory with `std::env::temp_dir().join(…)`, which
//! normalised the slash away, named it with six hex digits, and left the mode to
//! the umask.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit of `f`, then a worktree edit, and a driver that prints the
    /// pre-image's temporary path and its directory's permission bits.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-ext-diff-temp-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("tmp")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], None);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        f.run(&["add", "sub/f"], None);
        f.run(&["commit", "-q", "-m", "init"], None);
        std::fs::write(f.work.join("sub/f"), "b\n").unwrap();
        let script = f.root.join("ext.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"$2\"\nls -ld \"$(dirname \"$2\")\" | cut -c1-10\n")
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn run(&self, args: &[&str], tmpdir: Option<&str>) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_EXTERNAL_DIFF")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C");
        match tmpdir {
            Some(t) => cmd.env("TMPDIR", t),
            None => cmd.env_remove("TMPDIR"),
        };
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The driver's two lines for `git diff`, with the six random characters of
    /// the directory name checked and then masked.
    fn temp_path(&self, tmpdir: Option<&str>) -> (String, String) {
        let pgm = format!("diff.external={}", self.root.join("ext.sh").display());
        let (out, err, code) = self.run(&["-c", &pgm, "diff"], tmpdir);
        assert_eq!((err.as_str(), code), ("", 0));
        let (path, mode) = out.split_once('\n').unwrap();
        let at = path.find("git-blob-").expect("a git-blob- directory") + "git-blob-".len();
        let random = &path[at..at + 6];
        assert!(random.bytes().all(|b| b.is_ascii_alphanumeric()), "{path}");
        let masked = format!("{}XXXXXX{}", &path[..at], &path[at + 6..]);
        (masked, mode.trim_end().to_owned())
    }
}

#[test]
fn tmpdir_is_joined_verbatim_with_a_slash() {
    let f = Fixture::new("slash");
    let tmp = format!("{}/", f.root.join("tmp").display());
    assert_eq!(
        f.temp_path(Some(&tmp)),
        (format!("{tmp}/git-blob-XXXXXX/f"), "drwx------".to_owned())
    );
    let bare = f.root.join("tmp").display().to_string();
    assert_eq!(f.temp_path(Some(&bare)).0, format!("{bare}/git-blob-XXXXXX/f"));
}

#[test]
fn an_unset_tmpdir_means_slash_tmp() {
    let f = Fixture::new("unset");
    assert_eq!(f.temp_path(None).0, "/tmp/git-blob-XXXXXX/f");
}
