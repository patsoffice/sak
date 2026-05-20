# SAK (Swiss Army Knife for LLMs)

SAK is a read-only operations tool designed for use by language models. The key idea: since every operation is strictly read-only with no side effects, an LLM can learn the tool via `sak --help` and then use it autonomously without requiring human approval for each invocation.

Commands are organized by domain. Current domains: `fs` (filesystem), `git` (repository), `json`, `config` (TOML, YAML, plist, JSON), `csv`, `cert` (X.509 certificate inspection), `talos` (read-only Talos Linux cluster operations via `talosctl`), `gh` (read-only GitHub operations via the `gh` CLI), `k8s` (read-only Kubernetes against a live cluster), `lxc` (read-only LXD/Incus against a live daemon), `docker` (read-only Docker Engine against a live daemon), `sqlite` (read-only SQLite databases), `prom` (read-only Prometheus / Alertmanager HTTP API), and `hook` (pre-tool-use classification for LLM agent harnesses — see [Using SAK from an LLM agent](#using-sak-from-an-llm-agent)).

## Design Decisions

- **Two-level subcommands** — `sak <domain> <operation>` keeps the top level clean and allows future domains without clutter.
- **Read-only only** — No writes, no side effects. This is the core contract that makes the tool safe for autonomous LLM use.
- **LLM-optimized output** — No ANSI colors, no spinners, no interactive prompts. Deterministic sort order. Line numbers on by default. Every subcommand includes `--help` examples.
- **Bounded output** — All output flows through `BoundedWriter`, which enforces `--limit` and emits a truncation notice to stderr. This prevents LLMs from drowning in unbounded results.
- **Single binary** — One crate, no workspace. Keeps compilation fast and deployment simple.
- **Minimal dependencies** — Runtime dependencies: `clap`, `globset`, `walkdir`, `regex`, `anyhow`, `git2`, `serde`, `serde_json`, `toml`, `serde_yaml`, `plist`. Optional domains add their own clients on top: `k8s` brings `kube` + `k8s-openapi` + `tokio` + `http`; `lxc` and `docker` share a `hyper` + `hyperlocal` + `hyper-util` + `http-body-util` + `tokio` unix-socket stack; `sqlite` brings `rusqlite` with the bundled libsqlite3; `prom` brings `ureq` with rustls (synchronous — pulls no tokio).
- **Opt-out heavy domains** — `k8s`, `lxc`, `docker`, `sqlite`, and `prom` are all part of the default feature set so `cargo install sak` ships every domain, but any of them can be disabled (independently or together) with `--no-default-features` for a leaner build that drops the corresponding clients / generated code / bundled libraries / async runtime. See [Build features](#build-features) below.

## Installing

### From Source

```sh
# Default build — includes every domain (k8s, lxc, docker, sqlite, prom)
cargo install --path .

# Lean build — drops every optional domain and its dependencies
cargo install --path . --no-default-features
```

### From a Local Build

```sh
cargo build --release                       # default (k8s + lxc + docker + sqlite + prom)
cargo build --release --no-default-features # lean build (no optional domains)
cp target/release/sak /usr/local/bin/
```

### Build features

`sak` exposes the following optional features, all on by default so `cargo install sak` ships every domain:

| Feature | Default? | What it adds |
| --- | --- | --- |
| `k8s` | yes | The `k8s` domain (`contexts`, `kinds`, `get`, `images`, `env`, `schema`, `restarts`, `failing`, `pending`, `events`, `describe`, `logs`) and the `kube` / `k8s-openapi` / `tokio` / `http` dependencies needed to talk to a live cluster. |
| `lxc` | yes | The `lxc` domain for read-only access to a live LXD/Incus daemon over a unix socket. Pulls in raw `hyper` + `hyperlocal` + `hyper-util` + `http-body-util` + `tokio`. |
| `docker` | yes | The `docker` domain for read-only access to a live Docker Engine over a unix socket. Shares the same hyper stack as `lxc`. |
| `sqlite` | yes | The `sqlite` domain for peeking inside `.db` files read-only. Pulls in `rusqlite` with the `bundled` libsqlite3 (compiled from source — no system `libsqlite3` dependency at runtime, but adds C compile time on the first build). |
| `prom` | yes | The `prom` domain for read-only Prometheus and Alertmanager HTTP API operations. Pulls in `ureq` with a small rustls stack. Unlike the other optional domains it stays synchronous — `ureq` is blocking and each command is one HTTP round trip, so enabling `prom` alone does not bring `tokio` into the binary. |

The default-on domains together roughly triple the release binary size and cold link time vs the lean build. Users who don't need them can opt out:

```sh
cargo build --release --no-default-features                                  # lean: no k8s, lxc, docker, sqlite, or prom
cargo build --release --no-default-features --features k8s                   # lean + k8s
cargo build --release --no-default-features --features sqlite                # lean + sqlite
cargo build --release --no-default-features --features prom                  # lean + prom (no tokio)
cargo build --release --no-default-features --features lxc,docker            # lean + container daemons
cargo build --release --all-features                                         # everything (same as default today)
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
sak csv --help
sak cert --help
sak talos --help
sak gh --help
sak k8s --help            # default-on; --no-default-features removes it
sak lxc --help            # default-on; --no-default-features removes it
sak docker --help         # default-on; --no-default-features removes it
sak sqlite --help         # default-on; --no-default-features removes it
sak prom --help           # default-on; --no-default-features removes it
sak hook --help

# Discover options and see examples for a specific command
sak fs grep --help
sak git log --help
sak json query --help
sak config query --help
sak k8s get --help
```

Every subcommand includes `long_about` descriptions and `after_help` with concrete usage examples, so `--help` is always sufficient to learn a command without external documentation.

## Using SAK from an LLM agent

SAK is designed to be the canonical read-only interface for an LLM agent like [Claude Code](https://claude.com/claude-code). With two pieces of configuration in your agent's settings, sak becomes the obvious-and-only path for every read-only operation it covers (filesystem, git, json, config, X.509 certs, Talos clusters, Kubernetes, LXD/Incus, Docker, SQLite, Prometheus / Alertmanager):

1. **Auto-approve sak** so the agent never has to ask permission for an individual `sak` call.
2. **One pre-tool hook — `sak hook claude-code`** — that classifies the about-to-run Bash command and redirects read-only `cat`/`head`/`tail`, `grep`/`rg`, `find`, `jq`, `yq`/`tomlq`, `plistutil`, `openssl x509`, `git`, `kubectl`, `talosctl`, `docker`, `lxc`/`incus`, `gh`, and `sqlite3` invocations to their `sak` equivalents. (No `prom` redirect — Prometheus has no canonical CLI to redirect from; the dogfood instruction in `CLAUDE.md` is the right lever for `sak prom`.)

The configuration below is for Claude Code's `~/.claude/settings.json`. The pattern adapts to any agent harness that supports per-tool permissions and pre-tool hooks.

### Auto-approve `sak`

```json
{
  "permissions": {
    "allow": [
      "Bash(sak:*)"
    ]
  }
}
```

Every operation is read-only, so blanket-allowing `sak` is safe — there is no command in the tool that can mutate state, write files, or hit a remote API destructively.

### Install the pre-tool hook

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "sak hook claude-code",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

That's the whole hook. `sak hook claude-code` reads Claude Code's `PreToolUse` JSON payload from stdin, splits the about-to-run command on shell separators (`|`, `||`, `&&`, `;`, `&` — quote-aware), and for each piece checks the command name against an intent-aware rule set:

- **Read vs. write** — `git status`/`diff`/`log`/`show`/`blame`/`shortlog` block; `git commit`/`push`/`add`/`fetch`/`pull`/`checkout`/`rebase`/`merge`/`reset` pass through. `git branch`/`tag`/`remote` block only for listing forms; modifying forms (`-d`/`-D`/`-m`/`-c`/add/set-url) pass.
- **stdin vs. file** — `jq .name pkg.json` blocks (file arg); `echo … | jq .` passes (stdin). Same logic for `yq`, `tomlq`. `cat`/`head`/`tail` allow heredocs and stdin-only forms.
- **Recursive vs. non-recursive grep** — `grep -r foo .`, `grep foo file.txt` and `rg foo` block; `echo … | grep foo` passes.
- **Search vs. write find** — `find . -name *.rs` blocks; `find . -delete`/`-exec`/`-ok` passes.
- **Read vs. write sqlite** — `.tables`/`.schema`/`.dump`/`SELECT` blocks; `INSERT`/`CREATE` passes.

When the rule fires, the hook exits 2 with a stderr message naming the `sak` equivalent (Claude Code surfaces that message back to the model, which then auto-corrects). Otherwise it exits 0 and the command runs.

If you really need to bypass the hook for one call, prefix your Bash invocation with `SAK_HOOK_BYPASS=1`:

```bash
SAK_HOOK_BYPASS=1 git status   # bypass for this one call
```

To debug the rule set from the shell (no stdin payload needed):

```bash
sak hook claude-code --check 'git status'   # prints suggestion, exits 2
sak hook claude-code --check 'git commit'   # silent, exits 0
```

The rule set lives in `src/hook/claude_code.rs` with an inline test suite that pins every block/allow decision. When a new sak command shadows a kubectl/git/docker/lxc/talosctl/gh read that the hook doesn't yet redirect, add a case to `check_*` and a test next to the existing ones — no harness-side config changes needed.

### Tell the agent the rule directly (CLAUDE.md / AGENTS.md)

The hook above catches most CLI-shaped mistakes, but it can't redirect things that have no canonical CLI — Prometheus and Alertmanager are the big ones (`sak prom alerts|query|query-range|histogram|targets|rules|labels|label-values|series|metadata|tsdb-stats|flags|config|am alerts|am silences` vs. ad-hoc `curl + jq` against the HTTP API). It also won't *teach* the agent the underlying habit — when the agent reaches for `sed -n '10,20p'`, the hook stays silent, and the agent thinks it solved the problem. A project instruction file fills both gaps. In Claude Code that's `CLAUDE.md` at the repo root (other harnesses use `AGENTS.md` or similar). Drop a section like this near the top:

```markdown
## Use sak as your tool

When you need to inspect the filesystem, repo, JSON/TOML/YAML/plist,
a live Kubernetes cluster, an LXD/Incus or Docker daemon, a SQLite
database, or a Prometheus / Alertmanager endpoint, **prefer
`sak <domain> <command>` over shell equivalents**:

- `sak fs glob '<pattern>'` instead of `ls`, `find`, or `**` shell globs
- `sak fs read <file> -n <lo>-<hi>` instead of `cat`, `head`, `tail`, `sed -n`
- `sak fs grep <pattern> <path>` instead of `grep` / `rg`
- `sak fs cut -d <delim> -f <n>` instead of `cut` / `awk '{print $n}'`
- `sak git status|log|diff|blame|show` instead of read-only `git`
- `sak json query|exists|keys|flatten|paths|grep|length|schema|select|type|validate|diff` for `*.json`
- `sak config query|exists|keys|flatten|paths|grep|length|schema|type|validate|diff|convert` for TOML, YAML, plist, JSON
- `sak csv headers|query|stats|validate` for `*.csv` and other delimited text
- `sak gh pr-list` / `sak gh pr-view` / `sak gh issue-list` / `sak gh issue-view` / `sak gh run-list` / `sak gh run-view` / `sak gh release-list` / `sak gh workflow-list` / `sak gh repo-view` / `sak gh api <endpoint>` instead of `gh pr list` / `gh pr view` / `gh issue list` / `gh issue view` / `gh run list` / `gh run view` / `gh release list` / `gh workflow list` / `gh repo view` / `gh api` / `curl` against the GitHub API
- `sak k8s get|images|env|schema` instead of `kubectl` reads
- `sak lxc list|info|config|images` instead of `lxc` reads
- `sak docker list|info|config|images` instead of `docker` reads
- `sak sqlite tables|schema|query|info` instead of `sqlite3` reads
- `sak prom alerts|query|query-range|histogram|targets|rules|labels|label-values|series|metadata|tsdb-stats|flags|config|am alerts|am silences`
  instead of `curl + jq + base64` against a Prometheus or Alertmanager API

Discover flags with `sak <domain> <command> --help`. If you want a sak
command that doesn't exist yet, that's a signal to add it, not to fall
back to shell.
```

This catches the cases the hooks don't, and — importantly — it also tells the agent *why* to prefer sak (deterministic output, line numbers, bounded results, no decoration), so it picks sak even in novel situations the rule doesn't enumerate. Pair it with the auto-approve permission above and the agent will reach for sak unprompted.

### Make the rule stick across sessions (agent memory)

Project-instruction files are loaded fresh every conversation, but some agents also support a persistent per-project memory that survives across sessions. Claude Code, for example, has an auto-memory system at `~/.claude/projects/<slug>/memory/`. If your agent has something equivalent, save the same rule there as a *feedback* memory so it survives even in conversations where CLAUDE.md isn't read end-to-end. Suggested content:

```markdown
---
name: Prefer sak over shell tools in this repo
description: Reach for `sak <domain> <command>` instead of ls/find/cat/head/grep/cut/awk and read-only git/kubectl
type: feedback
---

In this repo, prefer `sak <domain> <command>` over shell equivalents
whenever a CLI is warranted. [...same substitution table as CLAUDE.md...]

**Why:** the project dogfoods sak; deterministic, bounded, LLM-tuned
output is the whole point of the tool.

**How to apply:** the harness's built-in file/search tools are still
fine, but when you do reach for a CLI, reach for sak. Missing command
= add it, don't fall back to shell.
```

Belt and braces: `sak hook claude-code` blocks the easy mistakes (`git log`, `kubectl get`, `cat /etc/passwd`, `jq .name pkg.json`, …), CLAUDE.md teaches the rule each session, and persistent memory keeps it sticky across sessions and context compactions.

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
| `tokio` | Current-thread async runtime for the k8s / lxc / docker domains (shared between those three features) |
| `http` | Raw `GET` request construction for `sak k8s schema` (behind the `k8s` feature) |
| `hyper` / `hyperlocal` / `hyper-util` / `http-body-util` | Unix-socket HTTP stack for the `lxc` and `docker` domains |
| `rusqlite` (bundled) | SQLite engine, compiled from source (behind the `sqlite` feature) |
| `ureq` | Blocking HTTP + TLS (rustls) client for the `prom` domain — separate from the hyper stack since `prom` targets remote endpoints, not unix sockets |

Dev dependencies: `criterion` (benchmarks), `tempfile` (test fixtures).

