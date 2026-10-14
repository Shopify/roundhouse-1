//! `value(::Trade.statuses[:pending])` in a nested class body (a GraphQL enum
//! type naming its values after another model's enum mapping) reads the String
//! label as Rails' indifferent-access mapping does; a plain Hash answered nil
//! and the enum value's name came out "".

use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"
ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "trades", force: :cascade do |t|
    t.integer "status"
  end
end
"#;

fn emitted() -> String {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        (
            "app/models/trade.rb",
            "class Trade < ApplicationRecord\n  enum :status, { pending: 0, shipped: 1 }\nend\n".to_string(),
        ),
        (
            "app/models/graph/trade_page.rb",
            "module Graph\n  class TradePage < Base\n    class Kind < Base\n      \
             value(::Trade.statuses[:pending], description: \"p\")\n      \
             value(Trade.statuses.fetch(:shipped), description: \"s\")\n    end\n  end\nend\n"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(p, s)| (std::path::PathBuf::from(p), s.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    emit_library(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_nested_class_body_reads_the_mapping_by_string_label() {
    let out = emitted();
    assert!(out.contains("Trade.statuses[\"pending\"]"), "{out}");
    assert!(out.contains("Trade.statuses.fetch(\"shipped\")"), "{out}");
}
