# SAK (Swiss Army Knife for LLMs)

SAK is a read-only operations tool designed for use by language models. The key idea: since every operation is strictly read-only with no side effects, an LLM can learn the tool via `sak --help` and then use it autonomously without requiring human approval for each invocation.

Commands are organized by domain. The initial domain is `fs` (filesystem), with more planned (e.g., `json`, `csv`, `git`, `k8s`).

### Design Decisions

- **Two-level subcommands** — `sak <domain> <operation>` keeps the top level clean and allows future domains without clutter.
- **Read-only only** — No writes, no side effects. This is the core contract that makes the tool safe for autonomous LLM use.
- **LLM-optimized output** — No ANSI colors, no spinners, no interactive prompts. Deterministic sort order. Line numbers on by default. Every subcommand includes `--help` examples.
- **Bounded output** — All output flows through `BoundedWriter`, which enforces `--limit` and emits a truncation notice to stderr. This prevents LLMs from drowning in unbounded results.
- **Single binary** — One crate, no workspace. Keeps compilation fast and deployment simple.
- **Minimal dependencies** — Five runtime dependencies: `clap`, `globset`, `walkdir`, `regex`, `anyhow`.

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

## Output Conventions

- **stdout** — Results only. Clean, parseable, no decoration.
- **stderr** — Errors (prefixed `sak: error:`) and truncation notices.
- **Exit codes** — `0` = results found, `1` = no results, `2` = error.
- **Line numbers** — Right-aligned, tab-separated (e.g., `  42\tcontent`).
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

Dev dependencies: `criterion` (benchmarks), `tempfile` (test fixtures).

## Planned Domains

- `json` — JSON querying and extraction
- `csv` — CSV filtering and projection
- `git` — Read-only git operations (log, diff, blame)
- `k8s` — Read-only Kubernetes operations (get, describe, logs)
