use std::{io, os::unix::net::UnixStream, path::Path, time::Duration};

use svarm_agent::{
    Result,
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, ErrorCode, Hello, HostTerminalCapabilities, Message,
        PROTOCOL_VERSION, ProtocolRange, Request, RequestId, Response, ServerStatusSnapshot,
    },
};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);

/// What a probe found listening on the socket.
///
/// A server from a different build is a real outcome, not an error to bail on: it happens on every
/// upgrade, and callers need to tell it apart from an empty socket so they can say something
/// useful rather than reporting a bare handshake failure.
pub enum Probe {
    None,
    Running(Box<ServerStatusSnapshot>),
    Incompatible(String),
}

pub struct ControlClient {
    stream: UnixStream,
    next_request_id: u64,
}

impl ControlClient {
    pub fn connect(socket: &Path, role: ConnectionRole) -> Result<Self> {
        let stream = UnixStream::connect(socket)?;
        Ok(Self::handshake(stream, role)?)
    }

    pub fn probe(socket: &Path) -> Result<Option<ServerStatusSnapshot>> {
        match Self::probe_socket(socket)? {
            Probe::None => Ok(None),
            Probe::Running(status) => Ok(Some(*status)),
            Probe::Incompatible(message) => Err(message.into()),
        }
    }

    pub fn probe_socket(socket: &Path) -> Result<Probe> {
        if !socket.exists() {
            return Ok(Probe::None);
        }
        let stream = match UnixStream::connect(socket) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(Probe::None);
            }
            Err(error) => return Err(error.into()),
        };
        let mut client = match Self::handshake(stream, ConnectionRole::Probe) {
            Ok(client) => client,
            Err(HandshakeError::Incompatible(message)) => return Ok(Probe::Incompatible(message)),
            Err(HandshakeError::Other(error)) => return Err(error),
        };
        match client.request(Request::ServerStatus)? {
            Response::ServerStatus(status) => Ok(Probe::Running(Box::new(status))),
            _ => Err("Svarm server returned an invalid probe response".into()),
        }
    }

    pub fn request(&mut self, request: Request) -> Result<Response> {
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or("control request identifier space exhausted")?;
        write_frame(
            &mut self.stream,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                message: Message::Request(request),
            },
        )?;
        loop {
            let Some(envelope) = read_frame::<_, Envelope>(&mut self.stream)? else {
                return Err("Svarm server disconnected before responding".into());
            };
            if envelope.request_id != Some(request_id) {
                continue;
            }
            return match envelope.message {
                Message::Response(response) => Ok(response),
                Message::Error(error) => Err(error.actionable_message().into()),
                _ => Err("Svarm server returned an invalid control response".into()),
            };
        }
    }

    fn handshake(
        mut stream: UnixStream,
        role: ConnectionRole,
    ) -> std::result::Result<Self, HandshakeError> {
        stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
        stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
        write_frame(
            &mut stream,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(RequestId(1)),
                message: Message::Hello(Hello {
                    application_version: env!("CARGO_PKG_VERSION").into(),
                    protocol: ProtocolRange::CURRENT,
                    role,
                    process_id: Some(std::process::id()),
                    terminal: HostTerminalCapabilities::default(),
                }),
            },
        )?;
        match read_frame::<_, Envelope>(&mut stream).map_err(HandshakeError::from)? {
            Some(Envelope {
                message: Message::Welcome(_),
                ..
            }) => Ok(Self {
                stream,
                next_request_id: 2,
            }),
            Some(Envelope {
                message: Message::Error(error),
                ..
            }) => Err(if error.code == ErrorCode::IncompatibleProtocol {
                HandshakeError::Incompatible(error.actionable_message())
            } else {
                HandshakeError::Other(error.actionable_message().into())
            }),
            _ => Err(HandshakeError::Other(
                "Svarm server did not complete the protocol handshake".into(),
            )),
        }
    }
}

/// Separates the one handshake failure callers act on from every other cause.
enum HandshakeError {
    Incompatible(String),
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl From<svarm_agent::framing::FramingError> for HandshakeError {
    fn from(error: svarm_agent::framing::FramingError) -> Self {
        Self::Other(error.into())
    }
}

impl From<io::Error> for HandshakeError {
    fn from(error: io::Error) -> Self {
        Self::Other(error.into())
    }
}

impl From<HandshakeError> for Box<dyn std::error::Error + Send + Sync> {
    fn from(error: HandshakeError) -> Self {
        match error {
            HandshakeError::Incompatible(message) => message.into(),
            HandshakeError::Other(error) => error,
        }
    }
}
