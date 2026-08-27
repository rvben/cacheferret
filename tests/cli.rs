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
