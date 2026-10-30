# Agent service

[`AgentService`](../src/app/agent_service.rs) runs reviewed tool workflows from a
committed [`SessionRecord`](../src/app/session_record.rs). A new service or
process can continue any closed session using the same store and explicit
configuration. There is no resident session cache or continuation token. See the
[session record guide](session-record.md) for aggregate ownership and audit invariants.

## Operations and extension points

`AgentService::new` receives a client implementing both
`ModelClient<AssistantMessage>` and `ModelClient<Decision>`, a `SessionStore`,
`Assistant`, `ToolCallReviewer`, registered tools, and `AgentSettings`.
Tool names are unique. Advertised definitions must match the registered catalog.

| Operation | Result |
| --- | --- |
| `new_session()` | Publishes an empty record and returns its domain session. |
| `chat(id, user)` | Runs one turn and returns its messages, including the user message. |
| `list_session()` | Reads current stored sessions in ID order without recovery writes. |
| `inspect_session(id)` | Reads the full record, including model audit and acknowledgements. |
| `resolve_effect(address, resolution)` | Records evidence for the exact closed execution. |
| `acknowledge_review_failure(id, invocation, evidence, actor)` | Records an explicit acknowledgement and releases that review block. |

`Assistant` and `ToolCallReviewer` remain caller-defined context hooks. They may
select historical exchanges but cannot alter their facts. Assistant context must
preserve the exact current exchange. The application supplies the reviewer with
mandatory current-turn facts, the exact candidate, and its complete address as a
typed `ReviewSubject`. Adapters render this subject as labelled user-role data,
separate from policy instructions.
Model audit metadata never enters these semantic facts.

## Commit and I/O ordering

A write command holds one OS session lock through its synchronous workflow.
Each command reads the latest record under that lock.

1. Commit the user message and a planned invocation.
2. The model adapter compiles the request. The application recorder commits its
   exact nonsecret bytes and the initial frozen view before permitting HTTP.
   The returned `SendPermit` owns those bytes and is consumed by transport.
3. Commit the response evidence and accepted assistant step together. Rejected
   output is audited without creating an executable step.
4. Record tool dispatch intent, plan and prepare its review, then commit the
   review decision before executing the target.
5. Commit each tool result before another model request.

A denial becomes an ordinary tool failure result. It does not block siblings or
fail the turn. A failed tool also produces an ordinary result. `max_iterations`
counts assistant invocations, and an accepted batch finishes before the next
iteration check.

Any commit error ends the command. Its writer becomes unusable, including when
rename succeeded but directory synchronization failed. `progress()` contains
the last confirmed committed working copy. Disk may already contain a newer
revision after an uncertain publication.

## Recovery and review failures

Inspection preserves active facts. The next writer explicitly commits recovery,
closing an abandoned active turn as `Interrupted(ProcessLost)`. It never resumes
that turn or retries its tools. Planned invocations become not sent. Prepared
invocations without a committed response become response unknown. A completed
reviewer failure remains active until acknowledgement closes it with its known
cause. Repeated chat and effect-resolution attempts preserve that cause.

Review refusal, incompleteness, malformed output, transport failure, preparation
failure, and unknown prepared responses block later chats. The initial failure
retains its typed cause and invocation ID. Later attempts return `ReviewBlocked`.
A planned-only reviewer interrupted before preparation does not create a block.

Acknowledgement requires nonblank evidence. Identical invocation, evidence, and
actor retries are idempotent. Conflicting or nonpending acknowledgements fail.
If the failed turn is still active, acknowledgement closes it with the original
failure. An existing `ProcessLost` outcome stays unchanged. A pending invocation
is recovered before acknowledgement. CLI and unauthenticated Web callers use
`actor: None`.

Acknowledgement performs no tool or model I/O. It does not fabricate a result,
change `Started` to `NotStarted`, or resolve an unknown effect. Effect resolution
is a separate operation addressed by session, turn, step, and native ID.

## Context policy and limits

The [context policy guide](context-policy.md) describes selection and fitting.
`with_context_policy` replaces the default `RecentContextPolicy`. The default
selects eight optional closed turns plus every turn with an unresolved effect.
Mandatory turns do not consume the optional quota. A custom policy cannot omit
unresolved-effect turns. Fitting drops the oldest optional whole turns until the
complete encoded request fits. Required content that cannot fit produces
`ContextSelectionTooLarge` before HTTP and closes the assistant turn.

CLI and Web configuration expose `context.optional_turns` and
`context.max_request_bytes`. The default request cap is 64 KiB. It includes
system instructions, tools,
settings, semantic history, current input, and native dependencies. The same cap
protects every later request in a growing tool chain. Reviewer requests fit
independently and never prune the assistant's frozen view.

Closed turns are quoted semantic history with role and outcome labels. New turns
can change provider or model. Within a turn, selected history, current user,
system, tools, and settings are frozen. Later requests append accepted native
responses and actual tool results from the record. Prefix or model changes
fail before HTTP.

The byte cap is not a model-specific token-window estimate. The application does
not infer context capacities for arbitrary model IDs. Callers must select a
request cap suitable for their configured models. The implementation provides no
universal guarantee against upstream truncation under an incorrectly sized cap.
There is no streaming, automatic retry, active-turn resumption, or cancellation.
