//! git's progress meter (`progress.c`), as `pack-objects`, `repack`, `gc` and the
//! fetch-side `index-pack`/`unpack-objects` meters write it.
//!
//! Every phase reports through [`Meter`], which reproduces `progress.c`'s framing
//! byte for byte (checked against git 2.55.0 driven from a pseudo-terminal):
//!
//! ```text
//!   Enumerating objects: 9, done.\n          an unbounded count
//!   Counting objects:  11% (1/9)\r           a bounded one, redrawn in place
//!   Counting objects: 100% (9/9), done.\n    …and its closing line
//!   Receiving objects: 100% (6/6), 6.70 KiB | 571.00 KiB/s, done.\n
//! ```
//!
//! Each redraw ends in a carriage return so the next one overwrites it; only the
//! closing line ends in a newline, which the terminal's own `onlcr` renders as
//! the `\r\n` a capture of git shows. A bounded meter redraws when its whole-number
//! percentage changes or when the once-a-second update tick has fired, and an
//! unbounded one only on the tick; the percentage is right-aligned in three
//! columns (`  1%`, ` 50%`, `100%`).
//!
//! Everything goes to stderr. Whether a command shows progress at all is its own
//! decision (git's callers pass a NULL `struct progress *` otherwise); a meter
//! built with `on == false` is inert. [`enabled`] answers the pack-writing
//! commands' rule once so every caller asks it the same way.
//!
//! "stderr" can be a relay rather than fd 2: `upload-pack` runs `pack-objects`
//! with `pack_objects.err = -1` and ships what the child writes there to the
//! client on band 2 (`create_pack_file()`, upload-pack.c:360-361/436-452). The
//! pack writer runs in-process here, so [`with_relay`] stands in for that pipe:
//! every meter line, and every other line the pack writer reports through
//! [`write_stderr`], goes to the relay instead.

use std::cell::RefCell;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

thread_local! {
    /// The pipe `start_command()` gave the `pack-objects` child for its stderr.
    static RELAY: RefCell<Option<Box<dyn FnMut(&[u8])>>> = const { RefCell::new(None) };
}

/// Run `f` with this thread's progress output handed to `relay` instead of
/// stderr, as `upload-pack`'s `pack-objects` child writes into a pipe.
pub fn with_relay<R>(relay: impl FnMut(&[u8]) + 'static, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Box<dyn FnMut(&[u8])>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            RELAY.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = RELAY.with(|slot| slot.borrow_mut().replace(Box::new(relay)));
    let _restore = Restore(previous);
    f()
}

/// `fprintf(stderr, …); fflush(stderr)` from the pack writer: to the relay when
/// one is installed, else to stderr.
pub fn write_stderr(bytes: &[u8]) {
    let relayed = RELAY.with(|slot| match slot.borrow_mut().as_mut() {
        Some(relay) => {
            relay(bytes);
            true
        }
        None => false,
    });
    if !relayed {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(bytes);
        let _ = err.flush();
    }
}

fn relaying() -> bool {
    RELAY.with(|slot| slot.borrow().is_some())
}

/// Whether a pack-writing command should report progress: git's rule is a
/// terminal on stderr and no `--quiet`.
pub fn enabled(quiet: bool) -> bool {
    !quiet && std::io::stderr().is_terminal()
}

/// The interval `set_progress_signal()` arms its `SIGALRM` for (`progress.c:88-91`).
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);

/// `TP_IDX_MAX` (`progress.c:24`): how many samples the running rate averages over.
const TP_IDX_MAX: usize = 8;

/// `struct throughput` (`progress.c:26-36`).
struct Throughput {
    curr_total: u64,
    prev_total: u64,
    prev: Instant,
    avg_bytes: u32,
    avg_misecs: u32,
    last_bytes: [u32; TP_IDX_MAX],
    last_misecs: [u32; TP_IDX_MAX],
    idx: usize,
    display: String,
}

/// One phase — git's `struct progress`.
///
/// A disabled meter writes nothing, so callers can drive it unconditionally.
pub struct Meter {
    title: &'static str,
    /// The phase's final count, if it is known up front. Without one the meter
    /// prints a bare running total, as git does while it is still enumerating.
    total: Option<usize>,
    current: usize,
    /// `last_value`, `-1` in git until the first display.
    last_value: Option<usize>,
    /// The last percentage drawn, so a bounded meter redraws only when the
    /// whole number changes rather than once per object.
    last_percent: Option<u32>,
    /// `progress->start_ns`, which the closing throughput average is taken over.
    started: Instant,
    /// When the next `SIGALRM` would set `progress_update`.
    next_update: Instant,
    throughput: Option<Throughput>,
    /// `counters_sb`: the text after `<title>: ` as last drawn.
    counters: String,
    /// `progress->split`: the counters moved to their own line once the full line
    /// no longer fit the terminal.
    split: bool,
    /// `progress->delay`: update ticks still to pass before the first draw.
    delay: u32,
    on: bool,
}

impl Meter {
    /// A phase whose size is not known yet — git's `Enumerating objects`.
    pub fn unknown(title: &'static str, on: bool) -> Self {
        Self::start(title, None, on)
    }

    /// A phase of `total` items — git's `Counting`, `Compressing` and `Writing`.
    pub fn counted(title: &'static str, total: usize, on: bool) -> Self {
        Self::start(title, Some(total), on)
    }

    /// `start_delayed_progress()` (`progress.c:291-295`): a phase of `total` items
    /// that stays silent until it has outlived `GIT_PROGRESS_DELAY` update ticks.
    /// A phase that finishes sooner never draws, and its [`Meter::stop`] prints
    /// nothing.
    ///
    /// `GIT_PROGRESS_DELAY` is read only for a meter that is on, as git only
    /// calls `start_delayed_progress()` when it will report; an unparsable value
    /// is `die()` at that point, which the caller reports as its own process would.
    pub fn delayed(title: &'static str, total: usize, on: bool) -> Result<Self, DelayError> {
        let mut meter = Self::start(title, Some(total), on);
        if on {
            meter.delay = default_delay()?;
        }
        Ok(meter)
    }

    /// `start_progress_delay()` (`progress.c:259-279`) with no delay.
    fn start(title: &'static str, total: Option<usize>, on: bool) -> Self {
        let now = Instant::now();
        Meter {
            title,
            total,
            current: 0,
            last_value: None,
            last_percent: None,
            started: now,
            next_update: now + UPDATE_INTERVAL,
            throughput: None,
            counters: String::new(),
            split: false,
            delay: 0,
            on,
        }
    }

    /// Count one item and redraw if that changed what the line would say.
    pub fn tick(&mut self) {
        self.advance(1);
    }

    /// Count `n` items at once, for a phase that reports in batches.
    pub fn advance(&mut self, n: usize) {
        self.set(self.current + n);
    }

    /// `display_progress(progress, n)`: the count is now `n`.
    pub fn set(&mut self, n: usize) {
        self.current = n;
        if self.on {
            self.display(n, None, false);
        }
    }

    /// Close a phase whose output went through a throughput-counting hashfile:
    /// `pack-objects --stdout`'s `Writing objects`, which `hashfd_ext()` hands
    /// the progress (`builtin/pack-objects.c:1350-1363`) so every flush reports
    /// the bytes written.
    ///
    /// The redraws before it carry no rate — `display_throughput()` only fills
    /// the display after half a second has passed (`progress.c:214-216`) — so
    /// only the closing line has one.
    ///
    /// Like [`Meter::stop`], a phase that never displayed ends silently:
    /// `stop_progress_msg()` skips `force_last_update()` while `last_value` is
    /// still `-1` (`progress.c:375-376`), so an empty pack draws no
    /// `Writing objects` line at all.
    pub fn done_with_throughput(mut self, total_bytes: u64) {
        let (true, Some(last)) = (self.on, self.last_value) else {
            return;
        };
        self.throughput(total_bytes);
        self.force_last_update("done", last);
    }

    /// `stop_progress_msg()` (`progress.c:362-385`): close the phase with
    /// `, <msg>.`, carrying the whole-phase throughput average if bytes were
    /// reported. A phase that was never displayed ends silently, as git's does.
    pub fn stop(mut self, msg: &str) {
        if let (true, Some(last)) = (self.on, self.last_value) {
            self.force_last_update(msg, last);
        }
    }

    /// `display_throughput()` (`progress.c:193-251`): `total` bytes have been
    /// transferred so far. The first report only starts the clock; the rate shown
    /// is refreshed at most every half second, averaged over the last
    /// [`TP_IDX_MAX`] refreshes, and drawn with the next update tick.
    pub fn throughput(&mut self, total: u64) {
        if !self.on {
            return;
        }
        let now = Instant::now();
        let Some(tp) = self.throughput.as_mut() else {
            self.throughput = Some(Throughput {
                curr_total: total,
                prev_total: total,
                prev: now,
                avg_bytes: 0,
                avg_misecs: 0,
                last_bytes: [0; TP_IDX_MAX],
                last_misecs: [0; TP_IDX_MAX],
                idx: 0,
                display: String::new(),
            });
            return;
        };
        tp.curr_total = total;
        let elapsed = now.duration_since(tp.prev);
        if elapsed.as_nanos() <= 500_000_000 {
            return;
        }
        let misecs = misecs(elapsed);
        let count = total.wrapping_sub(tp.prev_total) as u32;
        tp.prev_total = total;
        tp.prev = now;
        tp.avg_bytes = tp.avg_bytes.wrapping_add(count);
        tp.avg_misecs = tp.avg_misecs.wrapping_add(misecs);
        let rate = tp.avg_bytes / tp.avg_misecs.max(1);
        tp.avg_bytes = tp.avg_bytes.wrapping_sub(tp.last_bytes[tp.idx]);
        tp.avg_misecs = tp.avg_misecs.wrapping_sub(tp.last_misecs[tp.idx]);
        tp.last_bytes[tp.idx] = count;
        tp.last_misecs[tp.idx] = misecs;
        tp.idx = (tp.idx + 1) % TP_IDX_MAX;
        tp.display = throughput_string(total, rate);
        if let (Some(last), true) = (self.last_value, now >= self.next_update) {
            self.display(last, None, false);
        }
    }

    /// `force_last_update()` (`progress.c:332-348`): replace the running rate with
    /// the average since `start_progress()` and draw the closing line.
    fn force_last_update(&mut self, msg: &str, value: usize) {
        if let Some(tp) = self.throughput.as_mut() {
            let rate = tp.curr_total / u64::from(misecs(self.started.elapsed()).max(1));
            tp.display = throughput_string(tp.curr_total, rate as u32);
        }
        self.display(value, Some(&format!(", {msg}.\n")), true);
    }

    /// `100 * current / total`, or `None` when the total is unknown. A total of
    /// zero reads as complete.
    fn percent(&self) -> Option<u32> {
        match self.total {
            Some(0) => Some(100),
            Some(total) => Some(((self.current as u64 * 100) / total as u64) as u32),
            None => None,
        }
    }

    /// Consume the update tick: whether a `SIGALRM` fired since the last display.
    /// `display()` clears `progress_update` on every call, drawn or not.
    fn take_update(&mut self) -> bool {
        let now = Instant::now();
        if now < self.next_update {
            return false;
        }
        while self.next_update <= now {
            self.next_update += UPDATE_INTERVAL;
        }
        true
    }

    /// `display()` (`progress.c:112-173`). `done` is the closing text; `force` is
    /// `force_last_update()` setting `progress_update` first.
    fn display(&mut self, n: usize, done: Option<&str>, force: bool) {
        let update = self.take_update() || force;
        // `if (progress->delay && (!update || --progress->delay)) return;`
        if self.delay != 0 {
            if !update {
                return;
            }
            self.delay -= 1;
            if self.delay != 0 {
                return;
            }
        }
        self.last_value = Some(n);
        let tp = self.throughput.as_ref().map_or_else(String::new, |tp| tp.display.clone());
        let last_count_len = self.counters.len();
        // `if (progress->total)` (progress.c:127): a total of 0 draws like an
        // unknown one, a bare count and only on an update tick.
        let show_update = match self.total.filter(|&total| total != 0) {
            Some(total) => {
                let percent = ((n as u64 * 100) / total as u64) as u32;
                if self.last_percent != Some(percent) || update {
                    self.last_percent = Some(percent);
                    self.counters = format!("{percent:>3}% ({n}/{total}){tp}");
                    true
                } else {
                    false
                }
            }
            None if update => {
                self.counters = format!("{n}{tp}");
                true
            }
            None => false,
        };
        if !show_update || !(done.is_some() || is_foreground_stderr()) {
            return;
        }
        let eol = done.unwrap_or("\r");
        let clear_len = if self.counters.len() < last_count_len {
            last_count_len - self.counters.len() + 1
        } else {
            0
        };
        // The "+ 2" accounts for the ": ".
        let title_len = self.title.len();
        let progress_line_len = title_len + self.counters.len() + 2;
        let cols = usize::try_from(crate::pager::term_columns()).unwrap_or(80);
        let line = if self.split {
            format!("  {}{eol:>clear_len$}", self.counters)
        } else if done.is_none() && cols < progress_line_len {
            let clear_len = if title_len + 1 < cols { cols - title_len - 1 } else { 0 };
            self.split = true;
            format!("{}:{:clear_len$}\n  {}{eol}", self.title, "", self.counters)
        } else {
            format!("{}: {}{eol:>clear_len$}", self.title, self.counters)
        };
        write_stderr(line.as_bytes());
    }
}

/// `is_foreground_fd(fileno(stderr))` (`progress.c:106-110`): a background job
/// keeps quiet until its closing line. A relay is a pipe, where `tcgetpgrp()`
/// fails and the answer is yes.
fn is_foreground_stderr() -> bool {
    if relaying() {
        return true;
    }
    // SAFETY: plain queries on this process's own descriptor and group.
    unsafe {
        let tpgrp = libc::tcgetpgrp(libc::STDERR_FILENO);
        tpgrp < 0 || tpgrp == libc::getpgid(0)
    }
}

/// `get_default_delay()` (`progress.c:281-289`): `GIT_PROGRESS_DELAY` read once
/// through `git_env_ulong()` (`parse.c:214-220`), one tick when unset. The value
/// lands in an `int` and then the `unsigned` delay, which keeps its low 32 bits.
fn default_delay() -> Result<u32, DelayError> {
    static DELAY: std::sync::OnceLock<Result<u32, DelayError>> = std::sync::OnceLock::new();
    *DELAY.get_or_init(|| match std::env::var("GIT_PROGRESS_DELAY") {
        Err(std::env::VarError::NotPresent) => Ok(1),
        Ok(v) => crate::config::parse_config_ulong(&v).map(|n| n as u32).map_err(|_| DelayError),
        Err(std::env::VarError::NotUnicode(_)) => Err(DelayError),
    })
}

/// `git_env_ulong()`'s `die(_("failed to parse %s"), "GIT_PROGRESS_DELAY")`.
#[derive(Clone, Copy, Debug)]
pub struct DelayError;

impl std::fmt::Display for DelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to parse GIT_PROGRESS_DELAY")
    }
}

impl std::error::Error for DelayError {}

/// An interval in 1024ths of a second, as `progress.c:234` computes it.
fn misecs(elapsed: Duration) -> u32 {
    ((elapsed.as_nanos() as u64).wrapping_mul(4398) >> 32) as u32
}

/// `throughput_string()` (`progress.c:175-183`).
fn throughput_string(total: u64, rate: u32) -> String {
    format!(", {} | {}", humanise(total, false), humanise(u64::from(rate) * 1024, true))
}

/// `humanise_bytes()` (`strbuf.c:875-909`) without `HUMANISE_COMPACT`: git's
/// truncating fractions, its rounding nudges and its `>` unit boundaries, with
/// `rate` selecting the `/s` units `strbuf_humanise_rate()` asks for.
fn humanise(bytes: u64, rate: bool) -> String {
    let per = if rate { "/s" } else { "" };
    if bytes > 1 << 30 {
        let frac = (bytes & ((1 << 30) - 1)) / 10_737_419;
        format!("{}.{frac:02} GiB{per}", bytes >> 30)
    } else if bytes > 1 << 20 {
        let x = bytes + 5243;
        format!("{}.{:02} MiB{per}", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20)
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        format!("{}.{:02} KiB{per}", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10)
    } else if bytes == 1 {
        format!("1 byte{per}")
    } else {
        format!("{bytes} bytes{per}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stock 2.55.0 closed a 591-byte bundle pack with `591 bytes | 591.00
    /// KiB/s`: written inside one 1024th of a second, the rate is the total.
    #[test]
    fn throughput_renders_as_git_writes_it() {
        assert_eq!(humanise(591, false), "591 bytes");
        assert_eq!(humanise(591 * 1024, true), "591.00 KiB/s");
        assert_eq!(humanise(1, true), "1 byte/s");
        assert_eq!(humanise(1 << 20, false), "1024.00 KiB");
    }

    /// A disabled meter is inert, which is what lets every call site drive one
    /// without asking whether progress is on.
    #[test]
    fn a_disabled_meter_counts_without_drawing() {
        let mut m = Meter::counted("Counting objects", 4, false);
        m.tick();
        assert_eq!(m.current, 1);
        assert_eq!(m.last_percent, None, "nothing was drawn, so nothing was recorded");
    }

    /// The redraw cadence: one per whole-number percentage, not one per object.
    #[test]
    fn a_bounded_meter_redraws_once_per_percent() {
        let mut m = Meter::counted("Writing objects", 1000, true);
        for _ in 0..5 {
            m.tick();
        }
        assert_eq!(m.percent(), Some(0), "5 of 1000 has not reached one percent");
        assert_eq!(m.last_percent, Some(0), "the first draw records zero percent");
        for _ in 5..15 {
            m.tick();
        }
        assert_eq!(m.last_percent, Some(1), "crossing one percent redraws exactly once");
    }

    /// `if (progress->delay && (!update || --progress->delay)) return;`: a delayed
    /// meter records nothing until as many update ticks as its delay have fired,
    /// which is what keeps a fast phase — and its closing line — silent.
    #[test]
    fn a_delayed_meter_waits_out_its_ticks() {
        let mut m = Meter::start("Checking objects", Some(4), true);
        m.delay = 2;
        m.set(1);
        assert_eq!((m.last_value, m.delay), (None, 2), "no tick has fired");
        m.next_update = Instant::now();
        m.set(2);
        assert_eq!((m.last_value, m.delay), (None, 1), "the first tick only counts down");
        m.next_update = Instant::now();
        m.set(3);
        assert_eq!((m.last_value, m.delay), (Some(3), 0), "the second tick draws");
    }

    /// An empty phase is complete, not a division by zero.
    #[test]
    fn an_empty_phase_reads_as_complete() {
        assert_eq!(Meter::counted("Counting objects", 0, true).percent(), Some(100));
    }

    /// The first byte count only starts the clock: git's `display_throughput()`
    /// allocates the structure and returns, so a phase that reported bytes once
    /// has a rate on its closing line and none on the redraws before it.
    #[test]
    fn the_first_throughput_report_starts_the_clock() {
        let mut m = Meter::counted("Receiving objects", 3, true);
        m.throughput(403);
        let tp = m.throughput.as_ref().expect("the first report allocates");
        assert_eq!((tp.curr_total, tp.display.as_str()), (403, ""));
    }

    /// `stop_progress_msg()` skips `force_last_update()` for a meter whose
    /// `last_value` is still `-1`: an empty `Receiving objects` phase prints
    /// nothing at all.
    #[test]
    fn a_never_displayed_meter_stops_silently() {
        let m = Meter::counted("Receiving objects", 0, true);
        assert_eq!(m.last_value, None);
        m.stop("done");
    }
}
