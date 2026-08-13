//! Headless generation of short conversation names.
//!
//! The first message a user submits is kept as an immediate provisional name. This module asks the
//! same coding agent the session runs — `claude -p` for a Claude session, `codex exec` for a Codex
//! one, `grok -p` for a Grok Build session, or `pi -p` for a Pi session — for a shorter name, off
//! the critical path, and hands the result back through a channel.
//! Recognition of a usable name is pure and independently testable; only [`TitleNamer`] touches
//! processes and threads.

use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use crate::{AgentId, AgentKind};

/// Instructions the generator runs under. Deliberately narrow: one line, body only.
const SYSTEM_PROMPT: &str = concat!(
    "You name work sessions. Read the user's messages and reply with a single short name for ",
    "the work they describe. Constraints: English noun phrase, at most six words, no quotes, ",
    "no trailing punctuation, no emoji, no explanation or preamble. Output the name alone on ",
    "one line.",
);

/// Prompt characters handed to the generator.
const MAX_LOG_CHARS: usize = 1500;
/// Characters kept from the generated name.
const MAX_TITLE_CHARS: usize = 48;
/// How long a generator may run before it is killed.
const GENERATOR_TIMEOUT: Duration = Duration::from_secs(90);
/// How often a running generator is checked for completion.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Generated names outstanding before further requests are dropped.
const RESULT_QUEUE: usize = 64;
/// Environment the generator must not inherit: it would re-enter svarm's own agent wiring.
const SCRUBBED_ENV: &[&str] = &[
    "SVARM",
    "CLAUDECODE",
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE",
    crate::CLAUDE_SIGNAL_ENV,
    "GROK_SESSION_ID",
    "GROK_HOME",
    "PI_CODING_AGENT",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "PI_PROVIDER",
    "PI_MODEL",
    "PI_REASONING_LEVEL",
];

const fn default_model(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "haiku",
        AgentKind::Codex => "gpt-5.4-mini",
        AgentKind::Grok => "grok-4.6",
        AgentKind::Pi => "",
    }
}

/// Whether `SVARM_AUTO_TITLE` leaves naming on. Naming is on unless it is explicitly turned off.
fn enabled_by(value: Option<&str>) -> bool {
    !matches!(
        value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// A name produced for one conversation. `conversation_id` is the conversation the request was made
/// for, so a name that arrives after the agent moved on can be discarded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TitleResult {
    pub(crate) agent: AgentId,
    pub(crate) conversation_id: Option<String>,
    pub(crate) title: String,
}

/// How a generator is launched. Tests substitute a fixed command so no agent is ever invoked.
enum Generator {
    Agent,
    #[cfg(test)]
    Fixed(String, Vec<String>),
}

pub(crate) struct TitleNamer {
    enabled: bool,
    model: Option<String>,
    generator: Generator,
    sender: SyncSender<TitleResult>,
    receiver: Receiver<TitleResult>,
}

impl TitleNamer {
    #[cfg(not(test))]
    pub(crate) fn from_environment() -> Self {
        Self::new(
            enabled_by(std::env::var("SVARM_AUTO_TITLE").ok().as_deref()),
            std::env::var("SVARM_AUTO_TITLE_MODEL")
                .ok()
                .filter(|model| !model.trim().is_empty()),
            Generator::Agent,
        )
    }

    #[cfg(test)]
    pub(crate) fn fixed(program: &str, args: &[&str]) -> Self {
        Self::new(
            true,
            None,
            Generator::Fixed(
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ),
        )
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self::new(false, None, Generator::Agent)
    }

    fn new(enabled: bool, model: Option<String>, generator: Generator) -> Self {
        let (sender, receiver) = mpsc::sync_channel(RESULT_QUEUE);
        Self {
            enabled,
            model,
            generator,
            sender,
            receiver,
        }
    }

    /// Ask for a name in the background. Returns whether a generator was started, so the caller
    /// knows a request is in flight.
    pub(crate) fn request(
        &self,
        agent: AgentId,
        conversation_id: Option<String>,
        kind: AgentKind,
        prompts: &[String],
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let log = prompt_log(prompts, MAX_LOG_CHARS);
        if log.is_empty() {
            return false;
        }
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| default_model(kind).to_owned());
        let request = match &self.generator {
            Generator::Agent => GeneratorRequest::Agent { kind, model, log },
            #[cfg(test)]
            Generator::Fixed(program, args) => GeneratorRequest::Fixed {
                program: program.clone(),
                args: args.clone(),
            },
        };
        let sender = self.sender.clone();
        thread::Builder::new()
            .name("svarm-title".into())
            .spawn(move || {
                if let Some(title) = request.run() {
                    let _ = sender.try_send(TitleResult {
                        agent,
                        conversation_id,
                        title,
                    });
                }
            })
            .is_ok()
    }

    /// Deliver a name as if a generator had produced it.
    #[cfg(test)]
    pub(crate) fn deliver(&self, result: TitleResult) {
        self.sender.try_send(result).unwrap();
    }

    /// Names finished since the last call. Never blocks.
    pub(crate) fn drain(&self) -> Vec<TitleResult> {
        self.receiver.try_iter().collect()
    }
}

enum GeneratorRequest {
    Agent {
        kind: AgentKind,
        model: String,
        log: String,
    },
    #[cfg(test)]
    Fixed { program: String, args: Vec<String> },
}

impl GeneratorRequest {
    fn run(self) -> Option<String> {
        let invocation = match self {
            Self::Agent { kind, model, log } => generator_command(kind, &model, &log),
            #[cfg(test)]
            Self::Fixed { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                GeneratorInvocation {
                    command,
                    stdin: None,
                    answer_file: None,
                    cleanup_dir: None,
                    json_text: false,
                }
            }
        };
        invocation
            .run()
            .and_then(|output| sanitize_title(&output, MAX_TITLE_CHARS))
    }
}

/// A generator ready to run: its command, anything to write to its stdin, and the file it writes its
/// answer to when its stdout carries more than the answer.
struct GeneratorInvocation {
    command: Command,
    stdin: Option<String>,
    answer_file: Option<PathBuf>,
    cleanup_dir: Option<PathBuf>,
    json_text: bool,
}

impl GeneratorInvocation {
    fn run(self) -> Option<String> {
        let Self {
            command,
            stdin,
            answer_file,
            cleanup_dir,
            json_text,
        } = self;
        let stdout = run_generator(command, stdin.as_deref());
        let output = if let Some(path) = answer_file {
            let answer = std::fs::read_to_string(&path).ok();
            let _ = std::fs::remove_file(&path);
            stdout.and(answer)
        } else if json_text {
            stdout.and_then(|text| grok_answer_text(&text))
        } else {
            stdout
        };
        if let Some(directory) = cleanup_dir {
            let _ = std::fs::remove_dir_all(directory);
        }
        output
    }
}

fn grok_answer_text(stdout: &str) -> Option<String> {
    if let Some(text) = json_text_field(stdout.trim()) {
        return Some(text);
    }
    stdout.lines().rev().find_map(json_text_field)
}

fn json_text_field(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|value| value.get("text")?.as_str().map(str::to_owned))
}

/// The command that generates a name.
///
/// Providers are started without their user customizations: it makes them start faster, and it
/// keeps this call from re-entering the hooks and settings svarm installs for managed agents.
fn generator_command(kind: AgentKind, model: &str, log: &str) -> GeneratorInvocation {
    let mut command = Command::new(kind.command());
    match kind {
        AgentKind::Claude => {
            command.args([
                "-p",
                "--safe-mode",
                "--model",
                model,
                "--tools",
                "",
                "--no-session-persistence",
                "--max-budget-usd",
                "0.05",
                "--system-prompt",
                SYSTEM_PROMPT,
            ]);
            prepare_generator_environment(&mut command);
            GeneratorInvocation {
                command,
                stdin: Some(user_message(log)),
                answer_file: None,
                cleanup_dir: None,
                json_text: false,
            }
        }
        AgentKind::Grok => {
            let home = isolated_grok_home();
            command.args([
                "-p",
                &user_message(log),
                "--model",
                model,
                "--reasoning-effort",
                "low",
                "--system-prompt-override",
                SYSTEM_PROMPT,
                "--output-format",
                "json",
                "--tools",
                "",
                "--no-subagents",
                "--no-plan",
                "--no-memory",
                "--disable-web-search",
                "--max-turns",
                "1",
                "--permission-mode",
                "dontAsk",
                "--no-auto-update",
            ]);
            prepare_generator_environment(&mut command);
            if let Some(home) = &home {
                command.env("GROK_HOME", home);
            }
            GeneratorInvocation {
                command,
                stdin: None,
                answer_file: None,
                cleanup_dir: home,
                json_text: true,
            }
        }
        AgentKind::Codex => {
            // `codex exec` narrates its whole run on stdout, so the answer is collected from the
            // last-message file instead.
            let answer_file = answer_file_path();
            command.args([
                "exec",
                "--ignore-user-config",
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--color",
                "never",
                "--model",
                model,
                "-c",
                "project_doc_max_bytes=0",
                "-c",
                "model_reasoning_effort=none",
                "--output-last-message",
            ]);
            command.arg(&answer_file);
            command.arg(format!("{SYSTEM_PROMPT}\n\n{}", user_message(log)));
            prepare_generator_environment(&mut command);
            GeneratorInvocation {
                command,
                stdin: None,
                answer_file: Some(answer_file),
                cleanup_dir: None,
                json_text: false,
            }
        }
        AgentKind::Pi => {
            command.args([
                "-p",
                "--no-session",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-themes",
                "--no-context-files",
                "--no-tools",
                "--system-prompt",
                SYSTEM_PROMPT,
                &user_message(log),
            ]);
            prepare_generator_environment(&mut command);
            command.env("PI_OFFLINE", "1");
            command.env("PI_SKIP_VERSION_CHECK", "1");
            GeneratorInvocation {
                command,
                stdin: None,
                answer_file: None,
                cleanup_dir: None,
                json_text: false,
            }
        }
    }
}

fn isolated_grok_home() -> Option<PathBuf> {
    let source = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".grok"))?;
    let destination = std::env::temp_dir().join(format!(
        "svarm-grok-title-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    std::fs::create_dir_all(&destination).ok()?;
    for name in ["auth.json", "config.toml"] {
        let from = source.join(name);
        if from.exists() {
            let _ = std::fs::copy(&from, destination.join(name));
        }
    }
    Some(destination)
}

fn answer_file_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "svarm-title-{}-{:?}.txt",
        std::process::id(),
        thread::current().id()
    ))
}

fn user_message(log: &str) -> String {
    format!("These are the messages a user sent during a work session. Name that work.\n\n{log}")
}

fn prepare_generator_environment(command: &mut Command) {
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    if let Some(home) = home_directory() {
        // Leave the project directory so project-local agent configuration stays out of this call.
        command.current_dir(home);
    }
}

fn home_directory() -> Option<OsString> {
    std::env::var_os("HOME").filter(|home| !home.is_empty())
}

/// Run a generator to completion under a bounded wait, returning its stdout.
fn run_generator(mut command: Command, stdin: Option<&str>) -> Option<String> {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let deadline = Instant::now() + GENERATOR_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    String::from_utf8(output.stdout).ok()
}

/// Build the message log handed to the generator.
///
/// The first message states what the work was started for, so it is always kept; the remaining
/// budget is filled from the most recent messages backwards, because those describe where the work
/// actually went.
fn prompt_log(prompts: &[String], max_chars: usize) -> String {
    let Some(first) = prompts.first() else {
        return String::new();
    };
    let head = truncate_chars(first, max_chars);
    let mut used = head.chars().count();
    let mut lines = vec![format!("1. {head}")];
    let mut tail = Vec::new();
    for (index, prompt) in prompts.iter().enumerate().skip(1).rev() {
        let remaining = max_chars.saturating_sub(used);
        if remaining <= 40 {
            break;
        }
        let piece = truncate_chars(prompt, remaining);
        used += piece.chars().count();
        tail.push(format!("{}. {piece}", index + 1));
    }
    lines.extend(tail.into_iter().rev());
    lines.join("\n")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Reduce generator output to a name usable as one line in the sidebar, or nothing.
///
/// The last non-empty line is taken so a model that narrates before answering still yields its
/// answer, and the wrapping a model reaches for — a `Title:` prefix, quotes, a trailing period — is
/// stripped rather than shown to the user.
fn sanitize_title(raw: &str, max_chars: usize) -> Option<String> {
    let text = raw
        .chars()
        .map(|character| match character {
            '\n' => '\n',
            // Tabs and stray escape sequences are separators, not characters to swallow.
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>();
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    let title = strip_decoration(strip_prefix_label(strip_decoration(line)))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return None;
    }
    Some(truncate_title(&title, max_chars))
}

fn strip_decoration(line: &str) -> &str {
    line.trim_matches(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '"' | '\''
                    | '`'
                    | '*'
                    | '#'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '.'
                    | ':'
                    | '-'
                    | '—'
                    | '…'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
            )
    })
}

fn strip_prefix_label(line: &str) -> &str {
    let lowercase = line.to_ascii_lowercase();
    for label in ["title:", "name:"] {
        if let Some(rest) = lowercase.strip_prefix(label) {
            return &line[line.len() - rest.len()..];
        }
    }
    line
}

fn truncate_title(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        return title.to_owned();
    }
    let kept = title
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_clean_single_line_is_kept_as_written() {
        assert_eq!(
            sanitize_title("Sidebar truncation fix\n", MAX_TITLE_CHARS).as_deref(),
            Some("Sidebar truncation fix")
        );
    }

    #[test]
    fn preamble_quoting_and_labels_are_stripped() {
        assert_eq!(
            sanitize_title(
                "Here is a good name for the work:\n  \"Title: Archive modal rework.\"  ",
                MAX_TITLE_CHARS
            )
            .as_deref(),
            Some("Archive modal rework")
        );
    }

    #[test]
    fn internal_whitespace_collapses_to_single_spaces() {
        assert_eq!(
            sanitize_title("Agent   naming\tpipeline", MAX_TITLE_CHARS).as_deref(),
            Some("Agent naming pipeline")
        );
    }

    #[test]
    fn an_over_long_name_is_truncated_with_an_ellipsis() {
        let title = sanitize_title(&"word ".repeat(40), 12).unwrap();
        assert_eq!(title.chars().count(), 12);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn empty_or_decorative_output_yields_no_name() {
        for raw in ["", "   \n\t ", "\"\"", "---", "\u{7}\u{1b}"] {
            assert_eq!(sanitize_title(raw, MAX_TITLE_CHARS), None, "raw: {raw:?}");
        }
    }

    #[test]
    fn the_first_message_survives_a_tight_budget() {
        let prompts = owned(&["start the naming work", "and then something much later"]);
        let log = prompt_log(&prompts, 45);

        assert_eq!(log, "1. start the naming work");
    }

    #[test]
    fn later_messages_are_filled_in_original_order() {
        let prompts = owned(&["first", "second", "third"]);

        assert_eq!(prompt_log(&prompts, 1500), "1. first\n2. second\n3. third");
    }

    #[test]
    fn no_messages_produce_no_log() {
        assert!(prompt_log(&[], 1500).is_empty());
    }

    #[test]
    fn each_provider_is_asked_with_its_own_headless_command() {
        let claude = generator_command(AgentKind::Claude, "haiku", "1. do a thing");
        let claude_args = claude
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(claude.command.get_program(), AgentKind::Claude.command());
        assert!(claude_args.contains(&"-p".to_owned()));
        assert!(claude_args.contains(&"--safe-mode".to_owned()));
        assert!(
            claude_args
                .windows(2)
                .any(|pair| pair == ["--model", "haiku"])
        );
        // Claude prints only its answer, so it is read straight from stdout.
        assert!(claude.stdin.unwrap().contains("1. do a thing"));
        assert_eq!(claude.answer_file, None);

        let codex = generator_command(AgentKind::Codex, "gpt-5.4-mini", "1. do a thing");
        let codex_args = codex
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(codex.command.get_program(), AgentKind::Codex.command());
        assert_eq!(codex_args.first().map(String::as_str), Some("exec"));
        assert!(codex_args.contains(&"--ephemeral".to_owned()));
        assert!(
            codex_args
                .windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"])
        );
        assert!(codex_args.last().unwrap().contains("1. do a thing"));
        assert_eq!(codex.stdin, None);
        // Codex narrates its whole run on stdout, so its answer comes from the last-message file.
        let answer_file = codex
            .answer_file
            .expect("codex writes its answer to a file");
        assert!(codex_args.windows(2).any(|pair| {
            pair[0] == "--output-last-message" && pair[1] == answer_file.to_string_lossy()
        }));

        let grok = generator_command(AgentKind::Grok, "grok-4.6", "1. do a thing");
        let grok_args = grok
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(grok.command.get_program(), AgentKind::Grok.command());
        assert!(grok_args.contains(&"-p".to_owned()));
        assert!(
            grok_args
                .windows(2)
                .any(|pair| pair == ["--model", "grok-4.6"])
        );
        assert!(
            grok_args
                .windows(2)
                .any(|pair| pair == ["--reasoning-effort", "low"])
        );
        assert!(
            grok_args
                .windows(2)
                .any(|pair| pair == ["--output-format", "json"])
        );
        assert!(grok_args.contains(&"--no-auto-update".to_owned()));
        assert!(grok.json_text);
        assert_eq!(grok.stdin, None);
        let grok_home = grok
            .command
            .get_envs()
            .find(|(key, _)| *key == "GROK_HOME")
            .and_then(|(_, value)| value.map(|value| value.to_os_string()));
        assert!(grok_home.is_some(), "namer must isolate GROK_HOME");
        if let Some(directory) = grok.cleanup_dir {
            let _ = std::fs::remove_dir_all(directory);
        }

        let pi = generator_command(AgentKind::Pi, "", "1. do a thing");
        let pi_args = pi
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(pi.command.get_program(), AgentKind::Pi.command());
        assert_eq!(pi_args.first().map(String::as_str), Some("-p"));
        for flag in [
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--no-tools",
        ] {
            assert!(pi_args.contains(&flag.to_owned()), "missing {flag}");
        }
        assert!(pi_args.last().unwrap().contains("1. do a thing"));
        assert_eq!(pi.stdin, None);
        assert_eq!(pi.answer_file, None);
    }

    #[test]
    fn grok_names_are_taken_from_json_text() {
        assert_eq!(
            grok_answer_text(r#"{"text":"Sidebar truncation fix","sessionId":"abc"}"#).as_deref(),
            Some("Sidebar truncation fix")
        );
        assert_eq!(
            grok_answer_text("noise\n{\"text\":\"Archive modal rework\"}\n"),
            Some("Archive modal rework".into())
        );
        assert_eq!(grok_answer_text("not json"), None);
    }

    #[test]
    fn the_generator_never_inherits_svarms_agent_environment() {
        let invocation = generator_command(AgentKind::Claude, "haiku", "1. do a thing");
        let removed = invocation
            .command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        for key in SCRUBBED_ENV {
            assert!(removed.contains(&(*key).to_owned()), "kept {key}");
        }
    }

    #[test]
    fn naming_is_on_unless_it_is_explicitly_turned_off() {
        for value in [None, Some(""), Some("1"), Some("true"), Some("anything")] {
            assert!(enabled_by(value), "{value:?} should leave naming on");
        }
        for value in ["0", "false", "no", "off", " OFF "] {
            assert!(!enabled_by(Some(value)), "{value:?} should turn naming off");
        }
    }

    #[test]
    fn a_disabled_namer_starts_no_generator() {
        let namer = TitleNamer::disabled();

        assert!(!namer.request(AgentId::new(1), None, AgentKind::Claude, &owned(&["hello"])));
        assert!(namer.drain().is_empty());
    }

    #[test]
    fn a_generated_name_is_delivered_with_the_conversation_it_was_asked_for() {
        let namer = TitleNamer::fixed("printf", &["Named by the generator\\n"]);
        assert!(namer.request(
            AgentId::new(7),
            Some("conversation-a".into()),
            AgentKind::Claude,
            &owned(&["name this"]),
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = namer.drain().into_iter().next() {
                break result;
            }
            assert!(Instant::now() < deadline, "no name arrived");
            thread::sleep(POLL_INTERVAL);
        };

        assert_eq!(
            result,
            TitleResult {
                agent: AgentId::new(7),
                conversation_id: Some("conversation-a".into()),
                title: "Named by the generator".into(),
            }
        );
    }

    /// Opt-in check that the flags above still hold for the installed agents. Run it after a
    /// provider CLI updates:
    /// `cargo test -p svarm-agent -- --ignored real_agents_answer_with_a_usable_name`
    #[test]
    #[ignore = "invokes the installed coding agents and spends money"]
    fn real_agents_answer_with_a_usable_name() {
        for kind in AgentKind::ALL {
            let invocation = generator_command(
                kind,
                default_model(kind),
                "1. fix teh sidbar truncation pls\n2. also the status circle is misaligned",
            );
            let title = invocation
                .run()
                .and_then(|output| sanitize_title(&output, MAX_TITLE_CHARS));
            let title = title.unwrap_or_else(|| panic!("{kind} produced no usable name"));

            assert!(!title.is_empty());
            assert!(title.chars().count() <= MAX_TITLE_CHARS, "{kind}: {title}");
            assert!(!title.contains('\n'), "{kind}: {title}");
            assert!(
                title.to_lowercase().contains("sidebar") || title.to_lowercase().contains("status"),
                "{kind} named unrelated work: {title}"
            );
        }
    }

    #[test]
    fn a_failing_generator_delivers_nothing() {
        let namer = TitleNamer::fixed("false", &[]);
        assert!(namer.request(
            AgentId::new(1),
            None,
            AgentKind::Codex,
            &owned(&["name this"])
        ));

        thread::sleep(Duration::from_millis(200));
        assert!(namer.drain().is_empty());
    }

    #[test]
    fn a_missing_generator_delivers_nothing() {
        let namer = TitleNamer::fixed("svarm-no-such-generator", &[]);
        assert!(namer.request(
            AgentId::new(1),
            None,
            AgentKind::Codex,
            &owned(&["name this"])
        ));

        thread::sleep(Duration::from_millis(200));
        assert!(namer.drain().is_empty());
    }
}
