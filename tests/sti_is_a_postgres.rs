//! On the PostgreSQL CRuby tree `x.is_a?(Rooms::Open)` stays an `is_a?`:
//! the tree hydrates a row as its STI subclass, and the column read the
//! strict targets get (tests/sti_is_a.rs) is a NoMethodError on a
//! receiver that is no STI row. Core's `Payments::Transfer#
//! update_payout_instruction!` tests `ledger.is_a?(ShopifyPayments::
//! Provider::StripeConnect::Account)` on a polymorphic `ledger` that was
//! a `Payments::Account`, which has no `type`: `undefined method 'type'
//! for an instance of Payments::Account` (2 ids, e2e v2-r37).

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name", null: false
    t.string "type"
  end
end
"#;

const ROOM: &str = "class Room < ApplicationRecord\n  def open?\n    is_a?(Rooms::Open)\n  end\nend\n";

fn emitted(with_database_yml: bool) -> String {
    let mut files = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", ROOM),
        ("app/models/rooms/open.rb", "module Rooms\n  class Open < ::Room\n  end\nend\n"),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms\nend\n"),
    ];
    if with_database_yml {
        files.push(("roundhouse.yml", "database_adapter: postgresql\n"));
        files.push(("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"));
    }
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("room.rb"))
        .map(|f| f.content.clone())
        .expect("room.rb")
}

#[test]
fn the_postgresql_tree_keeps_is_a_against_an_sti_subclass() {
    let src = emitted(true);
    assert!(src.contains("is_a?(Rooms::Open)"), "the subclass hydrates, so is_a? answers:\n{src}");
    assert!(!src.contains("type =="), "no inheritance-column read:\n{src}");
}

#[test]
fn a_tree_without_the_postgresql_runtime_still_reads_the_column() {
    let src = emitted(false);
    assert!(src.contains("type == \"Rooms::Open\""), "{src}");
}
