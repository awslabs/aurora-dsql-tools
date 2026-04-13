use sqlparser::ast::Statement;
use crate::lint::Diagnostic;

pub fn check(_stmt: &Statement, _diagnostics: &mut Vec<Diagnostic>) {
    // Rules will be added in subsequent tasks
}
