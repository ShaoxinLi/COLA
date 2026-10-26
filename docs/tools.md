# Basic tool adapters

`cola::adapters::tools` exports `BashTool`, `ReadFileTool`, and `WriteFileTool`.
Each implements the synchronous `cola::domain::Tool` trait. Each has
`new(cwd: impl AsRef<Path>) -> io::Result<Self>` and a static
`definition() -> ToolDefinition` method.

Constructors canonicalize an existing directory once. Relative paths and Bash
execution use that instance's directory without changing the process cwd.
Absolute file paths, `..`, and symlinks follow host behavior. The directory does
not confine access. Tools use the host user's privileges.

Every call rejects missing fields, wrong types, unknown fields, and empty
`command` or `path` strings before I/O. Whitespace remains literal. Definitions
describe the same shape with required fields, `minLength: 1`, and
`additionalProperties: false`. `InputSchema` is a descriptor, not a validator.
Ordinary validation and I/O errors return a nonblank `ToolOutcome::Failure`.

## Design decisions

Each concrete adapter owns its argument shape, definition, and execution.
Independent types let callers construct only the capabilities they need and
combine them with other `Tool` implementations. A shared enum would centralize
future additions. A registry would duplicate the caller's selection responsibility.
Implementation modules stay private; only the three tool types are exported.

The instance directory is resolved at construction so later calls do not depend
on changes to the process cwd. Bash output decodes lossily because commands can
emit arbitrary bytes. File reads require valid UTF-8 so a read-and-write cycle
does not silently replace bytes.

Output capture limits retained memory while readers continue draining the pipes.
A prototype confirmed that Bash can exit while a descendant still holds an
output pipe. A timeout on Bash alone would therefore not bound the complete
call. Process-tree cancellation remains deferred until it can cover pipe readers
as well. File writes use ordinary create-and-truncate behavior to preserve host
symlink and permission semantics rather than introduce atomic replacement.

## BashTool

The `bash` definition accepts `{"command": "..."}`. Execution starts a fresh
`bash --noprofile --norc -c command` from PATH with inherited environment and
null stdin. Bash must be installed and available on PATH.

Two scoped readers drain stdout and stderr concurrently. Each retains its first
64 KiB and discards subsequent bytes while continuing to drain. A truncated
stream ends with `[output truncated after 65536 bytes]`. Bytes decode with
replacement characters for invalid UTF-8.

Success returns stdout verbatim when stderr is empty. Otherwise the output
contains `[stdout]` and `[stderr]` sections. Empty success is valid. Nonzero exit
or signal termination returns a failure containing the status and both captured
streams. Output text is intended for people and models, not machine parsing.
Reader startup and wait failures trigger direct-child termination and reaping
where possible. All started readers are joined before the call returns.

Execution has no wall-clock limit. A command or a descendant holding an output
pipe can keep a call blocked indefinitely, even after Bash exits. Retained output
is bounded; process runtime, output production, and process trees are not.

## ReadFileTool

The `read_file` definition accepts `{"path": "..."}`. Success returns exact UTF-8
text, including whitespace and empty content. Reads reject non-regular paths,
invalid UTF-8, and files larger than 1 MiB without returning partial text.

A metadata precheck rejects ordinary FIFOs and other non-regular paths before
open. Reads then consume at most 1 MiB plus one byte to enforce the limit. A path
can change between metadata and open, so the precheck does not prevent
replacement races or guarantee nonblocking filesystem access.

## WriteFileTool

The `write_file` definition accepts `{"path": "...", "content": "..."}`.
Execution creates or truncates the file and writes the exact UTF-8 bytes. Empty
content is valid. Success returns an empty string. Parent directories must
already exist.

Writes are non-atomic and provide no durability guarantee. A failure may leave a
created file or partial content. The adapter adds no retry or exactly-once
semantics.

## Caller-owned execution

The caller selects the adapter and records its outcome with the original call
ID. `Session` continues to own tool ordering and recorded facts. There is no
built-in registry, model loop, sandbox, or approval policy.

The [basic_tools example](../examples/basic_tools.rs) writes and reads a temporary
file, then runs Bash against it. It checks the actual outcomes and exits with an
error if they differ from the expected content.

```sh
cargo run --example basic_tools
```

The [domain example](../examples/read_file.rs) remains a separate in-memory
conversation with manually supplied output.
