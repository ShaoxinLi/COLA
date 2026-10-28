# Durable sessions and selected model context

A session archive is the source of truth. A new process can build the next model
request from its committed bytes. The archive stores more information than a
model prompt needs. `ContextPlan` selects and renders facts without calling an
LLM to summarize them.

## Recorded data

`SessionArchive` contains the domain conversation and an ordered generation
ledger. Every assistant and reviewer invocation records its target, requested
model and settings, serialized request, history selection, budget measurement,
start time and observation. Observations retain response metadata, bounded body
capture and acceptance or rejection. A pending invocation remains distinguishable
from an observed failure. Interrupted and size-limited captures are distinct.

Accepted assistant steps reference their source generation. A tool occurrence
uses `CallRef { generation, call_id }`. `call_id` is the original opaque provider
string. For example, generation 7 and `native-read` identify one occurrence;
the provider result also uses `native-read`. There is no random replacement ID
or reverse mapping. A later generation can reuse that string after the earlier
turn closes. Duplicate IDs within one active native bundle are rejected.

The JSONL archive has no format-version identifier or compatibility dispatch.
Old archives are unsupported. A snapshot revision and SHA-256 digest identify
publications for concurrency control. They do not identify a storage format.

## Context construction

Closed turns become portable factual groups. A group includes the original user
request, assistant statements, tool names and arguments, actual results, denials,
unknown effects and closure reason. Model names, usage, audit timestamps and
native IDs are omitted from this historical prose.

For example, stored user text asking to read `config.txt`, tool arguments naming
that file, and an actual return containing `port=8080` become a fixed record of
those fields. A later question about the port receives that record. No model
extracts or invents the record, and the file need not still exist.

The active assistant turn keeps its entire compatible native response bundle
and ordered tool outcomes. Native reasoning items and signatures remain available
where the provider protocol needs them. The reviewer receives the exact proposed
call, current user request and execution facts, with no callable tools.

Preparation measures the actual serialized request, including instructions,
tools and protocol framing. It retains required turns and the newest contiguous
suffix of optional whole turns that fits. It preserves chronological order and
adds an omission notice. `chat_with_history` lets library callers require older
turns. It does not automatically discover lasting preferences or semantic
relevance. An oversized mandatory bundle or required turn fails before HTTP.

The executable reserves `max_output_tokens` from each role's configured context
allowance. Input tokens are estimated as UTF-8 bytes divided by three, rounded
up; request bytes are measured exactly. The estimate is recorded and makes no
claim to match every provider tokenizer. There are no hidden token-count requests
or automatic history-changing retries.

## Publication and external effects

An operation acquires a stable per-session OS lock and reloads the archive.
A competing cooperating writer receives a busy error. A proposed change validates
its expected base and immutable recorded facts, writes a same-directory temporary
file, syncs it, renames it and syncs the directory.

Only confirmed publication permits the next external effect. The service records
invocation intent before HTTP, reply evidence and acceptance together, review
before tool dispatch intent, and the actual tool return before further generation.
An uncertain publication is resolved against the exact candidate and resynchronized
under the lock. Unresolved uncertainty stops dispatch. A fresh acquisition also
establishes a synchronization barrier before using the current archive.

Errors expose the last durable snapshot separately from an uncommitted candidate.
Listings never overlay uncommitted memory onto disk facts. Custom stores implement
this same transaction contract using public archive codecs and commit accessors.

## Recovery

`inspect` and `list` preserve the recorded state. They do not silently close
active turns. CLI `/recover`, HTTP `POST /sessions/{id}/recover`, and the library
recovery operation explicitly abandon pending invocations and close the active
turn as process lost. Recovery is idempotent and performs no model or tool calls.

| Last durable fact | What recovery can claim |
| --- | --- |
| Invocation intent without observation | The response was not recorded. |
| Requested or approved tool without dispatch intent | The tool was not dispatched. |
| Started tool without returned outcome | The effect is unknown. |
| Returned outcome | The recorded result is available. |

A subsequent turn uses portable closed history. It never fabricates a successful
result or repeats an unknown effect. Model and provider changes are possible
between turns; an active native bundle is not translated across incompatible
targets or settings.

## Tradeoffs and verification

Complete snapshots retain the existing atomic publication approach at the cost
of rewriting growing archives. An append-only journal would need separate framing,
reducer and torn-tail repair contracts. Neither it nor automatic semantic history
selection is required to repair cold continuation.

Integration tests cover both provider protocols with local HTTP fixtures,
transaction failures, recovery and bounded context. `scripts/verify-deepseek.py`
checks real DeepSeek CLI and Web continuation across process restarts, including
reading random file contents, deleting the file and answering from saved history.
This live journey does not establish live Anthropic compatibility.
