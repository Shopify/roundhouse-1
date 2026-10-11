//! `ActiveSupport::HashWithIndifferentAccess` types as the analyzer's
//! indifferent hash class (`analyze::indifferent`), the value
//! `with_indifferent_access` answers.
//!
//! The Ruby family's runtime ports the class (ruby_overlay
//! active_support_core_ext.rb), but the analyzer knew it only as the
//! value of `with_indifferent_access`: `HashWithIndifferentAccess.new(x)`
//! read as `constant not supported`, and a declared
//! `ActiveSupport::HashWithIndifferentAccess` answered no `[]`. The
//! constant (and its top-level alias) and the type name, in RBS and in
//! sorbet, now read as that class.

use std::process::Command;

fn check(name: &str, account: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_indifferent_access_typing_{}_{name}", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/account.rb", account),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn the_constant_and_its_alias_construct_the_indifferent_hash() {
    let account = "class Account < ApplicationRecord\n  #: (Hash[String, untyped]) -> untyped\n  def use(x)\n    a = ActiveSupport::HashWithIndifferentAccess.new(x)\n    b = HashWithIndifferentAccess.new\n    [a.aaa, b.bbb]\n  end\nend\n";
    let text = check("constant", account);
    assert!(!text.contains("constant not supported"), "{text}");
    assert!(text.contains("no known method `aaa` on ActiveSupport::HashWithIndifferentAccess[untyped]"), "{text}");
    assert!(text.contains("no known method `bbb` on ActiveSupport::HashWithIndifferentAccess["), "{text}");
}

#[test]
fn the_declared_type_reads_as_the_indifferent_hash() {
    let account = "class Account < ApplicationRecord\n  extend T::Sig\n\n  #: (ActiveSupport::HashWithIndifferentAccess) -> untyped\n  def rbs(h) = [h[:a], h.zzz]\n\n  sig { params(h: HashWithIndifferentAccess).returns(T.untyped) }\n  def sorbet(h) = [h[\"a\"], h.yyy]\nend\n";
    let text = check("declared", account);
    assert!(!text.contains("no known method `[]`"), "{text}");
    assert!(text.contains("no known method `zzz` on ActiveSupport::HashWithIndifferentAccess[untyped]"), "{text}");
    assert!(text.contains("no known method `yyy` on ActiveSupport::HashWithIndifferentAccess[untyped]"), "{text}");
}
