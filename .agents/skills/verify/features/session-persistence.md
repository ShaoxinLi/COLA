# Session persistence

`SessionArchive` combines conversation facts and invocation evidence. Public
codecs expose checked canonical JSONL bytes without a format-version field.
`ArchiveChange::initial` and `next` validate immutable facts and establish the
candidate revision and digest.

## Public entry points

Construct `JsonlSessionStore::new(root)`. The `SessionStore` port provides
`create`, `inspect`, `list`, and exclusive `acquire`. A `SessionTransaction`
exposes its confirmed archive and supports `commit` and `resolve`.
Inspection preserves active state; `AgentService::recover` closes it explicitly.

## Drive

After Launch and Doctor, set `COLA_VERIFY_FEATURE=session-persistence` and run
`"$COLA_VERIFY_ADAPTERS" jsonl:: --nocapture --test-threads=1` in the capture block.
Read `tests/adapters/stores/jsonl.rs`.

- `closed_roundtrips_every_fact_and_variant` checks full roundtrip facts and bytes.
- `active_reads_preserve_bytes_and_explicit_recovery_closes_once` checks faithful reads and durable idempotent recovery.
- `opaque_ids_sort_distinctly_and_unrelated_files_are_ignored` checks listing and roots.
- `rejects_corruption_and_preserves_bytes_on_refused_commit` and `unreadable_encoding_is_refused_before_publication` check strict rejection without replacement.
- `stable_lock_excludes_writers_creation_never_replaces_and_stale_changes_conflict` checks ownership and compare-and-swap behavior.
- `reconcile_syncs_exact_candidate_preserves_base_and_rejects_unexpected_publication` checks exact uncertainty resolution.
- `concurrent_publication_exposes_only_complete_snapshots` checks atomic visible snapshots.
- `killed_owner_releases_os_lock_for_fresh_acquire` checks process-exit lock release.
- `corrupt_records_report_path_ordinal_and_original_json_error` checks diagnostics.

The application group additionally checks external custom stores and failures
at every service effect gate. Run `cargo test --locked --lib` separately for
internal filesystem fault hooks. Those hooks inject write, rename and sync
failures; public integration tests do not invoke the hooks directly.

## Limits

Use disposable roots. Filenames hash opaque IDs and stable sibling lock files
coordinate cooperating writers. Revisions and digests detect stale candidates.
Readback plus sync-call injection does not establish real hardware power-loss
behavior. Temporary test archives are removed on completion; retain source,
logs and hashes as instructed by the driver.
