use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Deserialize;
use svarm_agent::{
    AgentId, PtySize, SessionStatus, TerminalNotifier, TerminalPalette, TerminalProcess,
    TerminalProcessSnapshot,
};

const HARVEST_INTERVAL: Duration = Duration::from_millis(250);
const HTTP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_SNIPPET_LINES: usize = 6;
const MAX_SNIPPET_CHARS: usize = 300;
const MAX_PROMPT_CHARS: usize = 64 * 1024;
const TRUNCATION_MARKER: &str = "\n\n(truncated)";

#[derive(Debug)]
pub(crate) enum HunkLaunchError {
    NotFound,
    NotRepository,
    NoChanges,
    Failed(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct HunkComment {
    file_path: String,
    body: String,
    #[serde(default)]
    old_range: Option<[usize; 2]>,
    #[serde(default)]
    new_range: Option<[usize; 2]>,
}

#[derive(Deserialize)]
struct HunkComments {
    comments: Vec<HunkComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HunkSession {
    session_id: String,
    repo_root: Option<String>,
    launched_at: Option<String>,
}

#[derive(Deserialize)]
struct HunkSessions {
    sessions: Vec<HunkSession>,
}

pub(crate) struct HunkReview {
    terminal: TerminalProcess,
    target_agent: AgentId,
    root: PathBuf,
    comments_url: String,
    session_id: Option<String>,
    comments: Vec<HunkComment>,
    comments_read: bool,
    next_harvest: Instant,
}

pub(crate) struct HunkReviewResult {
    pub target_agent: AgentId,
    pub prompt: Option<String>,
    pub error: Option<String>,
}

impl HunkReview {
    pub fn spawn(
        target_agent: AgentId,
        directory: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, HunkLaunchError> {
        let root = repository_root(directory)?;
        ensure_changes(&root)?;
        let args = [OsString::from("diff"), OsString::from("--watch")];
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
        let terminal = TerminalProcess::spawn_with_environment(
            OsStr::new("hunk"),
            &args,
            &root,
            size,
            palette,
            &environment,
            Some(notify) as Option<TerminalNotifier>,
        )
        .map_err(|error| {
            if program_not_found(OsStr::new("hunk")) {
                HunkLaunchError::NotFound
            } else {
                HunkLaunchError::Failed(format!("could not launch Hunk: {error}"))
            }
        })?;

        Ok(Self {
            terminal,
            target_agent,
            root,
            comments_url: hunk_comments_url(),
            session_id: None,
            comments: Vec::new(),
            comments_read: false,
            next_harvest: Instant::now(),
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

    pub fn next_timeout(&self, now: Instant) -> Duration {
        self.next_harvest.saturating_duration_since(now)
    }

    pub fn poll(&mut self, now: Instant) -> Option<HunkReviewResult> {
        if now >= self.next_harvest {
            self.harvest_comments();
            self.next_harvest = now + HARVEST_INTERVAL;
        }

        let snapshot = self.terminal.snapshot();
        if let Some(error) = snapshot.read_error {
            let _ = self.terminal.stop();
            return Some(self.finish(Some(format!("could not read Hunk output: {error}"))));
        }
        match self.terminal.poll() {
            Ok(SessionStatus::Running) => None,
            Ok(SessionStatus::Exited) => {
                let snapshot = self.terminal.snapshot();
                let error = snapshot.exit.and_then(|exit| {
                    (!exit.success).then(|| format!("Hunk exited with code {}", exit.code))
                });
                Some(self.finish(error))
            }
            Err(error) => Some(self.finish(Some(format!("could not poll Hunk: {error}")))),
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.terminal.stop().map_err(|error| error.to_string())
    }

    fn harvest_comments(&mut self) {
        let Some(root) = self.root.to_str() else {
            return;
        };
        if self.session_id.is_none() {
            let Some(body) = self.post_json(&serde_json::json!({ "action": "list" })) else {
                return;
            };
            let Ok(sessions) = serde_json::from_str::<HunkSessions>(&body) else {
                return;
            };
            self.session_id = newest_session(sessions.sessions, root);
        }
        let Some(session_id) = self.session_id.as_deref() else {
            return;
        };
        let body = serde_json::json!({
            "action": "comment-list",
            "selector": { "sessionId": session_id },
            "type": "user",
        });
        let Some(body) = self.post_json(&body) else {
            return;
        };
        let Ok(snapshot) = serde_json::from_str::<HunkComments>(&body) else {
            return;
        };
        self.comments = snapshot.comments;
        self.comments_read = true;
    }

    fn post_json(&self, body: &serde_json::Value) -> Option<String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .user_agent(concat!("svarm/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut response = agent
            .post(&self.comments_url)
            .header("Content-Type", "application/json")
            .send(serde_json::to_vec(body).unwrap_or_default())
            .ok()?;
        response.body_mut().read_to_string().ok()
    }

    pub fn into_result(self, error: Option<String>) -> HunkReviewResult {
        self.finish(error)
    }

    fn finish(&self, error: Option<String>) -> HunkReviewResult {
        let error = error.or_else(|| {
            (!self.comments_read).then(|| {
                "Hunk closed before its review comments could be read; update Hunk and try again"
                    .to_owned()
            })
        });
        HunkReviewResult {
            target_agent: self.target_agent,
            prompt: (!self.comments.is_empty())
                .then(|| format_review_prompt(&self.comments, &self.root)),
            error,
        }
    }
}

fn repository_root(directory: &Path) -> Result<PathBuf, HunkLaunchError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| {
            HunkLaunchError::Failed(format!("could not inspect repository: {error}"))
        })?;
    if !output.status.success() {
        return Err(HunkLaunchError::NotRepository);
    }
    let root = String::from_utf8(output.stdout)
        .map_err(|_| HunkLaunchError::Failed("repository path is not valid UTF-8".into()))?;
    let root = PathBuf::from(root.trim_end());
    root.canonicalize()
        .map_err(|error| HunkLaunchError::Failed(format!("could not open repository: {error}")))
}

fn ensure_changes(root: &Path) -> Result<(), HunkLaunchError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|error| HunkLaunchError::Failed(format!("could not inspect changes: {error}")))?;
    if !output.status.success() {
        return Err(HunkLaunchError::Failed(
            "could not inspect working-tree changes".into(),
        ));
    }
    if output.stdout.is_empty() {
        return Err(HunkLaunchError::NoChanges);
    }
    Ok(())
}

fn program_not_found(program: &OsStr) -> bool {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return !path.exists();
    }
    !env::var_os("PATH").is_some_and(|search| {
        env::split_paths(&search).any(|directory| directory.join(path).is_file())
    })
}

fn hunk_comments_url() -> String {
    let host = env::var("HUNK_MCP_HOST")
        .ok()
        .filter(|host| !host.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1".into());
    let port = env::var("HUNK_MCP_PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(47_657);
    format!("http://{host}:{port}/session-api")
}

fn newest_session(sessions: Vec<HunkSession>, root: &str) -> Option<String> {
    sessions
        .into_iter()
        .filter(|session| session.repo_root.as_deref() == Some(root))
        .max_by(|left, right| left.launched_at.cmp(&right.launched_at))
        .map(|session| session.session_id)
}

fn format_review_prompt(comments: &[HunkComment], root: &Path) -> String {
    let mut prompt = String::from("Review comments:\n\n");
    for (index, comment) in comments.iter().enumerate() {
        if index > 0 {
            prompt.push('\n');
        }
        prompt.push_str(&comment.file_path);
        prompt.push('\n');
        if let Some(range) = comment.new_range {
            let snippet = read_snippet(root, &comment.file_path, range);
            if snippet.is_empty() {
                prompt.push_str(&format_range(range, false));
                prompt.push('\n');
            } else {
                for (line, text) in snippet {
                    prompt.push_str(&format!("L{line} {text}\n"));
                }
            }
        } else if let Some(range) = comment.old_range {
            prompt.push_str(&format_range(range, true));
            prompt.push('\n');
        }
        prompt.push_str("comment: ");
        prompt.push_str(&comment.body);
        prompt.push('\n');
    }

    let prompt: String = prompt
        .chars()
        .filter(|character| *character == '\n' || *character == '\t' || !character.is_control())
        .collect();
    if prompt.len() <= MAX_PROMPT_CHARS {
        prompt
    } else {
        let end = floor_char_boundary(&prompt, MAX_PROMPT_CHARS - TRUNCATION_MARKER.len());
        format!("{}{TRUNCATION_MARKER}", &prompt[..end])
    }
}

fn read_snippet(root: &Path, file_path: &str, range: [usize; 2]) -> Vec<(usize, String)> {
    let relative = Path::new(file_path);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Vec::new();
    }
    let Ok(contents) = fs::read_to_string(root.join(relative)) else {
        return Vec::new();
    };
    let start = range[0].max(1);
    let end = range[1].max(start).min(start + MAX_SNIPPET_LINES - 1);
    let mut characters = 0;
    contents
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(end - start + 1)
        .map_while(|(index, line)| {
            let remaining = MAX_SNIPPET_CHARS.saturating_sub(characters);
            if remaining == 0 {
                return None;
            }
            let text: String = line.chars().take(remaining).collect();
            characters += text.chars().count();
            Some((index + 1, text))
        })
        .collect()
}

fn format_range([start, end]: [usize; 2], removed: bool) -> String {
    let lines = if start == end {
        format!("L{start}")
    } else {
        format!("L{start}-L{end}")
    };
    if removed {
        format!("{lines} (removed)")
    } else {
        lines
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_comment_response() {
        let response = r#"{"comments":[{"noteId":"user:1","source":"user","filePath":"src/lib.rs","newRange":[77,78],"body":"Use the helper.","editable":true}]}"#;
        let parsed: HunkComments = serde_json::from_str(response).unwrap();
        assert_eq!(
            parsed.comments,
            vec![HunkComment {
                file_path: "src/lib.rs".into(),
                body: "Use the helper.".into(),
                old_range: None,
                new_range: Some([77, 78]),
            }]
        );
    }

    #[test]
    fn identifies_the_newest_session_for_the_reviewed_repository() {
        let response = r#"{"sessions":[{"sessionId":"other","repoRoot":"/other","launchedAt":"2026-08-15T09:00:00Z"},{"sessionId":"older","repoRoot":"/repo","launchedAt":"2026-08-15T09:00:00Z"},{"sessionId":"review","repoRoot":"/repo","launchedAt":"2026-08-15T09:00:01Z"}]}"#;
        let parsed: HunkSessions = serde_json::from_str(response).unwrap();
        assert_eq!(
            newest_session(parsed.sessions, "/repo").as_deref(),
            Some("review")
        );
    }

    #[test]
    fn formats_ranges_and_comments_without_control_sequences() {
        let comments = vec![
            HunkComment {
                file_path: "src/lib.rs".into(),
                body: "Use the helper.\u{1b}[31m".into(),
                old_range: None,
                new_range: Some([77, 78]),
            },
            HunkComment {
                file_path: "old.rs".into(),
                body: "Keep this behavior.".into(),
                old_range: Some([4, 6]),
                new_range: None,
            },
        ];
        assert_eq!(
            format_review_prompt(&comments, Path::new("/does-not-exist")),
            "Review comments:\n\nsrc/lib.rs\nL77-L78\ncomment: Use the helper.[31m\n\nold.rs\nL4-L6 (removed)\ncomment: Keep this behavior.\n"
        );
    }

    #[test]
    fn rejects_snippet_paths_outside_the_repository() {
        assert!(read_snippet(Path::new("/tmp"), "../secret", [1, 1]).is_empty());
    }
}
