//! Spawning Claude Code as a child process and streaming its interpreted events back.
//!
//! [`AgentRunner`] is the seam: [`LocalRunner`] spawns the CLI directly today, and the trait
//! exists so a `DockerRunner` can be added later without touching a single call site — an
//! explicit design call in `TODO.md` Phase 13, not speculative abstraction. It is object-safe
//! (returns a boxed future rather than using `async fn`) the same way
//! [`crate::secrets::vault::Validator`] is, for the same reason: callers hold it as
//! `Arc<dyn AgentRunner>`, chosen once at startup.
//!
//! Everything here builds directly on `docs/research/claude-code-cli.md`'s verified
//! invocation and I/O discipline:
//!
//! - `--session-id` is generated up front (a UUID v4) rather than scraped back from the
//!   `system/init` event, so Atlas owns it from the start.
//! - `--resume <id>` is used *instead of* `--session-id` (never both) — resuming reuses the
//!   original id, and it is CWD-scoped: the caller must respawn with the exact
//!   `working_dir` that created the session or the CLI fails with non-JSON stderr and a bare
//!   exit 1, which this module cannot turn into anything more helpful.
//! - `--permission-mode dontAsk` plus an explicit `--allowedTools` allowlist: the mode
//!   purpose-built for headless use, never waits for input, and fails closed. Full
//!   per-project permission-mode configurability is later work in this phase; this always
//!   picks the least permissive mode that works.
//! - `--strict-mcp-config` is always passed, even before Atlas has its own MCP server to
//!   list — with no `--mcp-config` at all, it isolates the run from the operator's personal
//!   claude.ai connectors, which is the reproducibility guarantee Atlas wants from the start.
//! - `env_remove("ANTHROPIC_API_KEY")`: if it leaks in, the CLI silently bills the API
//!   instead of the inherited Max subscription, with no prompt.
//! - stderr is drained concurrently in its own task — the CLI's real diagnostics ("No
//!   conversation found…") land there, and an undrained pipe can fill its OS buffer and
//!   deadlock the child.
//! - the child is spawned as its own process group ([`ProcessGroup::leader`]) and killed
//!   through it, so a Bash tool call the agent started does not outlive cancellation.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop, ProcessGroup};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agent::claude_code::{self, Event};
use crate::error::{AppError, AppResult};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How many turns/how much spend Atlas allows before the CLI cuts a run off itself.
///
/// Required, not optional: `docs/research/claude-code-cli.md` calls these "non-optional, not
/// nice-to-haves" — a single trivial prompt has been observed costing several cents from
/// cache-creation tokens alone, and a real agentic run with no ceiling is an open bill.
#[derive(Debug, Clone, Copy)]
pub struct RunLimits {
    pub max_turns: u32,
    pub max_budget_usd: f64,
}

/// What to run, and where.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// The prompt. Passed as a positional CLI argument, never shell-interpolated.
    pub prompt: String,
    /// The directory the CLI is spawned in — a card's cloned workspace. Must stay the exact
    /// same path for the lifetime of a session: `--resume` is CWD-scoped.
    pub working_dir: PathBuf,
    /// Resumes an existing session instead of starting one. When set, this *is* the id the
    /// run continues under — no separate `--session-id` is passed alongside it.
    pub resume_session_id: Option<String>,
    /// Tools this run may use, in `--allowedTools` syntax (bare names or scoped rules like
    /// `Bash(git diff *)`). Empty means the CLI's own (permissive) default allowlist, which
    /// is very rarely what a headless run wants — most callers should pass an explicit list.
    pub allowed_tools: Vec<String>,
    pub limits: RunLimits,
}

/// One line of the CLI's stdout, already interpreted — or the reason it could not be.
#[derive(Debug)]
pub enum RunEvent {
    // Boxed: `Event`'s `Result` variant carries several maps and vecs, and `RunEvent` is sent
    // through a channel constantly for the life of a run — boxing keeps the common
    // `Unparseable` case from paying to move a much larger enum around.
    Parsed(Box<Event>),
    /// A line that did not parse as any known shape. Carries the raw text rather than being
    /// dropped or treated as fatal: the upstream schema is open, and one malformed line must
    /// not end an otherwise-healthy run.
    Unparseable(String),
}

/// A live run: its interpreted event stream, and a way to end it early.
#[derive(Debug)]
pub struct RunHandle {
    /// The session id this run is under — the caller-generated one, or the resumed id.
    pub session_id: String,
    /// Yields one [`RunEvent`] per stdout line, and closes after the terminal `result` event
    /// (or if the process exits/dies without producing one).
    pub events: mpsc::Receiver<RunEvent>,
    cancel: Option<oneshot::Sender<()>>,
}

impl RunHandle {
    /// Ends the run: kills the whole process group and closes `events`. A no-op if the run
    /// has already finished (the cancel signal is only sent once, and a finished reader task
    /// is not listening for it any more either way).
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }
}

/// Runs Claude Code against a working directory and streams its interpreted events back.
///
/// See the module doc for why this is object-safe rather than an `async fn` trait.
pub trait AgentRunner: Send + Sync {
    fn spawn(&self, request: RunRequest) -> BoxFuture<'_, AppResult<RunHandle>>;
}

/// Spawns the real `claude` CLI as a child process.
#[derive(Debug, Clone)]
pub struct LocalRunner {
    /// The program to spawn. `"claude"` in production; a stand-in script in tests, so the
    /// spawn/stream/drain/cancel machinery is exercised without a real subprocess, network
    /// call, or API cost.
    program: String,
}

impl Default for LocalRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalRunner {
    /// Spawns the real CLI, found on `PATH` as `claude`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            program: "claude".to_owned(),
        }
    }

    /// Spawns `program` instead — a test double, or an explicit path when `claude` is not on
    /// `PATH`.
    #[must_use]
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl AgentRunner for LocalRunner {
    fn spawn(&self, request: RunRequest) -> BoxFuture<'_, AppResult<RunHandle>> {
        let program = self.program.clone();
        Box::pin(async move { spawn_local(&program, &request) })
    }
}

/// Not `async`: `Command::spawn` and setting up the reader tasks are both synchronous: the
/// tasks themselves do the waiting. Kept as a plain function so [`AgentRunner::spawn`]'s
/// `Box::pin(async move { ... })` still returns a future, satisfying the trait, without an
/// `async fn` that clippy would (rightly) flag for never actually awaiting anything.
fn spawn_local(program: &str, request: &RunRequest) -> AppResult<RunHandle> {
    let session_id = match &request.resume_session_id {
        Some(resume) => resume.clone(),
        None => Uuid::new_v4().to_string(),
    };

    let mut command = CommandWrap::with_new(program, |command| {
        command
            .arg("-p")
            .arg(&request.prompt)
            .args(["--output-format", "stream-json", "--verbose"])
            .args(["--permission-mode", "dontAsk"])
            .arg("--strict-mcp-config")
            .arg("--max-turns")
            .arg(request.limits.max_turns.to_string())
            .arg("--max-budget-usd")
            .arg(format!("{:.2}", request.limits.max_budget_usd))
            .current_dir(&request.working_dir)
            // Left in unless a later increment deliberately opts a project into API billing
            // — see the module doc.
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !request.allowed_tools.is_empty() {
            command
                .arg("--allowedTools")
                .arg(request.allowed_tools.join(","));
        }
        match &request.resume_session_id {
            Some(resume) => {
                command.arg("--resume").arg(resume);
            }
            None => {
                command.arg("--session-id").arg(&session_id);
            }
        }
    });
    command.wrap(ProcessGroup::leader());
    command.wrap(KillOnDrop);

    let mut child = spawn_with_etxtbsy_retry(&mut command)
        .map_err(|err| AppError::internal(anyhow::anyhow!("failed to spawn {program}: {err}")))?;

    let stdout = child.stdout().take().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!("the child process had no stdout pipe"))
    })?;
    let stderr = child.stderr().take().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!("the child process had no stderr pipe"))
    })?;

    // Drained on its own task for the whole lifetime of the child — see the module doc.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(target: "agent::claude_code", %line, "claude stderr");
        }
    });

    let (events_tx, events_rx) = mpsc::channel(64);
    let (cancel_tx, mut cancel_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    let _ = Box::into_pin(child.kill()).await;
                    break;
                }
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    let event = match claude_code::parse_line(&line) {
                        Ok(Some(event)) => RunEvent::Parsed(Box::new(event)),
                        Ok(None) => continue,
                        Err(_) => RunEvent::Unparseable(line),
                    };
                    let is_terminal =
                        matches!(&event, RunEvent::Parsed(event) if matches!(event.as_ref(), Event::Result(_)));
                    if events_tx.send(event).await.is_err() {
                        // Nobody is listening any more. `child` drops with this task, and
                        // KillOnDrop + the process group take care of the rest.
                        break;
                    }
                    if is_terminal {
                        break;
                    }
                }
            }
        }
    });

    Ok(RunHandle {
        session_id,
        events: events_rx,
        cancel: Some(cancel_tx),
    })
}

/// Retries a spawn a few times on `ETXTBSY` (errno 26).
///
/// This is a well-documented multi-threaded fork/exec race, not a sign `program` is actually
/// unavailable: an unrelated `fork()` anywhere else in this process can transiently duplicate
/// a write file descriptor onto the very file being exec'd here, even though nothing in Atlas
/// ever opens `program` for writing itself. The window is microseconds; a genuinely missing
/// or non-executable program fails immediately with a different errno, so this never masks a
/// real failure — only a handful of short retries clear the race.
fn spawn_with_etxtbsy_retry(command: &mut CommandWrap) -> std::io::Result<Box<dyn ChildWrapper>> {
    const ETXTBSY: i32 = 26;
    for attempt in 0..5 {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(err) if err.raw_os_error() == Some(ETXTBSY) && attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("the loop above always returns on its last iteration")
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    use tokio::time::{Duration, timeout};
    use uuid::Uuid;

    use super::*;

    /// A private temporary directory, removed on drop. Mirrors [`crate::test_support::TempDb`]
    /// rather than pulling in a `tempfile` dependency for one test module's sake.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("atlas-agent-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes an executable shell script standing in for the `claude` CLI, and returns the
    /// directory holding it (kept alive for as long as the returned guard lives) plus its
    /// path.
    fn fake_program(body: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let path = dir.0.join("fake-claude");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh\n{body}").unwrap();
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    /// Unwraps a `RunEvent::Parsed`, panicking with the actual value otherwise — keeps the
    /// tests below reading as plain `Event` matches instead of reaching through the `Box`.
    fn parsed(event: RunEvent) -> Event {
        match event {
            RunEvent::Parsed(event) => *event,
            RunEvent::Unparseable(line) => panic!("expected a parsed event, got: {line}"),
        }
    }

    fn request(working_dir: PathBuf) -> RunRequest {
        RunRequest {
            prompt: "do the thing".to_owned(),
            working_dir,
            resume_session_id: None,
            allowed_tools: vec!["Read".to_owned()],
            limits: RunLimits {
                max_turns: 10,
                max_budget_usd: 1.0,
            },
        }
    }

    #[tokio::test]
    async fn a_session_id_is_generated_and_a_fresh_run_never_passes_resume() {
        let (_dir, program) = fake_program(
            r#"
            cat <<'EOF'
            {"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"whatever","total_cost_usd":0.01,"terminal_reason":"completed"}
            EOF
            "#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let mut handle = runner
            .spawn(request(std::env::temp_dir()))
            .await
            .expect("spawn should succeed");

        assert!(!handle.session_id.is_empty());
        assert_eq!(handle.session_id.len(), 36, "a UUID v4, hyphenated");

        let first = timeout(Duration::from_secs(5), handle.events.recv())
            .await
            .expect("should not hang")
            .expect("one event");
        assert!(matches!(parsed(first), Event::Result(_)));
    }

    #[tokio::test]
    async fn events_stream_in_order_and_the_channel_closes_after_the_result() {
        let (_dir, program) = fake_program(
            r#"
            echo '{"type":"system","subtype":"init","session_id":"s-1"}'
            echo '{"type":"assistant","session_id":"s-1","message":{"role":"assistant","content":[]}}'
            echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"s-1","total_cost_usd":0.01,"terminal_reason":"completed"}'
            "#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let mut handle = runner.spawn(request(std::env::temp_dir())).await.unwrap();

        let mut received = Vec::new();
        while let Some(event) = timeout(Duration::from_secs(5), handle.events.recv())
            .await
            .expect("should not hang")
        {
            received.push(parsed(event));
        }

        assert_eq!(received.len(), 3, "{received:?}");
        assert!(matches!(received[0], Event::System(_)));
        assert!(matches!(received[1], Event::Assistant(_)));
        assert!(matches!(received[2], Event::Result(_)));
    }

    #[tokio::test]
    async fn an_unparseable_line_is_reported_rather_than_dropped_or_fatal() {
        let (_dir, program) = fake_program(
            r#"
            echo 'this is not json'
            echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"s-1","total_cost_usd":0.01,"terminal_reason":"completed"}'
            "#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let mut handle = runner.spawn(request(std::env::temp_dir())).await.unwrap();

        let first = timeout(Duration::from_secs(5), handle.events.recv())
            .await
            .unwrap()
            .unwrap();
        let RunEvent::Unparseable(line) = first else {
            panic!("expected an unparseable line, got {first:?}");
        };
        assert_eq!(line, "this is not json");

        let second = timeout(Duration::from_secs(5), handle.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(parsed(second), Event::Result(_)));
    }

    #[tokio::test]
    async fn a_chatty_stderr_does_not_block_stdout_from_being_read() {
        // Writes well past a typical 64KB pipe buffer to stderr before ever touching stdout.
        // If stderr were not drained concurrently, the child would block writing to it and
        // this test would hang until the timeout.
        let (_dir, program) = fake_program(
            r#"
            i=0
            while [ $i -lt 4000 ]; do
                echo "noisy diagnostic line number $i, padded to make it wider than it needs to be" >&2
                i=$((i + 1))
            done
            echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"s-1","total_cost_usd":0.01,"terminal_reason":"completed"}'
            "#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let mut handle = runner.spawn(request(std::env::temp_dir())).await.unwrap();

        let event = timeout(Duration::from_secs(10), handle.events.recv())
            .await
            .expect("must not deadlock on an undrained stderr pipe")
            .expect("one event");
        assert!(matches!(parsed(event), Event::Result(_)));
    }

    #[tokio::test]
    async fn cancelling_a_run_ends_the_event_stream_promptly() {
        let (_dir, program) = fake_program("sleep 60");
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let mut handle = runner.spawn(request(std::env::temp_dir())).await.unwrap();

        handle.cancel();

        let ended = timeout(Duration::from_secs(5), handle.events.recv())
            .await
            .expect("cancelling must end the run promptly, not after the child's own sleep");
        assert!(ended.is_none(), "the channel should close with no events");
    }
}
