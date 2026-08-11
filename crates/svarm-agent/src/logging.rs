use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const RETAINED_LOGS: usize = 2;

pub fn append(path: &Path, line: &str) -> io::Result<()> {
    let mut file = writer(path)?;
    writeln!(file, "{line}")
}

pub fn writer(path: &Path) -> io::Result<File> {
    rotate_if_needed(path)?;
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn rotate_if_needed(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Svarm log path is not a regular file",
        ));
    }
    if metadata.len() < MAX_LOG_BYTES {
        return Ok(());
    }
    let oldest = rotated_path(path, RETAINED_LOGS);
    match fs::remove_file(oldest) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for index in (1..RETAINED_LOGS).rev() {
        let source = rotated_path(path, index);
        let target = rotated_path(path, index + 1);
        match fs::rename(source, target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::rename(path, rotated_path(path, 1))
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(format!(".{index}"));
    rotated.into()
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn logs_are_private_and_rotate() {
        let directory = std::env::temp_dir().join(format!(
            "svarm-log-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("client.log");
        append(&path, "client lifecycle").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let file = writer(&path).unwrap();
        file.set_len(MAX_LOG_BYTES).unwrap();
        drop(file);
        append(&path, "next lifecycle").unwrap();
        assert!(rotated_path(&path, 1).exists());
        assert!(fs::metadata(&path).unwrap().len() < MAX_LOG_BYTES);
        fs::remove_dir_all(directory).unwrap();
    }
}
