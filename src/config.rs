use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseStyle {
    Preserve,
    Lower,
    Upper,
    Capitalize,
}

impl CaseStyle {
    pub fn from_numeric(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Preserve),
            1 => Ok(Self::Lower),
            2 => Ok(Self::Upper),
            3 => Ok(Self::Capitalize),
            _ => bail!("case values must be one of 0, 1, 2, or 3"),
        }
    }

    pub fn apply(self, word: &str) -> String {
        match self {
            Self::Preserve => word.to_string(),
            Self::Lower => word.to_ascii_lowercase(),
            Self::Upper => word.to_ascii_uppercase(),
            Self::Capitalize => {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_ascii_uppercase().to_string()
                            + chars.as_str().to_ascii_lowercase().as_str()
                    }
                    None => String::new(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Space,
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommaStyle {
    End,
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent_style: IndentStyle,
    pub indent_size: usize,
    pub tab_width: usize,
    pub keyword_case: CaseStyle,
    pub type_case: CaseStyle,
    pub function_case: CaseStyle,
    pub comma_style: CommaStyle,
    pub comma_break: bool,
    pub remove_comments: bool,
    pub no_extra_line: bool,
    pub keep_blank_lines: bool,
    pub no_space_function: bool,
    pub redundant_parenthesis: bool,
    pub wrap_limit: Option<usize>,
    pub wrap_after: Option<usize>,
    pub anonymize: bool,
    pub insert_final_newline: bool,
    pub end_of_line: EndOfLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOfLine {
    Lf,
    Crlf,
    Cr,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Space,
            indent_size: 2,
            tab_width: 4,
            keyword_case: CaseStyle::Upper,
            type_case: CaseStyle::Lower,
            function_case: CaseStyle::Preserve,
            comma_style: CommaStyle::End,
            comma_break: false,
            remove_comments: false,
            no_extra_line: false,
            keep_blank_lines: false,
            no_space_function: false,
            redundant_parenthesis: false,
            wrap_limit: None,
            wrap_after: None,
            anonymize: false,
            insert_final_newline: true,
            end_of_line: EndOfLine::Lf,
        }
    }
}

impl FormatOptions {
    pub fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Space => " ".repeat(self.indent_size),
            IndentStyle::Tab => "\t".to_string(),
        }
    }

    pub fn apply_line_endings(&self, formatted: &str) -> String {
        let normalized = formatted.replace("\r\n", "\n").replace('\r', "\n");
        let mut out = if self.insert_final_newline {
            let mut s = normalized.trim_end_matches('\n').to_string();
            s.push('\n');
            s
        } else {
            normalized.trim_end_matches('\n').to_string()
        };

        match self.end_of_line {
            EndOfLine::Lf => out,
            EndOfLine::Crlf => {
                out = out.replace('\n', "\r\n");
                out
            }
            EndOfLine::Cr => {
                out = out.replace('\n', "\r");
                out
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct PartialOptions {
    pub indent_style: Option<IndentStyle>,
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub keyword_case: Option<CaseStyle>,
    pub type_case: Option<CaseStyle>,
    pub function_case: Option<CaseStyle>,
    pub comma_style: Option<CommaStyle>,
    pub comma_break: Option<bool>,
    pub remove_comments: Option<bool>,
    pub no_extra_line: Option<bool>,
    pub keep_blank_lines: Option<bool>,
    pub no_space_function: Option<bool>,
    pub redundant_parenthesis: Option<bool>,
    pub wrap_limit: Option<Option<usize>>,
    pub wrap_after: Option<Option<usize>>,
    pub anonymize: Option<bool>,
    pub insert_final_newline: Option<bool>,
    pub end_of_line: Option<EndOfLine>,
}

impl PartialOptions {
    pub fn apply_to(self, options: &mut FormatOptions) {
        if let Some(value) = self.indent_style {
            options.indent_style = value;
        }
        if let Some(value) = self.indent_size {
            options.indent_size = value.max(1);
        }
        if let Some(value) = self.tab_width {
            options.tab_width = value.max(1);
        }
        if let Some(value) = self.keyword_case {
            options.keyword_case = value;
        }
        if let Some(value) = self.type_case {
            options.type_case = value;
        }
        if let Some(value) = self.function_case {
            options.function_case = value;
        }
        if let Some(value) = self.comma_style {
            options.comma_style = value;
        }
        if let Some(value) = self.comma_break {
            options.comma_break = value;
        }
        if let Some(value) = self.remove_comments {
            options.remove_comments = value;
        }
        if let Some(value) = self.no_extra_line {
            options.no_extra_line = value;
        }
        if let Some(value) = self.keep_blank_lines {
            options.keep_blank_lines = value;
        }
        if let Some(value) = self.no_space_function {
            options.no_space_function = value;
        }
        if let Some(value) = self.redundant_parenthesis {
            options.redundant_parenthesis = value;
        }
        if let Some(value) = self.wrap_limit {
            options.wrap_limit = value;
        }
        if let Some(value) = self.wrap_after {
            options.wrap_after = value;
        }
        if let Some(value) = self.anonymize {
            options.anonymize = value;
        }
        if let Some(value) = self.insert_final_newline {
            options.insert_final_newline = value;
        }
        if let Some(value) = self.end_of_line {
            options.end_of_line = value;
        }
    }
}

pub fn parse_config_file(path: &Path) -> Result<PartialOptions> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_config_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn parse_config_str(content: &str) -> Result<PartialOptions> {
    let mut partial = PartialOptions::default();

    for (line_no, raw_line) in content.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected key=value", line_no + 1);
        };
        apply_key_value(&mut partial, normalize_key(key).as_str(), value.trim())
            .with_context(|| format!("line {}", line_no + 1))?;
    }

    Ok(partial)
}

pub fn apply_key_value(partial: &mut PartialOptions, key: &str, value: &str) -> Result<()> {
    let unquoted = unquote(value.trim());
    let value = unquoted.as_str();
    match key {
        "spaces" | "indent_size" | "indent-size" => {
            partial.indent_size = Some(parse_usize(value)?);
        }
        "tabs" => {
            if parse_bool(value)? {
                partial.indent_style = Some(IndentStyle::Tab);
                partial.indent_size = Some(1);
            }
        }
        "indent_style" | "indent-style" => {
            partial.indent_style = Some(match value.to_ascii_lowercase().as_str() {
                "tab" | "tabs" => IndentStyle::Tab,
                "space" | "spaces" => IndentStyle::Space,
                other => bail!("unknown indent_style {other:?}"),
            });
        }
        "tab_width" | "tab-width" => {
            partial.tab_width = Some(parse_usize(value)?);
        }
        "keyword_case" | "keyword-case" | "uc_keywords" | "uc-keywords" => {
            partial.keyword_case = Some(parse_case(value)?);
        }
        "type_case" | "type-case" | "uc_types" | "uc-types" => {
            partial.type_case = Some(parse_case(value)?);
        }
        "function_case" | "function-case" | "uc_functions" | "uc-functions" => {
            partial.function_case = Some(parse_case(value)?);
        }
        "comma" => {
            partial.comma_style = Some(match value.to_ascii_lowercase().as_str() {
                "end" | "trailing" => CommaStyle::End,
                "start" | "leading" => CommaStyle::Start,
                other => bail!("unknown comma style {other:?}"),
            });
        }
        "comma_start" | "comma-start" => {
            if parse_bool(value)? {
                partial.comma_style = Some(CommaStyle::Start);
            }
        }
        "comma_end" | "comma-end" => {
            if parse_bool(value)? {
                partial.comma_style = Some(CommaStyle::End);
            }
        }
        "comma_break" | "comma-break" => partial.comma_break = Some(parse_bool(value)?),
        "nocomment" | "no_comment" | "no-comment" | "remove_comments" | "remove-comments" => {
            partial.remove_comments = Some(parse_bool(value)?);
        }
        "no_extra_line" | "no-extra-line" => partial.no_extra_line = Some(parse_bool(value)?),
        "keep_newline" | "keep-newline" | "keep_blank_lines" | "keep-blank-lines" => {
            partial.keep_blank_lines = Some(parse_bool(value)?);
        }
        "no_space_function" | "no-space-function" => {
            partial.no_space_function = Some(parse_bool(value)?);
        }
        "redundant_parenthesis" | "redundant-parenthesis" => {
            partial.redundant_parenthesis = Some(parse_bool(value)?);
        }
        "wrap_limit" | "wrap-limit" | "max_line_length" | "max-line-length" => {
            partial.wrap_limit = Some(parse_optional_usize(value)?);
        }
        "wrap_after" | "wrap-after" => partial.wrap_after = Some(parse_optional_usize(value)?),
        "anonymize" => partial.anonymize = Some(parse_bool(value)?),
        "insert_final_newline" | "insert-final-newline" => {
            partial.insert_final_newline = Some(parse_bool(value)?);
        }
        "end_of_line" | "end-of-line" => {
            partial.end_of_line = Some(match value.to_ascii_lowercase().as_str() {
                "lf" => EndOfLine::Lf,
                "crlf" => EndOfLine::Crlf,
                "cr" => EndOfLine::Cr,
                other => bail!("unknown end_of_line {other:?}"),
            });
        }
        other => bail!("unknown config key {other:?}"),
    }

    Ok(())
}

pub fn resolve_psqlfmt_configs(start: Option<&Path>) -> Vec<PathBuf> {
    let mut configs = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        push_if_exists(&mut configs, home.join(".psqlfmt"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        push_if_exists(&mut configs, xdg.join("psqlfmt").join("psqlfmt.conf"));
    }

    let mut chain = Vec::new();
    let start = start.unwrap_or_else(|| Path::new("."));
    let mut dir = if start.extension().is_some() || start.file_name() == Some(OsStr::new("-")) {
        start
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        start.to_path_buf()
    };
    if dir.as_os_str().is_empty() {
        dir = PathBuf::from(".");
    }
    if let Ok(canonical) = fs::canonicalize(&dir) {
        dir = canonical;
    }

    loop {
        push_if_exists(&mut chain, dir.join(".psqlfmt"));
        if !dir.pop() {
            break;
        }
    }
    chain.reverse();
    configs.extend(chain);
    configs
}

fn push_if_exists(configs: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_file() && !configs.iter().any(|existing| existing == &path) {
        configs.push(path);
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' | ';' if !in_single && !in_double => return &line[..idx],
            _ => {}
        }
    }
    line
}

pub fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('_', "-")
}

fn parse_case(value: &str) -> Result<CaseStyle> {
    match value.to_ascii_lowercase().as_str() {
        "0" | "preserve" | "unchanged" | "keep" => Ok(CaseStyle::Preserve),
        "1" | "lower" | "lowercase" => Ok(CaseStyle::Lower),
        "2" | "upper" | "uppercase" => Ok(CaseStyle::Upper),
        "3" | "capitalize" | "capitalized" | "title" => Ok(CaseStyle::Capitalize),
        other => bail!("unknown case style {other:?}"),
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => bail!("expected boolean, got {other:?}"),
    }
}

fn parse_usize(value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("expected positive integer, got {value:?}"))
}

fn parse_optional_usize(value: &str) -> Result<Option<usize>> {
    let value = value.trim();
    if matches!(value, "0" | "off" | "false" | "unset") {
        Ok(None)
    } else {
        Ok(Some(parse_usize(value)?))
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_psqlfmt_keys() {
        let config = parse_config_str(
            r#"
            spaces=2
            keyword-case=1
            type-case=2
            comma=start
            no-extra-line=1
            "#,
        )
        .unwrap();
        let mut options = FormatOptions::default();
        config.apply_to(&mut options);
        assert_eq!(options.indent_size, 2);
        assert_eq!(options.keyword_case, CaseStyle::Lower);
        assert_eq!(options.type_case, CaseStyle::Upper);
        assert_eq!(options.comma_style, CommaStyle::Start);
        assert!(options.no_extra_line);
    }
}
