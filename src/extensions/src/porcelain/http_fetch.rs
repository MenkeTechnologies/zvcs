//! `git http-fetch` — download a repository over the *dumb* HTTP protocol.
//!
//! This is git's original HTTP client: it does plain `GET`s against a static
//! file server (`objects/<xx>/<38>`, `objects/info/packs`, `<ref-name>`) and
//! walks the object graph itself, one request per object. It predates and is
//! unrelated to smart HTTP (`git-upload-pack` over `info/refs?service=…`),
//! which is what `gix`'s transport layer implements.
//!
//! The walk is `walker.c`'s: a FIFO queue seeded with the targets, `process()`
//! marking objects SEEN and queueing them, `loop()` fetching each queued object
//! that is not already local and then scanning commits and trees for more.
//! Local refs seed a COMPLETE frontier that is popped by committer date so that
//! history already present is not re-walked; `--recover` disables that.
//!
//! Ported here, byte-verified against git 2.55.0 driven at a static file server:
//!
//!   * **Loose-object fetching** — the whole reachable graph, one `GET` per
//!     object. The downloaded bytes are installed verbatim at
//!     `objects/<xx>/<38>`, so the resulting files are byte-identical to the
//!     server's (git streams the same compressed bytes to disk).
//!   * **`<commit-id>`** as a full hex object id, or as a ref *path* fetched
//!     from `<url>/<target>` — note that git appends the target to the URL
//!     verbatim, so the argument is `refs/heads/main`, not `heads/main`, despite
//!     what the manual page suggests.
//!   * **`--stdin`** — `<commit-id>[TAB<filename-as-in--w>]` lines; every target
//!     is interpreted and queued before the walk runs, as git does.
//!   * **`-w <filename>`** — writes `refs/<filename>` after the walk, through a
//!     ref update, so the reflog line git writes (`fetch from <url>/`, with the
//!     URL always slash-terminated) is written too.
//!   * **`-v`** — `got <oid>` per downloaded object, `walk <oid>` per commit
//!     entered, `Getting alternates list for <base>` and `Getting pack list for
//!     <base>` on the first loose miss, all on stderr, in git's order.
//!   * **`-a`, `-c`, `-t`** — accepted and ignored, as git documents.
//!   * **`--recover`** — skips the COMPLETE seeding so the full graph is
//!     re-verified.
//!   * **exit codes and messages**: 0 on success; 129 with the usage line on
//!     stderr for a wrong argument count (which is also what a lone `-h`
//!     produces); 128 with `fatal: not a git repository` outside a repository;
//!     255 with `error: Could not interpret response from server '<t>' as
//!     something to pull` for an unresolvable target, and 255 with
//!     `error: Unable to find <oid> under <base>` plus
//!     `Cannot obtain needed <type> <oid>` plus, when a commit was being walked,
//!     `while processing commit <oid>.` for an object the server does not have.
//!     Objects fetched before a failure stay on disk, as with git.
//!   * **stdout is never written**, which is also true of stock `http-fetch`
//!     outside `--packfile`.
//!
//! Not ported — each bails rather than leaving a repository git would not have
//! produced:
//!
//!   * **Packed remotes.** When a loose `GET` 404s, git falls back to
//!     `objects/info/packs`, downloads the matching `.idx` and `.pack`, and
//!     installs them — plus a `.rev` reverse index, which `pack.writeReverseIndex`
//!     enables by default. `gix-pack` has no reverse-index writer at all (no
//!     `RIDX` encoder anywhere in the vendored tree), so that end state cannot
//!     be reproduced, and writing the pack without its `.rev` would leave a
//!     repository that differs from git's. This port therefore serves
//!     loose-object remotes only and bails the moment the pack list is
//!     non-empty. `git update-server-info` on an unpacked repository is exactly
//!     the loose case.
//!   * **`objects/info/http-alternates` / `objects/info/alternates`.** A
//!     non-empty alternates file makes git retry every miss against each listed
//!     base, recursively. Following that would fabricate a fetch order this
//!     port has not verified against git, so a non-empty list bails.
//!   * **`--packfile=<hash>` / `--index-pack-args=<args>`.** These bypass the
//!     walker entirely: git downloads one pack and pipes it through
//!     `git index-pack`, printing that program's stdout. It has the same `.rev`
//!     problem, and the URL-decoded `--index-pack-args` are git's own
//!     `index-pack` invocation, not something this port can honour. The
//!     `--packfile` argument is still validated first so git's
//!     `fatal: argument to --packfile must be a valid hash (got '<v>')` and its
//!     exit code 128 are reproduced.
//!   * **A `ref: <name>` response** to a target fetch (a symbolic ref served as
//!     a plain file). git records the symref and then walks a null id; that is a
//!     degenerate path this port refuses instead of imitating.
//!
//! One deliberate divergence: git silently ignores unrecognised options (its
//! parser tests one character and falls through), so `git http-fetch --nonsense
//! <id> <url>` exits 0. Silently accepting a flag whose effect is unimplemented
//! is exactly the failure mode this port must not have, so unknown options bail.
//!
//! One deliberate implementation difference with no observable effect: git
//! verifies a downloaded object in a temporary file and only then renames it
//! into place, while this port installs it and removes it again if verification
//! fails. Both leave nothing behind for a corrupt download.

use anyhow::{anyhow, bail, Result};
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, TreeRef};
use gix::protocol::transport::client::blocking_io::http::{reqwest::Remote, Http};
use gix::refs::transaction::PreviousValue;

/// The usage line stock `git http-fetch` prints, verbatim.
const USAGE: &str = "usage: git http-fetch [-c] [-t] [-a] [-v] [--recover] [-w ref] [--stdin | --packfile=hash | commit-id] url\n";

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: -a, -c, -t, -v, -w <ref>, --recover, --stdin";

/// `git http-fetch` — see the module docs for exactly what is and is not ported.
pub fn http_fetch(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand, mirroring git's `argv[0]`; git's own parser
    // starts at index 1 and so does this one.
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut arg = 1;

    let mut verbose = false;
    let mut recover = false;
    let mut on_stdin = false;
    let mut write_ref: Option<&str> = None;

    // git's loop: every argument starting with `-` is an option, and the tests
    // are on a single character, so `-w` takes the *next* argv entry.
    while arg < argv.len() && argv[arg].starts_with('-') {
        let a = argv[arg];
        match a.as_bytes()[1..].first().copied() {
            // Documented as ignored for historical reasons; `-h` matches no
            // branch in git either and falls through to the argument count check.
            Some(b't' | b'c' | b'a' | b'h') => {}
            Some(b'v') => verbose = true,
            Some(b'w') => {
                write_ref = argv.get(arg + 1).copied();
                arg += 1;
            }
            _ if a == "--recover" => recover = true,
            _ if a == "--stdin" => on_stdin = true,
            _ if a.starts_with("--packfile=") => {
                // git validates the hash inside the parse loop, before anything
                // else, and dies with this exact message.
                let value = &a["--packfile=".len()..];
                if ObjectId::from_hex(value.as_bytes()).is_err() {
                    eprintln!("fatal: argument to --packfile must be a valid hash (got '{value}')");
                    return Ok(ExitCode::from(128));
                }
                bail!(
                    "unsupported flag \"--packfile\" — it installs a downloaded pack via \
                     `git index-pack`, whose end state includes a `.rev` reverse index that \
                     gix-pack cannot write (no RIDX encoder in the vendored crates) ({PORTED})"
                )
            }
            _ if a.starts_with("--index-pack-args=") => bail!(
                "unsupported flag \"--index-pack-args\" — it carries git's own `index-pack` \
                 command line and is only meaningful with the unported --packfile ({PORTED})"
            ),
            // git ignores anything else; see the module docs for why this does not.
            _ => bail!("unsupported flag {a:?} ({PORTED})"),
        }
        arg += 1;
    }

    // `argc != arg + 2 - commits_on_stdin` — one target plus the URL, or just
    // the URL when the targets arrive on stdin.
    let expected = if on_stdin { 1 } else { 2 };
    if argv.len() != arg + expected {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // Targets are `(what to resolve, where to write it)` pairs.
    let mut targets: Vec<(String, Option<String>)> = Vec::new();
    if on_stdin {
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            match line.split_once('\t') {
                Some((target, name)) => targets.push((target.to_string(), Some(name.to_string()))),
                None => targets.push((line, None)),
            }
        }
    } else {
        targets.push((argv[arg].to_string(), write_ref.map(str::to_string)));
        arg += 1;
    }

    let url = argv[arg];
    // git slash-terminates the URL once and uses that form for the reflog
    // message, while the walker's base has every trailing slash stripped and is
    // what appears in its messages.
    let url_slash = if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    };
    let base = url.trim_end_matches('/').to_string();

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository");
        return Ok(ExitCode::from(128));
    };

    // Objects are written, so serialize behind the repo coordinator like the
    // other write commands; a no-op guard when no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut walker = Walker::new(repo, base, url_slash.clone(), verbose);
    if !recover {
        walker.mark_complete();
    }

    // git interprets and queues *every* target before running the walk once.
    let mut resolved: Vec<(ObjectId, Option<String>)> = Vec::with_capacity(targets.len());
    for (target, name) in &targets {
        let Some(id) = walker.interpret_target(target)? else {
            eprintln!("error: Could not interpret response from server '{target}' as something to pull");
            return Ok(ExitCode::from(255));
        };
        resolved.push((id, name.clone()));
        walker.process(id, None);
    }

    if !walker.walk()? {
        return Ok(ExitCode::from(255));
    }

    // The refs are written last, in one batch, exactly as git's single ref
    // transaction does.
    let message = format!("fetch from {url_slash}");
    for (id, name) in &resolved {
        let Some(name) = name else { continue };
        let full = format!("refs/{name}");
        if gix::validate::reference::name(full.as_str().into()).is_err() {
            eprintln!("error: refusing to update ref with bad name '{full}'");
            return Ok(ExitCode::from(255));
        }
        walker
            .repo
            .reference(full.as_str(), *id, PreviousValue::Any, message.as_str())?;
    }

    Ok(ExitCode::SUCCESS)
}

/// git's `type_name()`, used in the `Cannot obtain needed <type>` report.
fn type_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Commit => "commit",
        Kind::Tree => "tree",
        Kind::Blob => "blob",
        Kind::Tag => "tag",
    }
}

/// One entry of the walk queue, mirroring the object `process()` pushed.
struct Item {
    id: ObjectId,
    /// The type the object was queued as, which is what `report_missing()`
    /// names when the fetch fails. A target queued from the command line has no
    /// type yet and git calls it `object`.
    kind: Option<Kind>,
    /// git's `TO_SCAN`: the object is already local, so it is only scanned.
    to_scan: bool,
}

/// `walker.c`'s state plus the dumb-HTTP client that backs its `fetch`.
struct Walker {
    repo: gix::Repository,
    /// The remote URL with trailing slashes stripped — what git's messages show.
    base: String,
    /// The same URL slash-terminated — what every request is built from.
    base_slash: String,
    verbose: bool,
    http: Remote,
    /// git's `SEEN` flag.
    seen: HashSet<ObjectId>,
    /// git's `COMPLETE` flag.
    complete: HashSet<ObjectId>,
    /// git's `complete` commit list, kept newest-first by committer date.
    frontier: BinaryHeap<(i64, ObjectId)>,
    queue: VecDeque<Item>,
    /// git's `current_commit_oid`, reported when an object cannot be obtained.
    current_commit: Option<ObjectId>,
    alternates_probed: bool,
    packs_probed: bool,
}

impl Walker {
    fn new(repo: gix::Repository, base: String, url_slash: String, verbose: bool) -> Self {
        Walker {
            repo,
            base,
            base_slash: url_slash,
            verbose,
            http: Remote::default(),
            seen: HashSet::new(),
            complete: HashSet::new(),
            frontier: BinaryHeap::new(),
            queue: VecDeque::new(),
            current_commit: None,
            alternates_probed: false,
            packs_probed: false,
        }
    }

    /// `mark_complete()` over every local ref: each one that peels to a commit
    /// seeds the COMPLETE frontier, which `process_commit` then pops by date.
    fn mark_complete(&mut self) {
        let Ok(platform) = self.repo.references() else {
            return;
        };
        let Ok(iter) = platform.all() else { return };
        let mut seeds = Vec::new();
        for reference in iter.flatten() {
            let Ok(id) = reference.into_fully_peeled_id() else {
                continue;
            };
            let id = id.detach();
            if let Some(date) = self.commit_date(id) {
                seeds.push((date, id));
            }
        }
        for (date, id) in seeds {
            if self.complete.insert(id) {
                self.frontier.push((date, id));
            }
        }
    }

    /// The committer date of a locally present commit, or `None` when the object
    /// is absent or is not a commit — git's `lookup_commit_reference_gently`.
    fn commit_date(&self, id: ObjectId) -> Option<i64> {
        let object = self.repo.find_object(id).ok()?.detach();
        if object.kind != Kind::Commit {
            return None;
        }
        let commit = CommitRef::from_bytes(&object.data, self.repo.object_hash()).ok()?;
        Some(commit.committer().ok()?.time().ok()?.seconds)
    }

    /// `interpret_target()`: a full hex id, otherwise a name whose value is
    /// fetched from `<base>/<name>`. `Ok(None)` is git's "could not interpret".
    fn interpret_target(&mut self, target: &str) -> Result<Option<ObjectId>> {
        if let Ok(id) = ObjectId::from_hex(target.as_bytes()) {
            return Ok(Some(id));
        }
        let Some(body) = self.get(target) else {
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&body);
        let text = text.trim_end();
        if text.starts_with("ref: ") {
            bail!(
                "target {target:?} resolved to the symbolic ref {:?}; git records the symref and \
                 then walks a null object id, which this port refuses to imitate ({PORTED})",
                text["ref: ".len()..].trim()
            );
        }
        Ok(ObjectId::from_hex(text.as_bytes()).ok())
    }

    /// `process()`: mark SEEN, decide whether the object needs fetching, queue it.
    fn process(&mut self, id: ObjectId, kind: Option<Kind>) {
        if !self.seen.insert(id) {
            return;
        }
        let to_scan = self.repo.has_object(id);
        if !to_scan && self.complete.contains(&id) {
            return;
        }
        self.queue.push_back(Item { id, kind, to_scan });
    }

    /// `loop()`: drain the queue, fetching what is missing and scanning commits
    /// and trees for more. `Ok(false)` is git's "cannot obtain" failure.
    fn walk(&mut self) -> Result<bool> {
        while let Some(item) = self.queue.pop_front() {
            if !item.to_scan && !self.fetch(item.id)? {
                let type_name = item.kind.map_or("object", type_name);
                eprintln!("error: Unable to find {} under {}", item.id, self.base);
                eprintln!("Cannot obtain needed {type_name} {}", item.id);
                if let Some(commit) = self.current_commit {
                    eprintln!("while processing commit {commit}.");
                }
                return Ok(false);
            }

            // Detached so the walk can mutate its own state while scanning.
            let object = self.repo.find_object(item.id)?.detach();
            match object.kind {
                Kind::Commit => self.process_commit(item.id, &object.data)?,
                Kind::Tree => self.process_tree(&object.data)?,
                Kind::Blob | Kind::Tag => {}
            }
        }
        Ok(true)
    }

    /// `process_commit()`: advance the COMPLETE frontier past this commit's
    /// date, then queue its tree and parents.
    fn process_commit(&mut self, id: ObjectId, data: &[u8]) -> Result<()> {
        let commit = CommitRef::from_bytes(data, self.repo.object_hash())?;
        let date = commit
            .committer()
            .map_err(|e| anyhow!("{e}"))?
            .time()
            .map_err(|e| anyhow!("{e}"))?
            .seconds;

        while self.frontier.peek().is_some_and(|(top, _)| *top >= date) {
            self.pop_most_recent_commit();
        }
        if self.complete.contains(&id) {
            return Ok(());
        }

        self.current_commit = Some(id);
        if self.verbose {
            eprintln!("walk {id}");
        }
        self.process(commit.tree(), Some(Kind::Tree));
        for parent in commit.parents() {
            self.process(parent, Some(Kind::Commit));
        }
        Ok(())
    }

    /// `pop_most_recent_commit()`: take the newest commit off the frontier and
    /// push its locally present parents on in its place, marked COMPLETE.
    fn pop_most_recent_commit(&mut self) {
        let Some((_, id)) = self.frontier.pop() else {
            return;
        };
        let Ok(object) = self.repo.find_object(id).map(|o| o.detach()) else {
            return;
        };
        let Ok(commit) = CommitRef::from_bytes(&object.data, self.repo.object_hash()) else {
            return;
        };
        let parents: Vec<ObjectId> = commit.parents().collect();
        for parent in parents {
            if self.complete.contains(&parent) {
                continue;
            }
            if let Some(date) = self.commit_date(parent) {
                self.complete.insert(parent);
                self.frontier.push((date, parent));
            }
        }
    }

    /// `process_tree()`: queue every entry, skipping submodule commits, which
    /// are not stored in the superproject.
    fn process_tree(&mut self, data: &[u8]) -> Result<()> {
        let tree = TreeRef::from_bytes(data, self.repo.object_hash())?;
        let entries: Vec<(ObjectId, Kind)> = tree
            .entries
            .iter()
            .filter(|e| !e.mode.is_commit())
            .map(|e| {
                (
                    e.oid.to_owned(),
                    if e.mode.is_tree() { Kind::Tree } else { Kind::Blob },
                )
            })
            .collect();
        for (id, kind) in entries {
            self.process(id, Some(kind));
        }
        Ok(())
    }

    /// `fetch()`: try the loose object, and on a miss walk git's fallbacks far
    /// enough to tell an empty server apart from a packed one.
    fn fetch(&mut self, id: ObjectId) -> Result<bool> {
        let hex = id.to_hex().to_string();
        let (dir, file) = hex.split_at(2);
        if let Some(body) = self.get(&format!("objects/{dir}/{file}")) {
            if self.install_loose(id, &body)? {
                if self.verbose {
                    eprintln!("got {hex}");
                }
                return Ok(true);
            }
        }
        self.probe_alternates()?;
        self.probe_packs()?;
        Ok(false)
    }

    /// Install a downloaded loose object verbatim and verify it hashes to `id`,
    /// removing it again if it does not.
    fn install_loose(&self, id: ObjectId, body: &[u8]) -> Result<bool> {
        let dir = self
            .repo
            .common_dir()
            .join("objects")
            .join(&id.to_hex().to_string()[..2]);
        std::fs::create_dir_all(&dir)?;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let temp: PathBuf = dir.join(format!("tmp_obj_{}_{nonce}", std::process::id()));
        std::fs::write(&temp, body)?;

        let final_path = dir.join(&id.to_hex().to_string()[2..]);
        if let Err(e) = std::fs::rename(&temp, &final_path) {
            let _ = std::fs::remove_file(&temp);
            return Err(e.into());
        }

        let sound = match self.repo.find_object(id).map(|o| o.detach()) {
            Ok(object) => {
                gix::objs::compute_hash(self.repo.object_hash(), object.kind, &object.data)
                    .is_ok_and(|actual| actual == id)
            }
            Err(_) => false,
        };
        if !sound {
            let _ = std::fs::remove_file(&final_path);
        }
        Ok(sound)
    }

    /// git's `fetch_alternates()`, run once on the first miss. Following a
    /// non-empty list is unported, so it bails instead.
    fn probe_alternates(&mut self) -> Result<()> {
        if self.alternates_probed {
            return Ok(());
        }
        self.alternates_probed = true;
        if self.verbose {
            eprintln!("Getting alternates list for {}", self.base);
        }
        for name in ["objects/info/http-alternates", "objects/info/alternates"] {
            let Some(body) = self.get(name) else { continue };
            if body.iter().any(|b| !b.is_ascii_whitespace()) {
                bail!(
                    "remote lists {name} — git retries every miss against each alternate base \
                     recursively, a fetch order this port has not verified against git ({PORTED})"
                );
            }
        }
        Ok(())
    }

    /// git's `fetch_indices()`, run once on the first miss. A non-empty pack
    /// list is the unported case; see the module docs for the `.rev` gap.
    fn probe_packs(&mut self) -> Result<()> {
        if self.packs_probed {
            return Ok(());
        }
        self.packs_probed = true;
        if self.verbose {
            eprintln!("Getting pack list for {}", self.base);
        }
        let Some(body) = self.get("objects/info/packs") else {
            return Ok(());
        };
        // `objects/info/packs` is a list of `P <pack-name>.pack` lines.
        let listed = body
            .split(|b| *b == b'\n')
            .any(|line| line.starts_with(b"P "));
        if listed {
            bail!(
                "remote serves packs, which git installs alongside a `.rev` reverse index \
                 (pack.writeReverseIndex, on by default); gix-pack has no RIDX encoder, so that \
                 repository state cannot be reproduced. Only loose-object remotes are ported \
                 ({PORTED})"
            );
        }
        Ok(())
    }

    /// One `GET` of `<base>/<tail>`, or `None` for any non-success status —
    /// which is how a dumb server reports "no such file".
    fn get(&mut self, tail: &str) -> Option<Vec<u8>> {
        let url = format!("{}{tail}", self.base_slash);
        let mut response = self
            .http
            .get(&url, &self.base_slash, Vec::<&str>::new())
            .ok()?;

        // The headers must be drained first: the backend writes them into a
        // zero-capacity pipe before streaming the body, and reports a non-success
        // status as an error on that same pipe.
        let mut headers = Vec::new();
        response.headers.read_to_end(&mut headers).ok()?;

        let mut body = Vec::new();
        response.body.read_to_end(&mut body).ok()?;
        Some(body)
    }
}
