//! Hook-independent agent activity watcher (concept borrowed from
//! nmux-linux's `watch-agents` daemon).
//!
//! `agent_status` only learns anything when a Claude Code hook fires, which
//! means it stays blank until the operator hand-edits `~/.claude/settings.json`
//! and never sees gjc / codex / omx / aider at all. This module closes that
//! gap by observing the PTY sessions the server already owns:
//!
//!   1. walk each session's process tree and look for a known agent binary,
//!   2. decide "is it working right now" from two signals,
//!   3. drive a small running → done state machine with hysteresis.
//!
//! The two activity signals are deliberately different from nmux-linux's.
//! nmux has to `stat(2)` somebody else's PTY to guess at output; we own the
//! reader thread, so `TerminalSession::output_bytes()` is an exact byte
//! counter — no filesystem timestamp granularity, no 30s-gap heuristic. CPU
//! ticks stay as the secondary signal for agents that burn cycles without
//! printing (large context assembly, local inference).
//!
//! Hooks and the watcher are complementary, not redundant: a hook carries
//! *semantics* the process table cannot ("Notification" = waiting for the
//! human), the watcher carries *liveness* no hook can guarantee. See
//! [`merge_status`] for the precedence rules.
//!
//! Process inspection is Linux-only (`/proc`). On other platforms every probe
//! returns "no agents", so the watcher degrades to a no-op and the crate still
//! builds for the Windows/macOS CI targets.

use std::collections::HashMap;
use std::sync::Mutex;

use time::OffsetDateTime;

use crate::agent_status::AgentStatus;
use crate::terminal::TerminalManager;
use crate::util;

/// How often the watcher samples. Also the unit of the hysteresis below.
pub const TICK_SECONDS: u64 = 3;

/// CPU jiffies an agent must burn within one tick to count as "working" on
/// the CPU signal alone. 60 ticks ≈ 600ms of CPU per 3s sample — high enough
/// that idle daemons' periodic heartbeats (token counters, MCP pings) do not
/// register, low enough that real tool execution always does.
const CPU_ACTIVE_JIFFIES: u64 = 60;

/// Consecutive quiet samples before running → done. 4 × 3s ≈ 12s: long
/// enough that a model pausing between token bursts doesn't flap the badge,
/// short enough that a finished run shows ✓ promptly.
const QUIET_TICKS_TO_DONE: u32 = 4;

/// Walk cap. A runaway process tree must never stall the watcher.
const MAX_TREE_NODES: usize = 512;

/// Binaries recognised as coding agents. Matched against argv[0]'s basename,
/// or argv[1]'s when argv[0] is an interpreter (`node dist/cli.js` style
/// launchers are the norm for JS agents).
const AGENT_BINS: &[&str] = &[
    "gjc", "claude", "codex", "omx", "gemini", "aider", "opencode", "continue", "cursor",
];

/// Interpreters whose argv[1] carries the real program name.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "python3"];

/// What the watcher concluded about one terminal session.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentActivity {
    /// Detected agent binary, e.g. "claude" / "gjc". Empty once the process
    /// is gone but a sticky `done` is still being shown.
    pub agent: String,
    pub state: &'static str,
    pub updated_at: String,
}

/// Per-session sampling state. `prev_*` are the previous tick's raw counters;
/// deltas — not absolute values — decide activity.
struct Sample {
    prev_cpu: u64,
    prev_out: u64,
    /// Consecutive samples with no activity while an agent was present.
    quiet: u32,
    state: Option<&'static str>,
    agent: String,
    updated_at: String,
    updated_instant: OffsetDateTime,
    /// True once the first sample established a baseline. The very first
    /// tick cannot compute a delta, so it must not claim "running".
    primed: bool,
}

impl Sample {
    fn new() -> Self {
        Self {
            prev_cpu: 0,
            prev_out: 0,
            quiet: 0,
            state: None,
            agent: String::new(),
            updated_at: String::new(),
            updated_instant: OffsetDateTime::UNIX_EPOCH,
            primed: false,
        }
    }

    /// Core state machine, kept free of I/O so it can be unit-tested.
    ///
    /// - agent present + active            → running
    /// - agent present + quiet ≥ threshold → done (only from running)
    /// - agent gone while running          → done immediately (it exited)
    /// - agent gone while done             → keep done (sticky until ack)
    fn advance(&mut self, agent_present: bool, active: bool, now: OffsetDateTime) {
        let before = self.state;
        if !agent_present {
            // A running agent that vanished has, by definition, finished.
            if self.state == Some("running") {
                self.state = Some("done");
            }
            self.quiet = 0;
            if self.state != Some("done") {
                self.state = None;
                self.agent.clear();
            }
        } else if active {
            self.quiet = 0;
            self.state = Some("running");
        } else {
            self.quiet = self.quiet.saturating_add(1);
            if self.state == Some("running") && self.quiet >= QUIET_TICKS_TO_DONE {
                self.state = Some("done");
            }
        }
        if self.state != before {
            self.updated_instant = now;
            self.updated_at = util::fmt_offset(now);
        }
    }
}

#[derive(Default)]
pub struct ActivityRegistry {
    by_session: Mutex<HashMap<String, Sample>>,
}

impl ActivityRegistry {
    pub fn lookup(&self, session_id: &str) -> Option<AgentActivity> {
        let map = self.by_session.lock().unwrap();
        let s = map.get(session_id)?;
        let state = s.state?;
        Some(AgentActivity {
            agent: s.agent.clone(),
            state,
            updated_at: s.updated_at.clone(),
        })
    }

    /// Clear a sticky `done` once the user has looked at the session, so the
    /// badge does not persist forever. Mirrors `AgentStatusRegistry::ack`.
    /// A live `running` is left alone — the work is still in flight.
    pub fn ack(&self, session_id: &str) {
        let mut map = self.by_session.lock().unwrap();
        if let Some(s) = map.get_mut(session_id)
            && s.state == Some("done")
        {
            s.state = None;
            s.quiet = 0;
            s.agent.clear();
        }
    }

    /// One sampling pass over every live session. Entries for sessions that
    /// no longer exist are dropped so the map cannot grow unbounded.
    pub fn tick(&self, terminals: &TerminalManager) {
        let sessions = terminals.list();
        let now = OffsetDateTime::now_utc();
        let mut map = self.by_session.lock().unwrap();
        let live: std::collections::HashSet<&str> =
            sessions.iter().map(|s| s.id.as_str()).collect();
        map.retain(|id, _| live.contains(id.as_str()));

        for session in &sessions {
            if session.is_exited() {
                map.remove(&session.id);
                continue;
            }
            let Some(root_pid) = session.shell_pid() else {
                continue;
            };
            let agents = find_agents(root_pid);
            let cpu: u64 = agents.iter().map(|(pid, _)| cpu_jiffies(*pid)).sum();
            let out = session.output_bytes();

            let entry = map.entry(session.id.clone()).or_insert_with(Sample::new);
            // First sample only establishes the baseline — a delta against
            // zero would read as a burst of activity on every new session.
            let active = entry.primed
                && (out > entry.prev_out || cpu.saturating_sub(entry.prev_cpu) >= CPU_ACTIVE_JIFFIES);
            entry.prev_out = out;
            entry.prev_cpu = cpu;
            entry.primed = true;
            if let Some((_, name)) = agents.first() {
                entry.agent = name.clone();
            }
            entry.advance(!agents.is_empty(), active, now);
        }
    }
}

/// Which status the UI should show for a session.
///
/// Precedence, highest first:
///   1. a sticky hook state (`done` / `needs_input`) — it encodes semantics
///      the process table cannot express, and the user must acknowledge it;
///   2. a fresh hook state — hooks are exact while they keep arriving;
///   3. the watcher — universal fallback that needs no setup;
///   4. a stale non-sticky hook state, rather than showing nothing.
///
/// Returns `(state, agent_label, source)`.
pub fn merge_status<'a>(
    hook: Option<&'a AgentStatus>,
    watch: Option<&'a AgentActivity>,
    hook_fresh_seconds: i64,
    now: OffsetDateTime,
) -> Option<(&'static str, String, &'static str)> {
    if let Some(h) = hook
        && (h.is_sticky() || h.age_seconds(now) <= hook_fresh_seconds)
    {
        return Some((h.state, "claude".to_string(), "hook"));
    }
    if let Some(w) = watch {
        let label = if w.agent.is_empty() {
            "agent".to_string()
        } else {
            w.agent.clone()
        };
        return Some((w.state, label, "watch"));
    }
    hook.map(|h| (h.state, "claude".to_string(), "hook"))
}

// ── /proc inspection ──────────────────────────────────────────────────

/// Every agent process inside `root_pid`'s tree, as `(pid, binary_name)`.
#[cfg(target_os = "linux")]
fn find_agents(root_pid: u32) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    let mut stack = vec![root_pid];
    let mut seen = std::collections::HashSet::new();
    let mut budget = MAX_TREE_NODES;
    while let Some(pid) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if !seen.insert(pid) {
            continue;
        }
        if let Some(name) = read_cmdline(pid).and_then(|c| agent_name(&c)) {
            found.push((pid, name));
        }
        stack.extend(children_of(pid));
    }
    found
}

#[cfg(not(target_os = "linux"))]
fn find_agents(_root_pid: u32) -> Vec<(u32, String)> {
    Vec::new()
}

/// Direct children via `/proc/<pid>/task/<tid>/children` — O(threads) instead
/// of the O(all processes) scan a ppid sweep of /proc would cost every tick.
#[cfg(target_os = "linux")]
fn children_of(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for task in tasks.flatten() {
        let path = task.path().join("children");
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(
                text.split_ascii_whitespace()
                    .filter_map(|t| t.parse::<u32>().ok()),
            );
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn read_cmdline(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/cmdline")).ok()
}

/// Cumulative utime+stime in jiffies from `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
fn cpu_jiffies(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| parse_cpu_jiffies(&s))
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn cpu_jiffies(_pid: u32) -> u64 {
    0
}

/// Parse utime+stime out of a `/proc/<pid>/stat` line.
///
/// Field 2 (`comm`) is the executable name in parentheses and may itself
/// contain spaces *and* parentheses — `(my (weird) app)` is legal. Splitting
/// on whitespace from the left is therefore wrong; anchor on the LAST ')'
/// instead, after which fields are positional: utime is #14, stime #15
/// overall, i.e. offsets 11 and 12 in the remainder.
fn parse_cpu_jiffies(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Agent binary name from a raw NUL-separated `/proc/<pid>/cmdline`.
///
/// Handles the interpreter case (`node …/claude`) because every JS agent
/// ships as a script rather than a native binary.
fn agent_name(cmdline: &[u8]) -> Option<String> {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
    let base = |a: &[u8]| -> String {
        let s = String::from_utf8_lossy(a);
        s.rsplit('/').next().unwrap_or("").to_string()
    };
    let arg0 = base(args.first()?);
    if AGENT_BINS.contains(&arg0.as_str()) {
        return Some(arg0);
    }
    if INTERPRETERS.contains(&arg0.as_str()) {
        let arg1 = base(args.get(1)?);
        if AGENT_BINS.contains(&arg1.as_str()) {
            return Some(arg1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn agent_name_matches_basename_not_substring() {
        assert_eq!(agent_name(&cmd(&["/usr/bin/claude"])).as_deref(), Some("claude"));
        assert_eq!(agent_name(&cmd(&["gjc", "--resume"])).as_deref(), Some("gjc"));
        // A path that merely contains an agent name must not match.
        assert_eq!(agent_name(&cmd(&["/opt/claude-helper/bin/tail"])), None);
        assert_eq!(agent_name(&cmd(&["/bin/bash"])), None);
        assert_eq!(agent_name(&[]), None);
    }

    #[test]
    fn agent_name_unwraps_interpreters_one_level_only() {
        assert_eq!(
            agent_name(&cmd(&["node", "/home/u/.local/bin/codex", "resume"])).as_deref(),
            Some("codex")
        );
        // argv[1] is not an agent → no match, and we do not scan further args
        // (that would match `node build.js --watch claude` style noise).
        assert_eq!(agent_name(&cmd(&["node", "server.js", "claude"])), None);
    }

    #[test]
    fn cpu_jiffies_survives_parens_in_comm() {
        // A verbatim /proc/<pid>/stat shape, with a comm chosen to break any
        // parser that splits from the left or stops at the first ')'.
        //
        //          f1 f2                  f3 f4 f5 f6 f7 f8  f9      f10 f11 f12 f13 f14  f15
        let stat = "42 (my (weird) app) S 1 42 42 0 -1 4194304 100 0 0 0 1234 56 7 8 20 0";
        //                                                             utime ^    ^ stime
        assert_eq!(parse_cpu_jiffies(stat), Some(1234 + 56));

        // Simple comm, same offsets — guards against an off-by-one that only
        // shows up when the name has no spaces.
        let plain = "7 (claude) R 1 7 7 0 -1 4194304 9 0 0 0 900 100 0 0 20 0";
        assert_eq!(parse_cpu_jiffies(plain), Some(1000));

        assert_eq!(parse_cpu_jiffies("garbage"), None);
        assert_eq!(parse_cpu_jiffies("42 (sh) S 1"), None); // truncated
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    #[test]
    fn running_needs_hysteresis_before_done() {
        let mut s = Sample::new();
        s.advance(true, true, now());
        assert_eq!(s.state, Some("running"));
        for _ in 0..(QUIET_TICKS_TO_DONE - 1) {
            s.advance(true, false, now());
            assert_eq!(s.state, Some("running"), "must not flap mid-thought");
        }
        s.advance(true, false, now());
        assert_eq!(s.state, Some("done"));
    }

    #[test]
    fn agent_exit_completes_immediately_and_done_is_sticky() {
        let mut s = Sample::new();
        s.advance(true, true, now());
        s.advance(false, false, now()); // process vanished
        assert_eq!(s.state, Some("done"));
        s.advance(false, false, now()); // stays until ack
        assert_eq!(s.state, Some("done"));
    }

    #[test]
    fn idle_shell_never_reports_a_state() {
        let mut s = Sample::new();
        for _ in 0..10 {
            s.advance(false, false, now());
        }
        assert_eq!(s.state, None);
    }

    #[test]
    fn activity_after_done_restarts_the_cycle() {
        let mut s = Sample::new();
        s.advance(true, true, now());
        for _ in 0..QUIET_TICKS_TO_DONE {
            s.advance(true, false, now());
        }
        assert_eq!(s.state, Some("done"));
        s.advance(true, true, now());
        assert_eq!(s.state, Some("running"), "new prompt re-arms the badge");
    }

    fn hook(state: &'static str, age: i64) -> AgentStatus {
        AgentStatus::for_test(state, now() - time::Duration::seconds(age))
    }
    fn watch(state: &'static str, agent: &str) -> AgentActivity {
        AgentActivity {
            agent: agent.into(),
            state,
            updated_at: String::new(),
        }
    }

    #[test]
    fn sticky_hook_outranks_a_live_watcher() {
        // The user must still see "입력 필요" even though the process is busy.
        let h = hook("needs_input", 9999);
        let w = watch("running", "claude");
        assert_eq!(
            merge_status(Some(&h), Some(&w), 60, now()),
            Some(("needs_input", "claude".into(), "hook"))
        );
    }

    #[test]
    fn stale_transient_hook_yields_to_the_watcher() {
        // Hook said "running" 10 minutes ago and then the process died: the
        // watcher's verdict is the trustworthy one.
        let h = hook("running", 600);
        let w = watch("done", "gjc");
        assert_eq!(
            merge_status(Some(&h), Some(&w), 60, now()),
            Some(("done", "gjc".into(), "watch"))
        );
    }

    #[test]
    fn watcher_alone_covers_the_no_hooks_case() {
        assert_eq!(
            merge_status(None, Some(&watch("running", "codex")), 60, now()),
            Some(("running", "codex".into(), "watch"))
        );
        assert_eq!(merge_status(None, None, 60, now()), None);
    }

    #[test]
    fn stale_hook_survives_when_the_watcher_is_blind() {
        // Non-Linux, or an agent we do not recognise: better a stale hook
        // state than an empty badge.
        let h = hook("running", 600);
        assert_eq!(
            merge_status(Some(&h), None, 60, now()),
            Some(("running", "claude".into(), "hook"))
        );
    }
}

/// End-to-end coverage for the parts the unit tests above cannot reach: the
/// real `/proc` tree walk, against a real PTY session driven by the real
/// `TerminalManager`. Linux-only, and skipped when the sandbox has no
/// `/bin/bash` to impersonate an agent with.
#[cfg(all(test, target_os = "linux"))]
mod proc_tests {
    use super::*;
    use crate::config::TerminalConfig;
    use crate::terminal::TerminalManager;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    /// A copy of bash named `claude`, so argv[0]'s basename makes the process
    /// look like a coding agent to the watcher. This is exactly what the
    /// production path keys on, so nothing is stubbed.
    fn fake_agent(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        if !std::path::Path::new("/bin/bash").exists() {
            return None;
        }
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).ok()?;
        let dst = bin_dir.join("claude");
        std::fs::copy("/bin/bash", &dst).ok()?;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).ok()?;
        Some(dst)
    }

    /// Poll `f` until it holds, ticking the watcher on its real cadence.
    /// Returns the observed state so failures print something useful.
    fn poll_until(
        reg: &ActivityRegistry,
        mgr: &TerminalManager,
        id: &str,
        want: &str,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        while Instant::now() < deadline {
            reg.tick(mgr);
            last = reg.lookup(id).map(|a| a.state.to_string());
            if last.as_deref() == Some(want) {
                return last;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        last
    }

    #[test]
    fn watches_a_real_agent_process_from_running_to_done() {
        let root = std::env::temp_dir().join(format!("mymux-activity-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let Some(agent_bin) = fake_agent(&root) else {
            eprintln!("skip: no /bin/bash to impersonate an agent");
            return;
        };

        let mgr = TerminalManager::new(TerminalConfig {
            default_shell: "/bin/bash".into(),
            allowed_shells: vec!["/bin/bash".into()],
            workspace_root: root.clone(),
            max_sessions_per_user: 4,
            idle_timeout_seconds: 0, // no reaping mid-test
            max_lifetime_seconds: 0,
            replay_buffer_bytes: 65536,
        });
        let session = mgr
            .spawn("u1", "tester", None, None, 80, 24)
            .expect("spawn pty");
        assert!(session.shell_pid().is_some(), "pid must be captured at spawn");

        // Baseline pass: a bare shell must never claim to be running.
        reg_assert_idle(&mgr, &session.id);

        // Start the "agent" as a CHILD of the shell, so detection has to walk
        // the tree rather than just inspect the session's own pid.
        let cmd = format!(
            "{} -c 'while :; do echo working; sleep 0.2; done' &\n",
            agent_bin.display()
        );
        session.write_input(cmd.as_bytes()).unwrap();

        let reg = ActivityRegistry::default();
        let got = poll_until(&reg, &mgr, &session.id, "running", Duration::from_secs(15));
        assert_eq!(
            got.as_deref(),
            Some("running"),
            "watcher should see the agent emitting output"
        );
        assert_eq!(
            reg.lookup(&session.id).unwrap().agent,
            "claude",
            "detected binary name should reach the UI"
        );

        // Kill the agent: the shell stays alive, so this isolates "agent
        // exited" from "session exited".
        session.write_input(b"kill %1\n").unwrap();
        let got = poll_until(&reg, &mgr, &session.id, "done", Duration::from_secs(20));
        assert_eq!(
            got.as_deref(),
            Some("done"),
            "agent disappearing must complete the run"
        );

        // Sticky until acknowledged, then gone.
        reg.tick(&mgr);
        assert_eq!(reg.lookup(&session.id).unwrap().state, "done");
        reg.ack(&session.id);
        assert!(reg.lookup(&session.id).is_none(), "ack clears the badge");

        mgr.remove(&session.id);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::io::stdout().flush();
    }

    /// A shell with nothing running in it must stay silent even after several
    /// passes — this is the property that keeps the sidebar from lighting up
    /// for every open terminal.
    fn reg_assert_idle(mgr: &TerminalManager, id: &str) {
        let reg = ActivityRegistry::default();
        for _ in 0..3 {
            reg.tick(mgr);
            std::thread::sleep(Duration::from_millis(120));
        }
        assert!(
            reg.lookup(id).is_none(),
            "an idle shell must not report an agent state"
        );
    }
}
