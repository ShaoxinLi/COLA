# Session persistence

Rust callers save conversations, list them, and recover them through
`JsonlSessionStore` and the `SessionStore` trait.

## Sub-features

- `store-roundtrip` preserves every recorded fact and repeated-save bytes.
- `store-recover` closes active work as `ProcessLost` without modifying disk or repeating effects.
- `store-list` sorts opaque IDs and ignores unrelated files.
- `store-reject` refuses corrupt, future-version, or unreadable records without replacing bytes.
- `store-concurrent` exposes complete sessions during concurrent publication.
- `store-diagnostics` identifies the file and record for malformed JSON.

## How to get to it (user POV)

Construct `cola::adapters::stores::JsonlSessionStore::new(root)`. Call `save`,
`load`, or `list` through `cola::app::SessionStore`. An absent root starts empty.
Use a new disposable root for each journey.

## Driving it with the Rust test runner

Preconditions:

- Launch and Doctor pass. Set `COLA_VERIFY_FEATURE=session-persistence`.
- The temporary directory is writable. No stored sessions are required.

Run `"$COLA_VERIFY_ADAPTERS" jsonl:: --nocapture --test-threads=1` in the capture block.
The cases live in `tests/adapters/stores/jsonl.rs`.

- Save, reload, and list complete history through `closed_roundtrips_every_fact_and_variant`. Equality and identical repeat-save bytes prove persistence.
- Load active work through `active_recovery_is_read_only_and_retains_dispatch_intent`. It asserts recovered state and unchanged file bytes.
- Save opaque IDs and list through `opaque_ids_sort_distinctly_and_unrelated_files_are_ignored`. It also checks missing-root and storage errors.
- Attempt corrupt and future-version loads and saves through `rejects_corruption_and_preserves_bytes_on_refused_save`, `future_headers_are_classified_before_v1_fields`, and `unreadable_encoding_is_refused_before_publication`. Read-back assertions prove refused writes preserve the prior state.
- Read while saving through `concurrent_publication_exposes_complete_sessions`. Every observed session equals one whole published value.
- Load malformed JSON through `corrupt_records_report_path_and_ordinal_with_json_source`. Public errors retain record context and the JSON source.

## Gotchas

- Filenames hash the session ID. Use the public ID rather than inventing a filename.
- `load` recovers in memory. A separate `save` publishes the recovered value.
- A complete publication does not prove power-loss durability or stale-write detection.
- Tests inspect real files but remove them at completion. Retain the assertion source and transcript as the driver's evidence instructions specify.
