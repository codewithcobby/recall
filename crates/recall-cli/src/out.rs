//! Writing to stdout when the reader may stop listening.
//!
//! `recall sessions | head`, or `recall show <id> | less` quit halfway, closes
//! stdout while Recall is still writing. That is ordinary shell usage — `head`
//! does it on every invocation — but by default it makes a Rust program panic:
//!
//! ```text
//! thread 'main' panicked at library/std/src/io/stdio.rs:
//! failed printing to stdout: Broken pipe (os error 32)
//! ```
//!
//! The reason is that Rust ignores `SIGPIPE` at startup. A C program would be
//! killed by the signal and never notice; a Rust program gets `EPIPE` back from
//! the write instead, and `println!` panics on it.
//!
//! The usual fix is to restore the default signal handler, which needs `unsafe`
//! and is forbidden across this workspace. So the error is handled where it
//! appears: a write that fails because nobody is reading ends the program
//! quietly and successfully. Nothing has gone wrong — the reader asked for some
//! output and got it.
//!
//! Two paths need this, and they fail differently:
//!
//! - `println!` panics. [`outln!`] replaces it.
//! - `recall show` writes through `?`, so `EPIPE` becomes an ordinary error and
//!   is reported as `error: Broken pipe (os error 32)` with a non-zero status.
//!   [`is_closed_pipe`] lets `main` recognise it and exit 0 silently instead.
//!
//! Exit status 0 is deliberate. A program killed by `SIGPIPE` reports 141, but
//! that is an artefact of dying to a signal rather than a judgement about the
//! run. In a pipeline the shell reports the *reader's* status anyway, so this
//! is only visible to someone who went looking — and what it should tell them
//! is that nothing failed.

use std::io::{self, Write};
use std::process;

use crate::exit;

/// Print a line to stdout, treating a closed pipe as the end of the output.
///
/// A drop-in replacement for `println!` everywhere Recall writes its results.
macro_rules! outln {
    () => { $crate::out::line(format_args!("")) };
    ($($arg:tt)*) => { $crate::out::line(format_args!($($arg)*)) };
}

pub(crate) use outln;

/// Write one line, or stop.
///
/// Not fallible on purpose. Every caller is presentation code partway through
/// printing a result, and there is nothing useful for any of them to do about a
/// reader that has gone away — threading a `Result` back through them would add
/// error handling at thirty call sites to describe a situation that is not an
/// error.
pub fn line(args: std::fmt::Arguments<'_>) {
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    match writeln!(handle, "{args}") {
        Ok(()) => {}
        // Nobody is reading. Everything asked for has been delivered.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => process::exit(exit::SUCCESS as i32),
        // A genuine failure to write — a full disk, a closed terminal. Worth
        // reporting, and worth a status that says so.
        Err(e) => {
            eprintln!("error: could not write to stdout: {e}");
            process::exit(exit::IO as i32);
        }
    }
}

/// Whether a failure is really just a reader that stopped reading.
///
/// Walks the chain, because by the time this reaches `main` the `io::Error` is
/// usually underneath some context added on the way up.
pub fn is_closed_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_pipe_is_recognised_through_context() {
        let error = anyhow::Error::new(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
            .context("while printing a session")
            .context("recall show");
        assert!(is_closed_pipe(&error));
    }

    #[test]
    fn other_io_failures_are_not_mistaken_for_it() {
        // A full disk must still be reported. Treating every i/o error as "the
        // reader left" would turn real failures into silent successes.
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::WriteZero,
        ] {
            let error = anyhow::Error::new(io::Error::new(kind, "nope")).context("writing");
            assert!(
                !is_closed_pipe(&error),
                "{kind:?} was treated as a closed pipe"
            );
        }
    }

    #[test]
    fn an_unrelated_failure_is_not_a_closed_pipe() {
        let error = anyhow::anyhow!("something else entirely");
        assert!(!is_closed_pipe(&error));
    }
}
