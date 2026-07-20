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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Cooperative cancellation for a running job: a flag checked between steps, and
/// the pid of the currently-running child so a `zjob stop` can kill it.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    child: Arc<Mutex<Option<u32>>>,
}

impl Cancel {
    /// A handle that is never cancelled (synchronous in-process runs).
    pub fn none() -> Self {
        Self::default()
    }
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    /// Request cancellation and kill the current child (if any).
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(pid) = *self.child.lock().unwrap() {
            let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
        }
    }
    fn set_child(&self, pid: u32) {
        *self.child.lock().unwrap() = Some(pid);
    }
    fn clear_child(&self) {
        *self.child.lock().unwrap() = None;
    }
}

/// Result of running a job: exit code, combined output, and the resulting HEAD
/// sha (for a commit).
pub struct JobResult {
    pub ok: bool,
    pub output: String,
    pub sha_after: Option<String>,
    /// True if the job was cancelled mid-run (distinguishes stopped from failed).
    pub cancelled: bool,
}

/// This binary's path, for spawning child porcelain with a set cwd.
fn self_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot resolve current exe: {e}"))
}

/// Run one child `git <args>` in `cwd`; return `(success, combined-output)`.
/// Registers the child's pid on `cancel` so a concurrent `zjob stop` can kill it.
fn run(exe: &Path, cwd: &Path, args: &[String], cancel: &Cancel) -> (bool, String) {
    if cancel.cancelled() {
        return (false, "cancelled\n".to_string());
    }
    let mut child = match Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("spawn `git {}` failed: {e}\n", args.join(" "))),
    };
    cancel.set_child(child.id());
    let out = child.wait_with_output();
    cancel.clear_child();
    match out {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            // A killed child reports failure; treat cancellation distinctly.
            (out.status.success() && !cancel.cancelled(), s)
        }
        Err(e) => (false, format!("`git {}` failed: {e}\n", args.join(" "))),
    }
}

/// Execute a job spec, returning its result. Never panics. `cancel` allows a
/// concurrent `zjob stop` to abort it between steps and kill the current child.
pub fn execute(spec: &Value, cancel: &Cancel) -> JobResult {
    match execute_inner(spec, cancel) {
        Ok(mut r) => {
            r.cancelled = cancel.cancelled();
            r
        }
        Err(e) => JobResult {
            ok: false,
            output: format!("{e:#}\n"),
            sha_after: None,
            cancelled: cancel.cancelled(),
        },
    }
}

fn execute_inner(spec: &Value, cancel: &Cancel) -> Result<JobResult> {
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
                let (a_ok, a_out) = run(&exe, workdir, &args, cancel);
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
                    cancel,
                );
                output.push_str(&c_out);
                ok &= c_ok;
            }

            // Optional push.
            if ok && spec.get("push").and_then(Value::as_bool).unwrap_or(false) {
                let (p_ok, p_out) = run(&exe, workdir, &["push".into()], cancel);
                output.push_str(&p_out);
                ok &= p_ok;
            }

            // Only report the resulting HEAD when the commit actually succeeded.
            let sha_after = if ok { head_sha(&exe, workdir) } else { None };
            Ok(JobResult { ok, output, sha_after, cancelled: false })
        }
        "push" => {
            let mut args = vec!["push".to_string()];
            if let Some(rs) = spec.get("refspec").and_then(Value::as_str) {
                args.extend(rs.split_whitespace().map(String::from));
            }
            let (ok, output) = run(&exe, workdir, &args, cancel);
            Ok(JobResult { ok, output, sha_after: None, cancelled: false })
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
