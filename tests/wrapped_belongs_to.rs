//! An app macro that declares its association through `belongs_to`.
//!
//! Core's `belongs_to_with_query_constraints(name, ...)` (a polyfill
//! for Rails' `query_constraints` option) runs `belongs_to(name, ...)`.
//! Its associations had no reader, so `order` in OrderRisk resolved to
//! the AR `order` scope and `persisted?` was refused on a Relation.
//! The reader is typed like a `belongs_to`; the macro call itself is
//! refused (it would run `belongs_to` at class-body time, which the
//! lowered model declares statically).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(macro_body: &str, model: &str) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_wrapped_belongs_to_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let concern = format!(
        "module QueryConstrained\n  extend ActiveSupport::Concern\n\n  module ClassMethods\n    #: (Symbol, ?Proc?, foreign_key: Symbol, **untyped) -> void\n    def belongs_with_keys(name, scope = nil, foreign_key:, **options)\n      {macro_body}\n    end\n  end\nend\n"
    );
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n".to_string()),
        ("app/models/concerns/query_constrained.rb", concern),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n  include QueryConstrained\nend\n".to_string()),
        ("app/models/order.rb", "class Order < ApplicationRecord\nend\n".to_string()),
        ("app/models/risk.rb", model.to_string()),
    ];
    for (path, text) in &files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

const FORWARDS: &str = "belongs_to(name, scope, foreign_key: foreign_key, optional: true, **options)";

const RISK: &str = r#"class Risk < ApplicationRecord
  belongs_with_keys :order, foreign_key: :order_id
  belongs_with_keys :buyer, foreign_key: :buyer_id, class_name: "Order"

  #: () -> void
  def touch_order
    current = order
    return unless current&.persisted?

    current.touch
    buyer&.touch
  end
end
"#;

#[test]
fn a_macro_forwarding_its_name_to_belongs_to_types_the_reader() {
    let (_ok, text) = check(FORWARDS, RISK);
    assert!(!text.contains("Relation[Risk]"), "{text}");
    // The macro reaches `belongs_to`, which the runtime base does not
    // answer at class-body time, so its call is refused rather than
    // replayed; those refusals are the only errors.
    assert!(text.contains(" 2 error(s)"), "{text}");
    assert_eq!(text.matches("`belongs_with_keys` reaches `belongs_to`").count(), 2, "{text}");
}

#[test]
fn a_macro_declaring_some_other_association_types_nothing() {
    let (ok, text) = check("belongs_to(:account, scope, foreign_key: foreign_key)", RISK);
    assert!(!ok, "{text}");
    assert!(text.contains("no known method `persisted?` on Relation[Risk]"), "{text}");
}

#[test]
fn a_polymorphic_one_has_no_single_target() {
    let model = "class Risk < ApplicationRecord\n  belongs_with_keys :order, foreign_key: :order_id, polymorphic: true\n\n  #: () -> bool\n  def placed? = order&.persisted? || false\nend\n";
    let (ok, text) = check(FORWARDS, model);
    assert!(!ok, "{text}");
    assert!(text.contains("Relation[Risk]"), "{text}");
}
