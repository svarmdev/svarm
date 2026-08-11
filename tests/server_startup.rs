#![cfg(unix)]

use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use svarm_agent::{
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, Hello, HostTerminalCapabilities, Message, PROTOCOL_VERSION,
        ProtocolRange, Request, RequestId, Response, ServerInstanceId,
    },
};

#[test]
fn concurrent_server_candidates_share_one_instance() {
    let runtime_base = temp_dir();
    fs::create_dir(&runtime_base).unwrap();
    fs::set_permissions(&runtime_base, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime_base.join("svarm/server.sock");
    let binary = env!("CARGO_BIN_EXE_svarm");
    let mut candidates = (0..8)
        .map(|_| {
            Command::new(binary)
                .arg("__server")
                .env("XDG_RUNTIME_DIR", &runtime_base)
                .env("XDG_STATE_HOME", &runtime_base)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();

    wait_for_socket(&socket);
    let instances = (0..8)
        .map(|_| handshake(&socket, ConnectionRole::Probe).0)
        .collect::<Vec<_>>();
    assert!(instances.iter().all(|instance| instance == &instances[0]));

    let (_, mut control) = handshake(&socket, ConnectionRole::Control);
    write_frame(
        &mut control,
        &Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(RequestId(2)),
            message: Message::Request(Request::StopServer { confirmed: true }),
        },
    )
    .unwrap();
    let response: Envelope = read_frame(&mut control).unwrap().unwrap();
    assert!(matches!(
        response.message,
        Message::Response(Response::Stopped(_))
    ));
    drop(control);

    wait_for_children(&mut candidates);
    assert!(!socket.exists());
    fs::remove_dir_all(runtime_base).unwrap();
}

#[test]
fn hangup_is_ignored_and_termination_is_graceful() {
    let runtime_base = temp_dir();
    fs::create_dir(&runtime_base).unwrap();
    fs::set_permissions(&runtime_base, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime_base.join("svarm/server.sock");
    let mut server = Command::new(env!("CARGO_BIN_EXE_svarm"))
        .arg("__server")
        .env("XDG_RUNTIME_DIR", &runtime_base)
        .env("XDG_STATE_HOME", &runtime_base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    wait_for_socket(&socket);
    // SAFETY: the PID belongs to the child above and the signal values are valid.
    assert_eq!(
        unsafe { libc::kill(server.id() as libc::pid_t, libc::SIGHUP) },
        0
    );
    thread::sleep(Duration::from_millis(50));
    let _ = handshake(&socket, ConnectionRole::Probe);
    // SAFETY: the same live child is deliberately asked to shut down.
    assert_eq!(
        unsafe { libc::kill(server.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    wait_for_children(std::slice::from_mut(&mut server));
    assert!(!socket.exists());
    fs::remove_dir_all(runtime_base).unwrap();
}

fn handshake(socket: &Path, role: ConnectionRole) -> (ServerInstanceId, UnixStream) {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_frame(
        &mut stream,
        &Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(RequestId(1)),
            message: Message::Hello(Hello {
                application_version: "test".into(),
                protocol: ProtocolRange::CURRENT,
                role,
                process_id: Some(std::process::id()),
                terminal: HostTerminalCapabilities::default(),
            }),
        },
    )
    .unwrap();
    let welcome: Envelope = read_frame(&mut stream).unwrap().unwrap();
    match welcome.message {
        Message::Welcome(welcome) => (welcome.instance_id, stream),
        other => panic!("unexpected handshake response: {other:?}"),
    }
}

fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if socket.exists() && UnixStream::connect(socket).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("server socket did not become ready: {}", socket.display());
}

fn wait_for_children(children: &mut [Child]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if children
            .iter_mut()
            .all(|child| child.try_wait().unwrap().is_some())
        {
            return;
        }
        if Instant::now() >= deadline {
            for child in children {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("server candidates did not exit after shutdown");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "svarm-startup-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
