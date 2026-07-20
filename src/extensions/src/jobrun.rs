//! Execute an async `z`-verb job (`zcommit`/`zpush`).
//!
//! A job runs the **faithful** porcelain by spawning child `git` (this same
//! shadow binary) with the job's working directory as cwd — so it reuses the
//! exact tested `add`/`commit`/`push` implementations and their fair-lock
//! serialization, rather than re-deriving staging/commit logic. "Async" is only
//! that the daemon runs it off the submitting client's critical path.
//!
//! The spec is a small JSON object carried over the daemon socket:
//! ```json
//! {"kind":"commit","workdir":"/p","paths":["a"],"message":"m","push":false}
//! {"kind":"push","workdir":"/p","refspec":"origin main"}
//! ```

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Result of running a job: exit code, combined output, and the resulting HEAD
/// sha (for a commit).
pub struct JobResult {
    pub ok: bool,
    pub output: String,
    pub sha_after: Option<String>,
}

/// This binary's path, for spawning child porcelain with a set cwd.
fn self_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot resolve current exe: {e}"))
}

/// Run one child `git <args>` in `cwd`; return `(success, combined-output)`.
fn run(exe: &Path, cwd: &Path, args: &[String]) -> (bool, String) {
    match Command::new(exe).args(args).current_dir(cwd).output() {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), s)
        }
        Err(e) => (false, format!("spawn `git {}` failed: {e}\n", args.join(" "))),
    }
}

/// Execute a job spec, returning its result. Never panics.
pub fn execute(spec: &Value) -> JobResult {
    match execute_inner(spec) {
        Ok(r) => r,
        Err(e) => JobResult {
            ok: false,
            output: format!("{e:#}\n"),
            sha_after: None,
        },
    }
}

fn execute_inner(spec: &Value) -> Result<JobResult> {
    let exe = self_exe()?;
    let kind = spec.get("kind").and_then(Value::as_str).unwrap_or("");
    let workdir = spec
        .get("workdir")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("job spec missing workdir"))?;
    let workdir = Path::new(workdir);

    match kind {
        "commit" => {
            let mut output = String::new();
            let mut ok = true;

            // Stage the given paths (if any) first.
            let paths: Vec<String> = spec
                .get("paths")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if !paths.is_empty() {
                let mut args = vec!["add".to_string()];
                args.extend(paths);
                let (a_ok, a_out) = run(&exe, workdir, &args);
                output.push_str(&a_out);
                ok &= a_ok;
            }

            // Commit.
            if ok {
                let message = spec
                    .get("message")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("commit job missing message"))?;
                let (c_ok, c_out) = run(
                    &exe,
                    workdir,
                    &["commit".into(), "-m".into(), message.to_string()],
                );
                output.push_str(&c_out);
                ok &= c_ok;
            }

            // Optional push.
            if ok && spec.get("push").and_then(Value::as_bool).unwrap_or(false) {
                let (p_ok, p_out) = run(&exe, workdir, &["push".into()]);
                output.push_str(&p_out);
                ok &= p_ok;
            }

            let sha_after = head_sha(&exe, workdir);
            Ok(JobResult { ok, output, sha_after })
        }
        "push" => {
            let mut args = vec!["push".to_string()];
            if let Some(rs) = spec.get("refspec").and_then(Value::as_str) {
                args.extend(rs.split_whitespace().map(String::from));
            }
            let (ok, output) = run(&exe, workdir, &args);
            Ok(JobResult { ok, output, sha_after: None })
        }
        other => Err(anyhow!("unknown job kind {other:?}")),
    }
}

fn head_sha(exe: &Path, cwd: &Path) -> Option<String> {
    let out = Command::new(exe)
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}
