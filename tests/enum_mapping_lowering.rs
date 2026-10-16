use roundhouse::emit::ruby::emit_lowered_models;
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
            "class Trade < ApplicationRecord\n  enum :status, { pending: 0, \"on hold\" => 2 }\n\n  \
             def probe(name)\n    [Trade.statuses[:pending], Trade.statuses[name], Trade.statuses[\"on hold\"], \
             Trade.statuses.key?(:pending)]\n  end\nend\n"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(p, s)| (std::path::PathBuf::from(p), s.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    emit_lowered_models(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_plural_mapping_is_a_class_method_over_every_label() {
    let out = emitted();
    assert!(out.contains("def self.statuses\n    { \"pending\" => 0, \"on hold\" => 2 }\n  end"), "{out}");
}

#[test]
fn a_symbol_key_reads_the_string_label() {
    let out = emitted();
    assert!(out.contains("Trade.statuses[\"pending\"]"), "{out}");
    assert!(out.contains("Trade.statuses[name.to_s]"), "{out}");
    assert!(out.contains("Trade.statuses[\"on hold\"]"), "{out}");
    assert!(out.contains("Trade.statuses.key?(\"pending\")"), "{out}");
}

fn inheritance_app(base_declaration: &str, child_declaration: &str) -> roundhouse::App {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        (
            "app/models/content_base.rb",
            format!("class ContentBase < ApplicationRecord\n  self.abstract_class = true\n  self.table_name = \"trades\"\n  {base_declaration}\nend\n"),
        ),
        (
            "app/models/trade.rb",
            format!("class Trade < ContentBase\n  {child_declaration}\nend\n"),
        ),
        (
            "app/models/unrelated.rb",
            "class Unrelated < ApplicationRecord\n  self.table_name = \"trades\"\nend\n".to_string(),
        ),
        (
            "app/models/concerns/publication_state.rb",
            "module PublicationState\n  extend ActiveSupport::Concern\n  included { enum :status, { draft: 0, live: 3 } }\nend\n".to_string(),
        ),
        (
            "app/models/concerns/local_state.rb",
            "module LocalState\n  extend ActiveSupport::Concern\n  included { enum :status, { queued: 2, shipped: 7 } }\nend\n".to_string(),
        ),
    ]
    .into_iter()
    .map(|(path, source)| (std::path::PathBuf::from(path), source.into_bytes()))
    .collect();
    ingest_app_from_tree(tree).expect("ingest inherited enums")
}

#[test]
fn direct_and_concern_base_enum_maps_reach_only_their_children() {
    use roundhouse::expr::Literal;

    for declaration in [
        "enum :status, { draft: 0, live: 3 }",
        "include PublicationState",
    ] {
        let app = inheritance_app(declaration, "");
        let base = app
            .models
            .iter()
            .find(|model| model.name.0.as_str() == "ContentBase")
            .unwrap();
        let child = app
            .models
            .iter()
            .find(|model| model.name.0.as_str() == "Trade")
            .unwrap();
        let unrelated = app
            .models
            .iter()
            .find(|model| model.name.0.as_str() == "Unrelated")
            .unwrap();
        let status = roundhouse::Symbol::from("status");
        let expected = [
            ("draft".to_string(), Literal::Int { value: 0 }),
            ("live".to_string(), Literal::Int { value: 3 }),
        ];
        assert_eq!(child.parent.as_ref().unwrap().0.as_str(), "ContentBase");
        assert_eq!(
            base.enums.get(&status).map(Vec::as_slice),
            Some(expected.as_slice()),
            "base: {declaration}"
        );
        assert_eq!(
            child.enums.get(&status).map(Vec::as_slice),
            Some(expected.as_slice()),
            "child: {declaration}"
        );
        assert!(
            unrelated.enums.is_empty(),
            "same table is not enum ownership: {declaration}"
        );
    }
}

#[test]
fn a_childs_direct_or_concern_enum_map_wins_over_its_base() {
    use roundhouse::expr::Literal;

    for base_declaration in [
        "enum :status, { draft: 0, live: 3 }",
        "include PublicationState",
    ] {
        for declaration in [
            "enum :status, { queued: 2, shipped: 7 }",
            "include LocalState",
        ] {
            let app = inheritance_app(base_declaration, declaration);
            let base = app
                .models
                .iter()
                .find(|model| model.name.0.as_str() == "ContentBase")
                .unwrap();
            let child = app
                .models
                .iter()
                .find(|model| model.name.0.as_str() == "Trade")
                .unwrap();
            let unrelated = app
                .models
                .iter()
                .find(|model| model.name.0.as_str() == "Unrelated")
                .unwrap();
            let status = roundhouse::Symbol::from("status");
            assert_eq!(
                base.enums[&status],
                [
                    ("draft".to_string(), Literal::Int { value: 0 }),
                    ("live".to_string(), Literal::Int { value: 3 })
                ]
            );
            assert_eq!(
                child.enums[&status],
                [
                    ("queued".to_string(), Literal::Int { value: 2 }),
                    ("shipped".to_string(), Literal::Int { value: 7 })
                ],
                "base {base_declaration}, local override: {declaration}"
            );
            assert!(unrelated.enums.is_empty());
        }
    }
}

/// `status` maps differently in two models, so no column-wide rewrite is
/// safe; a query rooted at a model constant still means that model's
/// mapping (Optimization::RolloutRecord's `where(status: [:active,
/// :scheduled])` reached PostgreSQL as `status IN ('active',
/// 'scheduled')` against an integer column).
#[test]
fn a_where_rooted_at_a_model_uses_that_models_mapping_for_a_conflicted_column() {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        (
            "app/models/trade.rb",
            "class Trade < ApplicationRecord\n  enum :status, { pending: 0, active: 4, scheduled: 5 }\n\n  \
             def self.live(shop_id)\n    where(shop_id: shop_id).where(status: :pending)\n  end\n\n  \
             def self.probe\n    Trade.where(status: [:active, :scheduled]).to_a\n  end\nend\n"
                .to_string(),
        ),
        (
            "app/models/other.rb",
            "class Other < ApplicationRecord\n  self.table_name = \"trades\"\n  enum :status, { active: 1, scheduled: 2 }\nend\n"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(path, source)| (std::path::PathBuf::from(path), source.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let out = emit_lowered_models(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n");
    assert!(out.contains("Relation.new(Trade).where({ status: [4, 5] })"), "{out}");
}

/// A model that overrides `assign_attributes` must not have it run on the
/// empty shell a hydration factory builds: Core's ProductVariant read
/// `shop` there before `shop_id` was loaded and cached nil, so a fixture
/// variant's `shop.tax_codes` was `nil.tax_codes`.
#[test]
fn hydration_skips_an_overridden_assign_attributes_on_the_empty_shell() {
    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        (
            "app/models/trade.rb",
            "class Trade < ApplicationRecord\n  def assign_attributes(attributes)\n    super\n  end\nend\n".to_string(),
        ),
        ("app/models/plain.rb", "class Plain < ApplicationRecord\n  self.table_name = \"trades\"\nend\n".to_string()),
    ]
    .into_iter()
    .map(|(path, source)| (std::path::PathBuf::from(path), source.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let out = emit_lowered_models(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n");
    assert!(out.contains("instance = Trade.new(ActiveRecord::Base::HYDRATE_ATTRS)"), "{out}");
    assert!(out.contains("instance = Plain.new\n"), "{out}");
}

// Rails renders a fixture's ERB against the real model, where `states` is indifferent-access.
#[test]
fn a_fixture_erb_symbol_key_reads_the_string_label() {
    use roundhouse::dialect::FixtureValue;
    use roundhouse::Symbol;

    let tree = [
        ("db/schema.rb", SCHEMA.to_string()),
        ("app/models/trade.rb", "class Trade < ApplicationRecord\n  enum :status, { pending: 0, held: 2 }\nend\n".to_string()),
        (
            "test/fixtures/trades.yml",
            "one:\n  status: <%= Trade.statuses[:held] %>\n".to_string(),
        ),
    ]
    .into_iter()
    .map(|(p, s)| (std::path::PathBuf::from(p), s.into_bytes()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let FixtureValue::Ruby(expr) = &app.fixtures[0].records[&Symbol::from("one")][&Symbol::from("status")] else {
        panic!("expected a Ruby value")
    };
    let dump = format!("{expr:?}");
    assert!(dump.contains("Str { value: \"held\" }") && !dump.contains("Sym {"), "the key is the String label: {dump}");
}
