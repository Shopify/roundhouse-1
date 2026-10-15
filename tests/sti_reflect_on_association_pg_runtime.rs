//! `reflect_on_association` on an STI subclass whose parent declares
//! nothing, against native Rails 8.2 on the same local PostgreSQL database.
//!
//! Rails' `_reflections` inherits down the class chain, so
//! `Leaf.reflect_on_association(:owner)` finds the association `Root`
//! declares even when the class between them declares none. The compiled
//! registry handed a name on only to a parent that is itself a model, so a
//! leaf under an empty STI middle class answered nil, and Core's GraphQL
//! batch loader raised "No association api_client on
//! Pricing::CodeAppPriceRule" for 106 tests.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;
const STRUCTURE: &str = r#"CREATE TABLE owners (
    id bigserial NOT NULL,
    CONSTRAINT owners_pkey PRIMARY KEY (id)
);

CREATE TABLE rules (
    id bigserial NOT NULL,
    owner_id bigint,
    type character varying,
    CONSTRAINT rules_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_sti_reflect
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const OWNER: &str = r#"class Owner < ApplicationRecord
end
"#;

const RULE: &str = r#"class Rule < ApplicationRecord
  belongs_to :owner, primary_key: :id, foreign_key: :owner_id, inverse_of: false
end
"#;

const CODE_RULE: &str = r#"module Pricing
  class CodeRule < ::Rule
  end
end
"#;

const CODE_APP_RULE: &str = r#"module Pricing
  class CodeAppRule < CodeRule
    after_initialize :set_defaults

    def set_defaults
      @defaults = true
    end
  end
end
"#;

const PROBE: &str = r##"class StiReflectProbe
  def self.lines
    out = []
    [Rule, Pricing::CodeRule, Pricing::CodeAppRule].each do |klass|
      reflection = klass.reflect_on_association(:owner)
      out << "#{klass.name}: #{reflection ? "#{reflection.macro} #{reflection.name} #{reflection.foreign_key}" : "nil"} #{klass.reflect_on_association(:missing).inspect}"
    end
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 3] = [
    "Rule: belongs_to owner owner_id nil",
    "Pricing::CodeRule: belongs_to owner owner_id nil",
    "Pricing::CodeAppRule: belongs_to owner owner_id nil",
];

/// What native Rails requires, in order.
const LOADS: [&str; 6] = [
    "app/models/application_record",
    "app/models/owner",
    "app/models/rule",
    "app/models/pricing/code_rule",
    "app/models/pricing/code_app_rule",
    "app/models/sti_reflect_probe",
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/owner.rb", OWNER),
    ("app/models/rule.rb", RULE),
    ("app/models/pricing/code_rule.rb", CODE_RULE),
    ("app/models/pricing/code_app_rule.rb", CODE_APP_RULE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text)).write("app/models/sti_reflect_probe.rb", PROBE)
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn sti_subclass_under_an_empty_parent_reflects_the_root_models_associations_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let mut files = FILES.to_vec();
    files.push(("app/models/sti_reflect_probe.rb", PROBE));
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "sti-reflect",
        &files,
        &emitted,
        "rh_e2e_sti_reflect",
        "StiReflectProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
