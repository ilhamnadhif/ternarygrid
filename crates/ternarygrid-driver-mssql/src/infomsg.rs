//! SQL Server's informational messages: `PRINT`, `RAISERROR` of severity 10 or less,
//! warnings ("Null value is eliminated by an aggregate"), "Changed database context".
//!
//! tiberius reads their INFO tokens and drops them, but it logs each one as a DEBUG
//! `tracing` event of `tiberius::tds::stream::token`. A statement task runs under a
//! dispatcher of its own that keeps those messages (for the task that read them) and hands
//! every other event, span and callsite on to the app's dispatcher, as before.

use std::cell::RefCell;
use std::sync::OnceLock;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Level, Metadata, Subscriber};

/// Where tiberius logs the tokens it reads.
const TOKEN_TARGET: &str = "tiberius::tds::stream::token";

/// The same events carry the environment changes tiberius logs (not messages of the server).
const ENV_CHANGES: [&str; 10] = [
    "Database change from '",
    "Packet size change from '",
    "SQL collation change",
    "Begin transaction",
    "Commit transaction",
    "Rollback transaction",
    "Defect transaction",
    "Server requested routing",
    "Fallback mirror server",
    "Ignored env change",
];

tokio::task_local! {
    static SINK: RefCell<Vec<String>>;
}

/// Run `fut` (a statement task) with its server messages kept for [`take`].
pub(crate) async fn collecting<F: std::future::Future>(fut: F) -> F::Output {
    use tracing::instrument::WithSubscriber;
    SINK.scope(RefCell::new(Vec::new()), fut).with_subscriber(dispatch()).await
}

/// The messages read since the last call (none outside [`collecting`]).
pub(crate) fn take() -> Vec<String> {
    SINK.try_with(|s| std::mem::take(&mut *s.borrow_mut())).unwrap_or_default()
}

/// One for the process: creating a dispatcher re-evaluates every callsite.
fn dispatch() -> Dispatch {
    static DISPATCH: OnceLock<(Dispatch, Dispatch)> = OnceLock::new();
    DISPATCH
        .get_or_init(|| {
            // The app's dispatcher (this runs outside any task's scoped one).
            let inner = tracing::dispatcher::get_default(Dispatch::clone);
            // Keep a second live dispatcher registered so `tracing-core` stays out
            // of its single-dispatcher (`has_just_one`) fast path when no global
            // subscriber is installed (e.g. tests or library callers). Without it,
            // a callsite first encountered outside `collecting` (such as a concurrent
            // `connect()` reading login INFO tokens) evaluates only the calling
            // thread's default (`NoSubscriber`) and permanently caches
            // `Interest::never()`, silencing later `PRINT` / `RAISERROR` messages.
            let ambient = Dispatch::new(Ambient(inner.clone()));
            let capture = Dispatch::new(Capture { inner });
            (capture, ambient)
        })
        .0
        .clone()
}

/// Sentinel kept alongside [`Capture`] so `tracing-core`'s callsite registry
/// always combines interest across every registered dispatcher.
struct Ambient(Dispatch);

impl Subscriber for Ambient {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        self.0.register_callsite(meta)
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        // Never raise the global max level beyond what the app's own dispatcher
        // and `Capture` already request.
        Some(tracing::level_filters::LevelFilter::OFF)
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        self.0.enabled(meta)
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        self.0.new_span(span)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        self.0.record(span, values)
    }

    fn record_follows_from(&self, span: &Id, follows: &Id) {
        self.0.record_follows_from(span, follows)
    }

    fn event(&self, event: &Event<'_>) {
        self.0.event(event)
    }

    fn enter(&self, span: &Id) {
        self.0.enter(span)
    }

    fn exit(&self, span: &Id) {
        self.0.exit(span)
    }
}

fn is_token_event(meta: &Metadata<'_>) -> bool {
    meta.is_event() && *meta.level() == Level::DEBUG && meta.target() == TOKEN_TARGET
}

/// A server message, not an environment change or the login acknowledgement.
fn is_server_message(text: &str) -> bool {
    !text.is_empty() && !ENV_CHANGES.iter().any(|p| text.starts_with(p))
}

struct Capture {
    inner: Dispatch,
}

#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl Subscriber for Capture {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        let inner = self.inner.register_callsite(meta);
        if is_token_event(meta) { Interest::always() } else { inner }
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        // The app's own dispatcher keeps its level; this one needs DEBUG for the tokens.
        Some(tracing::level_filters::LevelFilter::DEBUG)
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        is_token_event(meta) || self.inner.enabled(meta)
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        self.inner.new_span(span)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        self.inner.record(span, values)
    }

    fn record_follows_from(&self, span: &Id, follows: &Id) {
        self.inner.record_follows_from(span, follows)
    }

    fn event(&self, event: &Event<'_>) {
        if is_token_event(event.metadata()) {
            let mut m = Message::default();
            event.record(&mut m);
            if is_server_message(&m.0) {
                let _ = SINK.try_with(|s| s.borrow_mut().push(m.0));
            }
        }
        if self.inner.enabled(event.metadata()) {
            self.inner.event(event);
        }
    }

    fn enter(&self, span: &Id) {
        self.inner.enter(span)
    }

    fn exit(&self, span: &Id) {
        self.inner.exit(span)
    }

    fn clone_span(&self, id: &Id) -> Id {
        self.inner.clone_span(id)
    }

    fn try_close(&self, id: Id) -> bool {
        self.inner.try_close(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The messages tiberius logs while a statement task reads them are kept for that task;
    /// its environment changes are not messages.
    #[tokio::test]
    async fn info_tokens_become_messages() {
        let got = collecting(async {
            tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", "step 1 done");
            tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", "Database change from 'a' to 'b'");
            tracing::event!(target: "other", Level::DEBUG, "{}", "not a token");
            let first = take();
            tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", "Warning: Null value is eliminated by an aggregate or other SET operation.");
            (first, take())
        })
        .await;
        assert_eq!(got.0, vec!["step 1 done".to_string()]);
        assert_eq!(got.1.len(), 1);
        // Outside a statement task nothing is kept.
        tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", "elsewhere");
        assert!(take().is_empty());
    }

    /// A token callsite first hit outside `collecting` (e.g. a concurrent `connect()`
    /// reading login INFO tokens after another query raised `MAX_LEVEL`) must still
    /// be captured when later hit inside `collecting`.
    #[tokio::test]
    async fn callsite_first_hit_outside_collecting_is_still_captured() {
        fn emit_token(msg: &str) {
            tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", msg);
        }

        // Initialise the capture dispatcher first, raising MAX_LEVEL to DEBUG.
        collecting(async {}).await;
        // Register `emit_token`'s callsite while no scoped subscriber is active.
        emit_token("Changed database context to 'master'");
        // Later invocations inside `collecting` must still capture the message.
        let got = collecting(async {
            emit_token("step one");
            take()
        })
        .await;
        assert_eq!(got, vec!["step one".to_string()]);

        // Dropping another scoped dispatcher triggers `tracing-core`'s live-dispatcher
        // pruning; a brand-new callsite hit outside `collecting` afterwards must still
        // be captured.
        fn emit_token_after_prune(msg: &str) {
            tracing::event!(target: "tiberius::tds::stream::token", Level::DEBUG, "{}", msg);
        }
        {
            let _temp = Dispatch::new(tracing::subscriber::NoSubscriber::new());
        }
        emit_token_after_prune("Changed language setting to us_english.");
        let got2 = collecting(async {
            emit_token_after_prune("step two");
            take()
        })
        .await;
        assert_eq!(got2, vec!["step two".to_string()]);
    }
}
