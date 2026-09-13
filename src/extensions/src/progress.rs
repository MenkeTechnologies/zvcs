//! git's progress meter, as `pack-objects`, `repack` and `gc` write it.
//!
//! Every phase of a pack write reports through [`Meter`], which reproduces
//! `progress.c`'s framing byte for byte (checked against git 2.55.0 driven from a
//! pseudo-terminal):
//!
//! ```text
//!   Enumerating objects: 9, done.\n          an unbounded count
//!   Counting objects:  11% (1/9)\r           a bounded one, redrawn in place
//!   Counting objects: 100% (9/9), done.\n    …and its closing line
//! ```
//!
//! Each redraw ends in a carriage return so the next one overwrites it; only the
//! closing line ends in a newline, which the terminal's own `onlcr` renders as
//! the `\r\n` a capture of git shows. A bounded meter redraws when its whole-number
//! percentage changes, which is the cadence git's own output shows, and the
//! percentage is right-aligned in three columns (`  1%`, ` 50%`, `100%`).
//!
//! Everything goes to stderr, and only when stderr is a terminal: git's
//! `start_progress()` is reached with progress enabled just when `isatty(2)` and
//! `--quiet` was not given, so a piped `gc` prints nothing at all. [`enabled`]
//! answers that question once so every caller asks it the same way.

use std::io::{IsTerminal, Write};

/// Whether a pack-writing command should report progress: git's rule is a
/// terminal on stderr and no `--quiet`.
pub fn enabled(quiet: bool) -> bool {
    !quiet && std::io::stderr().is_terminal()
}

/// One phase of a pack write.
///
/// A disabled meter writes nothing, so callers can drive it unconditionally.
pub struct Meter {
    title: &'static str,
    /// The phase's final count, if it is known up front. Without one the meter
    /// prints a bare running total, as git does while it is still enumerating.
    total: Option<usize>,
    current: usize,
    /// The last percentage drawn, so a bounded meter redraws only when the
    /// whole number changes rather than once per object.
    last_percent: Option<u32>,
    /// When an unbounded meter last redrew, which is all that paces one.
    last_draw: Option<std::time::Instant>,
    /// `progress->start_ns`, which the closing throughput average is taken over.
    started: std::time::Instant,
    /// The throughput text `display()` appends after the counters (`tp` in
    /// `progress.c:126`); empty until a closing rate has been computed.
    suffix: String,
    on: bool,
}

/// How often an unbounded meter redraws, matching the interval git's
/// `progress.c` arms its `SIGALRM` for.
const REDRAW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

impl Meter {
    /// A phase whose size is not known yet — git's `Enumerating objects`.
    pub fn unknown(title: &'static str, on: bool) -> Self {
        Meter {
            title,
            total: None,
            current: 0,
            last_percent: None,
            // Set now, so the first redraw waits out the interval as git does.
            last_draw: Some(std::time::Instant::now()),
            started: std::time::Instant::now(),
            suffix: String::new(),
            on,
        }
    }

    /// A phase of `total` items — git's `Counting`, `Compressing` and `Writing`.
    pub fn counted(title: &'static str, total: usize, on: bool) -> Self {
        Meter {
            title,
            total: Some(total),
            current: 0,
            last_percent: None,
            last_draw: None,
            started: std::time::Instant::now(),
            suffix: String::new(),
            on,
        }
    }

    /// Count one item and redraw if that changed what the line would say.
    pub fn tick(&mut self) {
        self.advance(1);
    }

    /// Count `n` items at once, for a phase that reports in batches.
    pub fn advance(&mut self, n: usize) {
        self.current += n;
        if !self.on {
            return;
        }
        match self.percent() {
            // Bounded: redraw only when the whole-number percentage moves.
            Some(percent) => {
                if self.last_percent != Some(percent) {
                    self.last_percent = Some(percent);
                    self.draw(false);
                }
            }
            // Unbounded: there is no percentage to move, so redraw on the clock.
            // git's `progress.c` arms a one-second `SIGALRM` for exactly this,
            // which is why a short enumeration prints its closing line only.
            None => {
                if self.last_draw.is_none_or(|at| at.elapsed() >= REDRAW_INTERVAL) {
                    self.last_draw = Some(std::time::Instant::now());
                    self.draw(false);
                }
            }
        }
    }

    /// Close the phase with git's `, done.` line. A phase that never ticked
    /// still prints, which is what git does for an empty pack.
    pub fn done(mut self) {
        if self.on {
            self.draw(true);
        }
    }

    /// Close a phase whose output went through a throughput-counting hashfile:
    /// `pack-objects --stdout`'s `Writing objects`, which `hashfd_ext()` hands
    /// the progress (`builtin/pack-objects.c:1350-1363`) so every flush reports
    /// the bytes written.
    ///
    /// `force_last_update()` (`progress.c:332-348`) replaces the running figure
    /// with the whole-phase average: the elapsed time since `start_progress()` in
    /// 1024ths of a second, floored at one, divided into the total. The redraws
    /// before it carry no rate — `display_throughput()` only fills the display
    /// after half a second has passed (`progress.c:214-216`) — so only the
    /// closing line has one.
    pub fn done_with_throughput(mut self, total_bytes: u64) {
        if !self.on {
            return;
        }
        let elapsed_ns = self.started.elapsed().as_nanos() as u64;
        let misecs = ((elapsed_ns.wrapping_mul(4398)) >> 32) as u32;
        let rate = (total_bytes / u64::from(misecs.max(1))) as u32;
        // `throughput_string()` (`progress.c:175-183`).
        self.suffix = format!(
            ", {} | {}",
            humanise(total_bytes, false),
            humanise(u64::from(rate) * 1024, true)
        );
        self.draw(true);
    }

    /// `100 * current / total`, or `None` when the total is unknown. A total of
    /// zero reads as complete, matching git's `display()`.
    fn percent(&self) -> Option<u32> {
        match self.total {
            Some(0) => Some(100),
            Some(total) => Some(((self.current as u64 * 100) / total as u64) as u32),
            None => None,
        }
    }

    /// One redraw. A line that will be overwritten ends in a carriage return and
    /// nothing else; the closing line ends in a newline, which the terminal's
    /// own `onlcr` turns into the `\r\n` a capture of git shows.
    fn draw(&mut self, done: bool) {
        let tail = if done { ", done.\n" } else { "\r" };
        let mut err = std::io::stderr().lock();
        let _ = match self.total {
            Some(total) => write!(
                err,
                "{}: {:>3}% ({}/{}){}{tail}",
                self.title,
                self.percent().unwrap_or(100),
                self.current,
                total,
                self.suffix
            ),
            None => write!(err, "{}: {}{tail}", self.title, self.current),
        };
        let _ = err.flush();
    }
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

    /// An empty phase is complete, not a division by zero.
    #[test]
    fn an_empty_phase_reads_as_complete() {
        assert_eq!(Meter::counted("Counting objects", 0, true).percent(), Some(100));
    }
}
