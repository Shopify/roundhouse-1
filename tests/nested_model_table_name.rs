//! Rails table names that Core's models rely on and ingest got wrong.
//!
//! - A base class nested in a concrete model takes the parent's singular
//!   table name as a prefix (`compute_table_name`): Core's
//!   `DraftOrder::AdditionalFee` is `draft_order_additional_fees`.
//! - The app's inflections live in any initializer (Core's
//!   `004_inflections.rb`), and an acronym underscores as one word:
//!   `ShopB2BReadinessScore` is `shop_b2b_readiness_scores`.
//!
//! Each model read a table no schema has, so its SQL named the wrong
//! table and every column read was `no known method`.

use roundhouse::det::HashMap;
use std::path::PathBuf;

fn table(name: &str, columns: &str) -> String {
    format!("CREATE TABLE {name} (\n    id bigint NOT NULL,\n{columns}    CONSTRAINT {name}_pkey PRIMARY KEY(id)\n);\n\n")
}

const READER: &str = r#"class Reader
  #: (DraftOrder::AdditionalFee) -> Integer
  def self.fee(fee) = fee.amount

  #: (Shop::B2BReadinessScore) -> Integer
  def self.score(score) = score.points
end
"#;

#[test]
fn nested_and_acronym_models_name_their_rails_tables() {
    let structure = [
        table("draft_orders", ""),
        table("draft_order_additional_fees", "    amount integer NOT NULL,\n"),
        table("draft_order_notes", ""),
        table("legacy_fees", ""),
        table("shops", ""),
        table("shop_b2b_readiness_scores", "    points integer NOT NULL,\n"),
        table("retail_tos_commitments", ""),
        table("html_parsers", ""),
        table("retail_hardware", ""),
    ]
    .concat();
    let inflections = "ActiveSupport::Inflector.inflections(:en) do |inflect|\n  inflect.acronym(\"B2B\")\n  inflect.acronym(\"TOS\")\n  inflect.uncountable(\"hardware\")\nend\n";
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/structure.sql", structure.as_str()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("config/initializers/004_inflections.rb", inflections),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/draft_order.rb", "class DraftOrder < ApplicationRecord\nend\n"),
        ("app/models/draft_order/additional_fee.rb", "class DraftOrder\n  class AdditionalFee < ApplicationRecord\n  end\nend\n"),
        // STI keeps the base table; an explicit `table_name` keeps its own.
        ("app/models/draft_order/rush.rb", "class DraftOrder\n  class Rush < DraftOrder\n  end\nend\n"),
        ("app/models/draft_order/legacy_fee.rb", "class DraftOrder::LegacyFee < ApplicationRecord\n  self.table_name = \"legacy_fees\"\nend\n"),
        ("app/models/draft_order/note.rb", "class DraftOrder::Note < ApplicationRecord\nend\n"),
        ("app/models/shop.rb", "class Shop < ApplicationRecord\nend\n"),
        ("app/models/shop/b2b_readiness_score.rb", "class Shop\n  class B2BReadinessScore < ApplicationRecord\n  end\nend\n"),
        ("app/models/retail_tos_commitment.rb", "class RetailTOSCommitment < ApplicationRecord\nend\n"),
        ("app/models/html_parser.rb", "class HTMLParser < ApplicationRecord\nend\n"),
        ("app/models/retail/hardware.rb", "module Retail\n  def self.table_name_prefix\n    \"retail_\"\n  end\n\n  class Hardware < ApplicationRecord\n  end\nend\n"),
        ("app/models/reader.rb", READER),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    let table = |name: &str| app.models.iter().find(|m| m.name.0.as_str() == name).map(|m| m.table.0.to_string());
    assert_eq!(table("DraftOrder::AdditionalFee").as_deref(), Some("draft_order_additional_fees"));
    assert_eq!(table("DraftOrder::Note").as_deref(), Some("draft_order_notes"));
    assert_eq!(table("DraftOrder::LegacyFee").as_deref(), Some("legacy_fees"));
    assert_ne!(table("DraftOrder::Rush").as_deref(), Some("draft_order_rushes"));
    assert_eq!(table("Shop::B2BReadinessScore").as_deref(), Some("shop_b2b_readiness_scores"));
    assert_eq!(table("RetailTOSCommitment").as_deref(), Some("retail_tos_commitments"));
    assert_eq!(table("HTMLParser").as_deref(), Some("html_parsers"));
    assert_eq!(table("Retail::Hardware").as_deref(), Some("retail_hardware"));
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("amount") || e.contains("points")), "{errors:?}");
}
