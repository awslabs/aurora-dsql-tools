pub(crate) mod lint;
pub(crate) mod rules;

#[cfg(feature = "grammar-diff")]
pub mod grammar;

pub use lint::{fix_sql, lint_sql, Diagnostic, FixOutput, FixResult, LintRule};

#[doc(hidden)]
pub use rules::errors::UNSUPPORTED_STMT_ARM_COUNT;
