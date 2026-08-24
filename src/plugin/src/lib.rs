//! # `znative` — native plugin SDK for zvcs
//!
//! zvcs is a compiled VCS that shadows `git`; this crate is what lets it host
//! third-party subcommands written in a **compiled** language and loaded at
//! runtime, instead of forking a `git-<verb>` script off PATH. A plugin is an
//! ordinary `cdylib` that the shadow binary `dlopen`s when a verb it owns is
//! typed.
//!
//! Ported from the zshrs SDK of the same name (`zshrs/znative/src/lib.rs`),
//! retargeted from shell state to repository state: `register_builtin` becomes
//! [`HostApi::register_verb`], `eval` becomes [`HostApi::run`], `getvar`/`setvar`
//! become [`HostApi::config_get`]/[`HostApi::config_set`], and the pair that
//! read and write host-internal state (`getfunction`/`addfunction`) becomes
//! [`HostApi::object_read`]/[`HostApi::object_write`].
//!
//! The boundary is a hand-rolled, versioned **C ABI** (`#[repr(C)]` structs +
//! `extern "C"` fn pointers). Both sides depend on THIS crate so they agree on
//! the exact layout. Nothing about Rust's unstable `repr(Rust)` layout,
//! allocator, or panic ABI crosses it — only C-representable data.
//!
//! ## Writing a plugin
//!
//! ```ignore
//! use znative::{declare_plugin, Args, Host};
//! use std::os::raw::c_int;
//!
//! fn hello(host: &Host, args: &Args) -> c_int {
//!     let head = host.repo_info("head").unwrap_or_else(|| "(unborn)".into());
//!     host.print(&format!("hello from {} at {head}\n", args.name()));
//!     0
//! }
//!
//! declare_plugin! {
//!     name: "hello",
//!     version: "0.1.0",
//!     verbs: { "hello" => hello },
//! }
//! ```
//!
//! `Cargo.toml`:
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//! [dependencies]
//! znative = { git = "https://github.com/MenkeTechnologies/zvcs" }
//! ```
//!
//! `cargo build --release` produces `libhello.dylib` / `libhello.so`; then
//! `git znative add path:.` installs it into the store and `git hello` is a
//! live subcommand.
//!
//! ## Host API
//!
//! Inside a handler, [`Host`] is the shadow binary's callback table:
//!
//! | Method | Purpose |
//! | --- | --- |
//! | [`print`](Host::print) / [`eprint`](Host::eprint) | write to stdout / stderr |
//! | [`run`](Host::run) | run a `git` subcommand in-process, return its status |
//! | [`dispatch_verb`](Host::dispatch_verb) | run a verb's ORIGINAL implementation (for an override to delegate to) |
//! | [`config_get`](Host::config_get) / [`config_set`](Host::config_set) | read / write a git config value |
//! | [`repo_info`](Host::repo_info) | `gitdir`, `workdir`, `head`, `branch` |
//! | [`resolve_rev`](Host::resolve_rev) | a revision spec → object id |
//! | [`object_read`](Host::object_read) / [`object_write`](Host::object_write) | read / write an object in the repository |
//! | [`register_verb`](Host::register_verb) / [`register_override`](Host::register_override) | add a subcommand / replace an existing one |

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

/// Magic word every [`HostApi`] and [`PluginInfo`] carries: ASCII `ZVCSPLUG`.
///
/// The version gate alone is not enough. A plugin built against a *different*
/// host with the same-named SDK (zshrs's `znative`) would present a struct of
/// the same size and a plausible `abi_version`; reading it through this layout
/// is undefined behaviour. The magic makes that a refusal instead.
pub const MAGIC: u64 = u64::from_be_bytes(*b"ZVCSPLUG");

/// ABI version. Bumped on ANY change to [`HostApi`], [`PluginInfo`],
/// [`ObjectBuf`], [`VerbFn`] or [`InitFn`] layout/semantics. The host refuses
/// to load a plugin whose `abi_version` does not match its own — a mismatched
/// struct layout is undefined behaviour, so this is a hard gate, not a warning.
pub const ABI_VERSION: u32 = 1;

/// The one symbol every plugin `cdylib` must export. The host resolves it with
/// `dlsym` after `dlopen`. Signature is [`InitFn`]. Deliberately NOT
/// `znative_init` (the zshrs SDK's symbol) so the two plugin worlds cannot
/// cross-load even before the magic word is read.
pub const INIT_SYMBOL: &[u8] = b"zvcs_native_init\0";

/// A plugin-provided subcommand handler.
///
/// * `host` — the host API table (call back into zvcs through it).
/// * `argc` — number of elements in `argv`.
/// * `argv` — NUL-terminated C strings; `argv[0]` is the verb name, `argv[1..]`
///   the arguments after it, exactly as `dispatch::run` receives them. Valid
///   only for the duration of the call; copy anything you need to keep.
///
/// Returns the command's exit status (0 = success), like any git subcommand.
pub type VerbFn =
    extern "C" fn(host: *const HostApi, argc: usize, argv: *const *const c_char) -> c_int;

/// Signature of [`INIT_SYMBOL`]. Called exactly once, right after the dylib is
/// loaded. The plugin registers its verbs through `host.register_verb` and
/// returns a pointer to a `'static` [`PluginInfo`] describing itself (or null
/// on failure).
pub type InitFn = extern "C" fn(host: *const HostApi) -> *const PluginInfo;

/// Object type tags used by [`HostApi::object_read`], in git's own numbering
/// order (`blob`, `tree`, `commit`, `tag`).
pub mod kind {
    /// A blob — file content.
    pub const BLOB: u32 = 1;
    /// A tree — one directory level.
    pub const TREE: u32 = 2;
    /// A commit.
    pub const COMMIT: u32 = 3;
    /// An annotated tag.
    pub const TAG: u32 = 4;
}

/// An object's decoded bytes, owned by the host. Filled in by
/// [`HostApi::object_read`] and released with [`HostApi::free_buf`]; a plugin
/// must not free `data` itself, because the host's allocator is the one that
/// produced it.
#[repr(C)]
pub struct ObjectBuf {
    /// One of the [`kind`] constants.
    pub kind: u32,
    /// The object's bytes — the decoded content, exactly as `git cat-file <type>`
    /// would print it. Null when `len` is 0.
    pub data: *mut u8,
    /// Length of `data`.
    pub len: usize,
    /// Allocation capacity — the host needs it to reconstruct its own
    /// allocation in [`HostApi::free_buf`]. Opaque to plugins.
    pub cap: usize,
}

impl Default for ObjectBuf {
    fn default() -> Self {
        ObjectBuf { kind: 0, data: std::ptr::null_mut(), len: 0, cap: 0 }
    }
}

/// The host API table handed to the plugin. Every field is a C-ABI function
/// pointer into zvcs. Layout is frozen by [`ABI_VERSION`].
///
/// A single instance lives for the whole process; plugins may store the
/// `*const HostApi` they are given and call through it from any handler.
#[repr(C)]
pub struct HostApi {
    /// Must equal [`MAGIC`]. Checked before anything else.
    pub magic: u64,
    /// Must equal [`ABI_VERSION`]. Checked by the plugin's own
    /// `declare_plugin!` glue before it trusts the rest of the table.
    pub abi_version: u32,
    /// Reserved for the host; opaque to plugins. Currently null.
    pub ctx: *mut c_void,

    /// Register a subcommand name → handler. Returns 0 on success. A name
    /// registered here resolves as `git <name>` after built-in verbs and
    /// aliases, before the `git-<name>` PATH lookup — the same slot git gives
    /// dashed externals. `name` is copied by the host.
    pub register_verb:
        extern "C" fn(host: *const HostApi, name: *const c_char, handler: VerbFn) -> c_int,
    /// Register a handler that REPLACES an existing verb (`blame`, `log`, a
    /// `z*` verb, …). The override is consulted at the top of dispatch, before
    /// the built-in implementation; `dispatch_verb` runs the original from
    /// inside it. Returns 0 on success. `name` is copied by the host.
    pub register_override:
        extern "C" fn(host: *const HostApi, name: *const c_char, handler: VerbFn) -> c_int,
    /// Run verb `name`'s ORIGINAL implementation with `argv[0..argc]` as its
    /// arguments (`argv[0]` is the verb name), bypassing every plugin override
    /// — including the caller's own, so an override can delegate without
    /// recursing. Returns the verb's exit status.
    pub dispatch_verb: extern "C" fn(
        host: *const HostApi,
        name: *const c_char,
        argc: usize,
        argv: *const *const c_char,
    ) -> c_int,
    /// Run a full `git` subcommand in-process — `argv[0]` is the verb,
    /// `argv[1..]` its arguments — and return its exit status. Plugin verbs and
    /// overrides are visible to it. No process is forked: this re-enters the
    /// same dispatch table the command line does.
    pub run: extern "C" fn(host: *const HostApi, argc: usize, argv: *const *const c_char) -> c_int,

    /// Write text to stdout (no trailing newline added).
    pub print: extern "C" fn(host: *const HostApi, text: *const c_char),
    /// Write text to stderr (no trailing newline added).
    pub eprint: extern "C" fn(host: *const HostApi, text: *const c_char),

    /// Read a git config value by full name (`user.email`, `zvcs.autostart`).
    /// Returns a freshly allocated C string the caller MUST release with
    /// `free_cstring`, or null if unset. Resolution is the command's own — the
    /// system → XDG → user → repo → worktree sequence, `-c` overrides included.
    pub config_get: extern "C" fn(host: *const HostApi, name: *const c_char) -> *mut c_char,
    /// Set a git config value in the repository-local file, like
    /// `git config <name> <value>`. Returns 0 on success.
    pub config_set:
        extern "C" fn(host: *const HostApi, name: *const c_char, value: *const c_char) -> c_int,
    /// Release a string previously returned by `config_get`, `repo_info`,
    /// `resolve_rev` or `object_write`.
    pub free_cstring: extern "C" fn(host: *const HostApi, s: *mut c_char),

    /// One field of the discovered repository: `gitdir`, `workdir`, `head` (the
    /// resolved commit id) or `branch` (the short name HEAD points at).
    /// Returns a freshly allocated C string to release with `free_cstring`, or
    /// null when there is no repository, no work tree, or an unborn HEAD.
    pub repo_info: extern "C" fn(host: *const HostApi, field: *const c_char) -> *mut c_char,
    /// Resolve a revision spec (`HEAD~2`, `v1.0^{commit}`, a short id) to a full
    /// object id in hex. Returns a freshly allocated C string to release with
    /// `free_cstring`, or null when it does not resolve.
    pub resolve_rev: extern "C" fn(host: *const HostApi, spec: *const c_char) -> *mut c_char,

    /// Read an object by revision spec or id into `out`. Returns 0 on success,
    /// non-zero when it does not resolve or cannot be decoded. On success the
    /// caller MUST release `out` with `free_buf`.
    pub object_read:
        extern "C" fn(host: *const HostApi, spec: *const c_char, out: *mut ObjectBuf) -> c_int,
    /// Write `len` bytes as a new object of type `kind` (`"blob"`, `"tree"`,
    /// `"commit"`, `"tag"`) into the repository's object database. Returns the
    /// new object's hex id, to release with `free_cstring`, or null on failure.
    pub object_write: extern "C" fn(
        host: *const HostApi,
        kind: *const c_char,
        data: *const u8,
        len: usize,
    ) -> *mut c_char,
    /// Release a buffer filled in by `object_read`.
    pub free_buf: extern "C" fn(host: *const HostApi, buf: *mut ObjectBuf),
}

/// What a plugin returns from its [`InitFn`]. The strings must have `'static`
/// lifetime (typically string literals via the `declare_plugin!` macro).
#[repr(C)]
pub struct PluginInfo {
    /// Must equal [`MAGIC`].
    pub magic: u64,
    /// Must equal [`ABI_VERSION`]. Redundant with the host-side check, but lets
    /// the host reject a plugin that lied about its ABI.
    pub abi_version: u32,
    /// Plugin name, NUL-terminated. The key the install index and
    /// `git znative remove` use.
    pub name: *const c_char,
    /// Plugin version, NUL-terminated. Informational.
    pub version: *const c_char,
}

// PluginInfo is only ever pointed at `'static` data; it carries no interior
// mutability. Marking it Sync lets the macro place it in a `static`.
unsafe impl Sync for PluginInfo {}

// ============================================================
// Ergonomic wrappers for plugin authors. None of this crosses the ABI; it is
// convenience over the raw pointers above.
// ============================================================

/// Safe wrapper over `*const HostApi` for use inside a handler. Cheap to
/// construct; borrows the host table.
pub struct Host {
    api: *const HostApi,
}

impl Host {
    /// Wrap a raw host pointer.
    ///
    /// # Safety
    /// `api` must be the non-null `*const HostApi` the host handed to the
    /// plugin (in `zvcs_native_init` or a [`VerbFn`] call) and must remain
    /// valid for the lifetime of this `Host`.
    pub unsafe fn from_raw(api: *const HostApi) -> Self {
        Host { api }
    }

    #[inline]
    fn t(&self) -> &HostApi {
        // Safe: constructed only from a valid host pointer.
        unsafe { &*self.api }
    }

    /// Take ownership of a host-allocated C string and release it through the
    /// host's own `free_cstring` — the allocator that produced it.
    fn take(&self, raw: *mut c_char) -> Option<String> {
        if raw.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned();
        (self.t().free_cstring)(self.api, raw);
        Some(s)
    }

    /// Register a subcommand handler by name. Usually done for you by
    /// `declare_plugin!`; exposed for dynamic registration.
    pub fn register_verb(&self, name: &str, handler: VerbFn) -> bool {
        let Ok(cname) = CString::new(name) else { return false };
        (self.t().register_verb)(self.api, cname.as_ptr(), handler) == 0
    }

    /// Register a handler that replaces an existing verb. Usually done for you
    /// by `declare_plugin!`'s `overrides:` section.
    pub fn register_override(&self, name: &str, handler: VerbFn) -> bool {
        let Ok(cname) = CString::new(name) else { return false };
        (self.t().register_override)(self.api, cname.as_ptr(), handler) == 0
    }

    /// Write `text` to stdout.
    pub fn print(&self, text: &str) {
        if let Ok(c) = CString::new(text) {
            (self.t().print)(self.api, c.as_ptr());
        }
    }

    /// Write `text` to stderr.
    pub fn eprint(&self, text: &str) {
        if let Ok(c) = CString::new(text) {
            (self.t().eprint)(self.api, c.as_ptr());
        }
    }

    /// Run `git <verb> <args…>` in-process; returns its exit status.
    pub fn run(&self, verb: &str, args: &[&str]) -> i32 {
        let Some((argv, _owned)) = argv_of(verb, args) else { return 1 };
        (self.t().run)(self.api, argv.len(), argv.as_ptr())
    }

    /// Run `verb`'s original implementation, bypassing plugin overrides — what
    /// an override calls to delegate. Returns its exit status.
    pub fn dispatch_verb(&self, verb: &str, args: &[&str]) -> i32 {
        let Ok(cname) = CString::new(verb) else { return 1 };
        let Some((argv, _owned)) = argv_of(verb, args) else { return 1 };
        (self.t().dispatch_verb)(self.api, cname.as_ptr(), argv.len(), argv.as_ptr())
    }

    /// Read git config value `name`, or `None` if unset.
    pub fn config_get(&self, name: &str) -> Option<String> {
        let cname = CString::new(name).ok()?;
        self.take((self.t().config_get)(self.api, cname.as_ptr()))
    }

    /// Set repository-local git config `name = value`. Returns true on success.
    pub fn config_set(&self, name: &str, value: &str) -> bool {
        let (Ok(cn), Ok(cv)) = (CString::new(name), CString::new(value)) else { return false };
        (self.t().config_set)(self.api, cn.as_ptr(), cv.as_ptr()) == 0
    }

    /// One field of the discovered repository: `gitdir`, `workdir`, `head`,
    /// `branch`.
    pub fn repo_info(&self, field: &str) -> Option<String> {
        let cf = CString::new(field).ok()?;
        self.take((self.t().repo_info)(self.api, cf.as_ptr()))
    }

    /// Resolve a revision spec to a full hex object id.
    pub fn resolve_rev(&self, spec: &str) -> Option<String> {
        let cs = CString::new(spec).ok()?;
        self.take((self.t().resolve_rev)(self.api, cs.as_ptr()))
    }

    /// Read an object by revision spec or id: `(kind, bytes)`, where `kind` is
    /// one of the [`kind`] constants. The bytes are copied out and the host's
    /// buffer released before returning.
    pub fn object_read(&self, spec: &str) -> Option<(u32, Vec<u8>)> {
        let cs = CString::new(spec).ok()?;
        let mut buf = ObjectBuf::default();
        if (self.t().object_read)(self.api, cs.as_ptr(), &mut buf) != 0 {
            return None;
        }
        let bytes = if buf.data.is_null() || buf.len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(buf.data, buf.len) }.to_vec()
        };
        let k = buf.kind;
        (self.t().free_buf)(self.api, &mut buf);
        Some((k, bytes))
    }

    /// Write `data` as a new object of type `kind` (`"blob"`, `"tree"`,
    /// `"commit"`, `"tag"`); returns its hex id.
    pub fn object_write(&self, kind: &str, data: &[u8]) -> Option<String> {
        let ck = CString::new(kind).ok()?;
        self.take((self.t().object_write)(self.api, ck.as_ptr(), data.as_ptr(), data.len()))
    }
}

/// Build the `argv` a host call needs: `[verb, args…]` as NUL-terminated C
/// strings plus the pointer array over them. The `CString`s are returned
/// alongside so they outlive the pointers.
fn argv_of(verb: &str, args: &[&str]) -> Option<(Vec<*const c_char>, Vec<CString>)> {
    let mut owned: Vec<CString> = Vec::with_capacity(args.len() + 1);
    owned.push(CString::new(verb).ok()?);
    for a in args {
        owned.push(CString::new(*a).ok()?);
    }
    let ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    Some((ptrs, owned))
}

/// Safe view over a handler's `(argc, argv)`. `argv[0]` is the verb name.
pub struct Args {
    items: Vec<String>,
}

impl Args {
    /// Decode a raw `(argc, argv)` pair into owned `String`s.
    ///
    /// # Safety
    /// `argv` must point to `argc` valid, NUL-terminated C strings, as
    /// guaranteed by the host when it invokes a [`VerbFn`].
    pub unsafe fn from_raw(argc: usize, argv: *const *const c_char) -> Self {
        let mut items = Vec::with_capacity(argc);
        if !argv.is_null() {
            for i in 0..argc {
                let p = *argv.add(i);
                if p.is_null() {
                    break;
                }
                items.push(CStr::from_ptr(p).to_string_lossy().into_owned());
            }
        }
        Args { items }
    }

    /// The verb name (`argv[0]`), or `""` if somehow empty.
    pub fn name(&self) -> &str {
        self.items.first().map(String::as_str).unwrap_or("")
    }

    /// The arguments after the verb.
    pub fn rest(&self) -> &[String] {
        if self.items.is_empty() {
            &[]
        } else {
            &self.items[1..]
        }
    }

    /// The arguments after the verb, as `&str` — the shape [`Host::run`] and
    /// [`Host::dispatch_verb`] take.
    pub fn rest_str(&self) -> Vec<&str> {
        self.rest().iter().map(String::as_str).collect()
    }

    /// All of `argv`, verb name included.
    pub fn to_vec(&self) -> &[String] {
        &self.items
    }
}

/// Declare a plugin: its identity, the subcommands it adds, and the existing
/// verbs it replaces. Expands to the `#[no_mangle] extern "C" fn
/// zvcs_native_init` the host looks for, plus the `'static` [`PluginInfo`].
///
/// * `verbs:` — each `"name" => handler` adds `git <name>`. A handler is
///   `fn(&Host, &Args) -> c_int`.
/// * `overrides:` — each `"verb" => handler` replaces an existing verb; the
///   handler calls [`Host::dispatch_verb`] to run the original.
///
/// Both sections are optional.
///
/// ```ignore
/// declare_plugin! {
///     name: "hello",
///     version: "0.1.0",
///     verbs:     { "hello" => hello },
///     overrides: { "blame" => my_blame },
/// }
/// ```
#[macro_export]
macro_rules! declare_plugin {
    (
        name: $name:literal,
        version: $version:literal,
        $(verbs: { $($verb:literal => $handler:path),+ $(,)? } $(,)?)?
        $(overrides: { $($over:literal => $ohandler:path),+ $(,)? } $(,)?)?
    ) => {
        static __ZVCS_PLUGIN_INFO: $crate::PluginInfo = $crate::PluginInfo {
            magic: $crate::MAGIC_FOR_MACRO,
            abi_version: $crate::ABIVERSION_FOR_MACRO,
            name: concat!($name, "\0").as_ptr() as *const ::std::os::raw::c_char,
            version: concat!($version, "\0").as_ptr() as *const ::std::os::raw::c_char,
        };

        #[no_mangle]
        pub extern "C" fn zvcs_native_init(
            host: *const $crate::HostApi,
        ) -> *const $crate::PluginInfo {
            if host.is_null() {
                return ::std::ptr::null();
            }
            // Verify the host is a zvcs host speaking our ABI before touching
            // any other field of the table.
            let (magic, ver) = unsafe { ((*host).magic, (*host).abi_version) };
            if magic != $crate::MAGIC || ver != $crate::ABI_VERSION {
                return ::std::ptr::null();
            }
            let h = unsafe { $crate::Host::from_raw(host) };
            $($(
                {
                    // One trampoline per registered handler: adapts the C-ABI
                    // VerbFn to the ergonomic fn(&Host,&Args).
                    extern "C" fn __verb(
                        host: *const $crate::HostApi,
                        argc: usize,
                        argv: *const *const ::std::os::raw::c_char,
                    ) -> ::std::os::raw::c_int {
                        let h = unsafe { $crate::Host::from_raw(host) };
                        let a = unsafe { $crate::Args::from_raw(argc, argv) };
                        $handler(&h, &a)
                    }
                    h.register_verb($verb, __verb);
                }
            )+)?
            $($(
                {
                    extern "C" fn __override(
                        host: *const $crate::HostApi,
                        argc: usize,
                        argv: *const *const ::std::os::raw::c_char,
                    ) -> ::std::os::raw::c_int {
                        let h = unsafe { $crate::Host::from_raw(host) };
                        let a = unsafe { $crate::Args::from_raw(argc, argv) };
                        $ohandler(&h, &a)
                    }
                    h.register_override($over, __override);
                }
            )+)?
            &__ZVCS_PLUGIN_INFO as *const $crate::PluginInfo
        }
    };
}

// The macro cannot name these inside a downstream crate's `const` initializer
// without an import; re-export under stable paths the macro hard-codes, so
// users need only the two names in the doc example.
#[doc(hidden)]
pub const ABIVERSION_FOR_MACRO: u32 = ABI_VERSION;
#[doc(hidden)]
pub const MAGIC_FOR_MACRO: u64 = MAGIC;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_the_ascii_word() {
        assert_eq!(MAGIC.to_be_bytes(), *b"ZVCSPLUG");
    }

    #[test]
    fn init_symbol_differs_from_the_shell_sdk() {
        // The whole cross-load guard: a zshrs plugin exports `znative_init`, so
        // resolving OUR symbol in it fails before any struct is read.
        assert_eq!(INIT_SYMBOL, b"zvcs_native_init\0");
        assert_ne!(INIT_SYMBOL, b"znative_init\0\0\0\0\0");
    }

    #[test]
    fn object_buf_starts_empty_and_null() {
        let b = ObjectBuf::default();
        assert!(b.data.is_null());
        assert_eq!((b.kind, b.len, b.cap), (0, 0, 0));
    }
}
