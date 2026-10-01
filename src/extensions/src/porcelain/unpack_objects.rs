//! `git unpack-objects` — read a pack stream from stdin and explode it into
//! loose objects in the current repository.
//!
//! A port of `builtin/unpack-objects.c` in its own shape: the pack is read
//! through the same `fill()`/`use()` pair over a `DEFAULT_IO_BUFFER_SIZE`
//! buffer, hashed as it is used, and each object is decoded and written the
//! moment it has been inflated (`unpack_one()`). That is what decides every
//! observable edge git has:
//!
//!   * a stream that runs dry dies `early EOF` wherever it happens, with every
//!     object before it already written;
//!   * `get_data()` reports a zlib failure as `git_inflate()`'s `inflate: …`
//!     line plus `inflate returned <n>` and exits 1, or under `-r` carries on
//!     from wherever the stream stopped, so later bytes can read as `bad
//!     object type <n>` before the trailer check;
//!   * deltas whose base is not known yet wait on `delta_list` and are resolved
//!     by `added_object()` as bases arrive; one left over is `unresolved deltas
//!     left after unpacking`, and a base neither in the pack nor the object
//!     database is `failed to read delta-pack base object <oid>`;
//!   * the trailer is checked last (`final sha1 did not match`), and whatever
//!     the last read brought in after it is copied to stdout;
//!   * `-n` inflates and checks everything and writes nothing;
//!   * `--strict` writes blobs as they come and holds commits, trees and tags
//!     in core until `write_rest()`, where `check_object()` runs the fsck
//!     message layer (`fsck error in packed object`), walks the links (`object
//!     of unexpected type` for one that is nowhere, `Error on reachable
//!     objects of <oid>`), and only then writes the object; `fsck_finish()`
//!     checks the `.gitmodules`/`.gitattributes` blobs;
//!   * `--pack_header=<version>,<objects>` pre-fills the buffer with the header
//!     the caller consumed, `--max-input-size=<n>` bounds the bytes used, and
//!     `Unpacking objects` progress is drawn unless `-q` or stderr is not a
//!     terminal (`quiet = !isatty(2)`).
//!
//! The argument loop mirrors git's exactly: flags are tested in git's order
//! and anything else — an unknown flag or any positional — is the usage line.
//!
//! Known differences: zlib's own diagnostic text comes from `zlib-rs`, which
//! reports `repeated call with bad state` where C zlib's fast path says
//! `invalid distance too far back` (and may stop consuming input at a
//! different byte), and `--strict=<id>=<severity>` is validated but its
//! severities are not applied: findings use the strict defaults, as
//! `index-pack --strict` does.

use anyhow::Result;
use std::collections::HashSet;
use std::io::{self, Read};
use std::process::ExitCode;

use gix::objs::Write as _;

/// The usage line stock `git unpack-objects` prints, verbatim.
const USAGE: &str = "usage: git unpack-objects [-n] [-q] [-r] [--strict]";

/// Every fsck message id `--strict=<id>=<severity>` accepts, in the form git
/// compares against: `fsck_set_msg_types()` lowercases the whole spec, and the
/// ids it matches are the `FOREACH_FSCK_MSG_ID` names with their underscores
/// removed. So `MISSING_EMAIL` is spelled `missingemail` here, and the
/// underscore form `missing_email` is rejected by git as an unknown id.
const FSCK_MSG_IDS: &[&str] = &[
    "nulinheader",
    "unterminatedheader",
    "badheadercontinuation",
    "baddate",
    "baddateoverflow",
    "bademail",
    "badgpgsig",
    "badheadtarget",
    "badname",
    "badobjectsha1",
    "badpackedrefentry",
    "badpackedrefheader",
    "badparentsha1",
    "badreferentname",
    "badrefcontent",
    "badreffiletype",
    "badrefname",
    "badrefoid",
    "badtimezone",
    "badtree",
    "badtreesha1",
    "badtype",
    "duplicateentries",
    "gitattributesblob",
    "gitattributeslarge",
    "gitattributeslinelength",
    "gitattributesmissing",
    "gitmodulesblob",
    "gitmoduleslarge",
    "gitmodulesmissing",
    "gitmodulesname",
    "gitmodulespath",
    "gitmodulessymlink",
    "gitmodulesupdate",
    "gitmodulesurl",
    "missingauthor",
    "missingcommitter",
    "missingemail",
    "missingnamebeforeemail",
    "missingobject",
    "missingspacebeforedate",
    "missingspacebeforeemail",
    "missingtag",
    "missingtagentry",
    "missingtree",
    "missingtype",
    "missingtypeentry",
    "multipleauthors",
    "packedrefentrynotterminated",
    "packedrefunsorted",
    "treenotsorted",
    "unknowntype",
    "zeropaddeddate",
    "badreftabletablename",
    "emptyname",
    "fullpathname",
    "hasdot",
    "hasdotdot",
    "hasdotgit",
    "largepathname",
    "nullsha1",
    "nulincommit",
    "zeropaddedfilemode",
    "badfilemode",
    "badtagname",
    "emptypackedrefsfile",
    "gitattributessymlink",
    "gitignoresymlink",
    "gitmodulesparse",
    "mailmapsymlink",
    "missingtaggerentry",
    "refmissingnewline",
    "symlinkref",
    "symreftargetisnotaref",
    "trailingrefcontent",
    "extraheaderentry",
];

/// The severities `--strict=<id>=<severity>` accepts. git's internal table also
/// carries `fatal` and `info`, but neither is settable from the command line —
/// both are answered with `Unknown fsck message type`.
const FSCK_SEVERITIES: &[&str] = &["error", "warn", "ignore"];

/// `git unpack-objects` — explode a pack read from stdin into loose objects.
///
/// See the module docs for the supported flag set and the documented gaps.
pub fn unpack_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands over the arguments after the subcommand; tolerate a
    // leading `unpack-objects` in case the caller passes argv unsliced. The
    // token is never a legal argument here (git answers any positional with
    // the usage line), so dropping it costs no fidelity.
    let args = match args.split_first() {
        Some((first, rest)) if first == "unpack-objects" => rest,
        _ => args,
    };

    // `git.c` intercepts a lone `-h` before the builtin ever runs and prints the
    // usage line on stdout. It is not part of the builtin's own flag loop, so
    // `-h` alongside anything else is just an unknown argument and lands on
    // stderr through the catch-all below. `--help-all` is the same sole-argument
    // request for `USAGE_FULL`, which here renders the same single line.
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help-all") {
        return Ok(super::show_usage(&format!("{USAGE}\n")));
    }

    let mut dry_run = false;
    let mut recover = false;
    let mut strict = false;
    let mut max_input_size: u64 = 0; // git: 0 means "unlimited"
    let mut pack_header: Option<[u8; 12]> = None;
    // `quiet = !isatty(2);` (builtin/unpack-objects.c:626), before `-q`.
    let mut quiet = !std::io::IsTerminal::is_terminal(&io::stderr());

    // git's own order, arm for arm. Anything that falls off the end is a usage
    // error, whether it started with a dash or not.
    for a in args {
        let a = a.as_str();
        match a {
            "-n" => dry_run = true,
            "-q" => quiet = true,
            "-r" => recover = true,
            "--strict" => strict = true,
            _ if a.starts_with("--strict=") => {
                strict = true;
                // git validates the spec while parsing, before it reads a byte
                // of the pack, and dies at 128 on a bad one.
                if let Err(msg) = check_fsck_msg_types(&a["--strict=".len()..]) {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
            _ if a.starts_with("--pack_header=") => {
                let value = &a["--pack_header=".len()..];
                let Some(hdr) = parse_pack_header_option(value) else {
                    eprintln!("fatal: bad --pack_header: {value}");
                    return Ok(ExitCode::from(128));
                };
                // The version is `unpack_all()`'s to check, as it checks a
                // header read from stdin.
                pack_header = Some(hdr);
            }
            _ if a.starts_with("--max-input-size=") => {
                max_input_size = parse_magnitude(&a["--max-input-size=".len()..]);
            }
            // Any other flag, and any positional, is a usage error for git.
            _ => {
                eprintln!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let Ok(repo) = crate::setup::discover() else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    let mut unpack = Unpack::new(&repo, dry_run, quiet, recover, strict, max_input_size);
    if let Some(header) = pack_header {
        // `parse_pack_header_option(arg, buffer, &len)`: the header the caller
        // already consumed goes into the input buffer, ahead of stdin's bytes.
        unpack.buffer[..header.len()].copy_from_slice(&header);
        unpack.len = header.len();
    }
    Ok(match unpack.run() {
        Ok(code) => code,
        Err(Stop::Die(message)) => {
            eprintln!("fatal: {message}");
            ExitCode::from(128)
        }
        Err(Stop::Exit(code)) => ExitCode::from(code),
    })
}

/// How a run ends early: `die()` (`fatal: <message>`, 128) or a plain
/// `exit(<code>)` after an `error()` line already printed.
enum Stop {
    Die(String),
    Exit(u8),
}

type Step<T> = std::result::Result<T, Stop>;

fn die<T>(message: impl Into<String>) -> Step<T> {
    Err(Stop::Die(message.into()))
}

/// `DEFAULT_IO_BUFFER_SIZE` (git-compat-util.h:737), the size of the static
/// input buffer `fill()` reads into.
const DEFAULT_IO_BUFFER_SIZE: usize = 128 * 1024;

/// `struct obj_info` (builtin/unpack-objects.c:196-200): where each object
/// started, and its id once known — `None` for a delta still waiting on its
/// base, which `oidclr()` leaves null.
struct ObjInfo {
    offset: u64,
    oid: Option<gix::ObjectId>,
    /// `obj_list[nr].obj`: a commit, tree or tag `--strict` holds in core.
    held: bool,
}

/// `struct delta_info` (builtin/unpack-objects.c:160-167).
struct DeltaInfo {
    base_oid: Option<gix::ObjectId>,
    base_offset: u64,
    nr: usize,
    delta: Vec<u8>,
}

/// The state `builtin/unpack-objects.c` keeps in file-scope statics.
struct Unpack<'r> {
    repo: &'r gix::Repository,
    hash: gix::hash::Kind,
    /// `buffer`, `offset`, `len`: the bytes read from stdin and not yet used.
    buffer: Vec<u8>,
    offset: usize,
    len: usize,
    consumed_bytes: u64,
    max_input_size: u64,
    /// `ctx`: every byte handed to `use()` so far.
    hasher: gix::hash::Hasher,
    dry_run: bool,
    quiet: bool,
    recover: bool,
    strict: bool,
    has_errors: bool,
    big_file_threshold: u64,
    progress: Option<crate::progress::Meter>,
    obj_list: Vec<ObjInfo>,
    /// `delta_list`, head first: `add_delta_to_list()` prepends.
    delta_list: Vec<DeltaInfo>,
    /// `--strict`'s in-core objects: `obj_decorate`'s buffers (`FLAG_OPEN`),
    /// `FLAG_WRITTEN`, and every object `lookup_<type>()` has created, by type.
    buffers: std::collections::HashMap<gix::ObjectId, (gix::object::Kind, Vec<u8>)>,
    written: HashSet<gix::ObjectId>,
    known: std::collections::HashMap<gix::ObjectId, gix::object::Kind>,
    /// `.gitmodules` / `.gitattributes` blobs `fsck_object()` queued for
    /// `fsck_finish()`.
    finish_blobs: Vec<(gix::ObjectId, bool, bool)>,
}

impl<'r> Unpack<'r> {
    fn new(
        repo: &'r gix::Repository,
        dry_run: bool,
        quiet: bool,
        recover: bool,
        strict: bool,
        max_input_size: u64,
    ) -> Self {
        let hash = repo.object_hash();
        Unpack {
            repo,
            hash,
            buffer: vec![0u8; DEFAULT_IO_BUFFER_SIZE],
            offset: 0,
            len: 0,
            consumed_bytes: 0,
            max_input_size,
            hasher: gix::hash::hasher(hash),
            dry_run,
            quiet,
            recover,
            strict,
            has_errors: false,
            big_file_threshold: super::fsck::big_file_threshold(repo),
            progress: None,
            obj_list: Vec::new(),
            delta_list: Vec::new(),
            buffers: Default::default(),
            written: Default::default(),
            known: Default::default(),
            finish_blobs: Vec::new(),
        }
    }

    /// `fill()` (builtin/unpack-objects.c:69-89): make at least `min` bytes
    /// available at `buffer[offset..]`.
    fn fill(&mut self, min: usize) -> Step<()> {
        if min <= self.len {
            return Ok(());
        }
        if min > self.buffer.len() {
            return die(format!("cannot fill {min} bytes"));
        }
        if self.offset != 0 {
            self.buffer.copy_within(self.offset..self.offset + self.len, 0);
            self.offset = 0;
        }
        loop {
            let got = {
                // fd 0 itself: the standard library's `Stdin` would buffer ahead.
                let mut stdin = std::mem::ManuallyDrop::new(unsafe {
                    <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(0)
                });
                stdin.read(&mut self.buffer[self.len..])
            };
            match got {
                Ok(0) => return die("early EOF"),
                Ok(n) => self.len += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return die(format!("read error on input: {}", crate::external::strerror(&e))),
            }
            if self.len >= min {
                return Ok(());
            }
        }
    }

    /// `use()` (builtin/unpack-objects.c:91-104). The bytes are hashed here
    /// rather than when `fill()` moves them, which feeds the same bytes to
    /// `ctx` in the same order.
    fn consume(&mut self, bytes: usize) -> Step<()> {
        if bytes > self.len {
            return die("used more bytes than were available");
        }
        self.hasher.update(&self.buffer[self.offset..self.offset + bytes]);
        self.len -= bytes;
        self.offset += bytes;
        self.consumed_bytes += bytes as u64;
        if self.max_input_size != 0 && self.consumed_bytes > self.max_input_size {
            return die("pack exceeds maximum allowed size");
        }
        if let Some(meter) = self.progress.as_mut() {
            meter.throughput(self.consumed_bytes);
        }
        Ok(())
    }

    /// `fill(1)` then one byte of it, `use(1)`.
    fn next_byte(&mut self) -> Step<u8> {
        self.fill(1)?;
        let byte = self.buffer[self.offset];
        self.consume(1)?;
        Ok(byte)
    }

    /// `get_data()` (builtin/unpack-objects.c:106-150): inflate `size` bytes
    /// off the stream. `None` is its `NULL`: a dry run, which only checks the
    /// stream, or an inflate failure `-r` let through.
    fn get_data(&mut self, size: usize) -> Step<Option<Vec<u8>>> {
        let bufsize = if self.dry_run && size > 8192 { 8192 } else { size };
        let mut out = vec![0u8; bufsize];
        let mut z = gix::zlib::Decompress::new();
        self.fill(1)?;
        loop {
            let before_in = z.total_in();
            let before_out = z.total_out() as usize;
            // Non-dry runs inflate into the whole object; a dry run reuses its
            // scratch buffer, capped at what is still to come.
            let window = if self.dry_run {
                let left = size - before_out;
                &mut out[..bufsize.min(left)]
            } else {
                &mut out[before_out..]
            };
            let input = &self.buffer[self.offset..self.offset + self.len];
            let status = z.decompress(input, window, gix::zlib::FlushDecompress::None);
            let used = (z.total_in() - before_in) as usize;
            self.consume(used)?;
            // `git_inflate()`'s return, as zlib numbers it.
            let ret = match status {
                Ok(gix::zlib::Status::StreamEnd) if z.total_out() as usize == size => break,
                Ok(gix::zlib::Status::Ok) => 0,
                Ok(gix::zlib::Status::StreamEnd) => 1,
                Ok(gix::zlib::Status::BufError) => -5,
                Err(e) => {
                    if matches!(e, gix::zlib::DecompressError::InsufficientMemory) {
                        return die("inflate: out of memory");
                    }
                    eprintln!("{}", super::fsck::inflate_error_line(&z, &e));
                    zlib_code(&e)
                }
            };
            if ret != 0 {
                eprintln!("error: inflate returned {ret}");
                if !self.recover {
                    return Err(Stop::Exit(1));
                }
                self.has_errors = true;
                return Ok(None);
            }
            self.fill(1)?;
        }
        if self.dry_run {
            return Ok(None);
        }
        Ok(Some(out))
    }

    /// `odb_write_object()`: the object's id, written loose unless the
    /// database already has it.
    fn write_odb(&self, kind: gix::object::Kind, data: &[u8]) -> Step<gix::ObjectId> {
        let id = gix::objs::compute_hash(self.hash, kind, data)
            .map_err(|e| Stop::Die(format!("failed to write object: {e}")))?;
        if let Err(e) = self.repo.write_buf_with_known_id(kind, data, id) {
            return die(format!("failed to write object: {e}"));
        }
        Ok(id)
    }

    /// `write_object()` (builtin/unpack-objects.c:285-326).
    fn write_object(&mut self, nr: usize, kind: gix::object::Kind, data: Vec<u8>) -> Step<()> {
        if !self.strict {
            let id = self.write_odb(kind, &data)?;
            self.obj_list[nr].oid = Some(id);
            self.added_object(nr, kind, &data)?;
            self.obj_list[nr].held = false;
        } else if kind == gix::object::Kind::Blob {
            let id = self.write_odb(kind, &data)?;
            self.obj_list[nr].oid = Some(id);
            self.added_object(nr, kind, &data)?;
            // `lookup_blob()`, dying `invalid blob object` on a clash, then
            // `FLAG_WRITTEN`.
            if self.lookup(id, kind).is_err() {
                return die("invalid blob object");
            }
            self.written.insert(id);
            self.obj_list[nr].held = false;
        } else {
            let id = gix::objs::compute_hash(self.hash, kind, &data)
                .map_err(|e| Stop::Die(format!("invalid {kind}: {e}")))?;
            self.obj_list[nr].oid = Some(id);
            self.added_object(nr, kind, &data)?;
            if self.lookup(id, kind).is_err()
                || !super::index_pack::parse_object_buffer(kind, &data, &id, self.hash.len_in_hex())
            {
                return die(format!("invalid {kind}"));
            }
            // `add_object_buffer()`
            if self.buffers.contains_key(&id) {
                return die(format!("object {id} tried to add buffer twice!"));
            }
            self.buffers.insert(id, (kind, data));
            self.obj_list[nr].held = true;
        }
        Ok(())
    }

    /// `lookup_<type>()`: create the in-core object, or find it with the type
    /// it already has. A clash is `object_as_type()`'s error and a `NULL`.
    fn lookup(&mut self, id: gix::ObjectId, kind: gix::object::Kind) -> std::result::Result<(), ()> {
        match self.known.get(&id) {
            Some(&existing) if existing != kind => {
                eprintln!("error: object {id} is a {existing}, not a {kind}");
                Err(())
            }
            Some(_) => Ok(()),
            None => {
                self.known.insert(id, kind);
                Ok(())
            }
        }
    }

    /// `resolve_delta()` (builtin/unpack-objects.c:328-343).
    fn resolve_delta(
        &mut self,
        nr: usize,
        kind: gix::object::Kind,
        base: &[u8],
        delta: &[u8],
    ) -> Step<()> {
        let result = match super::pack_check::patch_delta(base, delta) {
            Ok(result) => result,
            Err(line) => {
                if let Some(line) = line {
                    eprintln!("error: {line}");
                }
                return die("failed to apply delta");
            }
        };
        self.write_object(nr, kind, result)
    }

    /// `added_object()` (builtin/unpack-objects.c:345-368): resolve every
    /// queued delta whose base is the `nr`-th object, now that it is known.
    fn added_object(&mut self, nr: usize, kind: gix::object::Kind, data: &[u8]) -> Step<()> {
        let (oid, offset) = (self.obj_list[nr].oid, self.obj_list[nr].offset);
        while let Some(i) = self
            .delta_list
            .iter()
            .position(|info| (info.base_oid.is_some() && info.base_oid == oid) || info.base_offset == offset)
        {
            let info = self.delta_list.remove(i);
            self.resolve_delta(info.nr, kind, data, &info.delta)?;
        }
        Ok(())
    }

    /// `resolve_against_held()` (builtin/unpack-objects.c:430-444).
    fn resolve_against_held(&mut self, nr: usize, base: &gix::ObjectId, delta: &[u8]) -> Step<bool> {
        let Some((kind, data)) = self.buffers.get(base).map(|(k, d)| (*k, d.clone())) else {
            return Ok(false);
        };
        self.resolve_delta(nr, kind, &data, delta)?;
        Ok(true)
    }

    /// `stream_blob()` (builtin/unpack-objects.c:398-428): a blob over
    /// `core.bigFileThreshold`, written as it inflates. Unlike every other
    /// object it never reaches `added_object()`.
    fn stream_blob(&mut self, size: usize, nr: usize) -> Step<()> {
        let mut data = Vec::with_capacity(size);
        let mut chunk = vec![0u8; 16 * 1024];
        let mut z = gix::zlib::Decompress::new();
        let status = loop {
            self.fill(1)?;
            let before_in = z.total_in();
            let before_out = z.total_out();
            let input = &self.buffer[self.offset..self.offset + self.len];
            let status = z.decompress(input, &mut chunk, gix::zlib::FlushDecompress::None);
            let used = (z.total_in() - before_in) as usize;
            let produced = (z.total_out() - before_out) as usize;
            data.extend_from_slice(&chunk[..produced]);
            self.consume(used)?;
            match status {
                Ok(gix::zlib::Status::Ok) => continue,
                Ok(gix::zlib::Status::StreamEnd) => break 1,
                Ok(gix::zlib::Status::BufError) => break -5,
                Err(e) => {
                    eprintln!("{}", super::fsck::inflate_error_line(&z, &e));
                    break zlib_code(&e);
                }
            }
        };
        if data.len() != size {
            return die("failed to write object in stream");
        }
        let id = self.write_odb(gix::object::Kind::Blob, &data)?;
        if status != 1 {
            return die(format!("inflate returned ({status})"));
        }
        self.obj_list[nr].oid = Some(id);
        if self.strict {
            if self.lookup(id, gix::object::Kind::Blob).is_err() {
                return die("invalid blob object from stream");
            }
            self.written.insert(id);
        }
        self.obj_list[nr].held = false;
        Ok(())
    }

    /// `unpack_delta_entry()` (builtin/unpack-objects.c:446-543).
    fn unpack_delta_entry(&mut self, kind: i32, delta_size: usize, nr: usize) -> Step<()> {
        let base_oid;
        if kind == OBJ_REF_DELTA {
            let rawsz = self.hash.len_in_bytes();
            self.fill(rawsz)?;
            let oid = gix::ObjectId::from_bytes_or_panic(&self.buffer[self.offset..self.offset + rawsz]);
            self.consume(rawsz)?;
            let Some(delta) = self.get_data(delta_size)? else {
                return Ok(());
            };
            if self.repo.has_object(oid) {
                // "Ok we have this one"
            } else if self.resolve_against_held(nr, &oid, &delta)? {
                return Ok(());
            } else {
                // "cannot resolve yet --- queue it"
                self.obj_list[nr].oid = None;
                self.add_delta_to_list(nr, Some(oid), 0, delta);
                return Ok(());
            }
            base_oid = oid;
            return self.resolve_from(nr, base_oid, delta);
        }

        let mut c = self.next_byte()?;
        let mut base_offset = u64::from(c & 127);
        while c & 128 != 0 {
            base_offset += 1;
            if base_offset == 0 || base_offset >> (64 - 7) != 0 {
                return die("offset value overflow for delta base object");
            }
            c = self.next_byte()?;
            base_offset = (base_offset << 7) + u64::from(c & 127);
        }
        let own_offset = self.obj_list[nr].offset;
        if base_offset == 0 || base_offset >= own_offset {
            return die("offset value out of bound for delta base object");
        }
        let base_offset = own_offset - base_offset;

        let Some(delta) = self.get_data(delta_size)? else {
            return Ok(());
        };
        let found = self.obj_list[..nr]
            .binary_search_by(|info| info.offset.cmp(&base_offset))
            .ok()
            .and_then(|i| self.obj_list[i].oid);
        let Some(oid) = found else {
            // "The delta base object is itself a delta that has not been
            // resolved yet."
            self.obj_list[nr].oid = None;
            self.add_delta_to_list(nr, None, base_offset, delta);
            return Ok(());
        };
        self.resolve_from(nr, oid, delta)
    }

    /// The tail both delta kinds share: an in-core base under `--strict`, or
    /// else the object database.
    fn resolve_from(&mut self, nr: usize, base_oid: gix::ObjectId, delta: Vec<u8>) -> Step<()> {
        if self.resolve_against_held(nr, &base_oid, &delta)? {
            return Ok(());
        }
        let Ok(base) = self.repo.find_object(base_oid) else {
            eprintln!("error: failed to read delta-pack base object {base_oid}");
            if !self.recover {
                return Err(Stop::Exit(1));
            }
            self.has_errors = true;
            return Ok(());
        };
        let (kind, data) = (base.kind, base.data.clone());
        self.resolve_delta(nr, kind, &data, &delta)
    }

    /// `add_delta_to_list()`: prepended, as git's linked list is.
    fn add_delta_to_list(&mut self, nr: usize, base_oid: Option<gix::ObjectId>, base_offset: u64, delta: Vec<u8>) {
        self.delta_list.insert(0, DeltaInfo { base_oid, base_offset, nr, delta });
    }

    /// `unpack_one()` (builtin/unpack-objects.c:545-593).
    fn unpack_one(&mut self, nr: usize) -> Step<()> {
        self.obj_list[nr].offset = self.consumed_bytes;
        let mut c = self.next_byte()? as usize;
        let kind = ((c >> 4) & 7) as i32;
        let mut size = c & 15;
        let mut shift = 4u32;
        while c & 0x80 != 0 {
            if usize::BITS - 7 < shift {
                return die("object size too large for this platform");
            }
            c = self.next_byte()? as usize;
            size = size.wrapping_add((c & 0x7f) << shift);
            shift += 7;
        }
        let object_kind = match kind {
            1 => gix::object::Kind::Commit,
            2 => gix::object::Kind::Tree,
            3 => gix::object::Kind::Blob,
            4 => gix::object::Kind::Tag,
            OBJ_OFS_DELTA | OBJ_REF_DELTA => return self.unpack_delta_entry(kind, size, nr),
            _ => {
                eprintln!("error: bad object type {kind}");
                self.has_errors = true;
                if self.recover {
                    return Ok(());
                }
                return Err(Stop::Exit(1));
            }
        };
        if object_kind == gix::object::Kind::Blob && !self.dry_run && size as u64 > self.big_file_threshold {
            return self.stream_blob(size, nr);
        }
        // `unpack_non_delta_entry()`
        if let Some(data) = self.get_data(size)? {
            self.write_object(nr, object_kind, data)?;
        }
        Ok(())
    }

    /// `unpack_all()` (builtin/unpack-objects.c:595-626).
    fn unpack_all(&mut self) -> Step<()> {
        self.fill(12)?;
        let hdr = &self.buffer[self.offset..self.offset + 12];
        if &hdr[..4] != b"PACK" {
            return die("bad pack file");
        }
        let version = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        if version != 2 && version != 3 {
            return die(format!("unknown pack file version {version}"));
        }
        let nr_objects = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
        self.consume(12)?;

        if !self.quiet {
            self.progress = Some(crate::progress::Meter::counted("Unpacking objects", nr_objects, true));
        }
        self.obj_list = (0..nr_objects).map(|_| ObjInfo { offset: 0, oid: None, held: false }).collect();
        for nr in 0..nr_objects {
            self.unpack_one(nr)?;
            if let Some(meter) = self.progress.as_mut() {
                meter.set(nr + 1);
            }
        }
        if let Some(meter) = self.progress.take() {
            meter.stop("done");
        }
        if !self.delta_list.is_empty() {
            return die("unresolved deltas left after unpacking");
        }
        Ok(())
    }

    /// `check_object()` (builtin/unpack-objects.c:216-262), reached through
    /// `write_rest()` and `fsck_walk()`. `Ok(false)` is its `return 1`.
    fn check_object(&mut self, id: gix::ObjectId, expected: Option<gix::object::Kind>) -> Step<bool> {
        if self.written.contains(&id) {
            return Ok(true);
        }
        let own = self.known.get(&id).copied();
        if let (Some(expected), Some(own)) = (expected, own) {
            if own != expected {
                return die("object type mismatch");
            }
        }
        let Some((kind, data)) = self.buffers.get(&id).cloned() else {
            // Not `FLAG_OPEN`: the object is only somewhere else.
            let found = self.repo.find_header(id).ok().map(|h| h.kind());
            if found.is_none() || found != own {
                return die("object of unexpected type");
            }
            self.written.insert(id);
            return Ok(true);
        };
        let reported = super::index_pack::report_fsck_object(kind, &data, &id, self.hash.len_in_hex());
        for blob in reported.gitmodules {
            self.finish_blobs.push((blob, true, false));
        }
        for blob in reported.gitattributes {
            self.finish_blobs.push((blob, false, true));
        }
        if reported.error {
            return die("fsck error in packed object");
        }
        // `fsck_walk()` with `check_object` as the walker.
        let mut links = Vec::new();
        let walked = super::index_pack::collect_links(kind, &data, &id, &mut links, self.hash);
        let mut result = walked;
        for (child, child_kind) in links {
            let ok = match self.lookup(child, child_kind) {
                Ok(()) => self.check_object(child, Some(child_kind))?,
                Err(()) => false,
            };
            result &= ok;
        }
        if !result {
            return die(format!("Error on reachable objects of {id}"));
        }
        // `write_cached_object()`
        if let Err(e) = self.repo.write_buf_with_known_id(kind, &data, id) {
            return die(format!("failed to write object {id}: {e}"));
        }
        self.written.insert(id);
        Ok(true)
    }

    /// `write_rest()` (builtin/unpack-objects.c:264-271).
    fn write_rest(&mut self) -> Step<()> {
        for nr in 0..self.obj_list.len() {
            if !self.obj_list[nr].held {
                continue;
            }
            if let Some(id) = self.obj_list[nr].oid {
                self.check_object(id, None)?;
            }
        }
        Ok(())
    }

    /// `fsck_finish()` over the blobs `fsck_object()` queued.
    fn fsck_finish(&mut self) -> Step<()> {
        let mut error = false;
        let mut done: HashSet<gix::ObjectId> = HashSet::new();
        for (id, as_modules, as_attrs) in std::mem::take(&mut self.finish_blobs) {
            if !done.insert(id) {
                continue;
            }
            let found = self.repo.find_object(id).ok().map(|o| (o.kind, o.data.clone()));
            error |= super::index_pack::report_fsck_finish_blob(&id, found, as_modules, as_attrs);
        }
        if error {
            return die("fsck error in pack objects");
        }
        Ok(())
    }

    /// `cmd_unpack_objects()` after the argument loop
    /// (builtin/unpack-objects.c:667-693).
    fn run(&mut self) -> Step<ExitCode> {
        self.unpack_all()?;
        let computed = self
            .hasher
            .clone()
            .try_finalize()
            .map_err(|e| Stop::Die(format!("{e}")))?;
        if self.strict {
            self.write_rest()?;
            self.fsck_finish()?;
        }
        let rawsz = self.hash.len_in_bytes();
        self.fill(rawsz)?;
        if self.buffer[self.offset..self.offset + rawsz] != *computed.as_bytes() {
            return die("final sha1 did not match");
        }
        self.consume(rawsz)?;
        // "Write the last part of the buffer to stdout"
        let rest = &self.buffer[self.offset..self.offset + self.len];
        if !rest.is_empty() {
            use std::io::Write as _;
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(rest);
            let _ = stdout.flush();
        }
        Ok(ExitCode::from(u8::from(self.has_errors)))
    }
}

/// The pack entry types that carry a delta.
const OBJ_OFS_DELTA: i32 = 6;
const OBJ_REF_DELTA: i32 = 7;

/// zlib's return code for an inflate failure, as `git_inflate()` passes it on.
fn zlib_code(e: &gix::zlib::DecompressError) -> i32 {
    use gix::zlib::DecompressError as E;
    match e {
        E::NeedDict => 2,
        E::StreamError => -2,
        E::DataError => -3,
        E::InsufficientMemory => -4,
    }
}

/// Validate a `--strict=<spec>` argument the way git's `fsck_set_msg_types()`
/// does, returning the `fatal:` body it would die with.
///
/// git lowercases the whole spec first, then walks the comma-separated
/// elements: each needs an `=`, the id must be known, and the severity must be
/// one it accepts from the command line. Empty elements are skipped, so
/// `--strict=,` is valid, and `skiplist=<path>` takes a path rather than a
/// severity. The id is checked before the severity, so `nosuchid=bogus` is
/// reported as an unknown id.
fn check_fsck_msg_types(spec: &str) -> Result<(), String> {
    let lowered = spec.to_ascii_lowercase();
    for element in lowered.split(',') {
        if element.is_empty() {
            continue;
        }
        let Some((id, severity)) = element.split_once('=') else {
            return Err(format!("Missing '=': '{element}'"));
        };
        if id == "skiplist" {
            continue;
        }
        if !FSCK_MSG_IDS.contains(&id) {
            return Err(format!("Unhandled message id: {id}"));
        }
        if !FSCK_SEVERITIES.contains(&severity) {
            return Err(format!("Unknown fsck message type: '{severity}'"));
        }
    }
    Ok(())
}

/// Rebuild the 12-byte pack header `--pack_header=<version>,<objects>`
/// describes, exactly as git's `parse_pack_header_option()` does: a `strtoul`
/// for the version, a literal comma, a `strtoul` for the entry count, and
/// nothing after it. `None` is git's `-1`, which it turns into
/// `die("bad --pack_header: %s")`.
///
/// Both numbers go through `strtoul`, so ` 2 ,0` is malformed (the space stops
/// the scan before the comma) while `+2,+0` is not, and `,` parses as two
/// zeroes — git accepts that and dies later on the version instead.
fn parse_pack_header_option(value: &str) -> Option<[u8; 12]> {
    let (version, rest) = strtoul(value);
    let rest = rest.strip_prefix(',')?;
    let (entries, rest) = strtoul(rest);
    if !rest.is_empty() {
        return None;
    }

    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(b"PACK");
    header[4..8].copy_from_slice(&version.to_be_bytes());
    header[8..12].copy_from_slice(&entries.to_be_bytes());
    Some(header)
}

/// C's `strtoul` over a base-10 prefix, returning the value and the unconsumed
/// tail. Leading whitespace and a sign are skipped; when no digit follows, the
/// tail is the whole input, matching `strtoul`'s "no conversion performed"
/// contract of leaving `endptr` at the start.
///
/// The result is narrowed to 32 bits because every caller feeds it to git's
/// `store_be32`, which truncates the same way.
fn strtoul(s: &str) -> (u32, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negated = i < bytes.len() && bytes[i] == b'-';
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return (0, s);
    }
    // Out of range saturates the way `strtoul` does, at the maximum.
    let value: u64 = s[digits_start..i].parse().unwrap_or(u64::MAX);
    let value = value as u32;
    (if negated { value.wrapping_neg() } else { value }, &s[i..])
}

/// git parses `--max-input-size=` with `strtoumax(arg, NULL, 10)`: it consumes
/// the leading run of base-10 digits and ignores the rest, so `1k` is 1 and a
/// value with no leading digit is 0 (which then means "no limit").
fn parse_magnitude(s: &str) -> u64 {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0;
    }
    // Out of range saturates the way `strtoumax` does, at the maximum.
    digits.parse().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings that decide the failing parity cases: git accepts
    /// `--strict` and `-r` outright, so a usage error next to them has to come
    /// from the other argument, not the flag.
    #[test]
    fn strict_spec_validation_matches_git() {
        // Verified against git 2.55.0: `--strict=missingEmail` dies with the
        // lowercased element, `=ignore` is accepted, an unknown id beats an
        // unknown severity, and `skiplist` takes a path rather than a severity.
        assert_eq!(
            check_fsck_msg_types("missingEmail"),
            Err("Missing '=': 'missingemail'".into())
        );
        assert_eq!(check_fsck_msg_types("missingEmail=ignore"), Ok(()));
        assert_eq!(
            check_fsck_msg_types("nosuchid=bogus"),
            Err("Unhandled message id: nosuchid".into())
        );
        assert_eq!(
            check_fsck_msg_types("missingEmail=bogus"),
            Err("Unknown fsck message type: 'bogus'".into())
        );
        assert_eq!(
            check_fsck_msg_types("a=b,c"),
            Err("Unhandled message id: a".into())
        );
        assert_eq!(check_fsck_msg_types("skiplist=/dev/null"), Ok(()));
        assert_eq!(check_fsck_msg_types(","), Ok(()));
        assert_eq!(check_fsck_msg_types(""), Ok(()));
        // git rejects the underscore spelling: it compares against the
        // underscore-stripped name.
        assert_eq!(
            check_fsck_msg_types("missing_email=ignore"),
            Err("Unhandled message id: missing_email".into())
        );
        // Only these three severities are settable from the command line.
        assert_eq!(check_fsck_msg_types("badtree=error"), Ok(()));
        assert_eq!(check_fsck_msg_types("badtree=warn"), Ok(()));
        assert_eq!(
            check_fsck_msg_types("badtree=fatal"),
            Err("Unknown fsck message type: 'fatal'".into())
        );
        assert_eq!(
            check_fsck_msg_types("badtree=info"),
            Err("Unknown fsck message type: 'info'".into())
        );
    }

    #[test]
    fn pack_header_option_matches_git() {
        // Verified against git 2.55.0: `2,0` reconstructs a v2 header, a
        // missing or trailing component is `bad --pack_header`, whitespace
        // stops the scan before the comma, and `,` is two zeroes.
        let hdr = parse_pack_header_option("2,0").expect("2,0 is well formed");
        assert_eq!(&hdr[0..4], b"PACK");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 2);
        assert_eq!(u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]), 0);

        let hdr = parse_pack_header_option("2,17").expect("2,17 is well formed");
        assert_eq!(u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]), 17);

        // The version is carried through verbatim; the caller is what refuses
        // the ones the decoder cannot take, so parsing must not filter them.
        let hdr = parse_pack_header_option("3,0").expect("3,0 parses");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 3);
        let hdr = parse_pack_header_option("0,0").expect("0,0 parses");
        assert_eq!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 0);

        assert!(parse_pack_header_option("+2,+0").is_some());
        assert!(parse_pack_header_option(",").is_some());
        assert!(parse_pack_header_option("bad").is_none());
        assert!(parse_pack_header_option("2").is_none());
        assert!(parse_pack_header_option("2,3x").is_none());
        assert!(parse_pack_header_option("2,0,").is_none());
        assert!(parse_pack_header_option(" 2 ,0").is_none());
    }

    #[test]
    fn max_input_size_parses_like_strtoumax() {
        // git's `strtoumax(arg, NULL, 10)`: leading digits only, and a value
        // with no leading digit is 0, which then means "no limit".
        assert_eq!(parse_magnitude("1048576"), 1048576);
        assert_eq!(parse_magnitude("1k"), 1);
        assert_eq!(parse_magnitude("abc"), 0);
        assert_eq!(parse_magnitude(""), 0);
        assert_eq!(parse_magnitude("0"), 0);
    }
}
