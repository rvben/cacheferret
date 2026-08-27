//! CacheFerret command-line interface.

mod tui;

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use cacheferret::{
    CacheCandidate, CleanReport, DiscoveryOptions, Error, NativeCleanReport, NativeDiagnostic,
    NativeReport, NativeResource, OutputFormat, ScopeFilter, clean_candidates, default_roots,
    discover, format_bytes, format_signed_bytes, inspect_docker, preview_docker_build_cache,
    prune_docker_build_cache, schema,
};
use clap::error::ErrorKind as ClapErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

#[derive(Parser)]
#[command(
    name = "cacheferret",
    version,
    about = "Find and safely clean developer caches across macOS and Linux",
    long_about = "Find and safely clean developer caches across macOS and Linux.\n\nRunning without a command opens the TUI on a terminal and performs a read-only JSON scan when piped. Run `cacheferret schema` for the machine-readable clispec.dev v0.3 contract."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Output format; auto = text on a TTY, JSON when piped.
    #[arg(long, short = 'o', value_enum, default_value = "auto", global = true)]
    output: CliOutput,
}

#[derive(Subcommand)]
enum Command {
    /// Browse caches and press `d` to delete the focused entry.
    Tui(TuiArgs),
    /// Find and size developer caches without changing anything.
    Scan(ScanArgs),
    /// Inspect Docker-managed storage without changing daemon state.
    Docker(DockerArgs),
    /// Safely remove eligible caches after confirmation.
    Clean(CleanArgs),
    /// List every cache kind CacheFerret knows how to identify.
    Catalog(CatalogArgs),
    /// Print the machine-readable clispec.dev v0.3 contract.
    Schema {
        /// Optional command path used to narrow the contract.
        path: Vec<String>,
    },
    /// Generate a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Clone, Args)]
struct TuiArgs {
    /// Project directory to scan. Repeat for multiple roots.
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<PathBuf>,

    /// Include project caches, global caches and recognized temporary storage, or both.
    #[arg(long, value_enum, default_value = "all")]
    scope: CliScope,

    /// Restrict discovery to a cache kind from `cacheferret catalog`.
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,
}

impl Default for TuiArgs {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            scope: CliScope::All,
            kinds: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Args)]
struct CatalogArgs {
    /// Maximum records returned in this page.
    #[arg(long, default_value_t = 100, value_parser = parse_limit)]
    limit: usize,

    /// Zero-based record offset.
    #[arg(long, default_value_t = 0)]
    offset: usize,

    /// Catalog fields to include. Comma-separated or repeatable.
    #[arg(long, value_delimiter = ',', value_name = "FIELD")]
    fields: Vec<String>,
}

#[derive(Debug, Clone, Args)]
struct ScanArgs {
    /// Project directory to scan. Repeat for multiple roots.
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<PathBuf>,

    /// Include project caches, global caches and recognized temporary storage, or both.
    #[arg(long, value_enum, default_value = "all")]
    scope: CliScope,

    /// Restrict discovery to a cache kind from `cacheferret catalog`.
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,

    /// Consider caches modified within this many days protected.
    #[arg(long, default_value_t = 7)]
    protect_days: u64,

    /// Maximum records returned in this page.
    #[arg(long, default_value_t = 100, value_parser = parse_limit)]
    limit: usize,

    /// Zero-based record offset.
    #[arg(long, default_value_t = 0)]
    offset: usize,

    /// Candidate fields to include. Comma-separated or repeatable.
    #[arg(long, value_delimiter = ',', value_name = "FIELD")]
    fields: Vec<String>,
}

#[derive(Debug, Clone, Args)]
struct DockerArgs {
    #[command(subcommand)]
    command: Option<DockerCommand>,

    #[command(flatten)]
    inspection: DockerInspectionArgs,
}

#[derive(Debug, Clone, Subcommand)]
enum DockerCommand {
    /// Prune ordinary Docker build cache after a fresh preview and confirmation.
    Clean(DockerCleanArgs),
}

#[derive(Debug, Clone, Args)]
struct DockerInspectionArgs {
    /// Maximum records returned in this page.
    #[arg(long, default_value_t = 100, value_parser = parse_limit)]
    limit: usize,

    /// Zero-based record offset.
    #[arg(long, default_value_t = 0)]
    offset: usize,

    /// Native resource fields to include. Comma-separated or repeatable.
    #[arg(long, value_delimiter = ',', value_name = "FIELD")]
    fields: Vec<String>,
}

#[derive(Debug, Clone, Args)]
struct DockerCleanArgs {
    /// Preview the build-cache prune without changing Docker state.
    #[arg(long)]
    dry_run: bool,

    /// Confirm the build-cache prune non-interactively.
    #[arg(long, short = 'y')]
    yes: bool,

    /// Report fields to include in structured output.
    #[arg(long, value_delimiter = ',', value_name = "FIELD")]
    fields: Vec<String>,
}

impl Default for ScanArgs {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            scope: CliScope::All,
            kinds: Vec::new(),
            protect_days: 7,
            limit: 100,
            offset: 0,
            fields: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Args)]
struct CleanArgs {
    /// Project directory to scan. Repeat for multiple roots.
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<PathBuf>,

    /// Clean project caches, global caches and recognized temporary storage, or both.
    #[arg(long, value_enum, default_value = "project")]
    scope: CliScope,

    /// Restrict cleanup to a cache kind from `cacheferret catalog`.
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,

    /// Protect caches modified within this many days.
    #[arg(long, default_value_t = 7)]
    protect_days: u64,

    /// Include recently modified caches that are protected by default.
    #[arg(long)]
    include_recent: bool,

    /// Report what would be removed without changing the filesystem.
    #[arg(long)]
    dry_run: bool,

    /// Confirm cleanup non-interactively.
    #[arg(long, short = 'y')]
    yes: bool,

    /// Report fields to include in structured output.
    #[arg(long, value_delimiter = ',', value_name = "FIELD")]
    fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliScope {
    All,
    Project,
    Global,
}

impl From<CliScope> for ScopeFilter {
    fn from(value: CliScope) -> Self {
        match value {
            CliScope::All => ScopeFilter::All,
            CliScope::Project => ScopeFilter::Project,
            CliScope::Global => ScopeFilter::Global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliOutput {
    Auto,
    Json,
    Text,
}

impl CliOutput {
    fn resolve(self) -> OutputFormat {
        match self {
            CliOutput::Json => OutputFormat::Json,
            CliOutput::Text => OutputFormat::Text,
            CliOutput::Auto if std::io::stdout().is_terminal() => OutputFormat::Text,
            CliOutput::Auto => OutputFormat::Json,
        }
    }
}

fn main() -> ExitCode {
    let fallback_format = format_from_argv();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return handle_clap_error(error, fallback_format),
    };
    let format = cli.output.resolve();

    let result = match cli.command {
        Some(Command::Tui(args)) => return run_tui(args, cli.output),
        Some(Command::Schema { path }) => {
            println!("{}", schema::contract_json(&path));
            return ExitCode::SUCCESS;
        }
        Some(Command::Completions { shell }) => {
            let mut command = Cli::command();
            let name = command.get_name().to_owned();
            clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
            return ExitCode::SUCCESS;
        }
        Some(Command::Catalog(args)) => run_catalog(args, format),
        Some(Command::Scan(args)) => run_scan(args, format),
        Some(Command::Docker(args)) => {
            let DockerArgs {
                command,
                inspection,
            } = args;
            match command {
                Some(DockerCommand::Clean(args)) => run_docker_clean(args, format),
                None => run_docker(inspection, format),
            }
        }
        Some(Command::Clean(args)) => run_clean(args, format),
        None if cli.output == CliOutput::Auto
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal() =>
        {
            return run_tui(TuiArgs::default(), cli.output);
        }
        None => run_scan(ScanArgs::default(), format),
    };

    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            emit_error(&error, format);
            ExitCode::from(error.exit_code())
        }
    }
}

fn run_docker(args: DockerInspectionArgs, format: OutputFormat) -> Result<String, Error> {
    validate_field_set(&args.fields, &NATIVE_RESOURCE_FIELDS)?;
    let report = inspect_docker();
    let total = report.resources.len();
    let end = args.offset.saturating_add(args.limit).min(total);
    let page = if args.offset >= total {
        &[]
    } else {
        &report.resources[args.offset..end]
    };

    Ok(match format {
        OutputFormat::Json => {
            let items = page
                .iter()
                .map(|resource| project_native_fields(resource, &args.fields))
                .collect::<Vec<_>>();
            json!({
                "provider": report.provider,
                "available": report.available,
                "items": items,
                "total": total,
                "total_bytes": report.total_bytes(),
                "total_reclaimable_bytes": report.total_reclaimable_bytes(),
                "returned": page.len(),
                "offset": args.offset,
                "limit": args.limit,
                "truncated": end < total,
                "diagnostics": report.diagnostics,
            })
            .to_string()
        }
        OutputFormat::Text => render_docker_text(&report, page, end < total),
    })
}

fn run_docker_clean(args: DockerCleanArgs, format: OutputFormat) -> Result<String, Error> {
    validate_field_set(&args.fields, &NATIVE_CLEAN_FIELDS)?;
    let before = preview_docker_build_cache().map_err(native_error)?;
    let report = if args.dry_run {
        NativeCleanReport::preview(before, true, false)
    } else if before.reclaimable_bytes == 0 {
        NativeCleanReport::preview(before, false, true)
    } else {
        let confirmed = if args.yes {
            true
        } else if !std::io::stdin().is_terminal() {
            return Err(Error::NativeConfirmationRequired {
                provider: "Docker".to_owned(),
                kind: "build cache".to_owned(),
            });
        } else {
            prompt_for_docker_confirmation(&before)?
        };
        if confirmed {
            prune_docker_build_cache().map_err(native_error)?
        } else {
            NativeCleanReport::preview(before, false, false)
        }
    };
    Ok(render_docker_clean(&report, format, &args.fields))
}

fn native_error(diagnostic: NativeDiagnostic) -> Error {
    if matches!(
        diagnostic.kind.as_str(),
        "invalid_output" | "duplicate_output" | "missing_class" | "empty_output"
    ) {
        Error::NativeProtocol {
            provider: diagnostic.provider,
            message: diagnostic.message,
        }
    } else {
        Error::NativeUnavailable {
            provider: diagnostic.provider,
            message: diagnostic.message,
        }
    }
}

fn prompt_for_docker_confirmation(resource: &NativeResource) -> Result<bool, Error> {
    eprint!(
        "Prune Docker build cache ({} reclaimable of {} used)? Rebuilding may require downloads. [y/N] ",
        format_bytes(resource.reclaimable_bytes),
        format_bytes(resource.bytes)
    );
    std::io::stderr().flush().map_err(|source| Error::Io {
        path: PathBuf::from("<stderr>"),
        source,
    })?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|source| Error::Io {
            path: PathBuf::from("<stdin>"),
            source,
        })?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn run_tui(args: TuiArgs, output: CliOutput) -> ExitCode {
    let format = output.resolve();
    if output == CliOutput::Json {
        let error = Error::Usage {
            message: "the TUI does not produce JSON; use `cacheferret scan --output json`"
                .to_owned(),
        };
        emit_error(&error, format);
        return ExitCode::from(error.exit_code());
    }
    let roots = if args.roots.is_empty() {
        default_roots()
    } else {
        args.roots
    };
    match tui::run(tui::Options {
        roots,
        scope: args.scope.into(),
        kinds: args.kinds,
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            emit_error(&error, format);
            ExitCode::from(error.exit_code())
        }
    }
}

fn run_scan(args: ScanArgs, format: OutputFormat) -> Result<String, Error> {
    validate_fields(&args.fields)?;
    let roots = if args.roots.is_empty() {
        default_roots()
    } else {
        args.roots
    };
    let report = discover(&DiscoveryOptions {
        roots,
        scope: args.scope.into(),
        kinds: args.kinds,
        protect_days: args.protect_days,
    })?;

    let total = report.candidates.len();
    let total_bytes = report.total_bytes();
    let total_allocated_bytes = report.total_allocated_bytes();
    let end = args.offset.saturating_add(args.limit).min(total);
    let page = if args.offset >= total {
        &[]
    } else {
        &report.candidates[args.offset..end]
    };

    Ok(match format {
        OutputFormat::Json => {
            let items: Vec<Value> = page
                .iter()
                .map(|candidate| project_fields(candidate, &args.fields))
                .collect();
            json!({
                "items": items,
                "total": total,
                "total_bytes": total_bytes,
                "total_allocated_bytes": total_allocated_bytes,
                "returned": page.len(),
                "offset": args.offset,
                "limit": args.limit,
                "truncated": end < total,
                "warnings": report.warnings,
            })
            .to_string()
        }
        OutputFormat::Text => render_scan_text(
            page,
            total,
            total_bytes,
            total_allocated_bytes,
            end < total,
            &report.warnings,
        ),
    })
}

fn run_catalog(args: CatalogArgs, format: OutputFormat) -> Result<String, Error> {
    validate_field_set(&args.fields, &CATALOG_FIELDS)?;
    let entries = cacheferret::catalog();
    let total = entries.len();
    let end = args.offset.saturating_add(args.limit).min(total);
    let page = if args.offset >= total {
        &[]
    } else {
        &entries[args.offset..end]
    };
    Ok(match format {
        OutputFormat::Json => {
            let items: Vec<Value> = page
                .iter()
                .map(|entry| {
                    project_object(
                        serde_json::to_value(entry).expect("catalog entry serializes"),
                        &args.fields,
                    )
                })
                .collect();
            json!({
                "items": items,
                "total": total,
                "returned": page.len(),
                "offset": args.offset,
                "limit": args.limit,
                "truncated": end < total,
            })
            .to_string()
        }
        OutputFormat::Text => {
            let mut lines = vec!["KIND\tECOSYSTEM\tSCOPE\tRESTORE\tCLEAN".to_owned()];
            lines.extend(page.iter().map(|entry| {
                format!(
                    "{}\t{}\t{:?}\t{}\t{}",
                    entry.kind,
                    entry.ecosystem,
                    entry.scope,
                    if entry.network_restore {
                        "network"
                    } else {
                        "local"
                    },
                    if entry.cleanable { "yes" } else { "scan-only" }
                )
            }));
            lines.join("\n")
        }
    })
}

fn run_clean(args: CleanArgs, format: OutputFormat) -> Result<String, Error> {
    validate_clean_fields(&args.fields)?;
    let roots = if args.roots.is_empty() {
        default_roots()
    } else {
        args.roots
    };
    let scan = discover(&DiscoveryOptions {
        roots,
        scope: args.scope.into(),
        kinds: args.kinds,
        protect_days: args.protect_days,
    })?;
    for warning in &scan.warnings {
        eprintln!("warning: {warning}");
    }

    let policy_skipped = scan
        .candidates
        .iter()
        .filter(|candidate| !candidate.cleanable)
        .count();
    let protected_skipped = scan
        .candidates
        .iter()
        .filter(|candidate| candidate.cleanable && candidate.protected && !args.include_recent)
        .count();
    let eligible: Vec<CacheCandidate> = scan
        .candidates
        .into_iter()
        .filter(|candidate| candidate.cleanable && (args.include_recent || !candidate.protected))
        .collect();

    let mut report = if args.dry_run {
        clean_candidates(&eligible, true)
    } else if eligible.is_empty() {
        let mut empty = clean_candidates(&eligible, true);
        empty.dry_run = false;
        empty
    } else if args.yes {
        clean_candidates(&eligible, false)
    } else if !std::io::stdin().is_terminal() {
        return Err(Error::ConfirmationRequired {
            count: eligible.len(),
        });
    } else if prompt_for_confirmation(&eligible)? {
        clean_candidates(&eligible, false)
    } else {
        let mut declined = clean_candidates(&eligible, true);
        declined.dry_run = false;
        declined
    };
    report.protected_skipped = protected_skipped;
    report.policy_skipped += policy_skipped;

    if !report.dry_run
        && report.confirmed
        && report.selected > 0
        && report.skipped == report.selected
    {
        return Err(Error::Conflict {
            message: format!(
                "all {} selected targets were refused during final validation",
                report.selected
            ),
        });
    }

    Ok(render_clean(&report, format, &args.fields))
}

fn prompt_for_confirmation(candidates: &[CacheCandidate]) -> Result<bool, Error> {
    let bytes: u64 = candidates.iter().map(|candidate| candidate.bytes).sum();
    let allocated_bytes: u64 = candidates
        .iter()
        .map(|candidate| candidate.allocated_bytes)
        .sum();
    let downloads = candidates
        .iter()
        .filter(|candidate| candidate.network_restore)
        .count();
    eprintln!("APPARENT\tALLOCATED\tRESTORE\tKIND\tPATH");
    for candidate in candidates {
        eprintln!(
            "{}\t{}\t{}\t{}\t{}",
            format_bytes(candidate.bytes),
            format_bytes(candidate.allocated_bytes),
            if candidate.network_restore {
                "network"
            } else {
                "local"
            },
            candidate.kind,
            candidate.path.display()
        );
    }
    eprint!(
        "Clean {} cache directories ({} apparent, {} allocated)? {} require downloads to restore. [y/N] ",
        candidates.len(),
        format_bytes(bytes),
        format_bytes(allocated_bytes),
        downloads
    );
    std::io::stderr().flush().map_err(|source| Error::Io {
        path: PathBuf::from("<stderr>"),
        source,
    })?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|source| Error::Io {
            path: PathBuf::from("<stdin>"),
            source,
        })?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn render_scan_text(
    page: &[CacheCandidate],
    total: usize,
    total_bytes: u64,
    total_allocated_bytes: u64,
    truncated: bool,
    warnings: &[String],
) -> String {
    let mut lines = vec![format!(
        "Found {total} storage locations using {} apparent ({} allocated){}",
        format_bytes(total_bytes),
        format_bytes(total_allocated_bytes),
        if truncated { " (output truncated)" } else { "" }
    )];
    lines.push("APPARENT\tALLOCATED\tAGE\tSCOPE\tKIND\tPATH".to_owned());
    lines.extend(page.iter().map(|candidate| {
        format!(
            "{}\t{}\t{}\t{:?}\t{}\t{}{}",
            format_bytes(candidate.bytes),
            format_bytes(candidate.allocated_bytes),
            candidate
                .age_days
                .map_or_else(|| "?".to_owned(), |days| format!("{days}d")),
            candidate.scope,
            candidate.kind,
            candidate.path.display(),
            if candidate.protected {
                " [protected]"
            } else {
                ""
            }
        )
    }));
    if !warnings.is_empty() {
        lines.push(format!("{} paths could not be inspected", warnings.len()));
    }
    lines.join("\n")
}

fn render_docker_text(report: &NativeReport, page: &[NativeResource], truncated: bool) -> String {
    if !report.available {
        let detail = report
            .diagnostics
            .first()
            .map_or("Docker is unavailable", |diagnostic| {
                diagnostic.message.as_str()
            });
        return format!("Docker storage unavailable: {detail}");
    }

    let mut lines = vec![format!(
        "Docker uses {}; {} is potentially reclaimable{}",
        format_bytes(report.total_bytes()),
        format_bytes(report.total_reclaimable_bytes()),
        if truncated { " (output truncated)" } else { "" }
    )];
    lines.push("RECLAIMABLE\tSIZE\tACTIVE\tTOTAL\tKIND".to_owned());
    lines.extend(page.iter().map(|resource| {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            format_bytes(resource.reclaimable_bytes),
            format_bytes(resource.bytes),
            resource.active_count,
            resource.total_count,
            resource.kind
        )
    }));
    lines.extend(
        report
            .diagnostics
            .iter()
            .map(|diagnostic| format!("Diagnostic: {}", diagnostic.message)),
    );
    lines.join("\n")
}

fn render_docker_clean(
    report: &NativeCleanReport,
    format: OutputFormat,
    fields: &[String],
) -> String {
    match format {
        OutputFormat::Json => {
            let value = serde_json::to_value(report).expect("native clean report serializes");
            project_object(value, fields).to_string()
        }
        OutputFormat::Text => {
            let mut lines = if report.dry_run {
                vec![format!(
                    "Would prune Docker build cache · {} reclaimable of {} used · rebuilding may require downloads",
                    format_bytes(report.before.reclaimable_bytes),
                    format_bytes(report.before.bytes)
                )]
            } else if report.before.reclaimable_bytes == 0 {
                vec!["Docker build cache is already clean; nothing changed".to_owned()]
            } else if !report.confirmed {
                vec!["Docker build-cache prune cancelled; nothing changed".to_owned()]
            } else if let Some(bytes) = report.reported_reclaimed_bytes {
                vec![format!(
                    "Pruned Docker build cache · {} reported reclaimed",
                    format_bytes(bytes)
                )]
            } else if report.estimated_removed_bytes > 0 {
                vec![format!(
                    "Pruned Docker build cache · {} estimated removed",
                    format_bytes(report.estimated_removed_bytes)
                )]
            } else {
                vec![
                    "Docker build-cache prune completed; no reclaimed bytes were reported"
                        .to_owned(),
                ]
            };
            lines.extend(
                report
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("Diagnostic: {}", diagnostic.message)),
            );
            lines.join("\n")
        }
    }
}

fn render_clean(report: &CleanReport, format: OutputFormat, fields: &[String]) -> String {
    match format {
        OutputFormat::Json => {
            let value = serde_json::to_value(report).expect("clean report serializes");
            project_object(value, fields).to_string()
        }
        OutputFormat::Text => {
            let mut summary = format!(
                "{} {} of {} selected caches; {} protected; {} scan-only; {} apparent, {} allocated{}",
                if report.dry_run {
                    "Would clean"
                } else {
                    "Cleaned"
                },
                if report.dry_run {
                    report.selected
                } else {
                    report.cleaned
                },
                report.selected,
                report.protected_skipped,
                report.policy_skipped,
                format_bytes(if report.dry_run {
                    report.bytes_selected
                } else {
                    report.apparent_bytes_removed
                }),
                format_bytes(if report.dry_run {
                    report.allocated_bytes_selected
                } else {
                    report.allocated_bytes_removed_estimate
                }),
                if report.skipped > 0 {
                    format!("; {} refused during final validation", report.skipped)
                } else {
                    String::new()
                }
            );
            if !report.dry_run {
                match report.filesystem_deltas.as_slice() {
                    [delta] => summary.push_str(&format!(
                        "; observed disk-free change {} net",
                        format_signed_bytes(delta.delta_bytes)
                    )),
                    deltas if !deltas.is_empty() => summary.push_str(&format!(
                        "; observed disk-free changes on {} filesystems (reported separately in JSON)",
                        deltas.len()
                    )),
                    _ => summary.push_str("; observed disk-free change unavailable"),
                }
            }
            if !report.dry_run && report.filesystem_deltas.len() > 1 {
                let mut lines = vec![
                    summary,
                    "NET_CHANGE\tFREE_BEFORE\tFREE_AFTER\tFILESYSTEM_PROBE".to_owned(),
                ];
                lines.extend(report.filesystem_deltas.iter().map(|delta| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        format_signed_bytes(delta.delta_bytes),
                        format_bytes(delta.free_bytes_before),
                        format_bytes(delta.free_bytes_after),
                        delta.probe_path.display()
                    )
                }));
                return lines.join("\n");
            }
            if !report.dry_run || report.selected_targets.is_empty() {
                return summary;
            }
            let mut lines = vec![
                summary,
                "APPARENT\tALLOCATED\tRESTORE\tKIND\tPATH".to_owned(),
            ];
            lines.extend(report.selected_targets.iter().map(|target| {
                format!(
                    "{}\t{}\t{}\t{}\t{}",
                    format_bytes(target.bytes),
                    format_bytes(target.allocated_bytes),
                    if target.network_restore {
                        "network"
                    } else {
                        "local"
                    },
                    target.kind,
                    target.path.display()
                )
            }));
            lines.join("\n")
        }
    }
}

const CANDIDATE_FIELDS: [&str; 11] = [
    "kind",
    "ecosystem",
    "scope",
    "path",
    "bytes",
    "allocated_bytes",
    "modified_unix",
    "age_days",
    "protected",
    "network_restore",
    "cleanable",
];

const CATALOG_FIELDS: [&str; 6] = [
    "kind",
    "ecosystem",
    "scope",
    "description",
    "network_restore",
    "cleanable",
];

const NATIVE_RESOURCE_FIELDS: [&str; 9] = [
    "provider",
    "kind",
    "label",
    "total_count",
    "active_count",
    "bytes",
    "reclaimable_bytes",
    "cleanable",
    "scope",
];

const NATIVE_CLEAN_FIELDS: [&str; 10] = [
    "provider",
    "kind",
    "changed",
    "dry_run",
    "confirmed",
    "before",
    "after",
    "reported_reclaimed_bytes",
    "estimated_removed_bytes",
    "diagnostics",
];

const CLEAN_FIELDS: [&str; 19] = [
    "changed",
    "dry_run",
    "confirmed",
    "selected",
    "cleaned",
    "skipped",
    "protected_skipped",
    "policy_skipped",
    "network_restore_selected",
    "bytes_selected",
    "apparent_bytes_selected",
    "allocated_bytes_selected",
    "bytes_reclaimed_estimate",
    "apparent_bytes_removed",
    "allocated_bytes_removed_estimate",
    "filesystem_deltas",
    "selected_targets",
    "cleaned_paths",
    "skipped_paths",
];

fn validate_fields(fields: &[String]) -> Result<(), Error> {
    let allowed: HashSet<&str> = CANDIDATE_FIELDS.into_iter().collect();
    if let Some(field) = fields
        .iter()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(Error::InvalidInput {
            message: format!(
                "unknown field {field:?}; expected one of {}",
                CANDIDATE_FIELDS.join(", ")
            ),
        });
    }
    Ok(())
}

fn validate_clean_fields(fields: &[String]) -> Result<(), Error> {
    validate_field_set(fields, &CLEAN_FIELDS)
}

fn validate_field_set(fields: &[String], allowed_fields: &[&str]) -> Result<(), Error> {
    let allowed: HashSet<&str> = allowed_fields.iter().copied().collect();
    if let Some(field) = fields
        .iter()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(Error::InvalidInput {
            message: format!(
                "unknown field {field:?}; expected one of {}",
                allowed_fields.join(", ")
            ),
        });
    }
    Ok(())
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let parsed: usize = value
        .parse()
        .map_err(|_| "limit must be an integer from 1 through 1000".to_owned())?;
    if (1..=1000).contains(&parsed) {
        Ok(parsed)
    } else {
        Err("limit must be from 1 through 1000".to_owned())
    }
}

fn project_fields(candidate: &CacheCandidate, fields: &[String]) -> Value {
    let value = serde_json::to_value(candidate).expect("candidate serializes");
    project_object(value, fields)
}

fn project_native_fields(resource: &NativeResource, fields: &[String]) -> Value {
    let value = serde_json::to_value(resource).expect("native resource serializes");
    project_object(value, fields)
}

fn project_object(value: Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return value;
    }
    let source = value.as_object().expect("candidate is an object");
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = source.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    Value::Object(selected)
}

fn format_from_argv() -> OutputFormat {
    let args: Vec<String> = std::env::args().collect();
    for (index, arg) in args.iter().enumerate() {
        if (arg == "--output" || arg == "-o")
            && let Some(value) = args.get(index + 1)
        {
            return if value == "text" {
                OutputFormat::Text
            } else {
                OutputFormat::Json
            };
        }
        if let Some(value) = arg.strip_prefix("--output=") {
            return if value == "text" {
                OutputFormat::Text
            } else {
                OutputFormat::Json
            };
        }
    }
    if std::io::stderr().is_terminal() {
        OutputFormat::Text
    } else {
        OutputFormat::Json
    }
}

fn handle_clap_error(error: clap::Error, format: OutputFormat) -> ExitCode {
    match error.kind() {
        ClapErrorKind::DisplayHelp
        | ClapErrorKind::DisplayVersion
        | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = error.print();
            ExitCode::SUCCESS
        }
        _ => {
            let error = Error::Usage {
                message: error.to_string().trim().to_owned(),
            };
            emit_error(&error, format);
            ExitCode::from(error.exit_code())
        }
    }
}

fn emit_error(error: &Error, format: OutputFormat) {
    match format {
        OutputFormat::Text => {
            eprintln!("Error: {error}");
            if let Some(hint) = error.hint() {
                eprintln!("Hint: {hint}");
            }
        }
        OutputFormat::Json => {
            let mut body = Map::new();
            body.insert("kind".to_owned(), json!(error.kind()));
            body.insert("message".to_owned(), json!(error.to_string()));
            body.insert("exit_code".to_owned(), json!(error.exit_code()));
            body.insert("retryable".to_owned(), json!(error.retryable()));
            if let Some(hint) = error.hint() {
                body.insert("hint".to_owned(), json!(hint));
            }
            eprintln!("{}", json!({"error": body}));
        }
    }
}
