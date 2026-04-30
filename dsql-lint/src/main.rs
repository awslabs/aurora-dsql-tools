use clap::{Parser, ValueEnum};
use dsql_lint::{Diagnostic, FixResult};
use serde::Serialize;
use std::convert::Infallible;
use std::fmt;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;

/// Version of the JSON output schema. Increment only on breaking changes
/// (renamed/removed fields, changed semantics). Additive changes keep the
/// same version.
const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Parser)]
#[command(name = "dsql-lint", version)]
#[command(about = "Lint SQL files for Aurora DSQL compatibility")]
#[command(after_help = "\
EXIT CODES:
  0  Clean (no issues) or all issues fixed without warnings
  1  Errors found (lint mode) or unfixable errors remain (fix mode)
  2  Usage error (invalid arguments)
  3  Fix mode only: all issues fixed, but some produced warnings

CI USAGE:
  Exit code 3 means the fix succeeded but produced warnings (e.g., removed
  foreign keys). In shell scripts with set -e or CI pipelines, handle it:

    dsql-lint --fix input.sql; rc=$?
    if [ $rc -eq 1 ]; then echo 'unfixable errors'; exit 1; fi
    # rc 0 or 3: fix succeeded (3 = review warnings)")]
struct Args {
    /// SQL files to lint (use '-' to read from stdin)
    files: Vec<InputSource>,

    /// Fix mode: output DSQL-compatible SQL to a new file.
    /// Note: SQL comments in modified statements will not be preserved.
    #[arg(long)]
    fix: bool,

    /// Output file path (only with --fix and a single input file)
    #[arg(short, long)]
    output: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Copy, Default, PartialEq, ValueEnum)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

impl OutputFormat {
    fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

/// Parses `-` as stdin at the clap boundary; downstream code is agnostic
/// of the `-` convention.
#[derive(Clone, Debug, PartialEq, Eq)]
enum InputSource {
    Stdin,
    File(PathBuf),
}

impl FromStr for InputSource {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == STDIN_ARG {
            Ok(Self::Stdin)
        } else {
            Ok(Self::File(PathBuf::from(s)))
        }
    }
}

impl InputSource {
    fn is_stdin(&self) -> bool {
        matches!(self, Self::Stdin)
    }

    fn read_to_string(&self) -> io::Result<String> {
        // read_to_string wraps non-UTF-8 bytes as InvalidData with an opaque
        // "stream did not contain valid UTF-8" message. Replace it so users
        // see a clear reason in stderr / CI logs.
        match self {
            Self::Stdin => {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).map_err(|e| {
                    if e.kind() == ErrorKind::InvalidData {
                        io::Error::new(ErrorKind::InvalidData, "stdin contained non-UTF-8 bytes")
                    } else {
                        e
                    }
                })?;
                Ok(buf)
            }
            Self::File(p) => std::fs::read_to_string(p).map_err(|e| {
                if e.kind() == ErrorKind::InvalidData {
                    io::Error::new(ErrorKind::InvalidData, "file contained non-UTF-8 bytes")
                } else {
                    e
                }
            }),
        }
    }
}

impl fmt::Display for InputSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdin => f.write_str(STDIN_DISPLAY),
            Self::File(p) => write!(f, "{}", p.display()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct JsonOutput {
    schema_version: u32,
    files: Vec<JsonFileOutput>,
    summary: JsonSummary,
}

/// Public wire shape. Built from an internal sum type (`FixOutcome` /
/// `LintOutcome`) so the "exactly one of `error` / `output_file` / `fixed_sql`"
/// invariant holds during processing.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct JsonFileOutput {
    file: String,
    diagnostics: Vec<JsonDiagnostic>,
    error: Option<String>,
    output_file: Option<String>,
    fixed_sql: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct JsonDiagnostic {
    #[serde(flatten)]
    diagnostic: Diagnostic,
    statement_preview: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "snake_case")]
struct JsonSummary {
    errors: usize,
    warnings: usize,
    fixed: usize,
}

const STDIN_ARG: &str = "-";
const STDIN_DISPLAY: &str = "<stdin>";
const PREVIEW_MAX_CHARS: usize = 80;

/// Process exit codes. Values are the contract documented in `--help`.
#[repr(i32)]
#[derive(Clone, Copy, PartialEq)]
enum ExitCode {
    Ok = 0,
    Errors = 1,
    Usage = 2,
    Warnings = 3,
}

/// Result of rendering output. `BrokenPipe` short-circuits to `ExitCode::Ok`.
/// `WriteError` forces `ExitCode::Errors` so a failed write never looks clean.
enum RenderOutcome {
    Completed,
    BrokenPipe,
    WriteError,
}

/// Aggregated state reduced from a `Vec<LintOutcome>` — lets `run_lint` compute
/// the summary and exit code in one pass instead of juggling named mutables.
#[derive(Default)]
struct LintTally {
    summary: JsonSummary,
    total_diagnostics: usize,
    had_read_error: bool,
}

impl LintTally {
    fn exit_code(&self) -> ExitCode {
        if self.total_diagnostics > 0 || self.had_read_error {
            ExitCode::Errors
        } else {
            ExitCode::Ok
        }
    }
}

/// Aggregated state reduced from a `Vec<FixOutcome>`.
#[derive(Default)]
struct FixTally {
    summary: JsonSummary,
    had_unfixable: bool,
    had_io_error: bool,
}

impl FixTally {
    fn exit_code(&self) -> ExitCode {
        if self.had_unfixable || self.had_io_error {
            ExitCode::Errors
        } else if self.summary.warnings > 0 {
            ExitCode::Warnings
        } else {
            ExitCode::Ok
        }
    }
}

/// Collapse whitespace and truncate with an ellipsis so human- and
/// machine-readable previews can't be confused with an exactly-N-char statement.
fn make_preview(statement: &str) -> String {
    let collapsed: String = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > PREVIEW_MAX_CHARS {
        let truncated: String = collapsed.chars().take(PREVIEW_MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

fn default_output_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy())
        .unwrap_or_default();
    let new_name = if ext.is_empty() {
        format!("{stem}-fixed")
    } else {
        format!("{stem}-fixed.{ext}")
    };
    input.with_file_name(new_name)
}

fn to_json_diag(d: Diagnostic) -> JsonDiagnostic {
    JsonDiagnostic {
        statement_preview: make_preview(&d.statement),
        diagnostic: d,
    }
}

fn to_json_diags(ds: Vec<Diagnostic>) -> Vec<JsonDiagnostic> {
    ds.into_iter().map(to_json_diag).collect()
}

fn main() {
    let code = run();
    // process::exit does not flush stdio buffers; under a piped stdout (which
    // is block-buffered) that truncates the final chunk of JSON/SQL output.
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    process::exit(code as i32);
}

fn run() -> ExitCode {
    let args = Args::parse();

    if args.files.is_empty() {
        eprintln!("Usage: dsql-lint <file.sql> [file2.sql ...] or dsql-lint - (read from stdin)");
        return ExitCode::Usage;
    }

    let stdin_count = args.files.iter().filter(|s| s.is_stdin()).count();
    if stdin_count > 1 {
        eprintln!("Error: '-' (stdin) may appear at most once in the argument list");
        return ExitCode::Usage;
    }

    if args.output.is_some() && !args.fix {
        eprintln!("Error: -o/--output requires --fix");
        return ExitCode::Usage;
    }

    if args.output.is_some() && args.files.len() > 1 {
        eprintln!("Error: -o/--output can only be used with a single input file");
        return ExitCode::Usage;
    }

    // Reading from an interactive TTY would hang forever on read_to_string with
    // no indication to the user. Detect and reject.
    if stdin_count > 0 && io::stdin().is_terminal() {
        eprintln!(
            "Error: '-' reads from stdin, but stdin is a terminal. \
             Pipe SQL in (e.g. `cat file.sql | dsql-lint -`) or pass a filename."
        );
        return ExitCode::Usage;
    }

    if args.fix {
        run_fix(&args)
    } else {
        run_lint(&args)
    }
}

// -------------------------------------------------------------------------
// Lint mode
// -------------------------------------------------------------------------

/// Two disjoint states so rendering can't produce a half-state
/// (e.g. `error` and `diagnostics` both populated).
enum LintOutcome {
    ReadError {
        display: String,
        message: String,
    },
    Linted {
        display: String,
        diagnostics: Vec<Diagnostic>,
    },
}

fn lint_one(src: &InputSource) -> LintOutcome {
    match src.read_to_string() {
        Err(e) => LintOutcome::ReadError {
            message: format!("Error reading '{src}': {e}"),
            display: src.to_string(),
        },
        Ok(sql) => LintOutcome::Linted {
            diagnostics: dsql_lint::lint_sql(&sql),
            display: src.to_string(),
        },
    }
}

fn run_lint(args: &Args) -> ExitCode {
    let outcomes: Vec<LintOutcome> = args.files.iter().map(lint_one).collect();

    let tally = outcomes.iter().fold(LintTally::default(), |mut t, o| {
        match o {
            LintOutcome::ReadError { .. } => {
                t.had_read_error = true;
                // See FixTally for the summary.errors/exit-code invariant.
                t.summary.errors += 1;
            }
            LintOutcome::Linted { diagnostics, .. } => {
                t.total_diagnostics += diagnostics.len();
                for d in diagnostics {
                    match &d.fix_result {
                        FixResult::Unfixable => t.summary.errors += 1,
                        FixResult::FixedWithWarning(_) => t.summary.warnings += 1,
                        FixResult::Fixed(_) => {}
                    }
                }
            }
        }
        t
    });

    let exit_code = tally.exit_code();
    let render = if args.format.is_json() {
        let files: Vec<JsonFileOutput> = outcomes.into_iter().map(lint_outcome_json).collect();
        emit_json(files, tally.summary)
    } else {
        for o in outcomes {
            render_lint_outcome_text(o);
        }
        if tally.total_diagnostics > 0 {
            eprintln!("\n{} issue(s) found.", tally.total_diagnostics);
        } else if !tally.had_read_error {
            eprintln!("All statements compatible with DSQL.");
        }
        RenderOutcome::Completed
    };

    match render {
        RenderOutcome::BrokenPipe => ExitCode::Ok,
        RenderOutcome::WriteError => ExitCode::Errors,
        RenderOutcome::Completed => exit_code,
    }
}

fn lint_outcome_json(o: LintOutcome) -> JsonFileOutput {
    match o {
        LintOutcome::ReadError { display, message } => JsonFileOutput {
            file: display,
            diagnostics: Vec::new(),
            error: Some(message),
            output_file: None,
            fixed_sql: None,
        },
        LintOutcome::Linted {
            display,
            diagnostics,
        } => JsonFileOutput {
            file: display,
            diagnostics: to_json_diags(diagnostics),
            error: None,
            output_file: None,
            fixed_sql: None,
        },
    }
}

fn render_lint_outcome_text(o: LintOutcome) {
    match o {
        LintOutcome::ReadError { message, .. } => {
            eprintln!("{message}");
        }
        LintOutcome::Linted {
            display,
            diagnostics,
        } => {
            for d in &diagnostics {
                // Label each diagnostic by its fix_result so human readers get
                // the same severity split as JSON consumers.
                let severity = match &d.fix_result {
                    FixResult::Unfixable => "ERROR",
                    FixResult::FixedWithWarning(_) => "WARNING",
                    FixResult::Fixed(_) => "INFO",
                };
                eprintln!("{display}:{}: {severity} — {}", d.line, d.message);
                eprintln!("  → {}", d.suggestion);
                eprintln!("  | {}", make_preview(&d.statement));
            }
        }
    }
}

// -------------------------------------------------------------------------
// Fix mode
// -------------------------------------------------------------------------

/// Where the fixed SQL for a given input ends up written.
enum FixDest {
    /// Wrote the fix to a file on disk.
    File(PathBuf),
    /// Streamed to stdout (text mode + stdin input with no --output).
    Stdout,
    /// Embedded in the JSON response's `fixed_sql` field (json mode + stdin
    /// input with no --output).
    InlineJson,
}

/// Disjoint outcome so the JSON renderer can't construct a
/// `{ error: Some(..), fixed_sql: Some(..) }` half-state.
enum FixOutcome {
    ReadError {
        display: String,
        message: String,
    },
    IoError {
        display: String,
        message: String,
    },
    Processed {
        display: String,
        /// Cheap flag so the renderer can decide whether to emit the
        /// "SQL comments in modified statements were not preserved" note
        /// without keeping the full source SQL around.
        had_comments: bool,
        diagnostics: Vec<Diagnostic>,
        fixed_sql: String,
        dest: FixDest,
    },
}

fn compute_fix_dest(args: &Args, src: &InputSource) -> FixDest {
    match (args.output.as_deref(), src, args.format) {
        (Some(o), _, _) => FixDest::File(PathBuf::from(o)),
        (None, InputSource::File(p), _) => FixDest::File(default_output_path(p)),
        (None, InputSource::Stdin, OutputFormat::Json) => FixDest::InlineJson,
        (None, InputSource::Stdin, OutputFormat::Text) => FixDest::Stdout,
    }
}

/// Check whether the output path resolves to the same file as the input. The
/// input was just read successfully, so `canonicalize(input)` must succeed;
/// if it doesn't we err on the side of blocking the write (a false positive
/// is safer than overwriting the user's source file).
fn same_output_as_input(input: &Path, output: &Path) -> bool {
    let Ok(input_canonical) = std::fs::canonicalize(input) else {
        // Input path we just read no longer resolves. Something is wrong;
        // refuse to proceed rather than fall back to byte-exact comparison
        // (which misses aliases like `./foo.sql` vs `foo.sql`).
        return true;
    };
    let file_name = match output.file_name() {
        Some(f) => f,
        None => return false,
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Output's parent must exist (we're about to write a child into it); its
    // canonicalization is the basis for a reliable comparison.
    match std::fs::canonicalize(parent) {
        Ok(parent_canonical) => parent_canonical.join(file_name) == input_canonical,
        // Parent missing: the write is about to fail anyway. Don't block here.
        Err(_) => false,
    }
}

/// Atomically write `fixed_sql` to `output_path` via a sibling temp file.
/// `sync_all` forces the data to disk before `persist` so a power loss
/// between the rename and the next directory fsync can't leave a
/// zero-length output in place of the original.
fn atomic_write(output_path: &Path, fixed_sql: &str) -> io::Result<()> {
    let write_dir = output_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(write_dir)?;
    tmp.write_all(fixed_sql.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(output_path)?;
    Ok(())
}

fn fix_one(args: &Args, src: &InputSource) -> FixOutcome {
    let sql = match src.read_to_string() {
        Ok(s) => s,
        Err(e) => {
            return FixOutcome::ReadError {
                message: format!("Error reading '{src}': {e}"),
                display: src.to_string(),
            };
        }
    };

    let had_comments = sql.contains("--") || sql.contains("/*");
    let result = dsql_lint::fix_sql(&sql);
    let dest = compute_fix_dest(args, src);

    if let FixDest::File(ref output_path) = dest {
        if let InputSource::File(input_path) = src {
            if same_output_as_input(input_path, output_path) {
                return FixOutcome::IoError {
                    message: format!(
                        "Error: output path '{}' is the same as input '{src}'. Use a different output path.",
                        output_path.display(),
                    ),
                    display: src.to_string(),
                };
            }
        }
        if let Err(e) = atomic_write(output_path, &result.sql) {
            return FixOutcome::IoError {
                message: format!("Error writing '{}': {e}", output_path.display()),
                display: src.to_string(),
            };
        }
    }

    FixOutcome::Processed {
        display: src.to_string(),
        had_comments,
        diagnostics: result.diagnostics,
        fixed_sql: result.sql,
        dest,
    }
}

fn run_fix(args: &Args) -> ExitCode {
    let outcomes: Vec<FixOutcome> = args.files.iter().map(|src| fix_one(args, src)).collect();

    let tally = outcomes.iter().fold(FixTally::default(), |mut t, o| {
        match o {
            FixOutcome::ReadError { .. } | FixOutcome::IoError { .. } => {
                t.had_io_error = true;
                // Keep `summary.errors` in sync with the non-zero exit code
                // so JSON consumers gating on `summary.errors == 0` don't
                // mistake a failed run for clean.
                t.summary.errors += 1;
            }
            FixOutcome::Processed { diagnostics, .. } => {
                for d in diagnostics {
                    match &d.fix_result {
                        FixResult::Fixed(_) => t.summary.fixed += 1,
                        FixResult::FixedWithWarning(_) => t.summary.warnings += 1,
                        FixResult::Unfixable => t.summary.errors += 1,
                    }
                }
                if diagnostics
                    .iter()
                    .any(|d| matches!(d.fix_result, FixResult::Unfixable))
                {
                    t.had_unfixable = true;
                }
            }
        }
        t
    });

    let exit_code = tally.exit_code();
    let render = if args.format.is_json() {
        let files: Vec<JsonFileOutput> = outcomes.into_iter().map(fix_outcome_json).collect();
        emit_json(files, tally.summary)
    } else {
        render_fix_outcomes_text(outcomes)
    };

    match render {
        RenderOutcome::BrokenPipe => ExitCode::Ok,
        RenderOutcome::WriteError => ExitCode::Errors,
        RenderOutcome::Completed => exit_code,
    }
}

fn fix_outcome_json(o: FixOutcome) -> JsonFileOutput {
    match o {
        FixOutcome::ReadError { display, message } | FixOutcome::IoError { display, message } => {
            JsonFileOutput {
                file: display,
                diagnostics: Vec::new(),
                error: Some(message),
                output_file: None,
                fixed_sql: None,
            }
        }
        FixOutcome::Processed {
            display,
            diagnostics,
            fixed_sql,
            dest,
            ..
        } => {
            let (output_file, fixed_sql) = match dest {
                FixDest::File(p) => (Some(p.display().to_string()), None),
                FixDest::InlineJson => (None, Some(fixed_sql)),
                FixDest::Stdout => {
                    unreachable!("FixDest::Stdout cannot occur in JSON mode; see compute_fix_dest")
                }
            };
            JsonFileOutput {
                file: display,
                diagnostics: to_json_diags(diagnostics),
                error: None,
                output_file,
                fixed_sql,
            }
        }
    }
}

fn render_fix_outcomes_text(outcomes: Vec<FixOutcome>) -> RenderOutcome {
    let mut comment_note_printed = false;
    let mut unfixable_file_count: usize = 0;

    for o in outcomes {
        match o {
            FixOutcome::ReadError { message, .. } | FixOutcome::IoError { message, .. } => {
                eprintln!("{message}");
            }
            FixOutcome::Processed {
                display,
                had_comments,
                diagnostics,
                fixed_sql,
                dest,
            } => {
                if matches!(dest, FixDest::Stdout) {
                    match write_stdout(fixed_sql.as_bytes()) {
                        Ok(()) => {}
                        Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                            return RenderOutcome::BrokenPipe;
                        }
                        Err(e) => {
                            // Non-BrokenPipe: the fix output never reached the
                            // consumer. Surface and escalate to WriteError so
                            // callers never report success on truncated data.
                            eprintln!("Error writing fixed SQL: {e}");
                            return RenderOutcome::WriteError;
                        }
                    }
                }

                for d in &diagnostics {
                    match &d.fix_result {
                        FixResult::Fixed(msg) => {
                            eprintln!("{display}:{}: FIXED — {}", d.line, msg);
                        }
                        FixResult::FixedWithWarning(warning) => {
                            eprintln!("{display}:{}: WARNING — {}", d.line, warning);
                        }
                        FixResult::Unfixable => {
                            eprintln!("{display}:{}: ERROR (unfixable) — {}", d.line, d.message);
                            eprintln!("  → {}", d.suggestion);
                            eprintln!("  | {}", make_preview(&d.statement));
                        }
                    }
                }

                let had_any_fix = diagnostics.iter().any(|d| {
                    matches!(
                        d.fix_result,
                        FixResult::Fixed(_) | FixResult::FixedWithWarning(_)
                    )
                });
                if had_any_fix && !comment_note_printed && had_comments {
                    eprintln!("Note: SQL comments in modified statements were not preserved.");
                    comment_note_printed = true;
                }

                if diagnostics
                    .iter()
                    .any(|d| matches!(d.fix_result, FixResult::Unfixable))
                {
                    unfixable_file_count += 1;
                }

                if let FixDest::File(ref output_path) = dest {
                    eprintln!(
                        "{}",
                        fix_file_status_message(output_path, &fixed_sql, &diagnostics)
                    );
                }
            }
        }
    }

    if unfixable_file_count > 0 {
        eprintln!(
            "\nFix complete: {unfixable_file_count} file(s) had unfixable errors and require manual review."
        );
    }

    RenderOutcome::Completed
}

fn fix_file_status_message(
    output_path: &Path,
    fixed_sql: &str,
    diagnostics: &[Diagnostic],
) -> String {
    let path = output_path.display();
    if fixed_sql.is_empty() {
        return format!("Fixed output is empty — all statements were removed: {path}");
    }
    let unfixable = diagnostics
        .iter()
        .filter(|d| matches!(d.fix_result, FixResult::Unfixable))
        .count();
    if unfixable > 0 {
        return format!("Partial fix written to: {path} ({unfixable} unfixable error(s) remain)");
    }
    let warnings = diagnostics
        .iter()
        .filter(|d| matches!(d.fix_result, FixResult::FixedWithWarning(_)))
        .count();
    if warnings > 0 {
        return format!(
            "Fixed output written to: {path} ({warnings} warning(s) — review recommended)"
        );
    }
    format!("Fixed output written to: {path}")
}

// -------------------------------------------------------------------------
// Output helpers
// -------------------------------------------------------------------------

/// Returns `Err` on `BrokenPipe`. `print!`/`println!` panic on `BrokenPipe`,
/// which breaks pipe composition (`dsql-lint ... | head`).
fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(bytes)
}

fn emit_json(files: Vec<JsonFileOutput>, summary: JsonSummary) -> RenderOutcome {
    let output = JsonOutput {
        schema_version: JSON_SCHEMA_VERSION,
        files,
        summary,
    };
    let rendered =
        serde_json::to_string_pretty(&output).expect("json serialization should not fail");
    let mut bytes = rendered.into_bytes();
    bytes.push(b'\n');
    match write_stdout(&bytes) {
        Ok(()) => RenderOutcome::Completed,
        Err(e) if e.kind() == ErrorKind::BrokenPipe => RenderOutcome::BrokenPipe,
        Err(e) => {
            // JSON was never emitted; consumer sees an empty document. Escalate
            // to WriteError so the exit code matches reality.
            eprintln!("Error writing JSON output: {e}");
            RenderOutcome::WriteError
        }
    }
}
