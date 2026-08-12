//! Reads the working directory of a live process.
//!
//! Agents move between checkouts on their own — a coding agent that enters a linked worktree
//! changes its own directory — so the directory an agent was launched in stops describing where
//! it is working. This is the platform edge for that observation; callers fall back to the launch
//! directory when it reports nothing.

use std::path::PathBuf;

/// The current working directory of `pid`, or `None` when it cannot be observed: the process is
/// gone, the read is not permitted, or the platform exposes no way to ask.
#[cfg(target_os = "linux")]
pub(crate) fn of_process(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn of_process(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn reports_the_directory_of_a_live_process() {
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        assert_eq!(of_process(pid), Some(expected));
    }

    #[test]
    fn reports_nothing_for_a_process_that_does_not_exist() {
        assert_eq!(of_process(-1), None);
    }
}
