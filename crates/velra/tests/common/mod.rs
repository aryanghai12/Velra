//! Shared harness for the acceptance suite (§21).
#![allow(dead_code)]

use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::PathBuf;
use velra_core::checkpoint::{self, CheckpointRequest};
use velra_core::db::{Db, Role};
use velra_core::event::{dedupe_key, NewEvent, Payload, ProjectInfo};
use velra_core::model::Trigger;
use velra_core::render::{RenderConfig, Snapshot};
use velra_core::snapshot::SnapshotMeta;
use velra_core::{eventlog, hash, paths, reducer};

/// 2026-09-12T10:04:05Z — every test clock starts here so output is stable.
pub const BASE_MS: i64 = 1_789_207_445_000;

/// An isolated `$VELRA_HOME` + settings dir + project dir.
pub struct Env {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub config: PathBuf,
    pub project: PathBuf,
    pub session: String,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let config = dir.path().join("claude");
        let project = dir.path().join("project");
        for p in [&home, &config, &project] {
            std::fs::create_dir_all(p).expect("create dir");
        }
        Env {
            dir,
            home,
            config,
            project,
            session: "test-session".to_string(),
        }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn db_path(&self) -> PathBuf {
        self.home.join("velra.db")
    }

    pub fn spool_dir(&self) -> PathBuf {
        self.home.join("spool")
    }

    pub fn open_db(&self) -> Db {
        Db::open(&self.db_path(), Role::Cli).expect("open db")
    }

    pub fn project_id(&self) -> String {
        let canonical = paths::canonical(&self.project).unwrap_or_else(|| self.project.clone());
        let normalized = paths::normalize_abs(&canonical.to_string_lossy());
        hash::hex_prefix(paths::identity(&normalized).as_bytes(), 16)
    }

    pub fn project_info(&self) -> ProjectInfo {
        let canonical = paths::canonical(&self.project).unwrap_or_else(|| self.project.clone());
        ProjectInfo {
            project_id: self.project_id(),
            root_path: paths::normalize_abs(&canonical.to_string_lossy()),
            is_git: self.project.join(".git").exists(),
        }
    }

    /// The `velra` binary with this environment applied.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("velra").expect("binary");
        cmd.env("VELRA_HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.config)
            .env("CLAUDE_PROJECT_DIR", &self.project)
            .env("TZ", "UTC")
            .env_remove("VELRA_DISABLE")
            .env_remove("VELRA_LOG")
            .env_remove("VELRA_TEST_PANIC")
            .env_remove("VELRA_TEST_STALL_MS")
            .current_dir(&self.project);
        cmd
    }

    /// Runs one hook with the given stdin payload.
    pub fn hook(&self, event: &str, payload: &Value) -> HookOutput {
        self.hook_raw(event, payload.to_string().as_bytes())
    }

    pub fn hook_raw(&self, event: &str, stdin: &[u8]) -> HookOutput {
        let out = self
            .cmd()
            .arg("hook")
            .arg(event)
            .write_stdin(stdin.to_vec())
            .output()
            .expect("run hook");
        HookOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn reduce(&self) -> HookOutput {
        let out = self
            .cmd()
            .arg("reduce")
            .write_stdin("{}")
            .output()
            .expect("run reduce");
        HookOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Common hook input fields.
    pub fn base_payload(&self, event: &str) -> Value {
        json!({
            "session_id": self.session,
            "hook_event_name": event,
            "cwd": self.project.to_string_lossy(),
            "transcript_path": self.dir.path().join("transcript.jsonl").to_string_lossy(),
        })
    }

    pub fn write_file(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.project.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, content).expect("write file");
        path
    }

    /// Creates a minimal git repository (no `git` binary involved).
    pub fn init_git(&self, branch: &str, sha: &str) {
        let git = self.project.join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).expect("git dirs");
        std::fs::write(git.join("HEAD"), format!("ref: refs/heads/{branch}\n")).expect("HEAD");
        // Branch names may contain slashes (`feat/auth-refactor`), so the ref
        // file lives one or more directories deep.
        let ref_path = git.join(format!("refs/heads/{branch}"));
        if let Some(parent) = ref_path.parent() {
            std::fs::create_dir_all(parent).expect("ref dirs");
        }
        std::fs::write(&ref_path, format!("{sha}\n")).expect("ref");
    }
}

pub struct HookOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl HookOutput {
    /// §8.1: exit 0, empty stderr, stdout empty or exactly one JSON object
    /// on a single line followed by `\n`.
    pub fn assert_contract(&self) -> &Self {
        assert_eq!(self.code, 0, "exit code must be 0, stderr: {}", self.stderr);
        assert!(
            self.stderr.is_empty(),
            "stderr must be empty, got: {}",
            self.stderr
        );
        if !self.stdout.is_empty() {
            assert!(
                self.stdout.ends_with('\n'),
                "stdout must end with a newline"
            );
            let line = self.stdout.trim_end_matches('\n');
            assert!(!line.contains('\n'), "stdout must be a single line");
            let value: Value = serde_json::from_str(line).expect("stdout must be one JSON object");
            assert!(value.is_object(), "stdout must be a JSON object");
        }
        self
    }

    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(self.stdout.trim_end_matches('\n')).ok()
    }
}

/// Builds an event log directly, without going through the hook binary.
pub struct Log {
    pub env: Env,
    pub db: Db,
    pub ts: i64,
    seq: usize,
}

impl Log {
    pub fn new() -> Log {
        let env = Env::new();
        let db = env.open_db();
        Log {
            env,
            db,
            ts: BASE_MS,
            seq: 0,
        }
    }

    fn next(&mut self) -> (i64, String) {
        self.ts += 1_000;
        self.seq += 1;
        (self.ts, format!("toolu_{:04}", self.seq))
    }

    pub fn append(&mut self, hook_event: &str, tool: Option<&str>, payload: Payload) -> i64 {
        self.append_as(hook_event, tool, payload, None)
    }

    pub fn append_as(
        &mut self,
        hook_event: &str,
        tool: Option<&str>,
        payload: Payload,
        agent: Option<&str>,
    ) -> i64 {
        let (ts, tool_use_id) = self.next();
        let session = self.env.session.clone();
        let ev = NewEvent {
            dedupe_key: dedupe_key(hook_event, &session, Some(&tool_use_id), None, ts, agent),
            session_id: session,
            project_id: self.env.project_id(),
            agent_id: agent.map(str::to_string),
            hook_event: hook_event.to_string(),
            tool_name: tool.map(str::to_string),
            tool_use_id: Some(tool_use_id),
            ts_ms: ts,
            payload: payload.to_json(),
            project: Some(self.env.project_info()),
        };
        eventlog::append(&mut self.db.conn, &ev)
            .expect("append")
            .unwrap_or(0)
    }

    /// Appends an event carrying `ts_ms` instead of the log's own clock.
    ///
    /// This is the shape a spooled event takes: the hook stamped it when it
    /// happened, could not reach the database, wrote it to the spool, and the
    /// reducer ingested it later — so it lands with an old timestamp and a row
    /// id newer than events that really came after it.
    pub fn append_late(
        &mut self,
        hook_event: &str,
        tool: Option<&str>,
        payload: Payload,
        ts_ms: i64,
    ) -> i64 {
        self.seq += 1;
        let tool_use_id = format!("toolu_late_{:04}", self.seq);
        let session = self.env.session.clone();
        let ev = NewEvent {
            dedupe_key: dedupe_key(hook_event, &session, Some(&tool_use_id), None, ts_ms, None),
            session_id: session,
            project_id: self.env.project_id(),
            agent_id: None,
            hook_event: hook_event.to_string(),
            tool_name: tool.map(str::to_string),
            tool_use_id: Some(tool_use_id),
            ts_ms,
            payload: payload.to_json(),
            project: Some(self.env.project_info()),
        };
        eventlog::append(&mut self.db.conn, &ev)
            .expect("append")
            .unwrap_or(0)
    }

    /// Hashes the files as they are on disk right now, as a hook would.
    pub fn observe(&self, rels: &[&str]) -> Vec<velra_core::event::FileObservation> {
        rels.iter()
            .map(|rel| {
                let (h, size) = hash::hash_file(&self.env.project.join(rel));
                velra_core::event::FileObservation {
                    path: rel.to_string(),
                    hash: h,
                    size,
                }
            })
            .collect()
    }

    pub fn prompt(&mut self, text: &str) {
        self.append(
            "UserPromptSubmit",
            None,
            Payload {
                prompt: Some(text.into()),
                ..Default::default()
            },
        );
    }

    /// A full edit: pre-hash, file change on disk, post-hash.
    pub fn edit(&mut self, rel: &str, new_content: &str) {
        self.edit_with(rel, new_content, "Edit", None)
    }

    pub fn edit_as(&mut self, rel: &str, new_content: &str, agent: &str) {
        self.edit_with(rel, new_content, "Edit", Some(agent))
    }

    pub fn write_tool(&mut self, rel: &str, new_content: &str) {
        self.edit_with(rel, new_content, "Write", None)
    }

    fn edit_with(&mut self, rel: &str, new_content: &str, tool: &str, agent: Option<&str>) {
        let abs = self.env.project.join(rel);
        let old = std::fs::read_to_string(&abs).unwrap_or_default();
        let (pre_hash, pre_size) = hash::hash_file(&abs);
        self.append_as(
            "PreToolUse",
            Some(tool),
            Payload {
                path: Some(rel.into()),
                pre_hash: Some(pre_hash),
                size: Some(pre_size),
                ..Default::default()
            },
            agent,
        );
        self.env.write_file(rel, new_content);
        let (post_hash, size) = hash::hash_file(&abs);
        let excerpt = first_diff_line(&old, new_content);
        self.append_as(
            "PostToolUse",
            Some(tool),
            Payload {
                path: Some(rel.into()),
                post_hash: Some(post_hash),
                size: Some(size),
                lines_added: Some(new_content.lines().count() as u32),
                lines_removed: if tool == "Write" {
                    None
                } else {
                    Some(old.lines().count() as u32)
                },
                excerpt,
                ..Default::default()
            },
            agent,
        );
    }

    pub fn read(&mut self, rel: &str) {
        self.append(
            "PostToolUse",
            Some("Read"),
            Payload {
                path: Some(rel.into()),
                ..Default::default()
            },
        );
    }

    /// A shell command that succeeded.
    pub fn command_ok(&mut self, command: &str, stdout: &str) {
        self.append(
            "PostToolUse",
            Some("Bash"),
            Payload {
                command: Some(command.into()),
                cwd: Some(self.env.project.to_string_lossy().into_owned()),
                stdout_tail: Some(stdout.into()),
                ..Default::default()
            },
        );
    }

    /// A shell command that failed (`PostToolUseFailure` with `Exit code N`).
    pub fn command_fail(&mut self, command: &str, exit: i64, output: &str) {
        self.append(
            "PostToolUseFailure",
            Some("Bash"),
            Payload {
                command: Some(command.into()),
                cwd: Some(self.env.project.to_string_lossy().into_owned()),
                error: Some(format!("Exit code {exit}\n{output}")),
                is_interrupt: Some(false),
                tool_name: Some("Bash".into()),
                ..Default::default()
            },
        );
    }

    /// A git restore-family command: hashes before, file change, hashes after.
    pub fn git_restore(&mut self, command: &str, changes: &[(&str, &str)]) {
        self.git_command(command, changes, true, false)
    }

    /// A restore-family command reported as a *failed* tool call, which is what
    /// Claude Code sends for `git restore x && pytest` whenever the suite still
    /// fails afterwards.
    pub fn git_restore_failed(
        &mut self,
        command: &str,
        exit: i64,
        output: &str,
        changes: &[(&str, &str)],
    ) {
        self.git_command_with(command, changes, true, false, Some((exit, output)))
    }

    pub fn git_commit(&mut self, command: &str, files: &[&str]) {
        let changes: Vec<(&str, &str)> = files.iter().map(|f| (*f, "")).collect();
        self.git_command(command, &changes, false, true)
    }

    /// A commit reported as a failed tool call.
    pub fn git_commit_failed(&mut self, command: &str, exit: i64, output: &str, files: &[&str]) {
        let changes: Vec<(&str, &str)> = files.iter().map(|f| (*f, "")).collect();
        self.git_command_with(command, &changes, false, true, Some((exit, output)))
    }

    fn git_command(
        &mut self,
        command: &str,
        changes: &[(&str, &str)],
        restore: bool,
        commit: bool,
    ) {
        self.git_command_with(command, changes, restore, commit, None)
    }

    fn git_command_with(
        &mut self,
        command: &str,
        changes: &[(&str, &str)],
        restore: bool,
        commit: bool,
        failure: Option<(i64, &str)>,
    ) {
        let observe = |env: &Env, rel: &str| {
            let (h, size) = hash::hash_file(&env.project.join(rel));
            velra_core::event::FileObservation {
                path: rel.to_string(),
                hash: h,
                size,
            }
        };
        // The hook records the restore-family *subcommand*, not the whole line
        // it was chained into; mirror that here or the harness tests something
        // the product never produces.
        let restore = restore
            .then(|| velra_core::shell::git_effects(command, &|_| false).restore)
            .flatten()
            .or_else(|| restore.then(|| command.to_string()));
        let pre: Vec<_> = changes
            .iter()
            .map(|(rel, _)| observe(&self.env, rel))
            .collect();
        self.append(
            "PreToolUse",
            Some("Bash"),
            Payload {
                command: Some(command.into()),
                git: Some(velra_core::event::GitObservation {
                    restore: restore.clone(),
                    commit,
                    files: pre,
                }),
                ..Default::default()
            },
        );
        for (rel, content) in changes {
            if !content.is_empty() {
                self.env.write_file(rel, content);
            }
        }
        let post: Vec<_> = changes
            .iter()
            .map(|(rel, _)| observe(&self.env, rel))
            .collect();
        let git = Some(velra_core::event::GitObservation {
            restore,
            commit,
            files: post,
        });
        match failure {
            None => self.append(
                "PostToolUse",
                Some("Bash"),
                Payload {
                    command: Some(command.into()),
                    cwd: Some(self.env.project.to_string_lossy().into_owned()),
                    stdout_tail: Some(String::new()),
                    git,
                    ..Default::default()
                },
            ),
            Some((exit, output)) => self.append(
                "PostToolUseFailure",
                Some("Bash"),
                Payload {
                    command: Some(command.into()),
                    cwd: Some(self.env.project.to_string_lossy().into_owned()),
                    error: Some(format!("Exit code {exit}\n{output}")),
                    is_interrupt: Some(false),
                    tool_name: Some("Bash".into()),
                    git,
                    ..Default::default()
                },
            ),
        };
    }

    pub fn stop(&mut self) {
        self.append(
            "Stop",
            None,
            Payload {
                stop_hook_active: Some(false),
                ..Default::default()
            },
        );
    }

    pub fn reduce(&mut self) {
        reducer::reduce_all(&mut self.db.conn, Some(&self.env.spool_dir())).expect("reduce");
    }

    pub fn snapshot(&mut self) -> Snapshot {
        self.reduce();
        let meta = SnapshotMeta {
            checkpoint_id: "ckpt_01TESTFIXTURE0000000000000".to_string(),
            created_ms: self.ts + 1_000,
            trigger: Trigger::Manual,
            partial: false,
            preview: false,
            tz_offset_secs: 0,
        };
        velra_core::snapshot::build(&self.db.conn, &self.env.session.clone(), &meta)
            .expect("snapshot")
    }

    pub fn capsule(&mut self) -> String {
        let snap = self.snapshot();
        velra_core::render::render(&snap, &RenderConfig::default()).text
    }

    /// Creates a checkpoint the way `pre-compact` does.
    pub fn checkpoint(&mut self) -> String {
        self.reduce();
        let session = self.env.session.clone();
        let created = self.ts + 1_000;
        let tx = self.db.conn.transaction().expect("tx");
        let info = checkpoint::create_in_tx(
            &tx,
            &CheckpointRequest {
                session_id: &session,
                trigger: Trigger::Manual,
                created_ms: created,
                partial: false,
                watermark: 0,
            },
            &RenderConfig::default(),
        )
        .expect("checkpoint")
        .expect("something worth saving");
        tx.commit().expect("commit");
        info.checkpoint_id
    }

    pub fn edits(&self) -> Vec<(String, String, Option<String>)> {
        let mut stmt = self
            .db
            .conn
            .prepare("SELECT path, status, mechanism FROM edits ORDER BY id")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        rows
    }

    pub fn dead_ends(&self) -> Vec<(String, String, Option<String>, i64)> {
        let mut stmt = self
            .db
            .conn
            .prepare("SELECT path, mechanism, command_text, reapplied FROM dead_ends ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    pub fn versions(&self, path: &str) -> Vec<(String, String)> {
        let mut stmt = self
            .db
            .conn
            .prepare("SELECT source, content_hash FROM file_versions WHERE path = ?1 ORDER BY id")
            .expect("prepare");
        stmt.query_map([path], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    pub fn commands(&self) -> Vec<(String, String, String)> {
        let mut stmt = self
            .db
            .conn
            .prepare("SELECT kind, outcome, command_text FROM commands ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }
}

fn first_diff_line(old: &str, new: &str) -> Option<String> {
    let mut o = old.lines();
    let mut n = new.lines();
    loop {
        match (o.next(), n.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => return None,
            (a, b) => {
                let mut out = String::new();
                if let Some(a) = a.map(str::trim).filter(|s| !s.is_empty()) {
                    out.push_str(&format!("- {a}"));
                }
                if let Some(b) = b.map(str::trim).filter(|s| !s.is_empty()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&format!("+ {b}"));
                }
                return (!out.is_empty()).then_some(out);
            }
        }
    }
}

/// Snapshot settings so golden capsules live in `tests/golden/capsule`.
pub fn golden_capsule(name: &str, text: &str) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../../../../tests/golden/capsule");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| insta::assert_snapshot!(name, text));
}
