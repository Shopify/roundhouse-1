//! `ActiveSupport.on_load(:active_record_postgresqladapter)` in a boot
//! initializer (`ingest::on_load_hooks`, `project::load_hook_registrations`,
//! `runtime/spinel/active_support_lazy_load_hooks.rb`).
//!
//! A PostgreSQL app lifts the hook body onto the Application reopen,
//! registers it in boot.rb before the database configuration loads, and
//! `DatabaseConfiguration.configure!` fires it before the first
//! connection. Statements the runtime cannot run stay
//! `initializer_unsupported` gaps with their reason; the rest run.

use std::path::{Path, PathBuf};

const PG_RB: &str = r#"# frozen_string_literal: true

ActiveSupport.on_load(:active_record_postgresqladapter) do
  require "semian/activerecord_postgresql_adapter"
  if Rails.env.local?
    ActiveRecord::ConnectionAdapters::PostgreSQL::SchemaStatements.prepend(
      Module.new,
    )
  end

  #: self as singleton(ActiveRecord::ConnectionAdapters::PostgreSQLAdapter)
  self.create_unlogged_tables = true if Rails.env.test?

  if Rails.env.local? && (ENV["DB_ADAPTER"].present? || ENV["YUGABYTE"].present?)
    require "development_support/probe_subscriber"

    ProbeSubscriber.attach_to :active_record
  end
end
"#;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  semian:\n    name: master\n    tickets: 8\n    timeout: 0\n    success_threshold: 2\n    error_threshold: 3\n    error_timeout: 35\n    half_open_resource_timeout: 1\n\ntest:\n  <<: *default\n  database: hook_probe\n";

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn probe_app(adapter: Option<&str>, tag: &str) -> Dir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog");
    let dir = Dir(std::env::temp_dir().join(format!("roundhouse-on-load-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    copy_tree(&fixture, &dir.0);
    let root = &dir.0;
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "config/application.rb",
        "require_relative \"boot\"\nrequire \"rails/all\"\n\nmodule Hp\n  class Application < Rails::Application\n  end\nend\n",
    );
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n");
    write("config/initializers/pg.rb", PG_RB);
    write(
        "lib/development_support/probe_subscriber.rb",
        "class ProbeSubscriber < ActiveSupport::LogSubscriber\n  subscribe_log_level :sql, :error\n\n  def sql(event)\n    error(event.payload[:sql])\n  end\nend\n",
    );
    if let Some(adapter) = adapter {
        write("config/database.yml", DATABASE_YML);
        write("roundhouse.yml", &format!("database_adapter: {adapter}\n"));
    }
    dir
}

#[test]
fn postgresql_app_lifts_the_adapter_hook_and_fires_it_before_configure() {
    let dir = probe_app(Some("postgresql"), "pg");
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest probe");

    assert_eq!(app.load_hooks.len(), 1, "{:?}", app.load_hooks);
    let hook = &app.load_hooks[0];
    assert_eq!(hook.hook, "active_record_postgresqladapter");
    assert_eq!(hook.method, "__on_load_active_record_postgresqladapter_0");

    // Only the prepend statement is a gap, with its reason.
    let gaps: Vec<_> = app
        .initializer_gaps
        .iter()
        .filter(|g| app.sources.get(g.span.file.0 as usize - 1).is_some_and(|s| s.path.ends_with("config/initializers/pg.rb")))
        .collect();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].construct, "if");
    assert!(gaps[0].detail.starts_with("`prepend` into SchemaStatements is not applied: no schema reflection"), "{}", gaps[0].detail);

    let files = roundhouse::project::target_files(&app, &dir.0, roundhouse::project::BuildTarget::Ruby).expect("package");
    let file = |name: &str| -> String {
        files
            .iter()
            .find(|(n, _)| Path::new(n.as_str()) == Path::new(name))
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| panic!("no {name} in the emitted tree"))
    };
    let application = file("config/application.rb");
    let method = application
        .split("def __on_load_active_record_postgresqladapter_0(__on_load_base)")
        .nth(1)
        .expect("lifted hook method");
    let method = method.split("\n    end\n").next().unwrap();
    assert!(method.contains("require_relative \"../runtime/semian_activerecord_postgresql_adapter\""), "{method}");
    assert!(method.contains("__on_load_base.create_unlogged_tables = true if Rails.env.test?"), "{method}");
    assert!(method.contains("ProbeSubscriber.attach_to :active_record"), "{method}");
    assert!(!method.contains("prepend"), "{method}");

    let boot = file("boot.rb");
    let hooks = boot.find("require_relative \"runtime/active_support_lazy_load_hooks\"").expect("hooks runtime");
    let register = boot
        .find("ActiveSupport.on_load(:active_record_postgresqladapter) { Rails.application.__on_load_active_record_postgresqladapter_0(self) }")
        .expect("registration");
    let configure = boot.find("require_relative \"runtime/database_configuration\"").expect("database configuration");
    assert!(hooks < register && register < configure, "{boot}");

    let config = file("runtime/database_configuration.rb");
    let fire = config
        .find("ActiveSupport.run_load_hooks(:active_record_postgresqladapter, ActiveRecord::ConnectionAdapters::PostgreSQLAdapter)")
        .expect("fires the hook");
    let connect = config.find("Db.configure_database(").expect("configure_database");
    assert!(fire < connect, "{config}");
    assert!(files.iter().any(|(n, _)| n.as_str() == "runtime/semian_activerecord_postgresql_adapter.rb"));
}

#[test]
fn hook_stays_a_gap_when_the_app_is_not_compiled_for_postgresql() {
    let dir = probe_app(None, "sqlite");
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest probe");
    assert!(app.load_hooks.is_empty());
    assert!(
        app.initializer_gaps.iter().any(|g| g.construct == "ActiveSupport.on_load"),
        "{:?}",
        app.initializer_gaps
    );
}
