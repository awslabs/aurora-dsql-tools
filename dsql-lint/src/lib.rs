pub(crate) mod lint;
pub(crate) mod rules;

pub use lint::{fix_sql, lint_sql, Diagnostic, FixOutput, FixResult, LintRule};
