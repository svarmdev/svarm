use std::{
    fmt,
    io::{self, Read, Write},
};

use serde::{Serialize, de::DeserializeOwned};

pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum FramingError {
    Io(io::Error),
    InvalidLength(u32),
    FrameTooLarge(u32),
    Truncated,
    Malformed(serde_json::Error),
}

impl fmt::Display for FramingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "frame I/O failed: {error}"),
            Self::InvalidLength(length) => write!(formatter, "invalid frame length: {length}"),
            Self::FrameTooLarge(length) => write!(formatter, "frame is too large: {length} bytes"),
            Self::Truncated => formatter.write_str("frame ended before its declared length"),
            Self::Malformed(error) => write!(formatter, "malformed frame payload: {error}"),
        }
    }
}

impl std::error::Error for FramingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Malformed(error) => Some(error),
            _ => None,
        }
    }
}

pub fn encode_frame<T: Serialize>(message: &T) -> Result<Vec<u8>, FramingError> {
    let payload = serde_json::to_vec(message).map_err(FramingError::Malformed)?;
    if payload.is_empty() {
        return Err(FramingError::InvalidLength(0));
    }
    if payload.len() > MAX_FRAME_LEN {
        return Err(FramingError::FrameTooLarge(payload.len() as u32));
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), FramingError> {
    let frame = encode_frame(message)?;
    writer.write_all(&frame).map_err(FramingError::Io)
}

pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<Option<T>, FramingError> {
    let mut prefix = [0_u8; 4];
    loop {
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FramingError::Io(error)),
        }
    }
    read_exact(reader, &mut prefix[1..])?;
    let length = u32::from_be_bytes(prefix);
    if length == 0 {
        return Err(FramingError::InvalidLength(length));
    }
    if length as usize > MAX_FRAME_LEN {
        return Err(FramingError::FrameTooLarge(length));
    }
    let mut payload = vec![0_u8; length as usize];
    read_exact(reader, &mut payload)?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(FramingError::Malformed)
}

fn read_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), FramingError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            FramingError::Truncated
        } else {
            FramingError::Io(error)
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::protocol::{
        ConnectionRole, Envelope, Hello, HostTerminalCapabilities, Message, ProtocolRange,
        RequestId,
    };

    fn hello() -> Envelope {
        Envelope {
            protocol_version: 1,
            request_id: Some(RequestId(7)),
            message: Message::Hello(Hello {
                application_version: "0.1.0".into(),
                protocol: ProtocolRange { min: 1, max: 1 },
                role: ConnectionRole::Probe,
                process_id: Some(42),
                terminal: HostTerminalCapabilities::default(),
            }),
        }
    }

    struct OneByteReader<R>(R);

    impl<R: Read> Read for OneByteReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let length = buffer.len().min(1);
            self.0.read(&mut buffer[..length])
        }
    }

    #[test]
    fn round_trips_when_every_read_is_split() {
        let frame = encode_frame(&hello()).unwrap();
        let decoded = read_frame::<_, Envelope>(&mut OneByteReader(Cursor::new(frame))).unwrap();
        assert_eq!(decoded, Some(hello()));
    }

    #[test]
    fn hello_has_a_stable_inspectable_fixture() {
        let frame = encode_frame(&hello()).unwrap();
        let payload = std::str::from_utf8(&frame[4..]).unwrap();
        assert_eq!(
            payload,
            r#"{"protocol_version":1,"request_id":7,"message":{"kind":"hello","body":{"application_version":"0.1.0","protocol":{"min":1,"max":1},"role":"probe","process_id":42,"terminal":{"color_enabled":false,"true_color":false,"mouse":false,"bracketed_paste":false}}}}"#
        );
    }

    #[test]
    fn clean_eof_is_distinct_from_a_truncated_frame() {
        assert!(
            read_frame::<_, Envelope>(&mut Cursor::new([]))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            read_frame::<_, Envelope>(&mut Cursor::new([0, 0])),
            Err(FramingError::Truncated)
        ));
        assert!(matches!(
            read_frame::<_, Envelope>(&mut Cursor::new([0, 0, 0, 4, b'{'])),
            Err(FramingError::Truncated)
        ));
    }

    #[test]
    fn rejects_zero_and_oversized_lengths_before_reading_payloads() {
        assert!(matches!(
            read_frame::<_, Envelope>(&mut Cursor::new(0_u32.to_be_bytes())),
            Err(FramingError::InvalidLength(0))
        ));
        let length = (MAX_FRAME_LEN as u32 + 1).to_be_bytes();
        assert!(matches!(
            read_frame::<_, Envelope>(&mut Cursor::new(length)),
            Err(FramingError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn rejects_malformed_trailing_and_unknown_messages() {
        for payload in [
            br#"{"#.as_slice(),
            br#"{} trailing"#.as_slice(),
            br#"{"protocol_version":1,"request_id":null,"message":{"kind":"future"}}"#.as_slice(),
        ] {
            let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
            frame.extend_from_slice(payload);
            assert!(matches!(
                read_frame::<_, Envelope>(&mut Cursor::new(frame)),
                Err(FramingError::Malformed(_))
            ));
        }
    }
}
