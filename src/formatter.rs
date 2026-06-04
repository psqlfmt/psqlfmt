use crate::config::{CommaStyle, FormatOptions};
use crate::keywords;
use crate::lexer::{Token, TokenKind, tokenize};

pub fn format_sql(sql: &str) -> String {
    format_sql_with_options(sql, &FormatOptions::default())
}

pub fn format_sql_with_options(sql: &str, options: &FormatOptions) -> String {
    let mut formatter = Formatter::new(options);
    let mut formatted = formatter.format(sql);
    formatted = options.apply_line_endings(&formatted);
    formatted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    None,
    Select,
    From,
    Where,
    Group,
    Having,
    JoinOn,
    Window,
    Order,
    Limit,
    Values,
    Set,
    Returning,
    With,
    Create,
    Other,
}

#[derive(Debug, Clone)]
struct ParenContext {
    indent: usize,
    multiline: bool,
    insert_target_list: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelimiterKind {
    Paren,
    Bracket,
}

#[derive(Debug, Clone, Copy)]
struct DelimiterContext {
    kind: DelimiterKind,
    multiline: bool,
}

#[derive(Debug, Clone)]
struct SignificantToken {
    kind: TokenKind,
    text: String,
    upper: String,
}

struct Formatter<'a> {
    options: &'a FormatOptions,
    output: String,
    indent: usize,
    block_indent: usize,
    clause: Clause,
    parens: Vec<ParenContext>,
    brackets: Vec<ParenContext>,
    delimiters: Vec<DelimiterContext>,
    significant: Vec<SignificantToken>,
    select_depth: usize,
    create_depth: usize,
    in_merge_statement: bool,
    plpgsql_body: bool,
    plpgsql_declare_section: bool,
    plpgsql_exception_section: bool,
    plpgsql_case_depth: usize,
    expression_cases: Vec<usize>,
    in_between: bool,
    in_policy_statement: bool,
    list_items_on_line: usize,
    clause_indent_extra: usize,
    indent_unit: String,
}

impl<'a> Formatter<'a> {
    fn new(options: &'a FormatOptions) -> Self {
        Self {
            options,
            output: String::new(),
            indent: 0,
            block_indent: 0,
            clause: Clause::None,
            parens: Vec::new(),
            brackets: Vec::new(),
            delimiters: Vec::new(),
            significant: Vec::new(),
            select_depth: 0,
            create_depth: 0,
            in_merge_statement: false,
            plpgsql_body: false,
            plpgsql_declare_section: false,
            plpgsql_exception_section: false,
            plpgsql_case_depth: 0,
            expression_cases: Vec::new(),
            in_between: false,
            in_policy_statement: false,
            list_items_on_line: 0,
            clause_indent_extra: 0,
            indent_unit: options.indent_unit(),
        }
    }

    fn format(&mut self, sql: &str) -> String {
        let source = if self.options.anonymize {
            anonymize_sql(sql)
        } else {
            sql.to_string()
        };
        let tokens = tokenize(&source);
        self.format_tokens(&tokens, false);
        self.finish()
    }

    fn format_tokens(&mut self, tokens: &[Token], embedded: bool) {
        let mut i = 0usize;
        while i < tokens.len() {
            let token = &tokens[i];

            match token.kind {
                TokenKind::BlankLine => {
                    if self.options.keep_blank_lines {
                        self.blank_line();
                    }
                    i += 1;
                    continue;
                }
                TokenKind::LineComment | TokenKind::BlockComment => {
                    self.emit_comment(token);
                    i += 1;
                    continue;
                }
                TokenKind::MetaCommand => {
                    self.emit_meta_command(&token.text);
                    self.push_sig(token, token.text.clone());
                    i += 1;
                    continue;
                }
                TokenKind::DollarString => {
                    self.emit_dollar_string(token);
                    self.push_sig(token, token.text.clone());
                    i += 1;
                    continue;
                }
                _ => {}
            }

            if token.is_word() {
                let upper = token.upper();
                if upper == "POLICY" {
                    self.in_policy_statement = true;
                }
                if upper == "CREATE" && self.try_emit_create_routine(tokens, &mut i, embedded) {
                    continue;
                }
                if upper == "CREATE" && self.try_emit_create_trigger(tokens, &mut i, embedded) {
                    continue;
                }
                if matches!(upper.as_str(), "GRANT" | "REVOKE")
                    && self.try_emit_grant_or_revoke(tokens, &mut i, embedded)
                {
                    continue;
                }
                if self.try_emit_multiword_clause(tokens, &mut i) {
                    continue;
                }
                if self.plpgsql_body
                    && self.try_emit_plpgsql_keyword(token, &upper, tokens.get(i + 1))
                {
                    i += 1;
                    continue;
                }
                if self.try_emit_keyword(token, &upper, tokens.get(i + 1), embedded, tokens, i) {
                    i += 1;
                    continue;
                }
            }

            match token.kind {
                TokenKind::Punctuation(';') => self.emit_semicolon(embedded),
                TokenKind::Punctuation(',') => self.emit_comma(),
                TokenKind::Punctuation('(') => self.emit_open_paren(tokens, i),
                TokenKind::Punctuation(')') => self.emit_close_paren(),
                TokenKind::Punctuation('.') => self.emit_dot(),
                TokenKind::Punctuation('[') | TokenKind::Punctuation('{') => {
                    self.emit_open_bracket(tokens, i)
                }
                TokenKind::Punctuation(']') | TokenKind::Punctuation('}') => {
                    self.emit_close_bracket(&token.text)
                }
                TokenKind::Operator => self.emit_operator(token, tokens.get(i + 1)),
                TokenKind::Word
                | TokenKind::Number
                | TokenKind::String
                | TokenKind::QuotedIdentifier => {
                    self.emit_atom(token, tokens.get(i + 1));
                }
                TokenKind::Punctuation(_) | TokenKind::Other => self.emit_raw_atom(&token.text),
                TokenKind::DollarString
                | TokenKind::LineComment
                | TokenKind::BlockComment
                | TokenKind::MetaCommand
                | TokenKind::BlankLine => unreachable!(),
            }

            self.push_sig(token, token.text.clone());
            i += 1;
        }
    }

    fn try_emit_create_routine(&mut self, tokens: &[Token], i: &mut usize, embedded: bool) -> bool {
        let start = *i;
        let Some(end) = tokens[start..]
            .iter()
            .position(|token| token.is_punctuation(';'))
            .map(|offset| start + offset)
        else {
            return false;
        };

        let statement = &tokens[start..end];
        if statement.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::LineComment
                    | TokenKind::BlockComment
                    | TokenKind::BlankLine
                    | TokenKind::MetaCommand
            )
        }) {
            return false;
        }
        if create_routine_clause_start(statement).is_none()
            || find_top_level_word(statement, &["AS"], 1).is_none()
        {
            return false;
        }

        self.emit_create_routine_statement(statement);
        self.emit_semicolon(embedded);
        *i = end + 1;
        true
    }

    fn emit_create_routine_statement(&mut self, statement: &[Token]) {
        let Some(clause_start) = create_routine_clause_start(statement) else {
            return;
        };

        let base_indent = self.statement_indent();
        self.newline_if_needed();
        self.emit_inline_tokens_line(
            &statement[..clause_start],
            base_indent,
            TokenCaseContext::Normal,
        );

        let mut cursor = clause_start;
        while cursor < statement.len() {
            let next = find_next_routine_clause(statement, cursor + 1, statement.len());
            self.emit_routine_clause(&statement[cursor..next], base_indent);
            cursor = next;
        }

        self.clause = Clause::Other;
        self.list_items_on_line = 0;
    }

    fn emit_routine_clause(&mut self, clause: &[Token], base_indent: usize) {
        if clause.is_empty() {
            return;
        }

        if let Some(dollar_index) = clause
            .iter()
            .position(|token| token.kind == TokenKind::DollarString)
        {
            self.emit_inline_tokens_line(
                &clause[..dollar_index],
                base_indent,
                TokenCaseContext::Normal,
            );
            for token in &clause[..dollar_index] {
                self.push_sig(token, token.text.clone());
            }
            self.indent = base_indent;
            self.emit_dollar_string(&clause[dollar_index]);
            if dollar_index + 1 < clause.len() {
                self.emit_inline_tokens_line(
                    &clause[dollar_index + 1..],
                    base_indent,
                    TokenCaseContext::Normal,
                );
            }
            return;
        }

        self.emit_inline_tokens_line(clause, base_indent, TokenCaseContext::Normal);
    }

    fn try_emit_create_trigger(&mut self, tokens: &[Token], i: &mut usize, embedded: bool) -> bool {
        let start = *i;
        let Some(end) = tokens[start..]
            .iter()
            .position(|token| token.is_punctuation(';'))
            .map(|offset| start + offset)
        else {
            return false;
        };

        let statement = &tokens[start..end];
        if create_trigger_name_index(statement).is_none() {
            return false;
        }

        self.emit_create_trigger_statement(statement);
        self.emit_semicolon(embedded);
        *i = end + 1;
        true
    }

    fn emit_create_trigger_statement(&mut self, statement: &[Token]) {
        let Some((_, name_index)) = create_trigger_name_index(statement) else {
            return;
        };
        let Some(on_index) = find_top_level_word(statement, &["ON"], name_index + 1) else {
            self.emit_inline_tokens_line(
                statement,
                self.statement_indent(),
                TokenCaseContext::Normal,
            );
            return;
        };

        let base_indent = self.statement_indent();
        self.newline_if_needed();
        self.emit_inline_tokens_line(
            &statement[..=name_index],
            base_indent,
            TokenCaseContext::Normal,
        );
        self.emit_trigger_action(&statement[name_index + 1..on_index], base_indent);

        let execute_index =
            find_top_level_word(statement, &["EXECUTE"], on_index + 1).unwrap_or(statement.len());
        let mut cursor = on_index + 1;
        let table_end = find_next_trigger_clause(statement, cursor, execute_index);
        self.emit_prefixed_inline_line(
            "ON",
            &statement[cursor..table_end],
            base_indent,
            TokenCaseContext::GrantIdentifier,
        );
        cursor = table_end;

        while cursor < execute_index {
            let next = find_next_trigger_clause(statement, cursor + 1, execute_index);
            self.emit_inline_tokens_line(
                &statement[cursor..next],
                base_indent,
                TokenCaseContext::Normal,
            );
            cursor = next;
        }

        if execute_index < statement.len() {
            self.emit_inline_tokens_line(
                &statement[execute_index..],
                base_indent,
                TokenCaseContext::Normal,
            );
        }

        self.clause = Clause::Other;
        self.list_items_on_line = 0;
    }

    fn emit_trigger_action(&mut self, action: &[Token], base_indent: usize) {
        if action.is_empty() {
            return;
        }

        if let Some(of_index) = find_trigger_update_of(action) {
            let columns = &action[of_index + 1..];
            let column_items = split_top_level_commas(columns);
            if column_items.len() > 1 {
                self.emit_inline_tokens_line(
                    &action[..=of_index],
                    base_indent,
                    TokenCaseContext::Normal,
                );
                self.newline();
                self.emit_token_list(
                    &column_items,
                    base_indent + 1,
                    TokenCaseContext::GrantIdentifier,
                );
                return;
            }
        }

        self.emit_inline_tokens_line(action, base_indent, TokenCaseContext::Normal);
    }

    fn emit_prefixed_inline_line(
        &mut self,
        prefix: &str,
        tokens: &[Token],
        indent: usize,
        context: TokenCaseContext,
    ) {
        self.newline_if_needed();
        self.indent = indent;
        self.write_keyword_text(prefix);
        if !tokens.is_empty() {
            self.space();
            self.output
                .push_str(&format_inline_tokens(tokens, self.options, context));
        }
    }

    fn emit_inline_tokens_line(
        &mut self,
        tokens: &[Token],
        indent: usize,
        context: TokenCaseContext,
    ) {
        if tokens.is_empty() {
            return;
        }
        self.newline_if_needed();
        self.indent = indent;
        self.write_indent();
        self.output
            .push_str(&format_inline_tokens(tokens, self.options, context));
    }

    fn try_emit_multiword_clause(&mut self, tokens: &[Token], i: &mut usize) -> bool {
        let token = &tokens[*i];
        let upper = token.upper();
        let Some(next) = tokens.get(*i + 1) else {
            return false;
        };
        if !next.is_word() {
            return false;
        }
        let next_upper = next.upper();

        if upper == "END" && matches!(next_upper.as_str(), "IF" | "LOOP" | "CASE") {
            if next_upper == "CASE" {
                self.block_indent = self.block_indent.saturating_sub(2);
                self.plpgsql_case_depth = self.plpgsql_case_depth.saturating_sub(1);
            } else {
                self.block_indent = self.block_indent.saturating_sub(1);
            }
            self.newline();
            self.indent = self.block_indent + self.parens.len();
            self.write_word(token, Some(next));
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        }

        if upper == "DELETE" && next_upper == "FROM" {
            self.emit_statement_start(token, Some(next));
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        }

        if upper == "SELECT" && next_upper == "DISTINCT" {
            self.select_depth += 1;
            self.newline_if_needed();
            self.indent = self.statement_indent() + self.clause_indent_extra;
            self.write_word(token, Some(next));
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.clause = Clause::Select;
            self.list_items_on_line = 0;
            self.newline();
            self.indent = self.statement_indent() + self.clause_indent_extra + 1;
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        }

        if upper == "FOR"
            && self.is_policy_context()
            && matches!(
                next_upper.as_str(),
                "ALL" | "SELECT" | "INSERT" | "UPDATE" | "DELETE"
            )
        {
            self.space();
            self.write_word(token, Some(next));
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        }

        if upper == "FOR" && matches!(next_upper.as_str(), "UPDATE" | "SHARE") {
            self.newline_if_needed();
            self.indent = self.statement_indent() + self.clause_indent_extra;
            self.write_word(token, Some(next));
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        }

        let phrase = match (upper.as_str(), next_upper.as_str()) {
            ("GROUP", "BY") => Some((Clause::Group, true)),
            ("ORDER", "BY") => Some((Clause::Order, true)),
            ("PARTITION", "BY") if self.is_inside_over_clause() => None,
            ("ON", "CONFLICT") => Some((Clause::Other, true)),
            ("DO", "UPDATE") => Some((Clause::Set, true)),
            ("DO", "NOTHING") => Some((Clause::Other, false)),
            ("NOT", "MATCHED") => Some((Clause::Other, false)),
            ("WHEN", "MATCHED") => {
                self.emit_merge_when();
                self.write_word(token, Some(next));
                self.space();
                self.write_word(next, tokens.get(*i + 2));
                self.push_sig(token, token.text.clone());
                self.push_sig(next, next.text.clone());
                *i += 2;
                return true;
            }
            ("WHEN", "NOT") => {
                if tokens
                    .get(*i + 2)
                    .is_some_and(|token| token.is_word() && token.upper() == "MATCHED")
                {
                    self.emit_merge_when();
                    self.write_word(token, tokens.get(*i + 1));
                    self.space();
                    self.write_word(next, tokens.get(*i + 2));
                    self.space();
                    let third = &tokens[*i + 2];
                    self.write_word(third, tokens.get(*i + 3));
                    self.push_sig(token, token.text.clone());
                    self.push_sig(next, next.text.clone());
                    self.push_sig(third, third.text.clone());
                    *i += 3;
                    return true;
                }
                None
            }
            ("RETURN", "NEXT") | ("RETURN", "QUERY") if self.plpgsql_body => {
                self.emit_plpgsql_statement_start(token, Some(next));
                self.space();
                self.write_word(next, tokens.get(*i + 2));
                self.push_sig(token, token.text.clone());
                self.push_sig(next, next.text.clone());
                *i += 2;
                return true;
            }
            ("GET", "DIAGNOSTICS") | ("GET", "STACKED") if self.plpgsql_body => {
                self.emit_plpgsql_statement_start(token, Some(next));
                self.space();
                self.write_word(next, tokens.get(*i + 2));
                self.push_sig(token, token.text.clone());
                self.push_sig(next, next.text.clone());
                *i += 2;
                return true;
            }
            ("WITHOUT", "OVERLAPS") => Some((Clause::Other, false)),
            ("NOT", "ENFORCED") => Some((Clause::Other, false)),
            ("WITH", "CHECK") if self.is_policy_context() => Some((Clause::Other, true)),
            ("REJECT", "LIMIT") => Some((Clause::Other, false)),
            ("LOG", "VERBOSITY") => Some((Clause::Other, false)),
            ("ON", "ERROR") => Some((Clause::Other, false)),
            _ => None,
        };

        let Some((clause, force_newline)) = phrase else {
            return false;
        };

        if force_newline {
            self.newline_if_needed();
            self.indent = self.statement_indent();
            self.write_keyword_text(&upper);
            self.space();
            self.write_word(next, tokens.get(*i + 2));
            self.clause = clause;
            self.list_items_on_line = 0;
            self.newline();
            self.indent = self.statement_indent() + 1;
            self.push_sig(token, token.text.clone());
            self.push_sig(next, next.text.clone());
            *i += 2;
            return true;
        } else {
            self.space();
            self.write_word(token, Some(next));
        }
        self.space();
        self.write_word(next, tokens.get(*i + 2));

        self.push_sig(token, token.text.clone());
        self.push_sig(next, next.text.clone());
        *i += 2;
        true
    }

    fn try_emit_grant_or_revoke(
        &mut self,
        tokens: &[Token],
        i: &mut usize,
        embedded: bool,
    ) -> bool {
        let start = *i;
        let Some(end) = tokens[start..]
            .iter()
            .position(|token| token.is_punctuation(';'))
            .map(|offset| start + offset)
        else {
            return false;
        };

        let statement = &tokens[start..end];
        if statement.is_empty() {
            return false;
        }

        self.emit_grant_or_revoke_statement(statement);
        self.emit_semicolon(embedded);
        *i = end + 1;
        true
    }

    fn emit_grant_or_revoke_statement(&mut self, statement: &[Token]) {
        let action = statement[0].upper();
        let on_index = find_top_level_word(statement, &["ON"], 1);
        let recipient_words = if action == "REVOKE" {
            &["FROM", "TO"][..]
        } else {
            &["TO"][..]
        };
        let recipient_index = if let Some(on_index) = on_index {
            find_top_level_word(statement, recipient_words, on_index + 1)
        } else {
            find_top_level_word(statement, recipient_words, 1)
        };

        let privileges_end = on_index.or(recipient_index).unwrap_or(statement.len());
        let privileges = split_top_level_commas(&statement[1..privileges_end]);

        self.newline_if_needed();
        let base_indent = self.statement_indent();
        self.indent = base_indent;
        self.write_keyword_text(&action);
        self.newline();
        let grant_head_context = if on_index.is_some() {
            TokenCaseContext::GrantPrivilege
        } else {
            TokenCaseContext::GrantIdentifier
        };
        self.emit_token_list(&privileges, base_indent + 1, grant_head_context);

        if let Some(on_index) = on_index {
            let object_end = recipient_index.unwrap_or(statement.len());
            let (object_scope, object_list) =
                split_grant_object_scope(&statement[on_index + 1..object_end]);
            self.newline();
            self.indent = base_indent;
            self.write_keyword_text("ON");
            if !object_scope.is_empty() {
                self.space();
                self.output.push_str(&format_inline_tokens(
                    object_scope,
                    self.options,
                    TokenCaseContext::ObjectScope,
                ));
            }
            if !object_list.is_empty() {
                self.newline();
                let objects = split_top_level_commas(object_list);
                self.emit_token_list(&objects, base_indent + 1, TokenCaseContext::GrantIdentifier);
            }
        }

        if let Some(recipient_index) = recipient_index {
            let (recipients, tail) =
                split_grant_recipient_tail(&statement[recipient_index + 1..], action == "REVOKE");
            self.newline();
            self.indent = base_indent;
            self.write_word(
                &statement[recipient_index],
                statement.get(recipient_index + 1),
            );
            if !recipients.is_empty() {
                self.newline();
                let recipient_list = split_top_level_commas(recipients);
                self.emit_token_list(
                    &recipient_list,
                    base_indent + 1,
                    TokenCaseContext::GrantRecipient,
                );
            }
            if !tail.is_empty() {
                self.newline();
                self.indent = base_indent;
                self.output.push_str(&format_inline_tokens(
                    tail,
                    self.options,
                    TokenCaseContext::Normal,
                ));
            }
        }

        self.clause = Clause::Other;
        self.list_items_on_line = 0;
    }

    fn emit_token_list(&mut self, items: &[&[Token]], indent: usize, context: TokenCaseContext) {
        let items = items
            .iter()
            .copied()
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>();
        let item_count = items.len();
        for (idx, item) in items.into_iter().enumerate() {
            if idx > 0 {
                self.newline();
            }
            self.indent = indent;
            self.write_indent();
            self.output
                .push_str(&format_inline_tokens(item, self.options, context));
            if idx + 1 < item_count {
                self.output.push(',');
            }
        }
    }

    fn try_emit_keyword(
        &mut self,
        token: &Token,
        upper: &str,
        next: Option<&Token>,
        embedded: bool,
        tokens: &[Token],
        index: usize,
    ) -> bool {
        if self.is_contextual_identifier(upper, next)
            || self.is_create_table_column_name(upper)
            || self.is_insert_target_column_name()
            || self.is_values_identifier_list_item(upper, next)
            || self.is_distinct_from_operator(upper)
        {
            return false;
        }

        match upper {
            "WITH" if self.is_statement_boundary() || self.clause == Clause::None => {
                self.emit_clause_header(upper, Clause::With);
            }
            "SELECT" if self.plpgsql_body && self.last_upper_in(&["IN", "QUERY"]) => {
                self.select_depth += 1;
                self.emit_plpgsql_nested_clause_header(upper, Clause::Select);
            }
            "SELECT" => {
                self.select_depth += 1;
                self.emit_clause_header(upper, Clause::Select);
            }
            "FROM" => self.emit_clause_header(upper, Clause::From),
            "WHERE" if self.is_filter_where_context() => self.emit_filter_where(token, next),
            "WHERE" => self.emit_clause_header(upper, Clause::Where),
            "HAVING" => self.emit_clause_header(upper, Clause::Having),
            "WINDOW" => self.emit_clause_header(upper, Clause::Window),
            "LIMIT" | "OFFSET" | "FETCH" => self.emit_clause_header(upper, Clause::Limit),
            "VALUES" => self.emit_clause_header(upper, Clause::Values),
            "RETURNING" => {
                if self.is_merge_context() {
                    self.block_indent = 0;
                }
                self.emit_clause_header(upper, Clause::Returning);
            }
            "SET" if self.is_dml_statement() || self.last_upper_is("UPDATE") => {
                if self.is_referential_action_context() {
                    return false;
                }
                self.emit_clause_header(upper, Clause::Set);
            }
            "USING" if self.is_merge_context() => {
                self.emit_clause_header(upper, Clause::Other);
            }
            "USING" if self.is_policy_context() => {
                self.emit_clause_header(upper, Clause::Other);
            }
            "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "COPY" | "VACUUM" | "ANALYZE"
            | "REINDEX" | "GRANT" | "REVOKE" | "TRUNCATE" | "EXPLAIN" | "CALL" => {
                if self.last_upper_in(&["BEFORE", "AFTER", "INSTEAD", "OR"]) {
                    return false;
                }
                if matches!(upper, "DELETE" | "UPDATE") && self.last_upper_is("ON") {
                    return false;
                }
                if upper == "MERGE" {
                    self.in_merge_statement = true;
                }
                self.emit_statement_start(token, next);
            }
            "CREATE" => {
                self.create_depth += 1;
                self.emit_statement_start(token, next);
                self.clause = Clause::Create;
            }
            "ALTER" | "DROP" | "COMMENT" | "LISTEN" | "NOTIFY" | "LOCK" | "PREPARE" | "EXECUTE"
            | "DEALLOCATE" => self.emit_statement_start(token, next),
            "JOIN" => self.emit_join(token, next),
            "LEFT" | "RIGHT" | "FULL" | "INNER" | "OUTER" | "CROSS" | "NATURAL" => {
                if next.is_some_and(|next| next.is_word() && next.upper() == "JOIN")
                    || self.clause == Clause::From
                {
                    self.newline();
                    self.indent = self.block_indent + self.parens.len() + 1;
                }
                self.write_word(token, next);
            }
            "ON" if self.is_merge_context() => self.emit_clause_header(upper, Clause::Where),
            "ON" if self.clause == Clause::From => self.emit_join_on(token, next),
            "AND" | "OR" => self.emit_boolean(token, next, tokens, index),
            "BETWEEN" => {
                self.in_between = true;
                self.emit_atom(token, next);
            }
            "BEGIN" => self.emit_begin(token, next, embedded),
            "DECLARE" if embedded || self.in_function_context() => {
                self.emit_block_header(token, next, true);
            }
            "IF" if embedded || self.in_function_context() => {
                self.emit_block_header(token, next, false)
            }
            "LOOP" if embedded || self.in_function_context() => {
                self.write_word(token, next);
                self.newline();
                self.block_indent += 1;
                self.indent = self.block_indent;
            }
            "THEN" if self.in_expression_case() => self.emit_expression_case_then(token, next),
            "THEN" => self.emit_then(token, next),
            "EXCEPTION" if self.plpgsql_body => return false,
            "ELSE" if self.in_expression_case() => self.emit_expression_case_else(token, next),
            "ELSE" | "ELSIF" | "EXCEPTION" => self.emit_else_like(token, next),
            "END" if self.in_expression_case() => self.emit_expression_case_end(token, next),
            "END" => self.emit_end(token, next),
            "CASE" => {
                let case_indent = self.indent;
                self.emit_atom(token, next);
                self.expression_cases.push(case_indent);
            }
            "WHEN" if self.in_expression_case() => self.emit_expression_case_when(token, next),
            "WHEN" if self.is_merge_context() => self.emit_merge_when(),
            "WHEN" if self.plpgsql_body => return false,
            "WHEN" => {
                self.newline();
                self.indent = self.block_indent + self.parens.len();
                self.write_word(token, next);
            }
            "RAISE" | "RETURN" | "PERFORM" | "OPEN" | "CLOSE" | "MOVE"
                if embedded || self.in_function_context() =>
            {
                self.newline_if_needed();
                self.indent = self.block_indent;
                self.write_word(token, next);
            }
            _ => return false,
        }

        self.push_sig(token, token.text.clone());
        true
    }

    fn try_emit_plpgsql_keyword(
        &mut self,
        token: &Token,
        upper: &str,
        next: Option<&Token>,
    ) -> bool {
        match upper {
            "DECLARE" => self.emit_plpgsql_declare(token, next),
            "BEGIN" => self.emit_plpgsql_begin(token, next),
            "IF" => self.emit_plpgsql_control_start(token, next),
            "THEN" => self.emit_then(token, next),
            "ELSIF" => self.emit_plpgsql_elsif(token, next),
            "ELSE" => self.emit_plpgsql_else(token, next),
            "EXCEPTION" if !self.last_upper_is("RAISE") => self.emit_plpgsql_exception(token, next),
            "LOOP" => self.emit_plpgsql_loop(token, next),
            "WHILE" | "FOR" | "FOREACH" => self.emit_plpgsql_control_start(token, next),
            "CASE" => self.emit_plpgsql_case(token, next),
            "WHEN" if self.plpgsql_exception_section || self.plpgsql_case_depth > 0 => {
                self.emit_plpgsql_when(token, next)
            }
            "END" => self.emit_end(token, next),
            "RAISE" | "RETURN" | "PERFORM" | "OPEN" | "CLOSE" | "MOVE" | "FETCH" | "EXIT"
            | "CONTINUE" | "ASSERT" | "GET" | "EXECUTE" => {
                self.emit_plpgsql_statement_start(token, next)
            }
            _ => return false,
        }

        self.push_sig(token, token.text.clone());
        true
    }

    fn emit_clause_header(&mut self, upper: &str, clause: Clause) {
        self.newline_if_needed();
        self.indent = self.statement_indent() + self.clause_indent_extra;
        self.write_keyword_text(upper);
        self.clause = clause;
        self.list_items_on_line = 0;
        self.newline();
        self.indent = self.statement_indent() + self.clause_indent_extra + 1;
    }

    fn emit_plpgsql_nested_clause_header(&mut self, upper: &str, clause: Clause) {
        self.clause_indent_extra = 1;
        self.emit_clause_header(upper, clause);
    }

    fn emit_filter_where(&mut self, token: &Token, next: Option<&Token>) {
        self.newline_if_needed();
        self.indent = self.statement_indent();
        self.write_word(token, next);
        self.newline();
        self.indent = self.statement_indent() + 1;
    }

    fn emit_statement_start(&mut self, token: &Token, next: Option<&Token>) {
        if !self.output_trimmed_empty() && !self.last_output_is_newline() {
            self.newline();
        }
        self.indent = self.statement_indent();
        self.write_word(token, next);
        self.clause = Clause::Other;
        self.list_items_on_line = 0;
    }

    fn emit_join(&mut self, token: &Token, next: Option<&Token>) {
        if !self.last_upper_in(&[
            "LEFT", "RIGHT", "FULL", "INNER", "OUTER", "CROSS", "NATURAL",
        ]) {
            self.newline();
            self.indent = self.statement_indent() + 1;
        } else {
            self.space();
        }
        self.write_word(token, next);
        self.clause = Clause::From;
    }

    fn emit_join_on(&mut self, token: &Token, next: Option<&Token>) {
        self.newline();
        self.indent = self.statement_indent() + 2;
        self.write_word(token, next);
        self.clause = Clause::JoinOn;
    }

    fn emit_boolean(
        &mut self,
        token: &Token,
        next: Option<&Token>,
        tokens: &[Token],
        index: usize,
    ) {
        if token.upper() == "AND" && self.in_between {
            self.in_between = false;
            self.emit_atom(token, next);
            return;
        }
        if self.plpgsql_body && !matches!(self.clause, Clause::Where | Clause::Having) {
            if self.plpgsql_boolean_exceeds_wrap_limit(tokens, index) {
                self.newline();
                self.indent = self.block_indent + self.parens.len() + 1;
                self.write_word(token, next);
            } else {
                self.emit_atom(token, next);
            }
            return;
        }
        if token.upper() == "OR" && self.last_upper_is("CREATE") {
            self.emit_atom(token, next);
            return;
        }

        self.newline();
        self.indent = if self.clause == Clause::JoinOn {
            self.statement_indent() + 2
        } else {
            self.statement_indent() + 1
        };
        self.write_word(token, next);
    }

    fn emit_begin(&mut self, token: &Token, next: Option<&Token>, embedded: bool) {
        if next.is_some_and(|next| next.is_punctuation(';')) && !embedded {
            self.emit_statement_start(token, next);
            return;
        }

        self.emit_block_header(token, next, true);
    }

    fn emit_block_header(&mut self, token: &Token, next: Option<&Token>, newline_after: bool) {
        self.newline_if_needed();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        if newline_after {
            self.newline();
            self.block_indent += 1;
            self.indent = self.block_indent + self.parens.len();
        }
    }

    fn emit_plpgsql_declare(&mut self, token: &Token, next: Option<&Token>) {
        self.newline_if_needed();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
        self.plpgsql_declare_section = true;
    }

    fn emit_plpgsql_begin(&mut self, token: &Token, next: Option<&Token>) {
        if self.plpgsql_declare_section {
            self.block_indent = self.block_indent.saturating_sub(1);
            self.plpgsql_declare_section = false;
        }
        self.emit_block_header(token, next, true);
        self.plpgsql_exception_section = false;
    }

    fn emit_plpgsql_control_start(&mut self, token: &Token, next: Option<&Token>) {
        self.newline_if_needed();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
    }

    fn emit_plpgsql_loop(&mut self, token: &Token, next: Option<&Token>) {
        if matches!(
            self.clause,
            Clause::Select
                | Clause::From
                | Clause::Where
                | Clause::Group
                | Clause::Having
                | Clause::Order
                | Clause::Limit
        ) {
            self.newline();
            self.indent = self.block_indent + self.parens.len();
        } else {
            self.space();
        }
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
        self.clause = Clause::None;
        self.clause_indent_extra = 0;
    }

    fn emit_plpgsql_elsif(&mut self, token: &Token, next: Option<&Token>) {
        self.block_indent = self.block_indent.saturating_sub(1);
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
    }

    fn emit_plpgsql_else(&mut self, token: &Token, next: Option<&Token>) {
        self.block_indent = self.block_indent.saturating_sub(1);
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
    }

    fn emit_plpgsql_exception(&mut self, token: &Token, next: Option<&Token>) {
        self.block_indent = self.block_indent.saturating_sub(1);
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
        self.plpgsql_exception_section = true;
    }

    fn emit_plpgsql_case(&mut self, token: &Token, next: Option<&Token>) {
        self.newline_if_needed();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        self.block_indent += 1;
        self.plpgsql_case_depth += 1;
    }

    fn emit_plpgsql_when(&mut self, token: &Token, next: Option<&Token>) {
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
    }

    fn emit_plpgsql_statement_start(&mut self, token: &Token, next: Option<&Token>) {
        self.newline_if_needed();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
    }

    fn emit_then(&mut self, token: &Token, next: Option<&Token>) {
        self.space();
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
    }

    fn emit_expression_case_when(&mut self, token: &Token, next: Option<&Token>) {
        let case_indent = self.current_expression_case_indent();
        self.newline();
        self.indent = case_indent + 1;
        self.write_word(token, next);
    }

    fn emit_expression_case_then(&mut self, token: &Token, next: Option<&Token>) {
        let case_indent = self.current_expression_case_indent();
        self.space();
        self.write_word(token, next);
        self.newline();
        self.indent = case_indent + 2;
    }

    fn emit_expression_case_else(&mut self, token: &Token, next: Option<&Token>) {
        let case_indent = self.current_expression_case_indent();
        self.newline();
        self.indent = case_indent + 1;
        self.write_word(token, next);
        self.newline();
        self.indent = case_indent + 2;
    }

    fn emit_expression_case_end(&mut self, token: &Token, next: Option<&Token>) {
        let case_indent = self.expression_cases.pop().unwrap_or(self.indent);
        self.newline();
        self.indent = case_indent;
        self.write_word(token, next);
    }

    fn emit_else_like(&mut self, token: &Token, next: Option<&Token>) {
        self.block_indent = self.block_indent.saturating_sub(1);
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
        self.newline();
        self.block_indent += 1;
        self.indent = self.block_indent + self.parens.len();
    }

    fn emit_end(&mut self, token: &Token, next: Option<&Token>) {
        if self.plpgsql_body && self.plpgsql_exception_section {
            self.block_indent = self.block_indent.saturating_sub(2);
            self.plpgsql_exception_section = false;
        } else {
            self.block_indent = self.block_indent.saturating_sub(1);
        }
        self.newline();
        self.indent = self.block_indent + self.parens.len();
        self.write_word(token, next);
    }

    fn emit_merge_when(&mut self) {
        self.block_indent = self.block_indent.saturating_sub(1);
        self.newline();
        self.indent = self.block_indent + self.parens.len();
    }

    fn emit_semicolon(&mut self, embedded: bool) {
        self.trim_trailing_spaces();
        self.output.push(';');
        self.clause = Clause::None;
        self.clause_indent_extra = 0;
        self.list_items_on_line = 0;
        self.select_depth = self.select_depth.saturating_sub(1);
        self.create_depth = self.create_depth.saturating_sub(1);
        self.in_merge_statement = false;
        self.in_policy_statement = false;
        if embedded {
            self.newline();
            self.indent = self.block_indent + self.parens.len();
        } else {
            self.block_indent = 0;
            self.expression_cases.clear();
            self.blank_line();
            self.indent = self.block_indent + self.parens.len();
        }
    }

    fn emit_comma(&mut self) {
        self.trim_trailing_spaces();
        if self.options.comma_style == CommaStyle::Start {
            self.newline();
            self.write_indent();
            self.output.push(',');
            self.output.push(' ');
            return;
        }

        self.output.push(',');
        self.list_items_on_line += 1;
        if self.should_break_after_comma() {
            self.newline();
        } else {
            self.output.push(' ');
        }
    }

    fn should_break_after_comma(&self) -> bool {
        if self.options.comma_break {
            return true;
        }
        if let Some(wrap_after) = self.options.wrap_after
            && wrap_after > 0
            && self.list_items_on_line >= wrap_after
        {
            return true;
        }
        if self
            .delimiters
            .last()
            .is_some_and(|context| context.multiline)
        {
            return true;
        }
        matches!(
            self.clause,
            Clause::Select
                | Clause::Group
                | Clause::Order
                | Clause::Returning
                | Clause::Values
                | Clause::Set
        ) && self.parens.is_empty()
    }

    fn emit_open_paren(&mut self, tokens: &[Token], index: usize) {
        let next = tokens.get(index + 1);
        let next_starts_query = next.is_some_and(|next| {
            next.is_word()
                && matches!(
                    next.upper().as_str(),
                    "SELECT" | "WITH" | "VALUES" | "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                )
        });
        let ddl_list = self.starts_structural_ddl_list();
        let insert_list = self.starts_insert_structural_list();
        let policy_expression = self.is_policy_context() && self.last_upper_in(&["USING", "CHECK"]);
        let long_comma_group = self.paren_group_has_top_level_comma(tokens, index)
            && self.paren_group_exceeds_wrap_limit(tokens, index);
        let breakable_group = ddl_list || insert_list || policy_expression || long_comma_group;
        let filter_clause = self.last_upper_is("FILTER");
        let multiline = next_starts_query
            || filter_clause
            || (breakable_group && self.paren_group_needs_multiline(tokens, index));

        let previous_is_array_constructor = self.last_upper_is("ARRAY");
        let previous_is_callable = previous_is_array_constructor
            || (!ddl_list
                && !insert_list
                && !policy_expression
                && self.last_significant().is_some_and(|last| {
                    last.kind == TokenKind::Word
                        && (!keywords::is_keyword(&last.upper) || keywords::is_function(&last.text))
                }));
        if self.options.no_space_function || previous_is_callable {
            self.trim_trailing_spaces();
        } else {
            self.space();
        }
        self.write_indent();
        self.output.push('(');
        self.parens.push(ParenContext {
            indent: self.indent,
            multiline,
            insert_target_list: self.starts_insert_target_column_list(),
        });
        self.delimiters.push(DelimiterContext {
            kind: DelimiterKind::Paren,
            multiline,
        });
        if multiline {
            self.newline();
            self.indent += 1;
        }
    }

    fn paren_group_needs_multiline(&self, tokens: &[Token], index: usize) -> bool {
        let Some(close_index) = matching_close_paren(tokens, index) else {
            return true;
        };

        let mut depth = 0usize;
        for token in &tokens[index + 1..close_index] {
            match token.kind {
                TokenKind::Punctuation('(')
                | TokenKind::Punctuation('[')
                | TokenKind::Punctuation('{') => {
                    depth += 1;
                }
                TokenKind::Punctuation(')')
                | TokenKind::Punctuation(']')
                | TokenKind::Punctuation('}') => {
                    depth = depth.saturating_sub(1);
                }
                TokenKind::Punctuation(',') if depth == 0 => return true,
                TokenKind::LineComment | TokenKind::BlockComment | TokenKind::BlankLine => {
                    return true;
                }
                _ => {}
            }
        }

        let inline = format_inline_tokens(
            &tokens[index..=close_index],
            self.options,
            TokenCaseContext::Normal,
        );
        self.current_line_len() + self.leading_space_before_inline() + inline.chars().count()
            > self.effective_wrap_limit()
    }

    fn paren_group_has_top_level_comma(&self, tokens: &[Token], index: usize) -> bool {
        let Some(close_index) = matching_close_paren(tokens, index) else {
            return true;
        };

        let mut depth = 0usize;
        for token in &tokens[index + 1..close_index] {
            match token.kind {
                TokenKind::Punctuation('(')
                | TokenKind::Punctuation('[')
                | TokenKind::Punctuation('{') => depth += 1,
                TokenKind::Punctuation(')')
                | TokenKind::Punctuation(']')
                | TokenKind::Punctuation('}') => depth = depth.saturating_sub(1),
                TokenKind::Punctuation(',') if depth == 0 => return true,
                _ => {}
            }
        }

        false
    }

    fn paren_group_exceeds_wrap_limit(&self, tokens: &[Token], index: usize) -> bool {
        let Some(close_index) = matching_close_paren(tokens, index) else {
            return true;
        };

        let inline = format_inline_tokens(
            &tokens[index..=close_index],
            self.options,
            TokenCaseContext::Normal,
        );
        self.current_line_len() + self.leading_space_before_inline() + inline.chars().count()
            > self.effective_wrap_limit()
    }

    fn emit_close_paren(&mut self) {
        let context = self.parens.pop();
        self.pop_delimiter(DelimiterKind::Paren);
        self.trim_trailing_spaces();
        if context.as_ref().is_some_and(|ctx| ctx.multiline) && !self.last_output_is_newline() {
            self.newline();
        }
        if let Some(context) = context {
            self.indent = context.indent;
        }
        self.write_indent();
        self.output.push(')');
    }

    fn emit_dot(&mut self) {
        self.trim_trailing_spaces();
        self.output.push('.');
    }

    fn emit_open_bracket(&mut self, tokens: &[Token], index: usize) {
        let token = &tokens[index];
        let text = token.text.as_str();
        let close = match token.kind {
            TokenKind::Punctuation('[') => ']',
            TokenKind::Punctuation('{') => '}',
            _ => unreachable!("emit_open_bracket called for non-bracket"),
        };
        let multiline = token.is_punctuation('[')
            && self.last_upper_is("ARRAY")
            && self.bracket_group_needs_multiline(tokens, index, close);
        self.trim_trailing_spaces();
        self.write_indent();
        self.output.push_str(text);
        self.brackets.push(ParenContext {
            indent: self.indent,
            multiline,
            insert_target_list: false,
        });
        self.delimiters.push(DelimiterContext {
            kind: DelimiterKind::Bracket,
            multiline,
        });
        if multiline {
            self.newline();
            self.indent += 1;
        }
    }

    fn emit_close_bracket(&mut self, text: &str) {
        let context = self.brackets.pop();
        self.pop_delimiter(DelimiterKind::Bracket);
        self.trim_trailing_spaces();
        if context.as_ref().is_some_and(|ctx| ctx.multiline) && !self.last_output_is_newline() {
            self.newline();
        }
        if let Some(context) = context {
            self.indent = context.indent;
        }
        self.write_indent();
        self.output.push_str(text);
    }

    fn bracket_group_needs_multiline(&self, tokens: &[Token], index: usize, close: char) -> bool {
        let Some(close_index) = matching_close_delimiter(tokens, index, '[', close) else {
            return true;
        };

        if tokens[index + 1..close_index].iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::LineComment | TokenKind::BlockComment | TokenKind::BlankLine
            )
        }) {
            return true;
        }

        let inline = format_inline_tokens(
            &tokens[index..=close_index],
            self.options,
            TokenCaseContext::Normal,
        );
        self.current_line_len() + inline.chars().count() > self.effective_wrap_limit()
    }

    fn pop_delimiter(&mut self, kind: DelimiterKind) {
        if self
            .delimiters
            .last()
            .is_some_and(|context| context.kind == kind)
        {
            self.delimiters.pop();
        }
    }

    fn emit_operator(&mut self, token: &Token, next: Option<&Token>) {
        let op = token.text.as_str();
        match op {
            "<<" if self.plpgsql_body && (token.at_line_start || self.last_output_is_newline()) => {
                self.newline_if_needed();
                self.indent = self.block_indent + self.parens.len();
                self.write_indent();
                self.output.push_str("<<");
            }
            ">>" if self.plpgsql_body
                && self
                    .output
                    .rsplit('\n')
                    .next()
                    .is_some_and(|line| line.trim_start().starts_with("<<")) =>
            {
                self.trim_trailing_spaces();
                self.output.push_str(">>");
                self.newline();
            }
            "::" => {
                self.trim_trailing_spaces();
                self.output.push_str("::");
            }
            "%" if next.is_some_and(is_plpgsql_percent_type_word) => {
                self.trim_trailing_spaces();
                self.output.push('%');
            }
            "!" if self.is_unary_operator() => {
                self.trim_trailing_spaces();
                self.output.push('!');
            }
            "+" | "-" if self.is_unary_operator() => {
                self.output.push_str(op);
            }
            "=>" | ":=" => {
                self.space();
                self.output.push_str(op);
                self.output.push(' ');
            }
            _ => {
                self.space();
                self.output.push_str(op);
                self.output.push(' ');
            }
        }
    }

    fn emit_atom(&mut self, token: &Token, next: Option<&Token>) {
        self.space_if_needed_for_atom(token);
        if token.kind == TokenKind::Word {
            self.write_word(token, next);
        } else {
            self.write_indent();
            self.output.push_str(&token.text);
        }
    }

    fn emit_raw_atom(&mut self, text: &str) {
        self.space();
        self.output.push_str(text);
    }

    fn emit_comment(&mut self, token: &Token) {
        if self.options.remove_comments {
            return;
        }

        match token.kind {
            TokenKind::LineComment => {
                if !self.last_output_is_newline() {
                    self.space();
                }
                self.write_indent();
                self.output.push_str(token.text.trim());
                self.newline();
            }
            TokenKind::BlockComment => {
                self.newline_if_needed();
                self.write_indent();
                let indent = self.indent_string();
                let formatted = token
                    .text
                    .lines()
                    .enumerate()
                    .map(|(idx, line)| {
                        if idx == 0 {
                            line.trim_end().to_string()
                        } else {
                            format!("{indent}{}", line.trim())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.output.push_str(&formatted);
                self.newline();
            }
            _ => {}
        }
    }

    fn emit_meta_command(&mut self, text: &str) {
        self.newline_if_needed();
        self.output.push_str(text.trim_end());
        self.newline();
    }

    fn emit_dollar_string(&mut self, token: &Token) {
        if !self.should_format_dollar_body() {
            self.space();
            self.output.push_str(&token.text);
            return;
        }

        if let Some((delimiter, body)) = split_dollar_string(&token.text)
            && looks_like_embedded_code(body)
        {
            self.newline_if_needed();
            self.write_indent();
            self.output.push_str(delimiter);
            self.newline();

            let mut nested_options = self.options.clone();
            nested_options.no_extra_line = true;
            nested_options.insert_final_newline = true;
            let mut nested = Formatter::new(&nested_options);
            nested.plpgsql_body = true;
            nested.block_indent = self.block_indent + self.parens.len() + 1;
            nested.indent = nested.block_indent;
            nested.format_tokens(&tokenize(body.trim()), true);
            let nested_output = nested.finish();
            self.output.push_str(nested_output.trim_end());
            self.newline();
            self.write_indent();
            self.output.push_str(delimiter);
            return;
        }

        self.space();
        self.output.push_str(&token.text);
    }

    fn should_format_dollar_body(&self) -> bool {
        self.last_upper_in(&["AS", "DO"]) || self.in_function_context()
    }

    fn write_word(&mut self, token: &Token, next: Option<&Token>) {
        let upper = token.upper();
        let preserve_contextual_identifier = self.is_contextual_identifier(&upper, next)
            || (self.last_upper_is("AS")
                && !matches!(
                    upper.as_str(),
                    "SELECT"
                        | "WITH"
                        | "VALUES"
                        | "INSERT"
                        | "UPDATE"
                        | "DELETE"
                        | "MATERIALIZED"
                        | "NOT"
                        | "ON"
                        | "EXECUTE"
                ))
            || (upper == "KEY" && !self.last_upper_in(&["PRIMARY", "FOREIGN", "PARTITION", "NO"]))
            || (upper == "INDEX"
                && !self.last_upper_in(&[
                    "CREATE",
                    "DROP",
                    "REINDEX",
                    "UNIQUE",
                    "CONCURRENTLY",
                    "USING",
                ]))
            || (upper == "DOMAIN" && !self.last_upper_in(&["CREATE", "ALTER", "DROP"]))
            || self.is_create_table_column_name(&upper)
            || self.is_insert_target_column_name();
        let word = if self.is_insert_target_column_name()
            || self.is_values_identifier_list_item(&upper, next)
        {
            token.text.to_ascii_lowercase()
        } else if preserve_contextual_identifier {
            token.text.clone()
        } else if next.is_some_and(|next| next.is_punctuation('('))
            && keywords::is_function(&token.lower())
        {
            self.options.function_case.apply(&token.text)
        } else if keywords::is_type(&upper) {
            self.options.type_case.apply(&token.text)
        } else if keywords::is_keyword(&upper) {
            self.options.keyword_case.apply(&token.text)
        } else {
            token.text.clone()
        };
        self.write_indent();
        self.output.push_str(&word);
    }

    fn write_keyword_text(&mut self, upper: &str) {
        self.write_indent();
        self.output
            .push_str(&self.options.keyword_case.apply(upper));
    }

    fn space_if_needed_for_atom(&mut self, token: &Token) {
        if token.kind == TokenKind::Word
            && self
                .last_significant()
                .is_some_and(|last| last.kind == TokenKind::Punctuation('.') || last.text == "<<")
        {
            self.trim_trailing_spaces();
            return;
        }
        if self
            .last_significant()
            .is_some_and(|last| last.text == "::")
        {
            self.trim_trailing_spaces();
            return;
        }
        if token.kind == TokenKind::Word
            && is_plpgsql_percent_type_word(token)
            && self.last_significant().is_some_and(|last| last.text == "%")
        {
            self.trim_trailing_spaces();
            return;
        }
        if token.kind == TokenKind::Number
            && (self.output.ends_with('-') || self.output.ends_with('+'))
        {
            return;
        }
        self.space();
    }

    fn space(&mut self) {
        if self.output.is_empty() || self.last_output_is_newline() {
            return;
        }
        if let Some(ch) = self.output.chars().last()
            && !ch.is_whitespace()
            && ch != '('
            && ch != '['
            && ch != '{'
            && ch != '.'
        {
            self.output.push(' ');
        }
    }

    fn newline_if_needed(&mut self) {
        if !self.output_trimmed_empty() && !self.last_output_is_newline() {
            self.newline();
        }
    }

    fn newline(&mut self) {
        self.trim_trailing_spaces();
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn blank_line(&mut self) {
        self.trim_trailing_spaces();
        if self.output.is_empty() {
            return;
        }
        if self.options.no_extra_line {
            if !self.output.ends_with('\n') {
                self.output.push('\n');
            }
            return;
        }
        while self.output.ends_with("\n\n\n") {
            self.output.pop();
        }
        if self.output.ends_with("\n\n") {
            return;
        }
        if self.output.ends_with('\n') {
            self.output.push('\n');
        } else {
            self.output.push_str("\n\n");
        }
    }

    fn write_indent(&mut self) {
        if self.output.is_empty() || self.last_output_is_newline() {
            self.output.push_str(&self.indent_string());
        }
    }

    fn indent_string(&self) -> String {
        self.indent_unit.repeat(self.indent)
    }

    fn trim_trailing_spaces(&mut self) {
        while self.output.ends_with(' ') || self.output.ends_with('\t') {
            self.output.pop();
        }
    }

    fn last_output_is_newline(&self) -> bool {
        self.output.ends_with('\n')
    }

    fn output_trimmed_empty(&self) -> bool {
        self.output.trim().is_empty()
    }

    fn finish(&mut self) -> String {
        self.trim_trailing_spaces();
        if self.options.no_extra_line {
            if !self.output.ends_with('\n') {
                self.output.push('\n');
            }
        } else if !self.output.ends_with("\n\n") {
            if !self.output.ends_with('\n') {
                self.output.push('\n');
            }
            self.output.push('\n');
        }

        if !self.options.redundant_parenthesis {
            remove_redundant_parentheses(&self.output)
        } else {
            self.output.clone()
        }
    }

    fn push_sig(&mut self, token: &Token, text: String) {
        if matches!(
            token.kind,
            TokenKind::BlankLine | TokenKind::LineComment | TokenKind::BlockComment
        ) {
            return;
        }
        self.significant.push(SignificantToken {
            kind: token.kind,
            upper: text.to_ascii_uppercase(),
            text,
        });
        if self.significant.len() > 12 {
            self.significant.remove(0);
        }
    }

    fn last_significant(&self) -> Option<&SignificantToken> {
        self.significant.last()
    }

    fn last_upper_is(&self, word: &str) -> bool {
        self.last_significant()
            .is_some_and(|last| last.upper == word)
    }

    fn last_upper_in(&self, words: &[&str]) -> bool {
        self.last_significant()
            .is_some_and(|last| words.iter().any(|word| last.upper == *word))
    }

    fn recent_upper_contains(&self, words: &[&str], distance: usize) -> bool {
        self.significant
            .iter()
            .rev()
            .take(distance)
            .any(|token| words.iter().any(|word| token.upper == *word))
    }

    fn starts_structural_ddl_list(&self) -> bool {
        if self.create_depth == 0 {
            return false;
        }

        if self.last_upper_in(&["ENUM", "KEY", "UNIQUE", "EXCLUDE", "FOREIGN"]) {
            return true;
        }

        self.parens.is_empty()
            && self.recent_upper_contains(&["TABLE", "TYPE"], 8)
            && !self.last_upper_in(&["RANGE", "LIST", "HASH", "WITH", "USING", "AS"])
            && !self.recent_upper_contains(&["PARTITION", "BY"], 3)
    }

    fn starts_insert_structural_list(&self) -> bool {
        if self.last_upper_is("VALUES") {
            return true;
        }

        if self.clause == Clause::Values
            && self.parens.is_empty()
            && self
                .last_significant()
                .is_some_and(|last| last.kind == TokenKind::Punctuation(','))
        {
            return true;
        }

        self.parens.is_empty() && self.recent_upper_contains(&["INSERT"], 6)
    }

    fn starts_insert_target_column_list(&self) -> bool {
        self.parens.is_empty()
            && self.recent_upper_contains(&["INSERT"], 6)
            && self.recent_upper_contains(&["INTO"], 5)
            && !self.recent_upper_contains(&["VALUES", "SELECT"], 5)
    }

    fn statement_indent(&self) -> usize {
        self.parens
            .last()
            .filter(|context| context.multiline)
            .map_or(self.block_indent + self.parens.len(), |context| {
                context.indent + 1
            })
    }

    fn effective_wrap_limit(&self) -> usize {
        self.options.wrap_limit.unwrap_or(100)
    }

    fn current_line_len(&self) -> usize {
        self.output
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count())
    }

    fn leading_space_before_inline(&self) -> usize {
        usize::from(
            !self.output.is_empty()
                && !self.last_output_is_newline()
                && !self
                    .output
                    .chars()
                    .last()
                    .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '.')),
        )
    }

    fn inline_tokens_exceed_wrap_limit(&self, tokens: &[Token]) -> bool {
        let inline = format_inline_tokens(tokens, self.options, TokenCaseContext::Normal);
        self.current_line_len() + self.leading_space_before_inline() + inline.chars().count()
            > self.effective_wrap_limit()
    }

    fn plpgsql_boolean_exceeds_wrap_limit(&self, tokens: &[Token], index: usize) -> bool {
        let end = find_plpgsql_boolean_segment_end(tokens, index + 1);
        self.inline_tokens_exceed_wrap_limit(&tokens[index..end])
    }

    fn is_create_table_column_name(&self, upper: &str) -> bool {
        if self.create_depth == 0 {
            return false;
        }
        if matches!(
            upper,
            "CONSTRAINT" | "PRIMARY" | "FOREIGN" | "UNIQUE" | "CHECK" | "EXCLUDE" | "LIKE"
        ) {
            return false;
        }
        if !self.parens.last().is_some_and(|context| context.multiline) {
            return false;
        }
        self.last_significant().is_some_and(|last| {
            matches!(
                last.kind,
                TokenKind::Punctuation('(') | TokenKind::Punctuation(',')
            )
        })
    }

    fn is_insert_target_column_name(&self) -> bool {
        self.parens
            .last()
            .is_some_and(|context| context.insert_target_list)
    }

    fn is_distinct_from_operator(&self, upper: &str) -> bool {
        upper == "FROM" && self.last_upper_is("DISTINCT")
    }

    fn is_values_identifier_list_item(&self, upper: &str, next: Option<&Token>) -> bool {
        if upper != "VALUES" || !matches!(self.clause, Clause::Select | Clause::Returning) {
            return false;
        }

        let starts_list_item = self.last_significant().is_some_and(|last| {
            last.kind == TokenKind::Punctuation(',')
                || matches!(last.upper.as_str(), "SELECT" | "RETURNING")
        });
        let ends_list_item = next.is_none_or(|next| {
            next.is_punctuation(',')
                || (next.is_word() && matches!(next.upper().as_str(), "FROM" | "AS"))
        });

        starts_list_item && ends_list_item
    }

    fn is_contextual_identifier(&self, upper: &str, next: Option<&Token>) -> bool {
        self.last_significant()
            .is_some_and(|last| last.kind == TokenKind::Punctuation('.'))
            || (upper != "ARRAY"
                && !keywords::is_type(upper)
                && next.is_some_and(|next| next.is_punctuation('[')))
    }

    fn is_filter_where_context(&self) -> bool {
        let mut significant = self.significant.iter().rev();
        significant
            .next()
            .is_some_and(|token| token.kind == TokenKind::Punctuation('('))
            && significant
                .next()
                .is_some_and(|token| token.upper == "FILTER")
    }

    fn in_expression_case(&self) -> bool {
        !self.expression_cases.is_empty()
    }

    fn current_expression_case_indent(&self) -> usize {
        self.expression_cases.last().copied().unwrap_or(self.indent)
    }

    fn is_statement_boundary(&self) -> bool {
        self.output_trimmed_empty()
            || self
                .last_significant()
                .is_some_and(|last| last.kind == TokenKind::Punctuation(';'))
    }

    fn is_dml_statement(&self) -> bool {
        self.create_depth == 0
            && self
                .significant
                .iter()
                .rev()
                .take_while(|token| token.kind != TokenKind::Punctuation(';'))
                .any(|token| {
                    matches!(
                        token.upper.as_str(),
                        "UPDATE" | "INSERT" | "DELETE" | "MERGE"
                    )
                })
    }

    fn is_referential_action_context(&self) -> bool {
        self.last_upper_in(&["DELETE", "UPDATE"])
            && self
                .significant
                .iter()
                .rev()
                .take(3)
                .any(|token| token.upper == "ON")
    }

    fn is_merge_context(&self) -> bool {
        self.in_merge_statement
            || self
                .significant
                .iter()
                .rev()
                .any(|token| token.upper == "MERGE")
    }

    fn is_policy_context(&self) -> bool {
        self.in_policy_statement
            || self
                .significant
                .iter()
                .rev()
                .take_while(|token| token.kind != TokenKind::Punctuation(';'))
                .any(|token| token.upper == "POLICY")
    }

    fn is_inside_over_clause(&self) -> bool {
        self.significant
            .iter()
            .rev()
            .take(5)
            .any(|token| token.upper == "OVER")
    }

    fn in_function_context(&self) -> bool {
        self.significant.iter().rev().any(|token| {
            matches!(
                token.upper.as_str(),
                "FUNCTION" | "PROCEDURE" | "PLPGSQL" | "DECLARE" | "BEGIN"
            )
        })
    }

    fn is_unary_operator(&self) -> bool {
        self.last_significant().is_none_or(|last| {
            last.kind == TokenKind::Operator
                || matches!(
                    last.kind,
                    TokenKind::Punctuation('(')
                        | TokenKind::Punctuation('[')
                        | TokenKind::Punctuation('{')
                        | TokenKind::Punctuation(',')
                        | TokenKind::Punctuation(';')
                )
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum TokenCaseContext {
    Normal,
    ObjectScope,
    GrantPrivilege,
    GrantIdentifier,
    GrantRecipient,
}

fn find_top_level_word(tokens: &[Token], words: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().skip(start) {
        match token.kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => {
                depth += 1;
            }
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Word if depth == 0 => {
                let upper = token.upper();
                if words.iter().any(|word| upper == *word) {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn create_routine_clause_start(tokens: &[Token]) -> Option<usize> {
    if !tokens
        .first()
        .is_some_and(|token| token.is_word() && token.upper() == "CREATE")
    {
        return None;
    }

    let mut routine_index = 1usize;
    if tokens
        .get(routine_index)
        .is_some_and(|token| token.is_word() && token.upper() == "OR")
        && tokens
            .get(routine_index + 1)
            .is_some_and(|token| token.is_word() && token.upper() == "REPLACE")
    {
        routine_index += 2;
    }

    if !tokens.get(routine_index).is_some_and(|token| {
        token.is_word() && matches!(token.upper().as_str(), "FUNCTION" | "PROCEDURE")
    }) {
        return None;
    }

    let clause_start = find_next_routine_clause(tokens, routine_index + 1, tokens.len());
    (clause_start < tokens.len()).then_some(clause_start)
}

fn find_next_routine_clause(tokens: &[Token], start: usize, end: usize) -> usize {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().take(end).skip(start) {
        match token.kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => depth += 1,
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 && is_routine_clause_start(tokens, idx) => return idx,
            _ => {}
        }
    }
    end
}

fn find_plpgsql_boolean_segment_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().skip(start) {
        match token.kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => depth += 1,
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => depth = depth.saturating_sub(1),
            TokenKind::Punctuation(';') if depth == 0 => return idx,
            TokenKind::Word if depth == 0 => {
                let upper = token.upper();
                if matches!(
                    upper.as_str(),
                    "THEN" | "LOOP" | "ELSE" | "ELSIF" | "WHEN" | "EXCEPTION"
                ) {
                    return idx;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

fn is_routine_clause_start(tokens: &[Token], idx: usize) -> bool {
    let token = &tokens[idx];
    if !token.is_word() {
        return false;
    }

    let upper = token.upper();
    match upper.as_str() {
        "RETURNS" | "LANGUAGE" | "TRANSFORM" | "WINDOW" | "IMMUTABLE" | "STABLE" | "VOLATILE"
        | "LEAKPROOF" | "STRICT" | "SECURITY" | "PARALLEL" | "COST" | "ROWS" | "SUPPORT"
        | "SET" | "AS" => true,
        "CALLED" => tokens
            .get(idx + 1)
            .is_some_and(|token| token.is_word() && token.upper() == "ON"),
        "EXTERNAL" => tokens
            .get(idx + 1)
            .is_some_and(|token| token.is_word() && token.upper() == "SECURITY"),
        "NOT" => tokens
            .get(idx + 1)
            .is_some_and(|token| token.is_word() && token.upper() == "LEAKPROOF"),
        _ => false,
    }
}

fn create_trigger_name_index(tokens: &[Token]) -> Option<(usize, usize)> {
    if !tokens
        .first()
        .is_some_and(|token| token.is_word() && token.upper() == "CREATE")
    {
        return None;
    }

    let mut trigger_index = 1usize;
    if tokens
        .get(trigger_index)
        .is_some_and(|token| token.is_word() && token.upper() == "OR")
        && tokens
            .get(trigger_index + 1)
            .is_some_and(|token| token.is_word() && token.upper() == "REPLACE")
    {
        trigger_index += 2;
    }
    if tokens
        .get(trigger_index)
        .is_some_and(|token| token.is_word() && token.upper() == "CONSTRAINT")
    {
        trigger_index += 1;
    }
    if !tokens
        .get(trigger_index)
        .is_some_and(|token| token.is_word() && token.upper() == "TRIGGER")
    {
        return None;
    }

    let name_index = trigger_index + 1;
    tokens.get(name_index)?;
    Some((trigger_index, name_index))
}

fn find_trigger_update_of(tokens: &[Token]) -> Option<usize> {
    let mut depth = 0usize;
    for idx in 0..tokens.len().saturating_sub(1) {
        match tokens[idx].kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => depth += 1,
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => depth = depth.saturating_sub(1),
            TokenKind::Word
                if depth == 0
                    && tokens[idx].upper() == "UPDATE"
                    && tokens
                        .get(idx + 1)
                        .is_some_and(|token| token.is_word() && token.upper() == "OF") =>
            {
                return Some(idx + 1);
            }
            _ => {}
        }
    }
    None
}

fn find_next_trigger_clause(tokens: &[Token], start: usize, end: usize) -> usize {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().take(end).skip(start) {
        match token.kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => depth += 1,
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 => {
                let upper = token.upper();
                if matches!(
                    upper.as_str(),
                    "FROM"
                        | "REFERENCING"
                        | "FOR"
                        | "WHEN"
                        | "INITIALLY"
                        | "DEFERRABLE"
                        | "EXECUTE"
                ) || (upper == "NOT"
                    && tokens
                        .get(idx + 1)
                        .is_some_and(|token| token.is_word() && token.upper() == "DEFERRABLE"))
                {
                    return idx;
                }
            }
            _ => {}
        }
    }
    end
}

fn matching_close_paren(tokens: &[Token], open_index: usize) -> Option<usize> {
    matching_close_delimiter(tokens, open_index, '(', ')')
}

fn matching_close_delimiter(
    tokens: &[Token],
    open_index: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().skip(open_index) {
        match token.kind {
            TokenKind::Punctuation(ch) if ch == open => {
                depth += 1;
            }
            TokenKind::Punctuation(ch) if ch == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (idx, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => {
                depth += 1;
            }
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Punctuation(',') if depth == 0 => {
                parts.push(&tokens[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }

    parts.push(&tokens[start..]);
    parts
}

fn split_grant_object_scope(tokens: &[Token]) -> (&[Token], &[Token]) {
    if tokens.is_empty() {
        return (&[], &[]);
    }

    let upper = tokens[0].upper();
    if upper == "ALL" {
        if let Some(schema_idx) = tokens
            .iter()
            .position(|token| token.is_word() && token.upper() == "SCHEMA")
        {
            return (&tokens[..=schema_idx], &tokens[schema_idx + 1..]);
        }
        return (tokens, &[]);
    }

    let scope_len = match upper.as_str() {
        "FOREIGN"
            if tokens
                .get(1)
                .is_some_and(|token| token.is_word() && token.upper() == "DATA")
                && tokens
                    .get(2)
                    .is_some_and(|token| token.is_word() && token.upper() == "WRAPPER") =>
        {
            3
        }
        "FOREIGN"
            if tokens
                .get(1)
                .is_some_and(|token| token.is_word() && token.upper() == "SERVER") =>
        {
            2
        }
        "LARGE"
            if tokens
                .get(1)
                .is_some_and(|token| token.is_word() && token.upper() == "OBJECT") =>
        {
            2
        }
        "TABLE" | "TABLES" | "SEQUENCE" | "SEQUENCES" | "SCHEMA" | "DATABASE" | "DOMAIN"
        | "FUNCTION" | "FUNCTIONS" | "PROCEDURE" | "PROCEDURES" | "ROUTINE" | "ROUTINES"
        | "LANGUAGE" | "TYPE" | "TYPES" | "TABLESPACE" => 1,
        _ => 0,
    };

    if scope_len == 0 {
        (&[], tokens)
    } else {
        (&tokens[..scope_len], &tokens[scope_len..])
    }
}

fn split_grant_recipient_tail(tokens: &[Token], revoke: bool) -> (&[Token], &[Token]) {
    if tokens.is_empty() {
        return (&[], &[]);
    }

    if revoke
        && tokens.last().is_some_and(|token| {
            token.is_word() && matches!(token.upper().as_str(), "CASCADE" | "RESTRICT")
        })
    {
        return (&tokens[..tokens.len() - 1], &tokens[tokens.len() - 1..]);
    }

    if let Some(with_idx) = find_top_level_word(tokens, &["WITH", "GRANTED"], 0) {
        return (&tokens[..with_idx], &tokens[with_idx..]);
    }

    (tokens, &[])
}

fn format_inline_tokens(
    tokens: &[Token],
    options: &FormatOptions,
    context: TokenCaseContext,
) -> String {
    let mut out = String::new();

    for (idx, token) in tokens.iter().enumerate() {
        let next = tokens.get(idx + 1);
        match token.kind {
            TokenKind::Word => {
                inline_space(&mut out, token, tokens.get(idx.wrapping_sub(1)));
                out.push_str(&format_inline_word(token, next, options, context));
            }
            TokenKind::Number | TokenKind::String | TokenKind::QuotedIdentifier => {
                inline_space(&mut out, token, tokens.get(idx.wrapping_sub(1)));
                out.push_str(&token.text);
            }
            TokenKind::Punctuation(',') => {
                trim_inline_spaces(&mut out);
                out.push_str(", ");
            }
            TokenKind::Punctuation('.') => {
                trim_inline_spaces(&mut out);
                out.push('.');
            }
            TokenKind::Punctuation('(')
            | TokenKind::Punctuation('[')
            | TokenKind::Punctuation('{') => {
                trim_inline_spaces(&mut out);
                out.push_str(&token.text);
            }
            TokenKind::Punctuation(')')
            | TokenKind::Punctuation(']')
            | TokenKind::Punctuation('}') => {
                trim_inline_spaces(&mut out);
                out.push_str(&token.text);
            }
            TokenKind::Punctuation(_) => {
                inline_space(&mut out, token, tokens.get(idx.wrapping_sub(1)));
                out.push_str(&token.text);
            }
            TokenKind::Operator if token.text == "::" => {
                trim_inline_spaces(&mut out);
                out.push_str("::");
            }
            TokenKind::Operator
                if token.text == "%" && next.is_some_and(is_plpgsql_percent_type_word) =>
            {
                trim_inline_spaces(&mut out);
                out.push('%');
            }
            TokenKind::Operator => {
                inline_space(&mut out, token, tokens.get(idx.wrapping_sub(1)));
                out.push_str(&token.text);
                out.push(' ');
            }
            TokenKind::Other => {
                inline_space(&mut out, token, tokens.get(idx.wrapping_sub(1)));
                out.push_str(&token.text);
            }
            TokenKind::DollarString
            | TokenKind::LineComment
            | TokenKind::BlockComment
            | TokenKind::MetaCommand
            | TokenKind::BlankLine => {}
        }
    }

    trim_inline_spaces(&mut out);
    out
}

fn format_inline_word(
    token: &Token,
    next: Option<&Token>,
    options: &FormatOptions,
    context: TokenCaseContext,
) -> String {
    let upper = token.upper();
    match context {
        TokenCaseContext::ObjectScope | TokenCaseContext::GrantPrivilege => {
            options.keyword_case.apply(&token.text)
        }
        TokenCaseContext::GrantIdentifier | TokenCaseContext::GrantRecipient => {
            if upper == "PUBLIC" {
                if matches!(context, TokenCaseContext::GrantRecipient) {
                    options.keyword_case.apply(&token.text)
                } else {
                    token.text.clone()
                }
            } else if is_probable_function_signature_type(token, next) || keywords::is_type(&upper)
            {
                options.type_case.apply(&token.text)
            } else {
                token.text.clone()
            }
        }
        TokenCaseContext::Normal => {
            if next.is_some_and(|next| next.is_punctuation('('))
                && keywords::is_function(&token.lower())
            {
                options.function_case.apply(&token.text)
            } else if keywords::is_type(&upper) {
                options.type_case.apply(&token.text)
            } else if keywords::is_keyword(&upper) {
                options.keyword_case.apply(&token.text)
            } else {
                token.text.clone()
            }
        }
    }
}

fn is_probable_function_signature_type(token: &Token, next: Option<&Token>) -> bool {
    keywords::is_type(&token.upper()) && !next.is_some_and(|next| next.is_punctuation('('))
}

fn is_plpgsql_percent_type_word(token: &Token) -> bool {
    token.is_word() && matches!(token.upper().as_str(), "TYPE" | "ROWTYPE")
}

fn inline_space(out: &mut String, token: &Token, previous: Option<&Token>) {
    if out.is_empty() {
        return;
    }
    if previous.is_some_and(|previous| {
        previous.kind == TokenKind::Punctuation('.')
            || previous.text == "::"
            || matches!(
                previous.kind,
                TokenKind::Punctuation('(')
                    | TokenKind::Punctuation('[')
                    | TokenKind::Punctuation('{')
            )
    }) {
        trim_inline_spaces(out);
        return;
    }
    if token.kind == TokenKind::Word
        && is_plpgsql_percent_type_word(token)
        && previous.is_some_and(|previous| previous.text == "%")
    {
        trim_inline_spaces(out);
        return;
    }
    if token.kind == TokenKind::Number && (out.ends_with('-') || out.ends_with('+')) {
        return;
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
}

fn trim_inline_spaces(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

fn split_dollar_string(text: &str) -> Option<(&str, &str)> {
    if !text.starts_with('$') {
        return None;
    }
    let end = text[1..].find('$')? + 2;
    let delimiter = &text[..end];
    if !text.ends_with(delimiter) || text.len() < delimiter.len() * 2 {
        return None;
    }
    let body = &text[delimiter.len()..text.len() - delimiter.len()];
    Some((delimiter, body))
}

fn looks_like_embedded_code(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "begin", "declare", "select", "insert", "update", "delete", "if", "loop", "return", "raise",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn anonymize_sql(sql: &str) -> String {
    tokenize(sql)
        .into_iter()
        .map(|token| match token.kind {
            TokenKind::String => "'x'".to_string(),
            TokenKind::Number => "0".to_string(),
            _ => token.text,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn remove_redundant_parentheses(sql: &str) -> String {
    static RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?i)\b(WHERE|FROM|SELECT)\s+\(\(([^()\n]+)\)\)").unwrap()
    });
    RE.replace_all(sql, "$1 ($2)").to_string()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::config::{CaseStyle, FormatOptions};

    #[test]
    fn formats_basic_select() {
        let sql = "select a,b,c from tablea join tableb on ( tablea.a=tableb.a) where tablea.x = 1 and tableb.y=1 group by tablea.a, tableb.y order by tablea.a;";
        let formatted = format_sql(sql);
        assert_eq!(
            formatted,
            "SELECT\n  a,\n  b,\n  c\nFROM\n  tablea\n  JOIN tableb\n    ON (tablea.a = tableb.a)\nWHERE\n  tablea.x = 1\n  AND tableb.y = 1\nGROUP BY\n  tablea.a,\n  tableb.y\nORDER BY\n  tablea.a;\n"
        );
    }

    #[test]
    fn formats_join_conditions_on_continuation_lines() {
        let sql = "with args as (select @category_code::text as category_code, @material_type_code::text as material_type_code) select mt.id from material_types mt join material_categories mc on mc.id=mt.category_id and mc.delete_time is null cross join args where mc.code=args.category_code and mt.code=args.material_type_code;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "FROM\n  material_types mt\n  JOIN material_categories mc\n    ON mc.id = mt.category_id\n    AND mc.delete_time IS NULL\n  CROSS JOIN args"
        ));
        assert!(formatted.contains(
            "WHERE\n  mc.code = args.category_code\n  AND mt.code = args.material_type_code;"
        ));
    }

    #[test]
    fn preserves_sqlc_parameters_and_cast_spacing() {
        let sql =
            "select @code::text, sqlc.arg(user_id)::uuid from styles where code=@code for update;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("@code::text"));
        assert!(formatted.contains("sqlc.arg(user_id)::uuid"));
        assert!(formatted.contains("FOR UPDATE;"));
        assert!(!formatted.contains("@ code"));
        assert!(!formatted.contains(":: text"));
    }

    #[test]
    fn formats_create_table_column_lists() {
        let sql = "create table uploads (id uuid primary key default gen_random_uuid(), key text not null, etag text not null default gen_random_uuid()::text, constraint uploads_key_uniq unique (key));";
        let formatted = format_sql(sql);
        assert!(formatted.contains("CREATE TABLE uploads (\n"));
        assert!(formatted.contains("key text NOT NULL"));
        assert!(formatted.contains("gen_random_uuid()::text"));
        assert!(formatted.contains("CONSTRAINT uploads_key_uniq UNIQUE (key)"));
        assert!(!formatted.contains("UNIQUE (\n    key\n  )"));
    }

    #[test]
    fn keeps_values_functions_and_foreign_key_actions_inline() {
        let sql = "insert into audit(actor_id) values(current_actor()); create table child(parent_id uuid references parent(id) on delete cascade, user_id uuid references users(id) on delete set null);";
        let formatted = format_sql(sql);
        assert!(formatted.contains("current_actor()"));
        assert!(formatted.contains("VALUES\n  (current_actor())"));
        assert!(formatted.contains("ON DELETE CASCADE"));
        assert!(formatted.contains("ON DELETE SET NULL"));
        assert!(!formatted.contains("current_actor (\n"));
        assert!(!formatted.contains("ON\n  DELETE"));
    }

    #[test]
    fn preserves_contextual_identifier_keywords() {
        let sql = "create table t (index int, domain text, values jsonb, constraint domain_chk check (domain = lower(domain))); select index, domain from t order by index;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("index int"));
        assert!(formatted.contains("domain text"));
        assert!(formatted.contains("values jsonb"));
        assert!(formatted.contains("lower(domain)"));
        assert!(formatted.contains("ORDER BY\n  index;"));
        assert!(!formatted.contains("INDEX integer"));
        assert!(!formatted.contains("VALUES\n    jsonb"));
        assert!(!formatted.contains("lower(DOMAIN)"));
    }

    #[test]
    fn indents_nested_subqueries_relative_to_parentheses() {
        let sql = "delete from styles s where exists (select 1 from organization_settings os where os.id=s.id) and not exists (select 1 from samples where samples.style_id=s.id);";
        let formatted = format_sql(sql);
        assert!(formatted.contains("EXISTS (\n    SELECT\n      1"));
        assert!(formatted.contains("    FROM\n      organization_settings os"));
        assert!(formatted.contains("    WHERE\n      os.id = s.id"));
    }

    #[test]
    fn formats_lateral_filter_and_case_expressions() {
        let sql = "select s.id, stats.active_style_count, stats.latest_style_update_time from seasons s cross join lateral (select count(*) filter (where st.delete_time is null and st.archive_time is null)::int as active_style_count, case when count(st.id)=0 then '1970-01-01 00:00:00+00'::timestamptz else max(st.update_time)::timestamptz end as latest_style_update_time from styles st where st.season_id=s.id) stats where s.id=@season_uuid;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("CROSS JOIN LATERAL (\n    SELECT"));
        assert!(formatted.contains(
            "count(*) FILTER (\n        WHERE\n          st.delete_time IS NULL\n          AND st.archive_time IS NULL\n      )::int AS active_style_count"
        ));
        assert!(formatted.contains(
            "CASE\n        WHEN count(st.id) = 0 THEN\n          '1970-01-01 00:00:00+00'::timestamptz\n        ELSE\n          max(st.update_time)::timestamptz\n      END AS latest_style_update_time"
        ));
        assert!(formatted.contains("\nWHERE\n  s.id = @season_uuid;"));
    }

    #[test]
    fn formats_grant_lists() {
        let sql = "grant select, insert, update, delete on uploads, seasons, chat_messages to svc_server, svc_sweeper; grant sloper_owner, svc_server to postgres;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "GRANT\n  SELECT,\n  INSERT,\n  UPDATE,\n  DELETE\nON\n  uploads,\n  seasons,\n  chat_messages\nTO\n  svc_server,\n  svc_sweeper;"
        ));
        assert!(formatted.contains("GRANT\n  sloper_owner,\n  svc_server\nTO\n  postgres;"));
    }

    #[test]
    fn formats_revoke_function_lists() {
        let sql = "revoke execute on function authz_bump_version(text,text), authz_sync_member_roles(text,text), seed_organization_roles() from public;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "REVOKE\n  EXECUTE\nON FUNCTION\n  authz_bump_version(text, text),\n  authz_sync_member_roles(text, text),\n  seed_organization_roles()\nFROM\n  PUBLIC;"
        ));
    }

    #[test]
    fn keeps_trigger_events_inline() {
        let sql = "create trigger seasons_set_update_time before update on seasons for each row execute function set_update_time();";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "CREATE TRIGGER seasons_set_update_time\nBEFORE UPDATE\nON seasons\nFOR EACH ROW\nEXECUTE FUNCTION set_update_time();"
        ));
        assert!(!formatted.contains("BEFORE\nUPDATE"));
    }

    #[test]
    fn formats_trigger_update_of_columns() {
        let sql = "create trigger auth_organization_member_audit_update after update of role, organization_id, user_id on auth_organization_member for each row execute function audit_member_role();";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "AFTER UPDATE OF\n  role,\n  organization_id,\n  user_id\nON auth_organization_member"
        ));
        assert!(formatted.contains("EXECUTE FUNCTION audit_member_role();"));
    }

    #[test]
    fn formats_policy_using_and_check_clauses() {
        let sql = "create policy uploads_tenant on uploads using (organization_id=current_organization()) with check (organization_id=current_organization());";
        let formatted = format_sql(sql);
        assert!(formatted.contains("USING\n  (organization_id = current_organization())"));
        assert!(formatted.contains("WITH CHECK\n  (organization_id = current_organization())"));
    }

    #[test]
    fn supports_postgres_18_returning_old_new_aliases() {
        let sql = "update accounts set balance=balance+1 returning old.balance as before,new.balance as after;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("RETURNING\n  OLD.balance AS before,"));
        assert!(formatted.contains("NEW.balance AS after;"));
    }

    #[test]
    fn formats_temporal_constraints() {
        let sql = "create table booking(room_id int, valid_at tstzrange, primary key(room_id, valid_at without overlaps), foreign key(valid_at period) references rooms(valid_at period) not enforced);";
        let formatted = format_sql(sql);
        assert!(formatted.contains("WITHOUT OVERLAPS"));
        assert!(formatted.contains("PERIOD"));
        assert!(formatted.contains("NOT ENFORCED"));
    }

    #[test]
    fn formats_merge_and_returning_aliases() {
        let sql = "merge into products p using new_products n on p.product_no=n.product_no when not matched then insert values(n.product_no,n.name,n.price) when matched then update set name=n.name,price=n.price returning with (old as o,new as n2) o.*,n2.*;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("MERGE INTO products p\nUSING\n  new_products n\nON"));
        assert!(formatted.contains("WHEN NOT MATCHED THEN\n  INSERT"));
        assert!(formatted.contains("RETURNING\n  WITH (OLD AS o, NEW AS n2) o.*,"));
    }

    #[test]
    fn formats_nested_plpgsql_body() {
        let sql = "create function f() returns void language plpgsql as $$begin if true then raise notice 'ok'; end if; end$$;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("$$\n  BEGIN\n    IF TRUE THEN"));
        assert!(formatted.contains("RAISE NOTICE 'ok';"));
    }

    #[test]
    fn formats_create_function_options_on_separate_lines() {
        let sql = "create or replace function audit_member_role() returns trigger language plpgsql security definer set search_path = pg_catalog, public as $$begin return new; end;$$;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "CREATE OR REPLACE FUNCTION audit_member_role()\nRETURNS TRIGGER\nLANGUAGE plpgsql\nSECURITY DEFINER\nSET search_path = pg_catalog, public\nAS\n$$\n  BEGIN\n    RETURN NEW;\n  END;\n$$;"
        ));
        assert!(!formatted.contains("RETURNS TRIGGER LANGUAGE"));
    }

    #[test]
    fn formats_plpgsql_trigger_audit_function_contexts() {
        let sql = "create or replace function auth_audit_organization_change() returns trigger language plpgsql security definer set search_path = pg_catalog, public as $$ declare row_data auth_organization % ROWTYPE; action_name text; begin row_data := coalesce(new, old); if tg_op = 'INSERT' then action_name := 'org.create'; elsif tg_op = 'DELETE' then action_name := 'org.delete'; elsif new.delete_time is distinct from old.delete_time and new.delete_time is not null then action_name := 'org.delete'; else action_name := 'org.update'; end if; insert into auth_audit_log(actor_id, organization_id, ACTION, resource, result, metadata) values(current_actor(), row_data.id, action_name, 'auth_organization:' || row_data.id, 'success', jsonb_build_object('slug', row_data.slug)); return row_data; end; $$;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "CREATE OR REPLACE FUNCTION auth_audit_organization_change()\nRETURNS TRIGGER\nLANGUAGE plpgsql\nSECURITY DEFINER\nSET search_path = pg_catalog, public\nAS"
        ));
        assert!(formatted.contains("row_data auth_organization%ROWTYPE;"));
        assert!(formatted.contains(
            "ELSIF NEW.delete_time IS DISTINCT FROM OLD.delete_time AND NEW.delete_time IS NOT NULL THEN"
        ));
        assert!(formatted.contains("\n      action,\n"));
        assert!(!formatted.contains(" % ROWTYPE"));
        assert!(!formatted.contains("IS DISTINCT\n    FROM"));
        assert!(!formatted.contains("\n      ACTION,\n"));
    }

    #[test]
    fn formats_insert_target_keyword_identifiers() {
        let sql = "insert into sample_measurement_sets (sample_id, source, VALUES, checks, measured_time, creator_id) values (@sample_id, @source, @values, @checks, @measured_time, @creator_id) returning id, VALUES, checks; select id, VALUES, checks from sample_measurement_sets;";
        let formatted = format_sql(sql);
        assert!(formatted.contains(
            "INSERT INTO sample_measurement_sets (\n  sample_id,\n  source,\n  values,\n  checks,"
        ));
        assert!(formatted.contains("RETURNING\n  id,\n  values,\n  checks;"));
        assert!(formatted.contains("SELECT\n  id,\n  values,\n  checks\nFROM"));
        assert!(!formatted.contains("\n  VALUES,\n"));
        assert!(!formatted.contains("VALUES\n,"));
    }

    #[test]
    fn wraps_long_parenthesized_comma_groups() {
        let options = FormatOptions {
            wrap_limit: Some(70),
            ..FormatOptions::default()
        };
        let sql = "select jsonb_build_object('first_key','aaaaaaaaaaaaaaaaaaaa','second_key','bbbbbbbbbbbbbbbbbbbb') as payload;";
        let formatted = format_sql_with_options(sql, &options);
        assert!(formatted.contains(
            "jsonb_build_object(\n    'first_key',\n    'aaaaaaaaaaaaaaaaaaaa',\n    'second_key',\n    'bbbbbbbbbbbbbbbbbbbb'\n  ) AS payload"
        ));
    }

    #[test]
    fn wraps_long_plpgsql_boolean_segments() {
        let options = FormatOptions {
            wrap_limit: Some(80),
            ..FormatOptions::default()
        };
        let sql = "create function f() returns void language plpgsql as $$begin if new.delete_time is distinct from old.delete_time and new.delete_time is not null then return; end if; end$$;";
        let formatted = format_sql_with_options(sql, &options);
        assert!(formatted.contains(
            "IF NEW.delete_time IS DISTINCT FROM OLD.delete_time\n      AND NEW.delete_time IS NOT NULL THEN"
        ));
    }

    #[test]
    fn formats_full_plpgsql_control_structures() {
        let sql = "create or replace function demo(ids int[]) returns void language plpgsql as $$ <<outer_block>> declare item int; stack text; begin foreach item in array ids loop begin if item is null then raise notice 'skip'; continue; elsif item < 0 then exit outer_block when item < -10; else perform log_item(item); end if; exception when others then get stacked diagnostics stack = pg_exception_context; raise warning 'failed %: %', item, stack; end; end loop; return; end $$;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("<<outer_block>>"));
        assert!(formatted.contains("FOREACH item IN ARRAY ids LOOP"));
        assert!(formatted.contains("ELSIF item < 0 THEN"));
        assert!(formatted.contains("EXIT outer_block WHEN item < -10;"));
        assert!(formatted.contains("GET STACKED DIAGNOSTICS stack = PG_EXCEPTION_CONTEXT;"));
    }

    #[test]
    fn formats_plpgsql_case_for_and_return_query() {
        let sql = "do $$ declare r record; begin for r in select id,status from jobs where status='queued' loop case r.status when 'queued' then update jobs set status='running' where id=r.id; else raise exception 'bad'; end case; end loop; return query select 1; end; $$;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("FOR r IN\n      SELECT"));
        assert!(formatted.contains("CASE r.status\n        WHEN 'queued' THEN"));
        assert!(formatted.contains("RAISE EXCEPTION 'bad';"));
        assert!(formatted.contains("RETURN QUERY\n      SELECT\n        1;"));
    }

    #[test]
    fn wraps_large_array_literals() {
        let sql = "select unnest(array['sloper.boms.read','sloper.boms.write','sloper.chat.read','sloper.chat.write','sloper.materials.read','sloper.materials.write','sloper.samples.read','sloper.samples.write']) as permission_key;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("ARRAY[\n"));
        assert!(formatted.contains("'sloper.boms.read',\n"));
        assert!(formatted.contains("'sloper.samples.write'\n"));
        assert!(formatted.contains("\n  ]) AS permission_key;"));
    }

    #[test]
    fn keeps_small_array_literals_inline() {
        let sql = "select array[stmt], array['to@example.com']::text[], values[1] from inbox;";
        let formatted = format_sql(sql);
        assert!(formatted.contains("ARRAY[stmt]"));
        assert!(formatted.contains("ARRAY['to@example.com']::text[]"));
        assert!(formatted.contains("values[1]"));
        assert!(!formatted.contains("ARRAY[\n  stmt"));
    }

    #[test]
    fn formats_array_subquery_constructor_without_extra_space() {
        let sql = "select array(select id from jobs order by id);";
        let formatted = format_sql(sql);
        assert!(formatted.contains("ARRAY(\n    SELECT"));
        assert!(!formatted.contains("ARRAY ("));
    }

    #[test]
    fn keeps_nested_function_arguments_inline_inside_wrapped_arrays() {
        let sql = "select array[jsonb_build_object('resource','sloper.boms','action','read'),'sloper.boms.write','sloper.materials.read','sloper.materials.write','sloper.samples.read','sloper.samples.write'];";
        let formatted = format_sql(sql);
        assert!(formatted.contains("ARRAY[\n"));
        assert!(
            formatted
                .contains("jsonb_build_object('resource', 'sloper.boms', 'action', 'read'),\n")
        );
        assert!(!formatted.contains("jsonb_build_object('resource',\n"));
    }

    #[test]
    fn honors_case_options() {
        let options = FormatOptions {
            keyword_case: CaseStyle::Lower,
            ..FormatOptions::default()
        };
        let formatted = format_sql_with_options("select a from t;", &options);
        assert!(formatted.starts_with("select\n"));
    }
}
