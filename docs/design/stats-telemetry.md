# Design: Opt-in anonymous usage statistics for `sak`

> Tracking: epic `sak-llm-stats-telemetry-f91` and its child issues `.1`–`.10`
> (`br show sak-llm-stats-telemetry-f91`).

## Context

`sak` is a strictly read-only, stateless CLI run by LLM agents over sensitive inputs
(kubeconfigs, certs, secrets, SQLite query text, file paths). Today it writes nothing
to disk and makes network calls only to user-specified endpoints — invariants enforced
per-domain by chokepoint modules + grep tests.

We want anonymous usage statistics to understand which commands get used. This is a
deliberate, contained departure from "no side effects," so the design is built to keep
the existing guarantees legible and tested rather than quietly eroded.

**Decided shape:**
- **Opt-in, OFF by default**, and behind a non-default `telemetry` cargo feature — so it's
  opt-in at both build and runtime layers.
- **Local JSONL buffer** + **optional** remote upload that only happens on an explicit
  `sak stats report` (never automatic, never per-command).
- **Collector server** ships in this repo as a **feature-gated second binary**
  (`sak-stats-server`, behind a non-default `stats-server` feature) in the same crate —
  not a workspace, not a separate repo. It shares the exact `Event` source file with the
  client via a `#[path]` module include, so the wire format cannot drift and a reviewer
  sees client + server schema in one place. Default `cargo install sak` builds only the
  `sak` CLI.
- **Config is env-vars-only** — no persistent config file. Matches the `SAK_HOOK_BYPASS`
  precedent and keeps sak's only new state the opt-in buffer itself.
- **Event contents:** domain + subcommand, flag *names only* (no values, no positionals),
  a coarse exit outcome, a unix timestamp, and the sak version. No paths, no secrets.
- **Exit code:** coarse — `0` for the `Ok` arm, `2` for the `Err` arm (no 0-vs-1 split).
  Chosen to avoid a repo-wide `ExitCode`→`u8` refactor; `ExitCode` is opaque so the true
  0/1 distinction isn't recoverable at the dispatch boundary without changing every
  domain's `run()` signature.

## Key existing code to reuse
- Dispatch chokepoint: [main.rs:247-281](../../src/main.rs#L247-L281) — single `match` over
  the `Command` enum; exit handling at [main.rs:274-280](../../src/main.rs#L274-L280).
- `$HOME`-via-env resolution pattern (no `dirs` dep): [docker/client.rs:133-147](../../src/docker/client.rs#L133-L147).
- Endpoint flag→env precedence + param-injected testable resolver: `resolve_endpoint_inner` in [prom/client.rs:151-158](../../src/prom/client.rs#L151).
- Grep-test chokepoint pattern: `FORBIDDEN_TOKENS` in [prom/client.rs:187-198](../../src/prom/client.rs#L187), helper `assert_no_forbidden_tokens` in [test_support.rs:22](../../src/test_support.rs#L22).
- Domain dispatch template: [prom/mod.rs:51-118](../../src/prom/mod.rs#L51-L118).
- Opt-in env predicate precedent: `SAK_HOOK_BYPASS == "1"` in [hook/claude_code.rs:83](../../src/hook/claude_code.rs#L83).
- `serde_json` is already a core dep; `ureq` (blocking HTTP + rustls) already exists, optional, pulled by `prom`.

## Design

### Feature & deps (`Cargo.toml`)
- Add `telemetry = ["dep:ureq"]` (NOT in `default`). Reuses the same `ureq` the `prom`
  domain uses (client-side upload), so no new HTTP stack; shared when both features are on.
- Add `stats-server = ["dep:tiny_http"]` (NOT in `default`). `tiny_http` is a small
  synchronous HTTP server — minimal audit surface, no tokio, fits sak's lean-dep posture.
  (Alternative: reuse `hyper` server-side, but that pulls the async stack into a binary
  that's otherwise trivial; `tiny_http` keeps the collector readable.)
- Bump `version` **minor** (new domain + new bin), e.g. `0.16.x` → `0.17.0`.

### Runtime hook (`src/main.rs`, all under `#[cfg(feature = "telemetry")]`)
- Add `mod stats;`, a `Command::Stats(stats::StatsCommand)` enum variant, a dispatch arm,
  and a quick-start block (mirror how `prom` is gated at [main.rs:268](../../src/main.rs#L268)).
- In `main()`: build a `Pending` event *before* dispatch (captures domain+subcommand from
  the parsed `Command`, flag names from raw argv); after the existing `match result`,
  call `pending.record(ok_or_err)` where the outcome is `0` for `Ok`/`2` for `Err`.
  Keep the existing `match` returning `ExitCode` unchanged.
- `record()` is **best-effort and infallible** — every error discarded, never `eprintln`,
  never alters the returned `ExitCode`. This is the load-bearing invariant.

### New domain `src/stats/` (gated `#[cfg(feature = "telemetry")]`)
- `event.rs` — `Event` struct (`serde::Serialize`/`Deserialize`): `domain`, `subcommand`,
  `flags: Vec<String>`, `exit: u8`, `ts: i64` (unix secs via `SystemTime`),
  `version: String` (set from `env!("CARGO_PKG_VERSION")` at the call site, not baked into
  the struct, so the file stays self-contained). **Must depend only on `serde`/std** — no
  sak-internal imports and no `#[cfg(feature = ...)]` inside the file — because the server
  bin includes this same file via `#[path = "../stats/event.rs"]`. The `telemetry` gating
  happens at the `mod event;` declaration in `stats/mod.rs`, not in the file. Pure helpers:
  - `flag_names(args: &[String]) -> Vec<String>`: collect tokens starting with `-`, take
    the substring before any `=`, dedupe + sort. Values/positionals never start with `-`
    so they're naturally excluded (e.g. `-n 1-5` records `-n`, drops `1-5`). Documented
    edge case: a bare negative-number *value* like `-5` would be misrecorded — harmless
    (no secret leak), and no current sak command takes one.
  - `domain_subcommand(&Command) -> (&str, &str)`.
- `writer.rs` — **disk-write chokepoint** (the ONLY module crate-wide allowed to open files
  for writing). Pure `resolve_stats_path(file_env, xdg_env, home_env) -> Option<PathBuf>`
  with precedence `SAK_STATS_FILE` → `$XDG_STATE_HOME/sak/stats.jsonl` →
  `$HOME/.local/state/sak/stats.jsonl` → `None` (unset HOME ⇒ silent no-op).
  `is_enabled()` (true if `SAK_STATS=1` **or** `SAK_STATS_FILE` set). `append(&Event)`
  (`create_dir_all` + `OpenOptions::append`, one `write_all` of a serialized line + `\n`,
  all errors swallowed). `clear()` truncates the buffer.
- `uploader.rs` — **network chokepoint** (the ONLY `stats` module allowed `ureq::Agent` /
  `.post(`). POSTs the JSONL body to `--report-to` → `SAK_STATS_URL` (flag→env precedence).
- `show.rs` / `clear.rs` / `path.rs` — read+aggregate JSONL (counts per domain/subcommand,
  outcome split), truncate via `writer::clear`, print resolved path.
- `mod.rs` — `StatsCommand` enum + `run()` dispatch; defines `Pending` (capture/record).

Subcommands: `sak stats show` (read-only aggregate), `sak stats report [--report-to URL]`
(the only network egress), `sak stats clear`, `sak stats path`.

### Collector server (`src/bin/sak_stats_server.rs`, feature `stats-server`)
- Declared in `Cargo.toml` as `[[bin]] name = "sak-stats-server"`,
  `required-features = ["stats-server"]` so it's skipped unless explicitly built.
- Shares the schema with **zero drift** via `#[path = "../stats/event.rs"] mod event;` —
  the exact same source the client serializes. No workspace, no lib target (honors the
  "single crate, no lib split" rule literally; the only concession is one `#[path]` include).
- Minimal `tiny_http` loop: bind `--listen <addr>` (default `127.0.0.1:8787`), accept
  `POST`, read the JSONL body, validate each line parses as `event::Event` (reject the
  request on a malformed line), append accepted lines to `--store <path>` (default
  `./sak-stats.jsonl`), return `204`. Reject non-POST with `405`. v1 has no auth; note an
  optional `--token` shared-secret (compared against an `Authorization` header) as an easy
  follow-up — call this out but keep v1 simple.
- This binary legitimately writes to disk and binds a socket — it is the collector, **not**
  the read-only `sak` client. The read-only/disk-write/network grep tests below scope to the
  client surface and explicitly exempt this file (see enforcement).

### Read-only enforcement (the crux)
- **Disk-write grep test** in `writer.rs`: ban `OpenOptions`, `File::create`, `fs::write`,
  `fs::create_dir_all`, `.write_all(`, `set_len` from every other `src/stats/*.rs`.
- **Crate-wide disk-write test**: assert those tokens appear nowhere in `src/` outside
  `src/stats/writer.rs`, scoped to non-test code (whitelist the three existing `#[cfg(test)]`
  tempdir writes in [cert/expiring.rs:117](../../src/cert/expiring.rs#L117),
  [cert/from_kubeconfig.rs:191](../../src/cert/from_kubeconfig.rs#L191),
  [cert/from_yaml.rs:169](../../src/cert/from_yaml.rs#L169)) **and exempt
  `src/bin/sak_stats_server.rs`** (the collector — writing is its job; it's a separate
  binary, not part of the `sak` client). Makes "the `sak` client writes nothing but the
  opt-in stats buffer" a tested invariant.
- **Network grep test** in `uploader.rs`: copy the `FORBIDDEN_TOKENS` list from
  [prom/client.rs:187](../../src/prom/client.rs#L187), banning `ureq::Agent`/`.post(`/`.put(`/
  `.patch(`/`.delete(` from the other `src/stats/*.rs`. (The collector lives in `src/bin/`,
  outside this scan — its `tiny_http` socket bind is server-side and expected.)

### Env vars (consistent with `SAK_*` convention)
- `SAK_STATS=1` — master opt-in for local collection.
- `SAK_STATS_FILE=<path>` — override buffer path; setting it alone also opts in.
- `SAK_STATS_URL=<url>` — default upload endpoint for `sak stats report` (never auto-used).

### Docs
- `CLAUDE.md`: new Architecture bullet after the prom bullet stating telemetry is
  opt-in/off-by-default/non-default-feature, is the only disk-write and only
  non-user-directed egress path on the **client**, both confined to chokepoint modules with
  grep tests, and failures never affect exit code/stderr. Document the collector as a
  feature-gated second binary sharing `event.rs` by `#[path]` (so the "single crate, no
  workspace, no lib split" rule gets one stated, narrow concession). Carve out the explicit
  exception in the "strictly read-only" claims and the `about`/`long_about` at
  [main.rs:176-182](../../src/main.rs#L176-L182). Add build-matrix rows (incl. `--features
  stats-server`) and a Gotchas entry (coarse-exit decision + the infallible-record
  invariant + the `event.rs`-must-stay-self-contained constraint).
- `README.md`: add `sak stats show|report|clear|path` to the discovery list with a note
  that it's opt-in and how to enable.

### Agent hook
No change needed — `sak stats` is sak-native and shadows no external CLI read op the hook
covers ([hook/claude_code.rs:345-372](../../src/hook/claude_code.rs#L345)). Note "no hook
change" in the commit message per the CLAUDE.md checklist.

## Files
- **New:** `src/stats/{mod,event,writer,uploader,show,clear,path}.rs`,
  `src/bin/sak_stats_server.rs` (collector, `required-features = ["stats-server"]`)
- **Edit:** `Cargo.toml` (`telemetry` + `stats-server` features, `[[bin]]`, version),
  `src/main.rs` (mod/enum/dispatch/hook/quick-start/about),
  `src/test_support.rs` (crate-wide write test or a new top-level test module), `CLAUDE.md`, `README.md`

## Verification
1. `nix develop -c cargo fmt` and `cargo clippy --all-features --all-targets` clean.
2. Build matrix (use `nix develop -c`): default, `--no-default-features`,
   `--no-default-features --features telemetry`, `--no-default-features --features stats-server`
   (server bin compiles standalone, sharing `event.rs`), `--all-features` — all build + test clean.
3. Unit tests: `flag_names` (drops `1-5` after `-n`, splits `--flag=val`, dedupes/sorts,
   excludes positionals); `resolve_stats_path` precedence; `is_enabled` predicate; grep tests
   (disk-write + network + crate-wide) actually fail when a forbidden token is planted.
4. End-to-end manual:
   - Default off: run any `sak fs glob ...` with no env set → no file created at the resolved path (`sak stats path` shows where it would be).
   - Opt in: `SAK_STATS=1 sak fs glob '**/*.rs' .` then `SAK_STATS=1 sak git status` → `sak stats show` reports 2 events with correct domain/subcommand/flags and outcome; confirm no flag *values* or paths appear in `stats.jsonl`.
   - Error path: run a command that exits 2 (e.g. `SAK_STATS=1 sak prom alerts` with no URL) → event recorded with `exit: 2`.
   - Upload is manual only: confirm normal commands never make a network call.
   - End-to-end with the real collector: `cargo run --features stats-server --bin sak-stats-server -- --listen 127.0.0.1:8787 --store /tmp/collected.jsonl`, then `SAK_STATS_URL=http://127.0.0.1:8787 sak stats report` → the listener's store file contains the uploaded events and they round-trip through the shared `Event` schema (no parse errors).
