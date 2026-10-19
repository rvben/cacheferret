//! Returning freed Docker space to the host on Colima.
//!
//! Colima mounts the Docker data disk without discard, so a prune frees space
//! inside the VM while the host disk image keeps its allocation until the
//! filesystem is trimmed. The trim only runs when the active Docker endpoint is
//! a Colima socket; Docker Desktop, OrbStack and plain Linux daemons manage
//! their own disk reclamation.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::process;
use crate::{NativeDiagnostic, NativeTrim};

const PROVIDER: &str = "colima";
const METHOD: &str = "colima-fstrim";
const DOCKER_DATA_DIR: &str = "/var/lib/docker";
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const TRIM_TIMEOUT: Duration = Duration::from_secs(180);

/// Trim the Docker data disk after a prune or image removal when the active
/// Docker endpoint is managed by Colima. `None` means the endpoint is not
/// Colima (or could not be determined) and no trim applies.
pub(crate) fn trim_after_prune() -> Option<Result<NativeTrim, NativeDiagnostic>> {
    let profile = active_profile()?;
    Some(trim(&profile))
}

/// The endpoint the Docker CLI itself would use: `DOCKER_HOST` takes precedence
/// over `DOCKER_CONTEXT` and the configured current context, so the context is
/// only consulted when `DOCKER_HOST` is unset or empty.
fn active_profile() -> Option<String> {
    let endpoint = match std::env::var("DOCKER_HOST") {
        Ok(host) if !host.trim().is_empty() => host,
        _ => context_endpoint()?,
    };
    profile_from_endpoint(&endpoint)
}

fn context_endpoint() -> Option<String> {
    let output = process::run("docker", &["context", "inspect"], INSPECTION_TIMEOUT).ok()?;
    if !output.success {
        return None;
    }
    endpoint_from_context_json(&output.stdout_text())
}

fn endpoint_from_context_json(json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(json).ok()?;
    value
        .get(0)?
        .pointer("/Endpoints/docker/Host")?
        .as_str()
        .map(str::to_owned)
}

/// Extract the profile name from a Colima socket endpoint such as
/// `unix:///Users/example/.colima/work/docker.sock`.
fn profile_from_endpoint(endpoint: &str) -> Option<String> {
    let path = Path::new(endpoint.trim().strip_prefix("unix://")?);
    if !path.is_absolute() {
        return None;
    }
    let mut components = path
        .components()
        .map(|component| component.as_os_str().to_str());
    let socket = components.next_back()??;
    let profile = components.next_back()??;
    let home = components.next_back()??;
    let well_formed = path
        .components()
        .all(|component| !matches!(component, std::path::Component::ParentDir));
    (well_formed
        && socket == "docker.sock"
        && matches!(home, ".colima" | "colima")
        && valid_profile(profile))
    .then(|| profile.to_owned())
}

/// Profile names reach an argv, so only the characters Colima itself accepts
/// are allowed and a leading `-` can never be read as a flag.
fn valid_profile(profile: &str) -> bool {
    !profile.is_empty()
        && !profile.starts_with(['-', '.'])
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The trim is preceded by a sync: ext4 cannot discard blocks freed in a
/// journal transaction that has not committed yet (up to 5 seconds after the
/// prune), so a bare `fstrim` run straight after the prune trims nothing and
/// still exits 0.
fn trim(profile: &str) -> Result<NativeTrim, NativeDiagnostic> {
    let output = process::run(
        "colima",
        &[
            "ssh",
            "--profile",
            profile,
            "--",
            "sudo",
            "sh",
            "-c",
            &format!("sync && fstrim -v {DOCKER_DATA_DIR}"),
        ],
        TRIM_TIMEOUT,
    )
    .map_err(|error| failure(&error.describe("colima", "fstrim"), error.retryable()))?;
    if !output.success {
        return Err(failure(&output.failure_detail(), true));
    }
    Ok(NativeTrim {
        method: METHOD.to_owned(),
        profile: profile.to_owned(),
        trim_reported_bytes: parse_trimmed_bytes(&output.stdout_text()),
    })
}

fn failure(message: &str, retryable: bool) -> NativeDiagnostic {
    NativeDiagnostic {
        provider: PROVIDER.to_owned(),
        kind: "trim_failed".to_owned(),
        message: process::sanitize_message(&format!(
            "Docker data was removed, but returning the freed space to the host failed: {message}"
        )),
        retryable,
    }
}

/// Read the byte count from `fstrim -v` output such as
/// `/var/lib/docker: 12.3 GiB (13207339008 bytes) trimmed on /dev/vdb`.
fn parse_trimmed_bytes(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let head = line.split(" bytes) trimmed").next()?;
        if head.len() == line.len() {
            return None;
        }
        head.rsplit('(').next()?.trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::RunError;

    #[test]
    fn detects_the_default_and_named_profiles() {
        for (endpoint, profile) in [
            (
                "unix:///Users/example/.colima/default/docker.sock",
                "default",
            ),
            ("unix:///Users/example/.colima/work/docker.sock", "work"),
            (
                "unix:///home/example/.config/colima/dev.1/docker.sock",
                "dev.1",
            ),
        ] {
            assert_eq!(profile_from_endpoint(endpoint).as_deref(), Some(profile));
        }
    }

    #[test]
    fn other_runtimes_and_malformed_endpoints_are_not_colima() {
        for endpoint in [
            "unix:///var/run/docker.sock",
            "unix:///Users/example/.docker/run/docker.sock",
            "unix:///Users/example/.orbstack/run/docker.sock",
            "unix:///Users/example/.rd/docker.sock",
            "tcp://192.0.2.10:2375",
            "ssh://example@192.0.2.10",
            "",
            "unix://.colima/default/docker.sock",
            "unix:///Users/example/.colima/default/other.sock",
            "unix:///Users/example/.colima/docker.sock",
            "unix:///Users/example/.colima/-flag/docker.sock",
            "unix:///Users/example/.colima/../default/docker.sock",
            "unix:///Users/example/.colima/a b/docker.sock",
            "unix:///Users/example/.colima/default/sub/docker.sock",
        ] {
            assert_eq!(profile_from_endpoint(endpoint), None, "{endpoint}");
        }
    }

    #[test]
    fn reads_the_endpoint_from_context_inspect_output() {
        let json = r#"[{"Name":"colima","Endpoints":{"docker":{"Host":"unix:///Users/example/.colima/default/docker.sock","SkipTLSVerify":false}}}]"#;
        assert_eq!(
            endpoint_from_context_json(json).as_deref(),
            Some("unix:///Users/example/.colima/default/docker.sock")
        );
        assert_eq!(endpoint_from_context_json("[]"), None);
        assert_eq!(endpoint_from_context_json("not json"), None);
    }

    #[test]
    fn parses_fstrim_verbose_output() {
        assert_eq!(
            parse_trimmed_bytes(
                "/var/lib/docker: 12.3 GiB (13207339008 bytes) trimmed on /dev/vdb\n"
            ),
            Some(13_207_339_008)
        );
        assert_eq!(
            parse_trimmed_bytes("/var/lib/docker: 0 B (0 bytes) trimmed\n"),
            Some(0)
        );
        assert_eq!(parse_trimmed_bytes("trim finished\n"), None);
        assert_eq!(parse_trimmed_bytes(""), None);
    }

    #[test]
    fn a_trim_failure_is_a_bounded_retryable_diagnostic() {
        let diagnostic = failure(
            &RunError::Timeout(TRIM_TIMEOUT).describe("colima", "fstrim"),
            true,
        );
        assert_eq!(diagnostic.provider, "colima");
        assert_eq!(diagnostic.kind, "trim_failed");
        assert!(diagnostic.retryable);
        assert!(
            diagnostic
                .message
                .starts_with("Docker data was removed, but returning the freed space")
        );
    }
}
