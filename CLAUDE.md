# SAK (Swiss Army Knife for LLMs)

Read-only operations tool designed for LLM consumption. Organized by domain — currently `fs` (filesystem), `git` (repository), `json`, and `config` (TOML, YAML, plist). Run `ls src/*/` to see current domains and commands.

## Build & Test

```bash
cargo build                                                     # Build
cargo test                                                      # Run all tests
cargo clippy --all-features --all-targets                       # Check code quality
cargo clippy --all-features --all-targets --allow-dirty --fix   # Auto-fix clippy warnings before fixing manually
cargo fmt                                                       # Format code
cargo bench                                                     # Run criterion benchmarks
cargo run -- fs glob '**/*.rs' .                                # Example: find Rust files
```

- All tests must pass before committing
- `cargo clippy` must pass with no warnings
- `cargo fmt` must pass with no formatting changes
- Bump the version in `Cargo.toml` before committing new capabilities: minor for a new domain (0.1.0 -> 0.2.0), patch for a new command within an existing domain (0.1.0 -> 0.1.1)

## Commit Style

- Prefix: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`
- Summary line under 80 chars with counts where relevant
- Body: each logical change on its own `-` bullet
- Summarize what was added/changed and why, not just file names

## Architecture

- **Single crate** binary — no workspace, no lib split
- Two-level subcommand structure: `sak <domain> <operation>`
- Discover domains, commands, and usage via `--help` at each level:
  - `sak --help` — list domains and quick-start examples
  - `sak <domain> --help` — list commands in a domain
  - `sak <domain> <command> --help` — detailed options and examples
- Future domains (e.g., `csv`, `k8s`) add new modules under `src/`
- Git domain uses the `git2` crate (libgit2 bindings) — no shelling out to git
- JSON and config domains share `serde_json::Value` as the internal representation; format-agnostic helpers (path parsing, value resolution, key collection, flattening, type names, `ArrayMode`) live in `src/value.rs` and are consumed by both domains
- Config domain parses TOML, YAML, and plist (XML and binary) into `serde_json::Value` via each format's serde integration; format auto-detected by extension or set with `--format` (required for stdin)
- All operations are strictly read-only — no writes, no side effects
- Output goes to stdout, errors to stderr prefixed with `sak: error:`
- Exit codes: 0 = results found, 1 = no results, 2 = error

## Conventions

- **Errors**: `anyhow` throughout (CLI tool, not a library)
- **CLI**: clap derive API with `wrap_help` — every subcommand has `long_about` and `after_help` with examples
- **Output**: no ANSI colors, no spinners, no interactive output — LLMs are the audience
- **Line numbers**: right-aligned, tab-separated (via `format_line_number()` in `output.rs`)
- **BoundedWriter**: all output goes through `BoundedWriter` in `output.rs` — it enforces `--limit` and writes truncation notices to stderr
- **Directory skipping**: `.git`, `target`, `node_modules`, `__pycache__`, `.venv` are skipped by default; use `--hidden` to include dotfiles
- **Directory pruning**: use `walkdir`'s `filter_entry()` to prune, not `continue` (which doesn't prevent descent); always check `e.depth() > 0` to avoid filtering the root
- **Deterministic output**: sort by name by default — LLMs need reproducible results
- **Binary detection**: skip files with NUL bytes in first 8KB (grep)

## File Layout

```
src/main.rs           # Top-level CLI, domain dispatch
src/output.rs         # BoundedWriter (stdout-only), line formatting, path utils, binary detection
src/value.rs          # Shared serde_json::Value helpers (path parsing, walking, type names) used by json + config
src/<domain>/mod.rs   # Domain subcommand dispatch (one per domain)
src/<domain>/<cmd>.rs # Individual command implementation (one per command)
benches/benchmarks.rs # Criterion benchmarks
```

Each domain is a module under `src/` with a `mod.rs` for dispatch and one file per command. Use `ls src/*/` to see current domains and commands.

## Issue Tracking (beads)

This project uses `br` (beads_rust) for local issue tracking. Issues live in `.beads/` and are committed to git.

```bash
br list                                        # Show all open issues
br list --status open --priority 0-1 --json    # High-priority open issues (machine-readable)
br ready --json                                # Actionable issues (not blocked, not deferred)
br show <id>                                   # Show issue details
br create "Title" -p 2 --type feature          # Create an issue (types: feature, bug, task, chore)
br update <id> --status in_progress            # Claim work
br close <id> --reason "explanation"           # Close with reason
br dep add <id> <depends-on-id>                # Express dependency
br sync --flush-only                           # Export to JSONL for git commit
```

- **Priority scale**: 0 = critical, 1 = high, 2 = medium, 3 = low, 4 = backlog
- **Statuses**: `open`, `in_progress`, `deferred`, `closed`
- **Labels**: use to categorize by area (`fs`, `output`, `bench`, etc.)
- Use `RUST_LOG=error` prefix when parsing `--json` output to suppress log noise
- `br` never auto-commits — run `br sync --flush-only` then commit `.beads/` manually
- Check `br ready --json` at the start of a session to see what's actionable
- Close issues with descriptive `--reason` so context is preserved

## Documentation Hygiene

Do not embed volatile counts or statistics (e.g., "69 tests pass", "10 commands") in documentation files like CLAUDE.md, README.md, or issue close reasons. These go stale immediately after the next change. Instead, describe *what* exists qualitatively and let readers run `cargo test`, `sak --help`, etc. to get current numbers.

## Gotchas

- `BoundedWriter` is hardcoded to `StdoutLock` — not generic; this is intentional (all output must go to stdout)
- `--heading` and `--line-number` on grep default to `true` via `default_value = "true"` — they're on unless explicitly disabled
- Cut's `--max-fields` uses `splitn` semantics — splits into at most N fields, remainder stays in the last field
- Cut reads stdin when no files are given — enable piping from other sak commands
- Multiline grep (`-U`) reads entire files into memory; single-line mode reads line-by-line
- Glob uses `globset` (not `glob` crate) for `{a,b}` alternation and `**` support
- Config domain collapses lossy types when parsing into `serde_json::Value`: TOML datetimes, plist dates, and plist binary data become JSON-friendly representations rather than preserving the source-format type — acceptable for read-only LLM consumption
- `sak <domain> keys` takes an optional positional `path` *before* `files`; passing only a filename will be parsed as a path. Example: `sak config keys . Cargo.toml` (use `.` to mean root)
