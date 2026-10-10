//! `include ActiveModel::Model` on a plain class the tableless-model
//! lowering does not take (Core's chargeback-response
//! `Input::SerializableContext` under app/services, which declares no
//! writers of its own, and its `attr_accessor` subclasses), against
//! native Rails 8.2 on the same local PostgreSQL database. The include
//! stands, so ActiveModel::Model is the runtime's port
//! (runtime/spinel/cruby/active_model_api.rb): the attribute-hash
//! constructor, validations, naming, conversion, `persisted?` and
//! Access. Before the port the class raised NameError as it loaded.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_active_model_api
"#;

/// The ActiveModel::Validations port translates through I18n, which the
/// lockfile names.
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SERIALIZABLE: &str = r#"module Disputes
  module Input
    class SerializableContext
      include ActiveModel::Model
      include ActiveModel::Serializers::JSON

      def as_json(options = {})
        attributes.as_json(options)
      end

      def attributes
        raise NotImplementedError, "Subclasses must implement attributes"
      end
    end
  end
end
"#;

const SUBSCRIPTION: &str = r#"module Disputes
  module Input
    class SubscriptionContext < SerializableContext
      attr_accessor :status, :cycles

      validates :status, presence: true

      def attributes
        { "status" => status, "cycles" => cycles }
      end
    end
  end
end
"#;

const PROBE: &str = r##"class ActiveModelApiProbe
  def self.lines
    out = []
    klass = Disputes::Input::SubscriptionContext
    out << "ancestors: #{[ActiveModel::Model, ActiveModel::API, ActiveModel::Validations, ActiveModel::Conversion, ActiveModel::AttributeAssignment, ActiveModel::Access].map { |m| klass.include?(m) }.inspect}"
    s = klass.new(status: "active", cycles: 3)
    out << "new: #{s.status} #{s.cycles} #{s.persisted?} #{s.valid?} #{s.to_key.inspect}"
    out << "json: #{s.as_json.inspect}"
    out << "access: #{s.slice(:status, :cycles).inspect} #{s.values_at(:cycles, :status).inspect}"
    blank = klass.new
    out << "invalid: #{blank.valid?} #{blank.errors.full_messages.inspect}"
    out << "naming: #{klass.model_name.name} #{klass.model_name.param_key} #{klass.human_attribute_name(:status)} #{s.to_partial_path}"
    begin
      klass.new(nope: 1)
    rescue ActiveModel::UnknownAttributeError => e
      out << "unknown: #{e.message}"
    end
    out << "nil: #{klass.new(nil).status.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 8] = [
    "ancestors: [true, true, true, true, true, true]",
    "new: active 3 false true nil",
    r#"json: {"status" => "active", "cycles" => 3}"#,
    r#"access: {"status" => "active", "cycles" => 3} [3, "active"]"#,
    r#"invalid: false ["Status can't be blank"]"#,
    "naming: Disputes::Input::SubscriptionContext disputes_input_subscription_context Status disputes/input/subscription_contexts/subscription_context",
    "unknown: unknown attribute 'nope' for Disputes::Input::SubscriptionContext.",
    "nil: nil",
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] = [
    "app/models/application_record",
    "app/services/disputes/input/serializable_context",
    "app/services/disputes/input/subscription_context",
    "app/models/active_model_api_probe",
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("Gemfile.lock", LOCK),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", ""),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/services/disputes/input/serializable_context.rb", SERIALIZABLE),
    ("app/services/disputes/input/subscription_context.rb", SUBSCRIPTION),
    ("app/models/active_model_api_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the include stands and the tree carries the port,
/// loaded after AttributeAssignment and the Validations port.
#[test]
fn active_model_model_include_loads_the_port() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert!(emitted.join("runtime/active_model_api.rb").exists(), "no runtime/active_model_api.rb");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    let at = |feature: &str| facades.find(&format!("require_relative \"{feature}\"\n")).unwrap_or_else(|| panic!("no {feature} require:\n{facades}"));
    assert!(at("active_model_validations") < at("active_model_api"));
    assert!(at("active_record_core") < at("active_model_api"));
}

/// The classes natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn active_model_model_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "active-model-api",
        &FILES,
        &emitted,
        "rh_e2e_active_model_api",
        "ActiveModelApiProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
