# SAK (Swiss Army Knife for LLMs)

Read-only operations tool designed for LLM consumption. Organized by domain — currently `fs` (filesystem), `git` (repository), `json`, `config` (TOML, YAML, plist, JSON), `csv`, `cert` (X.509 certificate inspection), `talos` (read-only Talos Linux cluster operations via `talosctl`), `gh` (read-only GitHub operations via the `gh` CLI), `k8s` (read-only Kubernetes against a live cluster), `lxc` (read-only LXD/Incus against a live daemon), `docker` (read-only Docker Engine against a live daemon), `sqlite` (read-only SQLite databases), `prom` (read-only Prometheus / Alertmanager HTTP API), and `hook` (pre-tool-use classification for LLM agent harnesses). Run `sak fs glob 'src/*/'` to see current domains and commands.

## Use sak as your tool

This repo dogfoods its own product. When you need to inspect the filesystem, repo, JSON/TOML/YAML/plist, a live Kubernetes cluster, an LXD/Incus or Docker daemon, a SQLite database, a Prometheus / Alertmanager endpoint, or an X.509 certificate, **prefer `sak <domain> <command>` over shell equivalents**. This applies to both sak's own development and any other read-only inspection you do while working here. Concretely:

- `sak fs glob '<pattern>'` instead of `ls`, `find`, or `**` shell globs
- `sak fs read <file> -n <lo>-<hi>` instead of `cat`, `head`, `tail`, or `sed -n`
- `sak fs grep <pattern> <path>` instead of `grep` / `rg`
- `sak fs cut -d <delim> -f <n>` instead of `cut` / `awk '{print $n}'`
- `sak git status|log|diff|blame|show` instead of shelling out to `git` for read ops
- `sak json query|exists|keys|flatten|paths|grep|length|schema|select|type|validate|diff` for `*.json`
- `sak config query|exists|keys|flatten|paths|grep|length|schema|type|validate|diff|convert` for TOML, YAML, plist, JSON
- `sak csv headers|query|stats|validate` for `*.csv` and other delimited text
- `sak cert inspect|expiring|from-kubeconfig|from-yaml` instead of `openssl x509 | grep | awk` pipelines on PEM/DER
- `sak talos certs|read|get` instead of `for n in <ips>; do talosctl -n $n …; done` fan-out loops
- `sak gh pr-list` / `sak gh issue-list` / `sak gh run-list` / `sak gh release-list` / `sak gh workflow-list` / `sak gh repo-view` / `sak gh api <endpoint>` instead of `gh pr list` / `gh issue list` / `gh run list` / `gh release list` / `gh workflow list` / `gh repo view` / `gh api` / `curl`ing the GitHub REST or GraphQL API for reads
- `sak k8s get|images|env|schema` instead of `kubectl` read ops
- `sak lxc list|info|config|images` instead of `lxc` read ops
- `sak docker list|info|config|images` instead of `docker` read ops
- `sak sqlite tables|schema|query|info` instead of `sqlite3` read ops
- `sak prom alerts|query|query-range|histogram|targets|rules|labels|label-values|series|metadata|tsdb-stats|flags|config|am alerts|am silences` instead of `curl + jq + base64` against a Prometheus or Alertmanager API

The harness's built-in Glob/Read/Grep tools are still fine — and the rule against using bash for `cat`/`head`/`find`/`grep` still applies — but when you *do* reach for a CLI in this repo, reach for `sak`. Run `cargo run --quiet -- <domain> <command> --help` (or, after `cargo install --path .`, just `sak <domain> <command> --help`) to discover flags. If you find yourself wanting a sak command that doesn't exist yet, that's a signal to add it rather than fall back to shell.

## Build & Test

The repo ships a Nix `flake.nix` + `.envrc` that pins the Rust toolchain and a C compiler. If your shell wasn't started inside the dev shell (no `cc` / `cargo` on PATH, or `cargo build` fails with `linker 'cc' not found`), prefix the relevant command with `nix develop -c` — e.g. `nix develop -c cargo build`, `nix develop -c cargo test`, `nix develop -c cargo clippy …`, `nix develop -c cargo fmt`. The dev shell is the source of truth for the toolchain; don't try to install a system Rust to work around it.

```bash
cargo build                                                     # Build (default features = k8s + lxc + docker + sqlite + prom)
cargo build --no-default-features                               # Lean build (no k8s, lxc, docker, sqlite, prom, async runtime)
cargo build --no-default-features --features sqlite             # Lean + sqlite alone
cargo build --no-default-features --features prom               # Lean + prom alone (no tokio — prom is sync)
cargo build --all-features                                      # Same as default today
cargo test                                                      # Run all tests (default features)
cargo test --no-default-features                                # Run tests with no optional domains
cargo test --no-default-features --features sqlite              # sqlite alone
cargo test --no-default-features --features prom                # prom alone
cargo test --all-features                                       # Everything
cargo clippy --all-features --all-targets                       # Check code quality
cargo clippy --all-features --all-targets --allow-dirty --fix   # Auto-fix clippy warnings before fixing manually
cargo fmt                                                       # Format code
cargo bench                                                     # Run criterion benchmarks
cargo run -- fs glob '**/*.rs' .                                # Example: find Rust files
```

The `k8s`, `lxc`, `docker`, `sqlite`, and `prom` cargo features are **all on by default** so `cargo install sak` ships every domain. They pull in `kube` + `k8s-openapi` (k8s), `hyper` + `hyperlocal` + `hyper-util` + `http-body-util` (shared between lxc and docker), `rusqlite` with bundled libsqlite3 (sqlite), `ureq` + rustls (prom), plus a shared `tokio` + `http` stack used by k8s/lxc/docker (prom is sync — `ureq` is blocking and pulls no tokio). Together they roughly triple the release binary size and cold link time, and the bundled libsqlite3 adds C compile time on the first build. Users who don't need any of them can opt out with `--no-default-features`. Both the default and `--no-default-features` builds must build, test, clippy, and fmt clean before committing.

- All tests must pass before committing
- `cargo clippy` must pass with no warnings
- `cargo fmt` must pass with no formatting changes
- Bump the version in `Cargo.toml` before committing new capabilities: minor for a new domain (0.1.0 -> 0.2.0), patch for a new command within an existing domain (0.1.0 -> 0.1.1)

## Adding a new command

A new sak command typically follows this checklist:

1. Implement the command in `src/<domain>/<command>.rs`, wire it through the domain's `mod.rs`, and add an inline `#[cfg(test)] mod tests` block next to it.
2. Update `--help` examples (the per-command `after_help`, the domain quick-start in `src/main.rs`, and the discovery list in `README.md`).
3. **Update the agent hook.** If the new command shadows a read operation in a CLI the hook already covers (`git`, `kubectl`, `docker`, `lxc`/`incus`, `talosctl`, `gh`, `cat`/`head`/`tail`, `grep`/`rg`, `find`, `jq`, `yq`/`tomlq`, `openssl x509`, `sqlite3`), extend the corresponding `check_*` function in [src/hook/claude_code.rs](src/hook/claude_code.rs) and add a block/allow test next to the existing ones. If the new domain redirects from a CLI that's *not* yet on that list (e.g. a future `helm` domain would want to redirect `helm get/list/status`), add a new `check_<tool>` function, route to it from the match in `check()`, and add tests. The agent-side `settings.json` only points at `sak hook claude-code` — the rule set rides in the binary, so users pick up new redirects automatically by upgrading sak. Call out hook changes in the commit message anyway so people running older sak versions know to upgrade.
4. Bump the version in `Cargo.toml` per the rule above.
5. `cargo fmt && cargo clippy --all-features --all-targets && cargo test && cargo test --no-default-features` must all be clean before committing.

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
- JSON and config domains share `serde_json::Value` as the internal representation; format-agnostic helpers (path parsing, value resolution, key collection, flattening, type names, `ArrayMode`, structural diff via `value::diff`) live in `src/value.rs` and are consumed by both domains. `sak json diff` and `sak config diff` both wrap the same `value::diff` helper and only differ in how they load inputs — cross-format diffs (e.g. TOML vs YAML) fall out for free because both formats normalize through `serde_json::Value`
- Config domain parses TOML, YAML, plist (XML and binary), and JSON into `serde_json::Value` via each format's serde integration; format auto-detected by extension or set with `--format` (required for stdin). JSON support means `.json` files are valid inputs to every config command (redundant with the `json` domain for single-file ops, but it lets cross-format ops like `sak config diff a.toml b.json` and `sak config convert --to yaml a.json` fall out for free)
- Cert domain (`src/cert/`) is **always on** (no cargo feature) because its deps (`x509-parser`, `sha2`, `base64`) are pure-Rust and roughly the size of `serde_yaml` — closer to the json/config tier than the k8s/sqlite tier. Inputs auto-detect across PEM (single or bundle), raw DER, and base64-wrapped PEM (the shape Kubernetes uses for `client-certificate-data`). The detection helper `cert::extract_ders` returns a flat `Vec<Vec<u8>>` of DER blobs and is the single chokepoint every command goes through. There is **no** chokepoint grep test or read-only enforcement here — the entire domain is pure parsing with no network and no mutation surface, so there's nothing to guard. The shared `cert::CertInfo` struct and emission helpers (`write_kv`, `write_tsv_row`, JSON via `serde::Serialize`) live in `src/cert/mod.rs` and are reused by `inspect`, `expiring`, `from-kubeconfig`, and `from-yaml` so the four commands always agree on field set, ordering, and `--field <name>` spelling. `from-yaml` and `from-kubeconfig` reuse YAML parsing via `serde_yaml` and the dot-path / JSON-Pointer parser already in `src/value.rs`
- `sak cert expiring` deliberately **inverts** the standard sak exit-code convention — exit 0 means "no certs match the window" (healthy), exit 1 means "at least one cert matches" (alert). This makes `if sak cert expiring; then alert; fi` natural in shell. The inversion is documented in the command's `long_about` so callers aren't surprised
- Talos domain (`src/talos/`) is **always on** (no cargo feature) because it brings no new Rust deps — it shells out to the system `talosctl` binary instead of re-implementing the COSI gRPC client. The runtime cost is one external CLI on PATH; the build cost is zero. Read-only enforcement mirrors the k8s/lxc/docker/sqlite/prom pattern: every `std::process::Command::new("talosctl")` lives in `src/talos/client.rs`, gated by a [`READ_ONLY_VERBS`] allowlist (`get`, `read`, `version`), with a chokepoint grep test that bans the `"talosctl"` string literal and `Command::new(` outside `client.rs`. The allowlist is strictly stronger than convention because `talosctl` has plenty of mutating subcommands (`reboot`, `reset`, `apply-config`, `etcd snapshot restore`, ...). Connection details (talosconfig path + node list) resolve via `src/talos/config.rs`: flag → `$TALOSCONFIG` → `~/.talos/config`, then the active context's `nodes` list drives fan-out. `sak talos certs` is the killer use case for the cert/talos pairing — for every (node, well-known-cert-path) pair it pipes `talosctl read` output through `cert::extract_ders` + `cert::parse_cert` and emits a record per cert; missing files on a given node (e.g. control-plane-only paths on workers) silently drop rather than erroring, which is the expected case
- K8s domain talks to a live cluster via the `kube` crate using kubeconfig (or in-cluster service account). Gated behind the `k8s` cargo feature (on by default). The rest of sak stays sync — `k8s::run` builds a current-thread tokio runtime locally and `block_on`s the async dispatcher
- K8s read-only enforcement is convention + a grep test in `src/k8s/client.rs`: every `kube::Api` call and every mutation method (`create`, `delete`, `patch`, ...) must live in `client.rs`. Any other module under `src/k8s/` that mentions those tokens fails the test. This is the cheapest credible defense — `kube` has no read-only client variant
- K8s container walking (used by `images` and `env`) is a pure function over `serde_json::Value` in `src/k8s/containers.rs` — fully unit-testable on hand-built fixtures with no cluster
- K8s schema fetching uses the foundation chokepoint `client::request_text` for raw GETs against `/openapi/v3`, then matches schemas by `x-kubernetes-group-version-kind` annotation rather than by package-style key
- LXC and docker domains talk to the local LXD/Incus or Docker Engine REST API over a unix socket via `hyper` + `hyperlocal`. Each domain is gated behind its own cargo feature (`lxc`, `docker`, both on by default) and shares the k8s pattern of running async code on a per-invocation current-thread tokio runtime built locally in `lxc::run` / `docker::run` so the rest of sak stays sync. The chokepoint module (`src/lxc/client.rs`, `src/docker/client.rs`) is the only place allowed to construct a `hyper::Client`, import `hyperlocal::*`, or build a `Request` — every command goes through `LxcClient::get_json` / `DockerClient::get_json`, which return `Ok(None)` on a 404 so callers can map "not found" to sak's exit code 1 (mirrors `k8s::client::get_dyn`). LXD additionally unwraps the `{type, status, status_code, metadata}` envelope and exposes a `get_json_recursive` helper for `?recursion=N`; Docker returns the daemon's JSON verbatim
- Read-only enforcement for `lxc` and `docker` is the same convention + grep test as k8s: each `client.rs` has a `tests::no_mutation_methods_outside_client_module` test that scans every other `*.rs` in its domain for `hyper::Client`, `hyperlocal::`, `Request::builder`, and any `Method::POST|PUT|PATCH|DELETE` (or the equivalent `Request::post` / `put` / `patch` / `delete` constructors). Comment lines are exempt so the chokepoint can be referenced from doc comments. As with k8s, this is the cheapest credible defense — `hyper` has no read-only client variant
- SQLite domain (`src/sqlite/`) is gated behind the **opt-in** `sqlite` cargo feature (not in `default`) — pulls in `rusqlite` with the `bundled` libsqlite3. Read-only enforcement is stronger than k8s's: `client::open_readonly` opens with `SQLITE_OPEN_READ_ONLY` (OS-level) and then sets `PRAGMA query_only=ON` (engine-level), so writes are rejected at two layers, not just by convention. The same chokepoint pattern is enforced by a grep test in `src/sqlite/client.rs` — every `rusqlite::Connection`, `Connection::open`, `.execute(`, and `.execute_batch(` must live in `client.rs`
- Prom domain (`src/prom/`) talks to a remote Prometheus or Alertmanager endpoint over HTTP + TLS via `ureq` (blocking, with rustls). Gated behind the `prom` cargo feature (on by default). Unlike k8s/lxc/docker, the prom domain stays **synchronous** — `ureq` is blocking and each command is one HTTP round trip, so adding `prom` does not pull `tokio` into the binary. Connection is `--url <URL>` or per-server env vars (`PROMETHEUS_URL`, `ALERTMANAGER_URL`); auto-discovery via a Kubernetes service selector + transparent port-forward is a planned follow-up
- Read-only enforcement for `prom` mirrors the k8s/lxc/docker pattern: every `ureq::Agent` construction and every mutation method (`.post(`/`.put(`/`.patch(`/`.delete(`) must live in `src/prom/client.rs`, enforced by a grep test. Prometheus's admin write endpoints under `/api/v1/admin/tsdb/*` (enabled with `--web.enable-admin-api`) make this a real guardrail. `PromClient::get_prom` unwraps the Prometheus `{status, data, errorType?, error?}` envelope and surfaces `status=error` as `anyhow::Error`; `PromClient::get_json` returns the raw body (used by Alertmanager v2 endpoints, which are envelope-less arrays). Both map HTTP 404 to `Ok(None)` so callers can produce sak's exit code 1 for "not found", mirroring `k8s::client::get_dyn` / `lxc::client::get_json` / `docker::client::get_json`. Shared output helpers (`emit_json`, `collapse_newlines`) live in `src/prom/output.rs`; the duration parser shared by `query-range` and `histogram` lives in `src/prom/duration.rs`
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
- `sak cert expiring` exit codes are inverted from every other sak command: exit 0 = nothing matched (healthy), exit 1 = at least one cert is within the window (alert). This is intentional — it makes `if sak cert expiring; then …; fi` natural and matches `grep`'s convention. If you cargo-cult an exit-code check from another sak command, double-check direction
- `sak cert from-yaml --path` uses the same dot-notation/JSON-Pointer parser as `sak json query` / `sak config query`. The dot parser splits on `.`, so for keys that themselves contain dots (e.g. Kubernetes Secrets' `tls.crt` / `ca.crt` data keys) you must use JSON Pointer: `--path /data/tls.crt`, not `--path .data.tls.crt`. Inline tests in `src/cert/from_yaml.rs` pin both syntaxes
- `cert::extract_ders` tries PEM → base64-wrapped PEM → raw DER in that order. Random binary that happens to ASN.1-parse as a SEQUENCE will silently come back as a "DER cert" until `parse_cert` fails further downstream. If you add a new branch (e.g. PKCS#7), put it *before* the raw-DER fallback so the more specific format wins
- `sak cert` test fixture (`src/cert/testdata/sak-test.pem`) has hardcoded NotBefore=2026-01-01 and NotAfter=2036-01-01. If those dates ever drift past the test expectations (or once 2036 approaches), regenerate with `openssl req -x509 -newkey rsa:2048 -nodes -subj /CN=sak-test -addext subjectAltName=DNS:sak-test.invalid -addext keyUsage=digitalSignature,keyEncipherment -not_before <YYYYMMDD>000000Z -not_after <YYYYMMDD>000000Z -keyout /dev/null -out src/cert/testdata/sak-test.pem` and update the assertion in `cert::tests::parse_test_cert_fields`
- `sak talos` shells out to `talosctl` and inherits its connection idiosyncrasies: talosconfig client cert expiry is a *frequent* failure mode (the talosconfig admin cert defaults to 1 year and quietly stops working when it expires). When `sak talos certs` returns exit 1 with no output, suspect an expired client cert before assuming the cluster is unreachable — verify with `sak cert from-yaml ~/.talos/config --path /contexts/<name>/crt --field not_after`. The error from `talosctl` in this state is misleading: `error reading server preface: remote error: tls: expired certificate` makes it sound like the *server's* TLS cert expired, but it's actually the *client's* mTLS cert being rejected
- `sak talos certs` cert path list is hardcoded in `src/talos/certs.rs::CERT_PATHS` and absent paths are silently skipped (workers don't have control-plane paths, etcd-less setups don't have etcd certs, etc.). When extending the list, verify against `pkg/machinery/constants/constants.go` in the upstream Talos source for the current release. Don't add paths that aren't certs (e.g. `kubeconfig` files) — they'd just fail PEM parsing and get dropped, but they pollute the per-node round-trip count
- `sak talos read` has two output modes that differ in byte fidelity: single-node (`--node <ip>` resolves to exactly one node) writes raw bytes to stdout, suitable for piping binary content through `sak cert inspect`. Multi-node mode (the default fan-out) prefixes each section with `### node=<ip>` and runs the body through `String::from_utf8_lossy`, which is *not* byte-faithful for binary files. If you need bytes from multiple nodes, script around the per-node single-node form
- K8s `get_dyn` returns `Result<Option<DynamicObject>>` — apiserver 404s map to `Ok(None)` so callers can produce sak's exit code 1 for "not found" without losing the ability to surface other errors as exit code 2. Don't unwrap it unconditionally
- K8s `discovery::resolve` returns `(ApiResource, ApiCapabilities)`; cluster-scoped vs namespaced enforcement happens at the command layer by inspecting `caps.scope` *before* the list/get call (cluster-scoped + `--namespace` should be a hard error)
- Adding new optional dependencies for the `k8s` feature requires both declaring them with `optional = true` *and* adding the `dep:<name>` to the `k8s = [...]` feature list in `Cargo.toml`. `kube` does not re-export the `http::Request` types needed by `client::request_text`, so `http` is its own gated dep
- The `sqlite` feature uses `rusqlite` with the `bundled` cargo feature, which compiles libsqlite3 from C source. The first build is noticeably slower (libsqlite3 is a few MB of C), but there is no system `libsqlite3` runtime dependency — the binary is self-contained. `sqlite` is on by default; pass `--no-default-features` to opt out
- LXC socket discovery probes `LXD_SOCKET` first, then `/var/snap/lxd/common/lxd/unix.socket`, `/var/lib/lxd/unix.socket`, `/var/lib/incus/unix.sock` in that order. The first existing path wins — if you're testing against Incus on a host that also has a stale LXD snap socket file, set `LXD_SOCKET` explicitly rather than relying on discovery
- Docker socket discovery probes `DOCKER_HOST` first, then `/var/run/docker.sock`, then `$HOME/.docker/run/docker.sock` (recent Docker Desktop on macOS / rootless Linux). `$HOME` is resolved by the caller (`std::env::home_dir` is unstable) and an unset/empty `$HOME` cleanly skips the user-scoped probe
- `DOCKER_HOST` is **unix-only** in v1 — `parse_docker_host` accepts `unix:///path` and bare paths but rejects `tcp://` (and any other scheme) with a clear error. TCP transport needs cert handling that is out of scope for the foundation; if you need it, file an issue rather than papering over the rejection
- LXD's REST API is project-scoped: `/1.0/instances` returns instances in the *default* project unless you append `?project=<name>` (or `?all-projects=true` on newer LXD). Commands that don't pass a project flag are implicitly operating against `default` — keep this in mind when a user reports "missing" instances on a multi-project host
- `lxc::client::LxcClient::get_json` and `docker::client::DockerClient::get_json` both return `Result<Option<serde_json::Value>>` — apiserver/daemon 404s map to `Ok(None)` so callers can produce sak's exit code 1 for "not found" without losing the ability to surface other errors as exit code 2. This mirrors `k8s::client::get_dyn`; don't unwrap unconditionally
- SQLite `PRAGMA` results can contain embedded newlines (notably `integrity_check` on a problem database), so `sak sqlite info` runs every PRAGMA value through a sanitizer that replaces `\n` / `\r` / `\t` with spaces before emission. Without that step, a multi-line PRAGMA value would shred the `key<TAB>value` line contract. SQLite also has a legacy DQ-as-string-literal quirk where unrecognized double-quoted identifiers silently become string literals — `sak sqlite dump` defends against typo'd `--order-by` columns by pre-validating them against `PRAGMA table_info` rather than relying on the SELECT to error out
- `PromClient::get_json` reads the response body via `into_reader()` rather than `into_string()`. ureq's `into_string()` caps at 10 MiB and `/api/v1/targets` on a real cluster (with the full `discoveredLabels` set per target) exceeds that. Input is otherwise unbounded — consistent with the kube/docker/lxc clients, which also buffer whole API responses; `--limit` bounds *output*, not the response body
- The prom domain is split into two URL env vars: `PROMETHEUS_URL` for `sak prom alerts|query|query-range|histogram|targets|rules` and `ALERTMANAGER_URL` for `sak prom am alerts|silences`. Both are overridable per-command with `--url`, so a single shell can target both servers without re-exporting. `resolve_endpoint` takes the env-var name as a parameter; its `_inner` form takes the env value too, so tests avoid the `unsafe std::env::set_var` dance under Rust 2024
- `PromClient::get_prom` unwraps the Prometheus `{status, data, errorType?, error?}` envelope; for Alertmanager v2 endpoints (which return JSON arrays with no envelope) use `get_json` directly. A new command targeting `/api/v1/*` should reach for `get_prom`; anything under `/api/v2/*` should use `get_json`
- `sak prom histogram` parses `le` labels via `f64::from_str`, which accepts `"+Inf"` as `f64::INFINITY` — sorting buckets by the parsed value naturally places `+Inf` last. The `le` unit (raw vs duration vs bytes) is auto-detected from the metric name suffix (`_seconds` → duration, `_bytes` → bytes, else raw); pass `--unit` to override. Negative `delta` values in the output are a *signal*, not a bug — they happen when `sum by (le)` aggregates heterogeneous bucket layouts (e.g. mixed apiserver versions)
- Alertmanager silence matchers default `isEqual` to `true` when the field is absent (older AM releases don't emit it; the v2 schema documents the default). `format_matchers` honors that default, so a regex-only matcher renders as `=~` not `!~`. Operator picked from `(isEqual, isRegex)` — `(true, false)` → `=`, `(true, true)` → `=~`, `(false, false)` → `!=`, `(false, true)` → `!~`
