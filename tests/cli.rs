use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_cacheferret");

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Output {
    let out = Command::new(BIN).args(args).output().expect("spawn binary");
    Output {
        code: out.status.code().unwrap(),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

fn run_with_path(args: &[&str], path: &Path) -> Output {
    let out = Command::new(BIN)
        .args(args)
        .env("PATH", path)
        .output()
        .expect("spawn binary");
    Output {
        code: out.status.code().unwrap(),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

#[cfg(unix)]
fn docker_clean_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let docker = temp.path().join("docker");
    fs::write(
        &docker,
        r#"#!/bin/sh
case "$1 $2" in
  "system df")
    if [ -f "$CACHEFERRET_TEST_STATE" ]; then
      printf '%s\n' '{"Type":"Build Cache","TotalCount":"1","Active":"0","Size":"250MB","Reclaimable":"0B (0%)"}'
    else
      printf '%s\n' '{"Type":"Build Cache","TotalCount":"8","Active":"0","Size":"1GB","Reclaimable":"750MB (75%)"}'
    fi
    ;;
  "builder prune")
    printf '%s\n' "$*" > "$CACHEFERRET_TEST_LOG"
    : > "$CACHEFERRET_TEST_STATE"
    printf '%s\n' 'Total reclaimed space: 750MB'
    ;;
  *)
    printf '%s\n' "unexpected docker invocation: $*" >&2
    exit 42
    ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    let state = temp.path().join("pruned");
    let log = temp.path().join("argv");
    (temp, state, log)
}

#[cfg(unix)]
fn run_docker_clean(args: &[&str], temp: &TempDir, state: &Path, log: &Path) -> Output {
    let out = Command::new(BIN)
        .args(args)
        .env("PATH", temp.path())
        .env_remove("DOCKER_HOST")
        .env("CACHEFERRET_TEST_STATE", state)
        .env("CACHEFERRET_TEST_LOG", log)
        .output()
        .expect("spawn binary");
    Output {
        code: out.status.code().unwrap(),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

fn error_envelope(stderr: &str) -> serde_json::Value {
    let last = stderr.lines().last().expect("stderr has an error line");
    serde_json::from_str::<serde_json::Value>(last).expect("error envelope is JSON")["error"]
        .clone()
}

fn cargo_fixture() -> (TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("demo");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/app"), [7_u8; 64]).unwrap();
    let root = temp.path().to_string_lossy().into_owned();
    (temp, root)
}

#[test]
fn schema_is_clispec_v0_3() {
    let out = run(&["schema"]);
    assert_eq!(out.code, 0);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["clispec"], "0.3");
    assert_eq!(value["output"]["piped"], "json");
}

#[test]
fn schema_can_be_narrowed_to_a_command() {
    let out = run(&["schema", "clean"]);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["commands"].as_array().unwrap().len(), 1);
    assert_eq!(value["commands"][0]["name"], "clean");
}

#[test]
fn help_mentions_schema_and_safe_default() {
    let out = run(&["--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("schema"));
    assert!(out.stdout.contains("without a command opens the TUI"));
    assert!(out.stdout.contains("delete the focused entry"));
}

#[test]
fn tui_requires_an_interactive_terminal() {
    let out = run(&["tui"]);
    assert_eq!(out.code, 3);
    assert_eq!(error_envelope(&out.stderr)["kind"], "usage");
    assert!(out.stderr.contains("needs an interactive terminal"));
}

#[test]
fn tui_rejects_explicit_json_output() {
    let out = run(&["tui", "--output", "json"]);
    assert_eq!(out.code, 3);
    assert_eq!(error_envelope(&out.stderr)["kind"], "usage");
    assert!(out.stderr.contains("does not produce JSON"));
}

#[test]
fn scan_emits_paginated_json_when_piped() {
    let (_temp, root) = cargo_fixture();
    let out = run(&[
        "scan",
        "--root",
        &root,
        "--scope",
        "project",
        "--protect-days",
        "0",
        "--limit",
        "10",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["items"][0]["kind"], "cargo-target");
    assert_eq!(value["items"][0]["bytes"], 64);
    assert!(value["items"][0]["allocated_bytes"].as_u64().unwrap() >= 64);
    assert!(value["total_allocated_bytes"].as_u64().unwrap() >= 64);
    assert_eq!(value["truncated"], false);
}

#[test]
fn fields_projects_candidate_records() {
    let (_temp, root) = cargo_fixture();
    let out = run(&[
        "scan",
        "--root",
        &root,
        "--scope",
        "project",
        "--fields",
        "kind,bytes",
    ]);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let item = value["items"][0].as_object().unwrap();
    assert_eq!(item.len(), 2);
    assert!(item.contains_key("kind"));
    assert!(item.contains_key("bytes"));
}

#[test]
fn catalog_is_paginated_and_projectable() {
    let out = run(&[
        "catalog",
        "--limit",
        "2",
        "--offset",
        "1",
        "--fields",
        "kind,cleanable",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert!(value["total"].as_u64().unwrap() > 2);
    assert_eq!(value["returned"], 2);
    assert_eq!(value["offset"], 1);
    assert_eq!(value["limit"], 2);
    assert_eq!(value["truncated"], true);
    for item in value["items"].as_array().unwrap() {
        assert_eq!(item.as_object().unwrap().len(), 2);
        assert!(item.get("kind").is_some());
        assert!(item.get("cleanable").is_some());
    }
}

#[test]
#[cfg(unix)]
fn docker_inspection_is_paginated_projectable_and_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let docker = temp.path().join("docker");
    fs::write(
        &docker,
        r#"#!/bin/sh
printf '%s\n' \
'{"Type":"Images","TotalCount":"12","Active":"3","Size":"1.5GB","Reclaimable":"750MB (50%)"}' \
'{"Type":"Containers","TotalCount":"2","Active":"1","Size":"2MB","Reclaimable":"0B (0%)"}' \
'{"Type":"Local Volumes","TotalCount":"4","Active":"2","Size":"700MB","Reclaimable":"500MB (71%)"}' \
'{"Type":"Build Cache","TotalCount":"8","Active":"0","Size":"1GB","Reclaimable":"1GB (100%)"}'
"#,
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();

    let out = run_with_path(
        &[
            "docker",
            "--limit",
            "2",
            "--offset",
            "1",
            "--fields",
            "kind,reclaimable_bytes",
        ],
        temp.path(),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["provider"], "docker");
    assert_eq!(value["available"], true);
    assert_eq!(value["total"], 4);
    assert_eq!(value["returned"], 2);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["items"][0]["kind"], "docker-images");
    assert_eq!(value["items"][0]["reclaimable_bytes"], 750_000_000_u64);
    assert_eq!(value["items"][0].as_object().unwrap().len(), 2);
    assert_eq!(value["items"][1]["kind"], "docker-volumes");
    assert_eq!(value["diagnostics"].as_array().unwrap().len(), 0);
}

#[test]
fn missing_docker_is_a_structured_nonfatal_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let out = run_with_path(&["docker"], temp.path());

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(value["total"], 0);
    assert_eq!(value["diagnostics"][0]["provider"], "docker");
    assert_eq!(value["diagnostics"][0]["kind"], "not_found");
    assert_eq!(value["diagnostics"][0]["retryable"], false);
}

#[test]
#[cfg(unix)]
fn docker_clean_dry_run_previews_without_pruning() {
    let (temp, state, log) = docker_clean_fixture();
    let out = run_docker_clean(
        &[
            "docker",
            "clean",
            "--dry-run",
            "--fields",
            "kind,dry_run,before",
        ],
        &temp,
        &state,
        &log,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.is_empty());
    assert!(!state.exists());
    assert!(!log.exists());
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert_eq!(value["kind"], "docker-build-cache");
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["before"]["reclaimable_bytes"], 750_000_000_u64);
}

#[test]
#[cfg(unix)]
fn docker_clean_requires_confirmation_when_piped() {
    let (temp, state, log) = docker_clean_fixture();
    let out = run_docker_clean(&["docker", "clean"], &temp, &state, &log);

    assert_eq!(out.code, 6);
    let error = error_envelope(&out.stderr);
    assert_eq!(error["kind"], "confirmation_required");
    assert_eq!(error["retryable"], false);
    assert!(!state.exists());
    assert!(!log.exists());
}

#[test]
fn docker_clean_reports_missing_docker_as_retryable_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let out = run_with_path(&["docker", "clean", "--dry-run"], temp.path());

    assert_eq!(out.code, 7);
    let error = error_envelope(&out.stderr);
    assert_eq!(error["kind"], "native_unavailable");
    assert_eq!(error["retryable"], true);
}

#[test]
#[cfg(unix)]
fn docker_clean_yes_executes_only_bounded_build_cache_prune() {
    let (temp, state, log) = docker_clean_fixture();
    let out = run_docker_clean(&["docker", "clean", "--yes"], &temp, &state, &log);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(state.exists());
    assert_eq!(
        fs::read_to_string(log).unwrap().trim(),
        "builder prune --force"
    );
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["confirmed"], true);
    assert_eq!(value["changed"], true);
    assert_eq!(value["reported_reclaimed_bytes"], 750_000_000_u64);
    assert_eq!(value["after"]["reclaimable_bytes"], 0);
}

/// A fixture where the Docker CLI reports `$CACHEFERRET_TEST_ENDPOINT` as the
/// active context endpoint and a fake `colima` records its argv.
#[cfg(unix)]
fn colima_fixture() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (temp, state, log) = docker_clean_fixture();
    let docker = temp.path().join("docker");
    let script = fs::read_to_string(&docker).unwrap().replace(
        "  *)\n",
        "  \"context inspect\")\n    printf '[{\"Endpoints\":{\"docker\":{\"Host\":\"%s\"}}}]\\n' \"$CACHEFERRET_TEST_ENDPOINT\"\n    ;;\n  *)\n",
    );
    fs::write(&docker, script).unwrap();
    let colima_log = temp.path().join("colima-argv");
    let colima = temp.path().join("colima");
    fs::write(
        &colima,
        r#"#!/bin/sh
if [ ! -f "$CACHEFERRET_TEST_STATE" ]; then
  printf '%s\n' "trim ran before the prune" >&2
  exit 9
fi
printf '%s\n' "$*" > "$CACHEFERRET_TEST_COLIMA_LOG"
if [ -n "$CACHEFERRET_TEST_TRIM_FAILS" ]; then
  printf '%s\n' "sudo: a password is required" >&2
  exit 1
fi
printf '%s\n' "${CACHEFERRET_TEST_TRIM_OUTPUT:-/var/lib/docker: 700 MiB (734003200 bytes) trimmed on /dev/vdb}"
"#,
    )
    .unwrap();
    fs::set_permissions(&colima, fs::Permissions::from_mode(0o755)).unwrap();
    (temp, state, log, colima_log)
}

#[cfg(unix)]
fn run_colima_clean(
    temp: &TempDir,
    state: &Path,
    log: &Path,
    colima_log: &Path,
    envs: &[(&str, &str)],
) -> serde_json::Value {
    let mut command = Command::new(BIN);
    command
        .args(["docker", "clean", "--yes"])
        .env("PATH", temp.path())
        .env_remove("DOCKER_HOST")
        .env("CACHEFERRET_TEST_STATE", state)
        .env("CACHEFERRET_TEST_LOG", log)
        .env("CACHEFERRET_TEST_COLIMA_LOG", colima_log);
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("spawn binary");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
#[cfg(unix)]
fn docker_clean_trims_the_colima_disk_after_the_prune() {
    let (temp, state, log, colima_log) = colima_fixture();
    let value = run_colima_clean(
        &temp,
        &state,
        &log,
        &colima_log,
        &[(
            "CACHEFERRET_TEST_ENDPOINT",
            "unix:///Users/example/.colima/default/docker.sock",
        )],
    );

    assert_eq!(
        fs::read_to_string(&colima_log).unwrap().trim(),
        "ssh --profile default -- sudo fstrim -v /var/lib/docker"
    );
    assert_eq!(value["changed"], true);
    assert_eq!(value["trim"]["method"], "colima-fstrim");
    assert_eq!(value["trim"]["profile"], "default");
    assert_eq!(value["trim"]["trim_reported_bytes"], 734_003_200_u64);
    assert_eq!(value["diagnostics"].as_array().unwrap().len(), 0);
}

/// The human-readable lines a Colima prune prints about the trim.
#[cfg(unix)]
fn trim_text_lines(trim_output: &str) -> Vec<String> {
    let (temp, state, log, colima_log) = colima_fixture();
    let out = Command::new(BIN)
        .args(["--output", "text", "docker", "clean", "--yes"])
        .env("PATH", temp.path())
        .env_remove("DOCKER_HOST")
        .env("CACHEFERRET_TEST_STATE", &state)
        .env("CACHEFERRET_TEST_LOG", &log)
        .env("CACHEFERRET_TEST_COLIMA_LOG", &colima_log)
        .env("CACHEFERRET_TEST_TRIM_OUTPUT", trim_output)
        .env(
            "CACHEFERRET_TEST_ENDPOINT",
            "unix:///Users/example/.colima/default/docker.sock",
        )
        .output()
        .expect("spawn binary");
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.contains("fstrim"))
        .map(str::to_owned)
        .collect()
}

#[test]
#[cfg(unix)]
fn the_text_output_labels_the_fstrim_count_as_reported_not_reclaimed() {
    let lines = trim_text_lines("/var/lib/docker: 700 MiB (734003200 bytes) trimmed on /dev/vdb");
    assert_eq!(
        lines,
        [
            "fstrim reported 700.0 MiB trimmed (Colima profile default); not a measure of space returned to the host"
        ]
    );
    assert!(!lines[0].contains("reclaimed") && !lines[0].contains("Returned"));
}

#[test]
#[cfg(unix)]
fn the_text_output_without_an_fstrim_count_claims_no_amount() {
    let lines = trim_text_lines("trim finished");
    assert_eq!(
        lines,
        ["Ran fstrim to return freed space to the host (Colima profile default)"]
    );
    assert!(!lines[0].contains("reclaimed") && !lines[0].contains("freed 0"));
}

#[test]
#[cfg(unix)]
fn docker_clean_trims_the_named_colima_profile_from_docker_host() {
    let (temp, state, log, colima_log) = colima_fixture();
    let value = run_colima_clean(
        &temp,
        &state,
        &log,
        &colima_log,
        &[
            (
                "DOCKER_HOST",
                "unix:///Users/example/.colima/work/docker.sock",
            ),
            ("CACHEFERRET_TEST_ENDPOINT", "unix:///var/run/docker.sock"),
        ],
    );

    assert_eq!(
        fs::read_to_string(&colima_log).unwrap().trim(),
        "ssh --profile work -- sudo fstrim -v /var/lib/docker"
    );
    assert_eq!(value["trim"]["profile"], "work");
}

#[test]
#[cfg(unix)]
fn docker_host_decides_over_a_colima_docker_context() {
    // Docker gives DOCKER_HOST precedence over DOCKER_CONTEXT, so a Colima
    // context selected alongside a non-Colima DOCKER_HOST must not be trimmed.
    let (temp, state, log, colima_log) = colima_fixture();
    let value = run_colima_clean(
        &temp,
        &state,
        &log,
        &colima_log,
        &[
            (
                "DOCKER_HOST",
                "unix:///Users/example/.orbstack/run/docker.sock",
            ),
            ("DOCKER_CONTEXT", "colima"),
            (
                "CACHEFERRET_TEST_ENDPOINT",
                "unix:///Users/example/.colima/default/docker.sock",
            ),
        ],
    );

    assert!(!colima_log.exists());
    assert!(value["trim"].is_null(), "{value}");
}

#[test]
#[cfg(unix)]
fn docker_clean_does_not_trim_other_docker_runtimes() {
    for endpoint in [
        "unix:///Users/example/.orbstack/run/docker.sock",
        "unix:///Users/example/.docker/run/docker.sock",
        "unix:///var/run/docker.sock",
    ] {
        let (temp, state, log, colima_log) = colima_fixture();
        let value = run_colima_clean(
            &temp,
            &state,
            &log,
            &colima_log,
            &[("CACHEFERRET_TEST_ENDPOINT", endpoint)],
        );

        assert!(!colima_log.exists(), "{endpoint}");
        assert_eq!(value["changed"], true);
        assert!(value["trim"].is_null());
        assert_eq!(value["diagnostics"].as_array().unwrap().len(), 0);
    }
}

#[test]
#[cfg(unix)]
fn docker_clean_reports_a_failed_trim_as_a_warning_not_a_failure() {
    let (temp, state, log, colima_log) = colima_fixture();
    let value = run_colima_clean(
        &temp,
        &state,
        &log,
        &colima_log,
        &[
            (
                "CACHEFERRET_TEST_ENDPOINT",
                "unix:///Users/example/.colima/default/docker.sock",
            ),
            ("CACHEFERRET_TEST_TRIM_FAILS", "1"),
        ],
    );

    assert_eq!(value["changed"], true);
    assert_eq!(value["reported_reclaimed_bytes"], 750_000_000_u64);
    assert!(value["trim"].is_null());
    let diagnostic = &value["diagnostics"][0];
    assert_eq!(diagnostic["provider"], "colima");
    assert_eq!(diagnostic["kind"], "trim_failed");
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("password is required")
    );
}

#[test]
#[cfg(unix)]
fn docker_clean_dry_run_never_trims() {
    let (temp, state, log, colima_log) = colima_fixture();
    let out = Command::new(BIN)
        .args(["docker", "clean", "--dry-run"])
        .env("PATH", temp.path())
        .env(
            "DOCKER_HOST",
            "unix:///Users/example/.colima/default/docker.sock",
        )
        .env("CACHEFERRET_TEST_STATE", &state)
        .env("CACHEFERRET_TEST_LOG", &log)
        .env("CACHEFERRET_TEST_COLIMA_LOG", &colima_log)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    assert!(!colima_log.exists());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["trim"].is_null());
}

#[test]
#[cfg(unix)]
fn docker_clean_zero_reclaimable_is_a_confirmation_free_noop() {
    let (temp, state, log) = docker_clean_fixture();
    fs::write(&state, []).unwrap();
    let out = run_docker_clean(&["docker", "clean"], &temp, &state, &log);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!log.exists());
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["confirmed"], true);
    assert_eq!(value["changed"], false);
    assert_eq!(value["before"]["reclaimable_bytes"], 0);
}

#[test]
fn schema_describes_guarded_docker_clean() {
    let out = run(&["schema", "docker", "clean"]);
    assert_eq!(out.code, 0);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["commands"].as_array().unwrap().len(), 1);
    let command = &value["commands"][0];
    assert_eq!(command["name"], "docker clean");
    assert_eq!(command["mutating"], true);
    assert_eq!(command["confirmation_bypass_arg"], "--yes");
    assert!(
        command["description"]
            .as_str()
            .unwrap()
            .contains("Never prunes images")
    );
}

#[test]
fn clean_refuses_without_tty_or_yes() {
    let (_temp, root) = cargo_fixture();
    let out = run(&["clean", "--root", &root, "--protect-days", "0"]);
    assert_eq!(out.code, 6);
    assert_eq!(error_envelope(&out.stderr)["kind"], "confirmation_required");
    assert!(Path::new(&root).join("demo/target").exists());
}

#[test]
fn dry_run_reports_without_deleting() {
    let (_temp, root) = cargo_fixture();
    let out = run(&["clean", "--root", &root, "--protect-days", "0", "--dry-run"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["selected"], 1);
    assert_eq!(value["selected_targets"][0]["kind"], "cargo-target");
    assert_eq!(value["selected_targets"][0]["bytes"], 64);
    assert_eq!(value["apparent_bytes_selected"], 64);
    assert!(value["allocated_bytes_selected"].as_u64().unwrap() >= 64);
    assert!(
        value["selected_targets"][0]["allocated_bytes"]
            .as_u64()
            .unwrap()
            >= 64
    );
    let expected_path = Path::new(&root).canonicalize().unwrap().join("demo/target");
    assert_eq!(
        value["selected_targets"][0]["path"],
        expected_path.to_string_lossy().as_ref()
    );
    assert!(Path::new(&root).join("demo/target").exists());
}

#[test]
fn confirmed_clean_removes_only_cache() {
    let (_temp, root) = cargo_fixture();
    let out = run(&["clean", "--root", &root, "--protect-days", "0", "--yes"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["changed"], true);
    assert_eq!(value["cleaned"], 1);
    assert_eq!(value["apparent_bytes_removed"], 64);
    assert!(value["filesystem_deltas"].is_array());
    assert!(!Path::new(&root).join("demo/target").exists());
    assert!(Path::new(&root).join("demo/Cargo.toml").exists());
}

#[test]
fn invalid_kind_has_declared_error() {
    let out = run(&["scan", "--scope", "global", "--kind", "imaginary"]);
    assert_eq!(out.code, 2);
    assert_eq!(error_envelope(&out.stderr)["kind"], "invalid_input");
}

#[test]
#[cfg(target_os = "macos")]
fn device_support_entries_are_listed_and_removed_per_directory() {
    let home = tempfile::tempdir().unwrap();
    let support = home
        .path()
        .join("Library/Developer/Xcode/iOS DeviceSupport");
    let keep = support.join("iPhone15,2 26.1 (23B85)");
    let remove = support.join("iPhone15,2 26.2 (23C55)");
    for dir in [&keep, &remove] {
        fs::create_dir_all(dir.join("Symbols")).unwrap();
        fs::write(dir.join("Symbols/libsystem"), [1_u8; 128]).unwrap();
    }
    let home_arg = home.path().to_string_lossy().into_owned();
    let run_home = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .env("HOME", &home_arg)
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .expect("spawn binary");
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
        )
    };

    let (code, stdout) = run_home(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-device-support",
    ]);
    assert_eq!(code, 0);
    let scan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let items = scan["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item["ecosystem"] == "xcode"));
    assert!(items.iter().all(|item| item["network_restore"] == false));

    let (code, stdout) = run_home(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-device-support",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(code, 0, "{stdout}");
    assert!(!keep.exists() && !remove.exists());
    assert!(support.exists(), "the platform folder itself is preserved");
}

/// A rustup home with a default toolchain and two removable ones, plus a fake
/// `rustup` that answers `show home` and records and performs uninstalls.
#[cfg(unix)]
struct RustupFixture {
    temp: TempDir,
    home: std::path::PathBuf,
    log: std::path::PathBuf,
}

#[cfg(unix)]
impl RustupFixture {
    const DEFAULT: &'static str = "stable-aarch64-apple-darwin";
    const OLD: &'static str = "1.70.0-aarch64-apple-darwin";
    const NIGHTLY: &'static str = "nightly-2026-01-01-aarch64-apple-darwin";

    /// `failing` names a toolchain whose uninstall is refused by the fake.
    fn new(failing: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("rustup-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("settings.toml"),
            format!(
                "default_toolchain = \"{}\"\nprofile = \"default\"\nversion = \"12\"\n",
                Self::DEFAULT
            ),
        )
        .unwrap();
        for name in [Self::DEFAULT, Self::OLD, Self::NIGHTLY] {
            let bin = home.join("toolchains").join(name).join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("rustc"), [3_u8; 256]).unwrap();
        }
        let rustup = temp.path().join("rustup");
        let failing = failing.unwrap_or("none");
        let uninstall = format!(
            r#"if [ "$3" = "{failing}" ]; then echo "error: toolchain is in use" >&2; exit 1; fi; /bin/rm -rf "$CACHEFERRET_TEST_HOME/toolchains/$3""#
        );
        fs::write(
            &rustup,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$CACHEFERRET_TEST_LOG"
case "$1 $2" in
  "show home")
    if [ -f "$CACHEFERRET_TEST_HOME/settings-later.toml" ] && [ "$(/usr/bin/grep -c '^show home$' "$CACHEFERRET_TEST_LOG")" -ge 2 ]; then
      /bin/mv "$CACHEFERRET_TEST_HOME/settings-later.toml" "$CACHEFERRET_TEST_HOME/settings.toml"
    fi
    printf '%s\n' "$CACHEFERRET_TEST_HOME" ;;
  "toolchain uninstall") {uninstall} ;;
  *) echo "unexpected rustup invocation: $*" >&2; exit 42 ;;
esac
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
        let log = temp.path().join("argv");
        Self { temp, home, log }
    }

    fn toolchain(&self, name: &str) -> std::path::PathBuf {
        self.home.join("toolchains").join(name)
    }

    fn invocations(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = Command::new(BIN)
            .args(args)
            .env("PATH", self.temp.path())
            .env_remove("RUSTUP_TOOLCHAIN")
            .env("CACHEFERRET_TEST_HOME", &self.home)
            .env("CACHEFERRET_TEST_LOG", &self.log)
            .output()
            .expect("spawn binary");
        Output {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }
}

#[test]
#[cfg(unix)]
fn rustup_toolchains_are_listed_per_toolchain_without_the_default() {
    let fixture = RustupFixture::new(None);
    let out = fixture.run(&["scan", "--scope", "global", "--kind", "rust-toolchain"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let mut items: Vec<(String, String)> = scan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            assert_eq!(item["ecosystem"], "rust");
            assert_eq!(item["network_restore"], true);
            assert_eq!(item["bytes"], 256);
            (
                item["label"].as_str().unwrap().to_owned(),
                item["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    items.sort();
    assert_eq!(
        items,
        [
            (
                "1.70.0".to_owned(),
                fixture
                    .toolchain(RustupFixture::OLD)
                    .to_string_lossy()
                    .into_owned()
            ),
            (
                "nightly-2026-01-01".to_owned(),
                fixture
                    .toolchain(RustupFixture::NIGHTLY)
                    .to_string_lossy()
                    .into_owned()
            ),
        ]
    );
}

#[test]
#[cfg(unix)]
fn rustup_toolchain_selected_through_the_environment_is_not_listed() {
    let fixture = RustupFixture::new(None);
    let out = Command::new(BIN)
        .args(["scan", "--scope", "global", "--kind", "rust-toolchain"])
        .env("PATH", fixture.temp.path())
        .env("RUSTUP_TOOLCHAIN", "1.70.0")
        .env("CACHEFERRET_TEST_HOME", &fixture.home)
        .env("CACHEFERRET_TEST_LOG", &fixture.log)
        .output()
        .unwrap();
    let scan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let labels: Vec<&str> = scan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["nightly-2026-01-01"]);
}

#[test]
#[cfg(unix)]
fn a_toolchain_selected_by_absolute_path_is_not_listed() {
    let fixture = RustupFixture::new(None);
    let out = Command::new(BIN)
        .args(["scan", "--scope", "global", "--kind", "rust-toolchain"])
        .env("PATH", fixture.temp.path())
        .env(
            "RUSTUP_TOOLCHAIN",
            fixture.toolchain(RustupFixture::NIGHTLY),
        )
        .env("CACHEFERRET_TEST_HOME", &fixture.home)
        .env("CACHEFERRET_TEST_LOG", &fixture.log)
        .output()
        .unwrap();
    assert!(out.status.success());
    let scan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let labels: Vec<&str> = scan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["1.70.0"]);
}

#[test]
#[cfg(unix)]
fn a_toolchain_that_gets_selected_by_path_after_scanning_is_refused() {
    let fixture = RustupFixture::new(None);
    let later = format!(
        "default_toolchain = \"{}\"\n[overrides]\n\"/Users/example/app\" = \"{}\"\n",
        RustupFixture::DEFAULT,
        fixture.toolchain(RustupFixture::NIGHTLY).display()
    );
    fs::write(fixture.home.join("settings-later.toml"), later).unwrap();
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "rust-toolchain",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 1);
    assert_eq!(report["skipped"], 1);
    assert!(fixture.toolchain(RustupFixture::NIGHTLY).exists());
    assert!(!fixture.toolchain(RustupFixture::OLD).exists());
    assert!(
        !fixture
            .invocations()
            .iter()
            .any(|line| line.ends_with(RustupFixture::NIGHTLY))
    );
}

#[test]
#[cfg(unix)]
fn rustup_toolchains_are_removed_with_rustup_and_never_by_deleting_files() {
    let fixture = RustupFixture::new(None);
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "rust-toolchain",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 2);
    assert_eq!(report["network_restore_selected"], 2);
    assert!(!fixture.toolchain(RustupFixture::OLD).exists());
    assert!(!fixture.toolchain(RustupFixture::NIGHTLY).exists());
    assert!(fixture.toolchain(RustupFixture::DEFAULT).exists());

    let mut uninstalls: Vec<String> = fixture
        .invocations()
        .into_iter()
        .filter(|line| line.starts_with("toolchain uninstall"))
        .collect();
    uninstalls.sort();
    assert_eq!(
        uninstalls,
        [
            format!("toolchain uninstall {}", RustupFixture::OLD),
            format!("toolchain uninstall {}", RustupFixture::NIGHTLY),
        ]
    );
}

#[test]
#[cfg(unix)]
fn a_failed_rustup_uninstall_is_reported_and_leaves_the_toolchain() {
    let fixture = RustupFixture::new(Some(RustupFixture::NIGHTLY));
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "rust-toolchain",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 1);
    assert_eq!(report["skipped"], 1);
    let reason = report["skipped_paths"][0]["reason"].as_str().unwrap();
    assert!(reason.contains("toolchain is in use"), "{reason}");
    assert!(fixture.toolchain(RustupFixture::NIGHTLY).exists());
    assert!(!fixture.toolchain(RustupFixture::OLD).exists());
}

#[test]
#[cfg(unix)]
fn rustup_dry_run_never_uninstalls() {
    let fixture = RustupFixture::new(None);
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "rust-toolchain",
        "--protect-days",
        "0",
        "--dry-run",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        fixture
            .invocations()
            .iter()
            .all(|line| !line.starts_with("toolchain uninstall"))
    );
    assert!(fixture.toolchain(RustupFixture::OLD).exists());
}

#[test]
#[cfg(unix)]
fn rustup_settings_that_are_not_understood_offer_nothing() {
    let fixture = RustupFixture::new(None);
    fs::write(
        fixture.home.join("settings.toml"),
        "default_toolchain = stable\n",
    )
    .unwrap();
    let out = fixture.run(&["scan", "--scope", "global", "--kind", "rust-toolchain"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(scan["items"].as_array().unwrap().len(), 0);
    assert!(
        scan["warnings"][0]
            .as_str()
            .unwrap()
            .contains("not understood"),
        "{scan}"
    );
}

/// Fake `xcrun` serving recorded `simctl` JSON and performing deletions, with
/// simulator devices whose directories exist on disk. After the first listing
/// the fake serves `devices-later.json` and `runtimes-later.json` when they exist, so a state
/// change between scanning and cleaning can be reproduced.
#[cfg(unix)]
struct SimctlFixture {
    temp: TempDir,
    devices_root: std::path::PathBuf,
}

#[cfg(unix)]
impl SimctlFixture {
    const LARGE: &'static str = "2B5F8808-A600-48F0-9750-81CCBA294022";
    const SMALL: &'static str = "7A1C3F10-0000-4000-8000-000000000001";
    const IOS_RUNTIME: &'static str = "38BB7EA4-FB30-4597-A6EA-C38FEC38B89C";
    const WATCH_RUNTIME: &'static str = "5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2";
    /// 2023-11-14T22:13:20Z, far older than the protection window; the
    /// directories hold the current time.
    const APP_LAST_USED_UNIX: u64 = 1_700_000_000;
    const LOCKED_RUNTIME: &'static str = "15CCF121-0865-4533-B65E-582CB34C5835";

    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let devices_root = temp.path().join("CoreSimulator/Devices");
        for (udid, bytes) in [(Self::LARGE, 70 * 1024 * 1024), (Self::SMALL, 1024)] {
            let data = devices_root.join(udid).join("data");
            fs::create_dir_all(&data).unwrap();
            fs::File::create(data.join("store"))
                .unwrap()
                .set_len(bytes)
                .unwrap();
        }
        let large = devices_root.join(Self::LARGE);
        let app =
            large.join("data/Containers/Bundle/Application/33333333-3333-4333-8333-333333333333");
        fs::create_dir_all(app.join("Example.app")).unwrap();
        let container =
            large.join("data/Containers/Data/Application/44444444-4444-4444-8444-444444444444");
        fs::create_dir_all(&container).unwrap();
        // Files carry the last use; directory times are left at "now", as the
        // system rewrites them without the device being used.
        fs::write(container.join("state"), b"").unwrap();
        for file in [container.join("state"), large.join("data/store")] {
            fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs(Self::APP_LAST_USED_UNIX),
                )
                .unwrap();
        }
        let fixture = Self { temp, devices_root };
        fixture.set_devices("devices.json", "Shutdown");
        fs::write(
            fixture.temp.path().join("runtimes.json"),
            format!(
                r#"{{
  "{ios}":{{"build":"23F77","deletable":true,"identifier":"{ios}","lastUsedAt":"2026-10-08T21:00:32Z","path":"/System/Library/AssetsV2/example/ios.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.iOS-18-2","sizeBytes":8000000000,"state":"Ready","version":"18.2"}},
  "{watch}":{{"build":"22S99","deletable":true,"identifier":"{watch}","path":"/System/Library/AssetsV2/example/watch.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.watchOS-11-2","sizeBytes":4000000000,"state":"Ready","version":"11.2"}},
  "{locked}":{{"build":"23T570","deletable":false,"identifier":"{locked}","path":"/System/Library/AssetsV2/example/locked.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.watchOS-26-5","sizeBytes":3000000000,"state":"Ready","version":"26.5"}}
}}"#,
                ios = Self::IOS_RUNTIME,
                watch = Self::WATCH_RUNTIME,
                locked = Self::LOCKED_RUNTIME,
            ),
        )
        .unwrap();
        let xcrun = fixture.temp.path().join("xcrun");
        fs::write(
            &xcrun,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$CACHEFERRET_TEST_LOG"
dir="${0%/*}"
case "$1 $2 $3" in
  "simctl list devices")
    n=$(/bin/cat "$dir/list-count" 2>/dev/null || echo 0)
    echo $((n + 1)) > "$dir/list-count"
    if [ "$n" -ge 1 ] && [ -f "$dir/devices-later.json" ]; then /bin/cat "$dir/devices-later.json"; else /bin/cat "$dir/devices.json"; fi ;;
  "simctl runtime list")
    n=$(/bin/cat "$dir/runtime-count" 2>/dev/null || echo 0)
    echo $((n + 1)) > "$dir/runtime-count"
    if [ "$n" -ge 1 ] && [ -f "$dir/runtimes-later.json" ]; then /bin/cat "$dir/runtimes-later.json"; else /bin/cat "$dir/runtimes.json"; fi ;;
  "simctl delete "*) /bin/rm -rf "$CACHEFERRET_TEST_DEVICES/$3" ;;
  "simctl runtime delete")
    # CoreSimulator finishes removal later unless the test asks for it now.
    if [ -f "$dir/after-delete-$4.json" ]; then
      /bin/cat "$dir/after-delete-$4.json" > "$dir/runtimes.json"
    elif [ -f "$dir/sync-delete" ]; then
      /usr/bin/grep -v "\"$4\"" "$dir/runtimes.json" > "$dir/runtimes.new"
      /bin/cat "$dir/runtimes.new" > "$dir/runtimes.json"
    fi ;;
  *) echo "unexpected xcrun invocation: $*" >&2; exit 42 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&xcrun, fs::Permissions::from_mode(0o755)).unwrap();
        fixture
    }

    /// Write a device listing in which the large device has `state`.
    fn set_devices(&self, file: &str, state: &str) {
        let device = |udid: &str, name: &str, state: &str| {
            format!(
                r#"{{"dataPath":"{}/{udid}/data","udid":"{udid}","isAvailable":true,"state":"{state}","name":"{name}"}}"#,
                self.devices_root.display()
            )
        };
        fs::write(
            self.temp.path().join(file),
            format!(
                r#"{{"devices":{{"com.apple.CoreSimulator.SimRuntime.iOS-18-2":[{},{}]}}}}"#,
                device(Self::LARGE, "iPhone 16 Pro", state),
                device(Self::SMALL, "iPhone 16", "Shutdown"),
            ),
        )
        .unwrap();
    }

    fn log(&self) -> std::path::PathBuf {
        self.temp.path().join("argv")
    }

    fn mutations(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(" delete "))
            .map(str::to_owned)
            .collect()
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = Command::new(BIN)
            .args(args)
            .env("PATH", self.temp.path())
            .env("CACHEFERRET_TEST_LOG", self.log())
            .env("CACHEFERRET_TEST_DEVICES", &self.devices_root)
            .output()
            .expect("spawn binary");
        Output {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }
}

#[test]
#[cfg(unix)]
fn simulator_devices_are_listed_by_size_with_labels_and_known_ages() {
    let fixture = SimctlFixture::new();
    let out = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-device",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let items = scan["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "the small device is not listed: {scan}");
    let item = &items[0];
    assert_eq!(item["label"], "iPhone 16 Pro (iOS 18.2)");
    assert_eq!(item["bytes"], 70 * 1024 * 1024);
    assert_eq!(item["network_restore"], false);
    assert_eq!(item["cleanable"], true);
    assert!(item["modified_unix"].is_u64());
    assert!(
        item["path"]
            .as_str()
            .unwrap()
            .ends_with(SimctlFixture::LARGE)
    );
}

#[test]
#[cfg(unix)]
fn a_simulator_device_age_is_its_newest_file_and_ignores_directory_times() {
    let fixture = SimctlFixture::new();
    let out = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-device",
    ]);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let item = &scan["items"][0];
    assert_eq!(
        item["modified_unix"],
        SimctlFixture::APP_LAST_USED_UNIX,
        "{item}"
    );
    assert_eq!(item["protected"], false);
}

#[test]
#[cfg(unix)]
fn simulator_devices_without_installed_apps_are_not_listed() {
    let fixture = SimctlFixture::new();
    let apps = fixture
        .devices_root
        .join(SimctlFixture::LARGE)
        .join("data/Containers/Bundle/Application");
    fs::remove_dir_all(&apps).unwrap();
    for state in ["missing", "empty"] {
        if state == "empty" {
            fs::create_dir_all(&apps).unwrap();
        }
        let out = fixture.run(&[
            "scan",
            "--scope",
            "global",
            "--kind",
            "xcode-simulator-device",
        ]);
        assert_eq!(out.code, 0, "{state}: {}", out.stderr);
        let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
        assert!(
            scan["items"].as_array().unwrap().is_empty(),
            "{state}: {scan}"
        );
    }
}

#[test]
#[cfg(unix)]
fn a_shut_down_simulator_device_is_deleted_with_simctl() {
    let fixture = SimctlFixture::new();
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-device",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 1);
    assert_eq!(
        fixture.mutations(),
        [format!("simctl delete {}", SimctlFixture::LARGE)]
    );
    assert!(!fixture.devices_root.join(SimctlFixture::LARGE).exists());
    assert!(fixture.devices_root.join(SimctlFixture::SMALL).exists());
}

#[test]
#[cfg(unix)]
fn a_device_that_boots_after_scanning_is_refused_before_deletion() {
    let fixture = SimctlFixture::new();
    fixture.set_devices("devices-later.json", "Booted");
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-device",
        "--protect-days",
        "0",
        "--yes",
    ]);
    assert_eq!(out.code, 5, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("refused"), "{}", out.stderr);
    assert!(fixture.mutations().is_empty());
    assert!(fixture.devices_root.join(SimctlFixture::LARGE).exists());
}

#[test]
#[cfg(unix)]
fn simulator_runtimes_report_the_native_size_and_unknown_use() {
    let fixture = SimctlFixture::new();
    let out = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let find = |label: &str| {
        scan["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("{label} not listed: {scan}"))
    };

    let used = find("iOS 18.2 (23F77)");
    assert_eq!(used["bytes"], 8_000_000_000_u64);
    assert_eq!(used["network_restore"], true);
    assert_eq!(used["cleanable"], true);
    assert_eq!(used["modified_unix"], 1_791_493_232_u64);

    let never_used = find("watchOS 11.2 (22S99)");
    assert_eq!(never_used["bytes"], 4_000_000_000_u64);
    assert!(never_used["modified_unix"].is_null());
    assert!(never_used["age_days"].is_null());
    assert_eq!(never_used["protected"], true);

    assert_eq!(find("watchOS 26.5 (23T570)")["cleanable"], false);
}

#[test]
#[cfg(unix)]
fn only_deletable_simulator_runtimes_are_removed_and_only_with_simctl() {
    let fixture = SimctlFixture::new();
    fs::write(fixture.temp.path().join("sync-delete"), "").unwrap();
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 2);
    assert_eq!(report["policy_skipped"], 1);
    assert_eq!(report["bytes_selected"], 12_000_000_000_u64);
    assert_eq!(report["apparent_bytes_removed"], 12_000_000_000_u64);
    assert_eq!(report["deferred_paths"], serde_json::json!([]));
    let mut mutations = fixture.mutations();
    mutations.sort();
    assert_eq!(
        mutations,
        [
            format!("simctl runtime delete {}", SimctlFixture::IOS_RUNTIME),
            format!("simctl runtime delete {}", SimctlFixture::WATCH_RUNTIME),
        ]
    );
}

#[test]
#[cfg(unix)]
fn a_runtime_used_by_a_booted_device_is_not_cleanable() {
    let fixture = SimctlFixture::new();
    fixture.set_devices("devices.json", "Booted");
    let scan = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
    ]);
    let scan: serde_json::Value = serde_json::from_str(&scan.stdout).unwrap();
    let cleanable = |label: &str| {
        scan["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"].as_str().unwrap().starts_with(label))
            .unwrap()["cleanable"]
            .clone()
    };
    assert_eq!(cleanable("iOS"), false, "a booted device uses it: {scan}");
    assert_eq!(cleanable("watchOS 11.2"), true);

    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 1, "only the unused watchOS runtime goes");
    assert_eq!(report["policy_skipped"], 2);
    assert_eq!(
        fixture.mutations(),
        [format!(
            "simctl runtime delete {}",
            SimctlFixture::WATCH_RUNTIME
        )]
    );
}

#[test]
#[cfg(unix)]
fn a_runtime_whose_device_boots_after_scanning_is_refused() {
    let fixture = SimctlFixture::new();
    fixture.set_devices("devices-later.json", "Booted");
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 1, "only the unused watchOS runtime goes");
    assert_eq!(report["skipped"], 1);
    let reason = report["skipped_paths"][0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("booted") && reason.contains("iPhone 16 Pro"),
        "{reason}"
    );
    assert_eq!(
        fixture.mutations(),
        [format!(
            "simctl runtime delete {}",
            SimctlFixture::WATCH_RUNTIME
        )]
    );
}

#[test]
#[cfg(unix)]
fn a_booted_simulator_device_is_not_cleanable() {
    let fixture = SimctlFixture::new();
    fixture.set_devices("devices.json", "Booted");
    let out = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-device",
    ]);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(scan["items"][0]["cleanable"], false, "{scan}");
}

#[test]
#[cfg(unix)]
fn simulator_dry_run_never_deletes() {
    let fixture = SimctlFixture::new();
    for kind in ["xcode-simulator-device", "xcode-simulator-runtime"] {
        let out = fixture.run(&[
            "clean",
            "--scope",
            "global",
            "--kind",
            kind,
            "--include-recent",
            "--dry-run",
        ]);
        assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    }
    assert!(fixture.mutations().is_empty());
}

#[test]
#[cfg(unix)]
fn a_runtime_that_stops_being_deletable_after_scanning_is_refused() {
    let fixture = SimctlFixture::new();
    let runtimes = fs::read_to_string(fixture.temp.path().join("runtimes.json")).unwrap();
    let ios_entry = runtimes
        .lines()
        .find(|line| line.contains(SimctlFixture::IOS_RUNTIME))
        .unwrap();
    fs::write(
        fixture.temp.path().join("runtimes-later.json"),
        runtimes.replace(
            ios_entry,
            &ios_entry.replace("\"deletable\":true", "\"deletable\":false"),
        ),
    )
    .unwrap();
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["skipped"], 1);
    assert!(
        report["skipped_paths"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not deletable")
    );
    assert!(
        !fixture
            .mutations()
            .iter()
            .any(|line| line.contains(SimctlFixture::IOS_RUNTIME))
    );
}

#[test]
#[cfg(unix)]
fn a_runtime_that_is_still_listed_after_deletion_is_reported_as_deferred() {
    let fixture = SimctlFixture::new();
    // Without `sync-delete` the fake keeps listing the runtimes after the
    // delete command succeeds, as CoreSimulator does while it detaches them.
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["cleaned"], 2);
    assert_eq!(report["deferred_paths"].as_array().unwrap().len(), 2);
    assert_eq!(
        report["apparent_bytes_removed"], 0,
        "deferred removals are not counted as freed: {report}"
    );
    assert_eq!(report["allocated_bytes_removed_estimate"], 0);
    assert_eq!(report["bytes_selected"], 12_000_000_000_u64);

    let text = fixture.run(&[
        "clean",
        "-o",
        "text",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert!(text.stdout.contains("not finished"), "{}", text.stdout);
}

#[test]
#[cfg(unix)]
fn a_deleted_runtime_still_listed_in_an_unparseable_entry_is_deferred() {
    let fixture = SimctlFixture::new();
    let runtimes = fs::read_to_string(fixture.temp.path().join("runtimes.json")).unwrap();
    // After the delete the listing still holds the runtime, but without a size,
    // so the typed parser would drop it and make it look gone.
    fs::write(
        fixture
            .temp
            .path()
            .join(format!("after-delete-{}.json", SimctlFixture::IOS_RUNTIME)),
        runtimes.replace("\"sizeBytes\":8000000000,", ""),
    )
    .unwrap();
    let out = fixture.run(&[
        "clean",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
        "--include-recent",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let deferred = report["deferred_paths"].as_array().unwrap();
    assert!(
        deferred
            .iter()
            .any(|path| path.as_str().unwrap().ends_with("ios.dmg")),
        "{report}"
    );
    assert!(
        report["apparent_bytes_removed"].as_u64().unwrap() < 8_000_000_000,
        "the unconfirmed runtime is not counted as freed: {report}"
    );
}

#[test]
#[cfg(unix)]
fn a_booted_device_without_a_data_path_still_blocks_its_runtime() {
    let fixture = SimctlFixture::new();
    fs::write(
        fixture.temp.path().join("devices.json"),
        r#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-18-2":[{"state":"Booted","name":"Hidden","udid":"7A1C3F10-0000-4000-8000-000000000009"}]}}"#,
    )
    .unwrap();
    let out = fixture.run(&[
        "scan",
        "--scope",
        "global",
        "--kind",
        "xcode-simulator-runtime",
    ]);
    let scan: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let ios = scan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"].as_str().unwrap().starts_with("iOS"))
        .unwrap();
    assert_eq!(ios["cleanable"], false, "{scan}");
}
