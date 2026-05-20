use super::claude_code::classify;

fn blocks(cmd: &str) -> bool {
    classify(cmd).is_some()
}

fn allows(cmd: &str) -> bool {
    classify(cmd).is_none()
}

// ── git read-only ─────────────────────────────────────────────

#[test]
fn git_status_blocks() {
    assert!(blocks("git status"));
}

#[test]
fn git_diff_blocks() {
    assert!(blocks("git diff --staged"));
}

#[test]
fn git_log_blocks() {
    assert!(blocks("git log --oneline -n 10"));
}

#[test]
fn git_show_blocks() {
    assert!(blocks("git show HEAD"));
}

#[test]
fn git_blame_blocks() {
    assert!(blocks("git blame src/main.rs"));
}

#[test]
fn git_shortlog_blocks() {
    assert!(blocks("git shortlog -sn"));
}

#[test]
fn git_branch_list_blocks() {
    assert!(blocks("git branch"));
    assert!(blocks("git branch -a"));
    assert!(blocks("git branch --all"));
    assert!(blocks("git branch -r"));
}

#[test]
fn git_branch_modify_allows() {
    assert!(allows("git branch -D feature/old"));
    assert!(allows("git branch -m old-name new-name"));
    assert!(allows("git branch new-branch"));
}

#[test]
fn git_tag_list_blocks() {
    assert!(blocks("git tag"));
    assert!(blocks("git tag -l"));
    assert!(blocks("git tag --list"));
    assert!(blocks("git tag --sort=-creatordate"));
}

#[test]
fn git_tag_modify_allows() {
    assert!(allows("git tag -a v1.0 -m hi"));
    assert!(allows("git tag v1.0"));
    assert!(allows("git tag -d v0.9"));
}

#[test]
fn git_remote_list_blocks() {
    assert!(blocks("git remote"));
    assert!(blocks("git remote -v"));
    assert!(blocks("git remote show origin"));
    assert!(blocks("git remote get-url origin"));
}

#[test]
fn git_remote_modify_allows() {
    assert!(allows("git remote add origin url"));
    assert!(allows("git remote remove origin"));
    assert!(allows("git remote set-url origin url"));
}

#[test]
fn git_stash_list_blocks() {
    assert!(blocks("git stash list"));
}

#[test]
fn git_stash_other_allows() {
    assert!(allows("git stash"));
    assert!(allows("git stash push"));
    assert!(allows("git stash pop"));
}

#[test]
fn git_writes_allow() {
    assert!(allows("git commit -m foo"));
    assert!(allows("git add ."));
    assert!(allows("git push origin main"));
    assert!(allows("git pull"));
    assert!(allows("git fetch"));
    assert!(allows("git checkout main"));
    assert!(allows("git switch main"));
    assert!(allows("git rebase main"));
    assert!(allows("git merge feature"));
    assert!(allows("git reset --hard"));
    assert!(allows("git cherry-pick abc123"));
    assert!(allows("git revert HEAD"));
    assert!(allows("git clone https://example.com/r.git"));
    assert!(allows("git init"));
    assert!(allows("git config user.name foo"));
}

#[test]
fn git_with_global_flags() {
    assert!(blocks("git -C /tmp status"));
    assert!(blocks("git --git-dir /tmp/.git log"));
    assert!(blocks("git -C /tmp -c color.ui=false status"));
}

// ── kubectl ───────────────────────────────────────────────────

#[test]
fn kubectl_reads_block() {
    assert!(blocks("kubectl get pods"));
    assert!(blocks("kubectl describe deploy foo"));
    assert!(blocks("kubectl logs pod-x"));
    assert!(blocks("kubectl events -A"));
    assert!(blocks("kubectl api-resources"));
    assert!(blocks("kubectl explain pod"));
    assert!(blocks("kubectl config get-contexts"));
}

#[test]
fn kubectl_writes_allow() {
    assert!(allows("kubectl apply -f x.yaml"));
    assert!(allows("kubectl delete pod foo"));
    assert!(allows("kubectl edit deploy bar"));
    assert!(allows("kubectl exec -it pod -- sh"));
    assert!(allows("kubectl port-forward svc/x 8080:80"));
    assert!(allows("kubectl rollout restart deploy/foo"));
    // Not yet shadowed by a sak command — passes through.
    assert!(allows("kubectl top pods"));
    assert!(allows("kubectl version"));
}

// ── talosctl ──────────────────────────────────────────────────

#[test]
fn talosctl_reads_block() {
    assert!(blocks("talosctl get members --node 1.2.3.4"));
    assert!(blocks("talosctl read /etc/os-release"));
}

#[test]
fn talosctl_writes_allow() {
    assert!(allows("talosctl reboot"));
    assert!(allows("talosctl apply-config -f cfg.yaml"));
    assert!(allows("talosctl version"));
}

// ── docker ────────────────────────────────────────────────────

#[test]
fn docker_reads_block() {
    assert!(blocks("docker ps"));
    assert!(blocks("docker images"));
    assert!(blocks("docker inspect foo"));
}

#[test]
fn docker_writes_allow() {
    assert!(allows("docker run hello-world"));
    assert!(allows("docker build ."));
    assert!(allows("docker exec -it foo sh"));
    assert!(allows("docker rm foo"));
    // Not yet shadowed.
    assert!(allows("docker logs foo"));
    assert!(allows("docker stats"));
}

// ── lxc / incus ───────────────────────────────────────────────

#[test]
fn lxc_reads_block() {
    assert!(blocks("lxc list"));
    assert!(blocks("lxc info my-ct"));
    assert!(blocks("lxc config show my-ct"));
    assert!(blocks("lxc image list"));
    assert!(blocks("lxc image ls"));
    assert!(blocks("incus list"));
    assert!(blocks("incus info my-ct"));
}

#[test]
fn lxc_writes_allow() {
    assert!(allows("lxc launch ubuntu my-ct"));
    assert!(allows("lxc start my-ct"));
    assert!(allows("lxc stop my-ct"));
    assert!(allows("lxc exec my-ct -- sh"));
    // Not yet shadowed.
    assert!(allows("lxc storage list"));
    assert!(allows("lxc network list"));
}

// ── gh ────────────────────────────────────────────────────────

#[test]
fn gh_api_get_blocks() {
    assert!(blocks("gh api repos/cli/cli"));
    assert!(blocks("gh api 'repos/cli/cli/issues?state=open'"));
    // Explicit GET (redundant but legal) still redirects.
    assert!(blocks("gh api repos/cli/cli -X GET"));
    assert!(blocks("gh api repos/cli/cli --method get"));
    assert!(blocks("gh api repos/cli/cli -XGET"));
    assert!(blocks("gh api repos/cli/cli --method=GET"));
    // GraphQL queries are conventionally `gh api graphql` with no -X.
    assert!(blocks("gh api graphql -f query='{ viewer { login } }'"));
}

#[test]
fn gh_api_non_get_allows() {
    // A non-GET method is a write `sak gh` can't do — pass it through.
    assert!(allows("gh api repos/cli/cli -X POST"));
    assert!(allows("gh api repos/cli/cli --method DELETE"));
    assert!(allows("gh api repos/cli/cli -XPATCH"));
    assert!(allows("gh api repos/cli/cli --method=PUT"));
}

#[test]
fn gh_pr_list_blocks() {
    assert!(blocks("gh pr list"));
    assert!(blocks("gh pr list --state all --limit 50"));
    assert!(blocks("gh pr list --repo cli/cli --author octocat"));
}

#[test]
fn gh_pr_view_blocks() {
    assert!(blocks("gh pr view 13468"));
    assert!(blocks("gh pr view 13468 --repo cli/cli"));
    assert!(blocks("gh pr view https://github.com/cli/cli/pull/1"));
}

#[test]
fn gh_issue_list_blocks() {
    assert!(blocks("gh issue list"));
    assert!(blocks("gh issue list --state all --assignee octocat"));
    assert!(blocks("gh issue list --repo cli/cli --milestone v1"));
}

#[test]
fn gh_issue_view_blocks() {
    assert!(blocks("gh issue view 13464"));
    assert!(blocks("gh issue view 13464 --repo cli/cli"));
    assert!(blocks("gh issue view https://github.com/cli/cli/issues/1"));
}

#[test]
fn gh_run_list_blocks() {
    assert!(blocks("gh run list"));
    assert!(blocks("gh run list --workflow ci.yml --branch main"));
    assert!(blocks("gh run list --status completed --limit 50"));
}

#[test]
fn gh_run_view_blocks() {
    assert!(blocks("gh run view 26189297400"));
    assert!(blocks("gh run view 26189297400 --log-failed"));
    assert!(blocks(
        "gh run view 26189297400 --repo cli/cli --job 999 --log"
    ));
}

#[test]
fn gh_release_list_blocks() {
    assert!(blocks("gh release list"));
    assert!(blocks("gh release list --repo cli/cli --limit 50"));
    assert!(blocks("gh release list --exclude-drafts"));
}

#[test]
fn gh_release_view_blocks() {
    assert!(blocks("gh release view"));
    assert!(blocks("gh release view v2.92.0"));
    assert!(blocks("gh release view v2.92.0 --repo cli/cli"));
}

#[test]
fn gh_workflow_list_blocks() {
    assert!(blocks("gh workflow list"));
    assert!(blocks("gh workflow list --all"));
    assert!(blocks("gh workflow list --repo cli/cli --limit 50"));
}

#[test]
fn gh_repo_view_blocks() {
    assert!(blocks("gh repo view"));
    assert!(blocks("gh repo view cli/cli"));
    assert!(blocks("gh repo view --json nameWithOwner"));
}

#[test]
fn gh_mutations_and_unshadowed_allow() {
    // Mutating verbs are not redirected.
    assert!(allows("gh pr merge 123"));
    assert!(allows("gh issue close 5"));
    assert!(allows("gh repo create my/repo"));
    assert!(allows("gh run rerun 42"));
    assert!(allows("gh run cancel 42"));
    assert!(allows("gh release create v1.0"));
    assert!(allows("gh workflow run deploy.yml"));
    assert!(allows("gh workflow disable ci.yml"));
    assert!(allows("gh repo clone cli/cli"));
    // `pr` reads other than `list`/`view` have no command yet.
    assert!(allows("gh pr checks 123"));
    // `workflow` reads other than `list` have no command yet.
    assert!(allows("gh workflow view ci.yml"));
    // `repo list` has no command yet (only `repo view` is shadowed).
    assert!(allows("gh repo list"));
}

// ── filesystem readers ────────────────────────────────────────

#[test]
fn cat_with_file_blocks() {
    assert!(blocks("cat /etc/passwd"));
    assert!(blocks("cat -n file.txt"));
}

#[test]
fn cat_heredoc_allows() {
    assert!(allows("cat <<EOF\nhello\nEOF"));
    assert!(allows("cat <<-EOF\nhello\nEOF"));
}

#[test]
fn cat_no_args_allows() {
    // cat alone reads stdin; nothing to redirect.
    assert!(allows("cat"));
}

#[test]
fn head_tail_with_file_block() {
    assert!(blocks("head -20 file.txt"));
    assert!(blocks("tail -f log.txt"));
}

#[test]
fn grep_recursive_blocks() {
    assert!(blocks("grep -r foo ."));
    assert!(blocks("grep -R foo ."));
    assert!(blocks("grep --recursive foo ."));
    assert!(blocks("grep -rn foo ."));
}

#[test]
fn grep_pattern_and_file_blocks() {
    assert!(blocks("grep foo file.txt"));
    assert!(blocks("grep -i foo file.txt"));
}

#[test]
fn grep_stdin_allows() {
    assert!(allows("echo hi | grep h"));
    assert!(allows("grep foo"));
}

#[test]
fn ripgrep_blocks() {
    assert!(blocks("rg foo"));
    assert!(blocks("ripgrep foo"));
    assert!(blocks("rg foo src/"));
}

#[test]
fn find_search_blocks() {
    assert!(blocks("find . -name foo.rs"));
    assert!(blocks("find /tmp -type f"));
}

#[test]
fn find_action_allows() {
    assert!(allows("find . -name foo.tmp -delete"));
    assert!(allows("find . -type f -exec rm {} ;"));
    assert!(allows("find . -name old -ok rm {} ;"));
}

// ── parsers (jq/yq) ───────────────────────────────────────────

#[test]
fn jq_file_blocks() {
    assert!(blocks("jq .name pkg.json"));
    assert!(blocks("jq -r .name pkg.json"));
}

#[test]
fn jq_stdin_allows() {
    assert!(allows("echo foo | jq ."));
    assert!(allows("jq ."));
}

#[test]
fn yq_file_blocks() {
    assert!(blocks("yq .name pkg.yaml"));
    assert!(blocks("tomlq .package.name Cargo.toml"));
}

#[test]
fn yq_stdin_allows() {
    assert!(allows("echo foo | yq ."));
}

#[test]
fn plistutil_blocks() {
    assert!(blocks("plistutil -i Info.plist"));
}

// ── openssl x509 ──────────────────────────────────────────────

#[test]
fn openssl_x509_blocks() {
    assert!(blocks("openssl x509 -in cert.pem -text"));
    assert!(blocks("openssl x509 -in cert.pem -noout"));
}

#[test]
fn openssl_other_allows() {
    assert!(allows("openssl genrsa 2048"));
    assert!(allows("openssl req -x509 -newkey rsa:2048"));
    assert!(allows("openssl s_client -connect host:443"));
}

// ── sqlite3 ───────────────────────────────────────────────────

#[test]
fn sqlite_reads_block() {
    assert!(blocks("sqlite3 db.sqlite .tables"));
    assert!(blocks("sqlite3 db.sqlite .schema"));
    assert!(blocks("sqlite3 db.sqlite .dump users"));
    assert!(blocks("sqlite3 db.sqlite \"SELECT * FROM users\""));
}

#[test]
fn sqlite_writes_allow() {
    assert!(allows("sqlite3 db.sqlite \"INSERT INTO users VALUES (1)\""));
    assert!(allows("sqlite3 db.sqlite \"CREATE TABLE x (a INT)\""));
}

// ── pipeline / chaining / env prefixes ────────────────────────

#[test]
fn and_chain_catches() {
    assert!(blocks("echo hi && git log"));
    assert!(blocks("make foo && cat /etc/passwd"));
}

#[test]
fn semicolon_chain_catches() {
    assert!(blocks("echo hi; git status"));
}

#[test]
fn pipe_does_not_split_inside_quotes() {
    // `||` inside the quoted echo arg should NOT split the pipeline.
    // (The standalone string isn't a recognized cmd, so it would pass anyway,
    // but we're verifying the splitter behavior.)
    assert!(allows("echo 'a || b'"));
    // git log lives after a real `&&` so it should still be caught.
    assert!(blocks("echo 'a && b' && git log"));
}

#[test]
fn env_var_prefix_caught() {
    assert!(blocks("DEBUG=1 git status"));
    assert!(blocks("FOO=bar BAZ=qux cat /etc/passwd"));
}

#[test]
fn absolute_path_caught() {
    assert!(blocks("/usr/bin/git status"));
    assert!(blocks("/bin/cat /etc/passwd"));
}

// ── empty / weird input ───────────────────────────────────────

#[test]
fn empty_command_allows() {
    assert!(allows(""));
    assert!(allows("   "));
}

#[test]
fn unknown_command_allows() {
    assert!(allows("rustc --version"));
    assert!(allows("cargo build"));
    assert!(allows("make test"));
}
