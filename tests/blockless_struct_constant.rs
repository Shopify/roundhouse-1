//! A model's `X = Struct.new(:a, keyword_init: true)` constant is a class.
//!
//! Core's `IdentityAccount` declares `IdentityAccountFields =
//! Struct.new(:email, :uuid, …, keyword_init: true)` and signs
//! `#: (String user_uuid) -> IdentityAccountFields`; every `fields.email`
//! and `fields[:uuid]` was refused on a class with no methods. The
//! class now has what the block form gets: `Struct` as parent, `new`, an
//! untyped reader and writer per literal member. A member it does not
//! declare is still refused.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(factory: &str, body: &str) -> (bool, String) {
    let root = std::env::temp_dir().join(format!("rh_blockless_struct_constant_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let model = format!("class Account < ApplicationRecord\n  FIELDS = %i[email uuid].freeze\n  Fields = {factory}\n\n  #: -> Fields\n  def fields = Fields.new(email: \"a\", uuid: \"b\")\n\n  #: -> untyped\n  def read = {body}\nend\n");
    let files = [
        ("roundhouse.yml", "test_paths:\n  - test\n".to_string()),
        ("db/structure.sql", "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("app/models/account.rb", model),
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

const KEYWORD_STRUCT: &str = "Struct.new(:email, :uuid, keyword_init: true)";

#[test]
fn a_member_reader_writer_and_index_dispatch() {
    let (ok, text) = check(KEYWORD_STRUCT, "[fields.email, fields[:uuid], (fields.uuid = \"c\")]");
    assert!(ok, "{text}");
}

#[test]
fn an_undeclared_member_is_refused_on_the_struct_class() {
    let (ok, text) = check(KEYWORD_STRUCT, "fields.phone");
    assert!(!ok, "{text}");
    assert!(text.contains("no known method `phone` on Account::Fields"), "{text}");
}

#[test]
fn splatted_members_declare_no_readers() {
    let (ok, text) = check("Struct.new(*FIELDS, keyword_init: true)", "fields.email");
    assert!(!ok, "{text}");
    assert!(text.contains("no known method `email` on Fields"), "{text}");
}
