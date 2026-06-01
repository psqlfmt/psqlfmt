use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use psqlfmt::config::{
    CaseStyle, CommaStyle, FormatOptions, IndentStyle, PartialOptions, parse_config_file,
    resolve_psqlfmt_configs,
};
use psqlfmt::editorconfig;
use psqlfmt::format_sql_with_options;
use psqlfmt::paths::{WalkOptions, collect_sql_files};

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// SQL files or directories. Use - or no path to read stdin.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Rewrite files in place.
    #[arg(short = 'i', long, alias = "inplace")]
    write: bool,

    /// Check whether files are already formatted.
    #[arg(long)]
    check: bool,

    /// Write formatted output to a file. Valid for stdin or one input file.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Use an explicit .psqlfmt config file.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Do not search for .psqlfmt files.
    #[arg(long)]
    no_config: bool,

    /// Do not read .editorconfig.
    #[arg(long)]
    no_editorconfig: bool,

    /// Do not respect .gitignore, .ignore, or global git ignore files while walking directories.
    #[arg(long)]
    no_ignore: bool,

    /// Include hidden files when walking directories.
    #[arg(long)]
    hidden: bool,

    /// File extensions to format when walking directories.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "sql,pgsql,psql,ddl,dml,plpgsql,postgresql"
    )]
    extensions: Vec<String>,

    /// Spaces per indentation level.
    #[arg(short = 's', long)]
    spaces: Option<usize>,

    /// Use tabs for indentation.
    #[arg(short = 'T', long)]
    tabs: bool,

    /// Keyword case: preserve, lower, upper, capitalize, or 0..3.
    #[arg(short = 'u', long, value_name = "CASE")]
    keyword_case: Option<String>,

    /// Type case: preserve, lower, upper, capitalize, or 0..3.
    #[arg(short = 'U', long, value_name = "CASE")]
    type_case: Option<String>,

    /// Function case: preserve, lower, upper, capitalize, or 0..3.
    #[arg(short = 'f', long, value_name = "CASE")]
    function_case: Option<String>,

    /// Place commas at the start of continued list items.
    #[arg(short = 'b', long)]
    comma_start: bool,

    /// Place commas at the end of list items.
    #[arg(short = 'e', long)]
    comma_end: bool,

    /// Break after every comma.
    #[arg(short = 'B', long)]
    comma_break: bool,

    /// Remove SQL comments.
    #[arg(short = 'n', long, alias = "nocomment")]
    no_comment: bool,

    /// Preserve blank lines from the input where possible.
    #[arg(short = 'k', long)]
    keep_newline: bool,

    /// Do not add an extra blank line at EOF.
    #[arg(short = 'L', long)]
    no_extra_line: bool,

    /// Remove the space before function-call parentheses.
    #[arg(long)]
    no_space_function: bool,

    /// Wrap long comma-separated lists after N columns when possible.
    #[arg(short = 'w', long, value_name = "N")]
    wrap_limit: Option<usize>,

    /// Wrap lists after N items.
    #[arg(short = 'W', long, value_name = "N")]
    wrap_after: Option<usize>,

    /// Obscure literals before formatting.
    #[arg(short = 'a', long)]
    anonymize: bool,

    /// Print resolved config for each input and exit.
    #[arg(long)]
    print_config: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.write && cli.output.is_some() {
        bail!("--write cannot be used with --output");
    }
    if cli.check && cli.output.is_some() {
        bail!("--check cannot be used with --output");
    }

    let paths = if cli.paths.is_empty() {
        vec![PathBuf::from("-")]
    } else {
        cli.paths.clone()
    };

    if paths.iter().any(|path| path.as_os_str() == "-") {
        return format_stdin(&cli);
    }

    let files = collect_sql_files(
        &paths,
        &WalkOptions {
            respect_gitignore: !cli.no_ignore,
            include_hidden: cli.hidden,
            extensions: cli.extensions.clone(),
        },
    )?;

    if files.is_empty() {
        return Ok(());
    }
    if cli.output.is_some() && files.len() != 1 {
        bail!("--output can only be used with one input file");
    }

    let mut changed = Vec::new();
    for file in files {
        let options = resolve_options(&cli, Some(&file))?;
        if cli.print_config {
            print_config(&file, &options)?;
            continue;
        }
        let input = fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()))?;
        let formatted = format_sql_with_options(&input, &options);

        if cli.check {
            if formatted != input {
                changed.push(file);
            }
            continue;
        }

        if cli.write {
            if formatted != input {
                fs::write(&file, formatted)
                    .with_context(|| format!("failed to write {}", file.display()))?;
            }
        } else if let Some(output) = &cli.output {
            fs::write(output, formatted)
                .with_context(|| format!("failed to write {}", output.display()))?;
        } else {
            io::stdout().write_all(formatted.as_bytes())?;
        }
    }

    if cli.check && !changed.is_empty() {
        for file in &changed {
            eprintln!("would reformat {}", file.display());
        }
        bail!("{} file(s) would be reformatted", changed.len());
    }

    Ok(())
}

fn format_stdin(cli: &Cli) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let options = resolve_options(cli, None)?;
    if cli.print_config {
        print_config(Path::new("-"), &options)?;
        return Ok(());
    }
    let formatted = format_sql_with_options(&input, &options);
    if let Some(output) = &cli.output {
        fs::write(output, formatted)
            .with_context(|| format!("failed to write {}", output.display()))?;
    } else {
        io::stdout().write_all(formatted.as_bytes())?;
    }
    Ok(())
}

fn resolve_options(cli: &Cli, file: Option<&Path>) -> Result<FormatOptions> {
    let mut options = FormatOptions::default();

    if !cli.no_editorconfig
        && let Some(file) = file
    {
        editorconfig::load_for_file(file)?.apply_to(&mut options);
    }

    if !cli.no_config {
        for config in resolve_psqlfmt_configs(file) {
            parse_config_file(&config)?.apply_to(&mut options);
        }
    }

    if let Some(config) = &cli.config {
        parse_config_file(config)?.apply_to(&mut options);
    }

    cli_overrides(cli)?.apply_to(&mut options);
    Ok(options)
}

fn cli_overrides(cli: &Cli) -> Result<PartialOptions> {
    let mut partial = PartialOptions::default();
    if let Some(spaces) = cli.spaces {
        partial.indent_size = Some(spaces);
    }
    if cli.tabs {
        partial.indent_style = Some(IndentStyle::Tab);
        partial.indent_size = Some(1);
    }
    if let Some(value) = &cli.keyword_case {
        partial.keyword_case = Some(parse_case_cli(value)?);
    }
    if let Some(value) = &cli.type_case {
        partial.type_case = Some(parse_case_cli(value)?);
    }
    if let Some(value) = &cli.function_case {
        partial.function_case = Some(parse_case_cli(value)?);
    }
    if cli.comma_start {
        partial.comma_style = Some(CommaStyle::Start);
    }
    if cli.comma_end {
        partial.comma_style = Some(CommaStyle::End);
    }
    if cli.comma_break {
        partial.comma_break = Some(true);
    }
    if cli.no_comment {
        partial.remove_comments = Some(true);
    }
    if cli.keep_newline {
        partial.keep_blank_lines = Some(true);
    }
    if cli.no_extra_line {
        partial.no_extra_line = Some(true);
    }
    if cli.no_space_function {
        partial.no_space_function = Some(true);
    }
    if let Some(limit) = cli.wrap_limit {
        partial.wrap_limit = Some((limit > 0).then_some(limit));
    }
    if let Some(after) = cli.wrap_after {
        partial.wrap_after = Some((after > 0).then_some(after));
    }
    if cli.anonymize {
        partial.anonymize = Some(true);
    }
    Ok(partial)
}

fn parse_case_cli(value: &str) -> Result<CaseStyle> {
    match value.to_ascii_lowercase().as_str() {
        "0" | "preserve" | "unchanged" | "keep" => Ok(CaseStyle::Preserve),
        "1" | "lower" | "lowercase" => Ok(CaseStyle::Lower),
        "2" | "upper" | "uppercase" => Ok(CaseStyle::Upper),
        "3" | "capitalize" | "capitalized" | "title" => Ok(CaseStyle::Capitalize),
        _ => {
            let parsed = value.parse::<u8>().with_context(|| {
                format!("unknown case value {value:?}; use preserve/lower/upper/capitalize or 0..3")
            })?;
            CaseStyle::from_numeric(parsed)
        }
    }
}

fn print_config(path: &Path, options: &FormatOptions) -> Result<()> {
    println!(
        "{}: indent={:?} size={} keyword={:?} type={:?} function={:?} comma={:?} wrap_limit={:?} wrap_after={:?}",
        path.display(),
        options.indent_style,
        options.indent_size,
        options.keyword_case,
        options.type_case,
        options.function_case,
        options.comma_style,
        options.wrap_limit,
        options.wrap_after
    );
    Ok(())
}
