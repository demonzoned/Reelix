//! Transient notifications shown in the corner of the screen.

use std::time::Duration;

use leptos::prelude::*;

const AUTO_DISMISS: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Info,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
    /// What the toast is about, such as one provider's test, so a later success can clear the
    /// errors it answers. `None` for toasts no later result answers.
    pub topic: Option<String>,
}

/// Most toasts shown at once; the oldest go first.
pub const MAX_TOASTS: usize = 5;

/// Adds `toast` to the stack. A toast with the same kind and message as an earlier one replaces it,
/// and an error replaces an earlier error about the same topic, so repeats do not pile up.
fn add(items: &mut Vec<Toast>, toast: Toast) {
    items.retain(|t| {
        let repeat = t.kind == toast.kind && t.message == toast.message;
        let same_topic_error = toast.kind == ToastKind::Error
            && t.kind == ToastKind::Error
            && toast.topic.is_some()
            && t.topic == toast.topic;
        !repeat && !same_topic_error
    });
    items.push(toast);
    while items.len() > MAX_TOASTS {
        items.remove(0);
    }
}

/// Removes the errors about `topic`, which a later success has answered.
fn answer(items: &mut Vec<Toast>, topic: &str) {
    items.retain(|t| !(t.kind == ToastKind::Error && t.topic.as_deref() == Some(topic)));
}

#[derive(Clone, Copy)]
pub struct Toasts {
    pub items: RwSignal<Vec<Toast>>,
    next_id: StoredValue<u64>,
}

impl Toasts {
    pub fn new() -> Self {
        Self {
            items: RwSignal::new(Vec::new()),
            next_id: StoredValue::new(1),
        }
    }

    pub fn expect() -> Self {
        expect_context()
    }

    pub fn push(&self, kind: ToastKind, message: impl Into<String>) {
        self.push_about(kind, None, message);
    }

    fn push_about(&self, kind: ToastKind, topic: Option<String>, message: impl Into<String>) {
        let id = self.next_id.get_value();
        self.next_id.set_value(id + 1);
        self.items.update(|items| {
            add(
                items,
                Toast {
                    id,
                    kind,
                    message: message.into(),
                    topic,
                },
            );
        });
        // Errors stay until dismissed so there's time to read them (WCAG 2.2.1).
        if kind != ToastKind::Error {
            let this = *self;
            set_timeout(move || this.dismiss(id), AUTO_DISMISS);
        }
    }

    pub fn success(&self, message: impl Into<String>) {
        self.push(ToastKind::Success, message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.push(ToastKind::Error, message);
    }

    pub fn info(&self, message: impl Into<String>) {
        self.push(ToastKind::Info, message);
    }

    /// An error about `topic`; it replaces an earlier error about the same topic.
    pub fn error_about(&self, topic: impl Into<String>, message: impl Into<String>) {
        self.push_about(ToastKind::Error, Some(topic.into()), message);
    }

    /// A success about `topic`: clears the errors about it, then shows `message`.
    pub fn success_about(&self, topic: impl Into<String>, message: impl Into<String>) {
        let topic = topic.into();
        self.answered(&topic);
        self.push_about(ToastKind::Success, Some(topic), message);
    }

    /// Clears the errors about `topic` without a message of its own (the result shows elsewhere).
    pub fn answered(&self, topic: &str) {
        if self
            .items
            .try_with_untracked(|items| items.iter().any(|t| t.topic.as_deref() == Some(topic)))
            == Some(true)
        {
            self.items.update(|items| answer(items, topic));
        }
    }

    /// Removes every toast, errors included.
    pub fn dismiss_all(&self) {
        if self.items.try_with_untracked(|items| !items.is_empty()) == Some(true) {
            self.items.update(Vec::clear);
        }
    }

    pub fn dismiss(&self, id: u64) {
        // The signal may already be disposed if the app unmounted.
        if self
            .items
            .try_with_untracked(|items| items.iter().any(|t| t.id == id))
            == Some(true)
        {
            self.items.update(|items| items.retain(|t| t.id != id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(id: u64, kind: ToastKind, message: &str, topic: Option<&str>) -> Toast {
        Toast {
            id,
            kind,
            message: message.to_owned(),
            topic: topic.map(str::to_owned),
        }
    }

    fn ids(items: &[Toast]) -> Vec<u64> {
        items.iter().map(|t| t.id).collect()
    }

    #[test]
    fn a_repeat_replaces_the_earlier_toast() {
        let mut items = Vec::new();
        add(
            &mut items,
            toast(1, ToastKind::Error, "Could not save: 401", None),
        );
        add(
            &mut items,
            toast(2, ToastKind::Error, "Test failed: timeout", None),
        );
        add(
            &mut items,
            toast(3, ToastKind::Error, "Could not save: 401", None),
        );
        assert_eq!(ids(&items), [2, 3], "the repeat moves to the newest place");
        // The same words as a success are not a repeat of the error.
        add(
            &mut items,
            toast(4, ToastKind::Success, "Could not save: 401", None),
        );
        assert_eq!(ids(&items), [2, 3, 4]);
    }

    #[test]
    fn an_error_replaces_an_earlier_error_about_the_same_topic() {
        let mut items = Vec::new();
        add(
            &mut items,
            toast(
                1,
                ToastKind::Error,
                "Test failed: 401",
                Some("downloadclient:3:test"),
            ),
        );
        add(
            &mut items,
            toast(2, ToastKind::Error, "Test failed: 401", Some("indexer:3:test")),
        );
        add(
            &mut items,
            toast(
                3,
                ToastKind::Error,
                "Test failed: timeout",
                Some("downloadclient:3:test"),
            ),
        );
        assert_eq!(ids(&items), [2, 3]);
    }

    #[test]
    fn a_success_clears_only_the_errors_it_answers() {
        let mut items = Vec::new();
        add(
            &mut items,
            toast(
                1,
                ToastKind::Error,
                "Test failed: 401",
                Some("downloadclient:3:test"),
            ),
        );
        add(
            &mut items,
            toast(
                2,
                ToastKind::Error,
                "Could not save: 400",
                Some("downloadclient:3:save"),
            ),
        );
        add(
            &mut items,
            toast(3, ToastKind::Error, "Could not load the queue", None),
        );
        answer(&mut items, "downloadclient:3:test");
        assert_eq!(ids(&items), [2, 3]);
    }

    #[test]
    fn at_most_five_show_at_once() {
        let mut items = Vec::new();
        for id in 1..=7 {
            add(
                &mut items,
                toast(id, ToastKind::Error, &format!("Error {id}"), None),
            );
        }
        assert_eq!(ids(&items), [3, 4, 5, 6, 7]);
    }
}
