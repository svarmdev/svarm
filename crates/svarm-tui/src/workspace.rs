use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender, SyncSender},
    thread,
};

use crate::{agents::ClientEvent, app::DirectoryChoice};

pub(crate) struct DirectoryLoadResult {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<Vec<DirectoryChoice>, String>,
}

pub(crate) struct DirectoryLoader {
    requests: Sender<(u64, PathBuf)>,
}

impl DirectoryLoader {
    pub fn new(events: SyncSender<ClientEvent>) -> Self {
        let (requests, receiver) = mpsc::channel::<(u64, PathBuf)>();
        thread::spawn(move || {
            while let Ok((generation, path)) = receiver.recv() {
                let result = read_directories(&path);
                if events
                    .send(ClientEvent::DirectoryLoaded(DirectoryLoadResult {
                        generation,
                        path,
                        result,
                    }))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { requests }
    }

    pub fn load(&self, generation: u64, path: PathBuf) -> Result<(), String> {
        self.requests
            .send((generation, path))
            .map_err(|_| "directory browser worker stopped".into())
    }
}

fn read_directories(path: &Path) -> Result<Vec<DirectoryChoice>, String> {
    let entries = fs::read_dir(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("could not read an entry in {}: {error}", path.display()))?;
        let entry_path = entry.path();
        match fs::metadata(&entry_path) {
            Ok(metadata) if metadata.is_dir() => directories.push(DirectoryChoice {
                path: entry_path,
                label: entry.file_name().to_string_lossy().into_owned(),
            }),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect {}: {error}",
                    entry_path.display()
                ));
            }
        }
    }
    directories.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(directories)
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::symlink,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn directory_listing_keeps_hidden_and_symlinked_directories_but_omits_files() {
        let root = std::env::temp_dir().join(format!(
            "svarm-directory-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("beta")).unwrap();
        fs::create_dir(root.join("Alpha")).unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join("file"), b"not a directory").unwrap();
        symlink(root.join("beta"), root.join("linked")).unwrap();

        let entries = read_directories(&root).unwrap();
        let labels = entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, [".hidden", "Alpha", "beta", "linked"]);

        fs::remove_dir_all(root).unwrap();
    }
}
