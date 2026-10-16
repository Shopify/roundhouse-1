//! Relation SQL forms Core's Channels / Timeline / Merchandising / Retail
//! tests reach, each compared with native Rails 8.2 on the same local
//! PostgreSQL database:
//!
//! * `joins(links: :owner_group)` built in one method and `where.not(
//!   owner_group: { ... })` applied in another: Rails aliases the joined
//!   `groups` table to the hop's association name wherever it sits in the
//!   join, so the where's qualifier resolves (`missing FROM-clause entry
//!   for table "owner_group"`);
//! * `order` / `reorder` take an Array of Arel ordering nodes
//!   (`Order "[#<Arel::Nodes::Ascending...>]" is not a column name`);
//! * a has_one whose scope is `select(...).distinct` reads with `LIMIT 1`
//!   and no key order (`for SELECT DISTINCT, ORDER BY expressions must
//!   appear in select list`);
//! * `relation.from_clause`, and `relation.map` with no block, which
//!   `results.map.with_index { ... }` needs.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    kind character varying NOT NULL,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);

CREATE TABLE groups (
    id bigserial NOT NULL,
    kind character varying NOT NULL,
    CONSTRAINT groups_pkey PRIMARY KEY (id)
);

CREATE TABLE links (
    id bigserial NOT NULL,
    widget_id bigint NOT NULL,
    group_id bigint NOT NULL,
    CONSTRAINT links_pkey PRIMARY KEY (id)
);

CREATE TABLE notes (
    id bigserial NOT NULL,
    widget_id bigint NOT NULL,
    body character varying NOT NULL,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_join_alias_order_forms
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const GROUP: &str = "class Group < ApplicationRecord\nend\n";

const LINK: &str = r#"class Link < ApplicationRecord
  belongs_to :widget
  belongs_to :owner_group, class_name: "Group", foreign_key: :group_id
end
"#;

const NOTE: &str = "class Note < ApplicationRecord\n  belongs_to :widget\nend\n";

const WIDGET: &str = r#"class Widget < ApplicationRecord
  has_many :links
  has_one :first_note, -> { select(:widget_id).distinct }, class_name: "Note"

  def self.with_group_ties
    where(id: [1, 2, 3]).joins(links: :owner_group)
  end

  def self.in_other_groups(kinds)
    with_group_ties.where.not(owner_group: { kind: kinds }).distinct
  end
end
"#;

const PROBE: &str = r##"class JoinAliasOrderFormsProbe
  def self.lines
    out = []
    a = Widget.create!(kind: "a")
    b = Widget.create!(kind: "b")
    c = Widget.create!(kind: "c")
    red = Group.create!(kind: "red")
    blue = Group.create!(kind: "blue")
    Link.create!(widget_id: a.id, group_id: red.id)
    Link.create!(widget_id: b.id, group_id: blue.id)
    Link.create!(widget_id: c.id, group_id: red.id)
    Note.create!(widget_id: a.id, body: "x")
    Note.create!(widget_id: a.id, body: "y")

    out << "joined: #{Widget.with_group_ties.order(:id).map(&:kind).inspect}"
    out << "not_red: #{Widget.in_other_groups(["red"]).order(:id).map(&:kind).inspect}"
    out << "not_blue: #{Widget.in_other_groups(["blue"]).order(:id).map(&:kind).inspect}"

    table = Widget.arel_table
    out << "order_array: #{Widget.order([table[:kind].desc, table[:id].asc]).map(&:kind).inspect}"
    out << "order_splat: #{Widget.order(*[table[:kind].desc]).map(&:kind).inspect}"
    out << "reorder_array: #{Widget.order(:kind).reorder([table[:id].desc]).map(&:kind).inspect}"

    out << "first_note: #{a.first_note.widget_id == a.id}"
    out << "no_note: #{b.first_note.inspect}"

    out << "from_none: #{Widget.all.from_clause.value.inspect}"
    out << "from_set: #{Widget.from("widgets").from_clause.value.inspect}"
    out << "map_with_index: #{Widget.order(:id).map.with_index { |w, i| [w.kind, i] }.inspect}"
    out << "map_empty: #{Widget.where(kind: "none").map.with_index { |w, i| [w.kind, i] }.inspect}"
    out
  end
end
"##;

const EXPECTED: [&str; 12] = [
    "joined: [\"a\", \"b\", \"c\"]",
    "not_red: [\"b\"]",
    "not_blue: [\"a\", \"c\"]",
    "order_array: [\"c\", \"b\", \"a\"]",
    "order_splat: [\"c\", \"b\", \"a\"]",
    "reorder_array: [\"c\", \"b\", \"a\"]",
    "first_note: true",
    "no_note: nil",
    "from_none: nil",
    "from_set: \"widgets\"",
    "map_with_index: [[\"a\", 0], [\"b\", 1], [\"c\", 2]]",
    "map_empty: []",
];

const LOADS: [&str; 6] = [
    "app/models/application_record",
    "app/models/group",
    "app/models/link",
    "app/models/note",
    "app/models/widget",
    "app/models/join_alias_order_forms_probe",
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/group.rb", GROUP),
    ("app/models/link.rb", LINK),
    ("app/models/note.rb", NOTE),
    ("app/models/widget.rb", WIDGET),
];

fn files() -> Vec<(&'static str, &'static str)> {
    let mut all = FILES.to_vec();
    all.push(("app/models/join_alias_order_forms_probe.rb", PROBE));
    all
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn join_aliases_order_arrays_and_distinct_has_one_match_native_rails_on_postgresql() {
    let files = files();
    let overlay = files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "join-alias-order-forms",
        &files,
        &emitted,
        "rh_e2e_join_alias_order_forms",
        "JoinAliasOrderFormsProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
