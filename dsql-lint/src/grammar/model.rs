//! Serde types matching the grammar JSON shape.

use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum TokenType {
    Terminal,
    NonTerminal,
    Quoted,
    CharClass,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarToken {
    pub text: String,
    pub token_type: TokenType,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Production {
    pub choices: Vec<Vec<GrammarToken>>,
    /// `true` if the production may match the empty string.
    pub optional: bool,
    /// `Some("")` for `*`/`+` repetition with no separator;
    /// `Some(sep)` for separated repetition; `None` for no repetition.
    pub repetition: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarFile {
    pub rules: IndexMap<String, Production>,
    /// Declared in the JSON but the grammar oracle enumerates
    /// `TOP_LEVEL_RULES` instead, since the file's root is a dispatch
    /// rule that isn't itself defined. Kept on the type so deserialization
    /// faithfully reflects the schema.
    #[allow(dead_code)]
    pub root: String,
}
