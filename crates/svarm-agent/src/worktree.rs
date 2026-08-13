use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const CREATE_ATTEMPTS: u32 = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

pub fn repository_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| {
            let git = candidate.join(".git");
            git.is_file() || git.is_dir()
        })
        .map(Path::to_path_buf)
}

pub fn root() -> Option<PathBuf> {
    env::var_os("SVARM_WORKTREE_ROOT")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".svarm/worktrees")))
}

fn branch_name(token: &str) -> String {
    format!("svarm/{token}")
}

fn worktree_path(root: &Path, repository_name: &str, branch: &str) -> PathBuf {
    root.join(repository_name).join(branch.replace('/', "-"))
}

fn new_token(attempt: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
        .wrapping_add(u128::from(attempt) << 32);
    let mixed = nanos.wrapping_add(u128::from(std::process::id()));
    format!("{:08x}", (mixed & 0xffff_ffff) as u32)
}

pub fn create(checkout: &Path) -> Result<Worktree, String> {
    let repository = repository_root(checkout)
        .ok_or_else(|| format!("{} is not a git repository", checkout.display()))?;
    let repository_name = repository
        .file_name()
        .ok_or_else(|| format!("repository path {} has no name", repository.display()))?
        .to_string_lossy();
    let root = root().ok_or_else(|| {
        "could not determine worktree root (set HOME or SVARM_WORKTREE_ROOT)".to_string()
    })?;

    let mut last_error = String::from("could not allocate a unique worktree path");
    for attempt in 0..CREATE_ATTEMPTS {
        let branch = branch_name(&new_token(attempt));
        let path = worktree_path(&root, &repository_name, &branch);
        if path.exists() {
            last_error = format!("{} already exists", path.display());
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        match add_worktree(checkout, &branch, &path) {
            Ok(()) => return Ok(Worktree { path, branch }),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn add_worktree(checkout: &Path, branch: &str, path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["worktree", "add", "-b", branch])
        .arg(path)
        .arg("HEAD")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|error| format!("could not run git worktree add: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        Err(format!("git worktree add failed with {}", output.status))
    } else {
        Err(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        sync::Mutex,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn branch_name_prefixes_the_token() {
        assert_eq!(branch_name("deadbeef"), "svarm/deadbeef");
    }

    #[test]
    fn worktree_path_replaces_slashes_in_the_branch() {
        assert_eq!(
            worktree_path(Path::new("/wt"), "svarm", "svarm/deadbeef"),
            PathBuf::from("/wt/svarm/svarm-deadbeef")
        );
        assert_eq!(
            worktree_path(Path::new("/wt"), "repo", "feature/x"),
            PathBuf::from("/wt/repo/feature-x")
        );
    }

    #[test]
    fn root_honours_svarm_worktree_root() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let previous = env::var_os("SVARM_WORKTREE_ROOT");
        let custom = unique_temp("worktree-root");
        // SAFETY: ENV_LOCK serializes env mutation in these tests.
        unsafe { env::set_var("SVARM_WORKTREE_ROOT", &custom) };
        assert_eq!(root(), Some(custom));
        restore_env("SVARM_WORKTREE_ROOT", previous);
    }

    #[test]
    fn repository_root_walks_up_and_returns_none_outside_a_repo() {
        let repo = unique_temp("worktree-repo");
        fs::create_dir_all(repo.join("src/nested")).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(
            repository_root(&repo.join("src/nested")),
            Some(repo.clone())
        );
        assert_eq!(repository_root(&repo), Some(repo.clone()));

        let linked = unique_temp("worktree-linked");
        fs::create_dir(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            "gitdir: /tmp/example/.git/worktrees/linked\n",
        )
        .unwrap();
        assert_eq!(repository_root(&linked), Some(linked.clone()));

        // /tmp is a git repository on some hosts, so a temp directory is not
        // a reliable "outside a repo" path. /proc is never a checkout.
        assert_eq!(repository_root(Path::new("/proc")), None);

        fs::remove_dir_all(repo).unwrap();
        fs::remove_dir_all(linked).unwrap();
    }

    #[test]
    fn create_adds_a_linked_worktree_on_a_generated_branch() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let previous = env::var_os("SVARM_WORKTREE_ROOT");
        let root = unique_temp("worktree-create-repo");
        let worktree_root = unique_temp("worktree-create-root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&worktree_root).unwrap();
        // SAFETY: ENV_LOCK serializes env mutation in these tests.
        unsafe { env::set_var("SVARM_WORKTREE_ROOT", &worktree_root) };

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

        let created = create(&root).expect("worktree create");
        let context = crate::git::context(&created.path).expect("git context");
        assert!(context.linked);
        assert_eq!(context.branch, created.branch);
        assert!(created.branch.starts_with("svarm/"));
        assert_eq!(created.branch.len(), "svarm/".len() + 8);
        assert!(created.path.starts_with(&worktree_root));

        let _ = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["worktree", "remove", "--force"])
            .arg(&created.path)
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["branch", "-D"])
            .arg(&created.branch)
            .status();
        restore_env("SVARM_WORKTREE_ROOT", previous);
        let _ = fs::remove_dir_all(worktree_root);
        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "svarm-{label}-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn restore_env(key: &str, previous: Option<OsString>) {
        // SAFETY: ENV_LOCK serializes env mutation in these tests.
        match previous {
            Some(value) => unsafe { env::set_var(key, value) },
            None => unsafe { env::remove_var(key) },
        }
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
