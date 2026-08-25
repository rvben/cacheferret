//! The clispec.dev v0.3 contract emitted by `cacheferret schema`.

use serde_json::{Value, json};

pub const CLISPEC_VERSION: &str = "0.3";

pub fn contract() -> Value {
    json!({
        "clispec": CLISPEC_VERSION,
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "output": {"tty": "text", "piped": "json"},
        "global_args": [
            {
                "name": "--output",
                "short": "-o",
                "type": "string",
                "enum": ["auto", "json", "text"],
                "default": "auto",
                "description": "Output format. auto = text on a TTY, JSON when piped."
            }
        ],
        "commands": [
            {
                "name": "tui",
                "description": "Browse caches and press d to delete the focused entry.",
                "effects": "idempotent",
                "mutating": true,
                "args": tui_args(),
                "output_kind": "opaque",
                "media_type": "application/x-terminal",
                "errors": ["invalid_input", "io"],
                "stability": "stable",
                "example": {"args": ["tui", "--root", ".", "--scope", "project"]}
            },
            {
                "name": "scan",
                "description": "Find and size developer caches without changing anything. Also runs when no command is given outside an interactive terminal.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "unbounded",
                "pagination": {
                    "style": "offset",
                    "offset_arg": "--offset",
                    "limit_arg": "--limit"
                },
                "fields_arg": "--fields",
                "args": discovery_args("all", true),
                "output_fields": candidate_fields(),
                "errors": ["invalid_input", "io"],
                "stability": "stable",
                "example": {"args": ["scan", "--scope", "project", "--root", ".", "--limit", "10"]}
            },
            {
                "name": "clean",
                "description": "Remove eligible caches after final validation and explicit confirmation.",
                "effects": "idempotent",
                "mutating": true,
                "cardinality": "single",
                "confirmation_bypass_arg": "--yes",
                "fields_arg": "--fields",
                "args": clean_args(),
                "output_fields": [
                    {"name": "changed", "type": "boolean"},
                    {"name": "dry_run", "type": "boolean"},
                    {"name": "confirmed", "type": "boolean"},
                    {"name": "selected", "type": "integer"},
                    {"name": "cleaned", "type": "integer"},
                    {"name": "skipped", "type": "integer"},
                    {"name": "protected_skipped", "type": "integer"},
                    {"name": "policy_skipped", "type": "integer"},
                    {"name": "network_restore_selected", "type": "integer"},
                    {"name": "bytes_selected", "type": "integer"},
                    {"name": "bytes_reclaimed_estimate", "type": "integer"},
                    {
                        "name": "selected_targets",
                        "type": "array",
                        "items": {
                            "type": "object",
                            "fields": [
                                {"name": "kind", "type": "string"},
                                {"name": "path", "type": "string"},
                                {"name": "bytes", "type": "integer"},
                                {"name": "network_restore", "type": "boolean"}
                            ]
                        }
                    },
                    {"name": "cleaned_paths", "type": "array", "items": {"type": "string"}},
                    {
                        "name": "skipped_paths",
                        "type": "array",
                        "items": {
                            "type": "object",
                            "fields": [
                                {"name": "path", "type": "string"},
                                {"name": "reason", "type": "string"}
                            ]
                        }
                    }
                ],
                "errors": ["invalid_input", "confirmation_required", "conflict", "io"],
                "stability": "stable",
                "example": {"args": ["clean", "--root", ".", "--dry-run"]}
            },
            {
                "name": "catalog",
                "description": "List the closed safety catalog of supported cache kinds.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "unbounded",
                "pagination": {
                    "style": "offset",
                    "offset_arg": "--offset",
                    "limit_arg": "--limit"
                },
                "fields_arg": "--fields",
                "args": catalog_args(),
                "output_fields": [
                    {"name": "kind", "type": "string"},
                    {"name": "ecosystem", "type": "string"},
                    {"name": "scope", "type": "string", "enum": ["project", "global"]},
                    {"name": "description", "type": "string"},
                    {"name": "network_restore", "type": "boolean"},
                    {"name": "cleanable", "type": "boolean"}
                ],
                "stability": "stable",
                "example": {"args": ["catalog"]}
            },
            {
                "name": "schema",
                "description": "Print this clispec.dev v0.3 contract as JSON.",
                "effects": "read_only",
                "mutating": false,
                "cardinality": "single",
                "args": [
                    {"name": "path", "type": "string[]", "required": false, "description": "Optional command path used to narrow the contract."}
                ],
                "stdout_schema": {"$ref": "https://clispec.dev/schema/v0.3.json"},
                "stability": "stable"
            },
            {
                "name": "completions",
                "description": "Generate a shell completion script.",
                "effects": "read_only",
                "mutating": false,
                "output_kind": "opaque",
                "media_type": "text/x-shellscript",
                "args": [
                    {"name": "shell", "type": "string", "required": true, "enum": ["bash", "zsh", "fish", "powershell", "elvish"], "description": "Target shell."}
                ],
                "stability": "stable"
            }
        ],
        "errors": [
            {"kind": "invalid_input", "exit_code": 2, "retryable": false, "description": "A path, kind, field, or value was invalid."},
            {"kind": "usage", "exit_code": 3, "retryable": false, "description": "The command-line invocation was invalid."},
            {"kind": "io", "exit_code": 4, "retryable": false, "description": "A local filesystem or process operation failed."},
            {"kind": "conflict", "exit_code": 5, "retryable": false, "description": "Every selected target changed or became unsafe before deletion."},
            {"kind": "confirmation_required", "exit_code": 6, "retryable": false, "description": "Cleanup reached a confirmation gate without a TTY."}
        ],
        "extensions": {
            "homepage": env!("CARGO_PKG_HOMEPAGE"),
            "safety": {
                "follows_symlinks": false,
                "default_protect_days": 7,
                "default_clean_scope": "project"
            }
        }
    })
}

pub fn contract_for(path: &[String]) -> Value {
    let mut value = contract();
    if path.is_empty() {
        return value;
    }
    let query = path.join(" ");
    let prefix = format!("{query} ");
    if let Some(commands) = value.get_mut("commands").and_then(Value::as_array_mut) {
        commands.retain(|command| {
            command
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == query || name.starts_with(&prefix))
        });
    }
    value
}

pub fn contract_json(path: &[String]) -> String {
    serde_json::to_string_pretty(&contract_for(path)).expect("contract serializes")
}

fn discovery_args(default_scope: &str, paging: bool) -> Vec<Value> {
    let mut args = vec![
        json!({"name": "--root", "type": "path[]", "required": false, "description": "Project directory to scan. Repeat for multiple roots; defaults to common source directories."}),
        json!({"name": "--scope", "type": "string", "enum": ["all", "project", "global"], "default": default_scope, "description": "Project caches, global caches, and recognized temporary-storage locations to include."}),
        json!({"name": "--kind", "type": "string[]", "required": false, "description": "Restrict to kinds returned by `cacheferret catalog`."}),
        json!({"name": "--protect-days", "type": "integer", "default": 7, "description": "Consider caches modified within this many days protected."}),
    ];
    if paging {
        args.extend([
            json!({"name": "--limit", "type": "integer", "default": 100, "description": "Maximum records in this page (1-1000)."}),
            json!({"name": "--offset", "type": "integer", "default": 0, "description": "Zero-based record offset."}),
            json!({"name": "--fields", "type": "string[]", "required": false, "description": "Candidate fields to include."}),
        ]);
    }
    args
}

fn tui_args() -> Vec<Value> {
    let mut args = discovery_args("all", false);
    args.retain(|arg| arg["name"] != "--protect-days");
    args
}

fn clean_args() -> Vec<Value> {
    let mut args = discovery_args("project", false);
    args.extend([
        json!({"name": "--include-recent", "type": "boolean", "default": false, "description": "Include recently modified caches protected by default."}),
        json!({"name": "--dry-run", "type": "boolean", "default": false, "description": "Report what would be removed without changing the filesystem."}),
        json!({"name": "--yes", "short": "-y", "type": "boolean", "default": false, "description": "Confirm cleanup non-interactively."}),
        json!({"name": "--fields", "type": "string[]", "required": false, "description": "Report fields to include in structured output."}),
    ]);
    args
}

fn catalog_args() -> Vec<Value> {
    vec![
        json!({"name": "--limit", "type": "integer", "default": 100, "description": "Maximum records in this page (1-1000)."}),
        json!({"name": "--offset", "type": "integer", "default": 0, "description": "Zero-based record offset."}),
        json!({"name": "--fields", "type": "string[]", "required": false, "description": "Catalog fields to include."}),
    ]
}

fn candidate_fields() -> Vec<Value> {
    vec![
        json!({"name": "kind", "type": "string"}),
        json!({"name": "ecosystem", "type": "string"}),
        json!({"name": "scope", "type": "string", "enum": ["project", "global"]}),
        json!({"name": "path", "type": "string"}),
        json!({"name": "bytes", "type": "integer"}),
        json!({"name": "modified_unix", "type": "integer", "nullable": true}),
        json!({"name": "age_days", "type": "integer", "nullable": true}),
        json!({"name": "protected", "type": "boolean"}),
        json!({"name": "network_restore", "type": "boolean"}),
        json!({"name": "cleanable", "type": "boolean"}),
    ]
}
