//! A `def self.x` (or `class << self`) signature types the class method.
//!
//! ```ruby
//! class Pricing
//!   #: (String code) -> String
//!   def self.label(code) = TABLE.fetch(code)
//! end
//!
//! Pricing.label("a").imaginary_method   # no `imaginary_method` on String
//! ```
//!
//! The registry overlay put every app signature in `instance_methods`, and
//! dispatch on the class object reads `class_methods` first, where the entry
//! was whatever inference harvested from the body (`TABLE.fetch` is
//! untyped), so the declaration bounded nothing. Signatures read on the
//! singleton side now land in `class_methods`.

use roundhouse::analyze::{diagnose, Analyzer, DiagnosticKind};

fn errors(files: &[(&str, &str)]) -> Vec<String> {
    let mut all: Vec<(&str, &str)> = vec![
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table \"events\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"things\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n",
        ),
    ];
    all.extend_from_slice(files);
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> = all
        .iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .into_iter()
        .filter(|d| {
            matches!(
                d.kind,
                DiagnosticKind::SendDispatchFailed { .. } | DiagnosticKind::IncompatibleBinop { .. }
            )
        })
        .map(|d| format!("{:?}", d.kind))
        .collect()
}

const PRICING: (&str, &str) = (
    "app/models/pricing.rb",
    r#"class Pricing
  TABLE = {}

  #: (String code) -> String
  def self.label(code)
    TABLE.fetch(code)
  end

  class << self
    #: (String code) -> Integer
    def cents(code)
      TABLE.fetch(code)
    end
  end
end
"#,
);

#[test]
fn a_class_method_signature_types_the_class_method() {
    let found = errors(&[
        PRICING,
        (
            "app/models/thing.rb",
            "class Thing < ApplicationRecord\n  def probe\n    Pricing.label(\"a\").upcase\n    Pricing.cents(\"a\") + 1\n    Pricing.label(\"a\").imaginary_method\n  end\nend\n",
        ),
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("imaginary_method") && found[0].contains("Str"), "{found:?}");
}

#[test]
fn a_singleton_signature_names_classes_lexically() {
    let found = errors(&[(
        "app/models/feeds/event.rb",
        r#"module Feeds
  class Event < ApplicationRecord
    class << self
      #: (Event event) -> String?
      def describe(event)
        event.name
      end
    end

    #: (Event other) -> String?
    def self.other_name(other)
      other.name
    end
  end
end
"#,
    )]);
    assert!(found.is_empty(), "{found:?}");
}
