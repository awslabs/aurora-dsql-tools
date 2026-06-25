pub(crate) mod lint;
pub(crate) mod mysql;
pub(crate) mod rules;

#[cfg(feature = "grammar-diff")]
pub mod grammar;

pub use lint::{fix_sql, lint_sql, Diagnostic, FixOutput, FixResult, LintRule};
pub use mysql::fix_sql_mysql;
