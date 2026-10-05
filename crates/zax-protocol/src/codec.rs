//! One JSON value per line, both hops.

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

pub fn write_message<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// `Ok(None)` at end of stream. Blank lines are skipped.
pub fn read_message<R: BufRead, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::ClientMessage;

    #[test]
    fn messages_are_one_line_each_and_blank_lines_are_skipped() {
        let mut buf = Vec::new();
        write_message(&mut buf, &ClientMessage::Hello { identity: 1 }).unwrap();
        buf.extend_from_slice(b"\n");
        write_message(&mut buf, &ClientMessage::Hello { identity: 2 }).unwrap();
        let mut reader = std::io::BufReader::new(buf.as_slice());
        let a: ClientMessage = read_message(&mut reader).unwrap().unwrap();
        let b: ClientMessage = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(a, ClientMessage::Hello { identity: 1 });
        assert_eq!(b, ClientMessage::Hello { identity: 2 });
        assert_eq!(read_message::<_, ClientMessage>(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_line_that_is_not_the_message_is_invalid_data() {
        let mut reader = std::io::BufReader::new(&b"nope\n"[..]);
        let err = read_message::<_, ClientMessage>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
