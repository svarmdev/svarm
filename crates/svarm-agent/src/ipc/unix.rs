use std::{
    fs, io,
    os::unix::{
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

pub struct UnixListenerGuard {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixListenerGuard {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
        })
    }

    pub fn accept(&self) -> io::Result<Option<UnixStream>> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                verify_peer_user(&stream)?;
                Ok(Some(stream))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UnixListenerGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "linux")]
fn verify_peer_user(stream: &UnixStream) -> io::Result<()> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the pointers reference initialized storage of the declared length and remain valid
    // for the duration of getsockopt.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions.
    if credentials.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local IPC peer belongs to a different user",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_peer_user(stream: &UnixStream) -> io::Result<()> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: uid and gid point to writable values for the duration of getpeereid.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions.
    if uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local IPC peer belongs to a different user",
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn verify_peer_user(_stream: &UnixStream) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local IPC peer verification is supported only on Linux and macOS",
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::FileTypeExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_socket() -> PathBuf {
        std::env::temp_dir().join(format!(
            "svarm-ipc-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn socket_is_private_and_removed_with_its_owner() {
        let path = temp_socket();
        let listener = UnixListenerGuard::bind(&path).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let client = UnixStream::connect(&path).unwrap();
        let accepted = listener.accept().unwrap().unwrap();
        drop((client, accepted, listener));
        assert!(!path.exists());
    }
}
