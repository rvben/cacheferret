use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{NativeDiagnostic, NativeReport, NativeResource};

const PROVIDER: &str = "docker";
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// Inspect Docker-managed storage without changing daemon state.
pub fn inspect_docker() -> NativeReport {
    let output = match run_system_df() {
        Ok(output) => output,
        Err(diagnostic) => return unavailable(diagnostic),
    };

    if !output.status.success() {
        let detail = diagnostic_text(&output.stderr);
        let message = if detail.is_empty() {
            format!("Docker storage inspection exited with {}", output.status)
        } else {
            detail
        };
        return unavailable(diagnostic(classify_failure(&message), message, true));
    }

    let mut resources = Vec::new();
    let mut diagnostics = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for (line_index, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_resource(line) {
            Ok(resource)
                if resources
                    .iter()
                    .any(|current: &NativeResource| current.kind == resource.kind) =>
            {
                diagnostics.push(diagnostic(
                    "duplicate_output",
                    format!("Docker reported {} more than once", resource.label),
                    false,
                ));
            }
            Ok(resource) => resources.push(resource),
            Err(message) => diagnostics.push(diagnostic(
                "invalid_output",
                format!("Docker row {} could not be read: {message}", line_index + 1),
                false,
            )),
        }
    }

    resources.sort_by(|left, right| {
        right
            .reclaimable_bytes
            .cmp(&left.reclaimable_bytes)
            .then_with(|| left.kind.cmp(&right.kind))
    });

    if resources.is_empty() && diagnostics.is_empty() {
        diagnostics.push(diagnostic(
            "empty_output",
            "Docker returned no storage classes".to_owned(),
            true,
        ));
    } else if !resources.is_empty() {
        for (kind, label) in [
            ("docker-images", "Docker images"),
            ("docker-containers", "Docker containers"),
            ("docker-volumes", "Docker volumes"),
            ("docker-build-cache", "Docker build cache"),
        ] {
            if !resources.iter().any(|resource| resource.kind == kind) {
                diagnostics.push(diagnostic(
                    "missing_class",
                    format!("Docker did not report {label}"),
                    false,
                ));
            }
        }
    }

    NativeReport {
        provider: PROVIDER.to_owned(),
        available: true,
        resources,
        diagnostics,
    }
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum ReadOutputError {
    Io(io::Error),
    TooLarge,
}

fn run_system_df() -> Result<ProcessOutput, NativeDiagnostic> {
    let started = Instant::now();
    let mut child = Command::new("docker")
        .args(["system", "df", "--format", "json"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            let (kind, message, retryable) = if error.kind() == io::ErrorKind::NotFound {
                (
                    "not_found",
                    "Docker is not installed or is not available on PATH".to_owned(),
                    false,
                )
            } else {
                (
                    "unavailable",
                    format!("Docker storage inspection could not start: {error}"),
                    true,
                )
            };
            diagnostic(kind, message, retryable)
        })?;

    let stdout = child.stdout.take().expect("piped stdout is present");
    let stderr = child.stderr.take().expect("piped stderr is present");
    let stdout_reader = spawn_reader(stdout);
    let stderr_reader = spawn_reader(stderr);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < INSPECTION_TIMEOUT => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(diagnostic(
                    "timeout",
                    "Docker storage inspection timed out after 5 seconds".to_owned(),
                    true,
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(diagnostic(
                    "unavailable",
                    format!("Docker storage inspection could not be observed: {error}"),
                    true,
                ));
            }
        }
    };

    let deadline = started + INSPECTION_TIMEOUT;
    let stdout = receive_reader(stdout_reader, "stdout", deadline)?;
    let stderr = receive_reader(stderr_reader, "stderr", deadline)?;
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn spawn_reader(reader: impl Read + Send + 'static) -> Receiver<Result<Vec<u8>, ReadOutputError>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_bounded(reader));
    });
    rx
}

fn read_bounded(mut reader: impl Read) -> Result<Vec<u8>, ReadOutputError> {
    let mut output = Vec::new();
    reader
        .by_ref()
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(ReadOutputError::Io)?;
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(ReadOutputError::TooLarge);
    }
    Ok(output)
}

fn receive_reader(
    reader: Receiver<Result<Vec<u8>, ReadOutputError>>,
    stream: &str,
    deadline: Instant,
) -> Result<Vec<u8>, NativeDiagnostic> {
    match reader.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(ReadOutputError::TooLarge)) => Err(diagnostic(
            "output_too_large",
            format!("Docker {stream} exceeded the 1 MiB output limit"),
            false,
        )),
        Ok(Err(ReadOutputError::Io(error))) => Err(diagnostic(
            "unavailable",
            format!("Docker {stream} could not be read: {error}"),
            true,
        )),
        Err(RecvTimeoutError::Timeout) => Err(diagnostic(
            "timeout",
            "Docker storage inspection timed out after 5 seconds".to_owned(),
            true,
        )),
        Err(RecvTimeoutError::Disconnected) => Err(diagnostic(
            "unavailable",
            format!("Docker {stream} reader stopped unexpectedly"),
            true,
        )),
    }
}

fn parse_resource(line: &str) -> Result<NativeResource, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("invalid JSON ({error})"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected a JSON object".to_owned())?;
    let type_name = text_field(object, "Type")?;
    let (kind, label) = resource_identity(type_name)
        .ok_or_else(|| format!("unknown storage class {type_name:?}"))?;

    Ok(NativeResource {
        provider: PROVIDER.to_owned(),
        scope: "daemon".to_owned(),
        kind: kind.to_owned(),
        label: label.to_owned(),
        total_count: integer_field(object, "TotalCount")?,
        active_count: integer_field(object, "Active")?,
        bytes: size_field(object, "Size")?,
        reclaimable_bytes: reclaimable_field(object, "Reclaimable")?,
        cleanable: false,
    })
}

fn resource_identity(type_name: &str) -> Option<(&'static str, &'static str)> {
    match type_name {
        "Images" => Some(("docker-images", "Docker images")),
        "Containers" => Some(("docker-containers", "Docker containers")),
        "Local Volumes" => Some(("docker-volumes", "Docker volumes")),
        "Build Cache" => Some(("docker-build-cache", "Docker build cache")),
        _ => None,
    }
}

fn text_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} is missing or not text"))
}

fn integer_field(object: &serde_json::Map<String, Value>, field: &str) -> Result<u64, String> {
    let value = object
        .get(field)
        .ok_or_else(|| format!("{field} is missing"))?;
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    value
        .as_str()
        .ok_or_else(|| format!("{field} is not an integer"))?
        .parse()
        .map_err(|_| format!("{field} is not an unsigned integer"))
}

fn size_field(object: &serde_json::Map<String, Value>, field: &str) -> Result<u64, String> {
    parse_size(text_field(object, field)?).map_err(|message| format!("{field}: {message}"))
}

fn reclaimable_field(object: &serde_json::Map<String, Value>, field: &str) -> Result<u64, String> {
    let value = text_field(object, field)?;
    let size = value
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("{field} is empty"))?;
    parse_size(size).map_err(|message| format!("{field}: {message}"))
}

fn parse_size(value: &str) -> Result<u64, String> {
    let split = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .ok_or_else(|| format!("size {value:?} has no unit"))?;
    let amount: f64 = value[..split]
        .parse()
        .map_err(|_| format!("size {value:?} has an invalid number"))?;
    if !amount.is_finite() || amount < 0.0 {
        return Err(format!("size {value:?} is out of range"));
    }
    let multiplier = match value[split..].trim().to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "pb" => 1_000_000_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        unit => return Err(format!("size {value:?} has unsupported unit {unit:?}")),
    };
    let bytes = amount * multiplier;
    if bytes > u64::MAX as f64 {
        return Err(format!("size {value:?} is out of range"));
    }
    Ok(bytes.round() as u64)
}

fn unavailable(diagnostic: NativeDiagnostic) -> NativeReport {
    NativeReport {
        provider: PROVIDER.to_owned(),
        available: false,
        resources: Vec::new(),
        diagnostics: vec![diagnostic],
    }
}

fn diagnostic(kind: &str, message: String, retryable: bool) -> NativeDiagnostic {
    NativeDiagnostic {
        provider: PROVIDER.to_owned(),
        kind: kind.to_owned(),
        message: sanitize_message(&message),
        retryable,
    }
}

fn classify_failure(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission denied") {
        "permission_denied"
    } else if lower.contains("cannot connect")
        || lower.contains("is the docker daemon running")
        || lower.contains("docker daemon")
    {
        "daemon_unavailable"
    } else if lower.contains("context") || lower.contains("host") {
        "endpoint_unavailable"
    } else {
        "command_failed"
    }
}

fn diagnostic_text(bytes: &[u8]) -> String {
    sanitize_message(&String::from_utf8_lossy(bytes))
}

fn sanitize_message(message: &str) -> String {
    let flattened = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let compact = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(MAX_DIAGNOSTIC_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_supported_storage_class() {
        let cases = [
            (
                r#"{"Type":"Images","TotalCount":"12","Active":"3","Size":"1.5GB","Reclaimable":"750MB (50%)"}"#,
                "docker-images",
                1_500_000_000,
                750_000_000,
            ),
            (
                r#"{"Type":"Containers","TotalCount":2,"Active":1,"Size":"2KiB","Reclaimable":"1KiB (50%)"}"#,
                "docker-containers",
                2048,
                1024,
            ),
            (
                r#"{"Type":"Local Volumes","TotalCount":"1","Active":"0","Size":"0B","Reclaimable":"0B"}"#,
                "docker-volumes",
                0,
                0,
            ),
            (
                r#"{"Type":"Build Cache","TotalCount":"4","Active":"0","Size":"1TB","Reclaimable":"1TB (100%)"}"#,
                "docker-build-cache",
                1_000_000_000_000,
                1_000_000_000_000,
            ),
        ];

        for (line, kind, bytes, reclaimable_bytes) in cases {
            let resource = parse_resource(line).unwrap();
            assert_eq!(resource.kind, kind);
            assert_eq!(resource.bytes, bytes);
            assert_eq!(resource.reclaimable_bytes, reclaimable_bytes);
            assert!(!resource.cleanable);
        }
    }

    #[test]
    fn rejects_unknown_classes_and_invalid_sizes() {
        let unknown =
            r#"{"Type":"Secrets","TotalCount":"1","Active":"0","Size":"1B","Reclaimable":"1B"}"#;
        assert!(
            parse_resource(unknown)
                .unwrap_err()
                .contains("unknown storage class")
        );
        assert!(parse_size("12XB").unwrap_err().contains("unsupported unit"));
        assert!(parse_size("12").unwrap_err().contains("no unit"));
    }

    #[test]
    fn diagnostics_are_single_line_bounded_and_control_free() {
        let input = format!("first\nsecond\u{1b}[31m {}", "x".repeat(600));
        let output = sanitize_message(&input);
        assert!(!output.contains('\n'));
        assert!(!output.contains('\u{1b}'));
        assert!(output.chars().count() <= MAX_DIAGNOSTIC_CHARS);
    }

    #[test]
    fn bounded_reader_refuses_oversized_output() {
        let input = vec![b'x'; MAX_OUTPUT_BYTES as usize + 1];
        assert!(read_bounded(input.as_slice()).is_err());
    }

    #[test]
    fn output_collection_obeys_the_shared_deadline() {
        let (_tx, rx) = mpsc::channel::<Result<Vec<u8>, ReadOutputError>>();
        let error = receive_reader(rx, "stdout", Instant::now()).unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(error.retryable);
    }

    #[test]
    fn native_totals_saturate_on_adversarial_values() {
        let resource = NativeResource {
            provider: PROVIDER.to_owned(),
            scope: "daemon".to_owned(),
            kind: "docker-images".to_owned(),
            label: "Docker images".to_owned(),
            total_count: 1,
            active_count: 0,
            bytes: u64::MAX,
            reclaimable_bytes: u64::MAX,
            cleanable: false,
        };
        let report = NativeReport {
            provider: PROVIDER.to_owned(),
            available: true,
            resources: vec![resource.clone(), resource],
            diagnostics: Vec::new(),
        };

        assert_eq!(report.total_bytes(), u64::MAX);
        assert_eq!(report.total_reclaimable_bytes(), u64::MAX);
    }
}
