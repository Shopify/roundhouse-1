# Composite association keys

`foreign_key: [:shop_id, :order_id]`, `primary_key: [...]`, `query_constraints:` and
Core's `yugabyte_overrides: { query_constraints:, primary_key:, foreign_key: }` on
has_one / has_many / belongs_to (`src/ingest/composite_keys.rs`).

- Core runs on PostgreSQL, so `YugabyteAssociations` swaps `yugabyte_overrides` in for the
  plain options; the overrides win when written.
- Rails (the Shopify fork) reads each option as column pairs: the foreign-key-side column
  zipped with the referenced-side one. The IR keeps the writable pair in `foreign_key` /
  `primary_key` and the rest in `Association::extra_keys()`
  (`(foreign-key side, referenced side)`), so single-column consumers keep working.
- Readers (`has_one`, `has_many`, `belongs_to`) and join `ON` tails match every pair.
  `includes`/`preload` of a composite association keeps the lazy reader (the batch
  loaders match one column). `where(order: record)` on a composite belongs_to is kept for
  the runtime: the reflection carries `key_pairs:` and `association_key_predicates`
  matches every pair.

Known gap: building through a plain (non-`query_constraints`) array key does not write the
extra pair columns (`order.order_lines.build.shop_id`); Core's overrides are all
query-constraint keys, which Rails never writes either.

Proof: `tests/composite_foreign_keys.rs` (shape), `tests/composite_foreign_keys_pg_runtime.rs`
(PG-gated, native Rails vs emitted).
