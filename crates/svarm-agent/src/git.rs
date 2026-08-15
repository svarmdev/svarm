use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    thread,
};

#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

use crate::protocol::GitContext;

pub(crate) fn context(path: &Path) -> Option<GitContext> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "rev-parse",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
            "--git-dir",
            "--git-common-dir",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut context = parse(&String::from_utf8(output.stdout).ok()?)?;
    if let Some(output) = git_output(path, &["diff", "--numstat", "HEAD", "--"])
        .or_else(|| git_output(path, &["diff", "--cached", "--numstat", "--"]))
    {
        (context.additions, context.deletions) = parse_diff(&output);
    }
    context.additions = context.additions.saturating_add(untracked_additions(path));
    if let Some(output) = git_output(
        path,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    ) {
        (context.ahead, context.behind) = parse_tracking(&output);
    }
    Some(context)
}

pub(crate) struct ContextResult {
    pub directory: PathBuf,
    pub context: Option<GitContext>,
}

/// Runs Git probes away from the server coordinator. One worker is enough: serial execution also
/// prevents several agents from launching their Git subprocesses at the same instant.
pub(crate) struct ContextWorker {
    requests: SyncSender<PathBuf>,
    results: Receiver<ContextResult>,
}

impl ContextWorker {
    pub fn new() -> Self {
        let (requests, request_rx) = sync_channel::<PathBuf>(1);
        let (result_tx, results) = sync_channel(1);
        thread::spawn(move || {
            while let Ok(directory) = request_rx.recv() {
                let context = context(&directory);
                if result_tx
                    .send(ContextResult { directory, context })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { requests, results }
    }

    pub fn request(&self, directory: PathBuf) -> bool {
        self.requests.try_send(directory).is_ok()
    }

    pub fn try_result(&self) -> Option<ContextResult> {
        self.results.try_recv().ok()
    }
}

fn git_output(path: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn parse(output: &str) -> Option<GitContext> {
    let mut lines = output.lines();
    let worktree = lines.next()?.trim();
    let branch = lines.next()?.trim();
    if worktree.is_empty() || branch.is_empty() {
        return None;
    }
    // A linked worktree keeps its own git directory under the repository's common one; the main
    // checkout reports the same path twice.
    let git_directory = lines.next().map(str::trim);
    let common_directory = lines.next().map(str::trim);
    Some(GitContext {
        branch: if branch == "HEAD" {
            "detached".into()
        } else {
            branch.into()
        },
        worktree: worktree.into(),
        linked: git_directory.is_some() && git_directory != common_directory,
        additions: 0,
        deletions: 0,
        ahead: None,
        behind: None,
    })
}

fn parse_diff(output: &[u8]) -> (u64, u64) {
    output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut fields = line.split(|byte| *byte == b'\t');
            let additions = std::str::from_utf8(fields.next()?)
                .ok()?
                .parse::<u64>()
                .ok()?;
            let deletions = std::str::from_utf8(fields.next()?)
                .ok()?
                .parse::<u64>()
                .ok()?;
            Some((additions, deletions))
        })
        .fold((0_u64, 0_u64), |totals, change| {
            (
                totals.0.saturating_add(change.0),
                totals.1.saturating_add(change.1),
            )
        })
}

fn untracked_additions(path: &Path) -> u64 {
    let Some(output) = git_output(
        path,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    ) else {
        return 0;
    };
    output
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .filter_map(git_path)
        .filter_map(|relative| text_lines(&path.join(relative)))
        .fold(0_u64, u64::saturating_add)
}

#[cfg(unix)]
fn git_path(name: &[u8]) -> Option<PathBuf> {
    Some(OsString::from_vec(name.to_vec()).into())
}

#[cfg(not(unix))]
fn git_path(name: &[u8]) -> Option<PathBuf> {
    String::from_utf8(name.to_vec()).ok().map(PathBuf::from)
}

fn text_lines(path: &Path) -> Option<u64> {
    if !path.symlink_metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut buffer = [0_u8; 8 * 1024];
    let mut lines = 0_u64;
    let mut last = None;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        if bytes.contains(&0) {
            return None;
        }
        lines = lines.saturating_add(bytes.iter().filter(|byte| **byte == b'\n').count() as u64);
        last = bytes.last().copied();
    }
    Some(lines.saturating_add(u64::from(last.is_some_and(|byte| byte != b'\n'))))
}

fn parse_tracking(output: &[u8]) -> (Option<u64>, Option<u64>) {
    let mut counts = output.split(|byte| byte.is_ascii_whitespace());
    let ahead = counts
        .next()
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse().ok());
    let behind = counts
        .next()
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse().ok());
    match (ahead, behind) {
        (Some(ahead), Some(behind)) => (Some(ahead), Some(behind)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn parses_branch_and_detached_worktree_contexts() {
        assert_eq!(
            parse("/tmp/project\nfeature/sidebar\n.git\n.git\n"),
            Some(GitContext {
                branch: "feature/sidebar".into(),
                worktree: "/tmp/project".into(),
                linked: false,
                additions: 0,
                deletions: 0,
                ahead: None,
                behind: None,
            })
        );
        assert!(
            parse("/tmp/linked\nlinked-branch\n/tmp/project/.git/worktrees/linked\n/tmp/project/.git\n")
                .unwrap()
                .linked
        );
        assert_eq!(parse("/tmp/project\nHEAD\n").unwrap().branch, "detached");
        assert!(!parse("/tmp/project\nHEAD\n").unwrap().linked);
        assert_eq!(parse("fatal: not a repository\n"), None);
    }

    #[test]
    fn parses_worktree_diff_and_upstream_counts() {
        assert_eq!(
            parse_diff(b"557\t300\tsrc/main.rs\n-\t-\tasset.bin\n"),
            (557, 300)
        );
        assert_eq!(parse_tracking(b"2\t4\n"), (Some(2), Some(4)));
        assert_eq!(parse_tracking(b""), (None, None));
    }

    #[test]
    fn discovers_branch_changes_and_linked_worktrees() {
        let root = std::env::temp_dir().join(format!(
            "svarm-git-context-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let linked = root.with_extension("linked");
        fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        run(
            &root,
            &[
                "-c",
                "user.name=Svarm Test",
                "-c",
                "user.email=svarm@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let main_context = context(&root).unwrap();
        assert_eq!(main_context.branch, "main");
        assert!(!main_context.linked);

        run(&root, &["switch", "-q", "-c", "feature/sidebar"]);
        fs::write(root.join("tracked.txt"), "old\n").unwrap();
        run(&root, &["add", "tracked.txt"]);
        run(
            &root,
            &[
                "-c",
                "user.name=Svarm Test",
                "-c",
                "user.email=svarm@example.invalid",
                "commit",
                "-q",
                "-m",
                "add tracked file",
            ],
        );
        fs::write(root.join("tracked.txt"), "new\nmore\n").unwrap();
        fs::write(root.join("untracked.txt"), "one\ntwo\nthree").unwrap();
        fs::write(root.join("binary.dat"), b"not text\0more").unwrap();
        fs::write(root.join(".git/info/exclude"), "ignored.txt\n").unwrap();
        fs::write(root.join("ignored.txt"), "ignored\nlines\n").unwrap();
        let feature_context = context(&root).unwrap();
        assert_eq!(feature_context.branch, "feature/sidebar");
        assert_eq!(
            (feature_context.additions, feature_context.deletions),
            (5, 1)
        );

        run(&root, &["add", "untracked.txt"]);
        let staged_context = context(&root).unwrap();
        assert_eq!(
            (staged_context.additions, staged_context.deletions),
            (5, 1),
            "staged files must not be counted twice"
        );

        run(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-branch",
                linked.to_str().unwrap(),
            ],
        );
        let linked_context = context(&linked).unwrap();
        assert_eq!(linked_context.branch, "linked-branch");
        assert_eq!(linked_context.worktree, linked.canonicalize().unwrap());
        assert!(linked_context.linked);

        fs::remove_dir_all(&linked).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    fn run(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}
