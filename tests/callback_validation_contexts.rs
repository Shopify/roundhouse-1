//! `before_validation :a, on: [:create, :fulfillment]`: Core's `GiftCard`
//! generates its code, balance and cross-currency strategy there and
//! saves with `save!(context: :fulfillment)`. Ingest used to reject any
//! `on:` that was not a lone `:create`/`:update`/`:destroy`, so the three
//! callbacks were dropped and the gift card failed validation (blank
//! initial value and code). A validation hook now runs when the record's
//! `validation_context` is any of the contexts named.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  create_table :widgets do |t|\n    t.string :color\n  end\nend\n";

const WIDGET: &str = r##"class Widget < ApplicationRecord
  before_validation :assign_color, on: [:create, :fulfillment]
  before_validation :assign_other, on: :fulfillment
  after_validation :after_color, on: [:update]

  private
    def assign_color
      self.color = "#000000"
    end

    def assign_other
      self.color = "other"
    end

    def after_color
      self.color = "after"
    end
end
"##;

fn emitted() -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        ("app/models/widget.rb", WIDGET),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    roundhouse::emit::ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.ends_with("widget.rb"))
        .expect("widget.rb emitted")
        .content
        .clone()
}

#[test]
fn an_array_of_contexts_guards_the_validation_callback() {
    let src = emitted();
    assert!(
        src.contains("[:create, :fulfillment].intersect?(Array(validation_context))"),
        "the callback must run in any of the named contexts:\n{src}"
    );
    assert!(src.contains("assign_color"), "the callback must not be dropped:\n{src}");
}

#[test]
fn a_custom_context_guards_the_validation_callback() {
    let src = emitted();
    assert!(src.contains("[:fulfillment].intersect?(Array(validation_context))"), "{src}");
    assert!(src.contains("[:update].intersect?(Array(validation_context))"), "{src}");
}
