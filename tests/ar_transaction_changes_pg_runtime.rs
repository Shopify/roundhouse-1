//! The ar_transaction_changes gem's `transaction_changed_attributes`
//! (1.1.11, which Core includes into its models) against native Rails 8.2
//! with the real gem on the same local PostgreSQL database.
//!
//! The emitted program carries the gem's contract in the runtime
//! (runtime/spinel/cruby/active_record_transactions.rb, `module
//! ArTransactionChanges`): every attribute changed since the record's last
//! transaction finished, with its value before the first change, dropped
//! when written back to that value; the commit and rollback callbacks read
//! it, and it resets once they ran.
//!
//! The native side loads the gem itself (lib/native_gems.rb, native only);
//! the emitted side compiles the same models with only the gem's
//! version.rb, as Core's emitted tree carries it.
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
  database: rh_e2e_ar_transaction_changes
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const NATIVE_GEMS: &str = r#"require "ar_transaction_changes"
"#;

/// What Core's emitted tree carries of the gem: its version.rb (ingest
/// refuses lib/ar_transaction_changes.rb), emitted side only.
const GEM_VERSION: &str = r#"module ArTransactionChanges
  VERSION = "1.1.11"
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
  include ArTransactionChanges

  after_commit :log_commit
  after_rollback :log_rollback

  def log_commit
    ChangeLog.add("commit[#{changes_text}]")
  end

  def log_rollback
    ChangeLog.add("rollback[#{changes_text}]")
  end

  def changes_text
    transaction_changed_attributes.keys.sort.map do |key|
      value = transaction_changed_attributes[key]
      "#{key}=#{value.respond_to?(:strftime) ? "t#{value.year}" : value.inspect}"
    end.join(",")
  end
end
"##;

const PROBE: &str = r##"class ChangesProbe
  def self.run(out, label, widget)
    ChangeLog.take
    yield
    out << "#{label}: #{ChangeLog.take} after[#{widget.changes_text}]"
  end

  def self.lines
    out = []
    w = Widget.new
    run(out, "create in txn, update twice", w) do
      Widget.transaction do
        w.name = "a"
        w.count = 1
        w.save!
        w.update!(name: "b")
        w.update!(count: 2)
        ChangeLog.add("in[#{w.changes_text}]")
      end
    end
    w = Widget.find(w.id)
    run(out, "update twice in txn", w) do
      Widget.transaction do
        w.update!(name: "c")
        w.update!(count: 3)
        ChangeLog.add("in[#{w.changes_text}]")
      end
    end
    run(out, "written back", w) do
      Widget.transaction do
        w.update!(name: "d", count: 4)
        w.update!(name: "c")
        ChangeLog.add("in[#{w.changes_text}]")
      end
    end
    run(out, "rollback", w) do
      Widget.transaction do
        w.update!(name: "r")
        ChangeLog.add("in[#{w.changes_text}]")
        raise ActiveRecord::Rollback
      end
    end
    run(out, "unsaved write", w) do
      w.count = 9
      ChangeLog.add("pending[#{w.changes_text}]")
      w.count = 4
    end
    w = Widget.find(w.id)
    run(out, "save outside txn", w) { w.update!(count: 5) }
    out << "db: #{Widget.find(w.id).name} #{Widget.find(w.id).count}"
    out
  end
end
"##;

/// What native Rails 8.2 with ar_transaction_changes 1.1.11 answers
/// (checked on the task PostgreSQL), so two identical wrong answers still
/// fail.
const EXPECTED: [&str; 7] = [
    "create in txn, update twice: in[count=nil,created_at=nil,id=nil,name=nil,updated_at=nil] commit[count=nil,created_at=nil,id=nil,name=nil,updated_at=nil] after[]",
    "update twice in txn: in[count=2,name=\"b\",updated_at=t2026] commit[count=2,name=\"b\",updated_at=t2026] after[]",
    "written back: in[count=3,updated_at=t2026] commit[count=3,updated_at=t2026] after[]",
    "rollback: in[name=\"c\",updated_at=t2026] rollback[name=\"c\",updated_at=t2026] after[]",
    "unsaved write: pending[count=4] after[]",
    "save outside txn: commit[count=4,updated_at=t2026] after[]",
    "db: c 5",
];

const LOADS: [&str; 5] = [
    "lib/native_gems",
    "app/models/change_log",
    "app/models/application_record",
    "app/models/widget",
    "app/models/changes_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/change_log.rb", CHANGE_LOG),
    ("app/models/widget.rb", WIDGET),
    ("app/models/changes_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("app/models/ar_transaction_changes.rb", GEM_VERSION)
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect()
}

/// The emitted PostgreSQL runtime carries the gem's module, with the
/// commit/rollback resets and the save-time fold.
#[test]
fn runtime_carries_ar_transaction_changes() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let transactions = std::fs::read_to_string(emitted.join("runtime/active_record_transactions.rb"))
        .expect("runtime/active_record_transactions.rb");
    for expected in [
        "module ArTransactionChanges\n",
        "  def transaction_changed_attributes\n",
        "  def __track_saved_changes(was_new)\n",
        "    _reset_transaction_changes if should_run_callbacks\n",
    ] {
        assert!(transactions.contains(expected), "missing {expected:?}");
    }
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn ar_transaction_changes_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let mut native_files = FILES.to_vec();
    native_files.push(("lib/native_gems.rb", NATIVE_GEMS));
    let (native, emitted) = pg_probe::native_and_emitted(
        "ar-transaction-changes",
        &native_files,
        &emitted,
        "rh_e2e_ar_transaction_changes",
        "ChangesProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}

/// The gem's own source, as Core's emitted tree carries it (its module
/// body reopens the runtime's): its private readers (`_read_attribute_for_transaction`,
/// `_deserialize_transaction_change_value`) read `@attributes[name]`
/// and `_read_attribute`, which a compiled model keeps no attribute set
/// for. `attribute_will_change!` (Core's `settings_will_change!`) and
/// `write_attribute` reach them.
const GEM_SOURCE: &str = include_str!("support/ar_transaction_changes_gem.rb");

const WILL_CHANGE_PROBE: &str = r##"class WillChangeProbe
  def self.lines
    out = []
    w = Widget.create!(name: "x", count: 1)
    ChangeLog.take
    Widget.transaction do
      w.name_will_change!
      out << "noted: #{w.changes_text}"
    end
    out << "commit: #{ChangeLog.take}"
    w.update!(name: "y")
    out << "saved changes by symbol: #{w.saved_changes[:name].inspect} #{w.saved_changes.class}"
    out << "changes by symbol: #{w.changes[:name].inspect} #{w.changes.class}"
    ChangeLog.take
    w.write_attribute(:name, "z")
    out << "written: #{w.changes_text}"
    out
  end
end
"##;

const WILL_CHANGE_EXPECTED: [&str; 5] = [
    "noted: name=\"x\"",
    "commit: ",
    "saved changes by symbol: [\"x\", \"y\"] ActiveSupport::HashWithIndifferentAccess",
    "changes by symbol: nil ActiveSupport::HashWithIndifferentAccess",
    "written: name=\"y\"",
];

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn the_carried_gem_source_runs_on_a_compiled_record_as_native_rails_does() {
    let app = overlay()
        .write("app/models/ar_transaction_changes.rb", GEM_SOURCE)
        .write("app/models/will_change_probe.rb", WILL_CHANGE_PROBE);
    let (emitted, errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let mut native_files = FILES.to_vec();
    native_files.push(("lib/native_gems.rb", NATIVE_GEMS));
    native_files.push(("app/models/will_change_probe.rb", WILL_CHANGE_PROBE));
    let mut loads = LOADS.to_vec();
    loads.push("app/models/will_change_probe");
    let (native, emitted) = pg_probe::native_and_emitted(
        "ar-transaction-changes-gem-source",
        &native_files,
        &emitted,
        "rh_e2e_ar_transaction_changes_gem",
        "WillChangeProbe",
        &loads,
    );
    assert_eq!(native, WILL_CHANGE_EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
