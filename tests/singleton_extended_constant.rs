//! A bare constant in a `class << self` body found through a module the
//! class extends.
//!
//! Core's `CustomerApiSimulatedResponse::CompanyContact` extends
//! `Checkouts::One::SimulatedCheckoutService` and reads its `TIME_BEFORE`
//! bare in `class << self`, where Ruby looks in the singleton's ancestors.
//! The singleton also includes `ActionView::Helpers::DateHelper`, which
//! Rubydex cannot see, so it left the read unresolved and every read was
//! refused as `constant not supported`. A `def self.` body is not the
//! singleton's scope: Ruby raises NameError there, and it stays refused.

use roundhouse::det::HashMap;
use std::path::PathBuf;

const SIMULATED: &str = r#"module Cs
  module Simulated
    TIME_BEFORE = "then"
    ID = 7
  end
end
"#;

const CONTACT: &str = r#"module Cs
  module Contact
    extend Simulated

    class << self
      include ActionView::Helpers::DateHelper

      #: -> String
      def created = TIME_BEFORE

      #: -> Integer
      def id = ID + 1
    end
  end
end
"#;

const OUTSIDE: &str = r#"module Cs
  module Outside
    extend Simulated

    #: -> String
    def self.created = TIME_BEFORE
  end
end
"#;

#[test]
fn a_singleton_body_reads_an_extended_module_constant() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/cs/simulated.rb", SIMULATED),
        ("app/models/cs/contact.rb", CONTACT),
        ("app/models/cs/outside.rb", OUTSIDE),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    let in_file = |file: &str, name: &str| errors.iter().any(|e| e.contains(file) && e.contains(name));
    assert!(!in_file("contact.rb", "TIME_BEFORE"), "{errors:?}");
    assert!(!in_file("contact.rb", "`ID`") && !in_file("contact.rb", ": ID"), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("constant not supported") && e.contains("TIME_BEFORE")), "{errors:?}");
}
