# ROADMAP — remaining parity work

**Generated from a measured parity run, not from prose.** Every item below is a
case the differential harness scored as a failure, named by its case id so it can
be re-run individually.

```
run date   : 2026-08-20
binary     : target/release/git (release build)
oracle     : /opt/homebrew/bin/git — stock git 2.55.0
             (matches GIT_VERSION in porcelain/version.rs; version floor enforced)
corpus     : curated (no --fuzz)
```

```
coverage : 181/181 stock subcommands dispatched (100.0%)
parity   : 4825/4833 cases matched (99.8%)
           unsupported=0  stdout-diff=5  exit-diff=3
           state-diff=0   crash=0  hang=0  zvcs-flaky=0
```

**171 of 178 measured commands are clean** across 4,118 cases. The whole of the
remaining corpus work is **8 cases in 7 commands**, listed individually below —
there is no long tail to prioritize, so this document names every item rather
than ranking a backlog.

---

## 0. Read this before treating 99.8% as "almost done"

Three caveats, in descending order of how much they should change your plans.

**The corpus is not the surface.** These 4,833 cases are curated. The README
records that generated cases have found defects no curated case reached —
`init --bare nested/dir` failing outright, `fast-import` leaving a different
object store after a rejected command line, `cherry-pick` refusing strategies git
accepts. A clean corpus means *the corpus is nearly exhausted as a source of
work*, not that the port is nearly finished. **§5 is the real next step.**

**Coverage is not parity, and parity is not correctness.** 181/181 dispatch says
every subcommand answers. 99.8% says the cases we wrote agree. Neither says
anything about flags no case exercises. A green subcommand means "agrees on what
was asked", never "agrees".

**Two harness defects were fixed to obtain these numbers** (§4). Any parity
figure recorded before 2026-08-20 was measured against the wrong stock git and is
not comparable to this one.

---

## 1. Correctness tier — none

`state-diff=0`. Not one case left a repository in a state differing from stock's.
Every remaining failure is output text or an exit code; none can corrupt a
repository, lose a commit, or mislead a subsequent command about on-disk state.

`zvcs-flaky=0`: the port never disagreed with itself. Every failure below was
reproduced exactly on a second run, so all 8 are deterministic and directly
debuggable.

This tier being empty is the single most important line in this document.

---

## 2. Real defects — 7 cases

Ordered by blast radius: a wrong exit code silently breaks `&&` chains and CI
gates, so those rank above wrong bytes.

### 2.1 `--namespace=<ns>` is not honored by the ref readers — 2 cases

```
branched::for-each-ref::--namespace=ns for-each-ref
  stock: exit 0, lists all four refs
  zvcs : exit 1, "zvcs: for-each-ref: The reference 'HEAD' did not exist"

branched::show-ref::--namespace=ns show-ref
  stock: exit 0, lists all four refs
  zvcs : exit 1, empty stdout
```

Under `--namespace=ns`, git resolves refs inside `refs/namespaces/ns/` and falls
back to the real ref store for reads that find nothing there. Both commands
appear to resolve into the empty namespace and then fail rather than falling
back. `for-each-ref` additionally surfaces an internal message in a non-git
format (`zvcs: <cmd>: <gix error>`), which is a second, smaller defect: the
wording is not git's.

Likely one shared fix in the ref-resolution path, given both commands fail
identically. Worth checking whether other ref readers (`rev-list`, `branch`,
`tag`) have the same gap but no case covering it.

**Highest-value item here** — it is the only failure implicating a whole
subsystem rather than one flag.

### 2.2 `--pretty=<name>` does not consult `pretty.<name>` config — 1 case

```
branched::log::-c pretty.custom=%H %s log -1 --pretty=custom
  stock: exit 0, "5915d79… add two"
  zvcs : exit 128, "fatal: invalid --pretty format: custom"
```

Git treats an unrecognized `--pretty` name as a lookup into `pretty.<name>`
config before rejecting it. The port rejects immediately. The error path is
otherwise correct — right exit code and phrasing for a genuinely invalid name —
so this is a missing lookup, not a broken parser.

### 2.3 `rev-parse --git-common-dir` returns a relative path — 2 cases

```
linear::rev-parse::rev-parse --git-common-dir::cwd[.git/refs/heads]
  stock: /private<REPO>/.git
  zvcs : ../../.

behind-remote::rev-parse::rev-parse --git-common-dir::cwd[.remote.git/refs]
  stock: /private<REPO>/.remote.git
  zvcs : ../.
```

Both fail only when cwd is **inside** the git directory. The port emits a path
relative to cwd; stock emits an absolute one. Note the two disagree about depth
as well (`../../.` vs `../.`), so the relative form is at least self-consistent —
this is a missing absolutization, not a traversal bug.

Both cases come from the discovery-aware harness feature added after cases gained
their own `cwd`. That feature previously exposed three bugs including a process
abort in a bare repository's subdirectory; this is a fourth. **Treat
run-from-inside-`.git` as an under-tested area generally.**

### 2.4 `diff.mnemonicPrefix` is ignored — 1 case

```
dirty::diff::-c diff.mnemonicPrefix=true diff HEAD
  stock: diff --git c/README.md w/README.md
  zvcs : diff --git a/README.md b/README.md
```

With `diff.mnemonicPrefix` set, git replaces the `a/`…`b/` prefixes with
mnemonics for what each side *is* — `c/` commit, `w/` worktree, `i/` index,
`o/` object. The port always emits `a/`…`b/`. Self-contained: a prefix-selection
function keyed on the comparison's two sides, wired into the config read.

### 2.5 `stash branch` omits the pre-restore status lines — 1 case

```
!stashed::stash::stash branch off-stash stash@{1}
  stock stdout begins:  M<TAB>counter.txt
                        M<TAB>notes.txt
                        Already up to date.
  zvcs  stdout begins:  Already up to date.
```

Stock prints the two `M<TAB><path>` lines — checkout's report of carried
modifications — before the merge result. The port starts at `Already up to date.`
The rest of the output matches, so this is a missing emission at one point in
`stash branch`, not a formatting divergence.

Note the case id's leading `!`: this fixture is itself mutated by the case, which
is why it is worth re-running in isolation rather than only as part of a sweep.

---

## 3. Structural — not work, do not "fix"

### `version --build-options` — 1 case, permanently

```
linear::version::version --build-options
  stock: rust: disabled / feature: fsmonitor--daemon / gettext: enabled
         libcurl: 8.7.1 / zlib: 1.2.12
  zvcs : rust: enabled / zlib-rs: 0.6.6 (inflate only)
```

The command reports facts about **the build that prints it**. Stock describes a C
toolchain with gettext, libcurl and zlib linked; this binary is Rust and links
none of them. Ten of stock's fifteen lines already agree — `cpu`, `sizeof-*`,
`shell-path`, `SHA-1: SHA1_DC`, `SHA-256: SHA256_BLK`, `default-ref-format`,
`default-hash` — because those are the same facts about both.

Matching the rest would require reporting another installation's linked libraries
as this binary's own, which would be false. **This case will never pass and
should not.** It is why `version` reads 66.7%: 1 of only 3 cases.

The same reasoning covers the other documented structural limits — `git p4`'s
usage embedding `sys.argv[0]`, and `help --all --no-verbose` heading its listing
with the invoking installation's exec-path. Neither appears in the failures above
because no corpus case reaches them.

**Suggested action:** none to the code. Consider an explicit exclusion bucket in
the harness so a permanently-unmatchable case is reported as such rather than as
a failure — the harness already distinguishes unmeasurable cases, and this
belongs with them.

---

## 4. Harness defects fixed to produce this run

Both were silent, and both made previous numbers untrustworthy rather than wrong
in an obvious way. Recorded here because they are the reason this run supersedes
earlier ones.

**Version floor never fired** (`src/parity/src/stock.rs`). The target version was
located by scanning for a line *starting with* `const GIT_VERSION`; the
declaration reads `pub(crate) const GIT_VERSION`, and `trim_start` removes
whitespace, not modifiers. The scan returned `None`, the caller's `.filter()`
dropped the check, and the harness measured a 2.55.0-targeted binary against
stock **2.50.1** while printing numbers that read exactly like enforced ones.
Coverage was reported as 170/170 rather than 181/181 — 2.50.1 simply ships fewer
subcommands.

Fixed: match anywhere on the line; return `Result` so an unreadable target is a
hard error rather than an absent floor. Two regression tests added — `stock.rs`
previously had none.

**Pipe drain could deadlock after the case timeout** (`src/parity/src/runner.rs`).
`child.kill()` reaps the child but never its grandchildren; a process holding the
inherited stdout write end keeps the pipe open, and the subsequent `read_to_end`
blocks forever. A full corpus run hung indefinitely this way, with the case
timeout working perfectly and the read below it never returning.

Fixed: each pipe drains on its own thread under a 5s `DRAIN_TIMEOUT`; buffered
bytes are kept, a stuck reader is abandoned. Verified not to change any verdict —
`status` 81/81 and `am` 43/43 before and after.

---

## 5. The actual next step: fuzz

With the curated corpus at 99.8% and every remaining failure named above, **the
corpus has stopped being a source of new work.** Generated cases are where the
next real findings are.

```sh
cargo run --release -p zvcs-parity -- --fuzz 12          # ~12 generated cases/command
cargo run --release -p zvcs-parity -- --fuzz 40 --seed 7 # wider, reproducible
```

Expect this to *lower* the headline number, and treat that as the harness
working. A fuzz sweep found each of the corpus-invisible defects the README
documents.

Suggested order of work:

1. **Fuzz sweep first.** Sizes the real remaining surface before anyone spends a
   day on a single-case formatting fix. §2's eight cases are not going anywhere.
2. **§2.1 `--namespace`** — the only whole-subsystem gap, and it fails closed
   (exit 1) rather than producing wrong output.
3. **§2.3 run-from-inside-`.git`** — twice-demonstrated weak area; worth
   auditing beyond the two failing cases.
4. **§2.2, §2.4, §2.5** — self-contained single-behavior fixes, any order.
5. **§3** — no code change; consider reclassifying in the harness.

---

## 6. Reproducing this

```sh
# Requires stock git >= the GIT_VERSION in porcelain/version.rs (2.55.0).
# The harness now refuses an older one rather than measuring against it.
cargo build --release --bin git
cargo run --release -q -p zvcs-parity                     # full corpus
cargo run --release -q -p zvcs-parity -- --only diff,for-each-ref,log,rev-parse,show-ref,stash,version --verbose
```

A single case re-runs by its id's command segment via `--only`. `--verbose`
prints each failure's stock/zvcs stdout, exit codes, and state digests, plus the
unmeasurable and flaky buckets by name.
