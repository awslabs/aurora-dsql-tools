//! Maps `sqlparser` tokens to grammar terminals. The match is exhaustive
//! by design — a new `Token` variant in a sqlparser bump fails to compile.

use sqlparser::tokenizer::Token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    Keyword(String),
    Punct(&'static str),
    CharClass(&'static str),
    Skip,
}

pub fn map_token(tok: &Token) -> Terminal {
    match tok {
        Token::EOF => Terminal::Skip,
        Token::Whitespace(_) => Terminal::Skip,

        Token::Word(w) => {
            use sqlparser::keywords::Keyword;
            // Quoted identifiers (`"select"`) are identifiers regardless of
            // how the inside spells.
            if w.quote_style.is_some() || w.keyword == Keyword::NoKeyword {
                Terminal::CharClass("IDENT")
            } else {
                Terminal::Keyword(w.value.to_ascii_uppercase())
            }
        }

        Token::Number(s, is_long) => {
            if *is_long || !s.bytes().any(|b| b == b'.' || b == b'e' || b == b'E') {
                Terminal::CharClass("ICONST")
            } else {
                Terminal::CharClass("FCONST")
            }
        }
        Token::Char(_) => Terminal::CharClass("SCONST"),
        Token::SingleQuotedString(_) => Terminal::CharClass("SCONST"),
        Token::DoubleQuotedString(_) => Terminal::CharClass("IDENT"),
        Token::TripleSingleQuotedString(_) => Terminal::CharClass("SCONST"),
        Token::TripleDoubleQuotedString(_) => Terminal::CharClass("IDENT"),
        Token::DollarQuotedString(_) => Terminal::CharClass("SCONST"),
        Token::SingleQuotedByteStringLiteral(_) => Terminal::CharClass("BCONST"),
        Token::DoubleQuotedByteStringLiteral(_) => Terminal::CharClass("BCONST"),
        Token::TripleSingleQuotedByteStringLiteral(_) => Terminal::CharClass("BCONST"),
        Token::TripleDoubleQuotedByteStringLiteral(_) => Terminal::CharClass("BCONST"),
        Token::SingleQuotedRawStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::DoubleQuotedRawStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::TripleSingleQuotedRawStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::TripleDoubleQuotedRawStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::NationalStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::QuoteDelimitedStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::NationalQuoteDelimitedStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::EscapedStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::UnicodeStringLiteral(_) => Terminal::CharClass("SCONST"),
        Token::HexStringLiteral(_) => Terminal::CharClass("XCONST"),
        Token::Placeholder(_) => Terminal::CharClass("PARAM"),

        Token::Comma => Terminal::Punct(","),
        Token::Period => Terminal::Punct("."),
        Token::Mul => Terminal::Punct("*"),
        Token::Plus => Terminal::Punct("+"),
        Token::Minus => Terminal::Punct("-"),
        Token::Div => Terminal::Punct("/"),
        Token::Mod => Terminal::Punct("%"),
        Token::Caret => Terminal::Punct("^"),
        Token::LParen => Terminal::Punct("("),
        Token::RParen => Terminal::Punct(")"),
        Token::LBracket => Terminal::Punct("["),
        Token::RBracket => Terminal::Punct("]"),
        Token::Colon => Terminal::Punct(":"),
        Token::DoubleColon => Terminal::Punct("::"),
        Token::Assignment => Terminal::Punct(":="),
        Token::Eq => Terminal::Punct("="),
        Token::Lt => Terminal::Punct("<"),
        Token::Gt => Terminal::Punct(">"),
        Token::LtEq => Terminal::Punct("<="),
        Token::GtEq => Terminal::Punct(">="),
        Token::Neq => Terminal::Punct("<>"),
        Token::RArrow => Terminal::Punct("=>"),

        Token::SemiColon => Terminal::Punct(";"),

        // Operators the grammar doesn't list as `Quoted` go through the
        // `Op` character class — recognized only where the grammar permits
        // an operator, rejected elsewhere. Listed as `|`-pattern alternatives
        // (rather than `_ =>`) so a sqlparser bump that adds a new variant
        // fails to compile, matching the rest of this match's contract.
        Token::Backslash
        | Token::DoubleEq
        | Token::Spaceship
        | Token::DuckIntDiv
        | Token::StringConcat
        | Token::Ampersand
        | Token::Pipe
        | Token::LBrace
        | Token::RBrace
        | Token::Sharp
        | Token::DoubleSharp
        | Token::Tilde
        | Token::TildeAsterisk
        | Token::ExclamationMarkTilde
        | Token::ExclamationMarkTildeAsterisk
        | Token::DoubleTilde
        | Token::DoubleTildeAsterisk
        | Token::ExclamationMarkDoubleTilde
        | Token::ExclamationMarkDoubleTildeAsterisk
        | Token::ShiftLeft
        | Token::ShiftRight
        | Token::Overlap
        | Token::ExclamationMark
        | Token::DoubleExclamationMark
        | Token::AtSign
        | Token::CaretAt
        | Token::PGSquareRoot
        | Token::PGCubeRoot
        | Token::Arrow
        | Token::LongArrow
        | Token::HashArrow
        | Token::AtDashAt
        | Token::QuestionMarkDash
        | Token::AmpersandLeftAngleBracket
        | Token::AmpersandRightAngleBracket
        | Token::AmpersandLeftAngleBracketVerticalBar
        | Token::VerticalBarAmpersandRightAngleBracket
        | Token::TwoWayArrow
        | Token::LeftAngleBracketCaret
        | Token::RightAngleBracketCaret
        | Token::QuestionMarkSharp
        | Token::QuestionMarkDashVerticalBar
        | Token::QuestionMarkDoubleVerticalBar
        | Token::TildeEqual
        | Token::ShiftLeftVerticalBar
        | Token::VerticalBarShiftRight
        | Token::VerticalBarRightAngleBracket
        | Token::HashLongArrow
        | Token::AtArrow
        | Token::ArrowAt
        | Token::HashMinus
        | Token::AtQuestion
        | Token::AtAt
        | Token::Question
        | Token::QuestionAnd
        | Token::QuestionPipe
        | Token::CustomBinaryOperator(_) => Terminal::CharClass("Op"),
    }
}

/// Every CharClass name this mapper can emit. Asserted to cover every
/// CharClass the grammar references, by
/// `every_charclass_in_grammar_has_a_producer` — so a refresh that adds a
/// new class breaks loudly instead of silently mis-tokenizing. (The
/// reverse — stale entries here that nothing references — is not checked.)
pub const PRODUCED_CHARCLASSES: &[&str] = &[
    "IDENT", "ICONST", "FCONST", "SCONST", "BCONST", "XCONST", "PARAM", "Op",
];

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::tokenizer::Tokenizer;

    fn first_real_terminal(sql: &str) -> Terminal {
        let dialect = PostgreSqlDialect {};
        let tokens = Tokenizer::new(&dialect, sql).tokenize().unwrap();
        for tok in tokens {
            let term = map_token(&tok);
            if !matches!(term, Terminal::Skip) {
                return term;
            }
        }
        panic!("no non-skip terminal in {:?}", sql);
    }

    #[test]
    fn keyword_uppercased() {
        assert_eq!(
            first_real_terminal("create"),
            Terminal::Keyword("CREATE".into())
        );
        assert_eq!(
            first_real_terminal("Create"),
            Terminal::Keyword("CREATE".into())
        );
    }

    #[test]
    fn quoted_identifier_is_ident_not_keyword() {
        assert_eq!(
            first_real_terminal("\"create\""),
            Terminal::CharClass("IDENT")
        );
    }

    #[test]
    fn unquoted_identifier_is_ident() {
        assert_eq!(first_real_terminal("foo"), Terminal::CharClass("IDENT"));
    }

    #[test]
    fn integer_is_iconst() {
        assert_eq!(first_real_terminal("42"), Terminal::CharClass("ICONST"));
    }

    #[test]
    fn float_is_fconst() {
        assert_eq!(first_real_terminal("3.14"), Terminal::CharClass("FCONST"));
        assert_eq!(first_real_terminal("1e10"), Terminal::CharClass("FCONST"));
    }

    #[test]
    fn string_is_sconst() {
        assert_eq!(first_real_terminal("'hi'"), Terminal::CharClass("SCONST"));
    }

    #[test]
    fn punctuation_passes_through() {
        assert_eq!(first_real_terminal(","), Terminal::Punct(","));
        assert_eq!(first_real_terminal("::"), Terminal::Punct("::"));
        assert_eq!(first_real_terminal(">="), Terminal::Punct(">="));
    }
}
