//! Bounded reads that observe cancellation without changing stored bytes.
use std::{
    io::{self, Read},
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct CancellableRead<'a, R> {
    reader: R,
    cancel: Option<&'a AtomicBool>,
}

impl<'a, R> CancellableRead<'a, R> {
    pub(super) fn new(reader: R, cancel: Option<&'a AtomicBool>) -> Self {
        Self { reader, cancel }
    }
}

impl<R: Read> Read for CancellableRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            // Not Interrupted: io::copy/read_exact would retry that error.
            return Err(io::Error::other("import cancelled"));
        }
        let length = buffer.len().min(super::STREAM_BUFFER_BYTES);
        self.reader.read(&mut buffer[..length])
    }
}
