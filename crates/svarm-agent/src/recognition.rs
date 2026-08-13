use crate::{
    AgentKind,
    protocol::{AgentActivity, RecognitionEvidence},
    terminal_model::TerminalSnapshot,
};

const STATUS_ROWS: usize = 5;
const DIALOG_ROWS: usize = 12;

pub(crate) enum ScreenRecognition {
    Recognized(RecognitionEvidence),
    Preserve,
    Unknown,
}

pub(crate) struct TitleRecognition {
    pub conversation_title: Option<String>,
    pub evidence: RecognitionEvidence,
}

struct Rule {
    id: &'static str,
    activity: AgentActivity,
    rows_from_bottom: usize,
    required: &'static [&'static str],
    any: &'static [&'static str],
    excluded: &'static [&'static str],
}

const CODEX_RULES: &[Rule] = &[
    Rule {
        id: "codex.approval-dialog",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &[],
        any: &[
            "would you like to run the following command",
            "would you like to apply the following changes",
            "allow codex to run",
            "allow codex to apply proposed code changes",
            "approval required",
            "needs your approval",
            "allow this command",
        ],
        excluded: &["esc to interrupt"],
    },
    // Codex phrases each approval differently but always offers a verbatim
    // affirmative option, so the option labels identify the prompt when the
    // question itself has scrolled out of the dialog window.
    Rule {
        id: "codex.approval-options",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &[],
        any: &[
            "yes, and allow these permissions for this session",
            "yes, and allow this host in the future",
            "yes, grant these permissions for this session",
            "yes, grant these permissions for this turn",
            "yes, provide the requested info",
            "yes, implement this plan",
            "yes, continue anyway",
        ],
        excluded: &["esc to interrupt"],
    },
    Rule {
        id: "codex.question-selector",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &["enter to submit"],
        any: &[],
        excluded: &["select model", "select theme"],
    },
    Rule {
        id: "codex.active-turn",
        activity: AgentActivity::Working,
        rows_from_bottom: STATUS_ROWS,
        required: &["esc to interrupt"],
        any: &[],
        excluded: &[],
    },
    Rule {
        id: "codex.ready-prompt",
        activity: AgentActivity::Idle,
        rows_from_bottom: STATUS_ROWS,
        required: &[],
        any: &["? for shortcuts", "press ? for shortcuts"],
        excluded: &["esc to interrupt"],
    },
];

const CLAUDE_RULES: &[Rule] = &[
    Rule {
        id: "claude.approval-dialog",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &[],
        any: &[
            "do you want to proceed?",
            "do you want to continue?",
            "do you want to make this edit to",
            "do you want to create",
            "do you want to delete",
            "do you want to allow",
            "do you want to use this",
            "no, and tell claude what to do differently",
            "yes, and don't ask again for",
            "permission required",
            "allow this tool",
        ],
        excluded: &["esc to interrupt"],
    },
    // Claude renders every permission prompt as a numbered option list with the
    // selection marker on the first entry, which distinguishes it from the idle
    // composer prompt that also starts with the same marker.
    Rule {
        id: "claude.option-selector",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &["❯ 1."],
        any: &[],
        excluded: &[
            "esc to interrupt",
            "select model",
            "select theme",
            "resume session",
        ],
    },
    Rule {
        id: "claude.question-selector",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &["enter to select", "esc to cancel"],
        any: &[],
        excluded: &["select model", "select theme", "resume session"],
    },
    Rule {
        id: "claude.active-turn",
        activity: AgentActivity::Working,
        rows_from_bottom: STATUS_ROWS,
        required: &["esc to interrupt"],
        any: &[],
        excluded: &[],
    },
];

const GROK_RULES: &[Rule] = &[
    Rule {
        id: "grok.approval-dialog",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &[],
        any: &[
            "allow once",
            "reject once",
            "always allow on all sessions",
            "allow all edits this session",
            "enable always-approve mode",
            "always allow this command",
        ],
        excluded: &[],
    },
    Rule {
        id: "grok.plan-approval",
        activity: AgentActivity::Blocked,
        rows_from_bottom: DIALOG_ROWS,
        required: &["plan ready for review"],
        any: &[],
        excluded: &[],
    },
    Rule {
        id: "grok.active-turn",
        activity: AgentActivity::Working,
        rows_from_bottom: STATUS_ROWS,
        required: &[],
        any: &[
            "waiting for response",
            "waiting on subagent",
            "waiting on task output",
            "waiting on tasks",
            "send a message to interrupt",
            ":cancel",
        ],
        excluded: &[],
    },
];

const PI_RULES: &[Rule] = &[
    // Pi has no built-in permission popups. Its native streaming indicator and abort hint are the
    // only provider-owned working evidence we rely on here; extensions may render arbitrary UI.
    Rule {
        id: "pi.active-turn",
        activity: AgentActivity::Working,
        rows_from_bottom: STATUS_ROWS,
        required: &["working..."],
        any: &[],
        excluded: &["you:"],
    },
    Rule {
        id: "pi.ready-prompt",
        activity: AgentActivity::Idle,
        rows_from_bottom: STATUS_ROWS,
        required: &[],
        any: &["enter to send", "enter send"],
        excluded: &["working", "esc to interrupt", "escape to abort"],
    },
];

pub(crate) fn recognize_title(kind: AgentKind, title: &str) -> Option<TitleRecognition> {
    match kind {
        AgentKind::Codex => recognize_codex_title(title),
        AgentKind::Grok => recognize_grok_title(title),
        AgentKind::Claude | AgentKind::Pi => None,
    }
}

fn recognize_codex_title(title: &str) -> Option<TitleRecognition> {
    let (status, conversation_title) = title.split_once(" | ").unwrap_or((title, ""));
    let normalized = normalize(status);
    let (claim, rule, evidence) = if normalized.contains("action required") {
        (
            AgentActivity::Blocked,
            "codex.title-action-required",
            "Action Required",
        )
    } else if let Some(state) = ["working", "thinking", "waiting"]
        .into_iter()
        .find(|state| normalized.contains(state))
    {
        (AgentActivity::Working, "codex.title-active", state)
    } else if normalized == "ready" {
        (AgentActivity::Idle, "codex.title-ready", "ready")
    } else {
        return None;
    };

    Some(title_recognition(
        AgentKind::Codex,
        conversation_title,
        claim,
        rule,
        evidence,
    ))
}

fn recognize_grok_title(title: &str) -> Option<TitleRecognition> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parts = trimmed
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty() && !is_spinner_token(part))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }

    let mut claim = None;
    let mut evidence = None;
    let mut conversation_parts = Vec::new();
    for part in &parts {
        match grok_title_part_activity(part) {
            Some(activity) => {
                if claim != Some(AgentActivity::Blocked) {
                    claim = Some(activity);
                    evidence = Some(*part);
                }
            }
            None if normalize(part) != "grok" => conversation_parts.push(*part),
            None => {}
        }
    }

    let (claim, rule, evidence) = if let Some(claim) = claim {
        let rule = if claim == AgentActivity::Blocked {
            "grok.title-action-required"
        } else {
            "grok.title-active"
        };
        (claim, rule, evidence.unwrap_or(trimmed))
    } else if parts.last().is_some_and(|part| normalize(part) == "grok") {
        (AgentActivity::Idle, "grok.title-ready", trimmed)
    } else {
        return None;
    };

    Some(title_recognition(
        AgentKind::Grok,
        &conversation_parts.join(" - "),
        claim,
        rule,
        evidence,
    ))
}

fn title_recognition(
    provider: AgentKind,
    conversation_title: &str,
    claim: AgentActivity,
    rule: &'static str,
    evidence: &str,
) -> TitleRecognition {
    TitleRecognition {
        conversation_title: (!conversation_title.trim().is_empty())
            .then(|| conversation_title.trim())
            .filter(|title| !looks_like_uuid(title))
            .map(str::to_owned),
        evidence: RecognitionEvidence {
            provider,
            claim,
            rule: rule.into(),
            evidence: evidence.into(),
        },
    }
}

fn grok_title_part_activity(part: &str) -> Option<AgentActivity> {
    let normalized = strip_title_ellipsis(&normalize(part));
    if normalized.contains("action required") {
        return Some(AgentActivity::Blocked);
    }
    let working = matches!(
        normalized.as_str(),
        "thinking" | "responding" | "compacting" | "preparing" | "working" | "running tool"
    ) || normalized.starts_with("waiting for response")
        || normalized.starts_with("waiting on subagent")
        || normalized.starts_with("waiting on task")
        || normalized.starts_with("retrying")
        || normalized.starts_with("running:");
    working.then_some(AgentActivity::Working)
}

fn is_spinner_token(value: &str) -> bool {
    let mut characters = value.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => ('\u{2800}'..='\u{28FF}').contains(&character),
        _ => false,
    }
}

fn strip_title_ellipsis(value: &str) -> String {
    value
        .trim_end_matches('…')
        .trim_end_matches("...")
        .trim()
        .to_owned()
}

pub(crate) fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

pub(crate) struct ConversationIdDetector {
    kind: AgentKind,
    pending: Vec<u8>,
}

impl ConversationIdDetector {
    pub(crate) const fn new(kind: AgentKind) -> Self {
        Self {
            kind,
            pending: Vec::new(),
        }
    }

    pub(crate) fn process(&mut self, bytes: &[u8]) -> Option<String> {
        self.pending.extend_from_slice(bytes);
        let mut found = None;
        let mut consumed = 0;
        while let Some(start) = self.pending[consumed..]
            .windows(2)
            .position(|window| window == b"\x1b]")
            .map(|offset| consumed + offset)
        {
            let body = start + 2;
            let Some((end, terminator)) = osc_end(&self.pending[body..]) else {
                if start > 0 {
                    self.pending.drain(..start);
                }
                return found;
            };
            let end = body + end;
            let value = String::from_utf8_lossy(&self.pending[body..end]);
            let candidate = match self.kind {
                AgentKind::Codex if value.starts_with("0;") || value.starts_with("2;") => value
                    .split(|character: char| !character.is_ascii_hexdigit() && character != '-')
                    .find(|value| looks_like_uuid(value)),
                AgentKind::Codex => None,
                AgentKind::Claude => value.strip_prefix("777;svarm-conversation="),
                AgentKind::Pi => None,
                AgentKind::Grok => None,
            };
            if let Some(candidate) = candidate.filter(|value| looks_like_uuid(value)) {
                found = Some(candidate.to_ascii_lowercase());
            }
            consumed = end + terminator;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        if self.pending.len() > 512 {
            let keep = self.pending.len() - 512;
            self.pending.drain(..keep);
        }
        found
    }
}

fn osc_end(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.iter().enumerate().find_map(|(index, byte)| {
        if *byte == 0x07 {
            Some((index, 1))
        } else if *byte == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
            Some((index, 2))
        } else {
            None
        }
    })
}

pub(crate) fn recognize(kind: AgentKind, screen: &TerminalSnapshot) -> ScreenRecognition {
    let raw = screen.rows().collect::<Vec<_>>();
    let normalized = raw.iter().map(|row| normalize(row)).collect::<Vec<_>>();

    if is_history_overlay(&normalized) {
        return ScreenRecognition::Preserve;
    }

    let rules = match kind {
        AgentKind::Codex => CODEX_RULES,
        AgentKind::Claude => CLAUDE_RULES,
        AgentKind::Grok => GROK_RULES,
        AgentKind::Pi => PI_RULES,
    };
    for rule in rules {
        if let Some(evidence) = matches(rule, &raw, &normalized) {
            return ScreenRecognition::Recognized(RecognitionEvidence {
                provider: kind,
                claim: rule.activity,
                rule: rule.id.into(),
                evidence,
            });
        }
    }

    if kind == AgentKind::Claude
        && let Some(evidence) = bottom(&raw, STATUS_ROWS).iter().find(|row| {
            let row = row.trim_start();
            row.starts_with('❯')
                && !row
                    .trim_start_matches('❯')
                    .trim_start()
                    .starts_with(|character: char| character.is_ascii_digit())
        })
    {
        return ScreenRecognition::Recognized(RecognitionEvidence {
            provider: kind,
            claim: AgentActivity::Idle,
            rule: "claude.ready-prompt".into(),
            evidence: evidence.trim().into(),
        });
    }

    ScreenRecognition::Unknown
}

fn matches(rule: &Rule, raw: &[String], normalized: &[String]) -> Option<String> {
    let normalized = bottom(normalized, rule.rows_from_bottom);
    let joined = normalized.join("\n");
    if !rule.required.iter().all(|needle| joined.contains(needle))
        || (!rule.any.is_empty() && !rule.any.iter().any(|needle| joined.contains(needle)))
        || rule.excluded.iter().any(|needle| joined.contains(needle))
    {
        return None;
    }

    bottom(raw, rule.rows_from_bottom)
        .iter()
        .find(|row| {
            let row = normalize(row);
            rule.required.iter().any(|needle| row.contains(needle))
                || rule.any.iter().any(|needle| row.contains(needle))
        })
        .map(|row| row.trim().chars().take(160).collect())
        .or_else(|| Some(joined.chars().take(160).collect()))
}

fn is_history_overlay(rows: &[String]) -> bool {
    let all = rows.join("\n");
    let footer = bottom(rows, STATUS_ROWS).join("\n");
    ["transcript", "conversation history", "resume session"]
        .iter()
        .any(|label| all.contains(label))
        && (footer.contains("esc to close") || footer.contains("esc to cancel"))
}

fn bottom<T>(rows: &[T], count: usize) -> &[T] {
    &rows[rows.len().saturating_sub(count)..]
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CursorStyle,
        terminal_backend::Vt100Backend,
        terminal_model::{TerminalBackend, TerminalSize},
    };

    fn screen(chunks: &[&[u8]]) -> TerminalSnapshot {
        let mut backend = Vt100Backend::new(TerminalSize::new(24, 80), 0);
        for chunk in chunks {
            backend.process(chunk);
        }
        backend.snapshot(CursorStyle::default(), backend.modes(false, false))
    }

    fn claim(kind: AgentKind, chunks: &[&[u8]]) -> Option<AgentActivity> {
        let mut backend = Vt100Backend::new(TerminalSize::new(24, 80), 0);
        backend.process(b"\x1b[20;1H");
        for chunk in chunks {
            backend.process(chunk);
        }
        let snapshot = backend.snapshot(CursorStyle::default(), backend.modes(false, false));
        match recognize(kind, &snapshot) {
            ScreenRecognition::Recognized(evidence) => Some(evidence.claim),
            ScreenRecognition::Preserve | ScreenRecognition::Unknown => None,
        }
    }

    #[test]
    fn codex_recognizes_working_blocked_and_idle_from_affirmative_footers() {
        assert_eq!(
            claim(AgentKind::Codex, &[b"Working (4s)  esc to interrupt"]),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(
                AgentKind::Codex,
                &[b"Would you like to run the following command?\r\n1. Yes\r\n2. No"]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(AgentKind::Codex, &[b"\xe2\x80\xba  \r\n? for shortcuts"]),
            Some(AgentActivity::Idle)
        );
    }

    #[test]
    fn codex_title_reports_live_state_and_conversation_name() {
        for (title, expected) in [
            ("⠋ Working | Refactor sidebar", AgentActivity::Working),
            ("⠹ Thinking | Refactor sidebar", AgentActivity::Working),
            (
                "[ ! ] Action Required | Refactor sidebar",
                AgentActivity::Blocked,
            ),
            ("Ready | Refactor sidebar", AgentActivity::Idle),
        ] {
            let recognized = recognize_title(AgentKind::Codex, title).unwrap();
            assert_eq!(recognized.evidence.claim, expected);
            assert_eq!(
                recognized.conversation_title.as_deref(),
                Some("Refactor sidebar")
            );
        }
        assert!(recognize_title(AgentKind::Claude, "Ready | Conversation").is_none());
        assert!(recognize_title(AgentKind::Grok, "Ready | Conversation").is_none());
        assert!(recognize_title(AgentKind::Codex, "Conversation only").is_none());
        assert_eq!(
            recognize_title(
                AgentKind::Codex,
                "Ready | 019ff1d3-375e-7a72-a176-c47497827e49"
            )
            .unwrap()
            .conversation_title,
            None
        );
    }

    #[test]
    fn claude_recognizes_split_working_input_and_ready_prompt() {
        assert_eq!(
            claim(AgentKind::Claude, &[b"esc to inter", b"rupt"]),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(AgentKind::Claude, &["❯ ".as_bytes()]),
            Some(AgentActivity::Idle)
        );
        assert_eq!(
            claim(AgentKind::Claude, &[b"Do you want to proceed?\r\n1. Yes"]),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Codex,
                &[b"Would you like to run the following com", b"mand?"]
            ),
            Some(AgentActivity::Blocked)
        );
    }

    #[test]
    fn pi_recognizes_only_native_working_and_ready_evidence() {
        assert_eq!(
            claim(AgentKind::Pi, &[b"Working...", b"\r\nEsc to interrupt"]),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(AgentKind::Pi, &[b"You:\r\nEnter to send"]),
            Some(AgentActivity::Idle)
        );
        assert_eq!(claim(AgentKind::Pi, &[b"working on a file"]), None);
        assert_eq!(claim(AgentKind::Pi, &[b"You: working"]), None);
    }

    #[test]
    fn claude_recognizes_approvals_that_do_not_say_proceed() {
        assert_eq!(
            claim(
                AgentKind::Claude,
                &["Do you want to make this edit to recognition.rs?\r\n❯ 1. Yes\r\n".as_bytes()]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Claude,
                &["❯ 1. Yes\r\n  2. Yes, and don't ask again for ls commands\r\n".as_bytes()]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Claude,
                &["  3. No, and tell Claude what to do differently (esc)".as_bytes()]
            ),
            Some(AgentActivity::Blocked)
        );
    }

    #[test]
    fn codex_recognizes_approvals_phrased_as_allow_or_option_labels() {
        assert_eq!(
            claim(
                AgentKind::Codex,
                &[b"Allow Codex to run `ls -la` in `~/dev/svarm`?\r\n1. Yes"]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Codex,
                &[b"Allow Codex to apply proposed code changes?"]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Codex,
                &[b"2. Yes, grant these permissions for this session\r\n? for shortcuts"]
            ),
            Some(AgentActivity::Blocked)
        );
    }

    #[test]
    fn partial_or_historical_question_text_does_not_claim_blocked() {
        assert_eq!(claim(AgentKind::Claude, &[b"Do you want to proc"]), None);
        assert_eq!(
            claim(AgentKind::Codex, &[b"Working (4s)  esc to inter"]),
            None
        );
        // A half-drawn option row is neither the composer prompt nor a complete
        // option list, so it must not claim idle.
        assert_eq!(claim(AgentKind::Claude, &["❯ 1".as_bytes()]), None);
        let parser = screen(&[
            b"Do you want to proceed?\r\n",
            b"\x1b[20;1H",
            "❯ ".as_bytes(),
        ]);
        assert!(matches!(
            recognize(AgentKind::Claude, &parser),
            ScreenRecognition::Recognized(RecognitionEvidence {
                claim: AgentActivity::Idle,
                ..
            })
        ));
    }

    #[test]
    fn transcript_overlay_preserves_the_previous_state() {
        let parser = screen(&[b"Transcript\x1b[24;1HEsc to close"]);
        assert!(matches!(
            recognize(AgentKind::Codex, &parser),
            ScreenRecognition::Preserve
        ));
    }

    #[test]
    fn conversation_ids_are_detected_from_provider_specific_osc_sequences() {
        let id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let mut codex = ConversationIdDetector::new(AgentKind::Codex);
        assert_eq!(codex.process(b"before\x1b]2;Ready | 019ff1d3-375e"), None);
        assert_eq!(
            codex.process(b"-7a72-a176-c47497827e49\x1b\\after"),
            Some(id.into())
        );

        let mut claude = ConversationIdDetector::new(AgentKind::Claude);
        assert_eq!(
            claude.process(format!("\x1b]777;svarm-conversation={id}\x07").as_bytes()),
            Some(id.into())
        );
    }

    #[test]
    fn pi_does_not_treat_arbitrary_terminal_titles_as_session_ids() {
        let mut pi = ConversationIdDetector::new(AgentKind::Pi);
        assert_eq!(
            pi.process(b"\x1b]0;Pi - 019ff1d3-375e-7a72-a176-c47497827e49\x07"),
            None
        );
    }

    #[test]
    fn grok_recognizes_complete_permission_and_plan_prompts() {
        assert_eq!(
            claim(AgentKind::Grok, &[b"Allow once\r\nReject once"]),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(
                AgentKind::Grok,
                &[b"Always allow on all sessions\r\nAllow all edits this session"]
            ),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(
            claim(AgentKind::Grok, &[b"Plan ready for review"]),
            Some(AgentActivity::Blocked)
        );
        assert_eq!(claim(AgentKind::Grok, &[b"Allow onc"]), None);
        assert_eq!(claim(AgentKind::Grok, &[b"esc to interrupt"]), None);
        assert_eq!(claim(AgentKind::Grok, &["❯ ".as_bytes()]), None);
    }

    #[test]
    fn grok_recognizes_working_from_status_line_and_cancel_hint() {
        assert_eq!(
            claim(
                AgentKind::Grok,
                &[b"Waiting for respo", b"nse\r\nEsc  :cancel"]
            ),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(AgentKind::Grok, &[b"Waiting on subagent"]),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(AgentKind::Grok, &[b"Waiting on task output"]),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            claim(
                AgentKind::Grok,
                &[b"1 command still running | send a message to interrupt"]
            ),
            Some(AgentActivity::Working)
        );
        assert_eq!(claim(AgentKind::Grok, &[b"Waiting for respo"]), None);
        assert_eq!(claim(AgentKind::Grok, &[b":cance"]), None);
        assert_eq!(claim(AgentKind::Grok, &[b"1 command still running"]), None);
    }

    #[test]
    fn grok_title_reports_live_state_and_session_name() {
        for (title, expected, session) in [
            (
                "⠦ - Waiting for response… - grok",
                AgentActivity::Working,
                None,
            ),
            ("⠹ - Thinking - grok", AgentActivity::Working, None),
            ("⠸ - Responding - grok", AgentActivity::Working, None),
            (
                "⠦ - Thinking - Refactor sidebar - grok",
                AgentActivity::Working,
                Some("Refactor sidebar"),
            ),
            ("Action Required - grok", AgentActivity::Blocked, None),
            ("grok", AgentActivity::Idle, None),
            (
                "Simple 2+2 Arithmetic Question - grok",
                AgentActivity::Idle,
                Some("Simple 2+2 Arithmetic Question"),
            ),
        ] {
            let recognized = recognize_title(AgentKind::Grok, title).unwrap();
            assert_eq!(recognized.evidence.claim, expected, "{title}");
            assert_eq!(recognized.conversation_title.as_deref(), session, "{title}");
        }
        // A session name that merely mentions thinking is not an activity segment.
        let named = recognize_title(AgentKind::Grok, "Thinking about rust - grok").unwrap();
        assert_eq!(named.evidence.claim, AgentActivity::Idle);
        assert_eq!(
            named.conversation_title.as_deref(),
            Some("Thinking about rust")
        );
        assert!(recognize_title(AgentKind::Grok, "Conversation only").is_none());
        assert!(recognize_title(AgentKind::Grok, "Ready | Conversation").is_none());
    }

    #[test]
    fn conversation_id_detector_rejects_wrong_provider_and_malformed_ids() {
        let mut codex = ConversationIdDetector::new(AgentKind::Codex);
        assert_eq!(
            codex.process(b"\x1b]777;svarm-conversation=019ff1d3-375e-7a72-a176-c47497827e49\x07"),
            None
        );
        assert_eq!(codex.process(b"\x1b]2;Ready | not-a-uuid\x07"), None);
    }
}
