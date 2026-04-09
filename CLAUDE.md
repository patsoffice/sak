# SAK (Swiss Army Knife for LLMs)

Read-only operations tool designed for LLM consumption. Organized by domain — currently `fs` (filesystem), `git` (repository), `json`, `config` (TOML, YAML, plist), and `k8s` (read-only Kubernetes against a live cluster). Run `ls src/*/` to see current domains and commands.

## Build & Test

```bash
cargo build                                                     # Build (default features = with k8s)
cargo build --no-default-features                               # Lean build (no k8s, no async runtime)
cargo test                                                      # Run all tests (with k8s)
cargo test --no-default-features                                # Run tests without k8s
cargo clippy --all-features --all-targets                       # Check code quality
cargo clippy --all-features --all-targets --allow-dirty --fix   # Auto-fix clippy warnings before fixing manually
cargo fmt                                                       # Format code
cargo bench                                                     # Run criterion benchmarks
cargo run -- fs glob '**/*.rs' .                                # Example: find Rust files
```

The `k8s` cargo feature is **on by default** so `cargo install sak` ships every domain. It pulls in `kube`, `k8s-openapi`, `tokio`, and `http`, which roughly doubles the release binary size and roughly doubles cold link time. Users who don't need Kubernetes can opt out with `--no-default-features`. Both feature sets must build, test, clippy, and fmt clean before committing.

- All tests must pass before committing
- `cargo clippy` must pass with no warnings
- `cargo fmt` must pass with no formatting changes
- Bump the version in `Cargo.toml` before committing new capabilities: minor for a new domain (0.1.0 -> 0.2.0), patch for a new command within an existing domain (0.1.0 -> 0.1.1)
- When a new command shadows a `kubectl` or `git` read operation that the example agent hooks in [README.md](README.md)'s "Using SAK from an LLM agent" section don't yet redirect, extend the regex's alternation list in that section to add the new verb. Examples: a `sak k8s describe` command means the kubectl hook regex should add `describe`; a `sak git stash` command means the git hook regex should add `stash`. Call out the hook update in the commit message — users running Claude Code (or any other agent that copies the hook) need to update their own settings.json manually, and the change will be silently lost otherwise.

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
- Future domains (e.g., `csv`) add new modules under `src/`
- Git domain uses the `git2` crate (libgit2 bindings) — no shelling out to git
- JSON and config domains share `serde_json::Value` as the internal representation; format-agnostic helpers (path parsing, value resolution, key collection, flattening, type names, `ArrayMode`) live in `src/value.rs` and are consumed by both domains
- Config domain parses TOML, YAML, and plist (XML and binary) into `serde_json::Value` via each format's serde integration; format auto-detected by extension or set with `--format` (required for stdin)
- K8s domain talks to a live cluster via the `kube` crate using kubeconfig (or in-cluster service account). Gated behind the `k8s` cargo feature (on by default). The rest of sak stays sync — `k8s::run` builds a current-thread tokio runtime locally and `block_on`s the async dispatcher
- K8s read-only enforcement is convention + a grep test in `src/k8s/client.rs`: every `kube::Api` call and every mutation method (`create`, `delete`, `patch`, ...) must live in `client.rs`. Any other module under `src/k8s/` that mentions those tokens fails the test. This is the cheapest credible defense — `kube` has no read-only client variant
- K8s container walking (used by `images` and `env`) is a pure function over `serde_json::Value` in `src/k8s/containers.rs` — fully unit-testable on hand-built fixtures with no cluster
- K8s schema fetching uses the foundation chokepoint `client::request_text` for raw GETs against `/openapi/v3`, then matches schemas by `x-kubernetes-group-version-kind` annotation rather than by package-style key
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
- K8s `get_dyn` returns `Result<Option<DynamicObject>>` — apiserver 404s map to `Ok(None)` so callers can produce sak's exit code 1 for "not found" without losing the ability to surface other errors as exit code 2. Don't unwrap it unconditionally
- K8s `discovery::resolve` returns `(ApiResource, ApiCapabilities)`; cluster-scoped vs namespaced enforcement happens at the command layer by inspecting `caps.scope` *before* the list/get call (cluster-scoped + `--namespace` should be a hard error)
- Adding new optional dependencies for the `k8s` feature requires both declaring them with `optional = true` *and* adding the `dep:<name>` to the `k8s = [...]` feature list in `Cargo.toml`. `kube` does not re-export the `http::Request` types needed by `client::request_text`, so `http` is its own gated dep
