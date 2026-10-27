# Tool adapters

`cola::adapters::tools` exports three host tools. Each implements the synchronous
[`Tool` contract](domain.md#tool-contracts) and exposes a static
`definition() -> ToolDefinition` method.

## Implementations

| Adapter | Tool name | Required string fields | Success output | Source |
| --- | --- | --- | --- | --- |
| `BashTool` | `bash` | `command` | Captured process output. | [`bash.rs`](../src/adapters/tools/bash.rs) |
| `ReadFileTool` | `read_file` | `path` | Exact UTF-8 file content. | [`files.rs`](../src/adapters/tools/files.rs) |
| `WriteFileTool` | `write_file` | `path`, `content` | An empty string. | [`files.rs`](../src/adapters/tools/files.rs) |

All constructors have the signature `new(cwd: impl AsRef<Path>) -> io::Result<Self>`.
They canonicalize an existing directory once. Relative paths and Bash execution
use that instance's directory without changing the process working directory.
Absolute paths, `..`, and symlinks follow host behavior. The directory does not
confine access. Tools use the host user's privileges.

Every call rejects missing fields, wrong types, unknown fields, and empty
`command` or `path` strings before I/O. Whitespace remains literal. Empty
`content` is valid. Definitions describe the same shapes with required fields,
`minLength: 1` for commands and paths, and `additionalProperties: false`.
Validation and ordinary I/O failures return a nonblank `ToolOutcome::Failure`.

## Bash execution

`BashTool` accepts `{"command": "..."}`. It starts a fresh
`bash --noprofile --norc -c command` from PATH, with inherited environment and null
stdin. Bash must be installed and available on PATH. Shell state does not carry
between calls.

Two readers drain stdout and stderr concurrently. Each retains its first 64 KiB
and discards subsequent bytes while continuing to drain. Truncated streams end
with `[output truncated after 65536 bytes]`. Invalid UTF-8 becomes replacement
characters.

| Process result | Tool outcome |
| --- | --- |
| Success with empty stderr | Stdout verbatim, including whitespace and empty output. |
| Success with nonempty stderr | `[stdout]` and `[stderr]` sections. |
| Nonzero exit or signal | A failure containing the status and both captured streams. |

Output text is intended for people and models, not machine parsing. Reader
startup and wait failures trigger direct-child termination and reaping where
possible. All started readers are joined before the call returns.

Execution has no wall-clock limit. A command or descendant holding an output pipe
can block the call indefinitely, even after Bash exits. Capture limits bound
retained output, not process runtime, output production, or process trees.

## File reads

`ReadFileTool` accepts `{"path": "..."}`. Success returns exact UTF-8 text,
including whitespace and empty content. Reads reject non-regular paths, invalid
UTF-8, and files larger than 1 MiB without returning partial text.

A metadata precheck rejects ordinary FIFOs and other non-regular paths before
open. Reads consume at most 1 MiB plus one byte to enforce the limit. A path can
change between metadata and open. The precheck does not prevent replacement
races or guarantee nonblocking filesystem access.

## File writes

`WriteFileTool` accepts `{"path": "...", "content": "..."}`. It creates or
truncates the file and writes exact UTF-8 bytes. Parent directories must already
exist. Empty content is valid. Success returns an empty string.

Writes follow host symlink and permission behavior. They are non-atomic and
provide no durability guarantee. A failure can leave a created file or partial
content. The adapter adds no retries.

## Caller ownership

The caller selects the adapter and records its outcome with the original call
ID. `Session` owns call ordering and recorded facts.
[AgentService](agent-service.md) supplies tool lookup, per-call review, execution,
and result recording. Tool adapters provide no sandbox or approval policy.
Tests execute real local processes and file operations.
