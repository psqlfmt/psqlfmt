#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub line: usize,
    pub column: usize,
    pub at_line_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Number,
    String,
    DollarString,
    QuotedIdentifier,
    Operator,
    Punctuation(char),
    LineComment,
    BlockComment,
    MetaCommand,
    BlankLine,
    Other,
}

impl Token {
    pub fn upper(&self) -> String {
        self.text.to_ascii_uppercase()
    }

    pub fn lower(&self) -> String {
        self.text.to_ascii_lowercase()
    }

    pub fn is_word(&self) -> bool {
        self.kind == TokenKind::Word
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.kind, TokenKind::LineComment | TokenKind::BlockComment)
    }

    pub fn is_punctuation(&self, ch: char) -> bool {
        self.kind == TokenKind::Punctuation(ch)
    }
}

pub fn tokenize(sql: &str) -> Vec<Token> {
    Lexer::new(sql).tokenize()
}

struct Lexer<'a> {
    sql: &'a str,
    pos: usize,
    line: usize,
    column: usize,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            pos: 0,
            line: 1,
            column: 1,
            at_line_start: true,
        }
    }

    fn tokenize(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();

        while !self.is_eof() {
            let Some(ch) = self.peek_char() else {
                break;
            };

            if ch.is_whitespace() {
                let blank_lines = self.consume_whitespace();
                if blank_lines > 0 {
                    tokens.push(self.synthetic(TokenKind::BlankLine, "\n\n"));
                }
                continue;
            }

            let line = self.line;
            let column = self.column;
            let at_line_start = self.at_line_start;

            let (kind, text) = if at_line_start && ch == '\\' {
                (TokenKind::MetaCommand, self.consume_until_newline())
            } else if self.starts_with("--") {
                (TokenKind::LineComment, self.consume_until_newline())
            } else if self.starts_with("/*") {
                (TokenKind::BlockComment, self.consume_block_comment())
            } else if self.starts_case_insensitive("U&\"") {
                (
                    TokenKind::QuotedIdentifier,
                    self.consume_quoted_identifier(true),
                )
            } else if ch == '"' {
                (
                    TokenKind::QuotedIdentifier,
                    self.consume_quoted_identifier(false),
                )
            } else if self.starts_case_insensitive("U&'")
                || self.starts_case_insensitive("E'")
                || self.starts_case_insensitive("B'")
                || self.starts_case_insensitive("X'")
                || ch == '\''
            {
                (TokenKind::String, self.consume_single_quoted_string())
            } else if ch == '$' {
                if let Some(text) = self.try_consume_dollar_string() {
                    (TokenKind::DollarString, text)
                } else if self
                    .peek_next_char()
                    .is_some_and(|next| next.is_ascii_digit())
                {
                    (TokenKind::Word, self.consume_parameter())
                } else {
                    (TokenKind::Operator, self.consume_operator())
                }
            } else if ch == '@' && self.peek_next_char().is_some_and(is_ident_start) {
                (TokenKind::Word, self.consume_sqlc_parameter())
            } else if is_number_start(ch, self.peek_next_char()) {
                (TokenKind::Number, self.consume_number())
            } else if is_ident_start(ch) {
                (TokenKind::Word, self.consume_identifier())
            } else if let Some(punctuation) = punctuation_char(ch) {
                let text = self.consume_char().to_string();
                (TokenKind::Punctuation(punctuation), text)
            } else if is_operator_char(ch) {
                (TokenKind::Operator, self.consume_operator())
            } else {
                let text = self.consume_char().to_string();
                (TokenKind::Other, text)
            };

            tokens.push(Token {
                kind,
                text,
                line,
                column,
                at_line_start,
            });
        }

        tokens
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.sql.len()
    }

    fn synthetic(&self, kind: TokenKind, text: &str) -> Token {
        Token {
            kind,
            text: text.to_string(),
            line: self.line,
            column: self.column,
            at_line_start: true,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.sql[self.pos..].chars().next()
    }

    fn peek_next_char(&self) -> Option<char> {
        let mut chars = self.sql[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn starts_with(&self, needle: &str) -> bool {
        self.sql[self.pos..].starts_with(needle)
    }

    fn starts_case_insensitive(&self, needle: &str) -> bool {
        self.sql[self.pos..]
            .get(..needle.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
    }

    fn consume_char(&mut self) -> char {
        let ch = self.peek_char().expect("consume_char at eof");
        self.pos += ch.len_utf8();
        match ch {
            '\n' => {
                self.line += 1;
                self.column = 1;
                self.at_line_start = true;
            }
            '\r' => {
                if self.peek_char() == Some('\n') {
                    self.pos += 1;
                }
                self.line += 1;
                self.column = 1;
                self.at_line_start = true;
            }
            _ => {
                self.column += 1;
                if !ch.is_whitespace() {
                    self.at_line_start = false;
                }
            }
        }
        ch
    }

    fn consume_whitespace(&mut self) -> usize {
        let mut newline_count = 0usize;
        let mut blank_lines = 0usize;
        let mut saw_only_space_since_newline = self.at_line_start;

        while let Some(ch) = self.peek_char() {
            if !ch.is_whitespace() {
                break;
            }
            match ch {
                '\n' => {
                    newline_count += 1;
                    if newline_count >= 2 && saw_only_space_since_newline {
                        blank_lines += 1;
                    }
                    saw_only_space_since_newline = true;
                    self.consume_char();
                }
                '\r' => {
                    newline_count += 1;
                    if newline_count >= 2 && saw_only_space_since_newline {
                        blank_lines += 1;
                    }
                    saw_only_space_since_newline = true;
                    self.consume_char();
                }
                _ => {
                    self.consume_char();
                }
            }
        }

        blank_lines
    }

    fn consume_until_newline(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if matches!(ch, '\n' | '\r') {
                break;
            }
            self.consume_char();
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_block_comment(&mut self) -> String {
        let start = self.pos;
        let mut depth = 0usize;

        while !self.is_eof() {
            if self.starts_with("/*") {
                depth += 1;
                self.consume_char();
                self.consume_char();
            } else if self.starts_with("*/") {
                self.consume_char();
                self.consume_char();
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            } else {
                self.consume_char();
            }
        }

        self.sql[start..self.pos].to_string()
    }

    fn consume_quoted_identifier(&mut self, unicode_prefix: bool) -> String {
        let start = self.pos;
        if unicode_prefix {
            self.consume_char();
            self.consume_char();
        }
        self.consume_char();
        while let Some(ch) = self.peek_char() {
            self.consume_char();
            if ch == '"' {
                if self.peek_char() == Some('"') {
                    self.consume_char();
                } else {
                    break;
                }
            }
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_single_quoted_string(&mut self) -> String {
        let start = self.pos;
        if self.starts_case_insensitive("U&'") {
            self.consume_char();
            self.consume_char();
        } else if matches!(self.peek_char(), Some('E' | 'e' | 'B' | 'b' | 'X' | 'x'))
            && self.peek_next_char() == Some('\'')
        {
            self.consume_char();
        }

        self.consume_char();
        while let Some(ch) = self.peek_char() {
            self.consume_char();
            if ch == '\'' {
                if self.peek_char() == Some('\'') {
                    self.consume_char();
                } else {
                    break;
                }
            } else if ch == '\\' && self.sql[start..].starts_with(['E', 'e']) && !self.is_eof() {
                self.consume_char();
            }
        }

        self.sql[start..self.pos].to_string()
    }

    fn try_consume_dollar_string(&mut self) -> Option<String> {
        let delimiter = self.dollar_delimiter_at(self.pos)?;
        let start = self.pos;
        for _ in delimiter.chars() {
            self.consume_char();
        }

        while !self.is_eof() {
            if self.starts_with(&delimiter) {
                for _ in delimiter.chars() {
                    self.consume_char();
                }
                return Some(self.sql[start..self.pos].to_string());
            }
            self.consume_char();
        }

        Some(self.sql[start..self.pos].to_string())
    }

    fn dollar_delimiter_at(&self, pos: usize) -> Option<String> {
        let rest = &self.sql[pos..];
        if !rest.starts_with('$') {
            return None;
        }
        let mut len = 1usize;
        for ch in rest[1..].chars() {
            len += ch.len_utf8();
            if ch == '$' {
                return Some(rest[..len].to_string());
            }
            if !(ch == '_' || ch.is_ascii_alphanumeric()) {
                return None;
            }
        }
        None
    }

    fn consume_parameter(&mut self) -> String {
        let start = self.pos;
        self.consume_char();
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                self.consume_char();
            } else {
                break;
            }
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_sqlc_parameter(&mut self) -> String {
        let start = self.pos;
        self.consume_char();
        while let Some(ch) = self.peek_char() {
            if is_ident_continue(ch) {
                self.consume_char();
            } else {
                break;
            }
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_number(&mut self) -> String {
        let start = self.pos;
        if self.peek_char() == Some('.') {
            self.consume_char();
        }
        while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
            self.consume_char();
        }
        if self.peek_char() == Some('.')
            && self.peek_next_char().is_some_and(|ch| ch.is_ascii_digit())
        {
            self.consume_char();
            while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
                self.consume_char();
            }
        }
        if matches!(self.peek_char(), Some('e' | 'E')) {
            let checkpoint = (self.pos, self.line, self.column, self.at_line_start);
            self.consume_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.consume_char();
            }
            if self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
                while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
                    self.consume_char();
                }
            } else {
                (self.pos, self.line, self.column, self.at_line_start) = checkpoint;
            }
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_identifier(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if is_ident_continue(ch) {
                self.consume_char();
            } else {
                break;
            }
        }
        self.sql[start..self.pos].to_string()
    }

    fn consume_operator(&mut self) -> String {
        const MULTI_OPERATORS: &[&str] = &[
            ">>=", "<<=", "<->>", "<<->", "<->", "->>", "->", "#>>", "#>", "?&", "?|", "@?", "@@",
            "@@@", "!!", "||/", "|/", "!~~*", "!~~", "~~*", "~~", "!~*", "!~", "~*", "<=>", "<>",
            ">=", "<=", "=>", "==", "!=", ":=", "::", "||", "&&", "<<", ">>", "@-@", "##", "#-",
            "<<|", "|>>", "&<|", "&<", "|&>", "&>", "<^", ">^", "?#", "?-|", "?-", "?||", "?|",
            "@>", "<@", "~=", "~<=~", "~>=~", "~>~", "~<~", "*<>", "*<=", "*>=", "*<", "*>", "*=",
        ];

        for op in MULTI_OPERATORS {
            if self.starts_with(op) {
                for _ in op.chars() {
                    self.consume_char();
                }
                return (*op).to_string();
            }
        }

        let start = self.pos;
        self.consume_char();
        while let Some(ch) = self.peek_char() {
            if is_operator_char(ch)
                && !would_start_comment(&self.sql[self.pos..])
                && !(ch == '@' && self.peek_next_char().is_some_and(is_ident_start))
            {
                self.consume_char();
            } else {
                break;
            }
        }
        self.sql[start..self.pos].to_string()
    }
}

fn is_number_start(ch: char, next: Option<char>) -> bool {
    ch.is_ascii_digit() || (ch == '.' && next.is_some_and(|next| next.is_ascii_digit()))
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic() || (ch as u32) >= 0x80
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphanumeric() || (ch as u32) >= 0x80
}

fn punctuation_char(ch: char) -> Option<char> {
    match ch {
        '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '.' => Some(ch),
        _ => None,
    }
}

fn is_operator_char(ch: char) -> bool {
    matches!(
        ch,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '^'
            | '='
            | '<'
            | '>'
            | '!'
            | '~'
            | '|'
            | '&'
            | '?'
            | '@'
            | '#'
            | ':'
    )
}

fn would_start_comment(rest: &str) -> bool {
    rest.starts_with("--") || rest.starts_with("/*")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_nested_block_comments() {
        let tokens = tokenize("select /* a /* nested */ ok */ 1");
        assert!(tokens.iter().any(|token| {
            token.kind == TokenKind::BlockComment && token.text == "/* a /* nested */ ok */"
        }));
    }

    #[test]
    fn lexes_dollar_quoted_plpgsql() {
        let sql = "create function f() returns void language plpgsql as $$ begin raise notice 'x'; end $$;";
        let tokens = tokenize(sql);
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::DollarString)
        );
    }

    #[test]
    fn lexes_postgres_operators() {
        let tokens = tokenize("select doc #>> '{a,b}', embedding <#> $1 from t");
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Operator && token.text == "#>>")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Operator && token.text == "<#>")
        );
    }

    #[test]
    fn lexes_sqlc_named_parameters() {
        let tokens = tokenize("select @code, sqlc.arg(name)");
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Word && token.text == "@code")
        );
    }
}
