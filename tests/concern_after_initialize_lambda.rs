//! Core's Identity::UuidEntitySyncable fills `uuid` in an `included do
//! after_initialize -> { self.uuid ||= SecureRandom.uuid }, if: :new_record?`
//! hook; a model including it must run that default before validation.

use roundhouse::det::HashMap;
use roundhouse::emit::ruby::emit_lowered_models;
use std::path::PathBuf;

const SCHEMA: &str = r#"
ActiveRecord::Schema.define(version: 1) do
  create_table "destinations", force: :cascade do |t|
    t.string "uuid"
  end
end
"#;

#[test]
fn a_concern_after_initialize_lambda_reaches_the_including_model() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        (
            "app/models/concerns/uuid_syncable.rb",
            "module UuidSyncable\n  extend ActiveSupport::Concern\n\n  included do\n    after_initialize -> { self.uuid ||= SecureRandom.uuid }, if: :new_record?\n\n    validates :uuid, presence: true\n  end\nend\n",
        ),
        ("app/models/destination.rb", "class Destination < ApplicationRecord\n  include UuidSyncable\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let out = emit_lowered_models(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n");
    assert!(out.contains("self.uuid ||= SecureRandom.uuid if new_record?"), "{out}");
    // Construction and hydration reach the hook.
    assert!(out.contains("after_initialize if !(attrs.equal? ActiveRecord::Base::HYDRATE_ATTRS)"), "{out}");
    assert!(out.contains("instance.after_initialize"), "{out}");
}

#[test]
fn an_unless_condition_negates_the_guard() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        (
            "app/models/destination.rb",
            "class Destination < ApplicationRecord\n  after_initialize -> { self.uuid ||= \"x\" }, unless: :persisted?\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let out = emit_lowered_models(&app).into_iter().map(|f| f.content).collect::<Vec<_>>().join("\n");
    assert!(out.contains("if !(self.persisted?)"), "{out}");
}
