//! `git instaweb` — configure an HTTP daemon to serve gitweb, start it, and
//! point a browser at it.
//!
//! A port of `git-instaweb` (git 2.55.0), a 786-line POSIX shell script
//! installed as `$(git --exec-path)/git-instaweb`. The script is the spec: every
//! generated config byte, message, exit code and file-creation order below is
//! taken from it, and the structure mirrors it function-for-function
//! (`resolve_full_httpd`/`start_httpd`/`stop_httpd`/`httpd_is_ready`/
//! `configure_httpd`/`lighttpd_conf`/`apache2_conf`/`mongoose_conf`/
//! `plackup_conf`/`webrick_conf`/`python_conf`/`gitweb_conf`).
//!
//! The script contains no git object logic and ships no server of its own: it
//! writes a config file for an *external* daemon — `lighttpd`, `apache2`/`httpd`,
//! `mongoose`, `plackup`, `webrick` or `python` — execs it, waits for the port to
//! accept a connection, and hands the URL to `git web--browse`. Everything it
//! needs therefore has substrate here (`std::fs` for the generated files,
//! `std::process` for the daemon, `std::net::TcpStream` for the readiness poll,
//! `libc::kill` for `stop_httpd`, and this crate's own ported `web--browse` for
//! the last step), so this is a full port rather than a skeleton. Serving depends
//! on one of those six daemons being installed, exactly as it does for stock.
//!
//! ### Covered (against git 2.55.0)
//!
//! * The `git rev-parse --parseopt` front end that `git-sh-setup` (line 71)
//!   drives from the script's `OPTIONS_SPEC` (lines 9-21). `OPTIONS_KEEPDASHDASH`
//!   and `OPTIONS_STUCKLONG` are both empty, so neither `--keep-dashdash` nor
//!   `--stuck-long` applies. Reproduced: short bundling (`-lp 1234`), attached
//!   short values (`-p1234`), `--long=value`, detached long values, `--no-`
//!   negation of every option, unambiguous long-name abbreviation, and `--` as
//!   the option terminator. Options are permuted ahead of positionals exactly
//!   as parseopt emits them in its `set -- …` line.
//! * parseopt's five diagnostics, each with its own stream split, all exit 129:
//!   `-h` (usage on stdout); ``unknown option `x'`` / ``unknown switch `x'``
//!   (error *and* usage on stderr); ``option `port' requires a value`` /
//!   ``switch `p' requires a value`` / ``option `local' takes no value``
//!   (error alone on stderr); `ambiguous option: s (could be --stop or --start)`
//!   (error on stderr, usage on stdout).
//! * `git_dir_init` (`git-sh-setup` line 326), which runs while `git-sh-setup` is
//!   being sourced — i.e. after parseopt but *before* the config reads and the
//!   script's own option loop. No repository → `fatal: not a git repository (or
//!   any of the parent directories): .git` on stderr, exit 128, even when the
//!   command line also has an error the option loop would reject.
//!   `SUBDIRECTORY_OK=Yes`, so a subdirectory is fine, and `GIT_DIR` is absolute.
//! * The config reads at lines 27-31 — `instaweb.local` (through
//!   `git config --bool`), `instaweb.httpd`, `instaweb.gitwebdir`,
//!   `instaweb.port` and `instaweb.modulepath` — each overridable by its option,
//!   plus `instaweb.browser`, which the final `git web--browse -c` consults.
//! * The script's own `*)` fallthrough (line 196) for any token parseopt passes
//!   through that the `case` does not name — a stray positional, or a `--no-…`
//!   form, since the case matches neither. `git-sh-setup`'s `usage()` re-execs
//!   `"$0" -h`, so this prints the usage block on **stdout** and exits **1**.
//! * `mkdir -p "$GIT_DIR/gitweb/tmp"` (line 203), which runs for *every* action
//!   including `--stop`, before the action `case` at line 755, and the
//!   `GIT_EXEC_PATH`/`GIT_DIR`/`GITWEB_CONFIG` exports at line 207.
//! * `stop_httpd`: `kill $(cat "$fqgitdir/pid")` — unquoted, so every
//!   whitespace-separated word becomes a separate argument — then `rm -f`. The
//!   pid file is removed whether or not the signal lands, and `--stop` exits 0
//!   regardless. bash's three builtin-`kill` diagnostics are reproduced: the
//!   usage line for an empty pid file, `arguments must be process or job IDs`
//!   for a non-numeric word, and `(<pid>) - No such process` for a dead one.
//! * `resolve_full_httpd` (line 47) in full: the `-f` suffix forced onto
//!   apache2/lighttpd/httpd, the three generated-script daemons that short-circuit
//!   to `$fqgitdir/gitweb/{gitweb.psgi,webrick.rb,gitweb.py}`, the `httpd_only`
//!   cut on the first space, the absolute-path bypass, the `PATH` probe, the
//!   `/usr/local/sbin`, `/usr/sbin`, `$root`, `$fqgitdir/gitweb` fallback search
//!   (including its `full_httpd=$i/$httpd` concatenation, which keeps the
//!   daemon's arguments inside the path), and the `<name> not found. Install
//!   <name> or use --httpd to specify another httpd daemon.` failure on stderr,
//!   exit 1.
//! * `configure_httpd`'s dispatch (line 728), asymmetries intact: `*lighttpd*` is
//!   matched before `*httpd*`; `webrick` matches **exactly** here while
//!   `resolve_full_httpd` matches `*webrick*`; an unrecognized daemon prints
//!   `Unknown httpd specified: <httpd>` on **stdout** and exits 1.
//! * All six generators, byte-for-byte, including the 52-entry lighttpd/plackup
//!   mimetype table with its 16-column key field, `server.bind` /
//!   `Listen 127.0.0.1:` / `:BindAddress` / `bind = "127.0.0.1"` under `--local`,
//!   the `$fqgitdir/mime.types` apache2 writes, apache2's MPM/module probe
//!   (`echo "LoadModule ${mod}_module " "$path"` emits *two* spaces), its
//!   mod_perl-versus-plain-CGI split, the `$list_mods | grep 'mod_cgi\.c'` probe,
//!   the `ScriptSock logs/gitweb.sock` line that only the cgid path adds, and
//!   `You have no CGI support!` on stdout with exit 2. `webrick_conf` and
//!   `plackup_conf` delete `$conf` after writing their standalone server script,
//!   `python_conf` builds the `cgi-bin/gitweb.cgi` and `static` symlink tree, and
//!   `gitweb_conf` writes `gitweb_config.perl` with `$projectroot` set to
//!   `dirname "$fqgitdir"`.
//! * `start_httpd` (line 103): the `Instance already running. Restarting...`
//!   short-circuit, the `test -f "$conf" || configure_httpd` and
//!   `test -f …/gitweb_config.perl || gitweb_conf` reuse checks, the
//!   `*mongoose*|*plackup*|*python*` fork-and-record-`$!` arm versus the
//!   foreground arm, and `Could not execute http daemon <httpd>.` on stdout with
//!   exit 1 when the foreground daemon returns non-zero.
//! * `httpd_is_ready` (line 150): connect to `127.0.0.1:$port` once and return
//!   silently on success, else print `Waiting for '<httpd>' to start ..` and a
//!   further `.` per second until the port answers, then ` (done)` — all on
//!   stdout, unbuffered, as the perl one-liner's `$| = 1` makes it.
//! * The final browse: `git web--browse -b "$browser" <url>` when `-b` was given,
//!   otherwise `git web--browse -c instaweb.browser <url>`, with `echo <url>` as
//!   the `||` fallback when that fails. `web--browse` is called in-process
//!   (`porcelain/web__browse.rs` is itself a full port) rather than re-execed.
//!
//! ### Deliberate divergences, stated rather than hidden
//!
//! * **The default gitweb directory.** The script's `root` falls back to the
//!   `$(gitwebdir)` its build baked in — for the installed git that is
//!   `<prefix>/share/gitweb`, holding the `gitweb.cgi` Perl program. zvcs ships
//!   no gitweb, so the same prefix-relative rule is applied to *this* binary
//!   (`current_exe()/../../share/gitweb`), which normally does not exist: point
//!   `instaweb.gitwebdir` at a real gitweb installation to serve anything. The
//!   generated configs are otherwise identical, and setting that key makes them
//!   byte-identical to stock's.
//! * `GIT_EXEC_PATH` is this binary's own exec-path (`$HOME/.zvcs/bin` unless the
//!   environment overrides it, per `git --exec-path`), not stock's
//!   `libexec/git-core`; it is interpolated into the mongoose, webrick and python
//!   configs. `PATH` is likewise exported with that directory prepended, which is
//!   what `git.c`'s `setup_path()` does to the environment the shell script would
//!   have inherited, and is what mongoose's `cgi_env PATH=` records.
//! * bash prefixes its builtin-`kill` diagnostics with `<script>: line 146:`.
//!   That interpolates `$0`, which here is this binary rather than
//!   `…/git-core/git-instaweb`.
//! * `chmod +x` on the webrick wrapper is applied as `a+x` — the process umask is
//!   not subtracted. Under the usual 022 the result is identical.
//! * `$full_httpd "$conf"` is unquoted in the script, so the daemon command
//!   word-splits on whitespace; that splitting is reproduced, but the shell's
//!   globbing and quote removal on those words are not, since no daemon name
//!   produced by `resolve_full_httpd` contains a metacharacter.
//! * Values are decoded lossily, so a config value or path that is not valid
//!   UTF-8 diverges.
//! * `git config --bool --get instaweb.local` additionally prints git's own
//!   `fatal: bad boolean config value …` when the key holds a non-boolean. The
//!   script ignores that exit status and carries on with an empty `local`, which
//!   is what happens here; the extra stderr line is not reproduced.
//!
//! Known parseopt deviation: it reports an ambiguous abbreviation with exactly
//! the first two matching names, which is all this spec can produce (no prefix
//! here matches three options). A three-way form is not implemented.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The usage block `git rev-parse --parseopt` renders from `OPTIONS_SPEC`.
/// 530 bytes; the option column is padded to width 22 and long entries wrap.
const USAGE: &str = concat!(
    "usage: git instaweb [options] (--start | --stop | --restart)\n",
    "\n",
    "    -l, --[no-]local      only bind on 127.0.0.1\n",
    "    -p, --[no-]port ...   the port to bind to\n",
    "    -d, --[no-]httpd ...  the command to launch\n",
    "    -b, --[no-]browser ...\n",
    "                          the browser to launch\n",
    "    -m, --[no-]module-path ...\n",
    "                          the module path (only needed for apache2)\n",
    "\n",
    "Action\n",
    "    --[no-]stop           stop the web server\n",
    "    --[no-]start          start the web server\n",
    "    --[no-]restart        restart the web server\n",
    "\n",
);

/// `PERL='/usr/bin/perl'` (line 6), interpolated into the mongoose config and
/// the generated `gitweb.psgi` shebang.
const PERL: &str = "/usr/bin/perl";

/// One entry of `OPTIONS_SPEC` (lines 12-20): the long name, the optional short
/// letter, and whether the spec spells it with a trailing `=`.
struct Spec {
    long: &'static str,
    short: Option<char>,
    takes_value: bool,
}

/// The spec in declaration order — the order parseopt scans, and therefore the
/// order in which it names candidates in an ambiguity error.
const SPECS: &[Spec] = &[
    Spec { long: "local", short: Some('l'), takes_value: false },
    Spec { long: "port", short: Some('p'), takes_value: true },
    Spec { long: "httpd", short: Some('d'), takes_value: true },
    Spec { long: "browser", short: Some('b'), takes_value: true },
    Spec { long: "module-path", short: Some('m'), takes_value: true },
    Spec { long: "stop", short: None, takes_value: false },
    Spec { long: "start", short: None, takes_value: false },
    Spec { long: "restart", short: None, takes_value: false },
];

/// Where parseopt puts the usage block for a given outcome, if anywhere.
enum Usage {
    None,
    Stdout,
    Stderr,
}

/// A parseopt exit: an optional `error:` line on stderr plus a usage block.
/// Every one of these leaves with status 129.
struct Fail {
    error: Option<String>,
    usage: Usage,
}

/// The script's control-flow exits, carried as an `anyhow` error so `?` unwinds
/// the way `exit` unwinds a shell. Any message is already on the right stream by
/// the time this is constructed, exactly as the script's `echo …; exit n` does.
#[derive(Debug)]
struct Exit(u8);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// The script's `action` variable (line 32), defaulting to `browse`.
enum Action {
    Browse,
    Stop,
    Start,
    Restart,
}

/// `git instaweb` — configure, start and browse gitweb.
pub fn instaweb(args: &[String]) -> Result<ExitCode> {
    match run(args) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(exit) => Ok(ExitCode::from(exit.0)),
            None => Err(e),
        },
    }
}

/// The body of [`instaweb`], with the script's `exit` paths as errors.
fn run(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand at
    // index 0 so both calling conventions behave identically.
    let args: &[String] = match args.first() {
        Some(a) if a == "instaweb" => &args[1..],
        _ => args,
    };

    // parseopt runs inside `git-sh-setup` before `git_dir_init`, so every
    // diagnostic below is emitted whether or not there is a repository.
    let tokens = match parseopt(args) {
        Ok(tokens) => tokens,
        Err(fail) => {
            if let Some(error) = &fail.error {
                eprintln!("{error}");
            }
            match fail.usage {
                Usage::None => {}
                Usage::Stdout => print!("{USAGE}"),
                Usage::Stderr => eprint!("{USAGE}"),
            }
            return Err(Exit(129).into());
        }
    };

    // `git_dir_init`: `GIT_DIR=$(git rev-parse --git-dir) || exit`, then
    // `GIT_DIR=$(cd "$GIT_DIR" && pwd)` to make it absolute. Still part of
    // sourcing `git-sh-setup`, so it precedes the config reads and the loop.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Err(Exit(128).into());
    };
    let fqgitdir = match repo.git_dir().canonicalize() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("Unable to determine absolute path of git directory");
            return Err(Exit(1).into());
        }
    };

    let mut web = Instaweb::new(&repo, fqgitdir);

    // The script's `while test $# != 0` loop (lines 163-201) over the tokens
    // parseopt handed back.
    let mut action = Action::Browse;
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        // `shift; var="$1"` — parseopt guarantees the value is present.
        let mut value = || tokens.next().unwrap_or_default();
        match token.as_str() {
            "--stop" | "stop" => action = Action::Stop,
            "--start" | "start" => action = Action::Start,
            "--restart" | "restart" => action = Action::Restart,
            "-l" | "--local" => web.local = "true".to_string(),
            "-d" | "--httpd" => web.httpd = value(),
            "-b" | "--browser" => web.browser = value(),
            "-p" | "--port" => web.port = value(),
            "-m" | "--module-path" => web.module_path = value(),
            "--" => {}
            // `*) usage`, i.e. `"$0" -h; exit 1`: usage on stdout, status 1.
            _ => {
                print!("{USAGE}");
                return Err(Exit(1).into());
            }
        }
    }

    // Line 203, ahead of the action dispatch and so run for every action.
    std::fs::create_dir_all(web.fqgitdir.join("gitweb").join("tmp"))?;
    web.export_env();

    match action {
        Action::Stop => {
            web.stop_httpd()?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::Start => {
            web.start_httpd()?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::Restart => {
            web.stop_httpd()?;
            web.start_httpd()?;
            return Ok(ExitCode::SUCCESS);
        }
        Action::Browse => {}
    }

    // Lines 771-786.
    web.gitweb_conf()?;
    web.resolve_full_httpd()?;
    std::fs::create_dir_all(web.fqgitdir.join("gitweb").join(&web.httpd_only))?;
    web.conf = join_raw(&web.fqgitdir.join("gitweb"), &format!("{}.conf", web.httpd_only));
    web.configure_httpd()?;
    web.start_httpd()?;

    let url = format!("http://127.0.0.1:{}", web.port);
    web.httpd_is_ready();
    let browse: Vec<String> = if web.browser.is_empty() {
        vec!["-c".into(), "instaweb.browser".into(), url.clone()]
    } else {
        vec!["-b".into(), web.browser.clone(), url.clone()]
    };
    // The script runs `git web--browse …` as a child and branches on its status.
    // `web--browse` is this binary's own ported porcelain
    // (`porcelain/web__browse.rs`), so re-invoking this executable runs exactly
    // that code — and, unlike an in-process call, yields the exit status the
    // script's `||` needs, since `ExitCode` cannot be inspected.
    let ran = std::env::current_exe().ok().and_then(|exe| {
        Command::new(exe)
            .arg("web--browse")
            .args(&browse)
            .status()
            .ok()
    });
    match ran {
        Some(status) if status.success() => Ok(ExitCode::SUCCESS),
        // `|| echo $url` — reached both when web--browse exits non-zero and
        // when it could not run at all.
        _ => {
            println!("{url}");
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The script's global state: the values it reads from config at lines 26-34,
/// as its options may then override them, plus the two variables
/// `resolve_full_httpd` publishes.
struct Instaweb {
    /// `fqgitdir="$GIT_DIR"`, absolute.
    fqgitdir: PathBuf,
    /// `git config --bool --get instaweb.local`, so `""`, `"true"` or `"false"`.
    /// Kept as text because three of the generators interpolate it verbatim —
    /// notably `plackup_conf`, where perl reads the string `"false"` as *true*.
    local: String,
    httpd: String,
    root: String,
    port: String,
    module_path: String,
    browser: String,
    /// `GIT_EXEC_PATH="$(git --exec-path)"` (line 204).
    git_exec_path: String,
    /// `$fqgitdir/gitweb/gitweb_config.perl`, exported as `GITWEB_CONFIG`.
    gitweb_config: PathBuf,
    /// `$httpd` with its arguments stripped — the daemon's own name.
    httpd_only: String,
    /// The command `start_httpd` runs, arguments included.
    full_httpd: String,
    /// `conf`, retargeted to `$fqgitdir/gitweb/$httpd_only.conf` once the daemon
    /// is resolved (line 34 seeds it with `httpd.conf`).
    conf: PathBuf,
}

impl Instaweb {
    /// Lines 26-45: the config reads and their defaults.
    fn new(repo: &gix::Repository, fqgitdir: PathBuf) -> Self {
        let cfg = repo.config_snapshot();
        let get = |key: &str| {
            cfg.string(key)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_default()
        };

        // `git config --bool` prints `true`/`false`, or nothing when unset.
        let local = match cfg.boolean("instaweb.local") {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string();
        let mut httpd = get("instaweb.httpd");
        let mut root = get("instaweb.gitwebdir");
        let mut port = get("instaweb.port");
        let module_path = get("instaweb.modulepath");

        // if installed, it doesn't need further configuration (module_path)
        if httpd.is_empty() {
            httpd = "lighttpd -f".to_string();
        }
        if root.is_empty() {
            root = default_gitwebdir();
        }
        // any untaken local port will do...
        if port.is_empty() {
            port = "1234".to_string();
        }

        let gitweb_config = fqgitdir.join("gitweb").join("gitweb_config.perl");
        Self {
            conf: fqgitdir.join("gitweb").join("httpd.conf"),
            git_exec_path: exec_path(),
            gitweb_config,
            fqgitdir,
            local,
            httpd,
            root,
            port,
            module_path,
            browser: String::new(),
            httpd_only: String::new(),
            full_httpd: String::new(),
        }
    }

    /// Lines 204-207: the three variables the daemon and the CGI need, plus the
    /// `PATH` that `git.c`'s `setup_path()` hands any git subprogram.
    fn export_env(&self) {
        std::env::set_var("GIT_EXEC_PATH", &self.git_exec_path);
        std::env::set_var("GIT_DIR", &self.fqgitdir);
        std::env::set_var("GITWEB_CONFIG", &self.gitweb_config);
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![PathBuf::from(&self.git_exec_path)];
        dirs.extend(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(dirs) {
            std::env::set_var("PATH", joined);
        }
    }

    /// `$PATH` as the generated configs record it.
    fn path_env(&self) -> String {
        std::env::var("PATH").unwrap_or_default()
    }

    /// `resolve_full_httpd()` (line 47).
    fn resolve_full_httpd(&mut self) -> Result<()> {
        let gitweb = self.fqgitdir.join("gitweb");
        // yes, *httpd* covers *lighttpd* above, but it is there for clarity
        if contains_any(&self.httpd, &["apache2", "lighttpd", "httpd"]) {
            // ensure that the apache2/lighttpd command ends with "-f"
            if !ends_with_dash_f(&self.httpd) {
                self.httpd = format!("{} -f", self.httpd);
            }
        } else if let Some(script) = generated_server_script(&self.httpd) {
            // server is started by running the generated script in $fqgitdir/gitweb
            self.full_httpd = gitweb.join(script).to_string_lossy().into_owned();
            self.httpd_only = first_word(&self.httpd);
            return Ok(());
        }

        self.httpd_only = first_word(&self.httpd);
        if self.httpd_only.starts_with('/') || which(&self.httpd_only) {
            self.full_httpd = self.httpd.clone();
            return Ok(());
        }
        // many httpds are installed in /usr/sbin or /usr/local/sbin these days
        // and those are not in most users $PATHs; in addition, we may have
        // generated a server script in $fqgitdir/gitweb.
        for dir in [
            PathBuf::from("/usr/local/sbin"),
            PathBuf::from("/usr/sbin"),
            PathBuf::from(&self.root),
            gitweb,
        ] {
            if is_executable(&join_raw(&dir, &self.httpd_only)) {
                // `full_httpd=$i/$httpd` keeps the daemon's arguments inside the
                // path it builds — the script's own wording, reproduced.
                self.full_httpd = format!("{}/{}", dir.display(), self.httpd);
                return Ok(());
            }
        }
        eprintln!(
            "{} not found. Install {} or use --httpd to specify another httpd daemon.",
            self.httpd_only, self.httpd_only
        );
        Err(Exit(1).into())
    }

    /// `start_httpd()` (line 103).
    fn start_httpd(&mut self) -> Result<()> {
        let pid_file = self.fqgitdir.join("pid");
        if pid_file.is_file() {
            println!("Instance already running. Restarting...");
            self.stop_httpd()?;
        }

        // here $httpd should have a meaningful value
        self.resolve_full_httpd()?;
        std::fs::create_dir_all(join_raw(&self.fqgitdir.join("gitweb"), &self.httpd_only))?;
        self.conf = join_raw(
            &self.fqgitdir.join("gitweb"),
            &format!("{}.conf", self.httpd_only),
        );

        // generate correct config file if it doesn't exist
        if !self.conf.is_file() {
            self.configure_httpd()?;
        }
        if !self.gitweb_config.is_file() {
            self.gitweb_conf()?;
        }

        // don't quote $full_httpd, there can be arguments to it (-f)
        let mut words: Vec<String> = self.full_httpd.split_whitespace().map(str::to_string).collect();
        let program = if words.is_empty() {
            String::new()
        } else {
            words.remove(0)
        };
        let mut cmd = Command::new(&program);
        cmd.args(&words).arg(&self.conf);

        if contains_any(&self.httpd, &["mongoose", "plackup", "python"]) {
            // These servers don't have a daemon mode so we'll have to fork it
            let child = cmd.spawn();
            let Ok(child) = child else {
                println!("Could not execute http daemon {}.", self.httpd);
                return Err(Exit(1).into());
            };
            // Save the pid before doing anything else (we'll print it later)
            std::fs::write(&pid_file, format!("{}\n", child.id()))?;
        } else {
            let status = cmd.status();
            let ok = matches!(&status, Ok(s) if s.success());
            if !ok {
                println!("Could not execute http daemon {}.", self.httpd);
                return Err(Exit(1).into());
            }
        }
        Ok(())
    }

    /// `stop_httpd()` (line 145).
    fn stop_httpd(&self) -> Result<()> {
        let pid_file = self.fqgitdir.join("pid");
        if pid_file.is_file() {
            let text = std::fs::read_to_string(&pid_file).unwrap_or_default();
            kill_words(&text);
        }
        // `rm -f` — a missing file is not an error.
        let _ = std::fs::remove_file(&pid_file);
        Ok(())
    }

    /// `httpd_is_ready()` (line 150), whose perl one-liner runs with `$| = 1`.
    fn httpd_is_ready(&self) {
        let addr = format!("127.0.0.1:{}", self.port);
        if std::net::TcpStream::connect(&addr).is_ok() {
            return;
        }
        print!("Waiting for '{}' to start ..", self.httpd);
        let _ = std::io::stdout().flush();
        loop {
            print!(".");
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_secs(1));
            if std::net::TcpStream::connect(&addr).is_ok() {
                break;
            }
        }
        println!(" (done)");
    }

    /// `configure_httpd()` (line 728). `*lighttpd*` is matched ahead of
    /// `*httpd*`, and `webrick` matches exactly rather than as a substring.
    fn configure_httpd(&mut self) -> Result<()> {
        if self.httpd.contains("lighttpd") {
            self.lighttpd_conf()
        } else if contains_any(&self.httpd, &["apache2", "httpd"]) {
            self.apache2_conf()
        } else if self.httpd == "webrick" {
            self.webrick_conf()
        } else if self.httpd.contains("mongoose") {
            self.mongoose_conf()
        } else if self.httpd.contains("plackup") {
            self.plackup_conf()
        } else if self.httpd.contains("python") {
            self.python_conf()
        } else {
            println!("Unknown httpd specified: {}", self.httpd);
            Err(Exit(1).into())
        }
    }

    /// `gitweb_conf()` (line 717).
    fn gitweb_conf(&self) -> Result<()> {
        let projectroot = dirname(&self.fqgitdir);
        write_file(
            &self.gitweb_config,
            &format!(
                "#!/usr/bin/perl\n\
                 our $projectroot = \"{projectroot}\";\n\
                 our $git_temp = \"{gitdir}/gitweb/tmp\";\n\
                 our $projects_list = $projectroot;\n\
                 \n\
                 $feature{{'remote_heads'}}{{'default'}} = [1];\n",
                gitdir = self.fqgitdir.display(),
            ),
        )
    }

    /// `lighttpd_conf()` (line 259).
    fn lighttpd_conf(&self) -> Result<()> {
        let mut out = format!(
            "server.document-root = \"{root}\"\n\
             server.port = {port}\n\
             server.modules = ( \"mod_setenv\", \"mod_cgi\" )\n\
             server.indexfiles = ( \"gitweb.cgi\" )\n\
             server.pid-file = \"{gitdir}/pid\"\n\
             server.errorlog = \"{gitdir}/gitweb/{only}/error.log\"\n\
             \n\
             # to enable, add \"mod_access\", \"mod_accesslog\" to server.modules\n\
             # variable above and uncomment this\n\
             #accesslog.filename = \"{gitdir}/gitweb/{only}/access.log\"\n\
             \n\
             setenv.add-environment = ( \"PATH\" => env.PATH, \"GITWEB_CONFIG\" => env.GITWEB_CONFIG )\n\
             \n\
             cgi.assign = ( \".cgi\" => \"\" )\n\
             \n\
             # mimetype mapping\n\
             mimetype.assign             = (\n",
            root = self.root,
            port = self.port,
            gitdir = self.fqgitdir.display(),
            only = self.httpd_only,
        );
        for (i, (ext, mime)) in MIME_TYPES.iter().enumerate() {
            let comma = if i + 1 == MIME_TYPES.len() { "" } else { "," };
            out.push_str(&format!("  {:<16}=>      \"{mime}\"{comma}\n", format!("\"{ext}\"")));
        }
        out.push_str(" )\n");
        if self.local == "true" {
            out.push_str("server.bind = \"127.0.0.1\"\n");
        }
        write_file(&self.conf, &out)
    }

    /// `apache2_conf()` (line 335).
    fn apache2_conf(&mut self) -> Result<()> {
        for candidate in ["/etc/httpd", "/usr/lib/apache2", "/usr/lib/httpd"] {
            let modules = Path::new(candidate).join("modules");
            if modules.is_dir() {
                self.module_path = modules.to_string_lossy().into_owned();
                break;
            }
        }
        let bind = if self.local == "true" { "127.0.0.1:" } else { "" };
        write_file(&self.fqgitdir.join("mime.types"), "text/css css\n")?;

        let gitdir = self.fqgitdir.display();
        let mut out = format!(
            "ServerName \"git-instaweb\"\n\
             ServerRoot \"{root}\"\n\
             DocumentRoot \"{root}\"\n\
             ErrorLog \"{gitdir}/gitweb/{only}/error.log\"\n\
             CustomLog \"{gitdir}/gitweb/{only}/access.log\" combined\n\
             PidFile \"{gitdir}/pid\"\n\
             Listen {bind}{port}\n",
            root = self.root,
            only = self.httpd_only,
            port = self.port,
        );

        // only one mpm module permitted
        for m in ["mpm_event", "mpm_prefork", "mpm_worker"] {
            let so = format!("{}/mod_{m}.so", self.module_path);
            if Path::new(&so).exists() {
                out.push_str(&format!("LoadModule {m}_module  {so}\n"));
                break;
            }
        }
        for m in ["mime", "dir", "env", "log_config", "authz_core", "unixd"] {
            let so = format!("{}/mod_{m}.so", self.module_path);
            if Path::new(&so).exists() {
                out.push_str(&format!("LoadModule {m}_module  {so}\n"));
            }
        }
        out.push_str(&format!(
            "TypesConfig \"{gitdir}/mime.types\"\n\
             DirectoryIndex gitweb.cgi\n"
        ));

        if Path::new(&format!("{}/mod_perl.so", self.module_path)).exists() {
            // favor mod_perl if available
            out.push_str(&format!(
                "LoadModule perl_module {}/mod_perl.so\n\
                 PerlPassEnv GIT_DIR\n\
                 PerlPassEnv GIT_EXEC_PATH\n\
                 PerlPassEnv GITWEB_CONFIG\n\
                 <Location /gitweb.cgi>\n\
                 \tSetHandler perl-script\n\
                 \tPerlResponseHandler ModPerl::Registry\n\
                 \tPerlOptions +ParseHeaders\n\
                 \tOptions +ExecCGI\n\
                 </Location>\n",
                self.module_path
            ));
            return write_file(&self.conf, &out);
        }

        // plain-old CGI
        self.resolve_full_httpd()?;
        let list_mods = replace_trailing_dash_f_with_l(&self.full_httpd);
        // The script runs the pipeline twice — once per `grep` — so the daemon
        // is executed twice and any diagnostic it writes appears twice.
        if !run_and_capture(&list_mods).contains("mod_cgi.c") {
            let cgi = format!("{}/mod_cgi.so", self.module_path);
            if Path::new(&cgi).is_file() {
                out.push_str(&format!("LoadModule cgi_module {cgi}\n"));
            } else {
                let cgid = format!("{}/mod_cgid.so", self.module_path);
                if !run_and_capture(&list_mods).contains("mod_cgid.c") {
                    if Path::new(&cgid).is_file() {
                        out.push_str(&format!("LoadModule cgid_module {cgid}\n"));
                    } else {
                        // The config written so far is discarded, exactly as the
                        // script's `exit 2` leaves it half-written; write it
                        // first so the file matches byte for byte.
                        write_file(&self.conf, &out)?;
                        println!("You have no CGI support!");
                        return Err(Exit(2).into());
                    }
                }
                out.push_str("ScriptSock logs/gitweb.sock\n");
            }
        }
        out.push_str(
            "PassEnv GIT_DIR\n\
             PassEnv GIT_EXEC_PATH\n\
             PassEnv GITWEB_CONFIG\n\
             AddHandler cgi-script .cgi\n\
             <Location /gitweb.cgi>\n\
             \tOptions +ExecCGI\n\
             </Location>\n",
        );
        write_file(&self.conf, &out)
    }

    /// `mongoose_conf()` (line 430).
    fn mongoose_conf(&self) -> Result<()> {
        let gitdir = self.fqgitdir.display();
        write_file(
            &self.conf,
            &format!(
                "# Mongoose web server configuration file.\n\
                 # Lines starting with '#' and empty lines are ignored.\n\
                 # For detailed description of every option, visit\n\
                 # https://code.google.com/p/mongoose/wiki/MongooseManual\n\
                 \n\
                 root\t\t{root}\n\
                 ports\t\t{port}\n\
                 index_files\tgitweb.cgi\n\
                 #ssl_cert\t{gitdir}/gitweb/ssl_cert.pem\n\
                 error_log\t{gitdir}/gitweb/{only}/error.log\n\
                 access_log\t{gitdir}/gitweb/{only}/access.log\n\
                 \n\
                 #cgi setup\n\
                 cgi_env\t\tPATH={path},GIT_DIR={gitdir},GIT_EXEC_PATH={exec},GITWEB_CONFIG={cfg}\n\
                 cgi_interp\t{PERL}\n\
                 cgi_ext\t\tcgi,pl\n\
                 \n\
                 # mimetype mapping\n\
                 mime_types\t{MONGOOSE_MIME_TYPES}\n",
                root = self.root,
                port = self.port,
                only = self.httpd_only,
                path = self.path_env(),
                exec = self.git_exec_path,
                cfg = self.gitweb_config.display(),
            ),
        )
    }

    /// `webrick_conf()` (line 209). `$httpd` and `$httpd_only` are the same here,
    /// since `configure_httpd` only reaches this for the exact name `webrick`.
    fn webrick_conf(&self) -> Result<()> {
        let gitdir = self.fqgitdir.display();
        // webrick seems to have no way of passing arbitrary environment
        // variables to the underlying CGI executable, so we wrap the actual
        // gitweb.cgi using a shell script to force it
        let wrapper = self
            .fqgitdir
            .join("gitweb")
            .join(&self.httpd)
            .join("wrapper.sh");
        write_file(
            &wrapper,
            &format!(
                "#!/bin/sh\n\
                 # we use this shell script wrapper around the real gitweb.cgi since\n\
                 # there appears to be no other way to pass arbitrary environment variables\n\
                 # into the CGI process\n\
                 GIT_EXEC_PATH={exec} GIT_DIR={gitdir} GITWEB_CONFIG={cfg}\n\
                 export GIT_EXEC_PATH GIT_DIR GITWEB_CONFIG\n\
                 exec {root}/gitweb.cgi\n",
                exec = self.git_exec_path,
                cfg = self.gitweb_config.display(),
                root = self.root,
            ),
        )?;
        make_executable(&wrapper)?;

        // This assumes _ruby_ is in the user's $PATH. that's _one_ portable way
        // to run ruby, which could be installed anywhere, really.
        let script = join_raw(&self.fqgitdir.join("gitweb"), &format!("{}.rb", self.httpd));
        write_file(
            &script,
            &format!(
                "#!/usr/bin/env ruby\n\
                 require 'webrick'\n\
                 require 'logger'\n\
                 options = {{\n\
                 \x20 :Port => {port},\n\
                 \x20 :DocumentRoot => \"{root}\",\n\
                 \x20 :Logger => Logger.new('{gitdir}/gitweb/error.log'),\n\
                 \x20 :AccessLog => [\n\
                 \x20   [ Logger.new('{gitdir}/gitweb/access.log'),\n\
                 \x20     WEBrick::AccessLog::COMBINED_LOG_FORMAT ]\n\
                 \x20 ],\n\
                 \x20 :DirectoryIndex => [\"gitweb.cgi\"],\n\
                 \x20 :CGIInterpreter => \"{wrapper}\",\n\
                 \x20 :StartCallback => lambda do\n\
                 \x20   File.open(\"{gitdir}/pid\", \"w\") {{ |f| f.puts Process.pid }}\n\
                 \x20 end,\n\
                 \x20 :ServerType => WEBrick::Daemon,\n\
                 }}\n\
                 options[:BindAddress] = '127.0.0.1' if \"{local}\" == \"true\"\n\
                 server = WEBrick::HTTPServer.new(options)\n\
                 ['INT', 'TERM'].each do |signal|\n\
                 \x20 trap(signal) {{server.shutdown}}\n\
                 end\n\
                 server.start\n",
                port = self.port,
                root = self.root,
                wrapper = wrapper.display(),
                local = self.local,
            ),
        )?;
        make_executable(&script)?;
        // configuration is embedded in server script file, webrick.rb
        let _ = std::fs::remove_file(&self.conf);
        Ok(())
    }

    /// `plackup_conf()` (line 454): a standalone server script with embedded
    /// configuration; it does not use `$conf`.
    fn plackup_conf(&self) -> Result<()> {
        let mut out = format!(
            "#!{PERL}\n\
             \n\
             # gitweb - simple web interface to track changes in git repositories\n\
             #          PSGI wrapper and server starter (see https://plackperl.org)\n\
             \n\
             use strict;\n\
             \n\
             use IO::Handle;\n\
             use Plack::MIME;\n\
             use Plack::Builder;\n\
             use Plack::App::WrapCGI;\n\
             use CGI::Emulate::PSGI 0.07; # minimum version required to work with gitweb\n\
             \n\
             # mimetype mapping (from lighttpd_conf)\n\
             Plack::MIME->add_type(\n"
        );
        for (i, (ext, mime)) in MIME_TYPES.iter().enumerate() {
            let comma = if i + 1 == MIME_TYPES.len() { "" } else { "," };
            out.push_str(&format!("\t{:<16}=>      \"{mime}\"{comma}\n", format!("\"{ext}\"")));
        }
        out.push_str(&format!(
            ");\n\
             \n\
             my $app = builder {{\n\
             \t# to be able to override $SIG{{__WARN__}} to log build time warnings\n\
             \tuse CGI::Carp; # it sets $SIG{{__WARN__}} itself\n\
             \n\
             \tmy $logdir = \"{gitdir}/gitweb/{only}\";\n\
             \topen my $access_log_fh, '>>', \"$logdir/access.log\"\n\
             \t\tor die \"Couldn't open access log '$logdir/access.log': $!\";\n\
             \topen my $error_log_fh,  '>>', \"$logdir/error.log\"\n\
             \t\tor die \"Couldn't open error log '$logdir/error.log': $!\";\n\
             \n\
             \t$access_log_fh->autoflush(1);\n\
             \t$error_log_fh->autoflush(1);\n\
             \n\
             \t# redirect build time warnings to error.log\n\
             \t$SIG{{'__WARN__'}} = sub {{\n\
             \t\tmy $msg = shift;\n\
             \t\t# timestamp warning like in CGI::Carp::warn\n\
             \t\tmy $stamp = CGI::Carp::stamp();\n\
             \t\t$msg =~ s/^/$stamp/gm;\n\
             \t\tprint $error_log_fh $msg;\n\
             \t}};\n\
             \n\
             \t# write errors to error.log, access to access.log\n\
             \tenable 'AccessLog',\n\
             \t\tformat => \"combined\",\n\
             \t\tlogger => sub {{ print $access_log_fh @_; }};\n\
             \tenable sub {{\n\
             \t\tmy $app = shift;\n\
             \t\tsub {{\n\
             \t\t\tmy $env = shift;\n\
             \t\t\t$env->{{'psgi.errors'}} = $error_log_fh;\n\
             \t\t\t$app->($env);\n\
             \t\t}}\n\
             \t}};\n\
             \t# gitweb currently doesn't work with {{CHLD}} set to 'IGNORE',\n\
             \t# because it uses 'close  or die...' on piped filehandle \n\
             \t# (which causes the parent process to wait for child to finish).\n\
             \tenable_if {{ $SIG{{'CHLD'}} eq 'IGNORE' }} sub {{\n\
             \t\tmy $app = shift;\n\
             \t\tsub {{\n\
             \t\t\tmy $env = shift;\n\
             \t\t\tlocal $SIG{{'CHLD'}} = 'DEFAULT';\n\
             \t\t\tlocal $SIG{{'CLD'}}  = 'DEFAULT';\n\
             \t\t\t$app->($env);\n\
             \t\t}}\n\
             \t}};\n\
             \t# serve static files, i.e. stylesheet, images, script\n\
             \tenable 'Static',\n\
             \t\tpath => sub {{ m!\\.(js|css|png)$! && s!^/gitweb/!! }},\n\
             \t\troot => \"{root}/\",\n\
             \t\tencoding => 'utf-8'; # encoding for 'text/plain' files\n\
             \t# convert CGI application to PSGI app\n\
             \tPlack::App::WrapCGI->new(script => \"{root}/gitweb.cgi\")->to_app;\n\
             }};\n\
             \n\
             # make it runnable as standalone app,\n\
             # like it would be run via 'plackup' utility\n\
             if (caller) {{\n\
             \treturn $app;\n\
             }} else {{\n\
             \trequire Plack::Runner;\n\
             \n\
             \tmy $runner = Plack::Runner->new();\n\
             \t$runner->parse_options(qw(--env deployment --port {port}),\n\
             \t\t\t\t\"{local}\" ? qw(--host 127.0.0.1) : ());\n\
             \t$runner->run($app);\n\
             }}\n\
             __END__\n",
            gitdir = self.fqgitdir.display(),
            only = self.httpd_only,
            root = self.root,
            port = self.port,
            local = self.local,
        ));

        let script = self.fqgitdir.join("gitweb").join("gitweb.psgi");
        write_file(&script, &out)?;
        make_executable(&script)?;
        // configuration is embedded in server script file, gitweb.psgi
        let _ = std::fs::remove_file(&self.conf);
        Ok(())
    }

    /// `python_conf()` (line 602).
    ///
    /// Python's builtin http.server and its CGI support is very limited. The CGI
    /// handler can only run a script from inside a directory, so the script
    /// builds a web root at `$fqgitdir/gitweb/$httpd_only` and symlinks
    /// `gitweb.cgi` and `static` into it.
    fn python_conf(&self) -> Result<()> {
        let webroot = join_raw(&self.fqgitdir.join("gitweb"), &self.httpd_only);
        std::fs::create_dir_all(webroot.join("cgi-bin"))?;
        // Python http.server follows the symlinks
        symlink_force(
            Path::new(&format!("{}/gitweb.cgi", self.root)),
            &webroot.join("cgi-bin").join("gitweb.cgi"),
        )?;
        symlink_force(
            Path::new(&format!("{}/static", self.root)),
            &webroot.join("static"),
        )?;

        let script = self.fqgitdir.join("gitweb").join("gitweb.py");
        write_file(
            &script,
            &format!(
                "#!/usr/bin/env python\n\
                 import os\n\
                 import sys\n\
                 \n\
                 # Open log file in line buffering mode\n\
                 accesslogfile = open(\"{gitdir}/gitweb/access.log\", 'a', buffering=1)\n\
                 errorlogfile = open(\"{gitdir}/gitweb/error.log\", 'a', buffering=1)\n\
                 \n\
                 # and replace our stdout and stderr with log files\n\
                 # also do a lowlevel duplicate of the logfile file descriptors so that\n\
                 # our CGI child process writes any stderr warning also to the log file\n\
                 _orig_stdout_fd = sys.stdout.fileno()\n\
                 sys.stdout.close()\n\
                 os.dup2(accesslogfile.fileno(), _orig_stdout_fd)\n\
                 sys.stdout = accesslogfile\n\
                 \n\
                 _orig_stderr_fd = sys.stderr.fileno()\n\
                 sys.stderr.close()\n\
                 os.dup2(errorlogfile.fileno(), _orig_stderr_fd)\n\
                 sys.stderr = errorlogfile\n\
                 \n\
                 from functools import partial\n\
                 \n\
                 if sys.version_info < (3, 0):  # Python 2\n\
                 \tfrom CGIHTTPServer import CGIHTTPRequestHandler\n\
                 \tfrom BaseHTTPServer import HTTPServer as ServerClass\n\
                 else:  # Python 3\n\
                 \tfrom http.server import CGIHTTPRequestHandler\n\
                 \tfrom http.server import HTTPServer as ServerClass\n\
                 \n\
                 \n\
                 # Those environment variables will be passed to the cgi script\n\
                 os.environ.update({{\n\
                 \t\"GIT_EXEC_PATH\": \"{exec}\",\n\
                 \t\"GIT_DIR\": \"{gitdir}\",\n\
                 \t\"GITWEB_CONFIG\": \"{cfg}\"\n\
                 }})\n\
                 \n\
                 \n\
                 class GitWebRequestHandler(CGIHTTPRequestHandler):\n\
                 \n\
                 \tdef log_message(self, format, *args):\n\
                 \t\t# Write access logs to stdout\n\
                 \t\tsys.stdout.write(\"%s - - [%s] %s\\n\" %\n\
                 \t\t\t\t(self.address_string(),\n\
                 \t\t\t\tself.log_date_time_string(),\n\
                 \t\t\t\tformat%args))\n\
                 \n\
                 \tdef do_HEAD(self):\n\
                 \t\tself.redirect_path()\n\
                 \t\tCGIHTTPRequestHandler.do_HEAD(self)\n\
                 \n\
                 \tdef do_GET(self):\n\
                 \t\tif self.path == \"/\":\n\
                 \t\t\tself.send_response(303, \"See Other\")\n\
                 \t\t\tself.send_header(\"Location\", \"/cgi-bin/gitweb.cgi\")\n\
                 \t\t\tself.end_headers()\n\
                 \t\t\treturn\n\
                 \t\tself.redirect_path()\n\
                 \t\tCGIHTTPRequestHandler.do_GET(self)\n\
                 \n\
                 \tdef do_POST(self):\n\
                 \t\tself.redirect_path()\n\
                 \t\tCGIHTTPRequestHandler.do_POST(self)\n\
                 \n\
                 \t# rewrite path of every request that is not gitweb.cgi to out of cgi-bin\n\
                 \tdef redirect_path(self):\n\
                 \t\tif not self.path.startswith(\"/cgi-bin/gitweb.cgi\"):\n\
                 \t\t\tself.path = self.path.replace(\"/cgi-bin/\", \"/\")\n\
                 \n\
                 \t# gitweb.cgi is the only thing that is ever going to be run here.\n\
                 \t# Ignore everything else\n\
                 \tdef is_cgi(self):\n\
                 \t\tresult = False\n\
                 \t\tif self.path.startswith('/cgi-bin/gitweb.cgi'):\n\
                 \t\t\tresult = CGIHTTPRequestHandler.is_cgi(self)\n\
                 \t\treturn result\n\
                 \n\
                 \n\
                 bind = \"0.0.0.0\"\n\
                 if \"{local}\" == \"true\":\n\
                 \tbind = \"127.0.0.1\"\n\
                 \n\
                 # Set our http root directory\n\
                 # This is a work around for a missing directory argument in older Python versions\n\
                 # as this was added to SimpleHTTPRequestHandler in Python 3.7\n\
                 os.chdir(\"{webroot}/\")\n\
                 \n\
                 GitWebRequestHandler.protocol_version = \"HTTP/1.0\"\n\
                 httpd = ServerClass((bind, {port}), GitWebRequestHandler)\n\
                 \n\
                 sa = httpd.socket.getsockname()\n\
                 print(\"Serving HTTP on\", sa[0], \"port\", sa[1], \"...\")\n\
                 httpd.serve_forever()\n",
                gitdir = self.fqgitdir.display(),
                exec = self.git_exec_path,
                cfg = self.gitweb_config.display(),
                local = self.local,
                webroot = webroot.display(),
                port = self.port,
            ),
        )?;
        make_executable(&script)
    }
}

// ---------------------------------------------------------------------------
// shell primitives
// ---------------------------------------------------------------------------

/// `case "$s" in *a*|*b*)` — true when any needle occurs in `s`.
fn contains_any(s: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| s.contains(n))
}

/// `echo "$httpd" | grep -- '-f *$'`: the command already ends with `-f`,
/// optionally followed by spaces.
fn ends_with_dash_f(httpd: &str) -> bool {
    httpd.trim_end_matches(' ').ends_with("-f")
}

/// `sed 's/-f$/-l/'` over `full_httpd`, which builds apache2's module-listing
/// command.
fn replace_trailing_dash_f_with_l(full_httpd: &str) -> String {
    match full_httpd.strip_suffix("-f") {
        Some(head) => format!("{head}-l"),
        None => full_httpd.to_string(),
    }
}

/// The three daemons `resolve_full_httpd` serves from a script it generates.
fn generated_server_script(httpd: &str) -> Option<&'static str> {
    // The script tests these in order: plackup, webrick, python.
    if httpd.contains("plackup") {
        Some("gitweb.psgi")
    } else if httpd.contains("webrick") {
        Some("webrick.rb")
    } else if httpd.contains("python") {
        Some("gitweb.py")
    } else {
        None
    }
}

/// `echo $httpd | cut -f1 -d' '` — unquoted, so the shell word-splits first and
/// `echo` rejoins with single spaces; the result is the first whitespace token.
fn first_word(httpd: &str) -> String {
    httpd.split_whitespace().next().unwrap_or_default().to_string()
}

/// `which <name> >/dev/null 2>&1`: an executable file of that name on `PATH`.
fn which(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
}

/// `test -x <path>`.
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// Run a shell-word-split command and capture its stdout, as
/// `$list_mods | grep …` does. Only stdout is redirected into the pipe, so the
/// child's stderr still reaches the terminal. A command that cannot run
/// contributes nothing, which is what the pipeline's empty input means to
/// `grep`.
fn run_and_capture(command: &str) -> String {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return String::new();
    };
    let out = Command::new(program)
        .args(words)
        .stderr(Stdio::inherit())
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// `"$base/$tail"` as the shell builds it — plain text concatenation, so an
/// absolute `$tail` (a `--httpd` given as a path) is appended rather than
/// replacing the base the way `Path::join` would.
fn join_raw(base: &Path, tail: &str) -> PathBuf {
    PathBuf::from(format!("{}/{tail}", base.display()))
}

/// `dirname "$fqgitdir"`.
fn dirname(path: &Path) -> String {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => "/".to_string(),
    }
}

/// `cat > <path> <<EOF … EOF`, creating the parent directory the script's
/// earlier `mkdir -p` would have.
fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// `chmod a+x <path>`.
fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// `ln -sf <target> <link>`.
fn symlink_force(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("linking {}", link.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
    }
    Ok(())
}

/// `kill $(cat "$fqgitdir/pid")` as bash's builtin runs it: the file's contents
/// word-split into arguments, each signalled with the default `SIGTERM`, and a
/// diagnostic per failure. The overall status is discarded by the caller.
fn kill_words(text: &str) {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        eprintln!(
            "kill: usage: kill [-s sigspec | -n signum | -sigspec] pid | jobspec ... or kill -l [sigspec]"
        );
        return;
    }
    let argv0 = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "git-instaweb".to_string());
    for word in words {
        let Ok(pid) = word.parse::<i32>() else {
            eprintln!("{argv0}: line 146: kill: {word}: arguments must be process or job IDs");
            continue;
        };
        #[cfg(unix)]
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let err = std::io::Error::last_os_error();
            let reason = match err.raw_os_error() {
                Some(libc::ESRCH) => "No such process".to_string(),
                Some(libc::EPERM) => "Operation not permitted".to_string(),
                _ => err.to_string(),
            };
            eprintln!("{argv0}: line 146: kill: ({pid}) - {reason}");
        }
        #[cfg(not(unix))]
        let _ = pid;
    }
}

/// The exec-path `git --exec-path` reports, which line 204 captures.
fn exec_path() -> String {
    if let Ok(p) = std::env::var("GIT_EXEC_PATH") {
        if !p.is_empty() {
            return p;
        }
    }
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => format!("{h}/.zvcs/bin"),
        _ => ".zvcs/bin".to_string(),
    }
}

/// The script's compiled-in `$(gitwebdir)` default for `root`, which autoconf
/// sets to `<prefix>/share/gitweb`. Applied to this binary's own prefix; zvcs
/// ships no gitweb, so `instaweb.gitwebdir` is normally what makes this useful.
fn default_gitwebdir() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "share/gitweb".to_string();
    };
    let prefix = exe.parent().and_then(Path::parent);
    match prefix {
        Some(p) => p.join("share").join("gitweb").to_string_lossy().into_owned(),
        None => "share/gitweb".to_string(),
    }
}

/// The mimetype table `lighttpd_conf` (line 277) writes and `plackup_conf`
/// (line 472) repeats, in the script's order. The final entry's empty extension
/// is the lighttpd catch-all.
const MIME_TYPES: &[(&str, &str)] = &[
    (".pdf", "application/pdf"),
    (".sig", "application/pgp-signature"),
    (".spl", "application/futuresplash"),
    (".class", "application/octet-stream"),
    (".ps", "application/postscript"),
    (".torrent", "application/x-bittorrent"),
    (".dvi", "application/x-dvi"),
    (".gz", "application/x-gzip"),
    (".pac", "application/x-ns-proxy-autoconfig"),
    (".swf", "application/x-shockwave-flash"),
    (".tar.gz", "application/x-tgz"),
    (".tgz", "application/x-tgz"),
    (".tar", "application/x-tar"),
    (".zip", "application/zip"),
    (".mp3", "audio/mpeg"),
    (".m3u", "audio/x-mpegurl"),
    (".wma", "audio/x-ms-wma"),
    (".wax", "audio/x-ms-wax"),
    (".ogg", "application/ogg"),
    (".wav", "audio/x-wav"),
    (".gif", "image/gif"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".png", "image/png"),
    (".xbm", "image/x-xbitmap"),
    (".xpm", "image/x-xpixmap"),
    (".xwd", "image/x-xwindowdump"),
    (".css", "text/css"),
    (".html", "text/html"),
    (".htm", "text/html"),
    (".js", "text/javascript"),
    (".asc", "text/plain"),
    (".c", "text/plain"),
    (".cpp", "text/plain"),
    (".log", "text/plain"),
    (".conf", "text/plain"),
    (".text", "text/plain"),
    (".txt", "text/plain"),
    (".dtd", "text/xml"),
    (".xml", "text/xml"),
    (".mpeg", "video/mpeg"),
    (".mpg", "video/mpeg"),
    (".mov", "video/quicktime"),
    (".qt", "video/quicktime"),
    (".avi", "video/x-msvideo"),
    (".asf", "video/x-ms-asf"),
    (".asx", "video/x-ms-asf"),
    (".wmv", "video/x-ms-wmv"),
    (".bz2", "application/x-bzip"),
    (".tbz", "application/x-bzip-compressed-tar"),
    (".tar.bz2", "application/x-bzip-compressed-tar"),
    ("", "text/plain"),
];

/// The shorter, comma-separated table `mongoose_conf` writes (line 450).
const MONGOOSE_MIME_TYPES: &str = ".gz=application/x-gzip,.tar.gz=application/x-tgz,.tgz=application/x-tgz,.tar=application/x-tar,.zip=application/zip,.gif=image/gif,.jpg=image/jpeg,.jpeg=image/jpeg,.png=image/png,.css=text/css,.html=text/html,.htm=text/html,.js=text/javascript,.c=text/plain,.cpp=text/plain,.log=text/plain,.conf=text/plain,.text=text/plain,.txt=text/plain,.dtd=text/xml,.bz2=application/x-bzip,.tbz=application/x-bzip-compressed-tar,.tar.bz2=application/x-bzip-compressed-tar";

// ---------------------------------------------------------------------------
// parseopt
// ---------------------------------------------------------------------------

/// Reproduce `git rev-parse --parseopt -- "$@"` over [`SPECS`], returning the
/// token list its `set -- …` line would install: options first in the form
/// parseopt normalises them to (short letter when the spec has one, else the
/// long name; `--no-<long>` for negations; a value option followed by its
/// value), then `--`, then the positionals in order.
fn parseopt(args: &[String]) -> Result<Vec<String>, Fail> {
    let mut out: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.iter();
    let mut no_more_opts = false;

    while let Some(arg) = rest.next() {
        let arg = arg.as_str();
        if no_more_opts {
            positional.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, and it is not reached past `--`. The parseopt spec has
        // no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block
        // `-h` prints.
        if arg == "--help-all" {
            return Err(Fail { error: None, usage: Usage::Stdout });
        }
        if let Some(name) = arg.strip_prefix("--") {
            parse_long(name, &mut rest, &mut out)?;
            continue;
        }
        // A bare `-`, and anything not starting with `-`, is a positional.
        let bundle = match arg.strip_prefix('-') {
            Some(b) if !b.is_empty() => b,
            _ => {
                positional.push(arg.to_string());
                continue;
            }
        };
        parse_shorts(bundle, &mut rest, &mut out)?;
    }

    out.push("--".to_string());
    out.extend(positional);
    Ok(out)
}

/// One `--name`, `--name=value` or `--no-name` argument.
fn parse_long<'a>(
    name: &str,
    rest: &mut impl Iterator<Item = &'a String>,
    out: &mut Vec<String>,
) -> Result<(), Fail> {
    // parseopt's own `--help`, which behaves as `-h` does.
    if name == "help" {
        return Err(Fail { error: None, usage: Usage::Stdout });
    }
    let (name, attached) = match name.split_once('=') {
        Some((n, v)) => (n, Some(v.to_string())),
        None => (name, None),
    };

    // Candidates are the long names plus their `no-` forms; an exact match wins
    // outright, otherwise a unique prefix matches and two matches are ambiguous.
    let candidates: Vec<(String, &Spec)> = SPECS
        .iter()
        .flat_map(|spec| {
            [
                (spec.long.to_string(), spec),
                (format!("no-{}", spec.long), spec),
            ]
        })
        .collect();
    let matched = match candidates.iter().find(|(full, _)| full == name) {
        Some(hit) => hit,
        None => {
            let mut hits = candidates.iter().filter(|(full, _)| full.starts_with(name));
            match (hits.next(), hits.next()) {
                (Some(hit), None) => hit,
                (Some((a, _)), Some((b, _))) => {
                    return Err(Fail {
                        error: Some(format!(
                            "error: ambiguous option: {name} (could be --{a} or --{b})"
                        )),
                        usage: Usage::Stdout,
                    })
                }
                _ => {
                    return Err(Fail {
                        error: Some(format!("error: unknown option `{name}'")),
                        usage: Usage::Stderr,
                    })
                }
            }
        }
    };
    let (full, spec) = (matched.0.as_str(), matched.1);

    // A negation never takes a value and is always emitted in long form; the
    // script's `case` names none of these, so they reach its `*)` arm.
    if let Some(long) = full.strip_prefix("no-") {
        if attached.is_some() {
            return Err(Fail {
                error: Some(format!("error: option `{full}' takes no value")),
                usage: Usage::None,
            });
        }
        out.push(format!("--no-{long}"));
        return Ok(());
    }

    let emitted = match spec.short {
        Some(c) => format!("-{c}"),
        None => format!("--{}", spec.long),
    };
    if !spec.takes_value {
        if attached.is_some() {
            return Err(Fail {
                error: Some(format!("error: option `{full}' takes no value")),
                usage: Usage::None,
            });
        }
        out.push(emitted);
        return Ok(());
    }
    let Some(value) = attached.or_else(|| rest.next().cloned()) else {
        return Err(Fail {
            error: Some(format!("error: option `{full}' requires a value")),
            usage: Usage::None,
        });
    };
    out.push(emitted);
    out.push(value);
    Ok(())
}

/// One `-abc` bundle: flags accumulate and the first value-taking letter
/// consumes the remainder of the bundle, or the next argument when empty.
fn parse_shorts<'a>(
    bundle: &str,
    rest: &mut impl Iterator<Item = &'a String>,
    out: &mut Vec<String>,
) -> Result<(), Fail> {
    let mut tail = bundle;
    while let Some(c) = tail.chars().next() {
        tail = &tail[c.len_utf8()..];
        if c == 'h' {
            return Err(Fail { error: None, usage: Usage::Stdout });
        }
        let Some(spec) = SPECS.iter().find(|s| s.short == Some(c)) else {
            return Err(Fail {
                error: Some(format!("error: unknown switch `{c}'")),
                usage: Usage::Stderr,
            });
        };
        if !spec.takes_value {
            out.push(format!("-{c}"));
            continue;
        }
        let value = if tail.is_empty() {
            rest.next().cloned()
        } else {
            Some(std::mem::take(&mut tail).to_string())
        };
        let Some(value) = value else {
            return Err(Fail {
                error: Some(format!("error: switch `{c}' requires a value")),
                usage: Usage::None,
            });
        };
        out.push(format!("-{c}"));
        out.push(value);
        break;
    }
    Ok(())
}
