//! A requested stop: SIGINT and SIGTERM end the stream cleanly instead of
//! killing it, so the loop returns the success status a supervisor expects
//! from a requested shutdown. The handler writes to a socket pair, the only
//! thing a signal handler may do here; a reader thread turns the write into
//! the loop's own stop wake.

use std::io::Read;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use crate::util::{Error, Result};

/// What the handler writes: any bytes wake the reader.
const STOP: &[u8] = b"stop";

/// Write end of the socket pair the handler pokes, once installed.
static STOP_FD: OnceLock<libc::c_int> = OnceLock::new();

extern "C" fn on_stop_signal(_signal: libc::c_int) {
    if let Some(fd) = STOP_FD.get() {
        // write(2) is async-signal-safe; nothing else runs in the handler.
        unsafe {
            libc::write(*fd, STOP.as_ptr().cast(), STOP.len());
        }
    }
}

/// Send `stop` on `wake` when SIGINT or SIGTERM arrives.
pub(super) fn on_stop<T: Send + 'static>(wake: Sender<T>, stop: T) -> Result<()> {
    let (mut reader, writer) = UnixStream::pair()
        .map_err(|error| Error(format!("stream could not create its stop channel: {error}")))?;
    STOP_FD
        .set(writer.into_raw_fd())
        .map_err(|_| Error("stream's stop channel is already installed".to_string()))?;
    std::thread::spawn(move || {
        let mut received = [Default::default()];
        if reader.read(&mut received).is_ok() {
            let _ = wake.send(stop);
        }
    });
    let handler = on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    Ok(())
}
