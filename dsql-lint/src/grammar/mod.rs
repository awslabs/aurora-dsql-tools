//! Grammar oracle: load the grammar JSON, tokenize SQL, decide whether the
//! grammar accepts it.

pub mod model;
pub mod recognizer;
pub mod tokenize;

use std::path::Path;

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use self::model::GrammarFile;
use self::recognizer::{terminal_input_string, GrammarRecognizer};
use self::tokenize::{map_token, Terminal};

/// Statement-shaped rules `accepts` tries one at a time. The grammar JSON's
/// declared root is a dispatch rule that isn't itself defined; we enumerate
/// the leaves instead.
pub const TOP_LEVEL_RULES: &[&str] = &[
    "CreateSchemaStmt",
    "CreateStmt",
    "IndexStmt",
    "ViewStmt",
    "CreateSeqStmt",
    "CreateDomainStmt",
    "CreateRoleStmt",
    "AlterTableStmt",
    "AlterSeqStmt",
    "AlterDomainStmt",
    "AlterObjectSchemaStmt",
    "AlterOwnerStmt",
    "AlterRoleStmt",
    "AlterRoleSetStmt",
    "RenameStmt",
    "CommentStmt",
    "DropStmt",
    "DropRoleStmt",
    "GrantStmt",
    "RevokeStmt",
    "GrantRoleStmt",
    "RevokeRoleStmt",
    "VariableShowStmt",
    "VariableSetStmt",
    "VariableResetStmt",
    "TransactionStmt",
    "ExplainStmt",
    "DeallocateStmt",
    "SelectStmt",
    "InsertStmt",
    "UpdateStmt",
    "DeleteStmt",
];

pub struct Grammar {
    file: GrammarFile,
    recognizers: Vec<(String, GrammarRecognizer)>,
    /// Every Terminal text the grammar lists — i.e. the grammar's keyword
    /// vocabulary. Used to demote sqlparser-classified keywords that the
    /// grammar doesn't list (e.g. `ID`) back to `IDENT` before recognition;
    /// otherwise they'd be rejected wherever an identifier is expected.
    grammar_keywords: std::collections::HashSet<String>,
}

impl Grammar {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read grammar {}: {e}", path.display()))?;
        let file: GrammarFile = serde_json::from_str(&raw)
            .map_err(|e| format!("parse grammar {}: {e}", path.display()))?;

        let mut recognizers = Vec::with_capacity(TOP_LEVEL_RULES.len());
        for &root in TOP_LEVEL_RULES {
            if !file.rules.contains_key(root) {
                continue;
            }
            let r = GrammarRecognizer::build(&file, root)?;
            recognizers.push((root.to_string(), r));
        }
        if recognizers.is_empty() {
            return Err(format!(
                "no top-level rules from TOP_LEVEL_RULES found in {}",
                path.display()
            ));
        }

        let mut grammar_keywords: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for prod in file.rules.values() {
            for choice in &prod.choices {
                for t in choice {
                    if matches!(t.token_type, model::TokenType::Terminal) {
                        grammar_keywords.insert(t.text.clone());
                    }
                }
            }
        }
        Ok(Grammar {
            file,
            recognizers,
            grammar_keywords,
        })
    }

    pub fn accepts(&self, sql: &str) -> Result<bool, String> {
        let dialect = PostgreSqlDialect {};
        let raw_tokens = Tokenizer::new(&dialect, sql)
            .tokenize()
            .map_err(|e| format!("tokenize: {e}"))?;

        let terminals: Vec<String> = raw_tokens
            .iter()
            .filter_map(|t| {
                let term = map_token(t);
                if matches!(term, Terminal::Skip) {
                    return None;
                }
                // Statement-rule productions don't include a trailing `;`.
                if matches!(t, Token::SemiColon) {
                    return None;
                }
                // See `grammar_keywords` above.
                let term = match term {
                    Terminal::Keyword(ref kw) if !self.grammar_keywords.contains(kw) => {
                        Terminal::CharClass("IDENT")
                    }
                    other => other,
                };
                Some(terminal_input_string(&term))
            })
            .collect();

        if terminals.is_empty() {
            return Ok(false);
        }

        for (_root_name, recognizer) in &self.recognizers {
            if recognizer.accepts(&terminals) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn referenced_charclasses(&self) -> std::collections::BTreeSet<String> {
        let mut classes = std::collections::BTreeSet::new();
        for prod in self.file.rules.values() {
            for choice in &prod.choices {
                for t in choice {
                    if matches!(t.token_type, model::TokenType::CharClass) {
                        classes.insert(t.text.clone());
                    }
                }
            }
        }
        classes
    }
}

#[derive(Debug, Clone)]
pub struct SplitStatement {
    pub raw: String,
    pub line: usize,
}

/// Delegates to the lint engine's splitter so both see identical statement
/// boundaries — drift would make the diff compare different inputs.
pub fn split_statements(sql: &str) -> Result<Vec<SplitStatement>, String> {
    crate::lint::split_statements(sql).map(|pairs| {
        pairs
            .into_iter()
            .map(|(line, raw)| SplitStatement { raw, line })
            .collect()
    })
}
