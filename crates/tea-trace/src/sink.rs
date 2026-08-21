//! The observer boundary for linear traces.
//!
//! A sink is an output side effect, not part of agent execution.  The runtime
//! owns event ordering and may choose to disable a sink.  In particular, a
//! sink must never be able to make a model/tool state transition happen twice.
//! [`IsolatedSink`] is provided for integrations that want the usual
//! best-effort behavior: an individual sink failure is counted and swallowed.

use std::convert::Infallible;

use crate::event::TraceEvent;

/// Receives owned, already-redacted trace events in append order.
///
/// Implementations should keep this operation bounded and should not call back
/// into the agent runtime.  The associated error is intentionally generic so a
/// file, channel, test, or remote sink can choose its own error type without
/// adding a dependency to this crate.
pub trait TraceSink {
    /// Error returned by this sink.  A sink error is not an episode event.
    type Error;

    /// Appends one event to the sink.
    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error>;

    /// Alias for callers that use event-oriented terminology.
    fn record(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        self.append(event)
    }

    /// Alias for callers that use emission-oriented terminology.
    fn emit(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        self.append(event)
    }
}

/// A sink that deliberately discards all events.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopSink;

impl TraceSink for NoopSink {
    type Error = Infallible;

    fn append(&mut self, _event: TraceEvent) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A sink wrapper that isolates output failures from agent execution.
///
/// `IsolatedSink` always returns `Ok(())` from its [`TraceSink`] implementation.
/// If the wrapped sink fails, the event is dropped and [`Self::failed_events`]
/// is incremented.  This is intentionally observable for metrics and tests,
/// while the wrapped error itself is not retained: retaining arbitrary sink
/// errors can retain secrets or large buffers.
pub struct IsolatedSink<S> {
    inner: S,
    failed_events: u64,
}

impl<S> IsolatedSink<S> {
    /// Wraps a sink with failure isolation.
    pub const fn new(inner: S) -> Self {
        Self {
            inner,
            failed_events: 0,
        }
    }

    /// Number of events rejected by the wrapped sink.
    pub const fn failed_events(&self) -> u64 {
        self.failed_events
    }

    /// Returns the wrapped sink and the wrapper's failure count is discarded.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Borrows the wrapped sink for inspection or host-specific operations.
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Mutably borrows the wrapped sink for host-specific operations.
    pub const fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }
}

impl<S: TraceSink> TraceSink for IsolatedSink<S> {
    type Error = Infallible;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        if self.inner.append(event).is_err() {
            self.failed_events = self.failed_events.saturating_add(1);
        }
        Ok(())
    }
}

/// An in-memory sink useful for tests and small host integrations.
impl TraceSink for Vec<TraceEvent> {
    type Error = Infallible;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        self.push(event);
        Ok(())
    }
}
