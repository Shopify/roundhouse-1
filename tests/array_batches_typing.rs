//! `each_slice(n)` / `each_cons(n)` yield same-element sub-arrays.
//!
//! Core batches ids and records through them, often chaining the
//! enumerator (`missing.each_slice(500).flat_map { |batch| … }`). The
//! analyzer knew neither, so every such site read as `no known method`.

use std::process::Command;

fn check(name: &str, body: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_array_batches_typing_{}_{name}", std::process::id()));
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n".to_string()),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("app/models/account.rb", format!("class Account < ApplicationRecord\n  #: (Array[Integer]) -> untyped\n  def use(ids)\n{body}\n  end\nend\n")),
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
fn the_block_takes_a_batch() {
    let text = check("block", "    ids.each_slice(2) { |batch| batch.zzz }\n    ids.each_cons(2) { |pair| pair.yyy }");
    assert!(text.contains("no known method `zzz` on Array[Integer]"), "{text}");
    assert!(text.contains("no known method `yyy` on Array[Integer]"), "{text}");
    assert!(!text.contains("no known method `each_slice`"), "{text}");
    assert!(!text.contains("no known method `each_cons`"), "{text}");
}

#[test]
fn a_chained_enumerator_yields_batches() {
    let text = check("chain", "    ids.each_slice(2).flat_map { |batch| batch.xxx }");
    assert!(text.contains("no known method `xxx` on Array[Integer]"), "{text}");
    assert!(!text.contains("no known method `each_slice`"), "{text}");
}
