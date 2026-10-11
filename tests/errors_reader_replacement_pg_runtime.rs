//! A model whose class replaces `errors` with Rails' own spelling
//! (`@errors ||= ActiveModel::Errors.new(self)`, from an included module),
//! and a presence rule on a nullable timestamp column, against native
//! Rails 8.2 on the same local PostgreSQL database.
//!
//! The compiled model keeps its messages in the `@errors` Array
//! (runtime/ruby/active_record/base.rb), so a replacement written that
//! way reads the Array straight back, never reaching the wrapper that
//! teaches it ActiveModel::Errors' readers. `errors.full_messages` on it
//! reached a bare Array (`undefined method 'full_messages'`), and the
//! timestamp rule read `@installed_at`, which a temporal column never
//! assigns (it stores `@installed_at_raw`), so every record failed it.
//! The probe reads the collection through a method argument, so the
//! compiler's `errors.full_messages` fold cannot apply.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    installed_at timestamp(6) without time zone,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_errors_reader
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const REPORTING: &str = r#"module Reporting
  def errors
    @errors ||= ActiveModel::Errors.new(self)
  end
end
"#;

const WIDGET: &str = r#"class Widget < ApplicationRecord
  include Reporting

  validates :name, :installed_at, presence: true
end
"#;

const PROBE: &str = r##"class ErrorsReaderProbe
  def self.messages(errors)
    errors.full_messages
  end

  def self.describe(label, record)
    valid = record.valid?
    errors = record.errors
    "#{label}: #{valid} #{messages(errors).inspect} #{errors.messages.inspect} #{errors.details.inspect}"
  end

  def self.lines
    out = []
    out << describe("bare", Widget.new)
    out << describe("unstamped", Widget.new(name: "a"))
    out << describe("stamped", Widget.new(name: "a", installed_at: Time.now.utc))
    widget = Widget.new(installed_at: Time.now.utc)
    widget.valid?
    errors = widget.errors
    out << "queries: #{messages(errors).inspect} #{errors.full_messages_for(:name).inspect} #{errors.of_kind?(:name, :blank)} #{errors.key?(:installed_at)}"
    out << "saved: #{Widget.create(name: "b", installed_at: Time.now.utc).persisted?}"
    out << "rejected: #{Widget.create(name: "c").persisted?}"
    out << "count: #{Widget.count}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 7] = [
    r#"bare: false ["Name can't be blank", "Installed at can't be blank"] {name: ["can't be blank"], installed_at: ["can't be blank"]} {name: [{error: :blank}], installed_at: [{error: :blank}]}"#,
    r#"unstamped: false ["Installed at can't be blank"] {installed_at: ["can't be blank"]} {installed_at: [{error: :blank}]}"#,
    r#"stamped: true [] {} {}"#,
    r#"queries: ["Name can't be blank"] ["Name can't be blank"] true false"#,
    r#"saved: true"#,
    r#"rejected: false"#,
    r#"count: 1"#,
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("Gemfile.lock", LOCK),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/reporting.rb", REPORTING),
    ("app/models/widget.rb", WIDGET),
    ("app/models/errors_reader_probe.rb", PROBE),
];

const LOADS: [&str; 4] = [
    "app/models/application_record",
    "app/models/reporting",
    "app/models/widget",
    "app/models/errors_reader_probe",
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The timestamp rule reads the stored text, not an ivar nothing sets.
#[test]
fn presence_on_a_nullable_timestamp_reads_the_stored_text() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let widget = std::fs::read_to_string(emitted.join("app/models/widget.rb")).unwrap();
    assert!(widget.contains("@installed_at_raw.nil?"), "{widget}");
    assert!(!widget.contains("@installed_at.nil?"), "{widget}");
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn replaced_errors_reader_answers_rails_readers_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "errors-reader-replacement",
        &FILES,
        &emitted,
        "rh_e2e_errors_reader",
        "ErrorsReaderProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_lines, native, "emitted");
}
