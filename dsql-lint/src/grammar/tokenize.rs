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

        Token::Number(_, is_long) => {
            if *is_long {
                Terminal::CharClass("ICONST")
            } else {
                let text = match tok {
                    Token::Number(s, _) => s.as_str(),
                    _ => unreachable!(),
                };
                if text.bytes().any(|b| b == b'.' || b == b'e' || b == b'E') {
                    Terminal::CharClass("FCONST")
                } else {
                    Terminal::CharClass("ICONST")
                }
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

        // Operators the grammar doesn't list as `Quoted` go through the
        // `Op` character class — recognized only where the grammar permits
        // an operator, rejected elsewhere.
        Token::SemiColon => Terminal::Punct(";"),
        Token::Backslash => Terminal::CharClass("Op"),
        Token::DoubleEq => Terminal::CharClass("Op"),
        Token::Spaceship => Terminal::CharClass("Op"),
        Token::DuckIntDiv => Terminal::CharClass("Op"),
        Token::StringConcat => Terminal::CharClass("Op"),
        Token::Ampersand => Terminal::CharClass("Op"),
        Token::Pipe => Terminal::CharClass("Op"),
        Token::LBrace => Terminal::CharClass("Op"),
        Token::RBrace => Terminal::CharClass("Op"),
        Token::Sharp => Terminal::CharClass("Op"),
        Token::DoubleSharp => Terminal::CharClass("Op"),
        Token::Tilde => Terminal::CharClass("Op"),
        Token::TildeAsterisk => Terminal::CharClass("Op"),
        Token::ExclamationMarkTilde => Terminal::CharClass("Op"),
        Token::ExclamationMarkTildeAsterisk => Terminal::CharClass("Op"),
        Token::DoubleTilde => Terminal::CharClass("Op"),
        Token::DoubleTildeAsterisk => Terminal::CharClass("Op"),
        Token::ExclamationMarkDoubleTilde => Terminal::CharClass("Op"),
        Token::ExclamationMarkDoubleTildeAsterisk => Terminal::CharClass("Op"),
        Token::ShiftLeft => Terminal::CharClass("Op"),
        Token::ShiftRight => Terminal::CharClass("Op"),
        Token::Overlap => Terminal::CharClass("Op"),
        Token::ExclamationMark => Terminal::CharClass("Op"),
        Token::DoubleExclamationMark => Terminal::CharClass("Op"),
        Token::AtSign => Terminal::CharClass("Op"),
        Token::CaretAt => Terminal::CharClass("Op"),
        Token::PGSquareRoot => Terminal::CharClass("Op"),
        Token::PGCubeRoot => Terminal::CharClass("Op"),
        Token::Arrow => Terminal::CharClass("Op"),
        Token::LongArrow => Terminal::CharClass("Op"),
        Token::HashArrow => Terminal::CharClass("Op"),
        Token::AtDashAt => Terminal::CharClass("Op"),
        Token::QuestionMarkDash => Terminal::CharClass("Op"),
        Token::AmpersandLeftAngleBracket => Terminal::CharClass("Op"),
        Token::AmpersandRightAngleBracket => Terminal::CharClass("Op"),
        Token::AmpersandLeftAngleBracketVerticalBar => Terminal::CharClass("Op"),
        Token::VerticalBarAmpersandRightAngleBracket => Terminal::CharClass("Op"),
        Token::TwoWayArrow => Terminal::CharClass("Op"),
        Token::LeftAngleBracketCaret => Terminal::CharClass("Op"),
        Token::RightAngleBracketCaret => Terminal::CharClass("Op"),
        Token::QuestionMarkSharp => Terminal::CharClass("Op"),
        Token::QuestionMarkDashVerticalBar => Terminal::CharClass("Op"),
        Token::QuestionMarkDoubleVerticalBar => Terminal::CharClass("Op"),
        Token::TildeEqual => Terminal::CharClass("Op"),
        Token::ShiftLeftVerticalBar => Terminal::CharClass("Op"),
        Token::VerticalBarShiftRight => Terminal::CharClass("Op"),
        Token::VerticalBarRightAngleBracket => Terminal::CharClass("Op"),
        Token::HashLongArrow => Terminal::CharClass("Op"),
        Token::AtArrow => Terminal::CharClass("Op"),
        Token::ArrowAt => Terminal::CharClass("Op"),
        Token::HashMinus => Terminal::CharClass("Op"),
        Token::AtQuestion => Terminal::CharClass("Op"),
        Token::AtAt => Terminal::CharClass("Op"),
        Token::Question => Terminal::CharClass("Op"),
        Token::QuestionAnd => Terminal::CharClass("Op"),
        Token::QuestionPipe => Terminal::CharClass("Op"),
        Token::CustomBinaryOperator(_) => Terminal::CharClass("Op"),
    }
}

/// Every CharClass name this mapper can emit. Asserted exhaustive against
/// the grammar by `every_charclass_in_grammar_has_a_producer` so a refresh
/// that adds a new class breaks loudly instead of silently mis-tokenizing.
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
