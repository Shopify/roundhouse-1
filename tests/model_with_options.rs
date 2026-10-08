//! `with_options` in a model body merges its options into each call.
//!
//! Core's `ShopifyPayments::Account` declares `capabilities` and ten
//! other associations inside `with_options inverse_of:, foreign_key: …
//! do`, and 35 Core models wrap `validates` that way. The block was one
//! Unknown item: the associations were never registered (every read was
//! `no known method`) and neither they nor the validations ran in the
//! emitted app. Rails' `OptionMerger` deep-merges the options, the call's
//! own winning; the block-parameter form (`|o| o.validates`) is the same.

use roundhouse::dialect::{Association, ModelBodyItem, ValidationRule};
use roundhouse::det::HashMap;
use std::path::PathBuf;

const ACCOUNT: &str = r#"class Account < ApplicationRecord
  with_options inverse_of: :account, foreign_key: :owner_id, extra: { a: 1, b: 2 } do
    has_many :capabilities, class_name: "::Capability"
    has_many :pauses, foreign_key: :account_id, extra: { b: 3 }
  end

  with_options presence: true do
    validates :name
  end

  with_options length: { maximum: 5 } do |o|
    o.validates :code
  end

  #: -> Integer
  def count = capabilities.size + pauses.size
end
"#;

#[test]
fn with_options_merges_into_each_declaration() {
    let structure = "CREATE TABLE accounts (\n    id bigint NOT NULL,\n    name character varying NOT NULL,\n    code character varying NOT NULL\n);\n\n\
CREATE TABLE capabilities (\n    id bigint NOT NULL,\n    owner_id bigint NOT NULL\n);\n\n\
CREATE TABLE pauses (\n    id bigint NOT NULL,\n    account_id bigint NOT NULL\n);\n";
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/structure.sql", structure),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/account.rb", ACCOUNT),
        ("app/models/capability.rb", "class Capability < ApplicationRecord\n  belongs_to :account, foreign_key: :owner_id\nend\n"),
        ("app/models/pause.rb", "class Pause < ApplicationRecord\n  belongs_to :account\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    let account = app.models.iter().find(|m| m.name.0.as_str() == "Account").expect("Account");
    let has_many = |name: &str| {
        account.body.iter().find_map(|item| match item {
            ModelBodyItem::Association { assoc: Association::HasMany { name: n, target, foreign_key, .. }, .. }
                if n.as_str() == name =>
            {
                Some((target.0.to_string(), foreign_key.as_str().to_string()))
            }
            _ => None,
        })
    };
    assert_eq!(has_many("capabilities"), Some(("Capability".to_string(), "owner_id".to_string())));
    // The call's own `foreign_key:` wins over the block's.
    assert_eq!(has_many("pauses").map(|(_, fk)| fk).as_deref(), Some("account_id"));
    let rules = |attr: &str| -> Vec<ValidationRule> {
        account
            .body
            .iter()
            .filter_map(|item| match item {
                ModelBodyItem::Validation { validation, .. } if validation.attribute.as_str() == attr => {
                    Some(validation.rules.clone())
                }
                _ => None,
            })
            .flatten()
            .collect()
    };
    assert!(rules("name").contains(&ValidationRule::Presence), "{:?}", rules("name"));
    assert!(
        rules("code").iter().any(|r| matches!(r, ValidationRule::Length { max: Some(5), .. })),
        "{:?}",
        rules("code")
    );
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("capabilities") || e.contains("pauses")), "{errors:?}");
}
