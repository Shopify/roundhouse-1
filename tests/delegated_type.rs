//! `delegated_type :role, types: …` as a general Active Record pattern.
//! Documented options (`types:` as literal or compile-time constant /
//! concern-constant, `dependent: :destroy`, `foreign_key`,
//! `foreign_type`, `primary_key`, `optional:`) — not a named-app spelling.

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::dialect::{Association, MethodReceiver, ModelBodyItem};
use roundhouse::ingest::ingest_app_from_tree;

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn instance_names(app: &roundhouse::App, model: &str) -> Vec<String> {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == model)
        .expect(model)
        .methods()
        .filter(|m| m.receiver == MethodReceiver::Instance)
        .map(|m| m.name.as_str().to_string())
        .collect()
}

fn class_names(app: &roundhouse::App, model: &str) -> Vec<String> {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == model)
        .expect(model)
        .methods()
        .filter(|m| m.receiver == MethodReceiver::Class)
        .map(|m| m.name.as_str().to_string())
        .collect()
}

fn scopes(app: &roundhouse::App, model: &str) -> Vec<String> {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == model)
        .expect(model)
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Scope { scope, .. } => Some(scope.name.as_str().to_string()),
            _ => None,
        })
        .collect()
}

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table :entries do |t|
    t.string :entryable_type
    t.integer :entryable_id
  end
  create_table :messages do |t|
    t.string :subject
  end
  create_table :comments do |t|
    t.string :content
  end
end
"#;

fn entry_app(entry_body: &str) -> roundhouse::App {
    ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        (
            "app/models/entry.rb",
            &format!("class Entry < ApplicationRecord\n{entry_body}\nend\n"),
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
        (
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
    ]))
    .expect("ingest")
}

/// `%w[ Message Comment ]` — the documented types list: predicates,
/// scopes, typed readers, and `entryable_types`.
#[test]
fn word_array_types_ingest_convenience_names() {
    let app = entry_app("  delegated_type :entryable, types: %w[ Message Comment ]\n");
    let inst = instance_names(&app, "Entry");
    for name in ["message?", "comment?", "message", "comment", "message_id", "comment_id", "entryable_class", "entryable_name"]
    {
        assert!(inst.iter().any(|n| n == name), "{name} missing from {inst:?}");
    }
    let classes = class_names(&app, "Entry");
    assert!(classes.iter().any(|n| n == "entryable_types"), "{classes:?}");
    let sc = scopes(&app, "Entry");
    assert!(sc.iter().any(|n| n == "messages"), "{sc:?}");
    assert!(sc.iter().any(|n| n == "comments"), "{sc:?}");
    let entry = app.models.iter().find(|m| m.name.0.as_str() == "Entry").unwrap();
    let assoc = entry.associations().next().expect("belongs_to");
    let Association::BelongsTo {
        polymorphic,
        polymorphic_targets,
        foreign_key,
        ..
    } = assoc
    else {
        panic!("{assoc:?}");
    };
    assert!(polymorphic);
    assert_eq!(foreign_key.as_str(), "entryable_id");
    let names: Vec<&str> = polymorphic_targets.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, vec!["Message", "Comment"]);
}

/// Symbol-array and bracket-string `types:` are the same list.
#[test]
fn symbol_and_bracket_types_lists_ingest() {
    for types in ["%i[Message Comment]", "[\"Message\", \"Comment\"]"] {
        let app = entry_app(&format!("  delegated_type :entryable, types: {types}\n"));
        let inst = instance_names(&app, "Entry");
        assert!(inst.iter().any(|n| n == "message?"), "{types}: {inst:?}");
    }
}

/// Namespaced types tableize with `/` folded to `_`, as Rails documents.
#[test]
fn namespaced_types_ingest_underscored_names() {
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table :entries do |t|\n    t.string :entryable_type\n    t.integer :entryable_id\n  end\n  create_table :access_notice_messages do |t|\n    t.string :body\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: %w[ Access::NoticeMessage ]\nend\n",
        ),
        (
            "app/models/access/notice_message.rb",
            "module Access\n  class NoticeMessage < ApplicationRecord\n    has_one :entry, as: :entryable\n  end\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "access_notice_message?"), "{inst:?}");
    assert!(inst.iter().any(|n| n == "access_notice_message"), "{inst:?}");
    let sc = scopes(&app, "Entry");
    assert!(sc.iter().any(|n| n == "access_notice_messages"), "{sc:?}");
}

/// `foreign_key` / `foreign_type` / `primary_key` rename the convenience
/// methods the way the Rails API documents.
#[test]
fn foreign_key_type_and_primary_key_ingest() {
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table :entries do |t|\n    t.string :kind\n    t.string :entryable_uuid\n  end\n  create_table :messages do |t|\n    t.string :uuid\n    t.string :subject\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: %w[ Message ], primary_key: :uuid, foreign_key: :entryable_uuid, foreign_type: :kind\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "message_uuid"), "{inst:?}");
    assert!(!inst.iter().any(|n| n == "message_id"), "{inst:?}");
    let assoc = app
        .models
        .iter()
        .find(|m| m.name.0.as_str() == "Entry")
        .unwrap()
        .associations()
        .next()
        .unwrap();
    let Association::BelongsTo {
        foreign_key,
        foreign_type,
        primary_key,
        ..
    } = assoc
    else {
        panic!("{assoc:?}");
    };
    assert_eq!(foreign_key.as_str(), "entryable_uuid");
    assert_eq!(foreign_type.as_ref().map(|s| s.as_str()), Some("kind"));
    assert_eq!(primary_key.as_ref().map(|s| s.as_str()), Some("uuid"));
}

/// `optional:` and `dependent: :destroy` parse onto the belongs_to /
/// before_destroy expansion.
#[test]
fn optional_and_dependent_destroy_ingest() {
    let app = entry_app(
        "  delegated_type :entryable, types: %w[ Message Comment ], optional: true, dependent: :destroy\n",
    );
    let entry = app.models.iter().find(|m| m.name.0.as_str() == "Entry").unwrap();
    let Association::BelongsTo { optional, .. } = entry.associations().next().unwrap() else {
        panic!("belongs_to");
    };
    assert!(optional);
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "destroy_entryable"), "{inst:?}");
    assert!(entry.body.iter().any(|item| matches!(
        item,
        ModelBodyItem::Callback { callback, .. }
            if callback.targets.iter().any(|t| t.as_str() == "destroy_entryable")
    )));
}

/// A concern `included do` carries the same expansion onto the includer.
#[test]
fn included_do_delegated_type_ingests() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        (
            "app/models/concerns/entryable_owner.rb",
            "module EntryableOwner\n  extend ActiveSupport::Concern\n  included do\n    delegated_type :entryable, types: %w[ Message Comment ]\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  include EntryableOwner\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
        (
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "message?"), "{inst:?}");
}

/// Class-local `TYPES = %w[…]` then `types: TYPES` — compile-time constant.
#[test]
fn class_local_types_constant_ingests() {
    let app = entry_app(
        "  TYPES = %w[ Message Comment ]\n  delegated_type :entryable, types: TYPES\n",
    );
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "message?"), "{inst:?}");
    assert!(inst.iter().any(|n| n == "comment?"), "{inst:?}");
    let sc = scopes(&app, "Entry");
    assert!(sc.iter().any(|n| n == "messages"), "{sc:?}");
}

/// Documented freeze / array spellings for the same constant fold.
#[test]
fn frozen_and_string_array_types_constants_ingest() {
    for body in [
        "  TYPES = %w[ Message Comment ].freeze\n  delegated_type :entryable, types: TYPES\n",
        "  TYPES = %w[ Message Comment ]\n  delegated_type :entryable, types: TYPES.freeze\n",
        "  TYPES = [\"Message\", \"Comment\"]\n  delegated_type :entryable, types: TYPES\n",
        "  TYPES = %i[Message Comment]\n  delegated_type :entryable, types: TYPES\n",
    ] {
        let app = entry_app(body);
        let inst = instance_names(&app, "Entry");
        assert!(
            inst.iter().any(|n| n == "message?") && inst.iter().any(|n| n == "comment?"),
            "failed for {body:?}: {inst:?}"
        );
    }
}

/// Concern-module `%i[…]` constant — EnumConstants must fold symbols
/// the same way class-local `enum_label_values` does for `%w` / `%i`.
#[test]
fn concern_percent_i_types_constant_ingests() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        (
            "app/models/entryable.rb",
            "module Entryable\n  extend ActiveSupport::Concern\n  TYPES = %i[Message Comment]\n  included do\n    has_one :entry, as: :entryable\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: Entryable::TYPES, dependent: :destroy\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  include Entryable\nend\n",
        ),
        (
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  include Entryable\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    for name in ["message?", "comment?", "destroy_entryable"] {
        assert!(inst.iter().any(|n| n == name), "{name} missing from {inst:?}");
    }
}

/// Concern-module constant `types: Entryable::TYPES`.
#[test]
fn concern_constant_types_ingests() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        (
            "app/models/entryable.rb",
            "module Entryable\n  extend ActiveSupport::Concern\n  TYPES = %w[ Message Comment ]\n  included do\n    has_one :entry, as: :entryable\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: Entryable::TYPES, dependent: :destroy\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  include Entryable\nend\n",
        ),
        (
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  include Entryable\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    for name in ["message?", "comment?", "message", "comment", "entryable_class", "destroy_entryable"] {
        assert!(inst.iter().any(|n| n == name), "{name} missing from {inst:?}");
    }
    let sc = scopes(&app, "Entry");
    assert!(sc.iter().any(|n| n == "messages"), "{sc:?}");
    assert!(sc.iter().any(|n| n == "comments"), "{sc:?}");
}

/// `included do` with bare / qualified concern constant `types:`.
#[test]
fn included_do_types_constant_ingests() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", SCHEMA),
        (
            "app/models/concerns/entryable_owner.rb",
            "module EntryableOwner\n  extend ActiveSupport::Concern\n  TYPES = %w[ Message Comment ]\n  included do\n    delegated_type :entryable, types: TYPES\n  end\nend\n",
        ),
        (
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  include EntryableOwner\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
        (
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  has_one :entry, as: :entryable\nend\n",
        ),
    ]))
    .expect("ingest");
    let inst = instance_names(&app, "Entry");
    assert!(inst.iter().any(|n| n == "message?"), "{inst:?}");
}

/// Unresolvable or non-class-name constants fail closed.
#[test]
fn unresolvable_or_non_class_types_constant_stays_unexpanded() {
    let missing = entry_app("  delegated_type :entryable, types: Missing::TYPES\n");
    let inst = instance_names(&missing, "Entry");
    assert!(!inst.iter().any(|n| n == "message?"), "{inst:?}");

    let enumish = entry_app(
        "  STATES = %w[ draft active ]\n  delegated_type :entryable, types: STATES\n",
    );
    let inst = instance_names(&enumish, "Entry");
    assert!(
        !inst.iter().any(|n| n == "draft?"),
        "enum-style labels must not expand as delegated types: {inst:?}"
    );
}

/// `dependent: :nullify` (and other non-destroy values) stay unexpanded
/// so the unsupported ledger remains honest.
#[test]
fn other_dependent_stays_unexpanded() {
    let app = entry_app(
        "  delegated_type :entryable, types: %w[ Message Comment ], dependent: :nullify\n",
    );
    let inst = instance_names(&app, "Entry");
    assert!(!inst.iter().any(|n| n == "message?"), "{inst:?}");
    assert!(!inst.iter().any(|n| n == "destroy_entryable"), "{inst:?}");
}

/// Unmodeled belongs_to kwargs must not expand: dropping `class_name` /
/// `autosave` would change save/type behavior with no ledger entry.
#[test]
fn unmodeled_belongs_to_kwargs_stay_unexpanded() {
    for extra in ["class_name: \"Thing\"", "autosave: true", "unknown: true"] {
        let app = entry_app(&format!(
            "  delegated_type :entryable, types: %w[ Message Comment ], {extra}\n"
        ));
        let inst = instance_names(&app, "Entry");
        assert!(
            !inst.iter().any(|n| n == "message?"),
            "{extra} still expanded: {inst:?}"
        );
    }
}

fn runtime_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        )
        .write(
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        )
        .write(
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get \"/entries\", to: \"entries#index\"\nend\n",
        )
        .write(
            "app/controllers/entries_controller.rb",
            "class EntriesController < ApplicationController\n  def index\n    render plain: Entry.count.to_s\n  end\nend\n",
        )
}

#[test]
fn emitted_word_array_dependent_destroy_runs() {
    runtime_app()
        .write(
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "entries", force: :cascade do |t|
    t.string "entryable_type"
    t.integer "entryable_id"
  end
  create_table "messages", force: :cascade do |t|
    t.string "subject"
  end
  create_table "comments", force: :cascade do |t|
    t.string "content"
  end
end
"#,
        )
        .write(
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: %w[ Message Comment ], dependent: :destroy\nend\n",
        )
        .write(
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable, dependent: :destroy\nend\n",
        )
        .write(
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  has_one :entry, as: :entryable, dependent: :destroy\nend\n",
        )
        .run_ruby(
            r#"
msg = Message.create!(subject: "hello")
entry = Entry.create!(entryable: msg)
raise "predicate" unless entry.message?
raise "typed reader" unless entry.message.subject == "hello"
raise "id" unless entry.message_id == msg.id
raise "comment?" if entry.comment?
raise "class" unless entry.entryable_class == Message
raise "name" unless entry.entryable_name == "message"
raise "types" unless Entry.entryable_types == ["Message", "Comment"]
raise "scope" unless Entry.messages.where(id: entry.id).exists?
c = Comment.create!(content: "hi")
entry.entryable = c
raise "reassign" unless entry.comment?
raise "comment reader" unless entry.comment.content == "hi"
entry.destroy
raise "dependent destroy" if Comment.find_by(id: c.id)
orphan = Entry.new
raise "unset role" unless orphan.entryable.nil?
puts "delegated_type word-array dependent destroy passed"
"#,
        )
        .assert_passes();
}

#[test]
fn emitted_optional_and_namespaced_type_runs() {
    runtime_app()
        .write(
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "entries", force: :cascade do |t|
    t.string "entryable_type"
    t.integer "entryable_id"
  end
  create_table "access_notice_messages", force: :cascade do |t|
    t.string "body"
  end
end
"#,
        )
        .write(
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: %w[ Access::NoticeMessage ], optional: true\nend\n",
        )
        .write(
            "app/models/access/notice_message.rb",
            "module Access\n  class NoticeMessage < ApplicationRecord\n    self.table_name = \"access_notice_messages\"\n    has_one :entry, as: :entryable\n  end\nend\n",
        )
        .run_ruby(
            r#"
blank = Entry.create!
raise "optional" unless blank.entryable.nil?
n = Access::NoticeMessage.create!(body: "ping")
e = Entry.create!(entryable: n)
raise "ns predicate" unless e.access_notice_message?
raise "ns reader" unless e.access_notice_message.body == "ping"
raise "ns scope" unless Entry.access_notice_messages.where(id: e.id).exists?
raise "ns name" unless e.entryable_name == "access_notice_message"
puts "delegated_type namespaced optional passed"
"#,
        )
        .assert_passes();
}

#[test]
fn emitted_concern_constant_types_runs() {
    runtime_app()
        .write(
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "entries", force: :cascade do |t|
    t.string "entryable_type"
    t.integer "entryable_id"
  end
  create_table "messages", force: :cascade do |t|
    t.string "subject"
  end
  create_table "comments", force: :cascade do |t|
    t.string "content"
  end
end
"#,
        )
        .write(
            "app/models/entryable.rb",
            "module Entryable\n  extend ActiveSupport::Concern\n  TYPES = %w[ Message Comment ]\n  included do\n    has_one :entry, as: :entryable, dependent: :destroy\n  end\nend\n",
        )
        .write(
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: Entryable::TYPES, dependent: :destroy\nend\n",
        )
        .write(
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  include Entryable\nend\n",
        )
        .write(
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  include Entryable\nend\n",
        )
        .run_ruby(
            r#"
msg = Message.create!(subject: "hello")
entry = Entry.create!(entryable: msg)
raise "predicate" unless entry.message?
raise "typed reader" unless entry.message.subject == "hello"
raise "types" unless Entry.entryable_types == ["Message", "Comment"]
raise "scope" unless Entry.messages.where(id: entry.id).exists?
c = Comment.create!(content: "hi")
entry.entryable = c
raise "reassign" unless entry.comment?
entry.destroy
raise "dependent destroy" if Comment.find_by(id: c.id)
puts "delegated_type concern-constant types passed"
"#,
        )
        .assert_passes();
}

#[test]
fn emitted_foreign_key_type_and_primary_key_runs() {
    runtime_app()
        .write(
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "entries", force: :cascade do |t|
    t.string "kind"
    t.string "entryable_uuid"
  end
  create_table "messages", force: :cascade do |t|
    t.string "uuid"
    t.string "subject"
  end
end
"#,
        )
        .write(
            "app/models/entry.rb",
            "class Entry < ApplicationRecord\n  delegated_type :entryable, types: %w[ Message ], primary_key: :uuid, foreign_key: :entryable_uuid, foreign_type: :kind\nend\n",
        )
        .write(
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  has_one :entry, as: :entryable, foreign_key: :entryable_uuid\nend\n",
        )
        .run_ruby(
            r#"
msg = Message.create!(uuid: "m-1", subject: "keyed")
entry = Entry.create!(entryable: msg)
raise "kind" unless entry.kind == "Message"
raise "fk" unless entry.entryable_uuid == "m-1"
raise "convenience" unless entry.message_uuid == "m-1"
raise "reader" unless entry.message.subject == "keyed"
raise "predicate" unless entry.message?
puts "delegated_type foreign_key foreign_type primary_key passed"
"#,
        )
        .assert_passes();
}

