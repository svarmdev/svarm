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
            "approval required",
            "needs your approval",
            "allow this command",
        ],
        excluded: &[],
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
            "permission required",
            "allow this tool",
        ],
        excluded: &[],
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

pub(crate) fn recognize_title(kind: AgentKind, title: &str) -> Option<TitleRecognition> {
    if kind != AgentKind::Codex {
        return None;
    }
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

    Some(TitleRecognition {
        conversation_title: (!conversation_title.trim().is_empty())
            .then(|| conversation_title.trim())
            .filter(|title| !looks_like_uuid(title))
            .map(str::to_owned),
        evidence: RecognitionEvidence {
            provider: kind,
            claim,
            rule: rule.into(),
            evidence: evidence.into(),
        },
    })
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
                AgentKind::Claude => value.strip_prefix("777;svarm-conversation="),
                _ => None,
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
        && let Some(evidence) = bottom(&raw, STATUS_ROWS)
            .iter()
            .find(|row| row.trim_start().starts_with('❯'))
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
    fn partial_or_historical_question_text_does_not_claim_blocked() {
        assert_eq!(claim(AgentKind::Claude, &[b"Do you want to proc"]), None);
        assert_eq!(
            claim(AgentKind::Codex, &[b"Working (4s)  esc to inter"]),
            None
        );
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
    fn conversation_id_detector_rejects_wrong_provider_and_malformed_ids() {
        let mut codex = ConversationIdDetector::new(AgentKind::Codex);
        assert_eq!(
            codex.process(b"\x1b]777;svarm-conversation=019ff1d3-375e-7a72-a176-c47497827e49\x07"),
            None
        );
        assert_eq!(codex.process(b"\x1b]2;Ready | not-a-uuid\x07"), None);
    }
}
