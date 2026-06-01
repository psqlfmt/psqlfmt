# psqlfmt

`psqlfmt` is an opinionated PostgreSQL formatter written in Rust. It defaults
toward a small Prettier-style surface: stable output, project configuration,
directory formatting, and ignore-file aware operation.

## Highlights

- Token-preserving PostgreSQL formatter for SQL, DDL, DML, psql meta commands,
  extension operators, dollar-quoted bodies, and first-class PL/pgSQL blocks.
- PL/pgSQL formatting for declarations, labels, `BEGIN`/`EXCEPTION`, `IF` /
  `ELSIF` / `ELSE`, `CASE` / `WHEN`, `LOOP`, `WHILE`, `FOR`, `FOREACH`,
  `RETURN NEXT`, `RETURN QUERY`, `RAISE`, `ASSERT`, `EXIT`, `CONTINUE`, and
  diagnostics statements.
- PostgreSQL 18 syntax vocabulary, including `RETURNING WITH (OLD AS ..., NEW AS
  ...)`, `WITHOUT OVERLAPS`, `PERIOD`, `NOT ENFORCED`, virtual generated columns,
  `COPY` `ON_ERROR`/`REJECT_LIMIT`/`LOG_VERBOSITY`, and `uuidv7()`.
- Directory formatting with `.gitignore`, `.ignore`, git global ignore, and git
  exclude support.
- `.editorconfig` support for indentation, line endings, final newlines, and
  max line length.
- `.psqlfmt` resolution from the formatted file's directory upward, plus `$HOME`
  and XDG config locations.
- Library API via `psqlfmt::format_sql_with_options`.

## CLI

```sh
psqlfmt query.sql
psqlfmt --write migrations/
psqlfmt --check sql/
cat query.sql | psqlfmt -
```

By default, files are printed to stdout. Use `--write` to rewrite files and
`--check` in CI.

## Installation

```sh
cargo install psqlfmt
brew install psqlfmt/tap/psqlfmt
```

## Configuration

`psqlfmt` reads EditorConfig first, then `.psqlfmt`, then explicit CLI flags. A
nearer `.psqlfmt` overrides parent directories and home/XDG defaults.

Example `.psqlfmt`:

```ini
spaces=2
keyword-case=2
type-case=1
function-case=0
comma=end
wrap-after=0
no-extra-line=0
```

Supported case values:

- `0` or `preserve`
- `1` or `lower`
- `2` or `upper`
- `3` or `capitalize`

## Style

The default style is intentionally stable:

- Two spaces per indentation level.
- Major clauses start on their own line.
- Select lists, returning lists, `GROUP BY`, and `ORDER BY` break after commas.
- Boolean predicates break before `AND`/`OR`.
- Nested query parentheses and PL/pgSQL blocks indent recursively.
- Unknown syntax is preserved as tokens instead of being rejected by a partial AST.
