//! Shared decision for "an omitted file argument means stdin".
//!
//! Many sak commands read stdin when no file is given, so they compose in
//! pipelines (`cmd | sak json query .a`). That default is only safe when
//! something was actually piped in: run with nothing piped, the command would
//! read an empty stream and print an empty or zero result that looks like a
//! real answer. Every such command routes its *implicit* stdin read through
//! [`require_piped_stdin`] (or [`stdin_is_piped`] to choose a fallback, as
//! `fs grep` does). An explicit `-` argument always reads stdin.

use anyhow::Result;

/// Whether `fd` carries piped input: a FIFO (`cmd | sak …`) or a regular file
/// (`sak … < file`). Everything else — a TTY, `/dev/null`, a socket, or an fd
/// that can't be stat'd — is not. Sockets are deliberately excluded: agent
/// harnesses built on Node spawn commands over socketpairs with nothing
/// written (and possibly never an EOF), so an omitted path must not read that
/// stream. A shell pipe inside the command is still a FIFO.
pub(crate) fn fd_is_piped(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::unix::fs::FileTypeExt;

    let Ok(owned) = fd.try_clone_to_owned() else {
        return false;
    };
    let Ok(meta) = std::fs::File::from(owned).metadata() else {
        return false;
    };
    let ft = meta.file_type();
    ft.is_fifo() || ft.is_file()
}

/// [`fd_is_piped`] for this process's stdin: whether an omitted file argument
/// may mean "read stdin".
pub(crate) fn stdin_is_piped() -> bool {
    use std::os::fd::AsFd;
    fd_is_piped(std::io::stdin().as_fd())
}

/// Gate for commands whose omitted file argument means stdin: error (exit 2)
/// instead of reading a stdin nothing was piped into.
pub(crate) fn require_piped_stdin() -> Result<()> {
    if stdin_is_piped() {
        Ok(())
    } else {
        anyhow::bail!(
            "no file given and stdin is not piped; pass a file path, or '-' to read stdin anyway"
        )
    }
}

/// Resolve a `[PATH] [FILES]...` command's positionals, where PATH is an
/// optional location *inside* the document (`json keys`, `config type`, ...).
///
/// Clap fills PATH first, so `sak json keys data.json` arrives as
/// PATH=`data.json` with no files, and the command would then read stdin for
/// a document that was named on the command line. When no files were given
/// and the lone positional is an existing file, it is the input file and
/// the path falls back to the document root (`None`). A real in-document
/// path (`.a.b`, `/a/b`) isn't a file, so it stays a path.
///
/// The decision deliberately ignores stdin. It used to keep the positional
/// as a path whenever stdin was piped, which made the same command give
/// different answers by context: `sak json keys data.json` inside a
/// `while read …; done < list` loop read the loop's input instead of the
/// file. The cost is the rare `cmd | sak json keys name` where a file named
/// `name` also exists in the working directory; write `.name` there.
pub(crate) fn path_or_file(
    path: Option<&str>,
    files: &[std::path::PathBuf],
) -> (Option<String>, Vec<std::path::PathBuf>) {
    path_or_file_with(path, files, |p| p.is_file())
}

fn path_or_file_with(
    path: Option<&str>,
    files: &[std::path::PathBuf],
    is_file: impl Fn(&std::path::Path) -> bool,
) -> (Option<String>, Vec<std::path::PathBuf>) {
    if files.is_empty()
        && let Some(p) = path
        && is_file(std::path::Path::new(p))
    {
        return (None, vec![std::path::PathBuf::from(p)]);
    }
    (path.map(str::to_string), files.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::path::PathBuf;

    #[test]
    fn lone_file_positional_is_the_input() {
        // Regression: `sak json keys data.json` read stdin instead of the file.
        let file = |p: &std::path::Path| p.as_os_str() == "data.json";
        assert_eq!(
            path_or_file_with(Some("data.json"), &[], file),
            (None, vec![PathBuf::from("data.json")])
        );
        // A real in-document path stays a path.
        assert_eq!(
            path_or_file_with(Some(".a.b"), &[], file),
            (Some(".a.b".to_string()), vec![])
        );
        // With files given, the first positional is always the path.
        assert_eq!(
            path_or_file_with(Some("data.json"), &[PathBuf::from("x.json")], file),
            (Some("data.json".to_string()), vec![PathBuf::from("x.json")])
        );
    }

    #[test]
    fn lone_file_promotion_does_not_depend_on_stdin() {
        // Regression: promotion was skipped whenever stdin was piped, so
        // `sak json keys data.json < list` (a while-read loop) read the
        // loop's stdin. This runs the real `path_or_file` against a real
        // file under the test process's own stdin, whatever shape that has:
        // the answer must be the same everywhere.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.json");
        std::fs::write(&p, "{}").unwrap();
        let positional = p.display().to_string();
        assert_eq!(path_or_file(Some(&positional), &[]), (None, vec![p]));
    }

    #[test]
    fn pipes_and_files_are_piped_input() {
        // `cmd | sak …` hands sak a FIFO; `sak … < file` a regular file.
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(fd_is_piped(reader.as_fd()));
        let file = tempfile::tempfile().unwrap();
        assert!(fd_is_piped(file.as_fd()));
    }

    #[test]
    fn harness_shaped_stdin_is_not_piped_input() {
        // Regression (sak-llm-poqf): Node-based agent harnesses spawn
        // commands over a socketpair, and `< /dev/null` is a char device
        // like a TTY. Neither may stand in for an omitted file argument.
        let (sock, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(!fd_is_piped(sock.as_fd()));
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!fd_is_piped(null.as_fd()));
    }
}
