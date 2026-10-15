//! A required `belongs_to` whose target is keyed on a column list
//! (`self.primary_key = [..]`) and names `primary_key: :id`: its existence
//! check asks the target by that column. `Target.exists?(@fk)` on a
//! composite key takes a key tuple, so the scalar rendered `table.["a",
//! "b"] = $1` and PostgreSQL refused it (Core's MigratedNamedQuantity
//! `belongs_to :named_quantity` on NamedQuantity).

use roundhouse::emit::ruby::emit_lowered_models;
use roundhouse::ingest::{ingest_model, ingest_schema};
use roundhouse::App;

const SCHEMA: &[u8] = br#"
ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "parts", force: :cascade do |t|
    t.integer "shop_id"
    t.string "name"
  end

  create_table "lines", force: :cascade do |t|
    t.integer "part_id"
  end
end
"#;

const PART: &str = "class Part < ApplicationRecord\n  self.primary_key = [:shop_id, :name]\nend\n";
const LINE: &str = "class Line < ApplicationRecord\n  belongs_to :part, foreign_key: :part_id, primary_key: :id, optional: false\nend\n";
const PLAIN_LINE: &str = "class Line < ApplicationRecord\n  belongs_to :part, foreign_key: :part_id, optional: false\nend\n";

fn emitted(line: &str) -> String {
    let schema = ingest_schema(SCHEMA, "db/schema.rb").expect("ingest schema");
    let mut app = App::new();
    for (src, path) in [(PART, "app/models/part.rb"), (line, "app/models/line.rb")] {
        let model = ingest_model(src.as_bytes(), path, &schema, &Default::default())
            .expect("ingest model")
            .expect("model recognized");
        app.models.push(model);
    }
    app.schema = schema;
    emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/line.rb"))
        .map(|f| f.content)
        .expect("line.rb emitted")
}

#[test]
fn declared_id_asks_a_composite_target_by_that_column() {
    let line = emitted(LINE);
    assert!(!line.contains("Part.exists?(@part_id)"), "{line}");
    assert!(line.contains("\"id\""), "{line}");
}

#[test]
fn undeclared_key_keeps_the_plain_check() {
    let line = emitted(PLAIN_LINE);
    assert!(line.contains("Part.exists?(@part_id)"), "{line}");
}

/// The writer stores the target's `id` column, which a composite-keyed
/// target answers as `id_value` (its `id` is the key tuple): Core's
/// `Discount.create!(contract_identity: contract.contract_identity)`
/// stored the tuple, serialized as NULL.
#[test]
fn writer_stores_the_id_column_of_a_composite_target() {
    let line = emitted(LINE);
    assert!(line.contains("@part_id = value.id_value"), "{line}");
    assert!(!line.contains("@part_id = value.id\n"), "{line}");
    let plain = emitted(PLAIN_LINE);
    assert!(plain.contains("@part_id = value."), "{plain}");
}
