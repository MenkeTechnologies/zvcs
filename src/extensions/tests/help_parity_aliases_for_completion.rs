//! `git help --aliases-for-completion`, the hidden mode `git-completion.zsh`
//! reads every alias through (`list=(${(0)"$(git help --aliases-for-completion)"})`,
//! contrib/completion/git-completion.zsh:205).
//!
//! `cmd_help()` prints `list_aliases()` as `<name>\n<expansion>\0` records
//! (builtin/help.c:718-727). zvcs knew the option's name but had no arm for it,
//! so every `git <TAB>` under the stock zsh completion printed
//! `error: unknown option `aliases-for-completion'` and the usage block into the
//! prompt. The same `list_aliases()` feeds `--list-cmds=alias`, which listed
//! `[alias "foo"] command` as `foo.command` instead of `foo`.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    /// An empty directory with an isolated `$HOME`; `global` becomes the global
    /// config file when given.
    fn new(tag: &str, global: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-help-aliases-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        if let Some(text) = global {
            std::fs::write(root.join("gitconfig"), text).unwrap();
        }
        Fixture { root }
    }

    fn global(&self) -> PathBuf {
        self.root.join("gitconfig")
    }

    fn run(&self, args: &[&str]) -> Output {
        let global = self.global();
        Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", if global.exists() { global.as_path() } else { Path::new("/dev/null") })
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }
}

/// Records come in configuration order, a redefinition is appended rather than
/// merged, and an expansion is printed whole — newline and all — because the NUL
/// is the record separator.
#[test]
fn records_follow_configuration_order() {
    let f = Fixture::new("order", Some("[alias]\n\tzz = status\n\tml = \"!echo a\\nb\"\n"));
    let out = f.run(&["-c", "alias.aa=log", "-c", "alias.zz=diff", "help", "--aliases-for-completion"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"zz\nstatus\0ml\n!echo a\nb\0aa\nlog\0zz\ndiff\0");
    assert!(out.stderr.is_empty());
}

/// `config_alias_cb()`'s two syntaxes: `[alias "<name>"] command` names the
/// subsection, an empty subsection is plain `[alias]`, a subsection whose variable
/// is not `command` is the two-level `alias.x.y` form, and the subsection keeps
/// its case while the variable is lower-cased.
#[test]
fn subsection_syntax_names_the_alias() {
    let f = Fixture::new("subsection", None);
    let cfg = [
        "-c", "alias.foo.command=bar",
        "-c", "alias.x.y=z",
        "-c", "alias.Up=log",
        "-c", "alias..command=e",
        "-c", "alias.Foo.Bar=q",
    ];

    let mut args = cfg.to_vec();
    args.extend(["help", "--aliases-for-completion"]);
    let out = f.run(&args);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"foo\nbar\0x.y\nz\0up\nlog\0command\ne\0Foo.bar\nq\0");

    let mut args = cfg.to_vec();
    args.push("--list-cmds=alias");
    let out = f.run(&args);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"foo\nx.y\nup\ncommand\nFoo.bar\n");
}

/// A valueless alias is `config_error_nonbool()`, which aborts
/// `read_early_config()`: the reader's second line names the command line or
/// the file line, and nothing reaches stdout.
#[test]
fn valueless_alias_aborts_the_read() {
    let f = Fixture::new("valueless-cli", None);
    let out = f.run(&["-c", "alias.nv", "help", "--aliases-for-completion"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: missing value for 'alias.nv'\nfatal: unable to parse command-line config\n"
    );

    let f = Fixture::new("valueless-file", Some("[alias]\n\tok = log\n\tnv\n"));
    for args in [&["help", "--aliases-for-completion"][..], &["--list-cmds=alias"][..]] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "error: missing value for 'alias.nv'\nfatal: bad config line 3 in file {}\n",
                f.global().display()
            ),
            "{args:?}"
        );
    }
}

/// It is an `OPT_CMDMODE` like `--config-for-completion`: an operand, another
/// mode or a viewer is refused the way the other modes refuse them.
#[test]
fn refusals_match_the_other_modes() {
    let f = Fixture::new("refusals", None);

    let out = f.run(&["help", "--aliases-for-completion", "x"]);
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&out.stderr)
        .starts_with("fatal: the '--aliases-for-completion' option doesn't take any non-option arguments\n\nusage: git help "));

    let out = f.run(&["help", "--aliases-for-completion", "-a"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: options '-a' and '--aliases-for-completion' cannot be used together\n"
    );

    let out = f.run(&["help", "--aliases-for-completion", "-m"]);
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&out.stderr)
        .starts_with("fatal: options '--aliases-for-completion' and '--man' cannot be used together\n\nusage: git help "));
}
