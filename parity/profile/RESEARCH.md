# Pinned default-profile fixture

[`default-profile.json`](default-profile.json) is generated directly from the Pi SDK source at
the commit in [`../UPSTREAM_COMMIT`](../UPSTREAM_COMMIT); it is not a capture from the locally
installed `pi` CLI and must not be hand-edited. It records the generated default system prompt,
the active `read`, `bash`, `edit`, and `write` definitions, and all seven standard definitions.

From `parity/upstream/source`, regenerate and validate it with:

```text
npm ci --ignore-scripts
./node_modules/.bin/tsx ../profile-runner.mts > ../../profile/default-profile.json
jq empty ../../profile/default-profile.json
./node_modules/.bin/tsx ../../profile/verify-profile.mts
```

`profile-runner.mts` imports `buildSystemPrompt`, `createCodingToolDefinitions`, and
`createAllToolDefinitions` in-process. It does not execute the Pi CLI, create tool instances, or
access a model provider.

The prompt builder computes three documentation paths from its own installation. The runner
replaces only those known values with `/fixture/pi/README.md`, `/fixture/pi/docs`, and
`/fixture/pi/examples`; it uses the fixed workspace root `/fixture/workspace`. This keeps all
prompt text and tool-local guidance exact while removing installation-directory variability and
avoiding ambient resource discovery in the Rust profile. Schema JSON is retained in source
serialization order and hashed separately.

The fixture's `upstream` object, active-tool order, serialized schema hashes, and system-prompt
hash are its quick integrity checks. The more complete behavioral contract remains in
[`../../docs/default-coding-profile.md`](../../docs/default-coding-profile.md).

`verify-profile.mts` imports the same pinned factories and prompt builder and compares their
serialized output to the checked-in capture. It also verifies the source hashes in
[`source-manifest.json`](source-manifest.json) and the required success/invalid-input/host-error
case coverage in [`behavior-manifest.json`](behavior-manifest.json). This is an SDK-source
parity check only: it does not start the Pi CLI, instantiate an agent, execute a tool, or contact
a provider.
