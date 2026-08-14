# Quickstart

This guide runs a headless Rust agent with a caller-owned model provider and
Smol executor. The core never discovers a provider, workspace, or credential
for you.

## Build the repository

The checked-in toolchain is required; do not substitute stable Rust.

```bash
git clone <repository-url> pi-agent-core-rs
cd pi-agent-core-rs
cargo +nightly-2026-07-24 test --workspace
```

For an application in the same checkout, depend on the core and choose the
executor yourself:

```toml
[dependencies]
pi-agent-core = { path = "../pi-agent-core-rs/crates/pi-agent-core" }
smol = "=2.0.2"
```

`smol` belongs to the application here, not to `pi-agent-core`. Tokio is not a
supported runtime dependency in this project.

## Run one deterministic agent

This complete example uses the finite `ModelStream` test adapter. A production
provider implements the same `ModelProvider` port and returns an incremental
`ModelEventStream` instead.

```rust
use pi_agent_core::scheduler::{
    CancellationToken, ModelEventStream, ModelFuture, ModelProvider, ModelStream,
    ModelStreamEvent,
};
use pi_agent_core::state::{ModelDescriptor, StopReason};
use pi_agent_core::Agent;
use std::sync::Arc;

struct DemoProvider;

impl ModelProvider for DemoProvider {
    fn stream<'a>(
        &'a self,
        _request: pi_agent_core::scheduler::ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("Hello from the model.".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        };
        Box::pin(std::future::ready(Ok(
            Box::new(stream) as Box<dyn ModelEventStream>
        )))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = Agent::builder()
        .system_prompt("Be concise.")
        .model(ModelDescriptor {
            provider: "example".into(),
            model: "demo".into(),
            revision: None,
        })
        .model_provider(Arc::new(DemoProvider))
        .build();

    smol::block_on(agent.start_prompt("Say hello.")?.drive())?;
    println!("{:#?}", agent.snapshot().messages);
    Ok(())
}
```

`start_prompt` reserves the one active run. Drive its returned `RunHandle` on
your executor. The same agent may be reused only after the run has settled.
Call `agent.abort()` from the host to request structured cancellation, then
await the run or `agent.wait_for_idle()`.

## Add the pinned coding profile

The default profile is optional. When selected, provide an existing workspace
explicitly; it never infers a working directory or reads Pi configuration.

```rust,no_run
use pi_agent_core::{Agent, DefaultCodingTools};

let tools = DefaultCodingTools::new("/absolute/workspace")?;
let agent = Agent::builder()
    // Also configure .model_provider(...) before running.
    .pinned_default_coding_profile(tools)?
    .build();
# let _ = agent;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The active pinned tools are `read`, `bash`, `edit`, and `write`. The complete
captured set also contains `grep`, `find`, and `ls`, and every operation is
replaceable through `DefaultCodingTools::with_operations`. See the
[default-profile guide](default-coding-profile.md) before granting a real
filesystem or process capability.

## Connect a real model and world

Implement `ModelProvider::stream` outside the core. Return a stream source as
soon as transport setup succeeds, emit text/tool-call deltas incrementally,
and race any I/O with the supplied `CancellationToken`. Implement tools as
`AgentTool` values with narrow schemas and explicit authority. Do not put
provider credentials in the core, a system prompt, a tool environment, or a
Luau policy.

For the full request, tool, queue, hook, and terminal contracts, read
[runtime semantics](semantics.md). For an optional capability-scoped Luau
policy, start with [Writing Luau extensions](luau-extensions.md); a scripting
VM is not required for ordinary Rust agents.
