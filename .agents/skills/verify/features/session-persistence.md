# Session persistence

Use `SessionStore::create`, `acquire`, `inspect`, and `list` through disposable
`JsonlSessionStore` roots. `SessionWriter` holds the stable OS session lock and
publishes validated successor revisions. Inspection is read only; recovery is an
explicit application commit.

After Launch and Doctor, set `COLA_VERIFY_FEATURE=session-persistence` and run
all commands below inside the driver's evidence capture block:

```bash
"$COLA_VERIFY_ADAPTERS" jsonl:: --nocapture --test-threads=1
"$COLA_VERIFY_APP" session_store:: --nocapture --test-threads=1
"$COLA_VERIFY_APP" session_record:: --nocapture --test-threads=1
"$COLA_VERIFY_APP" agent_service::every_checkpoint_failure_stops_external_io_even_after_publication --exact --nocapture --test-threads=1
"$COLA_VERIFY_ADAPTERS" models::client::corrupt_invocation_associations_and_frozen_views_fail_read_validation --exact --nocapture --test-threads=1
"$COLA_VERIFY_ADAPTERS" models::client::archive_rejects_a_successful_return_after_a_denied_review --exact --nocapture --test-threads=1
```

- JSONL cases cover version 2 snapshots, stale revisions, writer invalidation, independent-process locks, corruption, identity mismatch, and unchanged rejected bytes.
- Record cases cover exact audit evidence, accepted-step provenance, frozen views, read-only reopening, durable review blocks, acknowledgement consistency, and planned/prepared recovery through public service/store entry points.
- The service checkpoint matrix covers failures before and after publication and proves that later external I/O stops.
- The adapter corruption cases reject invalid provenance, lifecycle, acknowledgement, frozen-view associations, and a successful return after denied review.

The executable integration tests separately run fresh CLI `/use` and Web
processes against persisted histories for both providers.

JSONL files are full record snapshots. Filenames hash IDs; lock files remain
stable and are not deleted to unlock. A publication error may leave newer data
visible and never permits further external I/O with that writer. Tests do not
claim network-filesystem locking or hardware power-loss guarantees.
