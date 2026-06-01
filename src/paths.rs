use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

const DEFAULT_EXTENSIONS: &[&str] = &[
    "sql",
    "pgsql",
    "psql",
    "ddl",
    "dml",
    "plpgsql",
    "postgresql",
];

#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub respect_gitignore: bool,
    pub include_hidden: bool,
    pub extensions: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            include_hidden: false,
            extensions: DEFAULT_EXTENSIONS
                .iter()
                .map(|extension| (*extension).to_string())
                .collect(),
        }
    }
}

pub fn collect_sql_files(paths: &[PathBuf], options: &WalkOptions) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for path in paths {
        if path.as_os_str() == "-" {
            continue;
        }
        let metadata = path
            .metadata()
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if metadata.is_file() {
            if is_sql_file(path, &options.extensions) {
                files.push(path.to_path_buf());
            }
        } else if metadata.is_dir() {
            let mut builder = WalkBuilder::new(path);
            builder
                .standard_filters(options.respect_gitignore)
                .hidden(!options.include_hidden)
                .git_ignore(options.respect_gitignore)
                .git_global(options.respect_gitignore)
                .git_exclude(options.respect_gitignore)
                .parents(options.respect_gitignore)
                .require_git(false);

            for entry in builder.build() {
                let entry = entry.with_context(|| format!("failed to walk {}", path.display()))?;
                if entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_file())
                    && is_sql_file(entry.path(), &options.extensions)
                {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

pub fn is_sql_file(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn respects_gitignore_for_directory_walks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.sql\n").unwrap();
        fs::write(dir.path().join("ignored.sql"), "select 1;").unwrap();
        fs::write(dir.path().join("kept.sql"), "select 2;").unwrap();

        let files =
            collect_sql_files(&[dir.path().to_path_buf()], &WalkOptions::default()).unwrap();
        assert_eq!(files, vec![dir.path().join("kept.sql")]);
    }
}
