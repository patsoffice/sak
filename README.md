# SAK (Swiss Army Knife for LLMs)

SAK is a read-only operations tool designed for use by language models. The key idea: since every operation is strictly read-only with no side effects, an LLM can learn the tool via `sak --help` and then use it autonomously without requiring human approval for each invocation.

Commands are organized by domain. Current domains: `fs` (filesystem), `git` (repository), `json`, `config` (TOML, YAML, plist), and `k8s` (read-only Kubernetes against a live cluster), with more planned (e.g., `csv`).

## Design Decisions

- **Two-level subcommands** — `sak <domain> <operation>` keeps the top level clean and allows future domains without clutter.
- **Read-only only** — No writes, no side effects. This is the core contract that makes the tool safe for autonomous LLM use.
- **LLM-optimized output** — No ANSI colors, no spinners, no interactive prompts. Deterministic sort order. Line numbers on by default. Every subcommand includes `--help` examples.
- **Bounded output** — All output flows through `BoundedWriter`, which enforces `--limit` and emits a truncation notice to stderr. This prevents LLMs from drowning in unbounded results.
- **Single binary** — One crate, no workspace. Keeps compilation fast and deployment simple.
- **Minimal dependencies** — Runtime dependencies: `clap`, `globset`, `walkdir`, `regex`, `anyhow`, `git2`, `serde`, `serde_json`, `toml`, `serde_yaml`, `plist`. The `k8s` domain adds `kube`, `k8s-openapi`, `tokio`, and `http` on top.
- **Opt-out k8s domain** — `k8s` is part of the default feature set so `cargo install sak` ships it, but it can be disabled with `--no-default-features` for a leaner build that drops the Kubernetes client, the OpenAPI generated code, and the async runtime. See [Build features](#build-features) below.

## Installing

### From Source

```sh
# Default build — includes every domain, including k8s
cargo install --path .

# Lean build — drops the k8s domain (and its kube/k8s-openapi/tokio/http deps)
cargo install --path . --no-default-features
```

### From a Local Build

```sh
cargo build --release                       # default (with k8s)
cargo build --release --no-default-features # lean build
cp target/release/sak /usr/local/bin/
```

### Build features

`sak` exposes a single optional feature:

| Feature | Default? | What it adds |
| --- | --- | --- |
| `k8s` | yes | The `k8s` domain (`kinds`, `get`, `images`, `env`, `schema`) and the `kube` / `k8s-openapi` / `tokio` / `http` dependencies needed to talk to a live cluster. |

The `k8s` feature roughly doubles the release binary size and roughly doubles cold link time. Most other domains are filesystem- and git-only, so users who don't need Kubernetes can opt out:

```sh
cargo build --release --no-default-features                  # no k8s
cargo build --release --no-default-features --features k8s   # explicit opt-in
```

### With Nix Flakes

```sh
# Run directly
nix run github:patsoffice/sak -- --help

# Install into a profile
nix profile install github:patsoffice/sak
```

Or add to a NixOS/home-manager flake:

```nix
{
  inputs.sak.url = "github:patsoffice/sak";

  # NixOS
  environment.systemPackages = [ inputs.sak.packages.${system}.default ];

  # home-manager
  home.packages = [ inputs.sak.packages.${system}.default ];
}
```

## Using

SAK is designed to be self-documenting. An LLM can discover all available domains, commands, and options through `--help` at each level:

```sh
# Discover available domains
sak --help

# Discover commands within a domain
sak fs --help
sak git --help
sak json --help
sak config --help
sak k8s --help            # only present if built with the k8s feature (default)

# Discover options and see examples for a specific command
sak fs grep --help
sak git log --help
sak json query --help
sak config query --help
sak k8s get --help
```

Every subcommand includes `long_about` descriptions and `after_help` with concrete usage examples, so `--help` is always sufficient to learn a command without external documentation.

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
| `serde` / `serde_json` | JSON parsing and shared value model |
| `toml` | TOML parsing for `config` domain |
| `serde_yaml` | YAML parsing for `config` domain |
| `plist` | Apple property list parsing (XML and binary) for `config` domain |
| `kube` | Kubernetes client (k8s domain, behind the `k8s` feature) |
| `k8s-openapi` | Generated Kubernetes API types (k8s domain, behind the `k8s` feature) |
| `tokio` | Current-thread async runtime for the k8s domain (behind the `k8s` feature) |
| `http` | Raw `GET` request construction for `sak k8s schema` (behind the `k8s` feature) |

Dev dependencies: `criterion` (benchmarks), `tempfile` (test fixtures).

## Planned Domains

- `csv` — CSV filtering and projection
