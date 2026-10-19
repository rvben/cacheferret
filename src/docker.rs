use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::colima;
use crate::{
    DockerImage, NativeCleanReport, NativeDiagnostic, NativeReport, NativeResource, SkippedImage,
    UnusedDockerImages,
};

const PROVIDER: &str = "docker";
// Large inventories and VM-backed daemons can take well over five seconds.
// The TUI runs this independently of filesystem discovery and interaction.
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(60);
const PRUNE_TIMEOUT: Duration = Duration::from_secs(120);
const IMAGE_REMOVAL_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// Inspect Docker-managed storage without changing daemon state.
pub fn inspect_docker() -> NativeReport {
    let output = match run_docker_command(
        &["system", "df", "--format", "json"],
        INSPECTION_TIMEOUT,
        "storage inspection",
    ) {
        Ok(output) => output,
        Err(diagnostic) => return unavailable(diagnostic),
    };

    if !output.status.success() {
        return unavailable(command_failure("storage inspection", &output));
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

/// Return a fresh Docker build-cache preview suitable for a confirmation gate.
pub fn preview_docker_build_cache() -> Result<NativeResource, NativeDiagnostic> {
    let report = inspect_docker();
    if !report.available {
        return Err(report.diagnostics.into_iter().next().unwrap_or_else(|| {
            diagnostic(
                "unavailable",
                "Docker build cache could not be inspected".to_owned(),
                true,
            )
        }));
    }
    report
        .resources
        .into_iter()
        .find(|resource| resource.kind == "docker-build-cache")
        .ok_or_else(|| {
            diagnostic(
                "missing_class",
                "Docker did not report build-cache storage".to_owned(),
                false,
            )
        })
}

/// Revalidate and prune Docker's ordinary build cache, excluding the broader
/// internal/frontend cache included by Docker's `--all` option.
pub fn prune_docker_build_cache() -> Result<NativeCleanReport, NativeDiagnostic> {
    let before = preview_docker_build_cache()?;
    if before.reclaimable_bytes == 0 {
        return Ok(NativeCleanReport::preview(before, false, true));
    }

    let output = run_docker_command(
        &["builder", "prune", "--force"],
        PRUNE_TIMEOUT,
        "build-cache prune",
    )?;
    if !output.status.success() {
        return Err(command_failure("build-cache prune", &output));
    }

    let reported_reclaimed_bytes =
        parse_reclaimed_bytes(&output.stdout).or_else(|| parse_reclaimed_bytes(&output.stderr));
    let mut diagnostics = Vec::new();
    let trim = match colima::trim_after_prune() {
        Some(Ok(trim)) => Some(trim),
        Some(Err(diagnostic)) => {
            diagnostics.push(diagnostic);
            None
        }
        None => None,
    };
    let after = match preview_docker_build_cache() {
        Ok(resource) => Some(resource),
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            None
        }
    };
    let estimated_removed_bytes = after
        .as_ref()
        .map_or(0, |resource| before.bytes.saturating_sub(resource.bytes));
    let changed =
        reported_reclaimed_bytes.is_some_and(|bytes| bytes > 0) || estimated_removed_bytes > 0;

    Ok(NativeCleanReport {
        provider: PROVIDER.to_owned(),
        kind: "docker-build-cache".to_owned(),
        changed,
        dry_run: false,
        confirmed: true,
        before,
        after,
        reported_reclaimed_bytes,
        estimated_removed_bytes,
        trim,
        images: Vec::new(),
        removed_images: Vec::new(),
        skipped_images: Vec::new(),
        diagnostics,
    })
}

/// List the tagged images that no container, running or stopped, was created
/// from, together with the `Images` storage class they belong to.
pub fn preview_unused_docker_images() -> Result<UnusedDockerImages, NativeDiagnostic> {
    let resource = images_resource()?;
    let (images, diagnostics) = list_images()?;
    Ok(UnusedDockerImages {
        resource,
        images: images.into_iter().filter_map(ListedImage::unused).collect(),
        diagnostics,
    })
}

/// Remove the confirmed images that are still unused, one `docker image rm`
/// per reference and never with `--force`, so Docker itself refuses an image
/// a container started using in the meantime. An image whose reference now
/// names a different image, or that a container uses, is kept and reported.
/// Containers, volumes and untagged images are never touched.
pub fn remove_unused_docker_images(
    confirmed: &[DockerImage],
) -> Result<NativeCleanReport, NativeDiagnostic> {
    let before = images_resource()?;
    let (current, mut diagnostics) = list_images()?;
    let (removed_images, skipped_images) = remove_still_unused(confirmed, &current, |reference| {
        let output = run_docker_command(
            &["image", "rm", "--", reference],
            IMAGE_REMOVAL_TIMEOUT,
            "image removal",
        )
        .map_err(|diagnostic| diagnostic.message)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("image removal", &output).message)
        }
    });

    let trim = if removed_images.is_empty() {
        None
    } else {
        match colima::trim_after_prune() {
            Some(Ok(trim)) => Some(trim),
            Some(Err(diagnostic)) => {
                diagnostics.push(diagnostic);
                None
            }
            None => None,
        }
    };
    let after = match images_resource() {
        Ok(resource) => Some(resource),
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            None
        }
    };
    let estimated_removed_bytes = after
        .as_ref()
        .map_or(0, |resource| before.bytes.saturating_sub(resource.bytes));

    Ok(NativeCleanReport {
        provider: PROVIDER.to_owned(),
        kind: before.kind.clone(),
        changed: !removed_images.is_empty(),
        dry_run: false,
        confirmed: true,
        before,
        after,
        reported_reclaimed_bytes: None,
        estimated_removed_bytes,
        trim,
        images: confirmed.to_vec(),
        removed_images,
        skipped_images,
        diagnostics,
    })
}

/// Run `remove` for each confirmed image that the current listing still shows
/// under the same reference and ID with no container, and return the removed
/// references and the kept images with their reasons.
fn remove_still_unused(
    confirmed: &[DockerImage],
    current: &[ListedImage],
    mut remove: impl FnMut(&str) -> Result<(), String>,
) -> (Vec<String>, Vec<SkippedImage>) {
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for image in confirmed {
        let listed = current
            .iter()
            .find(|listed| listed.image.reference == image.reference);
        let outcome = match listed {
            None => Err("no longer present".to_owned()),
            Some(listed) if listed.image.id != image.id => {
                Err("the reference now names a different image".to_owned())
            }
            Some(listed) if listed.containers != Some(0) => {
                Err("a container now uses this image".to_owned())
            }
            Some(_) => remove(&image.reference),
        };
        match outcome {
            Ok(()) => removed.push(image.reference.clone()),
            Err(reason) => skipped.push(SkippedImage {
                reference: image.reference.clone(),
                reason,
            }),
        }
    }
    (removed, skipped)
}

fn images_resource() -> Result<NativeResource, NativeDiagnostic> {
    let report = inspect_docker();
    if !report.available {
        return Err(report.diagnostics.into_iter().next().unwrap_or_else(|| {
            diagnostic(
                "unavailable",
                "Docker images could not be inspected".to_owned(),
                true,
            )
        }));
    }
    report
        .resources
        .into_iter()
        .find(|resource| resource.kind == "docker-images")
        .ok_or_else(|| {
            diagnostic(
                "missing_class",
                "Docker did not report image storage".to_owned(),
                false,
            )
        })
}

/// A tagged image with the number of containers created from it; `None` when
/// Docker's count could not be read, which never counts as unused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListedImage {
    image: DockerImage,
    containers: Option<u64>,
}

impl ListedImage {
    fn unused(self) -> Option<DockerImage> {
        (self.containers == Some(0)).then_some(self.image)
    }
}

fn list_images() -> Result<(Vec<ListedImage>, Vec<NativeDiagnostic>), NativeDiagnostic> {
    let output = run_docker_command(
        &["system", "df", "--verbose", "--format", "json"],
        INSPECTION_TIMEOUT,
        "image inspection",
    )?;
    if !output.status.success() {
        return Err(command_failure("image inspection", &output));
    }
    parse_images(&String::from_utf8_lossy(&output.stdout))
        .map_err(|message| diagnostic("invalid_output", message, false))
}

/// Read the `Images` array of `docker system df --verbose --format json`.
/// Untagged images are left out: they have no reference to remove them by,
/// and `docker image prune` is the tool for them.
fn parse_images(json: &str) -> Result<(Vec<ListedImage>, Vec<NativeDiagnostic>), String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("invalid JSON ({error})"))?;
    let rows = value
        .get("Images")
        .and_then(Value::as_array)
        .ok_or_else(|| "Images is missing or not a list".to_owned())?;
    let mut images = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| format!("image {} is not a JSON object", index + 1))?;
        let repository = text_field(object, "Repository")?;
        let tag = text_field(object, "Tag")?;
        if repository == "<none>" || tag == "<none>" || repository.is_empty() || tag.is_empty() {
            continue;
        }
        let reference = format!("{repository}:{tag}");
        let containers = integer_field(object, "Containers").ok();
        if containers.is_none() {
            diagnostics.push(diagnostic(
                "invalid_output",
                format!("Docker did not report how many containers use {reference}; it is kept"),
                false,
            ));
        }
        images.push(ListedImage {
            image: DockerImage {
                reference,
                id: text_field(object, "ID")?.to_owned(),
                bytes: size_field(object, "Size")?,
                unique_bytes: size_field(object, "UniqueSize")?,
                created: object
                    .get("CreatedSince")
                    .and_then(Value::as_str)
                    .filter(|created| !created.is_empty())
                    .map(str::to_owned),
            },
            containers,
        });
    }
    images.sort_by(|left, right| {
        right
            .image
            .unique_bytes
            .cmp(&left.image.unique_bytes)
            .then_with(|| left.image.reference.cmp(&right.image.reference))
    });
    Ok((images, diagnostics))
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

fn run_docker_command(
    args: &[&str],
    timeout: Duration,
    action: &str,
) -> Result<ProcessOutput, NativeDiagnostic> {
    let started = Instant::now();
    let mut child = Command::new("docker")
        .args(args)
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
                    format!("Docker {action} could not start: {error}"),
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
            Ok(None) if started.elapsed() < timeout => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(diagnostic(
                    "timeout",
                    format!(
                        "Docker {action} timed out after {} seconds",
                        timeout.as_secs()
                    ),
                    true,
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(diagnostic(
                    "unavailable",
                    format!("Docker {action} could not be observed: {error}"),
                    true,
                ));
            }
        }
    };

    let deadline = started + timeout;
    let stdout = receive_reader(stdout_reader, "stdout", deadline, action, timeout)?;
    let stderr = receive_reader(stderr_reader, "stderr", deadline, action, timeout)?;
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
    action: &str,
    timeout: Duration,
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
            format!(
                "Docker {action} timed out after {} seconds",
                timeout.as_secs()
            ),
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
        cleanable: kind == "docker-build-cache",
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

fn parse_reclaimed_bytes(output: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(output)
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim();
            let lower = line.to_ascii_lowercase();
            ["total reclaimed space:", "total:"]
                .into_iter()
                .find_map(|prefix| {
                    lower
                        .strip_prefix(prefix)
                        .and_then(|_| line.get(prefix.len()..))
                        .and_then(|value| parse_size(value.trim()).ok())
                })
        })
}

fn command_failure(action: &str, output: &ProcessOutput) -> NativeDiagnostic {
    let detail = diagnostic_text(&output.stderr);
    let message = if detail.is_empty() {
        format!("Docker {action} exited with {}", output.status)
    } else {
        detail
    };
    diagnostic(classify_failure(&message), message, true)
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
            assert_eq!(resource.cleanable, kind == "docker-build-cache");
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
        let error = receive_reader(
            rx,
            "stdout",
            Instant::now(),
            "storage inspection",
            INSPECTION_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(error.retryable);
    }

    #[test]
    fn parses_optional_owner_reported_reclaimed_space() {
        assert_eq!(
            parse_reclaimed_bytes(b"Total reclaimed space: 1.5GB\n"),
            Some(1_500_000_000)
        );
        assert_eq!(
            parse_reclaimed_bytes(b"ID\tRECLAIMABLE\nTotal:\t2MiB\n"),
            Some(2 * 1024 * 1024)
        );
        assert_eq!(parse_reclaimed_bytes(b"prune complete\n"), None);
    }

    fn image_row(repository: &str, tag: &str, id: &str, containers: &str, unique: &str) -> String {
        format!(
            r#"{{"Containers":"{containers}","CreatedAt":"2026-10-13 19:53:14 +0200 CEST","CreatedSince":"5 days ago","Digest":"","ID":"{id}","Repository":"{repository}","SharedSize":"12.93MB","Size":"361MB","Tag":"{tag}","UniqueSize":"{unique}"}}"#
        )
    }

    fn images_json(rows: &[String]) -> String {
        format!(
            r#"{{"Images":[{}],"Containers":[],"Volumes":[],"BuildCache":[]}}"#,
            rows.join(",")
        )
    }

    fn unused(json: &str) -> Vec<DockerImage> {
        let (images, _) = parse_images(json).unwrap();
        images.into_iter().filter_map(ListedImage::unused).collect()
    }

    #[test]
    fn lists_tagged_images_no_container_uses_largest_first() {
        let json = images_json(&[
            image_row(
                "registry.example/app",
                "latest",
                "sha256:bb",
                "0",
                "347.8MB",
            ),
            image_row("golang", "1.26", "sha256:aa", "0", "1.302GB"),
            image_row("postgres", "17", "sha256:cc", "2", "400MB"),
        ]);

        assert_eq!(
            unused(&json),
            vec![
                DockerImage {
                    reference: "golang:1.26".to_owned(),
                    id: "sha256:aa".to_owned(),
                    bytes: 361_000_000,
                    unique_bytes: 1_302_000_000,
                    created: Some("5 days ago".to_owned()),
                },
                DockerImage {
                    reference: "registry.example/app:latest".to_owned(),
                    id: "sha256:bb".to_owned(),
                    bytes: 361_000_000,
                    unique_bytes: 347_800_000,
                    created: Some("5 days ago".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn untagged_images_and_unreadable_container_counts_are_never_unused() {
        let json = images_json(&[
            image_row("<none>", "<none>", "sha256:aa", "0", "1GB"),
            image_row("app", "<none>", "sha256:bb", "0", "1GB"),
            image_row("app", "v1", "sha256:cc", "N/A", "1GB"),
            image_row("app", "v2", "sha256:dd", "0", "1GB"),
        ]);

        let (images, diagnostics) = parse_images(&json).unwrap();
        let unused = images
            .into_iter()
            .filter_map(ListedImage::unused)
            .map(|image| image.reference)
            .collect::<Vec<_>>();

        assert_eq!(unused, ["app:v2"]);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("app:v1"), "{diagnostics:?}");
    }

    #[test]
    fn rejects_image_listings_it_cannot_read() {
        assert!(parse_images("not json").is_err());
        assert!(parse_images(r#"{"Containers":[]}"#).is_err());
        let bad_size = images_json(&[image_row("app", "v1", "sha256:aa", "0", "huge")]);
        assert!(parse_images(&bad_size).is_err());
    }

    #[test]
    fn unique_bytes_count_each_image_id_once() {
        let json = images_json(&[
            image_row("app", "v1", "sha256:aa", "0", "100MB"),
            image_row("app", "latest", "sha256:aa", "0", "100MB"),
            image_row("tool", "1", "sha256:bb", "0", "50MB"),
        ]);
        assert_eq!(crate::unique_image_bytes(&unused(&json)), 150_000_000);
    }

    fn listed(reference: &str, id: &str, containers: Option<u64>) -> ListedImage {
        ListedImage {
            image: DockerImage {
                reference: reference.to_owned(),
                id: id.to_owned(),
                bytes: 1,
                unique_bytes: 1,
                created: None,
            },
            containers,
        }
    }

    #[test]
    fn removes_only_confirmed_images_that_are_still_unused_and_unchanged() {
        let confirmed = [
            listed("kept:v1", "sha256:aa", Some(0)).image,
            listed("retagged:v1", "sha256:bb", Some(0)).image,
            listed("started:v1", "sha256:cc", Some(0)).image,
            listed("gone:v1", "sha256:dd", Some(0)).image,
            listed("unreadable:v1", "sha256:ee", Some(0)).image,
            listed("refused:v1", "sha256:ff", Some(0)).image,
        ];
        let current = [
            listed("kept:v1", "sha256:aa", Some(0)),
            listed("retagged:v1", "sha256:b2", Some(0)),
            listed("started:v1", "sha256:cc", Some(1)),
            listed("unreadable:v1", "sha256:ee", None),
            listed("refused:v1", "sha256:ff", Some(0)),
            listed("new:v1", "sha256:99", Some(0)),
        ];
        let mut asked = Vec::new();

        let (removed, skipped) = remove_still_unused(&confirmed, &current, |reference| {
            asked.push(reference.to_owned());
            if reference == "refused:v1" {
                Err("conflict: unable to remove repository reference".to_owned())
            } else {
                Ok(())
            }
        });

        assert_eq!(asked, ["kept:v1", "refused:v1"], "only these reach docker");
        assert_eq!(removed, ["kept:v1"]);
        let reasons = skipped
            .iter()
            .map(|image| (image.reference.as_str(), image.reason.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            reasons,
            [
                ("retagged:v1", "the reference now names a different image"),
                ("started:v1", "a container now uses this image"),
                ("gone:v1", "no longer present"),
                ("unreadable:v1", "a container now uses this image"),
                (
                    "refused:v1",
                    "conflict: unable to remove repository reference"
                ),
            ]
        );
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
