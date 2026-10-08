//! `ActiveRecord::Base.configurations` and Rails' configuration classes
//! (`ActiveRecord::DatabaseConfigurations`, `::HashConfig`, `::UrlConfig`)
//! on the PostgreSQL CRuby tree, which carries them as the port boot
//! resolves config/database.yml with (runtime/spinel/cruby/
//! database_configurations.rb), against native Rails 8.2 on the same local
//! PostgreSQL database.
//!
//! The analyzer models them only where that runtime is carried; elsewhere
//! the read stays a named gap. The Rails classes no tree carries (the
//! fixture loader, database tasks, the adapter's schema reflection,
//! association reflection) are refused by name, with the reason
//! (`analyze::registry::refused_constants`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_database_configurations
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// Configurations built from a hash, as Core's code reads them
/// (`configs_for(env_name:, include_hidden: true)`, `filter_map(&:max_connections)`,
/// a HashConfig copied with one key changed), and `Base.configurations=`
/// wrapping a hash and restored. No environment-dependent call: native
/// Rails runs without RAILS_ENV, the emitted program under `test`.
const PROBE: &str = r##"class DatabaseConfigurationsProbe
  CONFIG = {
    "probe" => {
      "primary" => { "adapter" => "postgresql", "database" => "probe_primary", "max_connections" => 7, "min_connections" => 2, "checkout_timeout" => 3, "idle_timeout" => 0 },
      "primary_replica" => { "adapter" => "postgresql", "database" => "probe_primary", "replica" => true, "pool" => 4 },
      "archive" => { "adapter" => "postgresql", "database" => "probe_archive", "database_tasks" => false, "keepalive" => false, "max_age" => 30, "query_cache" => 12 },
    },
    "other" => {
      "primary" => { "adapter" => "postgresql", "database" => "probe_other", "max_threads" => 9 },
      "warehouse" => { "url" => "postgres://wh.example:5433/probe_url?pool=9" },
    },
  }.freeze

  def self.lines
    out = []
    base = ActiveRecord::Base.configurations
    out << "base: #{base.is_a?(ActiveRecord::DatabaseConfigurations)}"
    configs = ActiveRecord::DatabaseConfigurations.new(CONFIG)
    out << "names: #{configs.configurations.map { |c| "#{c.env_name}/#{c.name}/#{c.class.name.split("::").last}" }.inspect}"
    out << "configs_for: #{configs.configs_for(env_name: "probe").map(&:name).inspect}"
    out << "hidden: #{configs.configs_for(env_name: "probe", include_hidden: true).map(&:name).inspect}"
    replica = configs.configs_for(env_name: "probe", name: "primary_replica", include_hidden: true)
    out << "replica: #{replica.replica?.inspect} #{replica.configuration_hash.inspect}"
    out << "pools: #{configs.configs_for(env_name: "probe", include_hidden: true).filter_map(&:max_connections).inspect}"
    configs.configurations.each do |c|
      out << "#{c.name}: #{[c.adapter, c.database, c.host, c.max_connections, c.min_connections, c.min_threads, c.max_threads, c.max_queue, c.max_age, c.checkout_timeout, c.idle_timeout, c.keepalive, c.reaping_frequency, c.database_tasks?, c.query_cache].inspect}"
    end
    url = configs.configs_for(env_name: "other", name: "warehouse")
    out << "url: #{url.is_a?(ActiveRecord::DatabaseConfigurations::UrlConfig)} #{url.is_a?(ActiveRecord::DatabaseConfigurations::HashConfig)} #{url.url} #{url.configuration_hash.inspect}"
    out << "find: #{configs.find_db_config("probe")&.name.inspect} #{configs.find_db_config("other")&.name.inspect} #{configs.find_db_config("missing").inspect}"
    primary = configs.configs_for(env_name: "probe", name: "primary")
    copy = ActiveRecord::DatabaseConfigurations::HashConfig.new(primary.env_name, primary.name, primary.configuration_hash.merge(username: "probe_user"))
    out << "copy: #{copy.is_a?(ActiveRecord::DatabaseConfigurations::HashConfig)} #{copy.env_name} #{copy.name} #{copy.configuration_hash.inspect}"
    out << "resolve: #{configs.resolve(copy).equal?(copy)}"
    out << "empty: #{configs.empty?} #{configs.blank?} #{configs.any?} #{ActiveRecord::DatabaseConfigurations.new({}).empty?}"
    ActiveRecord::Base.configurations = CONFIG
    assigned = ActiveRecord::Base.configurations
    out << "assigned: #{assigned.is_a?(ActiveRecord::DatabaseConfigurations)} #{assigned.configs_for(env_name: "other").map(&:name).inspect}"
    ActiveRecord::Base.configurations = configs
    out << "rewrapped: #{ActiveRecord::Base.configurations.configurations.map(&:name) == configs.configurations.map(&:name)}"
    ActiveRecord::Base.configurations = base
    out << "restored: #{ActiveRecord::Base.configurations.configurations.size == base.configurations.size}"
    out << "widgets: #{Widget.count}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 17] = [
    "base: true",
    r#"names: ["probe/primary/HashConfig", "probe/primary_replica/HashConfig", "probe/archive/HashConfig", "other/primary/HashConfig", "other/warehouse/UrlConfig"]"#,
    r#"configs_for: ["primary"]"#,
    r#"hidden: ["primary", "primary_replica", "archive"]"#,
    r#"replica: true {adapter: "postgresql", database: "probe_primary", replica: true, pool: 4}"#,
    "pools: [7, 4, 5]",
    r#"primary: ["postgresql", "probe_primary", nil, 7, 2, 0, 5, 20, Infinity, 3.0, nil, 600.0, 20.0, true, nil]"#,
    r#"primary_replica: ["postgresql", "probe_primary", nil, 4, 0, 0, 4, 16, Infinity, 5.0, 300.0, 600.0, 20.0, false, nil]"#,
    r#"archive: ["postgresql", "probe_archive", nil, 5, 0, 0, 5, 20, 30, 5.0, 300.0, nil, 20.0, false, 12]"#,
    r#"primary: ["postgresql", "probe_other", nil, 5, 0, 0, 9, 36, Infinity, 5.0, 300.0, 600.0, 20.0, true, nil]"#,
    r#"warehouse: ["postgresql", "probe_url", "wh.example", 9, 0, 0, 5, 20, Infinity, 5.0, 300.0, 600.0, 20.0, true, nil]"#,
    r#"url: true true postgres://wh.example:5433/probe_url?pool=9 {adapter: "postgresql", port: 5433, database: "probe_url", host: "wh.example", pool: "9"}"#,
    r#"find: "primary" "primary" nil"#,
    r#"copy: true probe primary {adapter: "postgresql", database: "probe_primary", max_connections: 7, min_connections: 2, checkout_timeout: 3, idle_timeout: 0, username: "probe_user"}"#,
    "resolve: true",
    "empty: false false true true",
    r#"assigned: true ["primary", "warehouse"]"#,
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/widget.rb", "class Widget < ApplicationRecord\nend\n"),
    ("app/models/database_configurations_probe.rb", PROBE),
];

fn overlay(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn configurations_answer_as_natively_on_postgresql() {
    let (emitted, errors) = overlay(&FILES).emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "database-configurations",
        &FILES,
        &emitted,
        "rh_e2e_database_configurations",
        "DatabaseConfigurationsProbe",
        &["app/models/application_record", "app/models/widget", "app/models/database_configurations_probe"],
    );
    let mut expected: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    expected.extend(["rewrapped: true", "restored: true", "widgets: 0"].map(String::from));
    assert_eq!(native, expected, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// Core's shapes: the configuration a Trilogy connection is copied from
/// (globaldb_schema_sources.rb), a shard's configuration hash
/// (podding/database/config.rb), a pool size (concurrent_batch_map.rb).
const CORE_SHAPES: &str = r##"class ConfigurationReader
  def self.copied(name)
    config = ActiveRecord::Base.configurations.configs_for(env_name: "test_#{name}", include_hidden: true).sole
    ActiveRecord::DatabaseConfigurations::HashConfig.new(config.env_name, config.name, config.configuration_hash.merge(username: "u"))
  end

  def self.shard(name)
    ActiveRecord::Base.configurations.configs_for(name: name, include_hidden: true).configuration_hash
  end

  def self.pool_size
    ActiveRecord::Base.configurations.configs_for(env_name: "test").filter_map(&:max_connections).min || 1
  end

  def self.found
    ActiveRecord::Base.configurations.find_db_config("globaldb_config")&.database
  end
end
"##;

/// The Rails classes no tree carries, refused by name.
const REFUSED: &str = r##"class FrameworkReader
  def self.fixtures
    ActiveRecord::FixtureSet.reset_cache
  end

  def self.format_error
    ActiveRecord::Fixture::FormatError
  end

  def self.tasks
    ActiveRecord::Tasks::DatabaseTasks.env
  end

  def self.metadata
    ActiveRecord::ConnectionAdapters::SqlTypeMetadata.new(sql_type: "integer")
  end

  def self.through?(reflection)
    reflection.is_a?(ActiveRecord::Reflection::ThroughReflection)
  end
end
"##;

#[test]
fn configurations_type_on_postgresql_and_framework_classes_are_refused_by_name() {
    let mut files = FILES.to_vec();
    files.push(("app/models/configuration_reader.rb", CORE_SHAPES));
    files.push(("app/models/framework_reader.rb", REFUSED));
    let (_emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    let errors: Vec<&String> = real_errors(&errors);
    // Nothing from the configurations reads or the probe.
    let configurations: Vec<_> = errors
        .iter()
        .filter(|e| e.contains("configurations") || e.contains("DatabaseConfigurations") || e.contains("max_connections"))
        .collect();
    assert!(configurations.is_empty(), "{configurations:#?}");
    let refusals: Vec<&String> = errors.iter().copied().filter(|e| e.contains("constant not supported")).collect();
    for (name, reason) in [
        ("ActiveRecord::FixtureSet", "Rails' fixture loader is not carried"),
        ("ActiveRecord::Fixture::FormatError", "Rails' fixture loader is not carried"),
        ("ActiveRecord::Tasks::DatabaseTasks", "Rails' database tasks are not carried"),
        ("ActiveRecord::ConnectionAdapters::SqlTypeMetadata", "the adapter's schema reflection objects are not carried"),
        ("ActiveRecord::Reflection::ThroughReflection", "Rails' reflection classes are not carried"),
    ] {
        let expected = format!("constant not supported (all targets): {name} — {reason}");
        assert!(refusals.iter().any(|e| e.contains(&expected)), "no {expected:?} in:\n{refusals:#?}");
    }
    assert_eq!(refusals.len(), 5, "{refusals:#?}");
}

/// Without the PostgreSQL runtime nothing carries them: the reads stay
/// named gaps.
#[test]
fn configurations_stay_a_named_gap_without_the_postgresql_runtime() {
    let files = [
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\n  create_table \"widgets\" do |t|\n    t.string \"name\"\n  end\nend\n"),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\nend\n"),
        ("app/models/configuration_reader.rb", CORE_SHAPES),
    ];
    let (_emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    for expected in [
        "ActiveRecord::Base not supported (all targets): configurations lacks a shared runtime/lowering implementation",
        "constant not supported (all targets): ActiveRecord::DatabaseConfigurations::HashConfig",
    ] {
        assert!(errors.iter().any(|e| e.contains(expected)), "no {expected:?} in:\n{}", errors.join("\n"));
    }
}
