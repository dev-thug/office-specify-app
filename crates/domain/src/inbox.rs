//! inbox module domain types (spec §6, source §9).
//!
//! `classify_inbound` implements the deterministic-signals-first part of
//! §9.4: bounces and auto-replies are recognized by stable markers, explicit
//! unsubscribe language is flagged, and anything ambiguous stays `unknown`
//! for human review — LLM judgment is not part of v1.0.

pub use contracts::inbox::{Classification, ThreadStatus};

/// Deterministic inbound classification (§9.4). Inputs are the already
/// normalized message fields; returns the most specific confident class,
/// else `unknown` (ambiguous mail waits for review rather than guessing).
///
/// Order matters: bounce > auto_reply > unsubscribe_request > question >
/// human polarity > unknown.
pub fn classify_inbound(
    subject: Option<&str>,
    body_text: Option<&str>,
    from: &str,
) -> Classification {
    let subj = subject.unwrap_or_default().to_lowercase();
    let body = body_text.unwrap_or_default().to_lowercase();
    let from_l = from.to_lowercase();
    let text = format!("{subj}\n{body}");

    // Bounce: MTA identity or delivery-failure phrasing.
    if from_l.contains("mailer-daemon")
        || from_l.contains("postmaster@")
        || subj.contains("delivery status notification")
        || subj.contains("undeliverable")
        || subj.contains("mail delivery failed")
        || subj.contains("returned mail")
        || subj.contains("delivery failure")
    {
        return Classification::Bounce;
    }

    // Auto-reply: out-of-office / automatic-answer markers.
    if subj.starts_with("auto:")
        || subj.contains("automatic reply")
        || subj.contains("out of office")
        || subj.contains("auto-reply")
        || subj.contains("autoreply")
        || subj.contains("부재중")
        || subj.contains("자동 회신")
        || subj.contains("자동응답")
    {
        return Classification::AutoReply;
    }

    // Explicit unsubscribe language in a reply (§9.4: pause follow-ups and
    // hold for human review — no automatic suppression on text alone).
    if text.contains("unsubscribe")
        || text.contains("stop sending")
        || text.contains("stop emailing")
        || text.contains("remove me")
        || text.contains("do not contact")
        || text.contains("opt out")
        || text.contains("opt-out")
        || text.contains("수신 거부")
        || text.contains("수신거부")
        || text.contains("그만 보내")
        || text.contains("메일 그만")
    {
        return Classification::UnsubscribeRequest;
    }

    // Question: an actual question mark or common interrogatives.
    if body.contains('?')
        || body.contains("could you")
        || body.contains("can you")
        || body.contains("how much")
        || body.contains("가능한가요")
        || body.contains("인가요")
        || body.contains("문의")
    {
        return Classification::Question;
    }

    // Negative first (so "not interested, thanks" is negative, not positive).
    if text.contains("not interested")
        || text.contains("no thanks")
        || text.contains("don't contact")
        || text.contains("leave me alone")
        || text.contains("관심 없")
        || text.contains("관심없")
        || text.contains("필요 없")
        || text.contains("필요없")
    {
        return Classification::HumanNegative;
    }

    let has_word = |candidate: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .any(|word| word == candidate)
    };
    if text.contains("thank")
        || text.contains("interested")
        || text.contains("sounds good")
        || text.contains("let's talk")
        || has_word("like")
        || text.contains("감사")
        || text.contains("고마워")
        || text.contains("관심 있")
        || text.contains("좋습")
        || text.contains("좋겠")
    {
        return Classification::HumanPositive;
    }

    Classification::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_deterministic_signals() {
        assert_eq!(
            classify_inbound(
                Some("Delivery Status Notification (Failure)"),
                None,
                "MAILER-DAEMON@x"
            ),
            Classification::Bounce
        );
        assert_eq!(
            classify_inbound(Some("Automatic reply: out of office"), None, "a@b.c"),
            Classification::AutoReply
        );
        assert_eq!(
            classify_inbound(None, Some("please unsubscribe me"), "a@b.c"),
            Classification::UnsubscribeRequest
        );
        assert_eq!(
            classify_inbound(None, Some("could you send pricing?"), "a@b.c"),
            Classification::Question
        );
        assert_eq!(
            classify_inbound(None, Some("not interested, thanks"), "a@b.c"),
            Classification::HumanNegative
        );
        assert_eq!(
            classify_inbound(None, Some("thanks, interested in a demo"), "a@b.c"),
            Classification::HumanPositive
        );
        assert_eq!(
            classify_inbound(None, Some("ok"), "a@b.c"),
            Classification::Unknown
        );
    }

    #[test]
    fn classifies_a_simple_like_reply_as_positive() {
        assert_eq!(
            classify_inbound(None, Some("I like it"), "a@b.c"),
            Classification::HumanPositive
        );
    }
}
