//! Setting old tool results aside from the conversation about to be sent, and
//! saying which, so the frontend's own copy ends up identical.

use crate::chat::{self, Message};
use crate::compact;

/// The previous round of a tool turn, as the webview remembers it: how many
/// characters were sent and how many tokens Ollama counted for them. It is what
/// turns "characters" into "tokens" for the next round's budget.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    chars: u64,
    tokens: u32,
}

impl Calibration {
    /// A round's own numbers: the characters it sent and the tokens Ollama counted
    /// for them.
    pub fn new(chars: u64, tokens: u32) -> Self {
        Self { chars, tokens }
    }
}

/// What [`compact_for_send`] (or [`send_whole`]) did, for the reply to carry.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    /// Characters actually sent, after anything was set aside.
    prompt_chars: u64,
    /// Indices into the webview's conversation, oldest first.
    set_aside: Vec<usize>,
    /// What each of those messages now is, in the same order, so the webview
    /// replaces them with exactly what was sent rather than re-deriving the stub.
    replacements: Vec<Message>,
    fits: bool,
}

impl Sent {
    /// Characters actually sent, after anything was set aside.
    pub fn prompt_chars(&self) -> u64 {
        self.prompt_chars
    }

    /// Indices into the frontend's conversation, oldest first.
    pub fn set_aside(&self) -> &[usize] {
        &self.set_aside
    }

    /// What each of those messages now is, in the same order.
    pub fn replacements(&self) -> &[Message] {
        &self.replacements
    }

    /// Whether the prompt fits the context once those were set aside.
    pub fn fits(&self) -> bool {
        self.fits
    }
}

/// The webview's numbering for indices into what is sent. `lead` is how many
/// messages the backend put in front (the system prompt, when a folder is
/// chosen); an index inside them is never the webview's and is dropped.
fn frontend_indices(sent: &[usize], lead: usize) -> Vec<usize> {
    sent.iter().filter_map(|i| i.checked_sub(lead)).collect()
}

/// Set aside what will not fit, in `messages` as they are about to be sent.
pub fn compact_for_send(
    messages: &mut [Message],
    offered: &[chat::Tool],
    num_ctx: u32,
    calibration: Option<Calibration>,
    lead: usize,
) -> Sent {
    let budget = compact::Budget::new(num_ctx, calibration.map(|c| (c.chars, c.tokens)));
    let plan = compact::compact(messages, offered, &budget);
    let mut replacements = Vec::new();
    for &i in &plan.set_aside {
        // The call that produced a result sits just before it.
        let call = i
            .checked_sub(1)
            .and_then(|p| messages[p].tool_calls.as_deref()?.first().cloned());
        compact::set_aside(&mut messages[i], call.as_ref());
        replacements.push(messages[i].clone());
    }
    // Anything in front of the webview's messages is never reported; `compact`
    // does not set the system prompt aside, so nothing is lost by the filter.
    let set_aside = frontend_indices(&plan.set_aside, lead);
    replacements.drain(..plan.set_aside.len() - set_aside.len());
    Sent {
        prompt_chars: compact::prompt_chars(messages, offered),
        set_aside,
        replacements,
        fits: plan.fits,
    }
}

/// Send `messages` whole, setting nothing aside: the loop as it was before H7, which
/// a probe turns [`compact_for_send`] off for to measure what it buys.
///
/// `fits` is whether the budget would have let it go **as it is** — the same plan
/// [`compact_for_send`] makes, with nothing to set aside and the estimate within the
/// hard limit. A conversation that would have needed trimming does not fit, since this
/// one is not.
pub fn send_whole(
    messages: &[Message],
    offered: &[chat::Tool],
    num_ctx: u32,
    calibration: Option<Calibration>,
) -> Sent {
    let budget = compact::Budget::new(num_ctx, calibration.map(|c| (c.chars, c.tokens)));
    let plan = compact::compact(messages, offered, &budget);
    Sent {
        prompt_chars: compact::prompt_chars(messages, offered),
        set_aside: Vec::new(),
        replacements: Vec::new(),
        fits: plan.fits && plan.set_aside.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_are_reported_in_the_webviews_numbering_and_the_system_prompt_is_never_one() {
        assert_eq!(frontend_indices(&[3, 5], 1), vec![2, 4]);
        assert_eq!(frontend_indices(&[3, 5], 0), vec![3, 5]);
        assert_eq!(frontend_indices(&[0, 3], 1), vec![2]);
    }

    /// The pairs a webview would hold, with the system prompt in front.
    fn long_turn() -> Vec<Message> {
        let mut m = vec![Message::system("sys"), Message::user("find it")];
        for _ in 0..4 {
            m.push(Message {
                role: "assistant".into(),
                content: String::new(),
                images: None,
                tool_calls: Some(vec![chat::ToolCall {
                    id: None,
                    function: chat::FunctionCall {
                        name: "search".into(),
                        arguments: serde_json::json!({ "text": "x" }),
                    },
                }]),
                tool_name: None,
                thinking: None,
            });
            m.push(Message::tool_result("search", "x".repeat(6000)));
        }
        m
    }

    #[test]
    fn what_is_set_aside_comes_back_numbered_without_the_system_prompt_and_as_sent() {
        let mut messages = long_turn();
        // 24000 characters at 2.5 a token is 9600 tokens; 0.75 of 8192 is 6144.
        let sent = compact_for_send(&mut messages, &[], 8192, None, 1);
        // Sent indices 3 and 5 are the webview's 2 and 4; the last result stays.
        assert_eq!(sent.set_aside, vec![2, 4]);
        assert!(sent.fits);
        assert_eq!(sent.replacements, vec![messages[3].clone(), messages[5].clone()]);
        assert!(sent.replacements[0].content.starts_with(compact::STUB_MARKER));
        assert_eq!(sent.prompt_chars, compact::prompt_chars(&messages, &[]));
        assert_eq!(messages[9].content.len(), 6000);
    }

    #[test]
    fn sending_whole_sets_nothing_aside_and_says_whether_it_would_have_fit_as_it_is() {
        let messages = long_turn();
        let before = messages.clone();
        // The conversation `compact_for_send` trims to fit 8192 goes whole, and so
        // does not fit as it is.
        let sent = send_whole(&messages, &[], 8192, None);
        assert!(sent.set_aside().is_empty() && sent.replacements().is_empty());
        assert!(!sent.fits());
        assert_eq!(sent.prompt_chars(), compact::prompt_chars(&messages, &[]));
        assert_eq!(messages, before, "the caller's conversation is not touched");
        // The same conversation in a context with room for it.
        let roomy = send_whole(&messages, &[], 32768, None);
        assert!(roomy.set_aside().is_empty() && roomy.fits());
        // What is counted as sent is the whole conversation, whatever the context.
        assert_eq!(roomy.prompt_chars(), sent.prompt_chars());
    }
}
