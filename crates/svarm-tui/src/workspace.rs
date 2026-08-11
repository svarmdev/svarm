use std::{
    ffi::{OsStr, OsString},
    fs,
    fs::OpenOptions,
    io,
    os::unix::{ffi::OsStringExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender, SyncSender},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use svarm_agent::{
    PtySize, TerminalNotifier, TerminalPalette, TerminalProcess, TerminalProcessSnapshot,
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

static NEXT_RESULT_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) enum YaziLaunchError {
    NotFound,
    Failed(String),
}

pub(crate) enum YaziResult {
    Selected(PathBuf),
    Cancelled,
    Failed(String),
}

pub(crate) struct YaziPicker {
    terminal: TerminalProcess,
    result_path: PathBuf,
}

impl YaziPicker {
    pub fn spawn(
        starting_directory: &Path,
        runtime_directory: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, YaziLaunchError> {
        Self::spawn_program(
            OsStr::new("yazi"),
            starting_directory,
            runtime_directory,
            size,
            palette,
            Some(notify),
        )
    }

    fn spawn_program(
        program: &OsStr,
        starting_directory: &Path,
        runtime_directory: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Option<TerminalNotifier>,
    ) -> Result<Self, YaziLaunchError> {
        let result_path = create_result_file(runtime_directory)
            .map_err(|error| YaziLaunchError::Failed(error.to_string()))?;
        let mut cwd_file = OsString::from("--cwd-file=");
        cwd_file.push(&result_path);
        let args = [starting_directory.as_os_str().to_owned(), cwd_file];
        let environment = [
            (
                OsString::from("TERM"),
                Some(OsString::from("xterm-256color")),
            ),
            (
                OsString::from("COLORTERM"),
                Some(OsString::from("truecolor")),
            ),
            (OsString::from("SVARM"), Some(OsString::from("1"))),
            (
                OsString::from("SVARM_EMBEDDED_TOOL"),
                Some(OsString::from("1")),
            ),
        ];
        let terminal = match TerminalProcess::spawn_with_environment(
            program,
            &args,
            starting_directory,
            size,
            palette,
            &environment,
            notify,
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = fs::remove_file(&result_path);
                return if program_not_found(program) {
                    Err(YaziLaunchError::NotFound)
                } else {
                    Err(YaziLaunchError::Failed(format!(
                        "could not launch Yazi: {error}"
                    )))
                };
            }
        };
        Ok(Self {
            terminal,
            result_path,
        })
    }

    pub fn snapshot(&self) -> TerminalProcessSnapshot {
        self.terminal.snapshot()
    }

    pub fn send(&self, bytes: &[u8]) -> Result<(), String> {
        self.terminal.send(bytes).map_err(|error| error.to_string())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.terminal
            .resize(rows, cols)
            .map_err(|error| error.to_string())
    }

    pub fn poll(&mut self) -> Result<svarm_agent::SessionStatus, String> {
        self.terminal.poll().map_err(|error| error.to_string())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.terminal.stop().map_err(|error| error.to_string())
    }

    pub fn finish(&self) -> YaziResult {
        let snapshot = self.terminal.snapshot();
        if let Some(error) = snapshot.read_error {
            return YaziResult::Failed(format!("could not read Yazi output: {error}"));
        }
        let bytes = match fs::read(&self.result_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return YaziResult::Failed(format!("could not read Yazi result: {error}"));
            }
        };
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        if bytes.is_empty() {
            return if snapshot.exit.is_some_and(|exit| !exit.success) {
                YaziResult::Failed("Yazi exited without selecting a workspace".into())
            } else {
                YaziResult::Cancelled
            };
        }
        let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
        match path.canonicalize() {
            Ok(path) if path.is_dir() => YaziResult::Selected(path),
            Ok(_) => YaziResult::Failed(format!(
                "Yazi selected a path that is not a directory: {}",
                path.display()
            )),
            Err(error) => YaziResult::Failed(format!(
                "could not use Yazi selection {}: {error}",
                path.display()
            )),
        }
    }
}

impl Drop for YaziPicker {
    fn drop(&mut self) {
        let _ = self.terminal.stop();
        let _ = fs::remove_file(&self.result_path);
    }
}

fn create_result_file(runtime_directory: &Path) -> io::Result<PathBuf> {
    for _ in 0..32 {
        let suffix = NEXT_RESULT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = runtime_directory.join(format!("yazi-cwd-{}-{suffix}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private Yazi result file",
    ))
}

fn program_not_found(program: &OsStr) -> bool {
    let path = Path::new(program);
    let program_exists = if path.is_absolute() || path.components().count() > 1 {
        path.exists()
    } else {
        std::env::var_os("PATH").is_some_and(|search| {
            std::env::split_paths(&search).any(|directory| directory.join(path).exists())
        })
    };
    !program_exists
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
        os::unix::fs::{PermissionsExt, symlink},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

    #[test]
    fn fake_yazi_returns_literal_directory_and_cleans_private_result_file() {
        let root = test_root();
        let selected = root.join("space ;$() ü");
        fs::create_dir(&selected).unwrap();
        let executable = fake_executable(
            &root,
            "select",
            "result=${2#--cwd-file=}\nprintf '%s\\n' \"$1\" > \"$result\"",
        );
        let mut picker = YaziPicker::spawn_program(
            executable.as_os_str(),
            &selected,
            &root,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            None,
            None,
        )
        .unwrap();
        let result_path = picker.result_path.clone();
        assert_eq!(
            fs::metadata(&result_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        wait_for_exit(&mut picker);
        assert!(matches!(picker.finish(), YaziResult::Selected(path) if path == selected));

        drop(picker);
        assert!(!result_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn yazi_not_found_is_the_only_launch_error_that_requests_fallback() {
        let root = test_root();
        for program in [
            root.join("not-installed").into_os_string(),
            OsString::from("svarm-yazi-test-definitely-not-installed"),
        ] {
            match YaziPicker::spawn_program(
                &program,
                &root,
                &root,
                PtySize {
                    rows: 10,
                    cols: 40,
                    pixel_width: 0,
                    pixel_height: 0,
                },
                None,
                None,
            ) {
                Err(YaziLaunchError::NotFound) => {}
                Err(YaziLaunchError::Failed(error)) => panic!("wrong launch error: {error}"),
                Ok(_) => panic!("missing executable unexpectedly launched"),
            }
        }
        let not_executable = root.join("not-executable");
        fs::write(&not_executable, b"not executable").unwrap();
        let denied = YaziPicker::spawn_program(
            not_executable.as_os_str(),
            &root,
            &root,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            None,
            None,
        );
        assert!(matches!(denied, Err(YaziLaunchError::Failed(_))));
        let broken_interpreter = fake_executable(
            &root,
            "broken-interpreter",
            "#! this line is ignored because the helper supplies a shebang",
        );
        fs::write(&broken_interpreter, b"#!/does/not/exist\n").unwrap();
        let invalid = YaziPicker::spawn_program(
            broken_interpreter.as_os_str(),
            &root,
            &root,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            None,
            None,
        );
        match invalid {
            Err(YaziLaunchError::Failed(_)) => {}
            Ok(mut picker) => {
                wait_for_exit(&mut picker);
                assert!(matches!(picker.finish(), YaziResult::Failed(_)));
            }
            Err(YaziLaunchError::NotFound) => {
                panic!("an existing executable was mistaken for an absent program")
            }
        }
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("yazi-cwd-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fake_yazi_distinguishes_cancel_invalid_result_and_nonzero_exit() {
        let root = test_root();
        for (name, script, expected) in [
            ("cancel", "exit 0", "cancel"),
            (
                "invalid",
                "result=${2#--cwd-file=}\nprintf '/does/not/exist\\n' > \"$result\"",
                "failed",
            ),
            ("nonzero", "exit 7", "failed"),
        ] {
            let executable = fake_executable(&root, name, script);
            let mut picker = YaziPicker::spawn_program(
                executable.as_os_str(),
                &root,
                &root,
                PtySize {
                    rows: 10,
                    cols: 40,
                    pixel_width: 0,
                    pixel_height: 0,
                },
                None,
                None,
            )
            .unwrap();
            wait_for_exit(&mut picker);
            assert_eq!(
                match picker.finish() {
                    YaziResult::Cancelled => "cancel",
                    YaziResult::Failed(_) => "failed",
                    YaziResult::Selected(_) => "selected",
                },
                expected
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn force_close_reaps_fake_yazi_and_drop_removes_its_result() {
        let root = test_root();
        let executable = fake_executable(&root, "long-running", "exec sleep 60");
        let mut picker = YaziPicker::spawn_program(
            executable.as_os_str(),
            &root,
            &root,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            None,
            None,
        )
        .unwrap();
        let result_path = picker.result_path.clone();

        picker.stop().unwrap();
        assert_eq!(picker.poll().unwrap(), svarm_agent::SessionStatus::Exited);
        drop(picker);
        assert!(!result_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn test_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "svarm-yazi-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn fake_executable(root: &Path, name: &str, script: &str) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn wait_for_exit(picker: &mut YaziPicker) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while picker.poll().unwrap() == svarm_agent::SessionStatus::Running
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(picker.poll().unwrap(), svarm_agent::SessionStatus::Exited);
    }
}
