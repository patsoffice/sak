# SAK (Swiss Army Knife for LLMs)

SAK is a read-only operations tool designed for use by language models. The key idea: since every operation is strictly read-only with no side effects, an LLM can learn the tool via `sak --help` and then use it autonomously without requiring human approval for each invocation.

Commands are organized by domain. Current domains: `fs` (filesystem) and `git` (repository), with more planned (e.g., `json`, `csv`, `k8s`).

## Design Decisions

- **Two-level subcommands** — `sak <domain> <operation>` keeps the top level clean and allows future domains without clutter.
- **Read-only only** — No writes, no side effects. This is the core contract that makes the tool safe for autonomous LLM use.
- **LLM-optimized output** — No ANSI colors, no spinners, no interactive prompts. Deterministic sort order. Line numbers on by default. Every subcommand includes `--help` examples.
- **Bounded output** — All output flows through `BoundedWriter`, which enforces `--limit` and emits a truncation notice to stderr. This prevents LLMs from drowning in unbounded results.
- **Single binary** — One crate, no workspace. Keeps compilation fast and deployment simple.
- **Minimal dependencies** — Six runtime dependencies: `clap`, `globset`, `walkdir`, `regex`, `anyhow`, `git2`.

## Installing

### From Source

```sh
cargo install --path .
```

### From a Local Build

```sh
cargo build --release
cp target/release/sak /usr/local/bin/
```

## Using

SAK is designed to be self-documenting. An LLM can discover all available domains, commands, and options through `--help` at each level:

```sh
# Discover available domains
sak --help

# Discover commands within a domain
sak fs --help
sak git --help

# Discover options and see examples for a specific command
sak fs grep --help
sak git log --help
```

Every subcommand includes `long_about` descriptions and `after_help` with concrete usage examples, so `--help` is always sufficient to learn a command without external documentation.

### Finding Files

```sh
# Find all Rust files
sak fs glob '**/*.rs'

# Find files under src/ only
sak fs glob '**/*.rs' src/

# Find specific files with alternation
sak fs glob 'src/{main,lib}.rs'

# List directories
sak fs glob '**/*' --type dir

# Include hidden files
sak fs glob '**/*' --hidden

# Limit results
sak fs glob '**/*.log' --limit 10
```

### Searching File Contents

```sh
# Basic search
sak fs grep 'fn main' src/

# Case-insensitive
sak fs grep -i 'error' /var/log/app.log

# Multiline: find struct bodies spanning multiple lines
sak fs grep -U 'struct \w+\s*\{[^}]*\}' src/

# List files containing TODOs
sak fs grep -l 'TODO' --glob '**/*.rs'

# Count matches per file
sak fs grep -c 'error' logs/

# Show 3 lines of context around each match
sak fs grep -C 3 'panic' src/

# Search only Rust files
sak fs grep 'unwrap' --type rs src/
```

### Extracting Fields

```sh
# Extract fields 1 and 3 (whitespace-delimited)
echo 'alice 30 nyc' | sak fs cut -f 1,3

# Colon delimiter
sak fs cut -d: -f 1 /etc/passwd

# Comma delimiter with field range
sak fs cut -d ',' -f 2-4 data.csv

# Split into at most 4 fields (remainder stays in last field)
echo 'I hate everything about you' | sak fs cut -d ' ' --max-fields 4 -f 1-

# Filter: only lines where field 1 equals "error"
sak fs cut -f 2 --filter '1=error' log.txt

# Regex filter: field 1 matches pattern
sak fs cut -f 2 --filter '1~^ERR' log.txt

# Select fields by header name
sak fs cut --header -f name,age data.tsv

# Regex delimiter
sak fs cut --regex-delim '[,;]+' -f 1,3 data.txt

# Deduplicate output
sak fs cut -f 1 --unique names.txt
```

### Reading Files

```sh
# Read a file (up to 2000 lines by default)
sak fs read src/main.rs

# Read lines 1-50
sak fs read src/main.rs -n 1-50

# Read from line 100 to end
sak fs read src/main.rs -n 100-

# Read last 20 lines
sak fs read src/main.rs -n -20

# Skip 10 lines, show 5
sak fs read src/main.rs --offset 10 --limit 5

# Without line numbers
sak fs read src/main.rs --no-line-numbers
```

### Git Status

```sh
# Show working tree status (porcelain-style XY codes)
sak git status

# Status for another repo
sak git status -C /path/to/repo
```

### Git Log

```sh
# Last 10 commits, compact
sak git log --oneline -n 10

# Full log with details
sak git log -n 5

# Filter by author
sak git log --oneline --author alice

# Filter by message
sak git log --oneline --grep "fix"

# Commits since a date
sak git log --oneline --since 2024-01-01

# Commits touching specific paths
sak git log --oneline -- src/
```

### Git Diff

```sh
# Unstaged changes
sak git diff

# Staged changes
sak git diff --staged

# Changed file names only
sak git diff --name-only

# Diff with stat summary
sak git diff --stat

# Between two commits
sak git diff --commit HEAD~3 --commit2 HEAD
```

### Git Show

```sh
# Show HEAD commit with diff
sak git show

# Show with stat summary
sak git show HEAD --stat

# Show only changed file names
sak git show HEAD~2 --name-only

# Custom format
sak git show --format '%h %an: %s'
```

### Git Blame

```sh
# Blame entire file
sak git blame src/main.rs

# Blame specific line range
sak git blame -L 10,20 src/main.rs

# Blame with offset range
sak git blame -L 10,+5 src/main.rs

# Limit output
sak git blame src/main.rs --limit 50
```

### Git Branches, Tags, Remotes

```sh
# List local branches (* marks current)
sak git branch

# All branches including remote
sak git branch --all

# List tags
sak git tags

# Tags sorted by date (newest first)
sak git tags --sort date

# List remotes with URLs
sak git remote
```

### Git Contributors and Stash

```sh
# Contributors by commit count
sak git contributors

# Top 10 contributors
sak git contributors -n 10

# Sort alphabetically
sak git contributors --sort name

# List stash entries
sak git stash-list
```

## Output Conventions

- **stdout** — Results only. Clean, parseable, no decoration.
- **stderr** — Errors (prefixed `sak: error:`) and truncation notices.
- **Exit codes** — `0` = results found, `1` = no results, `2` = error.
- **Line numbers** — Right-aligned, tab-separated (e.g., `42\tcontent`).
- **Deterministic** — Results sorted by name by default for reproducibility.
- **Skipped directories** — `.git`, `target`, `node_modules`, `__pycache__`, `.venv` are excluded by default. Use `--hidden` to include dotfiles.

## Dependencies

| Crate | Purpose |
| --- | --- |
| `clap` (derive) | CLI argument parsing with `wrap_help` |
| `globset` | Glob pattern matching (`**`, `{a,b}`) |
| `walkdir` | Recursive directory traversal |
| `regex` | Regular expression search |
| `anyhow` | Error handling |
| `git2` | Git repository operations (libgit2 bindings) |

Dev dependencies: `criterion` (benchmarks), `tempfile` (test fixtures).

## Planned Domains

- `json` — JSON querying and extraction
- `csv` — CSV filtering and projection
- `config` — TOML/YAML querying and validation
- `k8s` — Read-only Kubernetes operations (get, describe, logs)
