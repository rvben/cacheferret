//! CacheFerret command-line interface.

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use cacheferret::{
    CacheCandidate, CleanReport, DiscoveryOptions, Error, OutputFormat, ScopeFilter,
    clean_candidates, default_roots, discover, format_bytes, schema,
};
use clap::error::ErrorKind as ClapErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

#[derive(Parser)]
#[command(
    name = "cacheferret",
    version,
    about = "Find and safely clean developer caches across macOS and Linux",
    long_about = "Find and safely clean developer caches across macOS and Linux.\n\nRunning without a command scans only. Run `cacheferret schema` for the machine-readable clispec.dev v0.3 contract."
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
    /// Find and size developer caches without changing anything.
    Scan(ScanArgs),
    /// Safely remove eligible caches after confirmation.
    Clean(CleanArgs),
    /// List every cache kind CacheFerret knows how to identify.
    Catalog,
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
struct ScanArgs {
    /// Project directory to scan. Repeat for multiple roots.
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<PathBuf>,

    /// Include project caches, shared global caches, or both.
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

    /// Clean project caches, shared global caches, or both.
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

#[derive(Debug, Clone, Copy, ValueEnum)]
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
        Some(Command::Catalog) => run_catalog(format),
        Some(Command::Scan(args)) => run_scan(args, format),
        Some(Command::Clean(args)) => run_clean(args, format),
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
                "returned": page.len(),
                "offset": args.offset,
                "limit": args.limit,
                "truncated": end < total,
                "warnings": report.warnings,
            })
            .to_string()
        }
        OutputFormat::Text => {
            render_scan_text(page, total, total_bytes, end < total, &report.warnings)
        }
    })
}

fn run_catalog(format: OutputFormat) -> Result<String, Error> {
    let entries = cacheferret::catalog();
    Ok(match format {
        OutputFormat::Json => json!({"total": entries.len(), "items": entries}).to_string(),
        OutputFormat::Text => {
            let mut lines = vec!["KIND\tECOSYSTEM\tSCOPE\tRESTORE\tCLEAN".to_owned()];
            lines.extend(entries.iter().map(|entry| {
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
    let downloads = candidates
        .iter()
        .filter(|candidate| candidate.network_restore)
        .count();
    eprintln!("SIZE\tRESTORE\tKIND\tPATH");
    for candidate in candidates {
        eprintln!(
            "{}\t{}\t{}\t{}",
            format_bytes(candidate.bytes),
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
        "Clean {} cache directories ({})? {} require downloads to restore. [y/N] ",
        candidates.len(),
        format_bytes(bytes),
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
    truncated: bool,
    warnings: &[String],
) -> String {
    let mut lines = vec![format!(
        "Found {total} cache directories using {}{}",
        format_bytes(total_bytes),
        if truncated { " (output truncated)" } else { "" }
    )];
    lines.push("SIZE\tAGE\tSCOPE\tKIND\tPATH".to_owned());
    lines.extend(page.iter().map(|candidate| {
        format!(
            "{}\t{}\t{:?}\t{}\t{}{}",
            format_bytes(candidate.bytes),
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

fn render_clean(report: &CleanReport, format: OutputFormat, fields: &[String]) -> String {
    match format {
        OutputFormat::Json => {
            let value = serde_json::to_value(report).expect("clean report serializes");
            project_object(value, fields).to_string()
        }
        OutputFormat::Text => {
            let summary = format!(
                "{} {} of {} selected caches; {} protected; {} scan-only; estimated {} reclaimed{}",
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
                    report.bytes_reclaimed_estimate
                }),
                if report.skipped > 0 {
                    format!("; {} refused during final validation", report.skipped)
                } else {
                    String::new()
                }
            );
            if !report.dry_run || report.selected_targets.is_empty() {
                return summary;
            }
            let mut lines = vec![summary, "SIZE\tRESTORE\tKIND\tPATH".to_owned()];
            lines.extend(report.selected_targets.iter().map(|target| {
                format!(
                    "{}\t{}\t{}\t{}",
                    format_bytes(target.bytes),
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

const CANDIDATE_FIELDS: [&str; 10] = [
    "kind",
    "ecosystem",
    "scope",
    "path",
    "bytes",
    "modified_unix",
    "age_days",
    "protected",
    "network_restore",
    "cleanable",
];

const CLEAN_FIELDS: [&str; 14] = [
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
    "bytes_reclaimed_estimate",
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
            if let Some(hint) = error.hint() {
                body.insert("hint".to_owned(), json!(hint));
            }
            eprintln!("{}", json!({"error": body}));
        }
    }
}
