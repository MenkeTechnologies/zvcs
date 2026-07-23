//! `git web--browse` — launch a web browser on the given URLs/files.
//!
//! Stock `git-web--browse` is a POSIX shell script (`git-web--browse.sh`)
//! sourcing `git-sh-setup` with `NONGIT_OK=Yes`. It touches no object database,
//! no index and no refs: its entire logic is argument scanning, `git config`
//! lookups, `type`-based `PATH` probing, and finally spawning a browser. All of
//! that has substrate here — `gix::config` for the lookups (including the
//! outside-a-repository fallback the script needs), `std::process` for the
//! spawns, and a child `sh -c` for the one `eval` the script performs — so this
//! is a full port rather than a skeleton.
//!
//! ### Covered (against git 2.55.0's `git-web--browse` and `git-sh-setup`)
//!
//! * `git-sh-setup`'s `-h` short circuit: only `$1` is inspected, the usage line
//!   goes to **stdout**, exit 0. Every other error path is `die`/`usage`, i.e.
//!   one line on stderr and exit 1.
//! * The option scanner verbatim, including its quirks: `-b`/`-t`/`-c` match
//!   exactly while `--browser*`/`--tool*`/`--config*` match as *prefixes* (so
//!   `--browserZZZ=x` sets the browser and `-bfirefox` is rejected); the
//!   `case "$#,$1"` split between an attached `=value`, a detached `$2`, and the
//!   `usage` taken when the flag is the last argument; `expr`'s capture of
//!   everything after the *first* `=`; `--` breaking the loop **without being
//!   shifted**, so it is passed through to the browser as an argument; any other
//!   `-*` being `usage`; and the trailing `test $# = 0 && usage`.
//! * Browser resolution: `--config`'s variable then `web.browser`, the
//!   `git config option <var> set to unknown browser: <b>` / `Resetting to
//!   default...` pair on an unknown value, `valid_tool` (the fixed list plus a
//!   non-empty `browser.<tool>.cmd`), `init_browser_path`
//!   (`browser.<tool>.path`, the `chromium` → `chromium-browser` fallback, and
//!   the `${browser_path:="$1"}` default), and the three `die` messages
//!   (`No known browser available.`, `Unknown browser '<b>'.`,
//!   `The browser <b> is not available as '<path>'.`).
//! * Candidate auto-detection: the `DISPLAY` / no-`DISPLAY` lists, the
//!   `KDE_FULL_SESSION = true` konqueror prepend, the `SECURITYSESSIONID` ||
//!   `TERM_PROGRAM` `open` prepend, and the `/bin/start` / `/usr/bin/cygstart`
//!   prepends, probed in order with `init_browser_path` + `type`.
//! * The dispatch table and the exit code each arm yields: backgrounded arms
//!   (firefox family incl. the `-version` `< 2` `-new-tab` suppression, the
//!   chromium family, konqueror's `kfmclient newTab` rewrite, opera/dillo)
//!   return 0 immediately; the `w3m`/`elinks`/`links`/`lynx`/`open`/`cygstart`/
//!   `xdg-open` arm runs in the foreground and propagates the child's status
//!   (`128 + signal` when it is killed); `start` `exec`s with the literal
//!   `"web-browse"` argument; a custom `browser.<tool>.cmd` is run through
//!   `sh -c` so the command text keeps full shell parsing and `"$@"` expands to
//!   the URLs as separate words.
//!
//! ### Not covered
//!
//! * `git config $opt` is unquoted in the script, so a `--config` value
//!   containing whitespace word-splits into several `git config` arguments —
//!   which in stock git *writes* config. That is rejected here rather than
//!   reproduced.
//! * Config values are decoded lossily, so a browser name or path that is not
//!   valid UTF-8 diverges. Argument values cannot: this entry point already
//!   takes `&[String]`.
//! * `type` here means "an executable on `PATH`, or an executable at the given
//!   path"; the shell builtins, functions and aliases a real `type` also finds
//!   are not consulted. No supported browser name is a shell builtin.

use anyhow::{bail, Result};
use std::path::Path;
use std::process::{Command, ExitCode, ExitStatus};

/// The `USAGE` string the script sets before sourcing `git-sh-setup`.
const USAGE: &str = "[--browser=browser|--tool=browser] [--config=conf.var] url/file ...";

/// The browsers `valid_tool()` accepts without consulting `browser.<tool>.cmd`.
const KNOWN_TOOLS: &[&str] = &[
    "firefox",
    "iceweasel",
    "seamonkey",
    "iceape",
    "chrome",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "konqueror",
    "opera",
    "w3m",
    "elinks",
    "links",
    "lynx",
    "dillo",
    "open",
    "start",
    "cygstart",
    "xdg-open",
];

pub fn web__browse(args: &[String]) -> Result<ExitCode> {
    // `git-sh-setup` inspects `$1` alone, echoes LONG_USAGE (which is just the
    // usage line, as this script sets no LONG_USAGE) to stdout, and exits 0.
    if args.first().map(String::as_str) == Some("-h") {
        println!("usage: git web--browse {USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    let mut browser = String::new();
    let mut conf = String::new();
    let mut i = 0usize;

    let rest: &[String] = loop {
        if i == args.len() {
            break &args[i..];
        }
        let a = args[i].as_str();
        // `$#` as the script's `case "$#,$1"` sees it.
        let remaining = args.len() - i;

        let is_browser_opt = a == "-b" || a == "-t" || a.starts_with("--browser") || a.starts_with("--tool");
        let is_conf_opt = a == "-c" || a.starts_with("--config");

        if is_browser_opt || is_conf_opt {
            let value = match take_value(a, remaining, args.get(i + 1)) {
                Some(Taken { value, shifted }) => {
                    if shifted {
                        i += 1;
                    }
                    value
                }
                None => return Ok(usage()),
            };
            if is_browser_opt {
                browser = value;
            } else {
                conf = value;
            }
        } else if a == "--" {
            // `break` fires before the `shift`, so `--` stays in "$@".
            break &args[i..];
        } else if a.starts_with('-') {
            return Ok(usage());
        } else {
            break &args[i..];
        }
        i += 1;
    };

    if rest.is_empty() {
        return Ok(usage());
    }

    let cfg = config()?;
    // `valid_custom_tool` assigns this global; the `*)` dispatch arm reads it.
    let mut browser_cmd: Option<String> = None;

    if browser.is_empty() {
        // `for opt in "$conf" "web.browser"` — `$opt` survives the loop, and
        // names whichever variable produced the value on a `break`.
        let mut opt_name = String::new();
        for opt in [conf.as_str(), "web.browser"] {
            opt_name = opt.to_string();
            if opt.is_empty() {
                continue;
            }
            if opt.split_whitespace().count() > 1 {
                bail!("config variable {opt:?} word-splits into several `git config` arguments (unsupported)");
            }
            if let Some(v) = git_config(&cfg, opt) {
                browser = v;
                break;
            }
        }
        if !browser.is_empty() && !valid_tool(&cfg, &browser, &mut browser_cmd) {
            eprintln!("git config option {opt_name} set to unknown browser: {browser}");
            eprintln!("Resetting to default...");
            browser.clear();
        }
    }

    let browser_path;
    if browser.is_empty() {
        let Some((name, path)) = detect_browser(&cfg) else {
            return Ok(die("No known browser available."));
        };
        browser = name;
        browser_path = path;
    } else {
        if !valid_tool(&cfg, &browser, &mut browser_cmd) {
            return Ok(die(&format!("Unknown browser '{browser}'.")));
        }
        browser_path = init_browser_path(&cfg, &browser);
        if browser_cmd.is_none() && !type_found(&browser_path) {
            return Ok(die(&format!(
                "The browser {browser} is not available as '{browser_path}'."
            )));
        }
    }

    launch(&browser, &browser_path, browser_cmd.as_deref(), rest)
}

/// A value pulled off the command line by the `case "$#,$1"` block.
struct Taken {
    value: String,
    /// Whether the value came from `$2`, so the loop must `shift` once extra.
    shifted: bool,
}

/// The script's `case "$#,$1"` value extraction, shared by the browser and
/// config options. `None` is the `1,*)` arm, i.e. `usage`.
///
/// An attached value is cut with `expr "z$1" : 'z-[^=]*=\(.*\)'`, whose
/// `[^=]*` cannot cross a `=`, so the capture is everything after the *first*
/// one — `--browser=a=b` yields `a=b`.
fn take_value(arg: &str, remaining: usize, next: Option<&String>) -> Option<Taken> {
    if let Some((_, v)) = arg.split_once('=') {
        return Some(Taken {
            value: v.to_string(),
            shifted: false,
        });
    }
    if remaining == 1 {
        return None;
    }
    Some(Taken {
        value: next.cloned().unwrap_or_default(),
        shifted: true,
    })
}

/// `usage()` from `git-sh-setup`: the usage line on stderr, exit 1.
fn usage() -> ExitCode {
    eprintln!("usage: git web--browse {USAGE}");
    ExitCode::FAILURE
}

/// `die()` from `git-sh-setup`: `printf >&2 '%s\n'`, exit 1.
fn die(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::FAILURE
}

/// The configuration `git config` would read. The script runs with
/// `NONGIT_OK=Yes`, so outside a repository the global set plus the
/// `GIT_CONFIG_*` overrides still apply.
fn config() -> Result<gix::config::File> {
    Ok(match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => {
            let mut file = gix::config::File::from_globals()?;
            file.append(gix::config::File::from_environment_overrides()?)?;
            file
        }
    })
}

/// `git config <key>` as the script consumes it: the last value for the key, or
/// `None`. Every caller guards with `test -z`, so an empty value reads as unset.
fn git_config(cfg: &gix::config::File, key: &str) -> Option<String> {
    let v = cfg.string(key)?;
    let v = String::from_utf8_lossy(&v).into_owned();
    (!v.is_empty()).then_some(v)
}

/// `valid_tool()`: a name from [`KNOWN_TOOLS`], or one with a non-empty
/// `browser.<tool>.cmd`. The custom path also publishes `browser_cmd`, exactly
/// as `valid_custom_tool` sets that global.
fn valid_tool(cfg: &gix::config::File, tool: &str, browser_cmd: &mut Option<String>) -> bool {
    if KNOWN_TOOLS.contains(&tool) {
        return true;
    }
    *browser_cmd = git_config(cfg, &format!("browser.{tool}.cmd"));
    browser_cmd.is_some()
}

/// `init_browser_path()`: `browser.<tool>.path`, the chromium fallback, then the
/// tool name itself.
fn init_browser_path(cfg: &gix::config::File, tool: &str) -> String {
    if let Some(p) = git_config(cfg, &format!("browser.{tool}.path")) {
        return p;
    }
    if tool == "chromium" && type_found("chromium-browser") {
        return "chromium-browser".to_string();
    }
    tool.to_string()
}

/// The `browser_candidates` probe: build the ordered list from the environment,
/// then return the first entry whose `init_browser_path` is on `PATH`.
fn detect_browser(cfg: &gix::config::File) -> Option<(String, String)> {
    let mut candidates: Vec<&str> = if env_nonempty("DISPLAY").is_some() {
        let mut v = vec![
            "firefox",
            "iceweasel",
            "google-chrome",
            "chrome",
            "chromium",
            "chromium-browser",
            "konqueror",
            "opera",
            "seamonkey",
            "iceape",
            "w3m",
            "elinks",
            "links",
            "lynx",
            "dillo",
            "xdg-open",
        ];
        if std::env::var("KDE_FULL_SESSION").as_deref() == Ok("true") {
            v.insert(0, "konqueror");
        }
        v
    } else {
        vec!["w3m", "elinks", "links", "lynx"]
    };

    // SECURITYSESSIONID indicates an OS X GUI login session.
    if env_nonempty("SECURITYSESSIONID").is_some() || env_nonempty("TERM_PROGRAM").is_some() {
        candidates.insert(0, "open");
    }
    if test_x(Path::new("/bin/start")) {
        candidates.insert(0, "start");
    }
    if test_x(Path::new("/usr/bin/cygstart")) {
        candidates.insert(0, "cygstart");
    }

    candidates.into_iter().find_map(|c| {
        let path = init_browser_path(cfg, c);
        type_found(&path).then(|| (c.to_string(), path))
    })
}

/// The `case "$browser"` dispatch, returning the exit status the script would.
fn launch(browser: &str, browser_path: &str, browser_cmd: Option<&str>, urls: &[String]) -> Result<ExitCode> {
    match browser {
        // Check the version because firefox < 2.0 does not support "-new-tab".
        "firefox" | "iceweasel" | "seamonkey" | "iceape" => {
            let mut cmd = Command::new(browser_path);
            if firefox_major(browser_path).is_none_or(|v| v >= 2) {
                cmd.arg("-new-tab");
            }
            cmd.args(urls);
            background(cmd)
        }
        // No need to specify newTab; it is the default in chromium.
        "google-chrome" | "chrome" | "chromium" | "chromium-browser" | "opera" | "dillo" => {
            let mut cmd = Command::new(browser_path);
            cmd.args(urls);
            background(cmd)
        }
        "konqueror" => {
            let base = Path::new(browser_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match base.as_str() {
                // It is simpler to use kfmclient to open a new tab in konqueror.
                "konqueror" => {
                    let rewritten = format!(
                        "{}kfmclient",
                        browser_path.strip_suffix("konqueror").unwrap_or(browser_path)
                    );
                    if !type_found(&rewritten) {
                        return Ok(die(&format!("No '{rewritten}' found.")));
                    }
                    let mut cmd = Command::new(&rewritten);
                    cmd.arg("newTab").args(urls);
                    background(cmd)
                }
                "kfmclient" => {
                    let mut cmd = Command::new(browser_path);
                    cmd.arg("newTab").args(urls);
                    background(cmd)
                }
                _ => {
                    let mut cmd = Command::new(browser_path);
                    cmd.args(urls);
                    background(cmd)
                }
            }
        }
        "w3m" | "elinks" | "links" | "lynx" | "open" | "cygstart" | "xdg-open" => {
            let status = Command::new(browser_path).args(urls).status()?;
            Ok(ExitCode::from(wait_status(status)))
        }
        "start" => {
            let mut cmd = Command::new(browser_path);
            // The script passes the quotes literally: `'"web-browse"'`.
            cmd.arg("\"web-browse\"").args(urls);
            exec(cmd)
        }
        // A custom `browser.<tool>.cmd`: `( eval "$browser_cmd \"\$@\"" )`.
        // Running `sh -c '<cmd> "$@"' sh <urls>` keeps the command text under
        // full shell parsing while each URL stays one word.
        _ => {
            let Some(cmd_text) = browser_cmd else {
                // `if test -n "$browser_cmd"` with no else — the compound is 0.
                return Ok(ExitCode::SUCCESS);
            };
            let status = Command::new("sh")
                .arg("-c")
                .arg(format!("{cmd_text} \"$@\""))
                .arg("sh")
                .args(urls)
                .status()?;
            Ok(ExitCode::from(wait_status(status)))
        }
    }
}

/// `<cmd> &` — the shell does not wait, so the script's status is 0 and the
/// child outlives it.
fn background(mut cmd: Command) -> Result<ExitCode> {
    cmd.spawn()?;
    Ok(ExitCode::SUCCESS)
}

/// `exec <cmd>` — replace this process. Only reachable through the MinGW-only
/// `start` arm; where `exec` is unavailable, wait for the child instead.
fn exec(mut cmd: Command) -> Result<ExitCode> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        Ok(ExitCode::from(wait_status(status)))
    }
}

/// `expr "$($browser_path -version)" : '.* \([0-9][0-9]*\)\..*'`.
///
/// The leading `.*` is greedy, so the capture is the *rightmost* run of digits
/// that is preceded by a space and followed by a `.`. A failed run, empty
/// output or no match leaves `vers` empty, and the script's `test "$vers" -lt 2`
/// then errors out — which keeps `-new-tab`, hence `None` here.
fn firefox_major(browser_path: &str) -> Option<u64> {
    let out = Command::new(browser_path).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim_end_matches('\n');
    let bytes = text.as_bytes();

    for start in (0..bytes.len()).rev() {
        if bytes[start] != b' ' {
            continue;
        }
        let digits_end = bytes[start + 1..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map(|n| start + 1 + n)
            .unwrap_or(bytes.len());
        if digits_end > start + 1 && bytes.get(digits_end) == Some(&b'.') {
            return text[start + 1..digits_end].parse().ok();
        }
    }
    None
}

/// An environment variable as `test -n` sees it: unset and empty are alike.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `test -x <path>`.
fn test_x(path: &Path) -> bool {
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

/// `type <cmd> >/dev/null 2>&1` for the command forms this script produces: a
/// name containing `/` is checked in place, a bare name is searched on `PATH`
/// (where an empty element means the current directory, as in the shell).
fn type_found(cmd: &str) -> bool {
    if cmd.is_empty() {
        return false;
    }
    let executable = |p: &Path| p.is_file() && test_x(p);
    if cmd.contains('/') {
        return executable(Path::new(cmd));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| executable(&dir.join(cmd)))
}

/// The `$?` a shell would see for a finished child: its exit code, or `128 + n`
/// when it died of signal `n`.
fn wait_status(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128u8.saturating_add(status.signal().unwrap_or(0) as u8)
    }
    #[cfg(not(unix))]
    {
        128
    }
}
