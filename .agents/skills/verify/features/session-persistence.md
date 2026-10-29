# Session persistence

Use `SessionStore::create`, `acquire`, `inspect`, and `list` through disposable
`JsonlSessionStore` roots. `SessionWriter` holds the stable OS session lock and
publishes validated successor revisions. Inspection is read only; recovery is an
explicit application commit.

After Launch and Doctor, set `COLA_VERIFY_FEATURE=session-persistence` and run:

```bash
"$COLA_VERIFY_ADAPTERS" jsonl:: --nocapture --test-threads=1
```

- `empty_document_roundtrip_and_stale_commit_invalidates_the_writer` checks versioned empty documents, stale revision rejection, and writer invalidation.
- `independent_process_lock_is_stable_and_released_on_exit` runs a child process against the same session and checks busy ownership and later acquisition.
- `old_version_corruption_and_identity_mismatch_are_preserved_and_refused` checks unsupported version 1, corrupt counts, identity mismatches, and unchanged rejected bytes.

Also run the app group for before/after-publication failure injection and explicit
recovery. Run `models::client::corrupt_invocation_associations_and_frozen_views_fail_read_validation`
for provenance, lifecycle, acknowledgement, and frozen-view corruption. The
executable integration tests run fresh CLI `/use` and Web processes against
persisted histories for both providers.

JSONL files are full document snapshots, not append-only logs. Filenames hash IDs;
lock files remain stable and are not deleted to unlock. A publication error may
leave newer data visible and never permits further external I/O with that writer.
Tests do not claim network-filesystem locking or hardware power-loss guarantees.
