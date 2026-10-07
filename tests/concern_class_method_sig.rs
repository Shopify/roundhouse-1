//! A concern's class-side method keeps its declared type on the includer.
//!
//! `class_methods do #: -> Integer; def cmeth … end` and a nested
//! `module ClassMethods` mixed in by `mixes_in_class_methods(ClassMethods)`
//! (api_versioning's `ApiClient.current`) reach each includer as a copy
//! of the `def`, which the harvest typed from its body: a declared
//! `-> Integer` over `raise` read as `bot`, and the reader never entered
//! the `class_methods` block at all. The includer now takes the
//! declaration, as it does from a module it `extend`s.

use std::process::Command;

fn check(name: &str, extra: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!("rh_concern_class_method_sig_{}_{name}", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        (
            "app/services/tracker.rb",
            "module Tracker\n  extend T::Sig\n  extend T::Helpers\n\n  module ClassMethods\n    extend T::Sig\n\n    sig { returns(Integer) }\n    def current = raise\n  end\n\n  mixes_in_class_methods(ClassMethods)\nend\n",
        ),
        (
            "app/services/conc.rb",
            "module Conc\n  extend ActiveSupport::Concern\n  extend T::Sig\n\n  class_methods do\n    #: -> Integer\n    def cmeth = raise\n\n    sig { returns(String) }\n    def smeth = raise\n  end\nend\n",
        ),
    ];
    for (path, text) in files.iter().chain(extra) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_model_takes_both_declared_class_side_types() {
    let account = "class Account < ApplicationRecord\n  include Tracker\n  include Conc\n\n  #: -> untyped\n  def use = [Account.current.zzz, Account.cmeth.yyy, Account.smeth.xxx]\nend\n";
    let text = check("model", &[("app/models/account.rb", account)]);
    assert!(text.contains("no known method `zzz` on Integer"), "{text}");
    assert!(text.contains("no known method `yyy` on Integer"), "{text}");
    assert!(text.contains("no known method `xxx` on String"), "{text}");
    assert!(!text.contains("on bot"), "{text}");
}

#[test]
fn a_library_class_takes_them_too() {
    let plain = "class Plain\n  include Tracker\n  include Conc\nend\n";
    let account = "class Account < ApplicationRecord\n  #: -> untyped\n  def use = [Plain.current.zzz, Plain.cmeth.yyy]\nend\n";
    let text = check("library", &[("app/services/plain.rb", plain), ("app/models/account.rb", account)]);
    assert!(text.contains("no known method `zzz` on Integer"), "{text}");
    assert!(text.contains("no known method `yyy` on Integer"), "{text}");
    assert!(!text.contains("on bot"), "{text}");
}
