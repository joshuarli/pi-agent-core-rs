//! Agent ownership and configuration.
//!
//! An [`Agent`] owns durable conversation state and permits exactly one active [`RunHandle`].
//! It has no executor and no provider implementation; callers configure those explicit
//! capabilities and drive the run from their own async environment.

use crate::default_tools::DefaultCodingTools;
use crate::error::CoreError;
use crate::event::EventObserver;
use crate::hooks::{HookSet, NoHooks};
use crate::profile::PiDefaultCodingProfile;
use crate::queue::{AgentQueues, QueueMode, QueuedMessage};
use crate::run::RunHandle;
use crate::scheduler::{CancellationToken, ModelProvider};
use crate::state::{
    AgentPhase, AgentSnapshot, AgentState, Message, ModelDescriptor, ThinkingLevel,
};
use crate::tool::{AgentTool, ToolRegistry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Poll, Waker};

/// Internal shared ownership record used by `Agent` and its run handle.
pub(crate) struct AgentInner {
    /// Durable and transient state.
    pub(crate) state: Mutex<AgentState>,
    /// Explicit queue state, not a general mailbox.
    pub(crate) queues: Mutex<AgentQueues>,
    /// Current run ownership marker.
    pub(crate) active_run: Mutex<Option<ActiveRun>>,
    /// Host capabilities and policy.
    pub(crate) tools: ToolRegistry,
    /// Drain policy for messages that steer an active run.
    pub(crate) steering_mode: Mutex<QueueMode>,
    /// Drain policy for messages that run only at the idle boundary.
    pub(crate) follow_up_mode: Mutex<QueueMode>,
    /// Optional model provider, driven externally.
    pub(crate) provider: RwLock<Option<Arc<dyn ModelProvider>>>,
    /// Optional caller-supplied compactor, driven externally.
    pub(crate) compactor: RwLock<Option<Arc<dyn crate::compaction::Compactor>>>,
    /// Hooks are held for the run loop boundary.
    pub(crate) hooks: Arc<dyn HookSet>,
    /// Awaited observers in registration order.
    pub(crate) observers: Mutex<Vec<ObserverRegistration>>,
    /// Monotonic process-local observer registrations.
    pub(crate) next_observer_id: AtomicU64,
    /// Non-blocking event subscribers that do not participate in settlement.
    pub(crate) subscribers: Mutex<Vec<SubscriberRegistration>>,
    /// Monotonic process-local non-blocking subscription registrations.
    pub(crate) next_subscriber_id: AtomicU64,
    /// Lossless live event subscribers that do not participate in settlement.
    pub(crate) lossless_subscribers: Mutex<Vec<LosslessSubscriberRegistration>>,
    /// Monotonic process-local lossless subscription registrations.
    pub(crate) next_lossless_subscriber_id: AtomicU64,
    /// Monotonic process-local run IDs.
    pub(crate) next_run_id: AtomicU64,
    /// Wakers awaiting the post-settlement idle boundary.
    pub(crate) idle_notifier: IdleNotifier,
}

/// A small executor-neutral idle notification primitive.
///
/// The agent owns no runtime, so this keeps only the wakers supplied by an
/// embedding executor. A settlement drains and wakes them after it has made
/// the agent idle.
#[derive(Default)]
pub(crate) struct IdleNotifier {
    waiters: Mutex<Vec<Waker>>,
}

impl IdleNotifier {
    fn register(&self, waker: &Waker) {
        let mut waiters = self.waiters.lock().expect("idle waiter mutex poisoned");
        if !waiters.iter().any(|existing| existing.will_wake(waker)) {
            waiters.push(waker.clone());
        }
    }

    pub(crate) fn notify(&self) {
        let waiters =
            std::mem::take(&mut *self.waiters.lock().expect("idle waiter mutex poisoned"));
        for waker in waiters {
            waker.wake();
        }
    }
}

/// Shared active-run marker, allowing `Agent::abort` to reach the handle's state without
/// keeping a second owning handle alive.
#[derive(Clone)]
pub(crate) struct ActiveRun {
    pub(crate) id: crate::state::RunId,
    pub(crate) state: Arc<Mutex<crate::state::RunState>>,
    pub(crate) cancellation: CancellationToken,
}

/// An owned agent state machine.
#[derive(Clone)]
pub struct Agent {
    pub(crate) inner: Arc<AgentInner>,
}

/// An owned registration for an awaited lifecycle observer.
///
/// Dropping this value removes the observer.  The removal affects events that
/// have not yet begun observer delivery; an observer snapshot already being
/// delivered remains stable for that event.  This makes unsubscribe from an
/// observer callback safe and deterministic without holding the registry lock
/// across an awaited callback.
#[must_use = "drop the subscription to unsubscribe, or retain it for the desired observation lifetime"]
pub struct ObserverSubscription {
    agent: std::sync::Weak<AgentInner>,
    id: u64,
}

impl std::fmt::Debug for ObserverSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObserverSubscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for ObserverSubscription {
    fn drop(&mut self) {
        if let Some(agent) = self.agent.upgrade() {
            let mut observers = agent.observers.lock().expect("observer mutex poisoned");
            observers.retain(|registration| registration.id != self.id);
        }
    }
}

/// One observer retained by the agent.
#[derive(Clone)]
pub(crate) struct ObserverRegistration {
    pub(crate) id: u64,
    pub(crate) observer: Arc<dyn EventObserver>,
}

/// A bounded, non-blocking lifecycle-event subscription.
///
/// Unlike [`ObserverSubscription`], receiving events from this subscription
/// never keeps an agent run active. A full queue drops the new event and
/// increments [`Self::dropped_events`], preserving source order for events
/// that are retained without creating backpressure in the run loop.
#[must_use = "drop the subscription to stop receiving events"]
pub struct EventSubscription {
    agent: std::sync::Weak<AgentInner>,
    id: u64,
    receiver: Receiver<crate::event::AgentEvent>,
    dropped: Arc<AtomicU64>,
}

impl std::fmt::Debug for EventSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSubscription")
            .field("id", &self.id)
            .field("dropped_events", &self.dropped_events())
            .finish_non_exhaustive()
    }
}

impl EventSubscription {
    /// Return the next queued event without waiting.
    pub fn try_recv(&self) -> Result<crate::event::AgentEvent, TryRecvError> {
        self.receiver.try_recv()
    }

    /// Number of events discarded because this subscription's queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Acquire)
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        if let Some(agent) = self.agent.upgrade() {
            let mut subscribers = agent.subscribers.lock().expect("subscriber mutex poisoned");
            subscribers.retain(|registration| registration.id != self.id);
        }
    }
}

/// A lossless, unbounded lifecycle-event subscription.
///
/// Unlike [`EventSubscription`], this subscription never drops an event because
/// of queue capacity. Events are enqueued in the core's sequence order and
/// publishing does not wait for the receiver to drain them or for an executor
/// task to run. The queue is intentionally unbounded: unread events consume
/// caller-owned memory until they are drained or this subscription is dropped.
/// Dropping the subscription releases the receiver and unregisters it from the
/// agent; subsequent sends are harmless.
#[must_use = "drop the subscription to stop receiving events"]
pub struct LosslessEventSubscription {
    agent: std::sync::Weak<AgentInner>,
    id: u64,
    receiver: Receiver<crate::event::AgentEvent>,
}

impl std::fmt::Debug for LosslessEventSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LosslessEventSubscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl LosslessEventSubscription {
    /// Return the next queued event without waiting.
    pub fn try_recv(&self) -> Result<crate::event::AgentEvent, TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for LosslessEventSubscription {
    fn drop(&mut self) {
        if let Some(agent) = self.agent.upgrade() {
            let mut subscribers = agent
                .lossless_subscribers
                .lock()
                .expect("lossless subscriber mutex poisoned");
            subscribers.retain(|registration| registration.id != self.id);
        }
    }
}

/// One bounded non-blocking event subscription retained by the agent.
#[derive(Clone)]
pub(crate) struct SubscriberRegistration {
    pub(crate) id: u64,
    pub(crate) sender: SyncSender<crate::event::AgentEvent>,
    pub(crate) dropped: Arc<AtomicU64>,
}

/// One unbounded lossless event subscription retained by the agent.
#[derive(Clone)]
pub(crate) struct LosslessSubscriberRegistration {
    pub(crate) id: u64,
    pub(crate) sender: Sender<crate::event::AgentEvent>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Agent {
    /// Start configuration with an empty profile and no provider.
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    /// Return an owned state snapshot.
    pub fn snapshot(&self) -> AgentSnapshot {
        self.inner
            .state
            .lock()
            .expect("agent state mutex poisoned")
            .snapshot()
    }

    /// Return prompt definitions for the currently registered capabilities.
    pub fn tool_definitions(&self) -> Vec<crate::tool::ToolDefinition> {
        self.inner.tools.definitions()
    }

    /// Whether an explicit model provider was configured.
    pub fn has_model_provider(&self) -> bool {
        self.inner
            .provider
            .read()
            .expect("agent provider lock poisoned")
            .is_some()
    }

    /// Atomically replace the configured model identity and provider while idle.
    ///
    /// The replacement preserves the retained linear conversation, tools,
    /// prompts, and explicit queues. A run owns its model/provider pair until
    /// terminal settlement, so this operation rejects active and cancelling
    /// agents rather than changing a provider beneath live model or tool work.
    /// The caller constructs the provider explicitly and is responsible for
    /// validating any provider-specific credential/configuration invariants
    /// before calling this operation.
    pub fn replace_model_provider(
        &self,
        model: ModelDescriptor,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<(), CoreError> {
        let mut provider_slot = self
            .inner
            .provider
            .write()
            .expect("agent provider lock poisoned");
        let mut state = self.inner.state.lock().expect("agent state mutex poisoned");
        match state.phase {
            AgentPhase::Idle => {
                state.model = Some(model);
                *provider_slot = Some(provider);
                Ok(())
            }
            AgentPhase::Running(run_id) | AgentPhase::Cancelling(run_id) => {
                Err(CoreError::ActiveRun { run_id })
            }
        }
    }

    /// Clone the host policy handle used at the run-loop boundary.
    pub fn hooks(&self) -> Arc<dyn HookSet> {
        Arc::clone(&self.inner.hooks)
    }

    /// Register an awaited lifecycle observer.
    ///
    /// Observers are invoked in registration order for every future event and
    /// are awaited as part of the run.  Keep the returned subscription alive
    /// for as long as observation is wanted; dropping it unsubscribes.  A
    /// registration made from an observer callback begins with the next event.
    pub fn subscribe(&self, observer: Arc<dyn EventObserver>) -> ObserverSubscription {
        let id = self
            .inner
            .next_observer_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.inner
            .observers
            .lock()
            .expect("observer mutex poisoned")
            .push(ObserverRegistration { id, observer });
        ObserverSubscription {
            agent: Arc::downgrade(&self.inner),
            id,
        }
    }

    /// Subscribe to a bounded, non-blocking copy of future lifecycle events.
    ///
    /// This is separate from [`Self::subscribe`]. Events are sent after
    /// awaited observer delivery with a bounded `try_send`; a slow consumer
    /// can neither delay settlement nor cause a background task. When the
    /// queue is full, the new event is dropped and
    /// [`EventSubscription::dropped_events`] records it.
    pub fn subscribe_nonblocking(&self, capacity: std::num::NonZeroUsize) -> EventSubscription {
        let id = self
            .inner
            .next_subscriber_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let (sender, receiver) = sync_channel(capacity.get());
        let dropped = Arc::new(AtomicU64::new(0));
        self.inner
            .subscribers
            .lock()
            .expect("subscriber mutex poisoned")
            .push(SubscriberRegistration {
                id,
                sender,
                dropped: Arc::clone(&dropped),
            });
        EventSubscription {
            agent: Arc::downgrade(&self.inner),
            id,
            receiver,
            dropped,
        }
    }

    /// Subscribe to an unbounded, lossless copy of future lifecycle events.
    ///
    /// This path is separate from [`Self::subscribe_nonblocking`]. Every event
    /// is sent in sequence order while the receiver is alive; no bounded
    /// overflow or hidden lossy fallback exists. The unbounded queue is owned
    /// by the caller, so a receiver that is not drained retains every event and
    /// can grow without limit. Dropping the returned subscription releases that
    /// queued memory, unregisters the receiver, and never delays run settlement.
    pub fn subscribe_lossless(&self) -> LosslessEventSubscription {
        let id = self
            .inner
            .next_lossless_subscriber_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let (sender, receiver) = channel();
        self.inner
            .lossless_subscribers
            .lock()
            .expect("lossless subscriber mutex poisoned")
            .push(LosslessSubscriberRegistration { id, sender });
        LosslessEventSubscription {
            agent: Arc::downgrade(&self.inner),
            id,
            receiver,
        }
    }

    /// Resolve after the active run has fully settled and the agent is idle.
    ///
    /// In particular, awaited observers for the terminal `AgentEnd` event
    /// run before this future resolves.
    pub async fn wait_for_idle(&self) {
        std::future::poll_fn(|context| {
            if self.is_idle() {
                return Poll::Ready(());
            }
            self.inner.idle_notifier.register(context.waker());
            if self.is_idle() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }

    /// Start a prompt run.  No model work is performed until the caller drives the returned
    /// handle on its own executor.
    pub fn start_prompt(&self, prompt: impl Into<String>) -> Result<RunHandle, CoreError> {
        self.start_run(vec![prompt.into()], false)
    }

    /// Continue from a retained user or tool-result message without adding a new prompt.
    ///
    /// If the transcript ends in an assistant message, Pi permits continuation only by consuming
    /// queued steering first, then queued follow-up input. Those consumed messages become the
    /// next run's prompt events; remaining steering is deliberately deferred until after its
    /// first assistant turn.
    pub fn start_continue(&self) -> Result<RunHandle, CoreError> {
        let assistant_tail = {
            let state = self.inner.state.lock().expect("agent state mutex poisoned");
            match state.phase {
                AgentPhase::Running(run_id) | AgentPhase::Cancelling(run_id) => {
                    return Err(CoreError::ActiveRun { run_id });
                }
                AgentPhase::Idle => {}
            }
            match state.messages.last() {
                None => {
                    return Err(CoreError::InvalidTransition(
                        crate::error::StateTransitionError::new("agent", "empty", "continue"),
                    ));
                }
                Some(Message::Assistant { .. }) => true,
                Some(Message::User { .. } | Message::ToolResult { .. }) => false,
            }
        };
        if !assistant_tail {
            return self.start_run(Vec::new(), false);
        }

        let queued = self.drain_continue_tail_messages();
        if queued.is_empty() {
            return Err(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new("agent", "assistant-tail", "continue"),
            ));
        }
        self.start_run(
            queued.into_iter().map(|message| message.content).collect(),
            true,
        )
    }

    fn drain_continue_tail_messages(&self) -> Vec<QueuedMessage> {
        let steering_mode = self.steering_mode();
        let follow_up_mode = self.follow_up_mode();
        let mut queues = self
            .inner
            .queues
            .lock()
            .expect("agent queue mutex poisoned");
        let steering = queues.steering.drain(steering_mode);
        if steering.is_empty() {
            queues.follow_up.drain(follow_up_mode)
        } else {
            steering
        }
    }

    fn start_run(
        &self,
        initial_contents: Vec<String>,
        skip_initial_steering: bool,
    ) -> Result<RunHandle, CoreError> {
        let run_number = self
            .inner
            .next_run_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let run_id = crate::state::RunId(run_number);
        let mut state = self.inner.state.lock().expect("agent state mutex poisoned");
        if let AgentPhase::Running(active) | AgentPhase::Cancelling(active) = state.phase {
            return Err(CoreError::ActiveRun { run_id: active });
        }
        let message_start_index = state.messages.len();
        let initial_messages = initial_contents
            .into_iter()
            .map(|content| Message::User {
                id: state.allocate_message_id(),
                content,
            })
            .collect::<Vec<_>>();
        state.messages.extend(initial_messages.iter().cloned());
        state.phase = AgentPhase::Running(run_id);
        state.last_error = None;
        state.partial_response = None;
        state.is_streaming = false;
        state.pending_tool_calls.clear();
        drop(state);
        let run_state = Arc::new(Mutex::new(crate::state::RunState::created(run_id)));
        let cancellation = CancellationToken::new();
        *self
            .inner
            .active_run
            .lock()
            .expect("active run mutex poisoned") = Some(ActiveRun {
            id: run_id,
            state: Arc::clone(&run_state),
            cancellation: cancellation.clone(),
        });
        Ok(RunHandle {
            agent: Arc::downgrade(&self.inner),
            state: run_state,
            cancellation,
            initial_messages,
            message_start_index,
            skip_initial_steering,
        })
    }

    /// Queue steering input for the next eligible active-turn drain point.
    ///
    /// Pi permits queuing while idle as well as while a run is active. Idle input is consumed by
    /// the next prompt/continuation run rather than implicitly starting work.
    pub fn enqueue_steering(&self, content: impl Into<String>) -> Result<u64, CoreError> {
        Ok(self
            .inner
            .queues
            .lock()
            .expect("queue mutex poisoned")
            .steering
            .push(content))
    }

    /// Queue follow-up input for the next idle boundary of a run.
    ///
    /// Idle input waits for an explicit prompt or continuation; queuing is never an implicit run.
    pub fn enqueue_follow_up(&self, content: impl Into<String>) -> Result<u64, CoreError> {
        Ok(self
            .inner
            .queues
            .lock()
            .expect("queue mutex poisoned")
            .follow_up
            .push(content))
    }

    /// Remove queued steering messages without changing conversation history.
    pub fn clear_steering_queue(&self) {
        self.inner
            .queues
            .lock()
            .expect("agent queue mutex poisoned")
            .steering
            .clear();
    }

    /// Remove queued follow-up messages without changing conversation history.
    pub fn clear_follow_up_queue(&self) {
        self.inner
            .queues
            .lock()
            .expect("agent queue mutex poisoned")
            .follow_up
            .clear();
    }

    /// Remove all queued messages without changing conversation history.
    pub fn clear_all_queues(&self) {
        self.inner
            .queues
            .lock()
            .expect("agent queue mutex poisoned")
            .clear();
    }

    /// Whether either explicit queue contains input waiting for an eligible drain point.
    pub fn has_queued_messages(&self) -> bool {
        let queues = self
            .inner
            .queues
            .lock()
            .expect("agent queue mutex poisoned");
        !queues.steering.is_empty() || !queues.follow_up.is_empty()
    }

    /// Change the steering drain mode for subsequent eligible turn boundaries.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        *self
            .inner
            .steering_mode
            .lock()
            .expect("agent steering mode mutex poisoned") = mode;
    }

    /// Return the currently configured steering drain mode.
    pub fn steering_mode(&self) -> QueueMode {
        *self
            .inner
            .steering_mode
            .lock()
            .expect("agent steering mode mutex poisoned")
    }

    /// Change the follow-up drain mode for subsequent idle boundaries.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        *self
            .inner
            .follow_up_mode
            .lock()
            .expect("agent follow-up mode mutex poisoned") = mode;
    }

    /// Return the currently configured follow-up drain mode.
    pub fn follow_up_mode(&self) -> QueueMode {
        *self
            .inner
            .follow_up_mode
            .lock()
            .expect("agent follow-up mode mutex poisoned")
    }

    /// Clear transcript, transient state, last error, and explicit queues while idle.
    ///
    /// Reset never starts, cancels, or otherwise settles a run. Calling it while a run owns the
    /// agent is rejected so durable state cannot change beneath model/tool work.
    pub fn reset(&self) -> Result<(), CoreError> {
        let mut state = self.inner.state.lock().expect("agent state mutex poisoned");
        match state.phase {
            AgentPhase::Idle => {
                state.messages.clear();
                state.host_messages.clear();
                state.partial_response = None;
                state.is_streaming = false;
                state.pending_tool_calls.clear();
                state.last_error = None;
                state.accounting = crate::state::ModelAccountingSnapshot::default();
                drop(state);
                self.clear_all_queues();
                Ok(())
            }
            AgentPhase::Running(run_id) | AgentPhase::Cancelling(run_id) => {
                Err(CoreError::ActiveRun { run_id })
            }
        }
    }

    /// Request cancellation for the active run, if one exists.
    ///
    /// An already-driving run remains active until its cancellation-aware
    /// model/tool boundary emits terminal events and settles observers. A run
    /// that has not yet been driven has no such boundary, so it settles here.
    /// No active run is not an error.
    pub fn abort(&self) {
        let active = self
            .inner
            .active_run
            .lock()
            .expect("active run mutex poisoned")
            .clone();
        if let Some(active) = active {
            active.cancellation.cancel();
            let mut run_state = active.state.lock().expect("run state mutex poisoned");
            let settle_immediately = run_state.phase == crate::state::RunPhase::Created;
            if settle_immediately {
                run_state.phase = crate::state::RunPhase::Cancelled;
                run_state.stop_reason = Some(crate::state::StopReason::Cancelled);
            }
            drop(run_state);
            let mut state = self.inner.state.lock().expect("agent state mutex poisoned");
            state.partial_response = None;
            state.is_streaming = false;
            state.pending_tool_calls.clear();
            if settle_immediately {
                state.phase = AgentPhase::Idle;
            } else if !matches!(state.phase, AgentPhase::Idle) {
                state.phase = AgentPhase::Cancelling(active.id);
            }
            drop(state);
            self.inner
                .queues
                .lock()
                .expect("queue mutex poisoned")
                .clear();
            if settle_immediately {
                let mut active_slot = self
                    .inner
                    .active_run
                    .lock()
                    .expect("active run mutex poisoned");
                if active_slot
                    .as_ref()
                    .is_some_and(|current| current.id == active.id)
                {
                    active_slot.take();
                }
                drop(active_slot);
                self.inner.idle_notifier.notify();
            }
        }
    }

    fn is_idle(&self) -> bool {
        matches!(
            self.inner
                .state
                .lock()
                .expect("agent state mutex poisoned")
                .phase,
            AgentPhase::Idle
        )
    }
}

/// Configuration builder for an [`Agent`].
#[derive(Default)]
pub struct AgentBuilder {
    system_prompt: String,
    model: Option<ModelDescriptor>,
    thinking_level: ThinkingLevel,
    host_messages: Vec<crate::state::SerializedJson>,
    tools: ToolRegistry,
    provider: Option<Arc<dyn ModelProvider>>,
    compactor: Option<Arc<dyn crate::compaction::Compactor>>,
    hooks: Option<Arc<dyn HookSet>>,
    observers: Vec<Arc<dyn EventObserver>>,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
}

impl std::fmt::Debug for AgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBuilder")
            .field("system_prompt", &self.system_prompt)
            .field("model", &self.model)
            .field("thinking_level", &self.thinking_level)
            .field("tools", &self.tools)
            .finish()
    }
}

impl AgentBuilder {
    /// Set system instructions.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Set provider-independent model identity.
    pub fn model(mut self, model: ModelDescriptor) -> Self {
        self.model = Some(model);
        self
    }

    /// Set reasoning level.
    pub fn thinking_level(mut self, level: ThinkingLevel) -> Self {
        self.thinking_level = level;
        self
    }

    /// Add one explicit host-only context value.
    ///
    /// Host messages are not ambient configuration and are not converted to a
    /// provider request unless the configured context hook chooses to do so.
    pub fn host_message(mut self, message: crate::state::SerializedJson) -> Self {
        self.host_messages.push(message);
        self
    }

    /// Replace the complete executable tool registry.
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Add one executable tool while preserving insertion order.
    pub fn tool(mut self, tool: Arc<dyn AgentTool>) -> Self {
        self.tools.insert(tool);
        self
    }

    /// Remove one named executable capability before building the agent.
    ///
    /// This makes profile composition explicit: callers may start with the
    /// batteries-included set and deliberately omit a capability without
    /// changing its prompt or scheduler implementation behind the scenes.
    pub fn remove_tool(mut self, name: &str) -> Self {
        self.tools.remove(name);
        self
    }

    /// Attach a caller-owned model provider.
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Attach a caller-owned manual compactor.
    pub fn compactor(mut self, compactor: Arc<dyn crate::compaction::Compactor>) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// Attach host policy hooks.
    pub fn hooks(mut self, hooks: Arc<dyn HookSet>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Add an awaited lifecycle observer in registration order.
    pub fn observer(mut self, observer: Arc<dyn EventObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Select how steering messages are drained at eligible turn boundaries.
    pub fn steering_mode(mut self, mode: QueueMode) -> Self {
        self.steering_mode = mode;
        self
    }

    /// Select how follow-up messages are drained at the idle boundary.
    pub fn follow_up_mode(mut self, mode: QueueMode) -> Self {
        self.follow_up_mode = mode;
        self
    }

    /// Apply a captured profile's prompt.  Executable tools still come from the caller.
    pub fn profile(mut self, profile: &PiDefaultCodingProfile) -> Self {
        self.system_prompt = profile.system_prompt();
        self
    }

    /// Apply the pinned Pi coding prompt and its explicit executable default tools.
    ///
    /// `tools` is constructed by the embedding with an explicit workspace and
    /// capability adapter. This convenience method never discovers a cwd,
    /// home directory, Pi installation, or authority on the caller's behalf.
    /// Subsequent [`Self::tool`] calls replace/extend individual tools and
    /// [`Self::remove_tool`] deliberately removes them.
    pub fn pinned_default_coding_profile(
        mut self,
        tools: DefaultCodingTools,
    ) -> Result<Self, crate::error::ProfileError> {
        let profile = PiDefaultCodingProfile::pinned_default()?;
        let registry = tools.registry();
        profile.validate_registry(&registry)?;
        self.system_prompt = profile.system_prompt_for_workspace(tools.workspace().as_path());
        self.tools = registry;
        Ok(self)
    }

    /// Build an owned agent.
    pub fn build(self) -> Agent {
        let next_observer_id = self.observers.len() as u64;
        let mut state = AgentState::default();
        state.system_prompt = self.system_prompt;
        state.model = self.model;
        state.thinking_level = self.thinking_level;
        state.host_messages = self.host_messages;
        Agent {
            inner: Arc::new(AgentInner {
                state: Mutex::new(state),
                queues: Mutex::new(AgentQueues::default()),
                active_run: Mutex::new(None),
                tools: self.tools,
                steering_mode: Mutex::new(self.steering_mode),
                follow_up_mode: Mutex::new(self.follow_up_mode),
                provider: RwLock::new(self.provider),
                compactor: RwLock::new(self.compactor),
                hooks: self.hooks.unwrap_or_else(|| Arc::new(NoHooks)),
                observers: Mutex::new(
                    self.observers
                        .into_iter()
                        .enumerate()
                        .map(|(index, observer)| ObserverRegistration {
                            id: (index as u64).saturating_add(1),
                            observer,
                        })
                        .collect(),
                ),
                next_observer_id: AtomicU64::new(next_observer_id),
                subscribers: Mutex::new(Vec::new()),
                next_subscriber_id: AtomicU64::new(0),
                lossless_subscribers: Mutex::new(Vec::new()),
                next_lossless_subscriber_id: AtomicU64::new(0),
                next_run_id: AtomicU64::new(0),
                idle_notifier: IdleNotifier::default(),
            }),
        }
    }
}
