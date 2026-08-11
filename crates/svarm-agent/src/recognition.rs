use vt100::Screen;

use crate::{
    AgentKind,
    protocol::{AgentActivity, RecognitionEvidence},
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

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn recognize(kind: AgentKind, screen: &Screen) -> ScreenRecognition {
    let (_, cols) = screen.size();
    let raw = screen.rows(0, cols).collect::<Vec<_>>();
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

    fn screen(chunks: &[&[u8]]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(24, 80, 0);
        for chunk in chunks {
            parser.process(chunk);
        }
        parser
    }

    fn claim(kind: AgentKind, chunks: &[&[u8]]) -> Option<AgentActivity> {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"\x1b[20;1H");
        for chunk in chunks {
            parser.process(chunk);
        }
        match recognize(kind, parser.screen()) {
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
            recognize(AgentKind::Claude, parser.screen()),
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
            recognize(AgentKind::Codex, parser.screen()),
            ScreenRecognition::Preserve
        ));
    }
}
