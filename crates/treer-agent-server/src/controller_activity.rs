//! Working/Idle/Blocked filter for terminal workloads whose status no Agent
//! Interface owns (`state.observe`).
//!
//! Raw PTY output is a poor activity signal: cursor blinks, clocks, spinners,
//! prompt re-renders and TUI redraws all write bytes while nothing is being
//! done. [`TerminalActivity`] is a small deterministic state machine with
//! hysteresis that turns output chunks and clock ticks into a stable status.
//! Time is always passed in, so tests drive it with a fake clock.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use treer_protocol::AgentStatus;

/// Window over which meaningful updates are counted before entering Working.
pub(super) const ACTIVITY_WINDOW: Duration = Duration::from_secs(3);
/// Meaningful updates within [`ACTIVITY_WINDOW`] that count as sustained work.
pub(super) const ACTIVITY_ENTER_UPDATES: usize = 3;
/// Working/Starting falls back to Idle after this long without meaningful
/// output or a live working marker.
pub(super) const QUIET_IDLE_AFTER: Duration = Duration::from_millis(4500);
/// A shell prompt at the end of output ends Working early, once the last
/// meaningful output is at least this old.
pub(super) const PROMPT_IDLE_GRACE: Duration = Duration::from_secs(1);
/// Minimum time a status is held, so emitted changes are at most this often.
/// Entering Blocked and marker-driven Working bypass it.
pub(super) const MIN_STATUS_DWELL: Duration = Duration::from_secs(2);
/// A working marker in the tail stays live this long after the last output
/// chunk with visible content (spinners redraw well within it).
pub(super) const WORKING_MARKER_TTL: Duration = Duration::from_secs(3);
/// A chunk without a line feed is an in-place repaint (clock, status line,
/// spinner, keystroke echo) unless it carries at least this many visible
/// characters (a full-screen cursor-addressed redraw).
pub(super) const INLINE_MEANINGFUL_CHARS: usize = 256;
/// Recently seen chunk contents; repeating one of them is a redraw.
pub(super) const REDRAW_MEMORY: usize = 8;
/// Tail lines searched for a blocking question or a working marker.
pub(super) const STATUS_TAIL_LINES: usize = 12;
/// Byte bound on that tail.
pub(super) const STATUS_TAIL_BYTES: usize = 4 * 1024;
/// Output this soon after terminal input is treated as its echo: typing is
/// not work, even when a TUI redraws its whole input box per keystroke.
pub(super) const INPUT_ECHO_WINDOW: Duration = Duration::from_millis(500);
/// Bytes before a chunk also searched for a marker split across chunks.
const MARKER_CARRY_BYTES: usize = 64;

/// Questions a TUI draws in a dialog; they hold until redrawn away.
const BLOCKED_PATTERNS: [&str; 6] = [
    "allow command?",
    "do you want to proceed",
    "press enter to confirm",
    "enter to submit answer",
    "[y/n]",
    "action required",
];
/// Line-mode shell questions; a later shell prompt means they were answered.
const SHELL_QUESTIONS: [&str; 1] = ["[y/n]"];
const WORKING_MARKERS: [&str; 4] = [
    "esc to interrupt",
    "working (",
    "thinking (",
    "running tool",
];

/// What the recent plain-text tail shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TailSignals {
    /// A blocking question is drawn after any working marker and is not
    /// followed by a fresh shell prompt.
    pub blocked: bool,
    /// A working marker is drawn and no later blocking question overrides it.
    pub marker: bool,
    /// The last line ends in a prompt character (`❯ › $ #`).
    pub prompt: bool,
}

/// Offset just past the last occurrence of any pattern in `haystack`.
fn last_match(haystack: &str, patterns: &[&str]) -> Option<usize> {
    patterns
        .iter()
        .filter_map(|pattern| haystack.rfind(pattern).map(|index| index + pattern.len()))
        .max()
}

/// Start of the last `lines` lines of `text`, bounded to `limit` bytes.
fn tail(text: &str, lines: usize, limit: usize) -> &str {
    let text = super::recent_text(text, limit);
    let trimmed = text.trim_end_matches('\n');
    let mut start = 0;
    for (count, (index, _)) in trimmed.rmatch_indices('\n').enumerate() {
        if count + 1 >= lines {
            start = index + 1;
            break;
        }
    }
    &text[start..]
}

pub(super) fn tail_signals(text: &str) -> TailSignals {
    let last_line = super::recent_text(text, STATUS_TAIL_BYTES)
        .lines()
        .next_back()
        .unwrap_or_default();
    let trimmed = last_line.trim_end();
    let prompt = trimmed.ends_with('❯')
        || trimmed.ends_with('›')
        || trimmed.ends_with('$')
        || trimmed.ends_with('#');
    let tail = tail(text, STATUS_TAIL_LINES, STATUS_TAIL_BYTES).to_lowercase();
    let blocked_at = last_match(&tail, &BLOCKED_PATTERNS);
    let marker_at = last_match(&tail, &WORKING_MARKERS);
    // TUI selection glyphs (`❯ 1. Yes`) can end a line too, so only a
    // line-mode question gives way to a prompt.
    let prompt_after_question = prompt
        && blocked_at.is_some()
        && last_match(&tail, &SHELL_QUESTIONS) == blocked_at
        && !last_line.to_lowercase().contains("[y/n]");
    let blocked = match (blocked_at, marker_at) {
        (Some(blocked), Some(marker)) => blocked >= marker,
        (Some(_), None) => true,
        _ => false,
    } && !prompt_after_question;
    TailSignals {
        blocked,
        marker: marker_at.is_some() && !blocked,
        prompt,
    }
}

/// Visible content of a chunk: printable, non-whitespace characters.
fn visible(chunk: &str) -> String {
    chunk
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect()
}

#[derive(Debug)]
pub(super) struct TerminalActivity {
    status: AgentStatus,
    since: Instant,
    /// Meaningful updates inside [`ACTIVITY_WINDOW`].
    updates: VecDeque<Instant>,
    last_meaningful: Option<Instant>,
    /// A working marker is on screen: set when a chunk draws one, cleared
    /// when lines are rendered and the tail no longer shows one. Cell-diff
    /// repaints (no line feed) keep it, so a long spinner does not age out.
    marker_drawn: bool,
    /// Last non-echo chunk with visible content while a marker was drawn.
    last_marker: Option<Instant>,
    last_input: Option<Instant>,
    redraws: VecDeque<u64>,
    signals: TailSignals,
}

impl TerminalActivity {
    pub(super) fn new(status: AgentStatus, now: Instant) -> Self {
        Self {
            status,
            since: now,
            updates: VecDeque::new(),
            last_meaningful: None,
            marker_drawn: false,
            last_marker: None,
            last_input: None,
            redraws: VecDeque::new(),
            signals: TailSignals::default(),
        }
    }

    /// Follow a status somebody else set (spawn, restart, bridge queue,
    /// a departed Agent Interface) and restart its dwell.
    fn adopt(&mut self, current: AgentStatus, now: Instant) {
        if current == self.status {
            return;
        }
        if current == AgentStatus::Starting {
            *self = Self::new(current, now);
        } else {
            self.status = current;
            self.since = now;
        }
    }

    /// Record terminal input written by a user or Agent.
    pub(super) fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// Record one output chunk (already ANSI-stripped) and the whole retained
    /// plain text after appending it; returns the filtered status.
    pub(super) fn observe(
        &mut self,
        now: Instant,
        current: AgentStatus,
        chunk: &str,
        text: &str,
    ) -> AgentStatus {
        self.adopt(current, now);
        self.signals = tail_signals(text);
        let window = super::recent_text(text, chunk.len() + MARKER_CARRY_BYTES).to_lowercase();
        if self.signals.blocked {
            self.marker_drawn = false;
        } else if last_match(&window, &WORKING_MARKERS).is_some() {
            self.marker_drawn = true;
        } else if chunk.contains('\n') {
            self.marker_drawn = self.signals.marker;
        }
        let content = visible(chunk);
        let echo = self
            .last_input
            .is_some_and(|at| now.duration_since(at) < INPUT_ECHO_WINDOW);
        if !content.is_empty() && !echo {
            if self.marker_drawn {
                self.last_marker = Some(now);
            }
            let mut hasher = DefaultHasher::new();
            content.hash(&mut hasher);
            let hash = hasher.finish();
            let redraw = self.redraws.contains(&hash);
            self.redraws.retain(|seen| *seen != hash);
            self.redraws.push_back(hash);
            if self.redraws.len() > REDRAW_MEMORY {
                self.redraws.pop_front();
            }
            let advances =
                chunk.contains('\n') || content.chars().count() >= INLINE_MEANINGFUL_CHARS;
            if advances && !redraw {
                self.last_meaningful = Some(now);
                self.updates.push_back(now);
            }
        }
        self.decide(now)
    }

    /// Re-evaluate without new output; returns the filtered status.
    pub(super) fn tick(&mut self, now: Instant, current: AgentStatus) -> AgentStatus {
        self.adopt(current, now);
        self.decide(now)
    }

    fn sustained(&mut self, now: Instant) -> bool {
        while self
            .updates
            .front()
            .is_some_and(|at| now.duration_since(*at) > ACTIVITY_WINDOW)
        {
            self.updates.pop_front();
        }
        self.updates.len() >= ACTIVITY_ENTER_UPDATES
    }

    fn quiet(&self, now: Instant) -> Duration {
        let last = [self.last_meaningful, self.last_marker]
            .into_iter()
            .flatten()
            .chain([self.since])
            .max()
            .unwrap_or(self.since);
        now.saturating_duration_since(last)
    }

    fn decide(&mut self, now: Instant) -> AgentStatus {
        if self.status.is_terminal() {
            return self.status;
        }
        let marker_live = self.marker_drawn
            && self
                .last_marker
                .is_some_and(|at| now.duration_since(at) < WORKING_MARKER_TTL);
        let quiet = self.quiet(now);
        let settled =
            quiet >= QUIET_IDLE_AFTER || (self.signals.prompt && quiet >= PROMPT_IDLE_GRACE);
        let desired = if self.signals.blocked {
            AgentStatus::Blocked
        } else if marker_live || self.sustained(now) {
            AgentStatus::Working
        } else {
            match self.status {
                AgentStatus::Working | AgentStatus::Starting if !settled => self.status,
                AgentStatus::Working | AgentStatus::Starting | AgentStatus::Blocked => {
                    AgentStatus::Idle
                }
                other => other,
            }
        };
        if desired == self.status {
            return desired;
        }
        let immediate =
            desired == AgentStatus::Blocked || (desired == AgentStatus::Working && marker_live);
        if immediate || now.duration_since(self.since) >= MIN_STATUS_DWELL {
            self.status = desired;
            self.since = now;
        }
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(200);

    /// Fake-clock driver that records every status change.
    struct Rig {
        activity: TerminalActivity,
        now: Instant,
        text: String,
        status: AgentStatus,
        changes: Vec<(Duration, AgentStatus)>,
        start: Instant,
    }

    impl Rig {
        fn new(status: AgentStatus) -> Self {
            let start = Instant::now();
            Self {
                activity: TerminalActivity::new(status, start),
                now: start,
                text: String::new(),
                status,
                changes: Vec::new(),
                start,
            }
        }

        fn record(&mut self, status: AgentStatus) {
            if status != self.status {
                self.changes.push((self.now - self.start, status));
                self.status = status;
            }
        }

        fn output(&mut self, raw: &str) {
            let plain = String::from_utf8_lossy(&strip_ansi_escapes::strip(raw)).into_owned();
            self.text.push_str(&plain);
            let status = self
                .activity
                .observe(self.now, self.status, &plain, &self.text);
            self.record(status);
        }

        /// Advance the fake clock, ticking like the Controller monitor.
        fn wait(&mut self, duration: Duration) {
            let end = self.now + duration;
            while self.now < end {
                self.now += TICK.min(end - self.now);
                let status = self.activity.tick(self.now, self.status);
                self.record(status);
            }
        }

        /// Repeat `chunk(i)` every `every` for `total`.
        fn repeat(&mut self, every: Duration, total: Duration, chunk: impl Fn(usize) -> String) {
            let mut elapsed = Duration::ZERO;
            let mut index = 0;
            while elapsed < total {
                self.output(&chunk(index));
                self.wait(every);
                elapsed += every;
                index += 1;
            }
        }

        fn idle_shell(&mut self) {
            self.output("user@host:~$ ");
            self.wait(Duration::from_secs(3));
            assert_eq!(self.status, AgentStatus::Idle);
            self.changes.clear();
        }
    }

    #[test]
    fn blinking_cursor_stays_idle() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        rig.repeat(Duration::from_millis(500), Duration::from_secs(20), |i| {
            if i % 2 == 0 { "\x1b[?25l" } else { "\x1b[?25h" }.into()
        });
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn clock_repainting_at_idle_prompt_stays_idle() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        rig.repeat(Duration::from_secs(1), Duration::from_secs(20), |i| {
            format!("\r\x1b[2K12:00:{i:02} | cpu 12% | mem 40% | load 0.{i:02}")
        });
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn long_build_stays_working_without_dips() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        let build_start = rig.now - rig.start;
        rig.output("make\r\n");
        rig.repeat(Duration::from_millis(300), Duration::from_secs(30), |i| {
            format!("   Compiling crate-{i} v0.1.0\r\n")
        });
        // A two-second link pause with no output does not dip.
        rig.wait(Duration::from_secs(2));
        rig.output("    Finished dev profile\r\nuser@host:~$ ");
        rig.wait(Duration::from_secs(3));
        let statuses: Vec<_> = rig.changes.iter().map(|(_, status)| *status).collect();
        assert_eq!(statuses, vec![AgentStatus::Working, AgentStatus::Idle]);
        // Working begins once the updates are sustained and the Idle dwell ends.
        assert!(rig.changes[0].0 - build_start <= MIN_STATUS_DWELL);
    }

    #[test]
    fn spinner_with_interrupt_marker_stays_working() {
        let mut rig = Rig::new(AgentStatus::Idle);
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴'];
        rig.output(
            "\n> fix the bug\n\n⠋ Thinking… (0s · esc to interrupt)\n\n╭────╮\n│ >  │\n╰────╯\n",
        );
        assert_eq!(rig.status, AgentStatus::Working, "marker switches at once");
        // Cell-diff renderers only repaint the glyph and the timer.
        rig.repeat(Duration::from_millis(120), Duration::from_secs(20), |i| {
            format!("\x1b[4;1H{}\x1b[4;14H{}s", frames[i % frames.len()], i / 8)
        });
        assert_eq!(rig.changes, vec![(Duration::ZERO, AgentStatus::Working)]);
    }

    #[test]
    fn tui_idle_redraws_stay_idle() {
        let mut rig = Rig::new(AgentStatus::Starting);
        let screen = "╭──────────────╮\n│ >            │\n╰──────────────╯\n  ? for shortcuts\n";
        rig.output(screen);
        rig.wait(Duration::from_secs(6));
        assert_eq!(rig.status, AgentStatus::Idle);
        rig.changes.clear();
        // Full identical redraws, focus repaints and a cursor blink.
        rig.repeat(
            Duration::from_millis(700),
            Duration::from_secs(20),
            |i| match i % 3 {
                0 => format!("\x1b[4A\x1b[J{screen}"),
                1 => "\x1b[?25l\x1b[?25h".into(),
                _ => "\x1b[2;4H".into(),
            },
        );
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn stale_marker_after_turn_does_not_flap_on_redraw() {
        let mut rig = Rig::new(AgentStatus::Idle);
        rig.output("✻ Working (3s · esc to interrupt)\n");
        assert_eq!(rig.status, AgentStatus::Working);
        rig.output("\x1b[1A\x1b[2K");
        let reply: String = (0..20).map(|i| format!("answer line {i}\n")).collect();
        rig.output(&reply);
        rig.wait(Duration::from_secs(6));
        assert_eq!(rig.status, AgentStatus::Idle);
        rig.changes.clear();
        rig.repeat(Duration::from_secs(1), Duration::from_secs(15), |_| {
            "\x1b[3A\x1b[J╭──╮\n│> │\n╰──╯\n".into()
        });
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn blocked_prompt_is_immediate_and_held() {
        let mut rig = Rig::new(AgentStatus::Idle);
        rig.output("✻ Working (3s · esc to interrupt)\n");
        rig.output("Bash command\n  rm -rf target\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n");
        assert_eq!(rig.status, AgentStatus::Blocked, "blocked bypasses dwell");
        // Line-level renderers redraw the whole dialog on a selection move;
        // cell-diff renderers only move the cursor glyph.
        rig.repeat(Duration::from_millis(400), Duration::from_secs(20), |i| match i % 3 {
            0 => "\x1b[5A\x1b[JBash command\n  rm -rf target\nDo you want to proceed?\n  1. Yes\n❯ 2. No\n".into(),
            1 => "\x1b[4;1H \x1b[5;1H❯".into(),
            _ => "\x1b[?25h".into(),
        });
        assert_eq!(rig.status, AgentStatus::Blocked);
        let statuses: Vec<_> = rig.changes.iter().map(|(_, status)| *status).collect();
        assert_eq!(statuses, vec![AgentStatus::Working, AgentStatus::Blocked]);
        // Approval: the spinner comes back below the dialog.
        rig.output("\x1b[5A\x1b[J✻ Working (9s · esc to interrupt)\n");
        rig.wait(Duration::from_secs(2));
        assert_eq!(rig.status, AgentStatus::Working);
    }

    #[test]
    fn shell_question_answered_returns_to_idle() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        rig.output("./install.sh\r\nContinue? [y/N] ");
        assert_eq!(rig.status, AgentStatus::Blocked);
        rig.wait(Duration::from_secs(5));
        assert_eq!(rig.status, AgentStatus::Blocked);
        rig.output("y\r\ndone\r\nuser@host:~$ ");
        rig.wait(Duration::from_secs(3));
        let statuses: Vec<_> = rig.changes.iter().map(|(_, status)| *status).collect();
        assert_eq!(statuses, vec![AgentStatus::Blocked, AgentStatus::Idle]);
    }

    #[test]
    fn finished_command_settles_once() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        rig.output("cargo test\r\n");
        rig.repeat(Duration::from_millis(200), Duration::from_secs(4), |i| {
            format!("test case_{i} ... ok\r\n")
        });
        rig.output("test result: ok\r\nuser@host:~$ ");
        let finished = rig.now;
        rig.wait(Duration::from_secs(10));
        let statuses: Vec<_> = rig.changes.iter().map(|(_, status)| *status).collect();
        assert_eq!(statuses, vec![AgentStatus::Working, AgentStatus::Idle]);
        let idle_at = rig.start + rig.changes[1].0;
        assert!(idle_at - finished <= QUIET_IDLE_AFTER);
    }

    #[test]
    fn quiet_command_without_prompt_goes_idle_after_window() {
        let mut rig = Rig::new(AgentStatus::Idle);
        rig.repeat(Duration::from_millis(300), Duration::from_secs(3), |i| {
            format!("line {i}\n")
        });
        assert_eq!(rig.status, AgentStatus::Working);
        rig.wait(QUIET_IDLE_AFTER - Duration::from_millis(400));
        assert_eq!(rig.status, AgentStatus::Working);
        rig.wait(Duration::from_secs(1));
        assert_eq!(rig.status, AgentStatus::Idle);
    }

    #[test]
    fn keystroke_echo_is_not_work() {
        let mut rig = Rig::new(AgentStatus::Starting);
        rig.idle_shell();
        rig.repeat(Duration::from_millis(150), Duration::from_secs(5), |i| {
            ["g", "i", "t", " ", "s", "t", "a"][i % 7].into()
        });
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn typing_into_a_tui_box_is_not_work() {
        let mut rig = Rig::new(AgentStatus::Idle);
        rig.output("✻ Working (3s · esc to interrupt)\n");
        rig.output("\x1b[1A\x1b[2KDone.\n╭────╮\n│ >  │\n╰────╯\n");
        rig.wait(Duration::from_secs(6));
        assert_eq!(rig.status, AgentStatus::Idle);
        rig.changes.clear();
        // Each keystroke redraws the whole box, with line feeds.
        let mut typed = String::new();
        for c in "explain the build failure".chars() {
            typed.push(c);
            rig.activity.input(rig.now);
            rig.wait(Duration::from_millis(30));
            rig.output(&format!("\x1b[3A\x1b[J╭────╮\n│ > {typed} │\n╰────╯\n"));
            rig.wait(Duration::from_millis(170));
        }
        rig.wait(Duration::from_secs(6));
        assert_eq!(rig.changes, vec![]);
    }

    #[test]
    fn tail_signals_order_questions_and_markers() {
        assert!(tail_signals("Working (esc to interrupt)\nAllow command?").blocked);
        assert!(!tail_signals("Allow command?\n✻ Working (1s · esc to interrupt)").blocked);
        assert!(tail_signals("Allow command?\n✻ Working (1s · esc to interrupt)").marker);
        assert!(!tail_signals("Proceed? [y/n] y\nok\n$ ").blocked);
        let scrolled: String = (0..STATUS_TAIL_LINES).map(|i| format!("{i}\n")).collect();
        assert!(!tail_signals(&format!("[y/n]\n{scrolled}")).blocked);
    }
}
