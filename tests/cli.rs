use std::fs;
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
