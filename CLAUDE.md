# Claude Code notes for SAK

Shared repo guidance lives in [AGENTS.md](AGENTS.md) — read it first. It is
authoritative for builds, tests, architecture, exit codes, and conventions.
This file holds only Claude Code-specific notes.

## Hook setup

Two pieces of `~/.claude/settings.json` make sak the obvious-and-only path
for every read-only operation (full docs under "Using SAK from an LLM agent"
in [README.md](README.md)):

- Auto-approve sak: `permissions.allow` gets `Bash(sak:*)` (every operation
  is read-only, so blanket-allowing it is safe).
- One `PreToolUse` hook on matcher `Bash` running `sak hook claude-code`
  (timeout 5). It reads the `PreToolUse` JSON payload from stdin, classifies
  the about-to-run command, and exits 2 with a stderr message naming the sak
  equivalent when a read-only CLI should have been sak (that message is fed
  back to the model, which then auto-corrects); otherwise it exits 0 and the
  command runs.

Debug the rule set from the shell with
`sak hook claude-code --check '<command>'` (no stdin payload needed).

## `sak hook claude-code` internals

- The `hook claude-code` classifier doesn't just split on pipeline operators — it recurses through shell composition (`classify_depth` in [src/hook/claude_code.rs](src/hook/claude_code.rs)): command substitutions (`$(…)`, backticks), subshell / brace groups (`(…)`, `{ …; }`), `sh -c '…'` scripts, command-prefix wrappers (`sudo`/`env`/`xargs`/`timeout`/`nice`/…), and `find … -exec …` clauses. So a `HookRule` you add fires even when the command is wrapped — but two non-obvious shapes feed the inner command a *synthetic* file operand so file-requiring guards still match: `xargs cat` appends `XARGS_OPERAND` (xargs supplies paths from stdin), and `find … -exec grep foo {} +` keeps the `{}` token (it stands in for the matched path). If you write a guard that counts positionals to decide "reads a file", remember it'll see that synthetic operand under those two wrappers
- Before splitting, every level runs the lexical pre-passes in [src/hook/shell_text.rs](src/hook/shell_text.rs): `strip_heredocs` removes heredoc bodies (a quoted-delimiter body vanishes; an unquoted one leaves only its `$(…)`/backtick substitutions), joins backslash-newline continuations and drops `#` comments; then `strip_redirections` runs per segment, dropping output redirections with their targets and turning `< file` into a plain operand. Newlines are segment separators, and the `&` in `2>&1` / `&>` is not. Consequence for guards: they never see `>`, `out.txt`, `<<EOF` or a heredoc body in their args — `cat > notes.md <<'EOF' … EOF` reaches the `cat` guard as a bare `cat`, and `cat < f` as `cat f`
- The `SAK_HOOK_BYPASS=1` escape hatch has **two** independent paths, and the documented one is not the environment. The hook runs as its own process, so `SAK_HOOK_BYPASS=1 git status` never puts anything in *our* environment — the assignment is just the first token of the command string the harness hands us, and `strip_env_assignments` would otherwise drop it before matching (this was the bug in sak-llm-hook-bypass-ignored-wxyi: the rejection message told callers to set a variable that could never work). `bypass_requested` in [src/hook/claude_code.rs](src/hook/claude_code.rs) scans the command string for the marker, and `run` separately honors a real exported `SAK_HOOK_BYPASS`. Both go through `is_bypass_value` so they can't drift on what counts as "set". Deliberate non-shell semantics: a marker *anywhere* in the command (leading assignment, wrapper operand, inside `sh -c`) disables the hook for the **whole** command, later pipeline segments included — shell scoping would confine it to one segment, which is useless when the blocking read is `… | wc -l`
