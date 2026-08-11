use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

use crate::Result;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub directory: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub pid: PathBuf,
    pub log_directory: PathBuf,
    pub server_log: PathBuf,
    pub client_log: PathBuf,
}

impl RuntimePaths {
    pub fn discover() -> Result<Self> {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let xdg_base = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| private_owned_directory(path, uid).unwrap_or(false));
        let directory = if let Some(base) = xdg_base {
            base.join("svarm")
        } else {
            env::temp_dir().join(format!("svarm-{uid}"))
        };
        ensure_private_directory(&directory, uid)?;
        let state_base = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|home| home.join(".local/state"))
            });
        let log_directory = state_base.map_or_else(
            || directory.join("logs"),
            |state_base| state_base.join("svarm"),
        );
        ensure_private_log_directory(&log_directory, uid)?;
        Ok(Self {
            socket: directory.join("server.sock"),
            lock: directory.join("server.lock"),
            pid: directory.join("server.pid"),
            server_log: log_directory.join("server.log"),
            client_log: log_directory.join("client.log"),
            log_directory,
            directory,
        })
    }

    pub fn acquire_server_lock(&self) -> Result<Option<ServerLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.lock)?;
        // SAFETY: flock receives a valid open file descriptor.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            Ok(Some(ServerLock { _file: file }))
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error.into())
            }
        }
    }

    pub fn write_pid(&self, pid: u32) -> Result<()> {
        let temporary = self.directory.join(format!("server.pid.{pid}.tmp"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        writeln!(file, "{pid}")?;
        file.sync_all()?;
        if let Err(error) = fs::rename(&temporary, &self.pid) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn remove_pid(&self) {
        let _ = fs::remove_file(&self.pid);
    }

    /// The process the running server recorded for itself, for the one case where the socket
    /// cannot be used to reach it: a server from a build whose protocol this one cannot speak.
    pub fn read_pid(&self) -> Option<u32> {
        fs::read_to_string(&self.pid).ok()?.trim().parse().ok()
    }
}

fn ensure_private_log_directory(path: &Path, uid: u32) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    ensure_private_directory(path, uid)
}

pub struct ServerLock {
    _file: File,
}

fn ensure_private_directory(path: &Path, uid: u32) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    if !private_owned_directory(path, uid)? {
        return Err(format!(
            "Svarm runtime directory must be an owned private directory, not a symlink: {}",
            path.display()
        )
        .into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn private_owned_directory(path: &Path, uid: u32) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(metadata.file_type().is_dir()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == uid
        && metadata.permissions().mode() & 0o022 == 0)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_path() -> PathBuf {
        env::temp_dir().join(format!(
            "svarm-path-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn creates_private_runtime_directory_and_exclusive_lock() {
        let directory = temp_path();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        ensure_private_directory(&directory, uid).unwrap();
        assert_eq!(
            fs::symlink_metadata(&directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let paths = RuntimePaths {
            socket: directory.join("server.sock"),
            lock: directory.join("server.lock"),
            pid: directory.join("server.pid"),
            log_directory: directory.join("logs"),
            server_log: directory.join("logs/server.log"),
            client_log: directory.join("logs/client.log"),
            directory: directory.clone(),
        };
        let lock = paths.acquire_server_lock().unwrap().unwrap();
        assert!(paths.acquire_server_lock().unwrap().is_none());
        drop(lock);
        assert!(paths.acquire_server_lock().unwrap().is_some());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn refuses_a_symlink_as_runtime_directory() {
        let target = temp_path();
        let link = target.with_extension("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        assert!(ensure_private_directory(&link, uid).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir(target).unwrap();
    }

    #[test]
    fn concurrent_creators_revalidate_the_winning_directory() {
        let directory = temp_path();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let barrier = Arc::new(Barrier::new(32));
        let creators = (0..32)
            .map(|_| {
                let directory = directory.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    ensure_private_directory(&directory, uid).is_ok()
                })
            })
            .collect::<Vec<_>>();

        for creator in creators {
            assert!(creator.join().unwrap());
        }
        assert!(private_owned_directory(&directory, uid).unwrap());
        fs::remove_dir(directory).unwrap();
    }
}
