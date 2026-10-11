//! `previous_changes` (ActiveModel::Dirty: `mutations_before_last_save
//! .changes`, the diff of the last save) against native Rails 8.2 on the
//! same local PostgreSQL database. A model's `after_commit` reads it to
//! learn what the save changed; the emitted runtime once answered
//! NameError there.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    count integer,
    created_at timestamp(6) without time zone,
    updated_at timestamp(6) without time zone,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_previous_changes
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const CHANGE_LOG: &str = r#"class ChangeLog
  def self.entries
    @entries ||= []
  end

  def self.add(entry)
    entries << entry
  end

  def self.take
    taken = entries.join(" ")
    entries.clear
    taken
  end
end
"#;

const WIDGET: &str = r##"class Widget < ApplicationRecord
  after_commit :log_commit

  def log_commit
    ChangeLog.add("commit[#{summary}]")
  end

  def summary
    pairs = previous_changes
    keys = pairs.keys.sort.join(",")
    "#{keys} name=#{pairs["name"].inspect}"
  end
end
"##;

const PROBE: &str = r##"class PreviousChangesProbe
  def self.lines
    out = []
    w = Widget.new
    w.name = "a"
    w.count = 1
    w.save!
    out << "create: #{ChangeLog.take} after[#{w.summary}]"
    w.update!(name: "b")
    out << "update name: #{ChangeLog.take} after[#{w.summary}]"
    w.update!(count: 2)
    out << "update count: #{ChangeLog.take} after[#{w.summary}]"
    w = Widget.find(w.id)
    out << "reloaded: after[#{w.summary}]"
    w.update!(name: "c")
    out << "update reloaded: #{ChangeLog.take} after[#{w.summary}]"
    out << "same as saved_changes: #{w.previous_changes == w.saved_changes}"
    out
  end
end
"##;

const LOADS: [&str; 4] = [
    "app/models/change_log",
    "app/models/application_record",
    "app/models/widget",
    "app/models/previous_changes_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/change_log.rb", CHANGE_LOG),
    ("app/models/widget.rb", WIDGET),
    ("app/models/previous_changes_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    "create: commit[count,created_at,id,name,updated_at name=[nil, \"a\"]] after[count,created_at,id,name,updated_at name=[nil, \"a\"]]",
    "update name: commit[name,updated_at name=[\"a\", \"b\"]] after[name,updated_at name=[\"a\", \"b\"]]",
    "update count: commit[count,updated_at name=nil] after[count,updated_at name=nil]",
    "reloaded: after[ name=nil]",
    "update reloaded: commit[name,updated_at name=[\"b\", \"c\"]] after[name,updated_at name=[\"b\", \"c\"]]",
    "same as saved_changes: true",
];

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn previous_changes_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let errors: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let (native, emitted) = pg_probe::native_and_emitted(
        "previous-changes",
        &FILES,
        &emitted,
        "rh_e2e_previous_changes",
        "PreviousChangesProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
