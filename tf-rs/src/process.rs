//! Scheduled process management: `/repeat` and `/quote`.
//!
//! Corresponds to `process.c` in the C source.
//!
//! [`ProcessScheduler`] maintains a list of active [`Proc`]s and knows when
//! the next one is due (for use as a `tokio::time::sleep_until` deadline).
//! The event loop calls [`ProcessScheduler::take_ready`] each tick to collect
//! processes whose timers have fired, executes their bodies, then calls
//! [`ProcessScheduler::reschedule`] for those that have remaining runs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

// ── Process kind ──────────────────────────────────────────────────────────

/// What a scheduled process does when it fires.
#[derive(Debug, Clone)]
pub enum ProcKind {
    /// `/repeat` — re-run a TF script body on a fixed interval.
    Repeat {
        /// The TF script body to execute each tick.
        body: String,
    },
    /// `/quote 'file` — feed lines from a local file to the world.
    QuoteFile {
        path: PathBuf,
        /// Byte offset of the next unread line.
        pos: u64,
    },
    /// `/quote !cmd` — feed stdout of a shell command to the world.
    QuoteShell {
        command: String,
    },
}

// ── Proc ──────────────────────────────────────────────────────────────────

/// A single scheduled process.
#[derive(Debug, Clone)]
pub struct Proc {
    /// Monotonically increasing process ID (not an OS PID).
    pub id: u32,
    /// What to do when the timer fires.
    pub kind: ProcKind,
    /// Target world name (None = active world).
    pub world: Option<String>,
    /// Time between runs.
    pub interval: Duration,
    /// When this process should next fire.
    pub next_run: Instant,
    /// Remaining runs; `None` means run forever.
    pub runs_left: Option<u32>,
}

impl Proc {
    /// Whether this process should be rescheduled after firing.
    pub fn has_more_runs(&self) -> bool {
        self.runs_left.is_none_or(|n| n > 1)
    }

    /// Consume one run and update `next_run` to the next interval.
    ///
    /// Returns `true` if the process should continue (was rescheduled).
    pub fn tick(&mut self) -> bool {
        if let Some(ref mut n) = self.runs_left {
            *n -= 1;
            if *n == 0 {
                return false;
            }
        }
        // Schedule from *when it was supposed to run* (not now) to prevent
        // drift: mirrors TF's PTIME_VAR behaviour.
        self.next_run += self.interval;
        true
    }
}

// ── ProcessScheduler ──────────────────────────────────────────────────────

/// Manages all active scheduled processes.
///
/// Designed to integrate with a `tokio::select!` loop:
///
/// ```rust,ignore
/// # use tf::process::ProcessScheduler;
/// # use std::time::Instant;
/// # use tokio::time::sleep_until;
/// # let mut sched = ProcessScheduler::new();
/// loop {
///     if let Some(deadline) = sched.next_wakeup() {
///         sleep_until(deadline.into()).await;
///     }
///     let ready = sched.take_ready(Instant::now());
///     for mut proc in ready {
///         // … execute proc.kind …
///         sched.reschedule(proc);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ProcessScheduler {
    procs: Vec<Proc>,
    next_id: u32,
}

impl Default for ProcessScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessScheduler {
    pub fn new() -> Self {
        Self { procs: Vec::new(), next_id: 1 }
    }

    /// Schedule a new `/repeat` process.
    ///
    /// Returns the assigned process ID.
    pub fn add_repeat(
        &mut self,
        body: String,
        interval: Duration,
        count: Option<u32>,
        world: Option<String>,
    ) -> u32 {
        self.add(ProcKind::Repeat { body }, interval, count, world)
    }

    /// Schedule a new `/quote 'file` process.
    pub fn add_quote_file(
        &mut self,
        path: PathBuf,
        interval: Duration,
        count: Option<u32>,
        world: Option<String>,
    ) -> u32 {
        self.add(ProcKind::QuoteFile { path, pos: 0 }, interval, count, world)
    }

    /// Schedule a new `/quote !cmd` process.
    pub fn add_quote_shell(
        &mut self,
        command: String,
        interval: Duration,
        count: Option<u32>,
        world: Option<String>,
    ) -> u32 {
        self.add(ProcKind::QuoteShell { command }, interval, count, world)
    }

    fn add(&mut self, kind: ProcKind, interval: Duration, count: Option<u32>, world: Option<String>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.procs.push(Proc {
            id,
            kind,
            world,
            interval,
            next_run: Instant::now() + interval,
            runs_left: count,
        });
        id
    }

    /// Remove a process by ID.  Returns `true` if found.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.procs.len();
        self.procs.retain(|p| p.id != id);
        self.procs.len() < before
    }

    /// Kill all scheduled processes.
    pub fn kill_all(&mut self) {
        self.procs.clear();
    }

    /// Return the soonest `next_run` across all processes, or `None` if empty.
    ///
    /// Use this as the `sleep_until` deadline in the event loop.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.procs.iter().map(|p| p.next_run).min()
    }

    /// Remove and return all processes whose `next_run <= now`.
    ///
    /// Call [`Self::reschedule`] for each returned process that should
    /// continue running (after executing it).
    pub fn take_ready(&mut self, now: Instant) -> Vec<Proc> {
        let (ready, pending): (Vec<Proc>, Vec<Proc>) =
            self.procs.drain(..).partition(|p| p.next_run <= now);
        self.procs = pending;
        ready
    }

    /// Put a process back into the scheduler after executing it.
    ///
    /// Call [`Proc::tick`] first to update `next_run` and `runs_left`;
    /// if `tick` returns `false` (no more runs), do not reschedule.
    pub fn reschedule(&mut self, proc: Proc) {
        self.procs.push(proc);
    }

    /// Number of active processes.
    pub fn len(&self) -> usize {
        self.procs.len()
    }

    /// Whether there are no scheduled processes.
    pub fn is_empty(&self) -> bool {
        self.procs.is_empty()
    }

    /// Iterate over all processes (for display / `/list`).
    pub fn iter(&self) -> impl Iterator<Item = &Proc> {
        self.procs.iter()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn add_and_remove() {
        let mut s = ProcessScheduler::new();
        let id = s.add_repeat("echo hi".into(), ms(100), Some(3), None);
        assert_eq!(s.len(), 1);
        assert!(s.remove(id));
        assert!(s.is_empty());
    }

    #[test]
    fn remove_unknown_id_returns_false() {
        let mut s = ProcessScheduler::new();
        assert!(!s.remove(99));
    }

    #[test]
    fn next_wakeup_none_when_empty() {
        let s = ProcessScheduler::new();
        assert!(s.next_wakeup().is_none());
    }

    #[test]
    fn next_wakeup_returns_soonest() {
        let mut s = ProcessScheduler::new();
        // First process fires in 200ms, second in 50ms.
        s.add_repeat("a".into(), ms(200), None, None);
        s.add_repeat("b".into(), ms(50), None, None);
        // next_wakeup must be <= now + 200ms and > now.
        let wake = s.next_wakeup().unwrap();
        let now = Instant::now();
        assert!(wake > now);
        assert!(wake <= now + ms(200));
    }

    #[test]
    fn take_ready_only_returns_due_processes() {
        let mut s = ProcessScheduler::new();
        let now = Instant::now();

        // Manually insert a process that is already past due.
        s.procs.push(Proc {
            id: 1,
            kind: ProcKind::Repeat { body: "past".into() },
            world: None,
            interval: ms(100),
            next_run: now - ms(1), // already overdue
            runs_left: None,
        });
        // And one that's not due yet.
        s.procs.push(Proc {
            id: 2,
            kind: ProcKind::Repeat { body: "future".into() },
            world: None,
            interval: ms(100),
            next_run: now + ms(1000),
            runs_left: None,
        });

        let ready = s.take_ready(now);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, 1);
        assert_eq!(s.len(), 1); // future proc still there
    }

    #[test]
    fn reschedule_puts_process_back() {
        let mut s = ProcessScheduler::new();
        let now = Instant::now();
        s.procs.push(Proc {
            id: 1,
            kind: ProcKind::Repeat { body: "x".into() },
            world: None,
            interval: ms(50),
            next_run: now - ms(1),
            runs_left: Some(3),
        });
        let mut ready = s.take_ready(now);
        assert_eq!(ready.len(), 1);
        let mut p = ready.remove(0);
        let keep = p.tick();
        assert!(keep);
        assert_eq!(p.runs_left, Some(2));
        s.reschedule(p);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn tick_decrements_finite_runs() {
        let mut p = Proc {
            id: 1,
            kind: ProcKind::Repeat { body: "y".into() },
            world: None,
            interval: ms(10),
            next_run: Instant::now(),
            runs_left: Some(2),
        };
        assert!(p.tick());   // 2 → 1, keep going
        assert!(!p.tick());  // 1 → 0, done
    }

    #[test]
    fn tick_infinite_runs_never_stops() {
        let mut p = Proc {
            id: 1,
            kind: ProcKind::Repeat { body: "z".into() },
            world: None,
            interval: ms(10),
            next_run: Instant::now(),
            runs_left: None,
        };
        for _ in 0..1000 {
            assert!(p.tick());
        }
    }

    #[test]
    fn kill_all_clears_all_processes() {
        let mut s = ProcessScheduler::new();
        s.add_repeat("a".into(), ms(100), None, None);
        s.add_repeat("b".into(), ms(200), None, None);
        s.kill_all();
        assert!(s.is_empty());
    }
}
