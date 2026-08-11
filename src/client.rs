use std::{io, os::unix::net::UnixStream, path::Path, time::Duration};

use svarm_agent::{
    Result,
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, Hello, HostTerminalCapabilities, Message, PROTOCOL_VERSION,
        ProtocolRange, Request, RequestId, Response, ServerStatusSnapshot,
    },
};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ControlClient {
    stream: UnixStream,
    next_request_id: u64,
}

impl ControlClient {
    pub fn connect(socket: &Path, role: ConnectionRole) -> Result<Self> {
        let stream = UnixStream::connect(socket)?;
        Self::handshake(stream, role)
    }

    pub fn probe(socket: &Path) -> Result<Option<ServerStatusSnapshot>> {
        if !socket.exists() {
            return Ok(None);
        }
        let stream = match UnixStream::connect(socket) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let mut client = Self::handshake(stream, ConnectionRole::Probe)?;
        match client.request(Request::ServerStatus)? {
            Response::ServerStatus(status) => Ok(Some(status)),
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

    fn handshake(mut stream: UnixStream, role: ConnectionRole) -> Result<Self> {
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
        match read_frame::<_, Envelope>(&mut stream)? {
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
            }) => Err(error.actionable_message().into()),
            _ => Err("Svarm server did not complete the protocol handshake".into()),
        }
    }
}
