//! An app macro that `class_eval`s a `def` named after its argument.
//!
//! Core's `trusted_id_attribute(attr_name, ...)` (gems/component) runs
//! `class_eval(<<-RUBY)` with `def trusted_#{attr_name}`, so Shop's
//! `trusted_id_attribute :id` defines `trusted_id`, and every
//! `shop.trusted_id` was refused. The method now types as gradual on the
//! model calling the macro; the macro call still runs.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(macro_source: &str) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_class_eval_def_macro_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let macro_module = format!("module TrustedIds\n  def trusted_id_attribute(attr_name)\n    class_eval(<<-RUBY, __FILE__, __LINE__ + 1)\n{macro_source}\n    RUBY\n  end\nend\n");
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n".to_string()),
        ("db/structure.sql", "CREATE TABLE core.shops (\n    id bigint NOT NULL\n);\n".to_string()),
        ("lib/trusted_ids.rb", macro_module),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n  extend TrustedIds\nend\n".to_string()),
        ("app/models/shop.rb", "class Shop < ApplicationRecord\n  trusted_id_attribute :id\n\n  #: (Shop) -> String\n  def self.tid(shop) = shop.trusted_id.to_s\nend\n".to_string()),
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

#[test]
fn the_evaluated_def_named_after_the_argument_types() {
    let (ok, text) = check("      def trusted_#{attr_name}\n        #{attr_name}\n      end");
    assert!(ok, "{text}");
}

#[test]
fn an_evaluated_def_with_a_fixed_name_defines_nothing_per_argument() {
    let (ok, text) = check("      def trusted_value\n        #{attr_name}\n      end");
    assert!(!ok, "{text}");
    assert!(text.contains("no known method `trusted_id` on Shop"), "{text}");
}

#[test]
fn an_evaluated_class_method_is_not_an_instance_method() {
    let (ok, text) = check("      def self.trusted_#{attr_name}\n        1\n      end");
    assert!(!ok, "{text}");
    assert!(text.contains("no known method `trusted_id` on Shop"), "{text}");
}
