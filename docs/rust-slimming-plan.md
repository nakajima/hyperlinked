# Rust slimming plan

## Goal
Reduce Rust code size and complexity significantly without changing user-facing behavior.

## Constraints
- Keep current user-facing functionality intact.
- Prefer deleting wrapper code and legacy compatibility code over redesigning core behavior.
- Do not add dependencies.
- Prefer direct SeaORM usage over extra abstraction layers.

## Baseline
Current rough inventory:
- `src/`: ~27.5k LOC
- `tests/`: ~8.9k LOC
- Largest files:
  - `src/app/controllers/admin/dashboard_controller.rs` (~3253)
  - `src/processors/snapshot_fetch.rs` (~2812)
  - `src/processors/readability_fetch.rs` (~1688)
  - `src/app/services/hyperlink_fetcher.rs` (~1337)
  - `src/server/graphql.rs` (~918)
  - `src/import/paperless_ngx.rs` (~818)
- `src/app/models/`: ~4451 LOC
- `src/entity/`: ~767 LOC

## Strategy
Start with the lowest-risk code that is mostly glue, compatibility, or duplication. Avoid touching the large processors first unless simpler cuts are exhausted.

---

## Phase 1: flatten the extra model layer

### Objective
Remove or shrink `src/app/models/*` modules that mostly wrap SeaORM queries and CRUD.

### Priority targets
1. `src/app/models/hyperlink_processing_job.rs`
2. `src/app/models/hyperlink_artifact.rs`
3. `src/app/models/hyperlink_search_doc.rs`
4. `src/app/models/kv_store.rs`
5. `src/app/models/rss_feed.rs`
6. `src/app/models/hyperlink.rs`

### Approach
- Move thin query/update helpers closer to the callers that use them.
- Use `crate::entity::*` directly where the helper adds little value.
- Keep only modules with real domain logic, especially:
  - `artifact_job`
  - `llm_discovery`
  - `url_canonicalize`
  - `settings` if it still meaningfully centralizes config persistence

### Concrete first cut
Start with the smallest, safest modules:
- `hyperlink_search_doc`
- `kv_store`
- `hyperlink_processing_job`

These have clear helper-style behavior and can be reduced without changing product behavior.

### Validation
- Targeted unit tests for affected modules/controllers/processors
- `cargo test` for the touched areas
- Manual check of queue/job lifecycle and search doc updates if needed

### Expected result
Meaningfully less indirection and likely a 1k+ LOC reduction over the full phase.

---

## Phase 2: split the admin god-controller and dedupe backup/import orchestration

### Objective
Reduce size and duplication in admin code.

### Current hotspots
- `src/app/controllers/admin/dashboard_controller.rs`
- `src/server/admin_backup.rs`
- `src/server/admin_import.rs`

### Approach
Split `dashboard_controller.rs` into feature-oriented files:
- `overview_controller.rs`
- `artifacts_controller.rs`
- `llm_controller.rs`
- `storage_controller.rs`
- `import_export_controller.rs`

Then extract common backup/import job-state tracking into a shared internal helper.

### Important note
Simple file splitting alone is not enough. The real win is removing duplicated status-machine code between backup and import managers.

### Validation
- Admin controller tests
- Backup/import status tests
- Smoke test routes still mount correctly

### Expected result
Lower file size, lower coupling, less repeated state-machine code.

---

## Phase 3: remove legacy compatibility and one-shot maintenance paths

### Objective
Delete code that exists only to support old storage/layout states once we define a supported cutoff.

### Candidates
- Blob-to-disk artifact backfill paths in `src/app/models/hyperlink_artifact.rs`
- Snapshot warc gzip backfill paths in `src/app/models/hyperlink_artifact.rs`
- Legacy fallback payload loading in `src/app/models/hyperlink_artifact.rs`
- Maintenance commands in `src/bin/hyperlinked.rs`
- Legacy `Oembed` handling in worker/job code if no longer needed
- Old schema-sync compatibility SQL in `src/db/schema.rs`

### Approach
- Decide the minimum supported on-disk/database version.
- Keep migration logic only if it must run in normal app flow.
- Move rare repair scripts out of the main runtime where possible.

### Validation
- Confirm current dev/prod DBs already satisfy the cutoff
- Run startup/schema sync against a representative DB
- Ensure artifact loading still works on supported data

### Expected result
A real code reduction with little or no user-visible impact.

---

## Phase 4: simplify hyperlink fetching and search

### Objective
Reduce custom search/query code in `src/app/services/hyperlink_fetcher.rs`.

### Approach
- Push more work into SQLite FTS5 where practical.
- Re-evaluate the custom `q=` mini-language.
- Prefer explicit query params for status/type/order where the UI already provides them.
- Reduce bespoke markdown-to-plain-text and snippet/highlight code if FTS can cover it.

### Validation
- Hyperlink index/search tests
- Search relevance sanity checks
- Pagination/filter behavior checks

### Expected result
Smaller, more maintainable search code with less custom string processing.

---

## Phase 5: shrink the custom GraphQL layer

### Objective
Reduce manual schema-building code in `src/server/graphql.rs`.

### Approach
- Let Seaography own as much read-only schema generation as possible.
- Keep only truly custom fields and payloads, such as:
  - updated hyperlink feed
  - readability progress mutation/query pieces
  - artifact URL convenience fields

### Validation
- GraphQL tests
- SDL export check
- Existing client queries still work

### Expected result
Less handwritten schema glue and lower maintenance cost.

---

## Phase 6: test deduplication

### Objective
Reduce repeated setup/assertion code in the largest test files without dropping coverage.

### Biggest test targets
- `tests/unit/app_controllers_hyperlinks_controller.rs`
- `tests/unit/app_controllers_admin_dashboard_controller.rs`
- `tests/unit/app_controllers_admin_jobs_controller.rs`
- `tests/unit/app_services_hyperlink_fetcher.rs`

### Approach
- Add shared fixture builders
- Add small assertion helpers for repeated HTML checks
- Convert repetitive cases to table-driven tests where it improves clarity

### Validation
- Full test suite
- Ensure test readability improves, not just raw LOC

---

## Recommended execution order
1. Phase 1 small model-layer flattening
2. Phase 2 admin split + backup/import dedupe
3. Phase 3 legacy compatibility cleanup
4. Phase 6 test dedupe for touched areas
5. Phase 4 search simplification
6. Phase 5 GraphQL simplification
7. Processor internals only if still worth it

## What we should not start with
Do not start by rewriting:
- `src/processors/snapshot_fetch.rs`
- `src/processors/readability_fetch.rs`

These are large, but much of their size is real feature complexity. They are higher-risk and lower-ROI for the first pass.

## Immediate next step
Begin Phase 1 with the smallest safe slice:
- inventory direct callers of `hyperlink_search_doc`, `kv_store`, and `hyperlink_processing_job`
- inline or relocate thin helpers
- remove dead wrapper code
- run targeted tests
