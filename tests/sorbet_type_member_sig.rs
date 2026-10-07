//! A `sig` naming a generic's type parameter reads it as the type the
//! parameter may be.
//!
//! shopify-adt's `ResultWrapper` declares `OkType = type_member(:out)`
//! and signs `sig { returns(OkType) }`; the parameter was dropped, so
//! the sig named a class `OkType` with no methods and every call on the
//! value was refused (`no known method … on OkType`). A parameter is now
//! `T.untyped` (gradual), or its `fixed:` type when it declares one. The
//! emitted class carries no constant for it.

use std::process::Command;

fn tree(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rh_sorbet_type_member_sig_{}_{name}", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n"),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/account.rb", "class Account < ApplicationRecord\n  #: (Wrapper w) -> untyped\n  def use(w) = [w.value.anything, w.count.nope, w.other.zzz]\nend\n"),
        (
            "app/services/wrapper.rb",
            "class Wrapper\n  extend T::Sig\n  extend T::Generic\n\n  OkType = type_member(:out)\n  Count = type_member { { fixed: Integer } }\n\n  sig { returns(OkType) }\n  def value = raise\n\n  sig { returns(Count) }\n  def count = 1\n\n  sig { returns(Wrapper) }\n  def other = self\nend\n",
        ),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    root
}

#[test]
fn a_type_member_is_untyped_and_a_fixed_one_is_its_type() {
    let root = tree("check");
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!text.contains("on OkType"), "{text}");
    assert!(text.contains("no known method `nope` on Integer"), "{text}");
    assert!(text.contains("no known method `zzz` on Wrapper"), "{text}");
}

#[test]
fn the_emitted_class_carries_no_type_parameter() {
    let root = tree("emit");
    let out = root.join("out");
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby"])
        .arg(&root)
        .arg("-o")
        .arg(&out)
        .arg("--allow-unsupported")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let wrapper = std::fs::read_to_string(out.join("app/models/wrapper.rb")).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(!wrapper.contains("OkType") && !wrapper.contains("type_member"), "{wrapper}");
}
