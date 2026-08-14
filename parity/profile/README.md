# Default profile evidence

default-profile.json is immutable captured profile data: the prompt bytes, active tool order,
definitions, schemas, and profile metadata used by PiDefaultCodingProfile. Runtime code reads
this checked-in capture and does not load an external SDK, source checkout, executable, or
ambient configuration.

Rust tests cover:

* exact default prompt bytes and workspace substitution;
* active tool order and definition/schema data;
* successful, invalid-input, and host-operation failures for all standard tools;
* replacement/removal of tools and sterile profiles;
* explicit workspace isolation and operation-adapter boundaries.

The capture manifests retain historical provenance where useful for review, but they are not
runtime inputs or update gates. A deliberate profile change updates the capture, its hashes, the
Rust profile tests, and the relevant fixture evidence together.

The concrete executable factory evidence is in
crates/pi-agent-core/tests/default_tools_behavior.rs. The complete deterministic kernel corpus
is checked with:

    ./parity/run-rust.sh

No profile check may consult the repository cwd, credentials, sessions, a live provider, or a
host-installed Pi executable.
