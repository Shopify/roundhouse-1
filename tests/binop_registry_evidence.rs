//! An operator is a mismatch only when the registry proves the receiver
//! lacks it.
//!
//! ```ruby
//! #: (Pricing::Amount a, Pricing::Amount b) -> Pricing::Amount
//! def add(a, b) = a + b
//!
//! def published? = self.class < ::Publishable
//!
//! #: (Class[top]) -> String
//! def name_for(type) = type <= Hash ? "object" : "value"
//! ```
//!
//! `Pricing::Amount` is declared by a signature but defined in a gem the
//! app does not ingest; its `+` is whatever that gem says, so the pair is
//! undecidable, not `Incompatible`. A class the registry models but whose
//! ancestry it cannot finish (an unmodeled superclass or mixin) is the same.
//! `self.class` and a `Class[top]` value are class objects, so `<` and `<=`
//! are `Module#<`, the subclass relation. A modeled class with no such
//! operator anywhere in its known ancestry is still reported.

use roundhouse::analyze::{diagnose, Analyzer, DiagnosticKind};

fn binops(files: &[(&str, &str)]) -> Vec<String> {
    let mut all: Vec<(&str, &str)> = vec![
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
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
        .filter(|d| matches!(d.kind, DiagnosticKind::IncompatibleBinop { .. }))
        .map(|d| format!("{:?}", d.kind))
        .collect()
}

#[test]
fn an_unmodeled_receiver_class_is_undecidable() {
    let found = binops(&[(
        "app/models/thing.rb",
        r#"class Thing < ApplicationRecord
  #: (Pricing::Amount a, Pricing::Amount b) -> Pricing::Amount
  def add(a, b)
    a + b
  end

  #: (Pricing::Amount a, Pricing::Amount b) -> bool
  def less(a, b)
    a < b
  end

  #: (Pricing::Amount a, Integer b) -> Pricing::Amount
  def scale(a, b)
    a * b
  end
end
"#,
    )]);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_modeled_class_with_an_unmodeled_ancestor_is_undecidable() {
    let found = binops(&[
        ("app/models/amount.rb", "class Amount < Pricing::Base\n  def cents = 1\nend\n"),
        ("app/models/ratio.rb", "class Ratio\n  include Pricing::Arithmetic\n  def value = 1\nend\n"),
        (
            "app/models/thing.rb",
            r#"class Thing < ApplicationRecord
  #: (Amount a, Amount b) -> untyped
  def add(a, b)
    a + b
  end

  #: (Ratio a, Ratio b) -> untyped
  def sub(a, b)
    a - b
  end
end
"#,
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn comparable_supplies_the_ordering_operators() {
    let found = binops(&[
        (
            "app/models/version.rb",
            "class Version\n  include Comparable\n  def initialize(n)\n    @n = n\n  end\n  def n = @n\n  def <=>(other) = n <=> other.n\nend\n",
        ),
        (
            "app/models/thing.rb",
            r#"class Thing < ApplicationRecord
  def newer?
    Version.new(2) > Version.new(1)
  end
end
"#,
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn class_values_compare_by_subclass_relation() {
    let found = binops(&[
        ("app/models/publishable.rb", "module Publishable\nend\n"),
        (
            "app/models/thing.rb",
            r#"class Thing < ApplicationRecord
  def published?
    self.class < ::Publishable
  end

  def kind_of_record?
    self.class <= ApplicationRecord
  end

  #: (Class[top] type) -> String
  def name_for(type)
    if type <= Hash
      "object"
    elsif type <= Integer
      "integer"
    elsif type < Numeric
      "number"
    else
      "value"
    end
  end
end
"#,
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_modeled_class_without_the_operator_is_still_a_mismatch() {
    let found = binops(&[
        ("app/models/box.rb", "class Box\n  def size = 1\nend\n"),
        (
            "app/models/thing.rb",
            r#"class Thing < ApplicationRecord
  #: (Box a, Box b) -> untyped
  def add(a, b)
    a + b
  end

  #: (Box a, Box b) -> untyped
  def less(a, b)
    a < b
  end
end
"#,
        ),
    ]);
    assert_eq!(found.len(), 2, "{found:?}");
}
