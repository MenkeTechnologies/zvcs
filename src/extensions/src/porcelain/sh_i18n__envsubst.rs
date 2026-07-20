//! `git sh-i18n--envsubst` — git's stripped-down copy of GNU `envsubst(1)`.
//!
//! A direct port of upstream `sh-i18n--envsubst.c` (itself derived from
//! gettext's `gettext-runtime/src/envsubst.c`). The whole program is
//! self-contained C over `argv`, the environment and stdin — it never opens a
//! repository — so this is a full port, not a shim: `cmd_main`'s `switch
//! (argc)`, `find_variables`, `print_variables` and `subst_from_stdin` are
//! reproduced branch-for-branch, so stdout, stderr and the exit code are
//! byte-identical.
//!
//! Interface (there are no options; every argument is data):
//!
//!   * no arguments        → `error: we won't substitute all variables on stdin
//!                           for you`, stdin is *not* read, nothing is written
//!                           to stdout. Upstream's `all_variables` path is
//!                           commented out in the C, so it is absent here too.
//!   * one argument        → that argument is the shell format. Its `$VAR` /
//!                           `${VAR}` references form the substitution set;
//!                           stdin is copied to stdout with exactly those
//!                           variables expanded from the environment.
//!   * two arguments       → the first must be `--variables`; the variable names
//!                           referenced by the second are printed one per line.
//!                           If the first is anything else, upstream still runs
//!                           `print_variables(argv[2])` after emitting `error:
//!                           first argument must be --variables when two are
//!                           given` — that non-fatal fall-through is preserved.
//!   * three or more       → `error: too many arguments`, no other output.
//!
//! Every one of those paths exits 0: upstream calls git's `error()`, which
//! returns rather than exiting, and `cmd_main` ends in `return EXIT_SUCCESS`.
//! The only `EXIT_FAILURE` upstream can produce is a failure to flush or close
//! stderr, which has no Rust analogue and is not reproduced.
//!
//! Not covered: `--help` as the lone argument. Real `git` intercepts it in
//! `git.c` and renders the man page before this program ever runs, so it is
//! rejected here rather than being silently treated as a format string.
//!
//! Parsing is done on raw bytes throughout, matching the C, which restricts
//! variable names to ASCII `[A-Za-z_][A-Za-z0-9_]*` precisely to stay
//! encoding-agnostic. Environment values are emitted as raw bytes on unix.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::ExitCode;

/// Upstream `error()`: writes `error: <msg>` to stderr and returns.
fn error(msg: &str) {
    eprintln!("error: {msg}");
}

/// Read one byte and advance, or report EOF without advancing.
///
/// Stands in for `do_getc`/`do_ungetc`: "ungetting" is simply not advancing.
fn next(input: &[u8], pos: &mut usize) -> Option<u8> {
    let c = input.get(*pos).copied();
    if c.is_some() {
        *pos += 1;
    }
    c
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Port of `find_variables`: scan for `$VARIABLE` / `${VARIABLE}` and invoke
/// `callback` with each valid name.
///
/// Note the deliberate lack of backtracking, which upstream shares: after a
/// `$` that is not followed by a name start, scanning resumes at the character
/// *after* the `$` (and after a consumed `{`), so `$$FOO` yields `FOO` while
/// `${1}` yields nothing.
fn find_variables(string: &[u8], callback: &mut impl FnMut(&[u8])) {
    let mut i = 0usize;
    while i < string.len() {
        let ch = string[i];
        i += 1;
        if ch != b'$' {
            continue;
        }

        // `if (*string == '{') string++;`
        let opening_brace = string.get(i) == Some(&b'{');
        if opening_brace {
            i += 1;
        }

        let variable_start = i;
        // `c = *string` — past the end reads the NUL terminator in C.
        if !string.get(i).copied().is_some_and(is_name_start) {
            continue;
        }
        loop {
            i += 1;
            if !string.get(i).copied().is_some_and(is_name_char) {
                break;
            }
        }
        let variable_end = i;

        let valid = if opening_brace {
            // `${NAME` without the closing brace is not a variable reference.
            if string.get(i) == Some(&b'}') {
                i += 1;
                true
            } else {
                false
            }
        } else {
            true
        };

        if valid {
            callback(&string[variable_start..variable_end]);
        }
    }
}

/// Port of `print_variables`: each referenced name on its own line.
fn print_variables(string: &[u8], out: &mut Vec<u8>) {
    find_variables(string, &mut |name: &[u8]| {
        out.extend_from_slice(name);
        out.push(b'\n');
    });
}

/// Port of `note_variables`: the set of names eligible for substitution.
///
/// Upstream sorts a list and binary-searches it; a hash set is the same
/// membership predicate and duplicates are equally irrelevant.
fn note_variables(string: &[u8]) -> HashSet<Vec<u8>> {
    let mut set = HashSet::new();
    find_variables(string, &mut |name: &[u8]| {
        set.insert(name.to_vec());
    });
    set
}

/// Look up `name` in the environment, returning its value as raw bytes.
fn getenv_bytes(name: &[u8]) -> Option<Vec<u8>> {
    // Names are ASCII by construction (`is_name_start`/`is_name_char`).
    let key = std::str::from_utf8(name).ok()?;
    let value = std::env::var_os(key)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(value.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Some(value.to_string_lossy().into_owned().into_bytes())
    }
}

/// Port of `subst_from_stdin`: copy `input` to `out`, expanding only the
/// variables named in `vars`.
///
/// A reference to a variable that is in `vars` but unset in the environment
/// expands to nothing (upstream: `if (env_value) fputs(...)`). A reference not
/// in `vars` is re-emitted verbatim, including the braces it was written with.
fn subst_from_stdin(input: &[u8], vars: &HashSet<Vec<u8>>, out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i < input.len() {
        let c = input[i];
        i += 1;
        if c != b'$' {
            out.push(c);
            continue;
        }

        let mut pos = i;
        let mut opening_brace = false;
        let mut closing_brace = false;
        let mut ch = next(input, &mut pos);
        if ch == Some(b'{') {
            opening_brace = true;
            ch = next(input, &mut pos);
        }

        if ch.is_some_and(is_name_start) {
            // Accumulate the name.
            let mut buffer = Vec::new();
            loop {
                buffer.push(ch.expect("loop entered with a name character"));
                ch = next(input, &mut pos);
                match ch {
                    Some(x) if is_name_char(x) => continue,
                    _ => break,
                }
            }

            let mut valid;
            if opening_brace {
                if ch == Some(b'}') {
                    closing_brace = true;
                    valid = true;
                } else {
                    valid = false;
                    if ch.is_some() {
                        pos -= 1; // do_ungetc
                    }
                }
            } else {
                valid = true;
                if ch.is_some() {
                    pos -= 1; // do_ungetc
                }
            }

            if valid && !vars.contains(&buffer) {
                valid = false;
            }

            if valid {
                if let Some(value) = getenv_bytes(&buffer) {
                    out.extend_from_slice(&value);
                }
            } else {
                out.push(b'$');
                if opening_brace {
                    out.push(b'{');
                }
                out.extend_from_slice(&buffer);
                if closing_brace {
                    out.push(b'}');
                }
            }
        } else {
            if ch.is_some() {
                pos -= 1; // do_ungetc
            }
            out.push(b'$');
            if opening_brace {
                out.push(b'{');
            }
        }

        i = pos;
    }
}

/// Read all of stdin.
///
/// Upstream reads byte-at-a-time and, on a read error, emits `error: error
/// while reading standard input` and stops — the bytes already consumed are
/// still written out. `read_to_end` leaves the partial read in the buffer, so
/// that behaviour is preserved.
fn read_stdin() -> Vec<u8> {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() {
        error("error while reading standard input");
    }
    buf
}

/// `git sh-i18n--envsubst` — see the module docs for the full contract.
pub fn sh_i18n__envsubst(args: &[String]) -> Result<ExitCode> {
    if args.len() == 1 && args[0] == "--help" {
        bail!("--help (man page display) is not supported; this command takes no options");
    }

    let mut out: Vec<u8> = Vec::new();

    // Upstream's `switch (argc)`; `argc` counts argv[0], hence the offset.
    match args.len() {
        0 => {
            // The `all_variables` branch is commented out upstream: stdin is
            // not read and nothing is written to stdout.
            error("we won't substitute all variables on stdin for you");
        }
        1 => {
            let vars = note_variables(args[0].as_bytes());
            let input = read_stdin();
            subst_from_stdin(&input, &vars, &mut out);
        }
        2 => {
            if args[0] != "--variables" {
                // Non-fatal upstream: the error is reported and the names are
                // printed anyway.
                error("first argument must be --variables when two are given");
            }
            print_variables(args[1].as_bytes(), &mut out);
        }
        _ => {
            error("too many arguments");
        }
    }

    std::io::stdout().write_all(&out)?;
    std::io::stdout().flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars_of(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        print_variables(s.as_bytes(), &mut out);
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn subst(format: &str, input: &str) -> String {
        let vars = note_variables(format.as_bytes());
        let mut out = Vec::new();
        subst_from_stdin(input.as_bytes(), &vars, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn find_variables_matches_upstream_scanner() {
        assert_eq!(vars_of("a $FOO b ${BAR} c"), ["FOO", "BAR"]);
        // No backtracking after a `$` that starts nothing: `$$FOO` still finds FOO.
        assert_eq!(vars_of("$$FOO"), ["FOO"]);
        // `${` with no closing brace is not a reference.
        assert_eq!(vars_of("${FOO"), Vec::<String>::new());
        // Names may not start with a digit.
        assert_eq!(vars_of("${1} $2"), Vec::<String>::new());
        // A trailing bare `$` is harmless.
        assert_eq!(vars_of("tail $"), Vec::<String>::new());
        // Duplicates are reported once per occurrence, in order.
        assert_eq!(vars_of("$A $A"), ["A", "A"]);
    }

    #[test]
    fn only_variables_named_in_the_format_are_substituted() {
        // SAFETY: single-threaded test process; no concurrent env access.
        unsafe {
            std::env::set_var("ZVCS_ENVSUBST_T1", "one");
            std::env::set_var("ZVCS_ENVSUBST_T2", "two");
        }
        // Named in the format → expanded.
        assert_eq!(
            subst("$ZVCS_ENVSUBST_T1", "x=$ZVCS_ENVSUBST_T1"),
            "x=one"
        );
        // Set in the environment but absent from the format → left verbatim,
        // braces preserved exactly as written.
        assert_eq!(
            subst("$ZVCS_ENVSUBST_T1", "y=${ZVCS_ENVSUBST_T2}"),
            "y=${ZVCS_ENVSUBST_T2}"
        );
        // Named in the format but unset → expands to nothing.
        assert_eq!(subst("$ZVCS_ENVSUBST_UNSET", "z=$ZVCS_ENVSUBST_UNSET"), "z=");
    }

    #[test]
    fn unterminated_brace_is_emitted_verbatim_and_rescanned() {
        // SAFETY: single-threaded test process; no concurrent env access.
        unsafe {
            std::env::set_var("ZVCS_ENVSUBST_T3", "v");
        }
        // `${NAME` without `}` is not a reference; the trailing text after the
        // name is re-read, so the following reference still expands.
        assert_eq!(
            subst("$ZVCS_ENVSUBST_T3", "${ZVCS_ENVSUBST_T3 $ZVCS_ENVSUBST_T3"),
            "${ZVCS_ENVSUBST_T3 v"
        );
        // A `$` followed by a non-name character passes through untouched.
        assert_eq!(subst("$ZVCS_ENVSUBST_T3", "cost: $5 ${9}"), "cost: $5 ${9}");
    }
}
