use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};

use crate::config::{EndOfLine, IndentStyle, PartialOptions, apply_key_value, normalize_key};

#[derive(Debug, Clone)]
struct EditorConfigFile {
    path: PathBuf,
    root: bool,
    sections: Vec<Section>,
}

#[derive(Debug, Clone)]
struct Section {
    pattern: String,
    values: Vec<(String, String)>,
}

pub fn load_for_file(path: &Path) -> Result<PartialOptions> {
    let mut configs = discover(path);
    configs.reverse();

    let mut partial = PartialOptions::default();
    for config_path in configs {
        let config = parse_editorconfig_file(&config_path)?;
        apply_editorconfig_file(&config, path, &mut partial)?;
    }
    Ok(partial)
}

fn discover(path: &Path) -> Vec<PathBuf> {
    let mut configs = Vec::new();
    let mut dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };

    if let Ok(canonical) = fs::canonicalize(&dir) {
        dir = canonical;
    }

    loop {
        let candidate = dir.join(".editorconfig");
        if candidate.is_file() {
            let root = fs::read_to_string(&candidate)
                .ok()
                .is_some_and(|content| editorconfig_declares_root(&content));
            configs.push(candidate);
            if root {
                break;
            }
        }
        if !dir.pop() {
            break;
        }
    }

    configs
}

fn editorconfig_declares_root(content: &str) -> bool {
    content.lines().any(|line| {
        let line = strip_comment(line).trim();
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("root")
                && matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "1"
                )
        })
    })
}

fn parse_editorconfig_file(path: &Path) -> Result<EditorConfigFile> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut root = false;
    let mut sections = Vec::<Section>::new();
    let mut current: Option<Section> = None;

    for raw_line in content.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some(Section {
                pattern: line[1..line.len() - 1].trim().to_string(),
                values: Vec::new(),
            });
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = normalize_key(key);
        let value = value.trim().to_string();
        if current.is_none() && key == "root" {
            root = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1");
        } else if let Some(section) = current.as_mut() {
            section.values.push((key, value));
        }
    }

    if let Some(section) = current {
        sections.push(section);
    }

    Ok(EditorConfigFile {
        path: path.to_path_buf(),
        root,
        sections,
    })
}

fn apply_editorconfig_file(
    config: &EditorConfigFile,
    file: &Path,
    partial: &mut PartialOptions,
) -> Result<()> {
    let base = config.path.parent().unwrap_or_else(|| Path::new("."));
    let relative = file.strip_prefix(base).unwrap_or(file);

    for section in &config.sections {
        if section_matches(&section.pattern, relative, file)? {
            for (key, value) in &section.values {
                apply_editorconfig_value(partial, key, value)?;
            }
        }
    }

    let _ = config.root;
    Ok(())
}

fn section_matches(pattern: &str, relative: &Path, absolute: &Path) -> Result<bool> {
    let relative = path_for_glob(relative);
    let basename = absolute
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let mut builder = GlobSetBuilder::new();

    if pattern.contains('/') {
        builder.add(Glob::new(pattern)?);
    } else {
        builder.add(Glob::new(pattern)?);
        builder.add(Glob::new(format!("**/{pattern}").as_str())?);
    }
    let set = builder.build()?;
    Ok(set.is_match(relative.as_str()) || set.is_match(basename))
}

fn apply_editorconfig_value(partial: &mut PartialOptions, key: &str, value: &str) -> Result<()> {
    match key {
        "indent-style" => match value.to_ascii_lowercase().as_str() {
            "tab" => partial.indent_style = Some(IndentStyle::Tab),
            "space" => partial.indent_style = Some(IndentStyle::Space),
            _ => {}
        },
        "indent-size" => {
            if value.eq_ignore_ascii_case("tab") {
                partial.indent_style = Some(IndentStyle::Tab);
            } else if let Ok(size) = value.parse::<usize>() {
                partial.indent_size = Some(size);
            }
        }
        "tab-width" => {
            if let Ok(size) = value.parse::<usize>() {
                partial.tab_width = Some(size);
            }
        }
        "max-line-length" if !value.eq_ignore_ascii_case("off") => {
            apply_key_value(partial, "wrap-limit", value)?;
        }
        "max-line-length" => {}
        "end-of-line" => {
            partial.end_of_line = match value.to_ascii_lowercase().as_str() {
                "lf" => Some(EndOfLine::Lf),
                "crlf" => Some(EndOfLine::Crlf),
                "cr" => Some(EndOfLine::Cr),
                _ => partial.end_of_line,
            };
        }
        "insert-final-newline" => {
            apply_key_value(partial, "insert-final-newline", value)?;
        }
        _ => {}
    }
    Ok(())
}

fn strip_comment(line: &str) -> &str {
    for (idx, ch) in line.char_indices() {
        if matches!(ch, '#' | ';') {
            return &line[..idx];
        }
    }
    line
}

fn path_for_glob(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FormatOptions;

    #[test]
    fn applies_closest_editorconfig() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.sql]\nindent_style = space\nindent_size = 2\n",
        )
        .unwrap();
        let file = dir.path().join("query.sql");
        fs::write(&file, "select 1;").unwrap();

        let partial = load_for_file(&file).unwrap();
        let mut options = FormatOptions::default();
        partial.apply_to(&mut options);
        assert_eq!(options.indent_size, 2);
    }
}
