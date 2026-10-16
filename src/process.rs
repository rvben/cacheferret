//! Bounded execution of native tools.
//!
//! Executables are always started from an argument array (never through a
//! shell), with a wall-clock timeout and a hard cap on captured output.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// Captured result of a finished process.
#[derive(Debug)]
pub(crate) struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// First line of stderr (or the exit status when stderr is empty), made
    /// safe to embed in a one-line diagnostic.
    pub fn failure_detail(&self) -> String {
        let detail = sanitize_message(&String::from_utf8_lossy(&self.stderr));
        if detail.is_empty() {
            "the command exited with a failure status".to_owned()
        } else {
            detail
        }
    }
}

/// Why a native tool could not produce output.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The executable is not installed or not on `PATH`.
    NotFound,
    Spawn(String),
    Timeout(Duration),
    OutputTooLarge,
    Read(String),
}

impl RunError {
    pub fn retryable(&self) -> bool {
        !matches!(self, RunError::NotFound | RunError::OutputTooLarge)
    }

    pub fn describe(&self, tool: &str, action: &str) -> String {
        match self {
            RunError::NotFound => format!("{tool} is not installed or is not available on PATH"),
            RunError::Spawn(error) => format!("{tool} {action} could not start: {error}"),
            RunError::Timeout(timeout) => format!(
                "{tool} {action} timed out after {} seconds",
                timeout.as_secs()
            ),
            RunError::OutputTooLarge => {
                format!("{tool} {action} produced more than the 4 MiB output limit")
            }
            RunError::Read(error) => format!("{tool} {action} output could not be read: {error}"),
        }
    }
}

/// Run `program` with `args`, killing it when `timeout` elapses.
pub(crate) fn run(program: &str, args: &[&str], timeout: Duration) -> Result<Output, RunError> {
    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                RunError::NotFound
            } else {
                RunError::Spawn(error.to_string())
            }
        })?;

    let stdout = spawn_reader(child.stdout.take().expect("piped stdout is present"));
    let stderr = spawn_reader(child.stderr.take().expect("piped stderr is present"));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout(timeout));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Read(error.to_string()));
            }
        }
    };

    let deadline = started + timeout;
    Ok(Output {
        success: status.success(),
        stdout: receive(stdout, deadline, timeout)?,
        stderr: receive(stderr, deadline, timeout)?,
    })
}

fn spawn_reader(reader: impl Read + Send + 'static) -> Receiver<Result<Vec<u8>, RunError>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_bounded(reader));
    });
    rx
}

fn read_bounded(mut reader: impl Read) -> Result<Vec<u8>, RunError> {
    let mut output = Vec::new();
    reader
        .by_ref()
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|error| RunError::Read(error.to_string()))?;
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(RunError::OutputTooLarge);
    }
    Ok(output)
}

fn receive(
    reader: Receiver<Result<Vec<u8>, RunError>>,
    deadline: Instant,
    timeout: Duration,
) -> Result<Vec<u8>, RunError> {
    match reader.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(RunError::Timeout(timeout)),
        Err(RecvTimeoutError::Disconnected) => Err(RunError::Read(
            "output reader stopped unexpectedly".to_owned(),
        )),
    }
}

/// Collapse control characters and whitespace so tool output can sit inside a
/// one-line, bounded diagnostic.
pub(crate) fn sanitize_message(message: &str) -> String {
    let flattened: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    flattened
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const SHORT: Duration = Duration::from_secs(5);

    #[test]
    fn captures_output_and_exit_status() {
        let ok = run("sh", &["-c", "printf out; printf err >&2"], SHORT).unwrap();
        assert!(ok.success);
        assert_eq!(ok.stdout_text(), "out");
        assert_eq!(ok.failure_detail(), "err");

        let failed = run("sh", &["-c", "exit 3"], SHORT).unwrap();
        assert!(!failed.success);
    }

    #[test]
    fn missing_executable_is_not_found() {
        let error = run("cacheferret-no-such-tool", &[], SHORT).unwrap_err();
        assert_eq!(error, RunError::NotFound);
        assert!(!error.retryable());
    }

    #[test]
    fn a_hung_process_is_killed_at_the_timeout() {
        let started = Instant::now();
        let error = run("sh", &["-c", "exec sleep 30"], Duration::from_millis(100)).unwrap_err();
        assert_eq!(error, RunError::Timeout(Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_beyond_the_cap_is_refused() {
        let error = run("sh", &["-c", "head -c 5000000 /dev/zero"], SHORT).unwrap_err();
        assert_eq!(error, RunError::OutputTooLarge);
    }

    #[test]
    fn messages_are_single_line_bounded_and_control_free() {
        let message = sanitize_message(&format!("a\nb\u{1b}[31m {}", "x".repeat(600)));
        assert!(!message.contains('\n'));
        assert!(!message.contains('\u{1b}'));
        assert!(message.chars().count() <= MAX_DIAGNOSTIC_CHARS);
    }
}
