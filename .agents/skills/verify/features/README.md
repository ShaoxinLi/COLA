# COLA verification map

This is the maintained map of behavior visible to Rust callers of COLA.
Start with [the driver](../SKILL.md). Use these recipes after its launch and
Doctor steps. Run a complete group to claim coverage of that feature.

## Baseline preconditions

- Build the current checkout into the run's empty `CARGO_TARGET_DIR`.
- Keep evidence outside `$COLA_VERIFY_STATE`.
- Use the executable paths produced by launch, then pass Doctor.
- Let each test create its own sessions and temporary data. No manual seed is needed.
- Allow loopback listeners for model journeys. Live credentials are unnecessary.
- Use Bash and Unix utilities for host tool journeys. Unix-only cases are conditional.

## Driving conventions

Use each feature's exact runner command inside the driver's evidence capture
block. Set `COLA_VERIFY_FEATURE` to that map's feature ID. Preserve a nonzero
exit status and reject zero matched tests. Record narrower selections as partial
coverage. Inspect the saved source for the side-effect assertions being claimed.

## Proof and skip reporting

Retain the action trace, test names, results, source, hashes, and exit status.
The library integration tests are its Rust consumer journeys. The `cola`
executable also has CLI and Web process journeys in `tests/main.rs`. Report fixture-based
orchestration and local HTTP compatibility with those limits. A failed
precondition is a failure to verify the feature, not permission to skip it.

## Features

- [Agent chats](agent-chats.md) covers service creation, per-call review, durable invocation audit, limits, and failures.
- [Session persistence](session-persistence.md) covers JSONL documents, locks, inspection, recovery, and refused writes.
- [Host tools](host-tools.md) covers file reads and writes, Bash, validation, and path anchoring.
- [Provider adapters](provider-adapters.md) covers OpenAI and Anthropic HTTP requests, responses, frozen native views, and errors.
- [Conversation contracts](conversation-contracts.md) covers public values, session transitions, recovery, context, and caller-defined roles.
