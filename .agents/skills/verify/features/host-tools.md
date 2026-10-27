# Host tools

Rust callers register or directly execute Bash, file-read, and file-write tools.
The tools return `ToolOutcome` values and perform operations on the host.

## Sub-features

- `tools-files` preserves exact UTF-8 text, empty content, literal paths, and truncating writes.
- `tools-errors` rejects invalid arguments and reports ordinary I/O failures.
- `tools-paths` anchors relative paths once while respecting host absolute paths and symlinks.
- `tools-bash` runs fresh Bash processes with null stdin and captured output.
- `tools-output` labels stderr, reports nonzero exits and signals, and bounds retained streams.

## How to get to it (user POV)

Construct `BashTool::new(cwd)`, `ReadFileTool::new(cwd)`, or
`WriteFileTool::new(cwd)` from `cola::adapters::tools`. Obtain the catalog entry
with `definition()`. Call `cola::domain::Tool::execute` with its JSON object.
Service users register the tool and matching definition with `AgentService`.

## Driving it with the Rust test runner

Preconditions:

- Launch and Doctor pass. Set `COLA_VERIFY_FEATURE=host-tools`.
- Bash and `tr` are on PATH. Unix FIFO coverage requires `mkfifo`.
- Allow disposable files, symlinks, and subprocesses. No user files are needed.

Run `"$COLA_VERIFY_ADAPTERS" tools:: --nocapture --test-threads=1` in the capture block.
Cases live in the inline `tools` module in `tests/adapters.rs`,
`tests/adapters/tools/files.rs`, and `tests/adapters/tools/bash.rs`.

- Write and read through `files::files_preserve_exact_text_and_literal_paths`. Check returned text, raw file bytes, empty writes, and shorter replacements.
- Reject invalid calls through `invalid_arguments_fail_before_effects`. The directory stays empty and an existing file retains its original bytes.
- Drive missing, oversized, invalid UTF-8, directory, symlink, and FIFO paths through `files::`. Read errors return no partial success.
- Construct tools with relative and symlinked roots through the path-anchor cases. Files and subprocess output identify the chosen directory without changing the caller's current directory.
- Execute arrays, EOF reads, and sequential shell calls through `bash::bash_supports_arrays_eof_stdin_and_fresh_processes`. Outputs prove shell state is fresh.
- Emit stderr, exit nonzero, terminate by signal, and overflow both streams through the remaining `bash::` cases. Assertions inspect real process results and truncation markers.

## Gotchas

- The working directory is an anchor, not a sandbox. Tests intentionally use paths outside it inside a disposable parent.
- Bash has no wall-clock limit. An arbitrary command or descendant retaining pipes can hang the caller.
- Unix signal, symlink, and FIFO tests are conditional. Record their presence from Doctor before claiming those cases on another platform.
- These direct adapter journeys do not exercise service approval. Verify the agent chat map for that entry point.
