//! Native (Rust) plugin host — the runtime half of the `znative` plugin system.
//!
//! Ported from zshrs's `src/extensions/plugin_host.rs`. zshrs generalises zsh's
//! `Src/module.c` — which `dlopen`s C modules that call `addbuiltin` against the
//! shell's own symbols — into a stable, versioned C ABI so a third party ships a
//! compiled `cdylib` and loads it at runtime. This is that host, retargeted from
//! shell builtins to git subcommands: a plugin registers verbs, and the shadow
//! `git` binary serves them from the `dlopen`ed library instead of forking a
//! `git-<verb>` script off PATH.
//!
//! ## Where plugin verbs resolve
//!
//! [`try_verb`] is consulted in `lib.rs` for a verb that is neither a builtin
//! nor an alias, BEFORE `external::try_dashed` — the same slot git gives dashed
//! externals, and the same precedence zshrs gives plugin builtins (after real
//! builtins, before PATH). [`try_override`] runs earlier still, at the top of
//! [`crate::dispatch::run`], for a plugin that replaces an existing verb.
//!
//! ## The process model, and why nothing is loaded eagerly
//!
//! zshrs loads every plugin once into a shell that then lives for hours. `git`
//! is a fresh process per command, so this host loads **nothing** until a verb
//! proves to belong to a plugin. The install index's derived side tables
//! ([`crate::pkg::store::VERBS_FILE`], [`crate::pkg::store::OVERRIDES_FILE`])
//! answer "who owns this verb" with one `stat` and, at most, one small read;
//! only then is a single library `dlopen`ed. A machine with no plugins
//! installed pays two failed `stat`s per command and nothing more.
//!
//! ## ABI safety
//!
//! Everything crossing the boundary is `#[repr(C)]`. The host verifies the
//! plugin's magic word and `abi_version` before trusting any pointer it returns;
//! a mismatch is refused, because a wrong struct layout is undefined behaviour.
//! The loaded [`libloading::Library`] is kept alive for the process lifetime —
//! its `Drop` is a `dlclose`, which would invalidate the still-registered
//! function pointers, so [`unload`] purges the registry first.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use znative::{HostApi, InitFn, ObjectBuf, PluginInfo, VerbFn, ABI_VERSION, INIT_SYMBOL, MAGIC};

use crate::pkg::store::{InstalledIndex, InstalledPlugin, Store};

/// One loaded plugin. Dropping `_lib` runs `dlclose`, so this is only ever
/// removed by [`unload`] AFTER its verbs are purged from [`registry`].
struct LoadedPlugin {
    name: String,
    version: String,
    path: String,
    /// Kept alive for the process lifetime; drop = `dlclose`.
    _lib: libloading::Library,
}

/// What one [`load`] registered — the installer records it in the index so a
/// later process can find the owning plugin without loading anything.
#[derive(Debug, Default, Clone)]
pub struct Loaded {
    /// The plugin's own name, from its [`PluginInfo`].
    pub name: String,
    /// The plugin's version string.
    pub version: String,
    /// Subcommands it added, sorted.
    pub verbs: Vec<String>,
    /// Existing verbs it replaced, sorted.
    pub overrides: Vec<String>,
}

fn plugins() -> &'static Mutex<Vec<LoadedPlugin>> {
    static P: OnceLock<Mutex<Vec<LoadedPlugin>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Vec::new()))
}

/// verb → handler. Consulted by [`dispatch`].
fn registry() -> &'static Mutex<HashMap<String, VerbFn>> {
    static R: OnceLock<Mutex<HashMap<String, VerbFn>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// overridden verb → handler. Consulted by [`dispatch_override`].
fn override_registry() -> &'static Mutex<HashMap<String, VerbFn>> {
    static O: OnceLock<Mutex<HashMap<String, VerbFn>>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Staging area for verbs registered during a single `init` call. `init` runs
/// before it returns the plugin name, so registrations are buffered here and
/// tagged with the owning plugin afterwards. Serialised by [`load_lock`].
fn staging() -> &'static Mutex<Vec<(String, VerbFn)>> {
    static S: OnceLock<Mutex<Vec<(String, VerbFn)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Staging for overrides registered during a single `init`. Serialised by
/// [`load_lock`].
fn override_staging() -> &'static Mutex<Vec<(String, VerbFn)>> {
    static OS: OnceLock<Mutex<Vec<(String, VerbFn)>>> = OnceLock::new();
    OS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Serialises `load`/`unload` so the staging buffers are single-writer.
fn load_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Which plugin owns each registered verb — parallel to [`registry`] and
/// [`override_registry`], used only for `unload` bookkeeping.
fn ownership() -> &'static Mutex<HashMap<String, String>> {
    static O: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
    /// Verbs whose override is currently delegating to the original. The
    /// override hook skips a verb on this stack, which is what makes
    /// `HostApi::dispatch_verb` reach the built-in implementation instead of
    /// re-entering the plugin that called it.
    static BYPASS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

// ============================================================
// Host API callbacks — the `extern "C"` functions plugins call back through.
// One shared, leaked `HostApi` table for the whole process.
// ============================================================

/// Decode a host-bound C string argument, or `None` when null.
fn arg(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Decode `argv[0..argc]` into owned `String`s.
fn argv_vec(argc: usize, argv: *const *const c_char) -> Vec<String> {
    let mut out = Vec::with_capacity(argc);
    if argv.is_null() {
        return out;
    }
    for i in 0..argc {
        let p = unsafe { *argv.add(i) };
        let Some(s) = arg(p) else { break };
        out.push(s);
    }
    out
}

/// Hand a `String` to the plugin as a C string it must release with
/// `free_cstring`; null when it cannot be represented.
fn out_string(s: String) -> *mut c_char {
    CString::new(s).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

extern "C" fn host_register_verb(_host: *const HostApi, name: *const c_char, handler: VerbFn) -> c_int {
    let Some(name) = arg(name).filter(|n| !n.is_empty()) else { return 1 };
    staging().lock().unwrap().push((name, handler));
    0
}

extern "C" fn host_register_override(
    _host: *const HostApi,
    name: *const c_char,
    handler: VerbFn,
) -> c_int {
    let Some(name) = arg(name).filter(|n| !n.is_empty()) else { return 1 };
    override_staging().lock().unwrap().push((name, handler));
    0
}

extern "C" fn host_dispatch_verb(
    _host: *const HostApi,
    name: *const c_char,
    argc: usize,
    argv: *const *const c_char,
) -> c_int {
    let Some(name) = arg(name) else { return 1 };
    let args = argv_vec(argc, argv);
    // argv[0] is the verb itself; dispatch takes the arguments after it.
    let rest: Vec<String> = args.into_iter().skip(1).collect();
    BYPASS.with(|b| b.borrow_mut().push(name.clone()));
    let rc = run_dispatch(&name, &rest);
    BYPASS.with(|b| {
        b.borrow_mut().pop();
    });
    rc
}

extern "C" fn host_run(_host: *const HostApi, argc: usize, argv: *const *const c_char) -> c_int {
    let args = argv_vec(argc, argv);
    let Some((verb, rest)) = args.split_first() else { return 1 };
    run_dispatch(verb, rest)
}

/// Run one subcommand in-process and return its numeric status.
fn run_dispatch(verb: &str, args: &[String]) -> c_int {
    match crate::dispatch::run(verb, args) {
        Ok(code) => crate::exit_status(code),
        Err(e) => {
            eprintln!("zvcs: {verb}: {e:#}");
            1
        }
    }
}

extern "C" fn host_print(_host: *const HostApi, text: *const c_char) {
    let Some(s) = arg(text) else { return };
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

extern "C" fn host_eprint(_host: *const HostApi, text: *const c_char) {
    let Some(s) = arg(text) else { return };
    use std::io::Write as _;
    let _ = std::io::stderr().write_all(s.as_bytes());
}

extern "C" fn host_config_get(_host: *const HostApi, name: *const c_char) -> *mut c_char {
    let Some(name) = arg(name) else { return std::ptr::null_mut() };
    let Ok(repo) = gix::discover(".") else { return std::ptr::null_mut() };
    // The same last-value-wins read the porcelain uses, so a plugin sees the
    // value `git config <name>` would print.
    match crate::config::last_value_with_origin(&repo, &name) {
        Some((value, _origin)) => out_string(value),
        None => std::ptr::null_mut(),
    }
}

extern "C" fn host_config_set(
    _host: *const HostApi,
    name: *const c_char,
    value: *const c_char,
) -> c_int {
    let (Some(name), Some(value)) = (arg(name), arg(value)) else { return 1 };
    // Routed through the `config` verb itself: key validation, scope selection
    // and the write path are then exactly the ones `git config` uses.
    run_dispatch("config", &[name, value])
}

extern "C" fn host_free_cstring(_host: *const HostApi, s: *mut c_char) {
    if !s.is_null() {
        // Reclaim ownership of a string we handed out via `into_raw`.
        unsafe { drop(CString::from_raw(s)) };
    }
}

extern "C" fn host_repo_info(_host: *const HostApi, field: *const c_char) -> *mut c_char {
    let Some(field) = arg(field) else { return std::ptr::null_mut() };
    let Ok(repo) = gix::discover(".") else { return std::ptr::null_mut() };
    let value = match field.as_str() {
        "gitdir" => Some(repo.git_dir().display().to_string()),
        "workdir" => repo.workdir().map(|p| p.display().to_string()),
        "head" => repo.head_id().ok().map(|id| id.to_string()),
        "branch" => repo
            .head_name()
            .ok()
            .flatten()
            .map(|n| n.shorten().to_string()),
        _ => None,
    };
    value.map(out_string).unwrap_or(std::ptr::null_mut())
}

extern "C" fn host_resolve_rev(_host: *const HostApi, spec: *const c_char) -> *mut c_char {
    let Some(spec) = arg(spec) else { return std::ptr::null_mut() };
    let Ok(repo) = gix::discover(".") else { return std::ptr::null_mut() };
    match repo.rev_parse_single(spec.as_str()) {
        Ok(id) => out_string(id.to_string()),
        Err(_) => std::ptr::null_mut(),
    }
}

extern "C" fn host_object_read(
    _host: *const HostApi,
    spec: *const c_char,
    out: *mut ObjectBuf,
) -> c_int {
    if out.is_null() {
        return 1;
    }
    let Some(spec) = arg(spec) else { return 1 };
    let Ok(repo) = gix::discover(".") else { return 1 };
    let Ok(id) = repo.rev_parse_single(spec.as_str()) else { return 1 };
    let Ok(object) = id.object() else { return 1 };
    let kind = match object.kind {
        gix::objs::Kind::Blob => znative::kind::BLOB,
        gix::objs::Kind::Tree => znative::kind::TREE,
        gix::objs::Kind::Commit => znative::kind::COMMIT,
        gix::objs::Kind::Tag => znative::kind::TAG,
    };
    // Hand the plugin the allocation itself; `free_buf` reconstructs the Vec
    // from (ptr, len, cap), so the host's allocator is the one that frees it.
    let mut data = object.data.clone();
    data.shrink_to_fit();
    let (ptr, len, cap) = (data.as_mut_ptr(), data.len(), data.capacity());
    std::mem::forget(data);
    unsafe { *out = ObjectBuf { kind, data: ptr, len, cap } };
    0
}

extern "C" fn host_object_write(
    _host: *const HostApi,
    kind: *const c_char,
    data: *const u8,
    len: usize,
) -> *mut c_char {
    let Some(kind) = arg(kind) else { return std::ptr::null_mut() };
    let Ok(kind) = gix::objs::Kind::from_bytes(kind.as_bytes()) else {
        return std::ptr::null_mut();
    };
    let bytes: &[u8] =
        if data.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, len) } };
    let Ok(repo) = gix::discover(".") else { return std::ptr::null_mut() };
    use gix::objs::Write as _;
    match repo.objects.write_buf(kind, bytes) {
        Ok(id) => out_string(id.to_string()),
        Err(_) => std::ptr::null_mut(),
    }
}

extern "C" fn host_free_buf(_host: *const HostApi, buf: *mut ObjectBuf) {
    if buf.is_null() {
        return;
    }
    let b = unsafe { &mut *buf };
    if !b.data.is_null() {
        // Reclaim the allocation `object_read` leaked into the plugin.
        unsafe { drop(Vec::from_raw_parts(b.data, b.len, b.cap)) };
    }
    *b = ObjectBuf::default();
}

/// The single process-wide host table. Leaked so its address is `'static` —
/// plugins may retain the `*const HostApi` and call through it from any handler.
fn host_api() -> *const HostApi {
    static API: OnceLock<usize> = OnceLock::new();
    let addr = API.get_or_init(|| {
        let boxed = Box::new(HostApi {
            magic: MAGIC,
            abi_version: ABI_VERSION,
            ctx: std::ptr::null_mut::<c_void>(),
            register_verb: host_register_verb,
            register_override: host_register_override,
            dispatch_verb: host_dispatch_verb,
            run: host_run,
            print: host_print,
            eprint: host_eprint,
            config_get: host_config_get,
            config_set: host_config_set,
            free_cstring: host_free_cstring,
            repo_info: host_repo_info,
            resolve_rev: host_resolve_rev,
            object_read: host_object_read,
            object_write: host_object_write,
            free_buf: host_free_buf,
        });
        Box::into_raw(boxed) as usize
    });
    *addr as *const HostApi
}

// ============================================================
// Loading
// ============================================================

/// Load a plugin `cdylib` from `path`, returning what it registered. Loading a
/// plugin whose name is already present is refused (unload first).
pub fn load(path: &str) -> Result<Loaded, String> {
    let _guard = load_lock().lock().unwrap();

    // `dlopen`. libloading resolves relative paths against the loader's search
    // rules; expand `~` since a stored path may carry one.
    let expanded = expand_tilde(path);
    let lib = unsafe { libloading::Library::new(&expanded) }
        .map_err(|e| format!("cannot load `{path}`: {e}"))?;

    // Resolve the mandatory init symbol.
    let init: libloading::Symbol<InitFn> = unsafe {
        lib.get(INIT_SYMBOL).map_err(|_| {
            format!(
                "`{path}`: not a zvcs plugin (no {})",
                String::from_utf8_lossy(&INIT_SYMBOL[..INIT_SYMBOL.len() - 1])
            )
        })?
    };

    // Clear staging, call init, collect what it registered.
    staging().lock().unwrap().clear();
    override_staging().lock().unwrap().clear();
    let info_ptr: *const PluginInfo = init(host_api());
    let discard = || {
        staging().lock().unwrap().clear();
        override_staging().lock().unwrap().clear();
    };
    if info_ptr.is_null() {
        discard();
        return Err(format!("`{path}`: plugin init failed (ABI mismatch or error)"));
    }
    let info = unsafe { &*info_ptr };
    if info.magic != MAGIC {
        discard();
        return Err(format!("`{path}`: not a zvcs plugin (bad magic)"));
    }
    if info.abi_version != ABI_VERSION {
        discard();
        return Err(format!(
            "`{path}`: ABI version {} != host {ABI_VERSION}",
            info.abi_version
        ));
    }
    let name = cstr_or(info.name, "unknown");
    let version = cstr_or(info.version, "?");

    // Refuse a duplicate name — the second load's verbs would shadow the first
    // with no clean unload story.
    if plugins().lock().unwrap().iter().any(|p| p.name == name) {
        discard();
        return Err(format!("plugin `{name}` already loaded"));
    }

    let staged: Vec<(String, VerbFn)> = std::mem::take(&mut *staging().lock().unwrap());
    let staged_over: Vec<(String, VerbFn)> =
        std::mem::take(&mut *override_staging().lock().unwrap());
    let mut loaded = Loaded {
        name: name.clone(),
        version: version.clone(),
        verbs: staged.iter().map(|(v, _)| v.clone()).collect(),
        overrides: staged_over.iter().map(|(v, _)| v.clone()).collect(),
    };
    loaded.verbs.sort();
    loaded.verbs.dedup();
    loaded.overrides.sort();
    loaded.overrides.dedup();

    // Commit staged registrations into the live registries, tagged with owner.
    {
        let mut reg = registry().lock().unwrap();
        let mut over = override_registry().lock().unwrap();
        let mut own = ownership().lock().unwrap();
        for (verb, func) in staged {
            reg.insert(verb.clone(), func);
            own.insert(verb, name.clone());
        }
        for (verb, func) in staged_over {
            over.insert(verb.clone(), func);
            own.insert(verb, name.clone());
        }
    }

    plugins().lock().unwrap().push(LoadedPlugin {
        name: name.clone(),
        version,
        path: expanded,
        _lib: lib,
    });
    Ok(loaded)
}

/// Unload a plugin by name: purge its registrations FIRST (so no live function
/// pointer survives), then drop the `Library` (`dlclose`).
pub fn unload(name: &str) -> Result<(), String> {
    let _guard = load_lock().lock().unwrap();

    if !plugins().lock().unwrap().iter().any(|p| p.name == name) {
        return Err(format!("plugin `{name}` not loaded"));
    }
    {
        let mut own = ownership().lock().unwrap();
        let mut reg = registry().lock().unwrap();
        let mut over = override_registry().lock().unwrap();
        let owned: Vec<String> =
            own.iter().filter(|(_, o)| o.as_str() == name).map(|(v, _)| v.clone()).collect();
        for verb in owned {
            reg.remove(&verb);
            over.remove(&verb);
            own.remove(&verb);
        }
    }
    // Now it is safe to dlclose.
    let mut ps = plugins().lock().unwrap();
    if let Some(pos) = ps.iter().position(|p| p.name == name) {
        let p = ps.remove(pos);
        drop(p); // explicit: dlclose here, after the registry purge.
    }
    Ok(())
}

/// `(name, version, path)` for each loaded plugin, sorted by name.
pub fn list() -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = plugins()
        .lock()
        .unwrap()
        .iter()
        .map(|p| (p.name.clone(), p.version.clone(), p.path.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Invoke a loaded plugin's handler for `verb`, or `None` when no plugin owns
/// it. `args` are the arguments after the verb.
pub fn dispatch(verb: &str, args: &[String]) -> Option<i32> {
    let func = *registry().lock().unwrap().get(verb)?;
    Some(invoke(func, verb, args))
}

/// Invoke a loaded plugin's OVERRIDE handler for `verb`, or `None`.
pub fn dispatch_override(verb: &str, args: &[String]) -> Option<i32> {
    let func = *override_registry().lock().unwrap().get(verb)?;
    Some(invoke(func, verb, args))
}

/// Call a plugin handler with `argv = [verb, args…]` as NUL-terminated C
/// strings, valid for the duration of the call.
fn invoke(func: VerbFn, verb: &str, args: &[String]) -> i32 {
    let mut owned: Vec<CString> = Vec::with_capacity(args.len() + 1);
    // Interior NULs cannot occur in a command line, but be defensive rather
    // than lose the argument entirely.
    let cstring = |s: &str| CString::new(s).unwrap_or_else(|_| CString::new(s.replace('\0', "")).unwrap_or_default());
    owned.push(cstring(verb));
    for a in args {
        owned.push(cstring(a));
    }
    let ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    let rc = func(host_api(), ptrs.len(), ptrs.as_ptr());
    // `owned`/`ptrs` outlive the call. Done.
    rc as i32
}

// ============================================================
// Lazy resolution — the per-process half.
// ============================================================

/// Look one verb up in a derived side table (`verbs.tsv` / `overrides.tsv`),
/// returning the owning plugin's name. The file's absence is the fast, common
/// answer: one failed `stat`, no open, no parse.
fn owner_of(table: &Path, verb: &str) -> Option<String> {
    let text = std::fs::read_to_string(table).ok()?;
    for line in text.lines() {
        let (v, plugin) = line.split_once('\t')?;
        if v == verb {
            return Some(plugin.to_string());
        }
    }
    None
}

/// The installed record for `name`, and the store directory it lives in.
fn installed(store: &Store, name: &str) -> Option<(InstalledPlugin, PathBuf)> {
    let index = InstalledIndex::load_from(store).ok()?;
    let entry = index.find(name)?.clone();
    let dir = store.package_dir(&entry.name, &entry.version);
    Some((entry, dir))
}

/// `dlopen` the plugin named `name` from the store. Returns its store directory.
fn load_installed(store: &Store, name: &str) -> Result<(InstalledPlugin, PathBuf), String> {
    let (entry, dir) =
        installed(store, name).ok_or_else(|| format!("plugin `{name}` is not installed"))?;
    if entry.kind == "native" {
        let lib = dir.join(&entry.lib);
        load(&lib.to_string_lossy())?;
    }
    Ok((entry, dir))
}

/// Dispatch hook for a verb that is neither a builtin nor an alias, consulted
/// BEFORE `git-<verb>` on PATH. Returns `Some` when a plugin owns the verb —
/// natively (the handler ran) or as a script plugin (the stored `git-<verb>`
/// was exec'd, which does not return).
pub fn try_verb(verb: &str, args: &[String]) -> Option<Result<ExitCode>> {
    let store = Store::user_default();
    let owner = owner_of(&store.verbs_file(), verb)?;
    let (entry, dir) = match load_installed(&store, &owner) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("zvcs: znative: {e}");
            return Some(Ok(ExitCode::FAILURE));
        }
    };
    if entry.kind == "script" {
        // A script plugin's verb is an executable in the store: exec it the way
        // `external::try_dashed` execs one from PATH, so it owns the terminal
        // and its status flows straight through.
        let exe = script_exe(&dir, &entry, verb)?;
        return Some(Ok(exec_script(&exe, args)));
    }
    let rc = dispatch(verb, args)?;
    Some(Ok(ExitCode::from(rc as u8)))
}

/// Dispatch hook for a plugin that REPLACES an existing verb. Consulted at the
/// top of [`crate::dispatch::run`]; returns `None` for the overwhelmingly
/// common case of no override table at all.
pub fn try_override(verb: &str, args: &[String]) -> Option<Result<ExitCode>> {
    // A verb whose override is delegating to the original must not re-enter it.
    if BYPASS.with(|b| b.borrow().iter().any(|v| v == verb)) {
        return None;
    }
    let store = Store::user_default();
    let owner = owner_of(&store.overrides_file(), verb)?;
    let (entry, _dir) = match load_installed(&store, &owner) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("zvcs: znative: {e}");
            return Some(Ok(ExitCode::FAILURE));
        }
    };
    if entry.kind != "native" {
        return None; // only a native plugin can override a verb.
    }
    let rc = dispatch_override(verb, args)?;
    Some(Ok(ExitCode::from(rc as u8)))
}

/// The `git-<verb>` executable a script plugin provides, searched in the
/// directories its index record names.
fn script_exe(dir: &Path, entry: &InstalledPlugin, verb: &str) -> Option<PathBuf> {
    let bins: Vec<String> = if entry.bin.is_empty() { vec![".".into()] } else { entry.bin.clone() };
    bins.iter().map(|b| dir.join(b).join(format!("git-{verb}"))).find(|p| p.is_file())
}

/// Run a script plugin's executable, replacing this process — the same `exec`
/// [`crate::external::try_dashed`] performs, for the same reasons (the child
/// owns the terminal, and its signals and status flow through unmediated).
fn exec_script(exe: &Path, args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(exe).args(args).exec();
    eprintln!("zvcs: cannot exec '{}': {err}", exe.display());
    ExitCode::FAILURE
}

fn cstr_or(p: *const c_char, dflt: &str) -> String {
    if p.is_null() {
        dflt.to_string()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest).to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "zvcs-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn owner_lookup_reads_the_side_table() {
        let dir = tmp();
        let table = dir.join("verbs.tsv");
        std::fs::write(&table, "hi\thello\nbye\tfarewell\n").unwrap();
        assert_eq!(owner_of(&table, "bye").as_deref(), Some("farewell"));
        assert_eq!(owner_of(&table, "nope"), None);
        // The absent-file case is the hot path, and must not error.
        assert_eq!(owner_of(&dir.join("missing.tsv"), "hi"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_plugin_library_is_refused_not_loaded() {
        // Any file without the init symbol: the loader must report it rather
        // than leave a half-registered plugin behind.
        let dir = tmp();
        let fake = dir.join("libnotaplugin.so");
        std::fs::write(&fake, b"not a shared object").unwrap();
        assert!(load(&fake.to_string_lossy()).is_err());
        assert!(list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_exe_searches_the_recorded_bin_dirs() {
        let dir = tmp();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/git-hi"), b"#!/bin/sh\n").unwrap();
        let entry = InstalledPlugin { bin: vec!["bin".into()], ..Default::default() };
        assert_eq!(script_exe(&dir, &entry, "hi"), Some(dir.join("bin/git-hi")));
        assert_eq!(script_exe(&dir, &entry, "nope"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
