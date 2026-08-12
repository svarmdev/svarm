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
    let mut context = parse(&String::from_utf8(output.stdout).ok()?)?;
    if let Some(output) = git_output(path, &["diff", "--numstat", "HEAD", "--"])
        .or_else(|| git_output(path, &["diff", "--cached", "--numstat", "--"]))
    {
        (context.additions, context.deletions) = parse_diff(&output);
    }
    if let Some(output) = git_output(
        path,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    ) {
        (context.ahead, context.behind) = parse_tracking(&output);
    }
    Some(context)
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
    Some(GitContext {
        branch: if branch == "HEAD" {
            "detached".into()
        } else {
            branch.into()
        },
        worktree: worktree.into(),
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
            parse("/tmp/project\nfeature/sidebar\n"),
            Some(GitContext {
                branch: "feature/sidebar".into(),
                worktree: "/tmp/project".into(),
                additions: 0,
                deletions: 0,
                ahead: None,
                behind: None,
            })
        );
        assert_eq!(parse("/tmp/project\nHEAD\n").unwrap().branch, "detached");
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
        assert_eq!(context(&root).unwrap().branch, "main");

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
        let feature_context = context(&root).unwrap();
        assert_eq!(feature_context.branch, "feature/sidebar");
        assert_eq!(
            (feature_context.additions, feature_context.deletions),
            (2, 1)
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
