//! A structure.sql dump plus the migrations it does not record as run.
//!
//! Native test setup loads the dump, then `db:migrate` runs every
//! migration whose version the dump's `schema_migrations` INSERT does not
//! list. Core's GlobalDB dump stops at a 2026-08-14 rollup and 972 later
//! migrations run on top of it; reading the dump alone left the tables
//! and columns they add out of the typing schema. A recorded migration is
//! already in the dump and is not replayed; a dump that records no
//! versions is taken as complete.

use roundhouse::det::HashMap;
use std::path::PathBuf;

const DUMP: &str = "CREATE TABLE widgets (\n    id bigint NOT NULL\n);\n\nCREATE TABLE gears (\n    id bigint NOT NULL\n);\n\nINSERT INTO \"schema_migrations\" (version) VALUES\n('20260101000000'),\n('20260102000000');\n";

fn ingest(dump: &str) -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/structure.sql", dump),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        // Recorded: its table is in the dump; replaying it would duplicate.
        ("db/migrate/20260101000000_create_widgets.rb", "class CreateWidgets < ActiveRecord::Migration[7.1]\n  def change\n    create_table :widgets\n  end\nend\n"),
        // Pending: run after the dump.
        ("db/migrate/20260201000000_create_sprockets.rb", "class CreateSprockets < ActiveRecord::Migration[7.1]\n  def change\n    create_table :sprockets do |t|\n      t.string :name, null: false\n    end\n  end\nend\n"),
        ("db/migrate/20260202000000_add_color_to_widgets.rb", "class AddColorToWidgets < ActiveRecord::Migration[7.1]\n  def change\n    add_column :widgets, :color, :string\n  end\nend\n"),
        ("db/migrate/20260203000000_drop_gears.rb", "class DropGears < ActiveRecord::Migration[7.1]\n  def change\n    drop_table :gears\n  end\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree")
}

fn columns(app: &roundhouse::App, table: &str) -> Option<Vec<String>> {
    app.schema
        .tables
        .iter()
        .find(|(name, _)| name.as_str() == table)
        .map(|(_, t)| t.columns.iter().map(|c| c.name.as_str().to_string()).collect())
}

#[test]
fn pending_migrations_fold_onto_the_dump() {
    let app = ingest(DUMP);
    assert_eq!(columns(&app, "widgets"), Some(vec!["id".to_string(), "color".to_string()]));
    assert_eq!(columns(&app, "sprockets"), Some(vec!["id".to_string(), "name".to_string()]));
    assert_eq!(columns(&app, "gears"), None, "a pending drop_table removes the dumped table");
}

#[test]
fn a_dump_without_recorded_versions_is_complete() {
    let dump = DUMP.split("INSERT INTO").next().unwrap();
    let app = ingest(dump);
    assert_eq!(columns(&app, "widgets"), Some(vec!["id".to_string()]));
    assert_eq!(columns(&app, "sprockets"), None);
    assert!(columns(&app, "gears").is_some());
}
