use std::{path::Path, process::Command};

use crate::protocol::GitContext;

pub(crate) fn context(path: &Path) -> Option<GitContext> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel", "--abbrev-ref", "HEAD"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse(&String::from_utf8(output.stdout).ok()?)
}

fn parse(output: &str) -> Option<GitContext> {
    let mut lines = output.lines();
    let worktree = lines.next()?.trim();
    let branch = lines.next()?.trim();
    if worktree.is_empty() || branch.is_empty() {
        return None;
    }
    Some(GitContext {
        branch: if branch == "HEAD" {
            "detached".into()
        } else {
            branch.into()
        },
        worktree: worktree.into(),
    })
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
            parse("/tmp/project\nfeature/sidebar\n"),
            Some(GitContext {
                branch: "feature/sidebar".into(),
                worktree: "/tmp/project".into(),
            })
        );
        assert_eq!(parse("/tmp/project\nHEAD\n").unwrap().branch, "detached");
        assert_eq!(parse("fatal: not a repository\n"), None);
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
        assert_eq!(context(&root).unwrap().branch, "main");

        run(&root, &["switch", "-q", "-c", "feature/sidebar"]);
        assert_eq!(context(&root).unwrap().branch, "feature/sidebar");

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
