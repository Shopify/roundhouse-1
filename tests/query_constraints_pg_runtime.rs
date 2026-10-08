//! `query_constraints`, called in the class body directly and through an
//! app class macro that reaches it through a local bound to `self` (Core's
//! `yugabyte_query_constraints :shop_id, :id`, which calls
//! `untyped_self.query_constraints(*cols)` when its configuration says
//! so), against native Rails 8.2 on the same local PostgreSQL database.
//!
//! The compiler gives such a model (`Model.query_constraints`) an
//! update, destroy and reload that key on every named column once the
//! call has run (`self.class.query_constraints_list`), as Rails'
//! `_query_constraints_hash` does. `developers` holds two rows with the
//! same `id` under different companies, so keying on `id` alone would
//! update, reload or delete the wrong one.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE developers (
    id bigint NOT NULL,
    company_id bigint NOT NULL,
    name character varying,
    CONSTRAINT developers_pkey PRIMARY KEY (company_id, id)
);
CREATE TABLE notes (
    id bigserial NOT NULL,
    page_id bigint NOT NULL,
    body character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_query_constraints
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.sharded_query_constraints(*columns)
    cols = columns.map(&:to_sym)
    untyped_self = self #: as untyped
    untyped_self.query_constraints(*cols) if sharded?
  end

  def self.sharded?
    true
  end
end
"#;

const DEVELOPER: &str = r#"class Developer < ApplicationRecord
  self.primary_key = :id
  sharded_query_constraints :company_id, :id
end
"#;

const NOTE: &str = r#"class Note < ApplicationRecord
  query_constraints :page_id, :id
end
"#;

const PROBE: &str = r##"class QueryConstraintsProbe
  def self.lines
    out = []
    a = Developer.create!(id: 1, company_id: 10, name: "a")
    b = Developer.create!(id: 1, company_id: 20, name: "b")
    out << "list: #{Developer.query_constraints_list.inspect} #{Developer.has_query_constraints?.inspect}"
    a.name = "a2"
    out << "update: #{a.save} #{Developer.where(company_id: 10).pluck(:name).inspect} #{Developer.where(company_id: 20).pluck(:name).inspect}"
    Developer.where(company_id: 20).update_all(name: "b2")
    Developer.where(company_id: 10).update_all(name: "a3")
    out << "reload: #{a.reload.name} #{b.reload.name}"
    b.destroy
    out << "destroy: #{Developer.count} #{Developer.where(company_id: 10).pluck(:name).inspect}"
    note = Note.create!(page_id: 5, body: "x")
    note.update!(body: "y")
    out << "note: #{Note.find(note.id).body} #{Note.query_constraints_list.inspect}"
    note.destroy
    out << "note destroy: #{Note.count}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    r#"list: ["company_id", "id"] ["company_id", "id"]"#,
    r#"update: true ["a2"] ["b"]"#,
    "reload: a3 b2",
    r#"destroy: 1 ["a3"]"#,
    r#"note: y ["page_id", "id"]"#,
    "note destroy: 0",
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] =
    ["app/models/application_record", "app/models/developer", "app/models/note", "app/models/query_constraints_probe"];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/developer.rb", DEVELOPER),
    ("app/models/note.rb", NOTE),
    ("app/models/query_constraints_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. Both body calls are claimed: they run as
/// written at class load (the runtime's `query_constraints`).
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect()
}

/// The compiler side: both models key update, destroy and reload on
/// every named column once the call has run.
#[test]
fn query_constraints_compile_to_a_composite_where() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let developer = std::fs::read_to_string(emitted.join("app/models/developer.rb")).unwrap();
    for expected in [
        r#"["company_id", "id"]"#,
        "if self.class.query_constraints_list",
        r#"WHERE \"company_id\" = $1 AND \"id\" = $2 LIMIT 1"#,
        "  sharded_query_constraints :company_id, :id\n",
    ] {
        assert!(developer.contains(expected), "missing {expected:?} in:\n{developer}");
    }
    let note = std::fs::read_to_string(emitted.join("app/models/note.rb")).unwrap();
    for expected in [r#"["page_id", "id"]"#, "  query_constraints :page_id, :id\n"] {
        assert!(note.contains(expected), "missing {expected:?} in:\n{note}");
    }
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn query_constraints_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted) = pg_probe::native_and_emitted(
        "query-constraints",
        &FILES,
        &emitted,
        "rh_e2e_query_constraints",
        "QueryConstraintsProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
