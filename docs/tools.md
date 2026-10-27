# Tool adapters

`cola::adapters::tools` exports synchronous host tools that implement
[`Tool`](domain.md#tool-contracts). Each exposes `definition()` for its catalog
entry and `new(cwd: impl AsRef<Path>) -> io::Result<Self>` for construction.

| Adapter | Tool name | Required string fields | Success output |
| --- | --- | --- | --- |
| `BashTool` | `bash` | `command` | Captured process output. |
| `ReadFileTool` | `read_file` | `path` | Exact UTF-8 file content. |
| `WriteFileTool` | `write_file` | `path`, `content` | An empty string. |

## Shared behavior

Constructors canonicalize an existing directory once. Relative paths and Bash
execution use that directory without changing the process working directory.
Absolute paths, `..`, and symlinks follow host behavior. The directory does not
confine access. Tools use the host user's privileges.

Calls reject missing fields, wrong types, unknown fields, and empty `command`
or `path` strings before I/O. Whitespace remains literal. Empty `content` is
valid. Definitions describe the same argument shapes. Validation and ordinary
I/O failures return a nonblank `ToolOutcome::Failure`.

## Bash execution

[`BashTool`](../src/adapters/tools/bash.rs) starts a fresh
`bash --noprofile --norc -c command` from PATH, with inherited environment and null
stdin. Bash must be available on PATH. Shell state does not persist between calls.

Two readers drain stdout and stderr concurrently. Each retains its first 64 KiB
and discards later bytes while continuing to drain. Truncated streams end with
`[output truncated after 65536 bytes]`. Invalid UTF-8 becomes replacement characters.

| Process result | Tool outcome |
| --- | --- |
| Success with empty stderr | Stdout verbatim, including empty output. |
| Success with nonempty stderr | `[stdout]` and `[stderr]` sections. |
| Nonzero exit or signal | Failure with the status and both captured streams. |

Output is text for people and models. Reader startup and wait failures trigger
direct-child termination and reaping where possible. Started readers are joined
before return.

Execution has no wall-clock limit. A command or descendant holding an output
pipe can block indefinitely, even after Bash exits. Capture limits bound
retained output, not runtime, output production, or process trees.

## File reads

[`ReadFileTool`](../src/adapters/tools/files.rs) returns exact UTF-8 text,
including whitespace and empty content. It rejects non-regular paths, invalid
UTF-8, and files larger than 1 MiB without returning partial text.

Metadata checks reject ordinary FIFOs and other non-regular paths before open.
Reads consume at most 1 MiB plus one byte. Replacement races between metadata
and open remain possible, so the check does not guarantee nonblocking access.

## File writes

[`WriteFileTool`](../src/adapters/tools/files.rs) creates or truncates a file
and writes exact UTF-8 bytes. Parent directories must exist. Empty content is
valid. Writes follow host symlink and permission behavior. They are non-atomic,
with no durability guarantee or retries. Failure can leave partial content.

`AgentService` supplies [lookup, review, execution, and result recording](agent-service.md).
Tools provide no sandbox or approval policy. Tests execute real local processes
and file operations.
