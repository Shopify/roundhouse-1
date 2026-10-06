//! type_toolkit's `x.not_nil!` answers the receiver with nil removed.
//! Analysis types it so; which trees can run it is `project`'s refusal
//! (see `tests/not_nil_type_toolkit.rs`, which runs the emitted call).

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{diagnose, Analyzer};
use roundhouse::ingest::ingest_app_from_tree;

fn failures(controller_body: &str) -> Vec<String> {
    let files: Vec<(&str, String)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"users\", force: :cascade do |t|\n    t.string \"name\", null: false\n  end\nend\n".into(),
        ),
        ("app/models/user.rb", "class User < ApplicationRecord\nend\n".into()),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n".into(),
        ),
        (
            "app/controllers/users_controller.rb",
            format!("class UsersController < ApplicationController\n{controller_body}\nend\n"),
        ),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  resources :users\nend\n".into(),
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.into_bytes())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .iter()
        .filter(|d| d.code() == "send_dispatch_failed" || d.code() == "unsupported")
        .map(|d| format!("{d:?}"))
        .collect()
}

#[test]
fn not_nil_rules_nil_out_of_the_receiver() {
    // `name.upcase` on a `String?` would fail dispatch; with nil ruled
    // out of `User?` the chain types clean.
    let f = failures("  def show\n    name = User.find_by(name: \"a\").not_nil!.name\n    name.upcase\n  end");
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn not_nil_still_checks_the_stripped_type() {
    let f = failures("  def show\n    User.find_by(name: \"a\").not_nil!.frobnicate\n  end");
    assert!(
        f.iter().any(|d| d.contains("frobnicate") && d.contains("ClassId(Symbol(\"User\"))")),
        "{f:?}"
    );
}

/// A receiver whose non-nil arm is itself un-inferred (`Var`) stays
/// gradual, the way union dispatch treats a `Var` arm, rather than
/// reporting `not_nil!` on `untyped?`; the unresolved name is still
/// the error it was.
#[test]
fn unknown_receivers_do_not_certify_a_non_nil_assertion() {
    let f = failures("  def show\n    x = UnknownThing.lookup(1) || nil\n    x.not_nil!.anything\n  end");
    assert!(f.iter().any(|m| m.contains("UnknownThing")), "{f:?}");
    assert!(!f.iter().any(|m| m.contains("not_nil")), "{f:?}");
}
