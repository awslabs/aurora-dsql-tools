//! Grammar oracle: load the grammar JSON, tokenize SQL, decide whether the
//! grammar accepts it.

pub(crate) mod model;
pub(crate) mod recognizer;
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
        Self::load_with_warnings(path, |line| eprintln!("{line}"))
    }

    /// `warn` receives one line per warning; `Grammar::load` routes them to
    /// stderr. Tests inject a sink to assert the warnings fire.
    pub fn load_with_warnings(path: &Path, mut warn: impl FnMut(&str)) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read grammar {}: {e}", path.display()))?;
        let file: GrammarFile = serde_json::from_str(&raw)
            .map_err(|e| format!("parse grammar {}: {e}", path.display()))?;

        // Asymmetry warnings — silent drift here means a `lint-too-lenient`
        // landslide with no obvious cause. Surface it once at load time.
        let in_grammar: std::collections::HashSet<&str> =
            file.rules.keys().map(String::as_str).collect();
        let in_top_level: std::collections::HashSet<&str> =
            TOP_LEVEL_RULES.iter().copied().collect();

        let missing_from_grammar: Vec<&&str> = TOP_LEVEL_RULES
            .iter()
            .filter(|r| !in_grammar.contains(*r))
            .collect();
        if !missing_from_grammar.is_empty() {
            warn(&format!(
                "warning: TOP_LEVEL_RULES entries not defined in grammar: {missing_from_grammar:?}"
            ));
        }

        let mut stmt_rules_missing_from_top_level: Vec<&str> = file
            .rules
            .keys()
            .map(String::as_str)
            .filter(|r| r.ends_with("Stmt") && !in_top_level.contains(r))
            .collect();
        stmt_rules_missing_from_top_level.sort();
        if !stmt_rules_missing_from_top_level.is_empty() {
            warn(&format!(
                "warning: grammar `*Stmt` rules not in TOP_LEVEL_RULES: {stmt_rules_missing_from_top_level:?}"
            ));
        }

        // Non-terminals referenced in any rule but not defined as one. Each
        // becomes a non-derivable sink in the recognizer; derivations
        // through them silently produce `lint-too-lenient`. Surface once.
        let mut referenced: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for prod in file.rules.values() {
            for choice in &prod.choices {
                for t in choice {
                    if matches!(t.token_type, model::TokenType::NonTerminal) {
                        referenced.insert(t.text.as_str());
                    }
                }
            }
        }
        let undefined_nonterms: Vec<&&str> = referenced
            .iter()
            .filter(|n| !in_grammar.contains(*n))
            .collect();
        if !undefined_nonterms.is_empty() {
            warn(&format!(
                "warning: non-terminals referenced but not defined: {undefined_nonterms:?}"
            ));
        }

        let mut recognizers = Vec::with_capacity(TOP_LEVEL_RULES.len());
        for &root in TOP_LEVEL_RULES {
            if !file.rules.contains_key(root) {
                continue;
            }
            let r = GrammarRecognizer::build(&file, root, &mut warn)?;
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
        self.accepts_with_demotions(sql).map(|(v, _)| v)
    }

    /// Like `accepts` but also returns each keyword that was demoted to
    /// `IDENT` because the grammar didn't list it. Aggregating demotions
    /// across the corpus surfaces silent drift if a refresh inadvertently
    /// drops a keyword the grammar relied on.
    pub fn accepts_with_demotions(&self, sql: &str) -> Result<(bool, Vec<String>), String> {
        let dialect = PostgreSqlDialect {};
        let raw_tokens = Tokenizer::new(&dialect, sql)
            .tokenize()
            .map_err(|e| format!("tokenize: {e}"))?;

        let mut demotions: Vec<String> = Vec::new();
        let terminals: Vec<String> = raw_tokens
            .iter()
            .filter_map(|t| {
                let term = map_token(t);
                if matches!(term, Terminal::Skip) {
                    return None;
                }
                // Assumption: top-level statement productions don't list a
                // trailing `;`. The splitter already strips the terminating
                // semicolon, and statement rules in the current grammar
                // don't list one.
                if matches!(t, Token::SemiColon) {
                    return None;
                }
                // See `grammar_keywords` above.
                let term = match term {
                    Terminal::Keyword(kw) if !self.grammar_keywords.contains(&kw) => {
                        demotions.push(kw);
                        Terminal::CharClass("IDENT")
                    }
                    other => other,
                };
                Some(terminal_input_string(&term))
            })
            .collect();

        if terminals.is_empty() {
            // Don't collapse "no real tokens after filtering" with "real
            // tokens that no rule accepts" — a future `map_token` regression
            // that classified everything as `Skip` would otherwise silently
            // route every statement to `lint-too-lenient`. Routing to
            // `parse-error` makes the failure visible.
            return Err("no terminals after Skip filter".to_string());
        }

        for (_root_name, recognizer) in &self.recognizers {
            if recognizer.accepts(&terminals) {
                return Ok((true, demotions));
            }
        }
        Ok((false, demotions))
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
