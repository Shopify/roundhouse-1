//! Rails' `ignored_columns`, written in the class body as Core writes it
//! (`= [..]`, `+= [..]`, another model's list), against native Rails 8.2 on
//! the same local PostgreSQL database.
//!
//! Rails leaves an ignored column out of the model: no attribute, no
//! accessor, and neither INSERT nor UPDATE names it, so the database
//! default fills it. `widgets.legacy` is NOT NULL with a default, so a
//! compiled INSERT that still wrote it (as nil) would fail.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    legacy character varying DEFAULT 'db' NOT NULL,
    old_count integer DEFAULT 7 NOT NULL,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
CREATE TABLE gadgets (
    id bigserial NOT NULL,
    label character varying,
    legacy character varying DEFAULT 'gdb' NOT NULL,
    CONSTRAINT gadgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_ignored_columns
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const WIDGET: &str = r#"class Widget < ApplicationRecord
  self.ignored_columns = %w[legacy gone]
  self.ignored_columns += [:old_count]
end
"#;

const GADGET: &str = r#"class Gadget < ApplicationRecord
  self.ignored_columns = ::Widget.ignored_columns
end
"#;

const PROBE: &str = r##"class IgnoredColumnsProbe
  def self.lines
    out = []
    out << "list: #{Widget.ignored_columns.inspect} #{Gadget.ignored_columns.inspect}"
    w = Widget.create!(name: "a")
    out << "attribute: #{w.has_attribute?(:legacy)} #{w.has_attribute?(:name)} #{w.respond_to?(:legacy)} #{w.respond_to?(:old_count=)}"
    out << "insert: #{Widget.connection.select_value("SELECT legacy || '/' || old_count FROM widgets WHERE id = #{w.id}")}"
    Widget.connection.execute("UPDATE widgets SET legacy = 'kept', old_count = 9 WHERE id = #{w.id}")
    w.name = "b"
    w.save!
    out << "update: #{Widget.find(w.id).name} #{Widget.connection.select_value("SELECT legacy || '/' || old_count FROM widgets WHERE id = #{w.id}")}"
    g = Gadget.create!(label: "g")
    out << "gadget: #{Gadget.find(g.id).label} #{Gadget.connection.select_value("SELECT legacy FROM gadgets WHERE id = #{g.id}")}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 5] = [
    r#"list: ["legacy", "gone", "old_count"] ["legacy", "gone", "old_count"]"#,
    "attribute: false true false false",
    "insert: db/7",
    "update: b kept/9",
    "gadget: g gdb",
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] =
    ["app/models/application_record", "app/models/widget", "app/models/gadget", "app/models/ignored_columns_probe"];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/widget.rb", WIDGET),
    ("app/models/gadget.rb", GADGET),
    ("app/models/ignored_columns_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. The writes run as written at class load
/// (the runtime's `ignored_columns=`).
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the ignored columns leave the compiled table and
/// the writes stay in the class body.
#[test]
fn ignored_columns_leave_the_compiled_table() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let widget = std::fs::read_to_string(emitted.join("app/models/widget.rb")).unwrap();
    for expected in [
        r#"["legacy", "gone", "old_count"]"#,
        "  self.ignored_columns = %w[legacy gone]\n",
        // `+=` emits expanded.
        "  self.ignored_columns = self.ignored_columns + [:old_count]\n",
    ] {
        assert!(widget.contains(expected), "missing {expected:?} in:\n{widget}");
    }
    for absent in ["\"legacy\" =", "old_count ="] {
        assert!(!widget.contains(absent), "unexpected {absent:?} in:\n{widget}");
    }
    let gadget = std::fs::read_to_string(emitted.join("app/models/gadget.rb")).unwrap();
    assert!(gadget.contains("  self.ignored_columns = ::Widget.ignored_columns\n"), "{gadget}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn ignored_columns_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted) = pg_probe::native_and_emitted(
        "ignored-columns",
        &FILES,
        &emitted,
        "rh_e2e_ignored_columns",
        "IgnoredColumnsProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
