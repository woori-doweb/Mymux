//! Server-side PTY terminal manager.
//!
//! Adapted from crates/mycli-desktop/src/terminal.rs but redesigned for the
//! server: instead of Tauri polling (pty_read), a blocking reader thread pushes
//! output into a tokio broadcast channel + a replay ring buffer, and WebSocket
//! clients subscribe. Cross-platform on purpose (CI builds on Win/macOS too):
//! `portable_pty::Child::kill()` is portable — no unix-only signal code.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use time::OffsetDateTime;
use tokio::sync::broadcast;

use crate::config::TerminalConfig;
use crate::error::AppError;
use crate::util;

/// Output events broadcast to attached WebSocket clients.
#[derive(Clone)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exit,
}

pub struct TerminalSession {
    pub id: String,
    pub owner_user_id: String,
    pub owner_username: String,
    pub shell: String,
    pub cwd: String,
    pub created_at: String,
    created_instant: OffsetDateTime,
    last_active_at: Mutex<OffsetDateTime>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    pub output_tx: broadcast::Sender<PtyEvent>,
    replay: Mutex<VecDeque<u8>>,
    replay_cap: usize,
    exited: AtomicBool,
}

impl TerminalSession {
    pub fn write_input(&self, data: &[u8]) -> Result<(), AppError> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)
            .map_err(|e| AppError::Internal(format!("pty write: {e}")))?;
        let _ = w.flush();
        *self.last_active_at.lock().unwrap() = OffsetDateTime::now_utc();
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), AppError> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Internal(format!("pty resize: {e}")))
    }

    pub fn replay_snapshot(&self) -> Vec<u8> {
        self.replay.lock().unwrap().iter().copied().collect()
    }

    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        // Reap so the killed shell doesn't linger as a zombie (portable).
        let _ = child.wait();
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn last_active_rfc3339(&self) -> String {
        util::fmt_offset(*self.last_active_at.lock().unwrap())
    }
}

pub struct TerminalManager {
    sessions: Mutex<HashMap<String, Arc<TerminalSession>>>,
    config: TerminalConfig,
}

impl TerminalManager {
    pub fn new(config: TerminalConfig) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            config,
        }
    }

    pub fn spawn(
        &self,
        owner_user_id: &str,
        owner_username: &str,
        shell: Option<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<TerminalSession>, AppError> {
        // Per-user session quota (count only live sessions).
        {
            let map = self.sessions.lock().unwrap();
            let live = map
                .values()
                .filter(|s| s.owner_user_id == owner_user_id && !s.is_exited())
                .count();
            if live >= self.config.max_sessions_per_user {
                return Err(AppError::BadRequest(format!(
                    "session limit reached ({})",
                    self.config.max_sessions_per_user
                )));
            }
        }

        // Shell allowlist.
        let shell = match shell {
            Some(s) if !s.is_empty() => s,
            _ => self.config.default_shell.clone(),
        };
        if !self.config.allowed_shells.iter().any(|a| a == &shell) {
            return Err(AppError::BadRequest(format!("shell not allowed: {shell}")));
        }

        // cwd must be contained in workspace_root (canonicalized, not prefix-matched).
        let cwd = self.resolve_cwd(cwd.as_deref())?;

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Internal(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&cwd);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::Internal(format!("spawn shell: {e}")))?;
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| AppError::Internal(format!("take_writer: {e}")))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::Internal(format!("clone_reader: {e}")))?;

        let (tx, _rx) = broadcast::channel::<PtyEvent>(1024);
        let id = format!("term_{}", uuid::Uuid::new_v4().simple());
        let now = OffsetDateTime::now_utc();
        let session = Arc::new(TerminalSession {
            id: id.clone(),
            owner_user_id: owner_user_id.to_string(),
            owner_username: owner_username.to_string(),
            shell: shell.clone(),
            cwd: cwd.to_string_lossy().to_string(),
            created_at: util::fmt_offset(now),
            created_instant: now,
            last_active_at: Mutex::new(now),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            output_tx: tx,
            replay: Mutex::new(VecDeque::new()),
            replay_cap: self.config.replay_buffer_bytes,
            exited: AtomicBool::new(false),
        });

        // Reader thread: PTY stdout -> replay ring buffer + broadcast.
        let s2 = Arc::clone(&session);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        {
                            let mut rb = s2.replay.lock().unwrap();
                            rb.extend(data.iter().copied());
                            while rb.len() > s2.replay_cap {
                                rb.pop_front();
                            }
                        }
                        let _ = s2.output_tx.send(PtyEvent::Output(data));
                    }
                    Err(_) => break,
                }
            }
            s2.exited.store(true, Ordering::Relaxed);
            let _ = s2.output_tx.send(PtyEvent::Exit);
        });

        self.sessions
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&session));
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Option<Arc<TerminalSession>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<TerminalSession>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    /// Remove a session from the map and kill its child process.
    pub fn remove(&self, id: &str) -> Option<Arc<TerminalSession>> {
        let removed = self.sessions.lock().unwrap().remove(id);
        if let Some(ref s) = removed {
            s.kill();
        }
        removed
    }

    fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf, AppError> {
        let root = std::fs::canonicalize(&self.config.workspace_root).map_err(|e| {
            AppError::Internal(format!(
                "workspace_root {} not accessible: {e}",
                self.config.workspace_root.display()
            ))
        })?;
        let target = match requested {
            Some(s) if !s.is_empty() => match std::fs::canonicalize(Path::new(s)) {
                Ok(c) => c,
                Err(_) => root.clone(),
            },
            _ => root.clone(),
        };
        if !target.starts_with(&root) {
            return Err(AppError::Forbidden);
        }
        Ok(target)
    }

    /// Reap exited / idle / over-lifetime sessions. Called periodically.
    pub fn reap(&self) {
        let now = OffsetDateTime::now_utc();
        let idle = self.config.idle_timeout_seconds;
        let life = self.config.max_lifetime_seconds;
        let mut to_remove = Vec::new();
        {
            let map = self.sessions.lock().unwrap();
            for (id, s) in map.iter() {
                if s.is_exited() {
                    to_remove.push(id.clone());
                    continue;
                }
                if idle > 0 {
                    let last = *s.last_active_at.lock().unwrap();
                    if (now - last).whole_seconds() >= idle {
                        to_remove.push(id.clone());
                        continue;
                    }
                }
                if life > 0 && (now - s.created_instant).whole_seconds() >= life {
                    to_remove.push(id.clone());
                }
            }
        }
        for id in to_remove {
            self.remove(&id);
        }
    }
}
