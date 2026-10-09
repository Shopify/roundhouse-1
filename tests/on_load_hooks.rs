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
    // Defined before any gem loads, as Rails defines it before
    // Bundler.require: a gem may register a hook as it loads.
    let facades = file("runtime/gem_facades.rb");
    assert!(facades.starts_with("require_relative \"active_support_lazy_load_hooks\"\n"), "{facades}");
    assert!(boot.find("require_relative \"runtime/gem_facades\"").is_some_and(|f| f < register), "{boot}");

    let config = file("runtime/database_configuration.rb");
    let fire = config
        .find("ActiveSupport.run_load_hooks(:active_record_postgresqladapter, ActiveRecord::ConnectionAdapters::PostgreSQLAdapter)")
        .expect("fires the hook");
    let connect = config.find("ActiveRecord::Base.establish_connection").expect("establish_connection");
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

// ---------------------------------------------------------------------
// Where boot fires the framework hooks, and the order a gem file's
// declarations load in, each on a probe booted on CRuby with Core's gem
// home (the `pg` gem; boot never connects) against native Ruby.

fn core_gem_home() -> String {
    std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into())
}

fn gem_path() -> String {
    let ruby_path = std::process::Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().expect("ruby");
    format!("{}:{}", core_gem_home(), String::from_utf8_lossy(&ruby_path.stdout))
}

fn write_app(tag: &str, files: &[(&str, &str)]) -> Dir {
    let dir = Dir(std::env::temp_dir().join(format!("roundhouse-on-load-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    for (path, text) in files {
        let file = dir.0.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

/// The emitted Ruby tree of `app`, through the CLI.
fn emit_tree(app: &Path) -> PathBuf {
    let out = app.join("emitted");
    let emit = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby", "--allow-unsupported"])
        .arg(app)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("roundhouse");
    assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
    out
}

/// `script` after `require "./boot"` in the emitted tree, with warnings
/// on (`ruby -w`): (stdout, stderr).
fn boot_and_run(tree: &Path, script: &str) -> (String, String) {
    let run = std::process::Command::new("ruby")
        .args(["-w", "-e", &format!("require \"./boot\"\n{script}")])
        .current_dir(tree)
        .env("GEM_PATH", gem_path())
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .env("RAILS_ENV", "test")
        .env_remove("DATABASE_URL")
        .env_remove("ROUNDHOUSE_DATABASE_URL")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(run.status.success(), "emitted boot failed:\n{stderr}");
    (stdout, stderr)
}

const TAGGING_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    tagging (1.0.0)\n\nDEPENDENCIES\n  tagging\n";

/// shopify-money 4.1.1's shape: lib/money.rb requires its railtie, whose
/// body registers `on_load(:active_record) { include Hooks }`, and the
/// hooks module's `included` extends the class macro a model calls.
/// Here the gem's own model calls it as Bundler.require loads it, so
/// the hook must have run by then (Core's Payments::Transfer calls
/// `money_column` as a boot file loads it, the same way).
const TAGGING_MAIN: &str = r#"require_relative "tagging/hooks"
require_relative "tagging/railtie"
require_relative "tagging/tag"
"#;

const TAGGING_RAILTIE: &str = r#"require "rails/railtie"

module Tagging
  class Railtie < Rails::Railtie
    ActiveSupport.on_load(:active_record) do
      ActiveRecord::Base.send(:include, Tagging::Hooks)
    end
  end
end
"#;

const TAGGING_HOOKS: &str = r#"module Tagging
  module Hooks
    def self.included(base)
      base.extend(ClassMethods)
    end

    module ClassMethods
      def tagged_columns
        @tagged_columns ||= []
      end

      def tagged(*columns)
        tagged_columns.concat(columns)
      end
    end
  end
end
"#;

const TAGGING_TAG: &str = r#"module Tagging
  class Tag < ActiveRecord::Base
    self.table_name = "tags"
    tagged :name
  end
end
"#;

const TAGGING_PROBE: &str = "p Tagging::Tag.tagged_columns\np ActiveRecord::Base.include?(Tagging::Hooks)\n";

/// Native Rails 8.2 (Core's pinned checkout): Rails::Railtie, then the
/// gem as Bundler.require loads it. The model's `ActiveRecord::Base`
/// loads base.rb, which fires the hook the railtie registered.
const TAGGING_NATIVE: &str = r#"require ENV.fetch("RH_RAILS_ORACLE_RB")
RailsOracle.activate("activesupport", "activemodel", "activerecord", "railties")
require "active_record"
require "rails/railtie"
$LOAD_PATH.unshift File.join(__dir__, "vendor/gems/tagging-1.0.0/lib")
require "tagging"
"#;

/// The tagging probe app (the gem compiled from vendor/gems), with
/// `native` as its native.rb.
fn tagging_app(tag: &str, native: &str) -> Dir {
    write_app(
        tag,
        &[
            ("roundhouse.yml", "gem_sources:\n  tagging: vendor/gems/tagging-1.0.0\ndatabase_adapter: postgresql\n"),
            ("Gemfile", "source \"https://rubygems.org\"\n\ngem \"tagging\"\n"),
            ("Gemfile.lock", TAGGING_LOCK),
            ("config/application.rb", "Bundler.require(*Rails.groups)\n"),
            ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"),
            ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
            ("db/structure.sql", "CREATE TABLE tags (\n    id bigserial NOT NULL,\n    name character varying,\n    CONSTRAINT tags_pkey PRIMARY KEY (id)\n);\n"),
            ("vendor/gems/tagging-1.0.0/lib/tagging.rb", TAGGING_MAIN),
            ("vendor/gems/tagging-1.0.0/lib/tagging/railtie.rb", TAGGING_RAILTIE),
            ("vendor/gems/tagging-1.0.0/lib/tagging/hooks.rb", TAGGING_HOOKS),
            ("vendor/gems/tagging-1.0.0/lib/tagging/tag.rb", TAGGING_TAG),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
            ("native.rb", native),
        ],
    )
}

/// The app's native.rb under Core's pinned Rails: its stdout.
fn run_native(app: &Path) -> String {
    let native = std::process::Command::new("ruby")
        .arg(app.join("native.rb"))
        .env("RH_RAILS_ORACLE_RB", Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/rails_oracle.rb"))
        .output()
        .expect("ruby");
    assert!(native.status.success(), "native run failed:\n{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).into_owned()
}

#[test]
fn a_gem_railtie_hook_has_run_when_a_model_loading_at_boot_calls_its_macro() {
    let dir = tagging_app("railtie-macro", &format!("{TAGGING_NATIVE}{TAGGING_PROBE}"));
    let native = run_native(&dir.0);
    assert_eq!(native, "[:name]\ntrue\n");

    let tree = emit_tree(&dir.0);
    let read = |rel: &str| std::fs::read_to_string(tree.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    // Boot fires :active_record before app/models.rb, which loads the gem
    // (Bundler.require) and its model; the controller, job and mailer
    // hooks fire after it, as Rails loads those bases after the
    // initializers. Each fires once.
    let boot = read("boot.rb");
    let at = |needle: &str| boot.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{boot}"));
    let hooks = at("require_relative \"runtime/active_support_lazy_load_hooks\"\nActiveSupport.run_load_hooks(:active_record,");
    let fire = at("ActiveSupport.run_load_hooks(:active_record, ActiveRecord::Base)");
    let controller = at("ActiveSupport.run_load_hooks(:action_controller, ActionController::Base)");
    let models = at("require_relative \"app/models\"\n");
    let subscriber = at("require_relative \"runtime/active_record_log_subscriber\"");
    let finisher = at("Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)");
    assert!(subscriber < hooks && hooks < fire && fire < models && models < controller && controller < finisher, "{boot}");
    for (hook, base) in [
        ("active_record", "ActiveRecord::Base"),
        ("action_controller_base", "ActionController::Base"),
        ("action_controller", "ActionController::Base"),
        ("active_job", "ActiveJob::Base"),
        ("action_mailer", "ActionMailer::Base"),
    ] {
        let line = format!("ActiveSupport.run_load_hooks(:{hook}, {base})");
        assert_eq!(boot.matches(line.as_str()).count(), 1, "{line}:\n{boot}");
    }
    let aggregator = read("app/models.rb");
    assert!(aggregator.contains("[\"models/tagging/tag\", nil], # vendor/gems/tagging-1.0.0/lib/tagging.rb:3"), "{aggregator}");
    assert!(read("app/models/tagging/tag.rb").contains("tagged :name"));

    let (emitted, _) = boot_and_run(&tree, TAGGING_PROBE);
    assert_eq!(emitted, native);
}

/// sequenced_column 1.2.1's shape, a native gem: it requires only
/// "active_support", and its `on_load(:active_record)` hook requires an
/// extension that does `extend ActiveSupport::Concern`. Core's component
/// libs require it from the Bundler.require list, after the tree fired
/// :active_record, so the hook runs at registration, as natively once
/// ActiveRecord::Base has loaded. Natively active_support.rb autoloads
/// Concern; on the tree `require "active_support"` must define it too.
const SEQ_COL_MAIN: &str = r#"require "active_support"

module SeqCol
end

ActiveSupport.on_load(:active_record) do
  require "seq_col/extension"

  include SeqCol
end
"#;

const SEQ_COL_EXTENSION: &str = r#"module SeqCol
  extend ActiveSupport::Concern

  module ClassMethods
    def seq_col(column)
      (@seq_cols ||= []) << column
    end

    def seq_cols
      @seq_cols || []
    end
  end
end
"#;

#[test]
fn a_native_gems_hook_registered_after_the_fire_runs_at_registration_with_concern() {
    let gem = write_app(
        "seq-col-gem",
        &[("seq_col/lib/seq_col.rb", SEQ_COL_MAIN), ("seq_col/lib/seq_col/extension.rb", SEQ_COL_EXTENSION)],
    );
    let probe = format!(
        "$LOAD_PATH.unshift {lib:?}\nrequire \"seq_col\"\nTagging::Tag.seq_col :position\n\
         p [ActiveRecord::Base.include?(SeqCol), ActiveRecord::Base.singleton_class.include?(SeqCol::ClassMethods), Tagging::Tag.seq_cols, Tagging::Tag.tagged_columns]\n",
        lib = gem.0.join("seq_col/lib").display().to_string()
    );
    let dir = tagging_app("seq-col", &format!("{TAGGING_NATIVE}{probe}"));
    let native = run_native(&dir.0);
    assert_eq!(native, "[true, true, [:position], [:name]]\n");

    let tree = emit_tree(&dir.0);
    let feature = std::fs::read_to_string(tree.join("runtime/features/active_support.rb")).expect("features/active_support.rb");
    assert!(feature.contains("require_relative \"../active_support_concern\"\n"), "{feature}");
    let (emitted, _) = boot_and_run(&tree, &probe);
    assert_eq!(emitted, native);
}

const WIDGETS_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    widgets (1.0.0)\n\nDEPENDENCIES\n  widgets\n";

/// graphql-shopify 0.20.3's metrics/authorized_type.rb: a module whose
/// last write follows the module it nests, and whose body reads it at
/// load, so its emitted file requires the child first.
const WIDGETS_AUTHORIZED: &str = r#"module Widgets
  module Authorized
    module ClassMethods
      def authorized_label
        "authorized"
      end
    end

    LABELS = ClassMethods.instance_methods.sort.freeze

    def self.included(base)
      base.extend(ClassMethods)
    end
  end
end
"#;

#[test]
fn an_initializer_require_of_a_gem_file_loads_the_namespace_file_before_its_child() {
    let dir = write_app(
        "source-files",
        &[
            ("roundhouse.yml", "gem_sources:\n  widgets: vendor/gems/widgets-1.0.0\ndatabase_adapter: postgresql\n"),
            ("Gemfile", "source \"https://rubygems.org\"\n\ngem \"widgets\"\n"),
            ("Gemfile.lock", WIDGETS_LOCK),
            ("config/application.rb", "Bundler.require(*Rails.groups)\n"),
            ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"),
            ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
            ("config/initializers/widgets.rb", "require \"widgets/authorized\"\n"),
            ("vendor/gems/widgets-1.0.0/lib/widgets.rb", "module Widgets\n  autoload :Authorized, \"widgets/authorized\"\nend\n"),
            ("vendor/gems/widgets-1.0.0/lib/widgets/authorized.rb", WIDGETS_AUTHORIZED),
            ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ],
    );
    let probe = "p Widgets::Authorized::LABELS\n";
    let lib = dir.0.join("vendor/gems/widgets-1.0.0/lib");
    let native = std::process::Command::new("ruby")
        .args(["-w", "-I"])
        .arg(&lib)
        .args(["-e", &format!("require \"widgets\"\nrequire \"widgets/authorized\"\n{probe}")])
        .output()
        .expect("ruby");
    assert!(native.status.success() && native.stderr.is_empty(), "{}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(native, "[:authorized_label]\n");

    let tree = emit_tree(&dir.0);
    let aggregator = std::fs::read_to_string(tree.join("app/models.rb")).expect("app/models.rb");
    assert!(
        aggregator.contains(
            "\"vendor/gems/widgets-1.0.0/lib/widgets/authorized.rb\" => [\"models/widgets/authorized\", \"models/widgets/authorized/class_methods\"],"
        ),
        "{aggregator}"
    );
    let authorized = std::fs::read_to_string(tree.join("app/models/widgets/authorized.rb")).expect("authorized.rb");
    assert!(authorized.contains("require_relative \"authorized/class_methods\""), "{authorized}");

    // Loaded child first, the child's `module Authorized` reopen ran the
    // namespace's autoload, which required the child still loading:
    // "circular require considered harmful", then ClassMethods unset.
    let (emitted, stderr) = boot_and_run(&tree, probe);
    assert!(!stderr.contains("circular require"), "{stderr}");
    assert!(!stderr.contains("initializer_statement_not_run"), "{stderr}");
    assert_eq!(emitted, native);
}
