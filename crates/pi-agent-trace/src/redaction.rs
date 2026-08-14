//! Redaction at the trace boundary.
//!
//! The trace crate cannot know which model prompts, tool arguments, or tool
//! results are sensitive.  A host therefore supplies a [`Redactor`] and wraps
//! its sink in [`RedactingSink`].  Redaction happens before the event reaches
//! the sink; it is not a storage-format feature and it cannot be retrofitted
//! after an event has been written.
//!
//! A redactor should preserve the event kind and lifecycle while replacing or
//! removing sensitive payloads.  It must not use redaction to change replay
//! semantics.  [`NoRedaction`] is available for explicitly trusted fixtures,
//! but production integrations should choose a policy deliberately.

use crate::event::TraceEvent;
use crate::sink::TraceSink;

/// Converts an owned trace event into the form allowed to leave the runtime.
pub trait Redactor {
    /// Redacts sensitive values while preserving the event's meaning and kind.
    fn redact(&self, event: TraceEvent) -> TraceEvent;
}

/// An explicit identity policy for trusted data.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRedaction;

impl Redactor for NoRedaction {
    fn redact(&self, event: TraceEvent) -> TraceEvent {
        event
    }
}

/// Applies a redaction policy before forwarding events to another sink.
pub struct RedactingSink<S, R> {
    inner: S,
    redactor: R,
}

impl<S, R> RedactingSink<S, R> {
    /// Creates a sink that applies `redactor` before each append.
    pub const fn new(inner: S, redactor: R) -> Self {
        Self { inner, redactor }
    }

    /// Borrows the wrapped sink.
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Mutably borrows the wrapped sink.
    pub const fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Borrows the redaction policy.
    pub const fn redactor(&self) -> &R {
        &self.redactor
    }

    /// Returns the wrapped sink, dropping the policy value.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Returns both wrapped values.
    pub fn into_parts(self) -> (S, R) {
        (self.inner, self.redactor)
    }
}

impl<S, R> TraceSink for RedactingSink<S, R>
where
    S: TraceSink,
    R: Redactor,
{
    type Error = S::Error;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        self.inner.append(self.redactor.redact(event))
    }
}
