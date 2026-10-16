//! Composite association keys (`foreign_key: [:shop_id, :order_id]`,
//! `primary_key: [..]`, `query_constraints:` and Core's
//! `yugabyte_overrides:`): ingest records the writable pair in the
//! single-column fields and the rest in `extra_keys`, and the readers
//! match every pair. The behavior against real rows, compared with native
//! Rails, is `composite_foreign_keys_pg_runtime.rs`.

use roundhouse::dialect::Association;
use roundhouse::emit::ruby::emit_lowered_models;
use roundhouse::ingest::{ingest_model, ingest_schema};
use roundhouse::App;

const SCHEMA: &[u8] = br#"
ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "orders", force: :cascade do |t|
    t.integer "shop_id"
    t.string "name"
  end

  create_table "draft_orders", force: :cascade do |t|
    t.integer "shop_id"
    t.integer "active_order_id"
  end

  create_table "order_lines", force: :cascade do |t|
    t.integer "shop_id"
    t.integer "order_id"
  end
end
"#;

const ORDER: &str = "class Order < ApplicationRecord
  has_one :draft_order,
    foreign_key: [:shop_id, :active_order_id],
    yugabyte_overrides: {
      query_constraints: [:active_order_id, :shop_id],
      primary_key: [:id, :shop_id],
      foreign_key: :active_order_id
    }
  has_many :order_lines, foreign_key: [:shop_id, :order_id]
end
";
const DRAFT_ORDER: &str = "class DraftOrder < ApplicationRecord
  belongs_to :active_order, class_name: \"Order\", optional: true,
    foreign_key: [:shop_id, :active_order_id],
    yugabyte_overrides: {
      query_constraints: [:active_order_id, :shop_id],
      primary_key: [:id, :shop_id],
      foreign_key: :active_order_id
    }
end
";
const ORDER_LINE: &str = "class OrderLine < ApplicationRecord
  belongs_to :order, foreign_key: [:shop_id, :order_id], primary_key: [:shop_id, :id]
end
";

fn app() -> App {
    let schema = ingest_schema(SCHEMA, "db/schema.rb").expect("ingest schema");
    let mut app = App::new();
    for (src, path) in [
        (ORDER, "app/models/order.rb"),
        (DRAFT_ORDER, "app/models/draft_order.rb"),
        (ORDER_LINE, "app/models/order_line.rb"),
    ] {
        let model = ingest_model(src.as_bytes(), path, &schema, &Default::default())
            .expect("ingest model")
            .expect("model recognized");
        app.models.push(model);
    }
    app.schema = schema;
    app
}

fn keys(assoc: &Association) -> (String, Option<String>, Vec<(String, String)>) {
    let fk = match assoc {
        Association::HasOne { foreign_key, .. }
        | Association::HasMany { foreign_key, .. }
        | Association::BelongsTo { foreign_key, .. } => foreign_key.to_string(),
        other => panic!("{other:?}"),
    };
    (
        fk,
        assoc.declared_primary_key().map(|k| k.to_string()),
        assoc.extra_keys().iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
    )
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
fn yugabyte_overrides_replace_the_plain_array() {
    let app = app();
    let draft_order = app.models[0].associations().next().unwrap();
    // `order_id` (what an Array foreign_key used to fall back to) is not the column.
    assert_eq!(keys(draft_order), ("active_order_id".into(), None, vec![pair("shop_id", "shop_id")]));
}

#[test]
fn a_plain_array_foreign_key_meets_the_owners_id() {
    let app = app();
    let lines = app.models[0].associations().nth(1).unwrap();
    assert_eq!(keys(lines), ("order_id".into(), None, vec![pair("shop_id", "shop_id")]));
}

#[test]
fn belongs_to_pairs_the_owner_column_with_the_target_column() {
    let app = app();
    let active_order = app.models[1].associations().next().unwrap();
    assert_eq!(keys(active_order), ("active_order_id".into(), None, vec![pair("shop_id", "shop_id")]));
    let order = app.models[2].associations().next().unwrap();
    assert_eq!(keys(order), ("order_id".into(), None, vec![pair("shop_id", "shop_id")]));
}

/// The text of `def <name>` up to the next method.
fn method_text<'a>(out: &'a str, name: &str) -> &'a str {
    let start = out.find(&format!("def {name}\n")).unwrap_or_else(|| panic!("no def {name}:\n{out}"));
    let rest = &out[start..];
    let end = rest[1..].find("\n  def ").map_or(rest.len(), |i| i + 1);
    &rest[..end]
}

#[test]
fn readers_match_every_key_pair() {
    let out = emit_lowered_models(&app())
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    let draft_order = method_text(&out, "draft_order");
    assert!(draft_order.contains("active_order_id") && draft_order.contains("shop_id"), "{draft_order}");
    assert!(!draft_order.contains("order_id: "), "the owner-derived `order_id` column is gone:\n{draft_order}");
    let lines = method_text(&out, "order_lines");
    assert!(lines.contains("shop_id"), "{lines}");
    let order = method_text(&out, "order");
    assert!(order.contains("shop_id"), "the belongs_to reader matches the owner's shop too:\n{order}");
    let active_order = method_text(&out, "active_order");
    assert!(active_order.contains("shop_id"), "{active_order}");
}
