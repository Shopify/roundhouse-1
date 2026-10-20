//! `enum :category, category_enum, prefix: :category` where `category_enum`
//! is a local the class body assigned above (Core's `ShopifyMetric` builds
//! its mapping with `Enum.values.each_with_index.map { … }.to_h`). The
//! declaration was refused, and a survey dropped the whole enum, so
//! `category=` cast every label to nil.

use roundhouse::emit::ruby::{emit_library, emit_lowered_models};
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"
ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "metrics", force: :cascade do |t|
    t.integer "category"
  end
end
"#;

fn emitted(model: &str) -> String {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n".to_string()),
        ("app/models/metric.rb", model.to_string()),
    ]
    .into_iter()
    .map(|(p, s)| (std::path::PathBuf::from(p), s.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let mut out: Vec<String> = emit_lowered_models(&app).into_iter().map(|f| f.content).collect();
    out.extend(emit_library(&app).into_iter().map(|f| f.content));
    out.join("\n")
}

#[test]
fn a_local_array_mapping_is_kept() {
    let out = emitted(
        "class Metric < ApplicationRecord\n  \
         category_enum = %i[sales orders]\n\n  \
         enum :category, category_enum, prefix: :category\nend\n",
    );
    assert!(out.contains("def category_sales?"), "{out}");
    assert!(out.contains("def category_orders?"), "{out}");
}

#[test]
fn a_local_hash_mapping_is_kept() {
    let out = emitted(
        "class Metric < ApplicationRecord\n  \
         category_enum = { sales: 5, orders: 9 }\n\n  \
         enum :category, category_enum, prefix: :category\nend\n",
    );
    assert!(out.contains("def category_sales?"), "{out}");
    assert!(out.contains('9'), "{out}");
}

#[test]
fn a_mapping_indexed_over_labels_is_kept() {
    let out = emitted(
        "class Metric < ApplicationRecord\n  \
         category_enum = %i[sales orders].each_with_index.map do |label, index|\n    [label.to_sym, index]\n  end.to_h\n\n  \
         enum :category, category_enum, prefix: :category\nend\n",
    );
    assert!(out.contains("def category_sales?") && out.contains("def category_orders?"), "{out}");
}

/// A local written twice is not the value the enum read.
#[test]
fn a_reassigned_local_is_not_folded() {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n".to_string()),
        (
            "app/models/metric.rb",
            "class Metric < ApplicationRecord\n  category_enum = %i[a]\n  category_enum = %i[b]\n  \
             enum :category, category_enum\nend\n"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(p, s)| (std::path::PathBuf::from(p), s.into_bytes()))
    .collect();
    let err = ingest_app_from_tree(tree).err().expect("refused");
    assert!(format!("{err:?}").contains("mapping must be"), "{err:?}");
}
