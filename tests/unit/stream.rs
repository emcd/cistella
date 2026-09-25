//! Incremental frame reader: assembly across slices, stall bounds.
use std::io::Read;
use std::os::fd::AsFd;
use std::time::Duration;

use cistella::framework::stream::StreamReader;

/// Pipe-backed reader adapter: `StreamReader` needs `Read + AsFd`.
struct PipeReader {
    /// Read end (owned).
    file: std::fs::File,
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl AsFd for PipeReader {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.file.as_fd()
    }
}

fn pipe_pair() -> (std::fs::File, std::fs::File) {
    let mut fds = [0; 2];
    // SAFETY: valid out-param pair; both ends wrapped exactly once.
    unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0, "pipe");
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    }
}

use std::os::fd::FromRawFd;

#[test]
fn quiet_stream_stays_idle() {
    // No bytes ever: every slice returns idle, never an error.
    let (read, _write) = pipe_pair();
    let mut reader = StreamReader::new(1024, Duration::from_secs(60));
    let mut source = PipeReader { file: read };
    for _ in 0..3 {
        assert!(
            reader
                .poll_frame(&mut source, Duration::from_millis(50))
                .expect("idle slice never errors")
                .is_none(),
            "quiet stream idles"
        );
    }
}

#[test]
fn stalled_partial_fails_typed() {
    // One byte then silence: completion is due by the frame
    // One byte then silence with the writer HELD open (no EOF):
    // completion is due by the frame timeout, and expiry fails
    // typed instead of pending forever across slices.
    let (read, mut write) = pipe_pair();
    use std::io::Write;
    write.write_all(b"\x00").expect("partial byte");
    let mut reader = StreamReader::new(1024, Duration::from_millis(200));
    let mut source = PipeReader { file: read };
    // First slices: progress recorded, still idle (budget not hit).
    // Then expiry must fail typed, fast (200ms budget, not 60s).
    let start = std::time::Instant::now();
    let result = loop {
        match reader.poll_frame(&mut source, Duration::from_millis(50)) {
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(5) {
                    panic!("stall must fail by frame timeout");
                }
            }
            conclusion => break conclusion,
        }
    };
    let error = result.expect_err("stalled partial must fail");
    assert!(
        error.to_string().contains("stalled after first byte"),
        "typed stall failure, got: {error}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "frame timeout bounds the stall"
    );
    drop(write);
}

#[test]
fn trickle_across_slices_resolves() {
    // Bytes straddling slices resolve: the header arrives alone,
    // the body arrives next slice, and the frame completes.
    let (read, mut write) = pipe_pair();
    use std::io::Write;
    let body = b"{}";
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    let (header, rest) = frame.split_at(4);
    write.write_all(header).expect("header slice");
    let mut reader = StreamReader::new(1024, Duration::from_secs(60));
    let mut source = PipeReader { file: read };
    assert!(
        reader
            .poll_frame(&mut source, Duration::from_millis(100))
            .expect("header slice idles, never fails")
            .is_none()
    );
    write.write_all(rest).expect("body slice");
    let completed = reader
        .poll_frame(&mut source, Duration::from_secs(2))
        .expect("body slice completes")
        .expect("frame resolves across slices");
    assert_eq!(completed, body);
}
