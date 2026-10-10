//! The state_machines gem's `state_machine` macro on a model, with
//! state_machines-activerecord, against native Rails 8.2 on the same
//! local PostgreSQL database. The lowered model carries the call into
//! its class body and it runs against the loaded gems
//! (`lower::state_machines`): the initial state, a predicate, an event
//! that transitions and saves, a namespaced event under `action: nil`,
//! the `with_*` / `without_*` scopes and a refused transition.
//!
//! Not yet the audit trail: state_machines-audit_trail logs through
//! `record.send(:<owner>_<attr>_transitions).create(...)`, and a direct
//! has_many reader of an undecorated target is folded to an eager Array
//! (`lower::model_to_library::associations`), which has no `create`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE vehicles (
    id bigserial NOT NULL,
    name character varying,
    state character varying,
    alarm_state character varying,
    wheels integer DEFAULT 4,
    CONSTRAINT vehicles_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_state_machines
"#;

// Core's Gemfile entries and lock (Gemfile.lock 3964-3972), with its
// Rails GIT entry: the integrations depend on activemodel and
// activerecord, which RubyGems resolves through runtime/git_gems.rb, and
// Active Support's i18n, which state_machines-activemodel translates
// state and event names with.
const GEMFILE: &str = r#"source "https://rubygems.org"

gem "state_machines-activerecord", "~> 0.9"
"#;

const LOCK: &str = r#"GIT
  remote: https://github.com/Shopify/rails.git
  revision: 5813559f4aedd8f37cb8af46fcb24f64ec3d6e7f
  branch: sep-21-plus-query-constraints
  specs:
    activemodel (8.2.0.alpha)
      activesupport (= 8.2.0.alpha)
    activerecord (8.2.0.alpha)
      activemodel (= 8.2.0.alpha)
      activesupport (= 8.2.0.alpha)
      timeout (>= 0.4.0)
    activesupport (8.2.0.alpha)
      concurrent-ruby (~> 1.0, >= 1.3.1)
      i18n (>= 1.6, < 2)

GEM
  remote: https://rubygems.org/
  specs:
    concurrent-ruby (1.3.8)
    i18n (1.15.2)
      concurrent-ruby (~> 1.0)
    state_machines (0.6.0)
    state_machines-activemodel (0.9.0)
      activemodel (>= 6.0)
      state_machines (>= 0.6.0)
    state_machines-activerecord (0.9.0)
      activerecord (>= 6.0)
      state_machines-activemodel (>= 0.9.0)

PLATFORMS
  x86_64-linux

DEPENDENCIES
  state_machines-activerecord (~> 0.9)
"#;

// The lines of Core's tapioca RBI (sorbet/rbi/gems/state_machines@0.6.0.rbi)
// that declare the constants the probe reads.
const RBI: &str = r#"# typed: true

module StateMachines; end
class StateMachines::Error < ::StandardError; end
class StateMachines::InvalidTransition < ::StateMachines::Error; end
"#;

// What native Bundler.require does with GEMFILE (the driver loads no
// Gemfile); outside the app's load paths, so the emit does not read it.
const BUNDLER_REQUIRE: &str = r#"require "state_machines-activerecord"
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const VEHICLE: &str = r#"class Vehicle < ApplicationRecord
  state_machine :state, initial: :parked do
    event :ignite do
      transition parked: :idling
    end

    event :park do
      transition idling: :parked
    end

    after_transition on: :ignite do |vehicle|
      vehicle.log << "ignited"
    end

    state :idling, value: "running"
  end

  state_machine :alarm_state, namespace: "alarm", initial: :active, action: nil do
    event :disable do
      transition active: :off
    end
  end

  # A class-level override over Active Record's, as Core's concerns do.
  def self.table_exists?
    super && name == "Vehicle"
  end

  def log
    @log ||= []
  end
end
"#;

const PROBE: &str = r##"class StateMachinesProbe
  def self.lines
    out = []
    v = Vehicle.new(name: "a")
    out << "new: #{v.state} #{v.parked?} #{v.idling?} #{v.alarm_state} #{v.alarm_active?}"
    v.save!
    out << "can: #{v.can_ignite?} #{v.can_park?}"
    out << "ignite: #{v.ignite} #{v.state} #{v.idling?} #{v.log.join(",")} #{Vehicle.find(v.id).state}"
    out << "again: #{v.ignite} #{v.state}"
    begin
      v.ignite!
    rescue StateMachines::InvalidTransition => e
      out << "bang: #{e.class} #{e.event} #{e.from}"
    end
    out << "alarm: #{v.disable_alarm} #{v.alarm_state} #{v.alarm_off?} #{Vehicle.find(v.id).alarm_state}"
    Vehicle.create!(name: "b")
    out << "scopes: #{Vehicle.with_state(:idling).map(&:name).join(",")} #{Vehicle.without_state(:idling).map(&:name).join(",")} #{Vehicle.with_states(:parked, :idling).count}"
    out << "park: #{v.park} #{Vehicle.find(v.id).state}"
    out << "names: #{Vehicle.state_machines.keys.join(",")} #{Vehicle.state_machine(:state).states.map(&:value).join(",")}"
    out << "schema: #{Vehicle.table_exists?} #{Vehicle.column_defaults.map { |k, d| "#{k}=#{d.inspect}" }.join(",")}"
    out
  end
end
"##;

/// Native Rails 8.2 with the same gems on the same PostgreSQL, so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 10] = [
    "new: parked true false active true",
    "can: true false",
    "ignite: true running true ignited running",
    "again: false running",
    "bang: StateMachines::InvalidTransition ignite running",
    "alarm: true off true active",
    "scopes: a b 2",
    "park: true parked",
    "names: state,alarm_state parked,running",
    "schema: true id=nil,name=nil,state=nil,alarm_state=nil,wheels=4",
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] = [
    "native/bundler_require",
    "app/models/application_record",
    "app/models/vehicle",
    "app/models/state_machines_probe",
];

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/application.rb", "Bundler.require(*Rails.groups)\n"),
    ("Gemfile", GEMFILE),
    ("Gemfile.lock", LOCK),
    ("sorbet/rbi/gems/state_machines@0.6.0.rbi", RBI),
    ("native/bundler_require.rb", BUNDLER_REQUIRE),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/vehicle.rb", VEHICLE),
    ("app/models/state_machines_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect()
}

/// Both `state_machine` calls are carried into the lowered class body,
/// in body order, below the class's methods; no DSL is reported dropped.
#[test]
fn state_machine_calls_are_carried_into_the_class_body() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let vehicle = std::fs::read_to_string(emitted.join("app/models/vehicle.rb")).unwrap();
    let at = |s: &str| vehicle.find(s).unwrap_or_else(|| panic!("missing {s}:\n{vehicle}"));
    assert!(at("state_machine :state, initial: :parked do") < at("state_machine :alarm_state, namespace:"), "{vehicle}");
    assert!(at("def log") < at("state_machine :state"), "{vehicle}");
    assert!(at("transition parked: :idling") < at("state :idling, value: \"running\""), "{vehicle}");
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gems, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn state_machines_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "state-machines",
        &FILES,
        &emitted,
        "rh_e2e_state_machines",
        "StateMachinesProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
