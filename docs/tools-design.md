# Basic tool adapters

## Problem

COLA records tool calls but has no host executors. The existing synchronous
`Tool` trait owns execution; `ToolDefinition` describes model-visible inputs.
`Session` owns call ordering and recorded outcomes. Adapters must validate their
arguments and translate ordinary failures to `ToolOutcome::Failure`.

## Usage

```rust
use cola::adapters::tools::{BashTool, ReadFileTool, WriteFileTool};
use cola::domain::{Tool, ToolResult};
use serde_json::json;

let bash = BashTool::new("/work/project")?;
let definitions = vec![BashTool::definition(), ReadFileTool::definition()];
let output = bash.execute(json!({"command": "printf hello"}).as_object().unwrap());

let writer = WriteFileTool::new("/work/project")?;
let saved = writer.execute(json!({"path": "notes.txt", "content": "hello\n"})
    .as_object().unwrap());
let reader = ReadFileTool::new("/work/project")?;
let text = reader.execute(json!({"path": "notes.txt"}).as_object().unwrap());

// After recording a matching read_file request, the caller owns dispatch.
let call = session.start_tool(turn, &call_id)?;
let result = ToolResult::new(call.id().clone(), reader.execute(call.arguments()));
session.record_tool_result(turn, result)?;
```

These fragments show separate direct and session call sites. The real
`basic_tools` example constructs and runs the tools without a model.

## Shape

The implementation follows this sketch. Bodies belong to the adapter modules.

```rust
struct WorkingDirectory(PathBuf);
struct Captured { bytes: Vec<u8>, truncated: bool }

pub struct BashTool { cwd: WorkingDirectory }
pub struct ReadFileTool { cwd: WorkingDirectory }
pub struct WriteFileTool { cwd: WorkingDirectory }

// Each concrete type has these methods and implements the existing Tool trait.
impl BashTool {
    pub fn new(cwd: impl AsRef<Path>) -> io::Result<Self> { todo!() }
    pub fn definition() -> ToolDefinition { todo!() }
}
impl Tool for BashTool {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome { todo!() }
}
```

`src/adapters/tools/mod.rs` re-exports only the three concrete types. Private
`bash`, `files`, and shared-support code own argument structs, schemas, path
resolution, process capture, and error conversion. There is no new registry,
execution trait, or application loop.

Per Model the Domain, private serde argument structs encode each operation's
input shape. Per Boundary Discipline, parsing happens once at `execute` and
rejects missing, mistyped, and unknown fields before effects. Required strings
`command` and `path` reject empty values. Whitespace stays literal. Empty
`content` is valid. Descriptors use matching required fields, `minLength: 1`,
and `additionalProperties: false`. `InputSchema` remains descriptive.

Constructors canonicalize an existing directory and store it per instance.
Relative file paths resolve there; absolute paths, `..`, and symlinks use host
semantics. The tools never change process cwd. This is not confinement.

Bash starts `bash --noprofile --norc -c command` from PATH with null stdin,
inherited environment, and its instance cwd. Each call uses a fresh process.
Two scoped readers drain both streams concurrently and retain the first 64 KiB
of each. A truncated stream has an explicit marker. Bytes decode lossily.
Success returns stdout verbatim when stderr is empty, including empty success.
Otherwise both streams have `[stdout]` and `[stderr]` labels. Failure reports exit
status or signal and the partial streams. Startup, reader, and wait failures
return contextual nonblank errors. On capture setup or wait failure, terminate
and reap the direct child where possible, and join all started readers.

`read_file` returns exact UTF-8 text, including empty content. It rejects
non-regular paths before opening, then reads at most 1 MiB plus one byte and
rejects oversized input without returning partial text. Invalid UTF-8 fails.
The metadata precheck avoids ordinary FIFO opens but cannot prevent replacement
races or guarantee nonblocking filesystem operations.

`write_file` creates or truncates a file and writes exact UTF-8 bytes. It returns
empty success, creates no parent directories, and follows host symlink behavior.
It promises neither atomic replacement nor durability. A failure can leave
partial content. No retry or exactly-once guarantee is added.

The interface hides OS effects and validation behind construction, a static
definition, and the existing execution method. Per Laziness Protocol, callers
retain dispatch rather than learning a second built-in selector API.

## Synthesis decision

Four independent candidates used GPT-6.1 Sol, GPT-6 Astra, Claude Opus 5.5, and
Claude Sonnet 5.5. All chose three independent concrete tools. A separate closed
enum sketch tested centralized ownership. Cross-judge Claude Sonnet 5.5 chose
Sol's candidate, scoring it 23/25 against 22/25 for Astra, 13/25 and 15/25 for
the timeout variants, and 18/25 for the enum. Parent scoring agreed on the base.

Keep Sol's canonical cwd, associated definitions, bounded capture, exact file
content, and small API. Graft Astra's explicit capture-error cleanup and the
other candidates' useful I/O diagnostics. Retain the empty write success and
simple truncation flag. Reject new timeout configuration, shelling out to kill
process groups, detached reader threads, and automatic parent creation.

## Tradeoffs accepted

- Accept blocking execution in exchange for deferring process-tree lifecycle
  machinery. A command, background descendant holding a pipe, or filesystem
  operation can block indefinitely. There is no wall-clock deadline.
- Accept host privileges and inherited environment in exchange for basic local
  integration. The cwd is not a sandbox or an approval policy.
- Accept prefix-only truncation and lossy shell text in exchange for bounded
  retained output. Late diagnostics can be omitted. File text stays exact.
- Accept non-atomic writes and caller-owned dispatch in exchange for a small
  implementation aligned with existing domain contracts.

## Alternatives considered

Independent types localize capability ownership. A closed `BasicToolKind` enum
plus `BasicTool` also implements `Tool`, but centralizes every future addition
in a selector and multiple matches. A collection adds name lookup the caller
already owns. A workspace factory merely forwards cwd. None hides more OS
complexity than three independent adapters.

## Prototype resolutions

Throwaway Rust probes under the local `.scratch/basic-tools` directory exercised
real Bash arrays, null-stdin EOF, separate streams, code 7 with partial output,
invalid output bytes, literal filenames, and empty writes. Raw concatenation
produced `hellowarning`; labeled streams preserved the boundary. A second probe
drained 200000 bytes per stream while retaining 65536 and preserving code 7.
A supervised Python probe observed Bash exit while a descendant still held its
output pipe. This rejects a direct-child-only timeout as a call deadline. It
does not rule out a later complete cancellation design.

No product decision remains open for this scope. Runtime limits beyond retained
output and file read size are deferred. Integration tests must repeat the
behavior against the production adapters, since prototype success alone does
not verify the implementation. Per Prove It Works, the real example must also
run successfully before completion.

## Implementation reconciliation

The parent accepted the synthesized contracts before delegation. The regular
file metadata precheck incorporates the cross-judge's FIFO objection without
promising race-free or nonblocking I/O. Other changes to outcomes, signatures,
or responsibility assignments require reconciliation here before completion.

The implementation preserves the sketch's signatures and outcomes. Independent
code review found no actionable issue. Final tests also cover truncation inside
a UTF-8 character and removed working directories for all three tools. Ordinary
spawn failure is exercised through a removed cwd, which uses the same failure
boundary as a missing Bash executable. A command factory solely for that test
was declined. Resource-exhaustion failures when creating readers or waiting for
the child are handled but not fault-injected. No extra runtime hooks were added
for those tests. Oversized reads report their limit; a suggested Bash command is
left to the caller. These are parent-accepted verification choices, not changes
to the execution contract.

## Next implementation step

Implement file adapters and real-file tests, then Bash capture and process tests.
