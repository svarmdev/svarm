use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, Result,
    protocol::{ArchivedConversation, SessionId},
};

const INDEX_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ConversationIndex {
    version: u8,
    pub(crate) session_id: SessionId,
    pub(crate) active: Vec<ActiveConversation>,
    pub(crate) archived: Vec<ArchivedConversation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ActiveConversation {
    pub(crate) conversation_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) kind: AgentKind,
    pub(crate) launch_directory: PathBuf,
}

impl ConversationIndex {
    pub(crate) fn new(
        session_id: SessionId,
        active: Vec<ActiveConversation>,
        archived: Vec<ArchivedConversation>,
    ) -> Self {
        Self {
            version: INDEX_VERSION,
            session_id,
            active,
            archived,
        }
    }

    pub(crate) fn restored_conversations(self) -> Vec<ArchivedConversation> {
        let mut seen = HashSet::new();
        self.active
            .into_iter()
            .filter_map(|conversation| {
                Some(ArchivedConversation {
                    conversation_id: conversation.conversation_id?,
                    title: conversation.title?,
                    kind: conversation.kind,
                    launch_directory: conversation.launch_directory,
                })
            })
            .chain(self.archived)
            .filter(|conversation| seen.insert(conversation.conversation_id.clone()))
            .collect()
    }
}

pub(crate) struct ConversationStore {
    directory: PathBuf,
    saved: BTreeMap<SessionId, ConversationIndex>,
}

impl ConversationStore {
    pub(crate) fn open(directory: PathBuf) -> Result<(Self, Vec<ConversationIndex>)> {
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let mut saved = BTreeMap::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            match read_index(&path) {
                Ok(index) if index.version == INDEX_VERSION => {
                    saved.insert(index.session_id, index);
                }
                Ok(_) => eprintln!("ignoring unsupported conversation index {}", path.display()),
                Err(error) => eprintln!(
                    "ignoring unreadable conversation index {}: {error}",
                    path.display()
                ),
            }
        }
        let indexes = saved.values().cloned().collect();
        Ok((Self { directory, saved }, indexes))
    }

    pub(crate) fn sync(&mut self, indexes: Vec<ConversationIndex>) -> Result<()> {
        let current = indexes
            .into_iter()
            .map(|index| (index.session_id, index))
            .collect::<BTreeMap<_, _>>();
        for (&session_id, index) in &current {
            if index.active.is_empty() && index.archived.is_empty() {
                self.remove(session_id)?;
            } else if self.saved.get(&session_id) != Some(index) {
                self.write(index)?;
            }
        }
        for session_id in self.saved.keys().copied().collect::<Vec<_>>() {
            if !current.contains_key(&session_id) {
                self.remove(session_id)?;
            }
        }
        self.saved = current
            .into_iter()
            .filter(|(_, index)| !index.active.is_empty() || !index.archived.is_empty())
            .collect();
        Ok(())
    }

    fn write(&self, index: &ConversationIndex) -> Result<()> {
        let destination = self.path(index.session_id);
        let temporary = destination.with_extension(format!("json.{}.tmp", std::process::id()));
        let result = (|| -> Result<()> {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            serde_json::to_writer_pretty(&file, index)?;
            file.sync_all()?;
            fs::rename(&temporary, &destination)?;
            sync_directory(&self.directory)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn remove(&mut self, session_id: SessionId) -> Result<()> {
        match fs::remove_file(self.path(session_id)) {
            Ok(()) => sync_directory(&self.directory)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.saved.remove(&session_id);
        Ok(())
    }

    fn path(&self, session_id: SessionId) -> PathBuf {
        self.directory.join(format!("{}.json", session_id.0))
    }
}

fn read_index(path: &Path) -> Result<ConversationIndex> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    Ok(serde_json::from_reader(file)?)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    let directory = File::open(path)?;
    // SAFETY: fsync receives a valid open file descriptor.
    let result = unsafe { libc::fsync(directory.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, time::SystemTime};

    use super::*;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "svarm-conversation-store-test-{}-{:?}",
            std::process::id(),
            SystemTime::now()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn conversation(id: &str, title: &str) -> ArchivedConversation {
        ArchivedConversation {
            conversation_id: id.into(),
            title: title.into(),
            kind: AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/workspace"),
        }
    }

    fn active(id: Option<&str>, title: Option<&str>) -> ActiveConversation {
        ActiveConversation {
            conversation_id: id.map(str::to_owned),
            title: title.map(str::to_owned),
            kind: AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/workspace"),
        }
    }

    #[test]
    fn atomically_round_trips_private_session_indexes_and_removes_empty_ones() {
        let directory = temp_dir();
        let (mut store, restored) = ConversationStore::open(directory.clone()).unwrap();
        assert!(restored.is_empty());

        let index = ConversationIndex::new(
            SessionId(7),
            vec![active(Some("active"), Some("Active"))],
            vec![conversation("archived", "Archived")],
        );
        store.sync(vec![index.clone()]).unwrap();
        let path = directory.join("7.json");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let (_, restored) = ConversationStore::open(directory.clone()).unwrap();
        assert_eq!(restored, vec![index]);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );

        store.sync(Vec::new()).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restored_active_conversations_become_archived_without_duplicates() {
        let duplicate = conversation("same", "Current title");
        let index = ConversationIndex::new(
            SessionId(1),
            vec![
                active(Some("same"), Some("Current title")),
                active(Some("id-only"), None),
                active(None, Some("Title only")),
            ],
            vec![
                conversation("same", "Old title"),
                conversation("other", "Other"),
            ],
        );
        assert_eq!(
            index.restored_conversations(),
            vec![duplicate, conversation("other", "Other")]
        );
    }
}
