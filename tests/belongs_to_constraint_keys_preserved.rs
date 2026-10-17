//! A `belongs_to` declared through Core's `yugabyte_overrides:
//! { query_constraints:, primary_key:, foreign_key: }` writes the foreign
//! key alone (`BelongsToWithQueryConstraints#replace_keys` saves and
//! restores the other constraint columns): `ApiPermission.new(shop_id: s,
//! creator: nil)` kept `shop_id`, where the writer that stored every key
//! part wrote nil over it and `Shop can't be blank` failed the save. A
//! plain `foreign_key: [..]` list still writes every part.

use roundhouse::emit::ruby::emit_lowered_models;
use roundhouse::ingest::{ingest_model, ingest_schema};
use roundhouse::App;

const SCHEMA: &[u8] = br#"
ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "users", force: :cascade do |t|
    t.integer "shop_id"
  end

  create_table "permissions", force: :cascade do |t|
    t.integer "shop_id"
    t.integer "created_by"
  end
end
"#;

const USER: &str = "class User < ApplicationRecord\nend\n";
const OVERRIDES: &str = "class Permission < ApplicationRecord\n  belongs_to :creator, class_name: \"User\", foreign_key: :created_by, inverse_of: false,\n    yugabyte_overrides: { query_constraints: [:shop_id, :created_by], primary_key: [:shop_id, :id], foreign_key: :created_by }\nend\n";
const PLAIN: &str = "class Permission < ApplicationRecord\n  belongs_to :creator, class_name: \"User\", foreign_key: [:shop_id, :created_by], primary_key: [:shop_id, :id], inverse_of: false\nend\n";

const COVERING: &str = "class Permission < ApplicationRecord\n  belongs_to :creator, class_name: \"User\", foreign_key: [:shop_id, :created_by], inverse_of: false,\n    yugabyte_overrides: { query_constraints: [:created_by, :shop_id], primary_key: [:id, :shop_id], foreign_key: [:created_by, :shop_id] }\nend\n";

fn emitted(permission: &str) -> String {
    let schema = ingest_schema(SCHEMA, "db/schema.rb").expect("ingest schema");
    let mut app = App::new();
    for (src, path) in [(USER, "app/models/user.rb"), (permission, "app/models/permission.rb")] {
        let model = ingest_model(src.as_bytes(), path, &schema, &Default::default())
            .expect("ingest model")
            .expect("model recognized");
        app.models.push(model);
    }
    app.schema = schema;
    emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/permission.rb"))
        .map(|f| f.content)
        .expect("permission.rb emitted")
}

/// The text of `def <signature>` up to the next method.
fn method_with<'a>(out: &'a str, signature: &str) -> &'a str {
    let start = out.find(&format!("def {signature}")).unwrap_or_else(|| panic!("no def {signature}:\n{out}"));
    let rest = &out[start..];
    let end = ["\n  def ", "\n    def "].iter().filter_map(|next| rest[1..].find(next)).min().map_or(rest.len(), |i| i + 1);
    &rest[..end]
}

#[test]
fn query_constraints_columns_survive_the_writer() {
    let out = emitted(OVERRIDES);
    let writer = method_with(&out, "creator=(value)");
    assert!(writer.contains("@created_by = nil") || writer.contains("@created_by = 0"), "{writer}");
    assert!(!writer.contains("@shop_id"), "{writer}");
    let autosave = method_with(&out, "_autosave_creator");
    assert!(!autosave.contains("@shop_id"), "{autosave}");
    // A `shop_id` written after the target does not drop it (the wrapper's
    // `stale_state` reads the foreign key alone).
    assert!(!method_with(&out, "shop_id=(").contains("@creator_loaded = false"), "{out}");
    // The reader still matches the whole key.
    assert!(out.contains("= @shop_id"), "{out}");
}

#[test]
fn a_plain_composite_list_still_writes_every_part() {
    let out = emitted(PLAIN);
    let writer = method_with(&out, "creator=(value)");
    assert!(writer.contains("@shop_id = nil"), "{writer}");
    assert!(writer.contains("@shop_id = value._read_attribute(:shop_id)"), "{writer}");
    // ... and a later `shop_id` write drops the loaded target.
    assert!(method_with(&out, "shop_id=(").contains("@creator_loaded = false"), "{out}");
}

/// Core's `ContractDiscount#contract_identity` lists every constraint column
/// as the wrapper's writable `foreign_key`, so `replace_keys` writes both:
/// `self.contract_identity ||= contract.contract_identity` must set
/// `contract_identity_id` as well as `shop_id` (the row's NOT NULL column).
#[test]
fn a_foreign_key_list_covering_the_constraints_writes_every_part() {
    let out = emitted(COVERING);
    let writer = method_with(&out, "creator=(value)");
    assert!(writer.contains("@created_by = value._read_attribute(:id)") || writer.contains("@created_by = value.id"), "{writer}");
    assert!(writer.contains("@shop_id = value._read_attribute(:shop_id)"), "{writer}");
    assert!(writer.contains("@shop_id = nil"), "{writer}");
}
