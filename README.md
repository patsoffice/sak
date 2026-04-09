# SAK (Swiss Army Knife for LLMs)

SAK is a read-only operations tool designed for use by language models. The key idea: since every operation is strictly read-only with no side effects, an LLM can learn the tool via `sak --help` and then use it autonomously without requiring human approval for each invocation.

Commands are organized by domain. Current domains: `fs` (filesystem), `git` (repository), `json`, `config` (TOML, YAML, plist), `k8s` (read-only Kubernetes against a live cluster), `lxc` (read-only LXD/Incus against a live daemon), `docker` (read-only Docker Engine against a live daemon), and `sqlite` (read-only SQLite databases), with more planned (e.g., `csv`).

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

`sak` exposes four optional features, all on by default so `cargo install sak` ships every domain:

| Feature | Default? | What it adds |
| --- | --- | --- |
| `k8s` | yes | The `k8s` domain (`kinds`, `get`, `images`, `env`, `schema`) and the `kube` / `k8s-openapi` / `tokio` / `http` dependencies needed to talk to a live cluster. |
| `lxc` | yes | The `lxc` domain for read-only access to a live LXD/Incus daemon over a unix socket. Pulls in raw `hyper` + `hyperlocal` + `hyper-util` + `http-body-util` + `tokio`. |
| `docker` | yes | The `docker` domain for read-only access to a live Docker Engine over a unix socket. Shares the same hyper stack as `lxc`. |
| `sqlite` | yes | The `sqlite` domain for peeking inside `.db` files read-only. Pulls in `rusqlite` with the `bundled` libsqlite3 (compiled from source — no system `libsqlite3` dependency at runtime, but adds C compile time on the first build). |

The default-on domains together roughly triple the release binary size and cold link time vs the lean build. Users who don't need them can opt out:

```sh
cargo build --release --no-default-features                                  # lean: no k8s, lxc, docker, or sqlite
cargo build --release --no-default-features --features k8s                   # lean + k8s
cargo build --release --no-default-features --features sqlite                # lean + sqlite
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
sak k8s --help            # default-on; --no-default-features removes it
sak lxc --help            # default-on; --no-default-features removes it
sak docker --help         # default-on; --no-default-features removes it
sak sqlite --help         # default-on; --no-default-features removes it

# Discover options and see examples for a specific command
sak fs grep --help
sak git log --help
sak json query --help
sak config query --help
sak k8s get --help
```

Every subcommand includes `long_about` descriptions and `after_help` with concrete usage examples, so `--help` is always sufficient to learn a command without external documentation.

## Using SAK from an LLM agent

SAK is designed to be the canonical read-only interface for an LLM agent like [Claude Code](https://claude.com/claude-code). With two pieces of configuration in your agent's settings, sak becomes the obvious-and-only path for read-only filesystem, git, json, config, and Kubernetes operations:

1. **Auto-approve sak** so the agent never has to ask permission for an individual `sak` call.
2. **A pre-tool hook that redirects raw `git` and `kubectl` read commands** to their `sak` equivalents, with a clear error message the agent can act on.

The examples below are for Claude Code's `~/.claude/settings.json`. The same patterns adapt to any agent harness that supports per-tool permissions and pre-tool hooks.

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

### Redirect raw `git` reads to `sak git`

The hook below catches the read-only `git` subcommands sak implements (`diff`, `log`, `status`, `show`, `blame`, `branch`, `tag`, `remote`) and returns a `deny` decision that tells the agent to use `sak git` instead. Mutations (`git push`, `git commit`, `git reset`, ...) are not blocked — they pass through and the agent's normal permission flow handles them.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "jq -r '.tool_input.command // \"\"' | grep -qE '^\\s*git\\s+(diff|log|status|show|blame|branch|tag|remote)\\b' && printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"Use sak git instead (e.g. sak git diff, sak git log, sak git status, sak git show, sak git blame)\"}}' || true",
            "statusMessage": "Checking for raw git commands..."
          }
        ]
      }
    ]
  }
}
```

### Redirect raw `kubectl` reads to `sak k8s`

Same pattern for `kubectl`. The hook only blocks the kubectl read commands that have a direct `sak k8s` equivalent today:

| Blocked | Replace with |
| --- | --- |
| `kubectl get` | `sak k8s get` |
| `kubectl api-resources` | `sak k8s kinds` |
| `kubectl explain` | `sak k8s schema` |
| `kubectl config get-contexts` | `sak k8s contexts` |

Other `kubectl` reads (`describe`, `logs`, `top`, `events`, `auth can-i`, `version`, ...) pass through because sak doesn't implement them yet — extend the regex's alternation list as new `sak k8s` commands land. Mutations (`apply`, `delete`, `edit`, `exec`, `port-forward`, ...) also pass through and go through the agent's permission flow.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "jq -r '.tool_input.command // \"\"' | grep -qE '^\\s*kubectl\\s+(get|api-resources|explain|config\\s+get-contexts)\\b' && printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"Use sak k8s instead (sak k8s get for kubectl get, sak k8s kinds for kubectl api-resources, sak k8s schema for kubectl explain, sak k8s contexts for kubectl config get-contexts)\"}}' || true",
            "statusMessage": "Checking for raw kubectl commands..."
          }
        ]
      }
    ]
  }
}
```

If you already have a `PreToolUse.Bash.hooks` array, append the new entries rather than replacing the array.

### Redirect raw `docker` reads to `sak docker`

Same pattern for the Docker CLI. The hook only blocks the read commands that have a direct `sak docker` equivalent today:

| Blocked | Replace with |
| --- | --- |
| `docker ps` | `sak docker list` |
| `docker inspect` | `sak docker info` (or `sak docker config` for the configuration subset) |
| `docker images` | `sak docker images` |

Other `docker` reads (`logs`, `stats`, `events`, `top`, `port`, `version`, ...) pass through because sak doesn't implement them yet — extend the regex's alternation list as new `sak docker` commands land. Mutations (`run`, `exec`, `rm`, `rmi`, `build`, `pull`, `push`, ...) also pass through and go through the agent's permission flow.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "jq -r '.tool_input.command // \"\"' | grep -qE '^\\s*docker\\s+(ps|inspect|images)\\b' && printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"Use sak docker instead (sak docker list for docker ps, sak docker info or sak docker config for docker inspect, sak docker images for docker images)\"}}' || true",
            "statusMessage": "Checking for raw docker commands..."
          }
        ]
      }
    ]
  }
}
```

### Redirect raw `lxc` reads to `sak lxc`

Same pattern for the LXD/Incus CLI. The hook only blocks the read commands that have a direct `sak lxc` equivalent today:

| Blocked | Replace with |
| --- | --- |
| `lxc list` | `sak lxc list` |
| `lxc info` | `sak lxc info` |
| `lxc config` (show) | `sak lxc config` |
| `lxc image list` / `lxc image ls` | `sak lxc images` |

Other `lxc` reads (`storage`, `network`, `profile`, `cluster`, `monitor`, ...) pass through because sak doesn't implement them yet — extend the regex's alternation list as new `sak lxc` commands land. Mutations (`launch`, `start`, `stop`, `delete`, `exec`, `file push`, ...) also pass through and go through the agent's permission flow.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "jq -r '.tool_input.command // \"\"' | grep -qE '^\\s*lxc\\s+(list|info|config|image\\s+(list|ls))\\b' && printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"Use sak lxc instead (sak lxc list for lxc list, sak lxc info for lxc info, sak lxc config for lxc config show, sak lxc images for lxc image list)\"}}' || true",
            "statusMessage": "Checking for raw lxc commands..."
          }
        ]
      }
    ]
  }
}
```

### Tell the agent the rule directly (CLAUDE.md / AGENTS.md)

The hooks above only catch `git` and `kubectl` — they say nothing about `ls`, `find`, `cat`, `head`, `tail`, `grep`, `cut`, or `awk`. For those, the most reliable lever is a project instruction file that the agent reads at the start of every session. In Claude Code that's `CLAUDE.md` at the repo root (other harnesses use `AGENTS.md` or similar). Drop a section like this near the top:

```markdown
## Use sak as your tool

When you need to inspect the filesystem, repo, JSON/TOML/YAML/plist, or
a live Kubernetes cluster, **prefer `sak <domain> <command>` over shell
equivalents**:

- `sak fs glob '<pattern>'` instead of `ls`, `find`, or `**` shell globs
- `sak fs read <file> -n <lo>-<hi>` instead of `cat`, `head`, `tail`, `sed -n`
- `sak fs grep <pattern> <path>` instead of `grep` / `rg`
- `sak fs cut -d <delim> -f <n>` instead of `cut` / `awk '{print $n}'`
- `sak git status|log|diff|blame|show` instead of read-only `git`
- `sak json query|keys|flatten|validate` for `*.json`
- `sak config query|keys|flatten|validate` for TOML, YAML, plist
- `sak k8s get|list|images|env|schema` instead of `kubectl` reads

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

Belt and braces: hooks block the easy mistakes (`git log`, `kubectl get`), CLAUDE.md teaches the rule each session, and persistent memory keeps it sticky across sessions and context compactions.

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
