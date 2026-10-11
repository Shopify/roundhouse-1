//! `has_many`/`has_one` keys derived from the inverse `belongs_to`.
//!
//! activerecord reflection.rb `derive_foreign_key`: with `inverse_of:` and
//! no `foreign_key:`/`as:`, a has_many/has_one reads its key off the
//! inverse reflection (`inverse_of.foreign_key(infer_from_inverse_of:
//! false)`), not off the owner's class name.

use roundhouse::dialect::Association;
use roundhouse::ingest::ingest_app_from_tree;

fn app_from(files: Vec<(&str, &str)>) -> roundhouse::App {
    let tree = files
        .into_iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    ingest_app_from_tree(tree).expect("ingest tree")
}

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  create_table :widgets do |t|\n  end\n  create_table :attachments do |t|\n    t.integer :widget_id\n    t.integer :target_id\n  end\nend\n";

fn app(widget_body: &str, attachment_body: &str) -> roundhouse::App {
    app_from(vec![
        ("db/schema.rb", SCHEMA),
        (
            "app/models/widget.rb",
            &format!("class Widget < ApplicationRecord\n{widget_body}\nend\n"),
        ),
        (
            "app/models/attachment.rb",
            &format!("class Attachment < ApplicationRecord\n{attachment_body}\nend\n"),
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
    ])
}

fn widget_fk(app: &roundhouse::App) -> String {
    let widget = app.models.iter().find(|m| m.name.0.as_str() == "Widget").unwrap();
    match widget.associations().next().expect("association") {
        Association::HasMany { foreign_key, .. } | Association::HasOne { foreign_key, .. } => {
            foreign_key.as_str().to_string()
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn has_many_takes_its_key_from_the_inverse_belongs_to() {
    let app = app(
        "  has_many :attachments, inverse_of: :widget",
        "  belongs_to :widget, foreign_key: \"target_id\", inverse_of: :attachments",
    );
    assert_eq!(widget_fk(&app), "target_id");
}

#[test]
fn has_one_takes_its_key_from_the_inverse_belongs_to() {
    let app = app(
        "  has_one :attachment, inverse_of: :widget",
        "  belongs_to :widget, foreign_key: :target_id",
    );
    assert_eq!(widget_fk(&app), "target_id");
}

#[test]
fn an_explicit_foreign_key_wins_over_the_inverse() {
    let app = app(
        "  has_many :attachments, foreign_key: :widget_id, inverse_of: :widget",
        "  belongs_to :widget, foreign_key: \"target_id\"",
    );
    assert_eq!(widget_fk(&app), "widget_id");
}

#[test]
fn without_inverse_of_the_key_is_owner_derived() {
    let app = app(
        "  has_many :attachments",
        "  belongs_to :widget, foreign_key: \"target_id\"",
    );
    assert_eq!(widget_fk(&app), "widget_id");
}
