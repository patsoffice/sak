//! Lexical pre-passes the hook classifier runs before splitting a command
//! string into pipeline segments and tokens.
//!
//! The classifier's splitter and tokenizer only understand quotes, escapes
//! and the `| || && ; &` separators. Three other pieces of shell syntax
//! change what is a command and what is data, so they are normalized first:
//!
//! - **Heredocs** ([`strip_heredocs`]): a body is data fed to stdin, not
//!   commands. With a quoted delimiter (`<<'EOF'`) nothing in it runs; with
//!   an unquoted one only its `$(…)` / backtick substitutions do. Bodies are
//!   removed and those substitutions kept, so a doc written with
//!   `cat > notes.md <<'EOF'` can mention `git status` without tripping a
//!   rule, while `cat <<EOF … $(cat secret) … EOF` still does.
//! - **Line structure** (also [`strip_heredocs`], which has to walk lines
//!   anyway): backslash-newline continuations are joined and `#` comments
//!   dropped. Newlines themselves become separators in the splitter.
//! - **Redirections** ([`strip_redirections`]): an output target
//!   (`> notes.md`, `2>/dev/null`) is not a file the command reads, so it is
//!   dropped with its operator; an input redirection (`< file`) *is* a read
//!   of that file, so its target is kept as an ordinary operand.
//!
//! Like the rest of the classifier these are deliberately approximate
//! scanners, not a shell parser: on malformed input they fall back to
//! leaving text in place.

use super::claude_code::{backtick_span, balanced};

/// Shell metacharacters that end an unquoted word.
fn ends_word(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')')
}

/// Parse a shell word starting at `start`, honoring quotes and backslash
/// escapes. Returns the word with quoting removed, whether any quoting was
/// present, and the index just past the word.
fn parse_word(b: &[u8], start: usize) -> (String, bool, usize) {
    let mut word = Vec::new();
    let mut quoted = false;
    let mut i = start;
    while i < b.len() && !ends_word(b[i]) {
        match b[i] {
            q @ (b'\'' | b'"') => {
                quoted = true;
                i += 1;
                while i < b.len() && b[i] != q {
                    if q == b'"' && b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    word.push(b[i]);
                    i += 1;
                }
                i += 1; // closing quote (or past the end)
            }
            b'\\' if i + 1 < b.len() => {
                quoted = true;
                word.push(b[i + 1]);
                i += 2;
            }
            c => {
                word.push(c);
                i += 1;
            }
        }
    }
    (
        String::from_utf8_lossy(&word).into_owned(),
        quoted,
        i.min(b.len()),
    )
}

/// Skip spaces and tabs (not newlines) from `i`.
fn skip_blanks(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// Whether `i` starts a new word: start of input or preceded by whitespace.
fn at_word_start(b: &[u8], i: usize) -> bool {
    i == 0 || b[i - 1].is_ascii_whitespace()
}

/// One heredoc waiting for its body, which starts after the next newline.
struct Pending {
    delimiter: String,
    /// Any quoting on the delimiter: the body is literal, nothing expands.
    quoted: bool,
    /// `<<-`: leading tabs are stripped from body lines and the delimiter.
    strip_tabs: bool,
}

/// The `$(…)` and backtick substitutions in an unquoted heredoc body — the
/// only parts of it the shell executes. Quotes are literal in a body, so the
/// scan only honors backslash escapes outside the substitutions themselves.
fn body_substitutions(body: &str) -> Vec<String> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'$' if b.get(i + 1) == Some(&b'(') => match balanced(b, i + 1, b'(', b')') {
                Some((inner, end)) => {
                    // `$((…))` is arithmetic, not a command.
                    if !inner.starts_with('(') {
                        out.push(format!("$({inner})"));
                    }
                    i = end;
                }
                None => i += 1,
            },
            b'`' => match backtick_span(b, i) {
                Some((inner, end)) => {
                    out.push(format!("`{inner}`"));
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

/// Remove heredoc bodies, join line continuations and drop comments.
///
/// Each `<<WORD` / `<<-WORD` operator (with its delimiter word) is replaced by
/// a space; at the next unquoted newline the pending bodies are consumed up
/// to their delimiter lines. A quoted delimiter's body vanishes entirely; an
/// unquoted one's is replaced by its command substitutions, one per line, so
/// the classifier still sees what would run. A body without a delimiter line
/// runs to the end of the input, as in bash. Here-strings (`<<<`) and
/// arithmetic (`$((1<<2))`) are left for later passes.
pub(super) fn strip_heredocs(cmd: &str) -> String {
    let b = cmd.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut copied = 0;
    let mut pending: Vec<Pending> = Vec::new();
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0;

    while i < n {
        let c = b[i];
        if in_single {
            in_single = c != b'\'';
            i += 1;
            continue;
        }
        if in_double {
            if c == b'\\' && b.get(i + 1) == Some(&b'\n') {
                // Continuations join inside double quotes too.
                out.push_str(&cmd[copied..i]);
                copied = i + 2;
                i += 2;
                continue;
            }
            if c == b'\\' {
                i += 2;
                continue;
            }
            in_double = c != b'"';
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'\\' if b.get(i + 1) == Some(&b'\n') => {
                out.push_str(&cmd[copied..i]);
                copied = i + 2;
                i += 2;
                continue;
            }
            b'\\' => {
                i += 2;
                continue;
            }
            b'#' if at_word_start(b, i) => {
                out.push_str(&cmd[copied..i]);
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                copied = i;
                continue;
            }
            b'$' if b.get(i + 1) == Some(&b'(') && b.get(i + 2) == Some(&b'(') => {
                if let Some((_, end)) = balanced(b, i + 1, b'(', b')') {
                    i = end;
                    continue;
                }
            }
            // A here-string: skip it whole, or its second `<` would read as
            // a heredoc operator.
            b'<' if b[i..].starts_with(b"<<<") => {
                i += 3;
                continue;
            }
            b'<' if b.get(i + 1) == Some(&b'<') => {
                let mut j = i + 2;
                let strip_tabs = b.get(j) == Some(&b'-');
                if strip_tabs {
                    j += 1;
                }
                j = skip_blanks(b, j);
                let (delimiter, quoted, end) = parse_word(b, j);
                if !delimiter.is_empty() {
                    out.push_str(&cmd[copied..i]);
                    out.push(' ');
                    copied = end;
                    i = end;
                    pending.push(Pending {
                        delimiter,
                        quoted,
                        strip_tabs,
                    });
                    continue;
                }
            }
            b'\n' if !pending.is_empty() => {
                out.push_str(&cmd[copied..=i]);
                let mut pos = i + 1;
                for hd in pending.drain(..) {
                    let body_start = pos;
                    let mut body_end = n;
                    while pos < n {
                        let line_end = cmd[pos..].find('\n').map_or(n, |k| pos + k);
                        let mut line = &cmd[pos..line_end];
                        if hd.strip_tabs {
                            line = line.trim_start_matches('\t');
                        }
                        let next = (line_end + 1).min(n);
                        if line == hd.delimiter {
                            body_end = pos;
                            pos = next;
                            break;
                        }
                        pos = next;
                    }
                    if !hd.quoted {
                        for sub in body_substitutions(&cmd[body_start..body_end.min(pos)]) {
                            out.push_str(&sub);
                            out.push('\n');
                        }
                    }
                }
                copied = pos;
                i = pos;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&cmd[copied.min(n)..]);
    out
}

/// Drop output redirections and keep input-redirection targets as operands,
/// within one pipeline segment.
///
/// `> f`, `>> f`, `>| f`, `&> f`, `2> f`, `2>&1`, `>&2`, `<&0`, here-strings
/// (`<<< word`) and any heredoc operator a caller left in place are removed
/// with their target, since none of those name a file the command reads.
/// `< f` and `<> f` become plain ` f`: `cat < f` reads `f` exactly like
/// `cat f`. Process substitutions (`<(…)`, `>(…)`) are left for
/// `extract_nested`.
pub(super) fn strip_redirections(part: &str) -> String {
    let b = part.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut copied = 0;
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0;

    while i < n {
        let c = b[i];
        if in_single {
            in_single = c != b'\'';
            i += 1;
            continue;
        }
        if in_double {
            if c == b'\\' {
                i += 2;
                continue;
            }
            in_double = c != b'"';
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'\\' => {
                i += 2;
                continue;
            }
            b'<' | b'>' if b.get(i + 1) != Some(&b'(') => {
                // An fd number (`2>`) or `&` (`&>`) directly before the
                // operator, at the start of a word, belongs to it.
                let mut op_start = i;
                while op_start > copied && b[op_start - 1].is_ascii_digit() {
                    op_start -= 1;
                }
                if op_start == i && op_start > copied && b[op_start - 1] == b'&' {
                    op_start -= 1;
                }
                if !at_word_start(b, op_start) {
                    op_start = i;
                }
                let rest = &b[i..];
                let (op_len, input) = if rest.starts_with(b"<<<") || rest.starts_with(b"<<-") {
                    (3, false)
                } else if rest.starts_with(b"<<") {
                    (2, false)
                } else if rest.starts_with(b"<>") {
                    (2, true)
                } else if rest.starts_with(b"<&")
                    || rest.starts_with(b">&")
                    || rest.starts_with(b">>")
                    || rest.starts_with(b">|")
                {
                    (2, false)
                } else {
                    (1, c == b'<')
                };
                let word_start = skip_blanks(b, i + op_len);
                let (_, _, word_end) = parse_word(b, word_start);
                out.push_str(&part[copied..op_start]);
                out.push(' ');
                if input && word_end > word_start {
                    out.push_str(&part[word_start..word_end]);
                    out.push(' ');
                }
                copied = word_end;
                i = word_end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&part[copied.min(n)..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn squash(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn quoted_heredoc_body_is_removed() {
        let cmd = "cat > notes.md <<'EOF'\nsee `git status`; cat x\nEOF\necho done";
        assert_eq!(squash(&strip_heredocs(cmd)), "cat > notes.md echo done");
        // Every quoting style of the delimiter counts as quoted.
        for op in ["<<\"EOF\"", "<<\\EOF", "<<E'O'F"] {
            let cmd = format!("cat {op}\n$(cat secret)\nEOF\n");
            assert_eq!(squash(&strip_heredocs(&cmd)), "cat", "{op}");
        }
    }

    #[test]
    fn unquoted_heredoc_keeps_only_substitutions() {
        let cmd = "cat <<EOF\ndon't $(cat a.txt) and `head b` or $((1+2))\nEOF\n";
        assert_eq!(squash(&strip_heredocs(cmd)), "cat $(cat a.txt) `head b`");
    }

    #[test]
    fn dash_heredoc_strips_tabs_and_multiple_bodies_run_in_order() {
        let cmd = "paste <<-A <<B\n\tone\n\tA\ntwo\nB\nnext";
        assert_eq!(squash(&strip_heredocs(cmd)), "paste next");
    }

    #[test]
    fn unterminated_heredoc_runs_to_end() {
        assert_eq!(squash(&strip_heredocs("cat <<'EOF'\ngit status\n")), "cat");
    }

    #[test]
    fn heredoc_lookalikes_are_left_alone() {
        for cmd in [
            "cat <<< \"here string\"",
            "echo $((1<<2))",
            "echo '<<EOF'",
            "echo \"<<EOF\"",
        ] {
            assert_eq!(strip_heredocs(cmd), cmd, "{cmd}");
        }
    }

    #[test]
    fn continuations_join_and_comments_drop() {
        assert_eq!(
            squash(&strip_heredocs("cat \\\n  file.txt")),
            "cat file.txt"
        );
        assert_eq!(
            squash(&strip_heredocs("ls # cat secret\necho a#b")),
            "ls echo a#b"
        );
        // A `#` inside quotes is not a comment.
        assert_eq!(strip_heredocs("echo '# x'"), "echo '# x'");
    }

    #[test]
    fn output_redirections_are_dropped() {
        for (part, want) in [
            ("cat > out.txt", "cat"),
            ("cat >> out.txt", "cat"),
            ("cat >| out.txt", "cat"),
            ("cat 2>/dev/null", "cat"),
            ("cat 2> err.log", "cat"),
            ("cat 2>&1", "cat"),
            ("cat &> all.log", "cat"),
            ("cat >&2", "cat"),
            ("cat > \"my out.txt\"", "cat"),
            ("cat <<< \"hi there\"", "cat"),
            ("echo hi>out.txt", "echo hi"),
            ("cat file.txt > out.txt", "cat file.txt"),
        ] {
            assert_eq!(squash(&strip_redirections(part)), want, "{part}");
        }
    }

    #[test]
    fn input_redirections_become_operands() {
        assert_eq!(squash(&strip_redirections("cat < in.txt")), "cat in.txt");
        assert_eq!(squash(&strip_redirections("cat <in.txt")), "cat in.txt");
        assert_eq!(
            squash(&strip_redirections("grep x < 'a b.txt'")),
            "grep x 'a b.txt'"
        );
    }

    #[test]
    fn redirection_lookalikes_are_left_alone() {
        for part in [
            "grep '>' file",
            "grep \">\" file",
            "diff <(cat a) b",
            "a2>b",
        ] {
            let got = strip_redirections(part);
            if part == "a2>b" {
                // Mid-word digits are not an fd: `a2` stays, `>b` drops.
                assert_eq!(squash(&got), "a2");
            } else {
                assert_eq!(got, part, "{part}");
            }
        }
    }
}
