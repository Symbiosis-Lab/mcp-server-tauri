use std::io::Write;

/// Format a log line and write it, **swallowing any write error**.
///
/// The bridge is a dev tool embedded in the host app (e.g. moss, built with
/// `panic = "abort"`). Rust installs `SIGPIPE` as `SIG_IGN`, so when the reader
/// of the process's stdout/stderr pipe closes — the AI harness disconnects, a
/// `... | head`, a closed terminal — a write returns `EPIPE` instead of raising
/// the signal. The `println!`/`eprintln!` macros **panic** on that error, and
/// with `panic = "abort"` in the host that aborts the whole app.
///
/// Writing through `writeln!` and discarding the `Result` turns a closed pipe
/// into a silent no-op, so the host keeps running (matching the host's own
/// resilient-stdout policy). Split out from the public fns so the swallow
/// behavior is unit-testable against a writer that always fails.
fn write_log(mut w: impl Write, scope: &str, level: &str, msg: &str) {
    let _ = writeln!(w, "[MCP][{scope}][{level}] {msg}");
}

pub fn mcp_log_info(scope: &str, msg: &str) {
    write_log(std::io::stdout(), scope, "INFO", msg);
}

pub fn mcp_log_error(scope: &str, msg: &str) {
    write_log(std::io::stderr(), scope, "ERROR", msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose every operation fails with `BrokenPipe`, standing in for
    /// a stdout/stderr pipe whose reader has closed.
    struct AlwaysBrokenPipe;
    impl Write for AlwaysBrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn write_log_swallows_broken_pipe_without_panicking() {
        // The exact failure `println!` turns into an app-aborting panic. This
        // must return normally, not unwind.
        write_log(AlwaysBrokenPipe, "WS", "INFO", "client disconnected");
    }

    #[test]
    fn write_log_writes_formatted_line_to_healthy_sink() {
        let mut sink: Vec<u8> = Vec::new();
        write_log(&mut sink, "WS", "ERROR", "boom");
        assert_eq!(sink, b"[MCP][WS][ERROR] boom\n");
    }
}
