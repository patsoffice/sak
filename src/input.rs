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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

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
