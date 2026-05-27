use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(super) async fn read_capped_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<bool>>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            trim_line_end(line);
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(truncated))
            };
        }

        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            truncated |= append_capped(line, &available[..pos], max_bytes);
            reader.consume(pos + 1);
            trim_line_end(line);
            return Ok(Some(truncated));
        }

        let consumed = available.len();
        truncated |= append_capped(line, available, max_bytes);
        reader.consume(consumed);
    }
}

fn append_capped(line: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(line.len());
    let copied = remaining.min(chunk.len());
    if copied > 0 {
        line.extend_from_slice(&chunk[..copied]);
    }
    copied < chunk.len()
}

fn trim_line_end(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}
