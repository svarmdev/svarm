use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use svarm_agent::{
    Result, logging,
    paths::RuntimePaths,
    server::{ServerConfig, run_foreground_ready},
};

use crate::client::{ControlClient, Probe};

const STARTUP_DEADLINE: Duration = Duration::from_secs(3);

pub fn ensure_server(paths: &RuntimePaths) -> Result<()> {
    match ControlClient::probe_socket(&paths.socket)? {
        Probe::Running(_) => return Ok(()),
        // Starting a second server is not an option: the running one owns the socket and the
        // lock. Name the command that clears it rather than reporting a handshake failure.
        Probe::Incompatible(reason) => {
            return Err(format!(
                "{reason}\nRun `svarm server stop` to stop it, then start Svarm again."
            )
            .into());
        }
        Probe::None => {}
    }
    let executable = std::env::current_exe()?;
    let server_log = logging::writer(&paths.server_log)
        .map_err(|error| format!("could not open the Svarm server log: {error}"))?;
    let server_error_log = server_log.try_clone()?;
    let mut command = Command::new(executable);
    command
        .arg("__server")
        .stdin(Stdio::null())
        .stdout(Stdio::from(server_log))
        .stderr(Stdio::from(server_error_log));
    // SAFETY: setsid is async-signal-safe and runs before exec while the child is single-threaded.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
        .spawn()
        .map_err(|error| format!("could not start Svarm server: {error}"))?;

    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut last_error = None;
    while Instant::now() < deadline {
        match ControlClient::probe(&paths.socket) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => last_error = Some(error.to_string()),
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "Svarm server did not become ready{}",
        last_error.map_or_else(String::new, |error| format!(": {error}"))
    )
    .into())
}

pub fn run_server() -> Result<()> {
    let paths = RuntimePaths::discover()
        .map_err(|error| format!("could not prepare the Svarm runtime directory: {error}"))?;
    let Some(_lock) = paths
        .acquire_server_lock()
        .map_err(|error| format!("could not acquire the Svarm server lock: {error}"))?
    else {
        return Ok(());
    };
    let _ = logging::append(
        &paths.server_log,
        concat!("server starting version=", env!("CARGO_PKG_VERSION")),
    );
    if ControlClient::probe(&paths.socket)
        .map_err(|error| format!("could not probe the Svarm server socket: {error}"))?
        .is_some()
    {
        return Ok(());
    }
    if paths.socket.exists() {
        fs::remove_file(&paths.socket)
            .map_err(|error| format!("could not remove the stale Svarm socket: {error}"))?;
    }
    let process_id = std::process::id();
    let result = run_foreground_ready(
        ServerConfig::new(paths.socket.clone(), env!("CARGO_PKG_VERSION"))
            .with_conversation_directory(paths.conversation_directory.clone())
            .with_signal_handling(),
        || {
            paths
                .write_pid(process_id)
                .map_err(|error| format!("could not write the Svarm server PID: {error}").into())
        },
    )
    .map_err(|error| format!("Svarm server failed: {error}").into());
    paths.remove_pid();
    let _ = logging::append(
        &paths.server_log,
        if result.is_ok() {
            "server stopped cleanly"
        } else {
            "server stopped after an error"
        },
    );
    result
}
