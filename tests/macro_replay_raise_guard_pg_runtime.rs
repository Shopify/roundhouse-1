//! A replayed class macro never writes a load-time raise into the class
//! body. On the PostgreSQL tree a model-body macro that reaches
//! `validates` is replayed (`lower::model_to_library::macro_replay`);
//! one whose arguments hold a site the emit writes as
//! `(raise "roundhouse: ... not supported")` (here a constant only a
//! Rails-dependent gem declares, error[gem_requires_rails], as Core's
//! User passes to `validates_sensitive :email, format: { with: ... }`)
//! is refused by name instead, and the rest of the model loads.
//!
//! Core's `symbol_enum :status, [...], optional: true` (SymbolEnum,
//! Playbook): the define_method expansion refuses its `**options` and
//! computed names, which analysis reads; the replay runs the macro
//! itself, so the call is written as is, not as the refusal's raise, and
//! the enum answers as native Rails 8.2 does on the same PostgreSQL.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE playbooks (
    id bigserial NOT NULL,
    status character varying,
    kind character varying,
    email character varying,
    CONSTRAINT playbooks_pkey PRIMARY KEY (id)
);
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

/// The lock with a gem that depends on Rails, whose constant the RBI
/// declares: every read of it is refused (gem_requires_rails).
const LOCK_WITH_RAILS_GEM: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    concurrent-ruby (1.3.5)\n    email_check (1.0.0)\n      activesupport\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  email_check\n  i18n\n";

const RBI: &str = "# typed: true\nmodule EmailCheck\n  PATTERN = T.let(T.unsafe(nil), Regexp)\nend\n";

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_macro_replay_raise_guard
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  extend SensitiveValidations
end
"#;

/// Core's components/platform/essentials/app/utils/symbol_enum.rb,
/// without its Sorbet `requires_ancestor` lines.
const SYMBOL_ENUM: &str = r#"module SymbolEnum
  def symbol_enum(column, possible_values, **options)
    singleton_class.send(:define_method, column.to_s.pluralize) do
      possible_values
    end

    define_method(column) { super()&.to_sym }

    if options[:optional]
      validates(column, inclusion: { in: possible_values }, allow_nil: true)
    else
      validates(column, presence: true)
      validates(column, inclusion: { in: possible_values })
    end
  end
end
"#;

const SENSITIVE: &str = r#"module SensitiveValidations
  def validates_sensitive(*attributes, **options)
    validates(*attributes, **options)
  end
end
"#;

const PLAYBOOK: &str = r#"class Playbook < ApplicationRecord
  extend SymbolEnum

  symbol_enum :status, [:active, :paused, :errored], optional: true
  symbol_enum :kind, [:manual, :auto]
end
"#;

const PLAYBOOK_WITH_GEM_ARG: &str = r#"class Playbook < ApplicationRecord
  extend SymbolEnum

  symbol_enum :status, [:active, :paused, :errored], optional: true
  validates_sensitive :email, format: { with: EmailCheck::PATTERN }
  validates_sensitive :email, presence: true
  pattern = EmailCheck::PATTERN
  validates_sensitive :email, format: { with: pattern }, allow_nil: true
  symbol_enum :kind, [:manual, :auto]
end
"#;

const PROBE: &str = r##"class EnumProbe
  def self.lines
    out = []
    out << "statuses: #{Playbook.statuses.inspect} #{Playbook.kinds.inspect}"
    p = Playbook.create!(status: "paused", kind: "auto")
    p = Playbook.find(p.id)
    out << "read: #{p.status.inspect} #{p.kind.inspect}"
    out << "optional: #{Playbook.new(kind: :manual).valid?}"
    bad = Playbook.new(status: "gone", kind: :manual)
    out << "inclusion: #{bad.valid?} #{bad.errors[:status].inspect}"
    missing = Playbook.new
    out << "required: #{missing.valid?} #{missing.errors[:kind].inspect}"
    p.update!(status: :errored)
    out << "symbol write: #{Playbook.find(p.id).status.inspect} #{p.read_attribute(:status).inspect}"
    out
  end
end
"##;

const LOADS: [&str; 5] = [
    "app/models/symbol_enum.rb",
    "app/models/sensitive_validations.rb",
    "app/models/application_record.rb",
    "app/models/playbook.rb",
    "app/models/enum_probe.rb",
];

const FILES: [(&str, &str); 10] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("Gemfile.lock", LOCK),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/symbol_enum.rb", SYMBOL_ENUM),
    ("app/models/sensitive_validations.rb", SENSITIVE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/playbook.rb", PLAYBOOK),
    ("app/models/enum_probe.rb", PROBE),
];

const EXPECTED: [&str; 6] = [
    "statuses: [:active, :paused, :errored] [:manual, :auto]",
    "read: :paused :auto",
    "optional: true",
    "inclusion: false [\"is not included in the list\"]",
    "required: false [\"can't be blank\", \"is not included in the list\"]",
    "symbol write: :errored \"errored\"",
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// Class-body lines (two-space indent, not a `def`) that raise a
/// roundhouse placeholder when the class loads.
fn load_time_raises(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   ") && !l.trim_start().starts_with("def "))
        .filter(|l| l.contains("raise \"roundhouse:") || l.contains("::NotImplementedError, 'roundhouse:"))
        .collect()
}

/// The `symbol_enum` calls are replayed as written; without PostgreSQL.
#[test]
fn symbol_enum_with_keywords_replays_as_written() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/playbook.rb")).unwrap();
    assert!(load_time_raises(&text).is_empty(), "{text}");
    assert!(text.contains("  symbol_enum :status, [:active, :paused, :errored], optional: true\n"), "{text}");
    assert!(text.contains("  symbol_enum :kind, [:manual, :auto]\n"), "{text}");
    let refused: Vec<&String> = errors.iter().filter(|e| e.contains("symbol_enum")).collect();
    assert!(refused.is_empty(), "{refused:#?}");
}

/// A macro call whose argument emits as a raise is refused by name; the
/// model's other calls replay; without PostgreSQL.
#[test]
fn a_macro_call_with_a_raising_argument_is_refused_not_written() {
    let (emitted, errors) = overlay()
        .write("Gemfile.lock", LOCK_WITH_RAILS_GEM)
        .write("sorbet/rbi/gems/email_check@1.0.0.rbi", RBI)
        .write("app/models/playbook.rb", PLAYBOOK_WITH_GEM_ARG)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/playbook.rb")).unwrap();
    assert!(load_time_raises(&text).is_empty(), "{text}");
    assert!(!text.contains("EmailCheck"), "{text}");
    assert!(text.contains("  validates_sensitive :email, presence: true\n"), "{text}");
    assert!(text.contains("  symbol_enum :status, [:active, :paused, :errored], optional: true\n"), "{text}");
    assert!(text.contains("  symbol_enum :kind, [:manual, :auto]\n"), "{text}");
    assert!(
        errors.iter().any(|e| e.contains("validates_sensitive") && e.contains("EmailCheck::PATTERN is declared by the email_check gem") && e.contains("raise when the class loads")),
        "{errors:#?}"
    );
    // A call reading a body local whose write raises is refused too, and
    // the write is not carried.
    assert!(!text.contains("pattern"), "{text}");
    assert!(
        errors.iter().any(|e| e.contains("validates_sensitive") && e.contains("reads the class-body local `pattern`")),
        "{errors:#?}"
    );
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn symbol_enum_matches_native_rails_on_postgresql() {
    let (emitted, _) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "macro-replay-raise-guard",
        &FILES,
        &emitted,
        "rh_e2e_macro_replay_raise_guard",
        "EnumProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_lines, native, "emitted");
}
