//! Best-effort access to provider-owned conversation history.
//!
//! Providers persist their resumable conversations outside svarm. This adapter only reads the
//! first real user message, so a `/resume` inside a running agent can use the same naming flow as a
//! new conversation without making the provider transcript part of the application model.

use std::{
    env,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::AgentKind;

#[derive(Clone, Debug, Default)]
pub(crate) struct ConversationHistory {
    claude_projects: Option<PathBuf>,
    codex_sessions: Option<PathBuf>,
    grok_sessions: Option<PathBuf>,
    pi_sessions: Option<PathBuf>,
}

impl ConversationHistory {
    pub(crate) fn from_environment() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from);
        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".codex")));
        let grok_home = env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".grok")));
        let pi_home = env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".pi/agent")));
        Self {
            claude_projects: home.map(|home| home.join(".claude/projects")),
            codex_sessions: codex_home.map(|home| home.join("sessions")),
            grok_sessions: grok_home.map(|home| home.join("sessions")),
            pi_sessions: env::var_os("PI_CODING_AGENT_SESSION_DIR")
                .map(PathBuf::from)
                .or_else(|| pi_home.map(|home| home.join("sessions"))),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_home(home: &Path) -> Self {
        Self {
            claude_projects: Some(home.join(".claude/projects")),
            codex_sessions: Some(home.join(".codex/sessions")),
            grok_sessions: Some(home.join(".grok/sessions")),
            pi_sessions: Some(home.join(".pi/agent/sessions")),
        }
    }

    pub(crate) fn first_user_message(
        &self,
        kind: AgentKind,
        conversation_id: &str,
        working_directory: &Path,
    ) -> Option<String> {
        match kind {
            AgentKind::Claude => self
                .claude_projects
                .as_deref()
                .and_then(|root| find_file(root, |name| name == format!("{conversation_id}.jsonl")))
                .and_then(|path| first_line(&path, claude_prompt)),
            AgentKind::Codex => self
                .codex_sessions
                .as_deref()
                .and_then(|root| find_file(root, |name| name.contains(conversation_id)))
                .and_then(|path| first_line(&path, codex_prompt)),
            AgentKind::Grok => {
                let direct = self.grok_sessions.as_deref().and_then(|root| {
                    let path = root
                        .join(encode_grok_directory(working_directory))
                        .join(conversation_id)
                        .join("chat_history.jsonl");
                    path.is_file().then_some(path)
                });
                direct
                    .or_else(|| {
                        self.grok_sessions.as_deref().and_then(|root| {
                            find_file(root, |name| name == "chat_history.jsonl").filter(|path| {
                                path.parent().is_some_and(|parent| {
                                    parent
                                        .file_name()
                                        .is_some_and(|name| name == conversation_id)
                                })
                            })
                        })
                    })
                    .and_then(|path| first_line(&path, grok_prompt))
            }
            AgentKind::Pi => self
                .pi_sessions
                .as_deref()
                .and_then(|root| find_file(root, |name| name.contains(conversation_id)))
                .and_then(|path| first_line(&path, pi_prompt)),
        }
    }
}

fn find_file(root: &Path, matches: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(&matches)
            {
                return Some(path);
            }
        }
    }
    None
}

fn first_line<T>(path: &Path, parse: impl Fn(&Value) -> Option<T>) -> Option<T> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(prompt) = parse(&value) {
            return Some(prompt);
        }
    }
    None
}

fn claude_prompt(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "user"
        || value.get("isMeta").and_then(Value::as_bool) == Some(true)
        || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || value.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    normalize_prompt(text_content(content)?)
}

fn grok_prompt(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "user" || value.get("synthetic_reason").is_some() {
        return None;
    }
    normalize_prompt(text_content(value.get("content")?)?)
}

fn codex_prompt(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "event_msg"
        || value.get("payload")?.get("type")?.as_str()? != "user_message"
    {
        return None;
    }
    normalize_prompt(value.get("payload")?.get("message")?.as_str()?)
}

fn pi_prompt(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "message"
        || value.get("message")?.get("role")?.as_str()? != "user"
    {
        return None;
    }
    normalize_prompt(text_content(value.get("message")?.get("content")?)?)
}

fn text_content(value: &Value) -> Option<&str> {
    if let Some(text) = value.as_str() {
        return Some(text);
    }
    let parts = value.as_array()?;
    if parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("tool_result"))
    {
        return None;
    }
    parts
        .iter()
        .find(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
}

fn normalize_prompt(prompt: &str) -> Option<String> {
    let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    (!prompt.is_empty() && !prompt.starts_with('/')).then_some(prompt)
}

fn encode_grok_directory(directory: &Path) -> String {
    directory.to_string_lossy().replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_home() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "svarm-history-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn finds_the_first_claude_user_prompt_and_skips_meta_messages() {
        let home = temp_home();
        let id = "019ff1d3-375e-4a72-a176-c47497827e49";
        write(
            &home.join(format!(".claude/projects/work/{id}.jsonl")),
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"/init\"}}\n{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"fix   the sidebar\"}]}}\n",
        );

        let history = ConversationHistory::from_home(&home);
        assert_eq!(
            history.first_user_message(AgentKind::Claude, id, Path::new("/work")),
            Some("fix the sidebar".into())
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn finds_a_grok_prompt_in_a_session_outside_the_current_directory() {
        let home = temp_home();
        let id = "019ff1d3-375e-4a72-a176-c47497827e49";
        write(
            &home.join(format!(".grok/sessions/%2Fother/{id}/chat_history.jsonl")),
            "{\"type\":\"user\",\"synthetic_reason\":\"system_reminder\",\"content\":[{\"type\":\"text\",\"text\":\"system\"}]}\n{\"type\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"resume the release work\"}]}\n",
        );

        let history = ConversationHistory::from_home(&home);
        assert_eq!(
            history.first_user_message(AgentKind::Grok, id, Path::new("/current")),
            Some("resume the release work".into())
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn finds_a_codex_prompt_in_a_rollout_file() {
        let home = temp_home();
        let id = "019ff1d3-375e-4a72-a176-c47497827e49";
        write(
            &home.join(format!(".codex/sessions/2026/08/13/rollout-{id}.jsonl")),
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"add tests to the parser\"}}\n",
        );

        let history = ConversationHistory::from_home(&home);
        assert_eq!(
            history.first_user_message(AgentKind::Codex, id, Path::new("/work")),
            Some("add tests to the parser".into())
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn finds_a_pi_prompt_in_a_session_file() {
        let home = temp_home();
        let id = "019ff1d3-375e-4a72-a176-c47497827e49";
        write(
            &home.join(format!(
                ".pi/agent/sessions/--tmp-project--/20260813_{id}.jsonl"
            )),
            "{\"type\":\"session\",\"version\":3,\"id\":\"019ff1d3-375e-4a72-a176-c47497827e49\",\"cwd\":\"/tmp/project\"}\n{\"type\":\"message\",\"id\":\"a1b2c3d4\",\"parentId\":null,\"timestamp\":\"2026-08-13T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"fix   the Pi adapter\"}}\n",
        );

        let history = ConversationHistory::from_home(&home);
        assert_eq!(
            history.first_user_message(AgentKind::Pi, id, Path::new("/tmp/project")),
            Some("fix the Pi adapter".into())
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn unavailable_or_command_only_history_returns_none() {
        let home = temp_home();
        let id = "019ff1d3-375e-4a72-a176-c47497827e49";
        write(
            &home.join(format!(".codex/sessions/rollout-{id}.jsonl")),
            "not json\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"/status\"}}\n",
        );

        let history = ConversationHistory::from_home(&home);
        assert_eq!(
            history.first_user_message(AgentKind::Codex, id, Path::new("/work")),
            None
        );
        assert_eq!(
            ConversationHistory::default().first_user_message(
                AgentKind::Claude,
                id,
                Path::new("/work")
            ),
            None
        );
        fs::remove_dir_all(home).unwrap();
    }
}
