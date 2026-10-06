//! Boot-time `config/database.yml` resolution (`ingest::database_yml`,
//! `runtime/spinel/cruby/database_configurations.rb`).
//!
//! The template's ERB compiles into the app (`DatabaseYml.render`, calling
//! the app's own `lib/` code); the emitted program renders, parses and
//! resolves it at boot. The oracle tests compare that resolution, as JSON
//! and `inspect` (symbol vs string keys, Integer vs String), with native
//! Rails 8.1.4 reading the same file (`tests/database_yml_oracle.rb`:
//! `Rails::Application::Configuration#database_configuration` and
//! `ActiveRecord::DatabaseConfigurations`, with the same lib file loaded
//! plainly; no app boot). Native Rails runs only in the oracle.

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const DB_SETTINGS: &str = r#"module DbSettings
  def self.ticket_timeout
    ENV["RESQUE_WORKER_NAME"] ? 1 : 0
  end

  def self.flavor
    ENV.fetch("DB_FLAVOR", "postgresql")
  end

  def self.user_for(role)
    "app_#{role}"
  end
end
"#;

/// Anchors, `<<` merges (two in one mapping, and nested under `semian:`),
/// ERB calling app lib code, ERB inside an alias name, `if`/`else`, a
/// `<%#` comment, `<%%`, text that looks like interpolation, `shared:`,
/// a three-tier environment with a replica and a hidden configuration.
const DATABASE_YML: &str = r#"<%# Boot-time database configuration fixture. %>
semian_common: &semian_common
  timeout: <%= DbSettings.ticket_timeout %>
  success_threshold: 2
  error_threshold: 3
  quota: <%= 0.5 %>

semian_static: &semian_static
  <<: *semian_common
  tickets: <%= ENV.fetch("SEMIAN_TICKETS", "4").to_i %>

flavor_postgresql: &flavor_postgresql
  adapter: postgresql
  encoding: unicode
  options: "-c IntervalStyle=iso_8601 -c TimeZone=UTC"
  schema_search_path: "runtime_test,public"
  note: "literal #{not_interpolated} <%% kept"

flavor_other: &flavor_other
  adapter: postgresql
  schema_search_path: other

flavor_sqlite: &flavor_sqlite
  adapter: sqlite3

default: &default
  pool: 3
  variables:
    statement_timeout: 5000

shared:
  connect_timeout: 2

test:
  primary_readonly:
    <<: *default
    <<: *flavor_postgresql
    database: fixture_test
    replica: true
  primary:
    <<: *default
<% if ENV["FIXTURE_ALT"] %>
    <<: *flavor_other
<% else %>
    <<: *<%= "flavor_#{DbSettings.flavor}" %>
    username: <%= DbSettings.user_for(:writer) %>
<% end %>
    database: fixture_test
    semian:
      <<: *semian_static
      name: master
  archive:
    <<: *flavor_postgresql
    database: fixture_archive
    database_tasks: false
    migrations_paths: db/archive_migrate

development:
  <<: *default
  <<: *flavor_postgresql
  database: fixture_dev
"#;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "roundhouse-dbyml-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// tiny-blog plus an ApplicationRecord, `lib/db_settings.rb`,
/// `roundhouse.yml` (`database_adapter: postgresql`) and `database_yml`.
fn fixture_app(database_yml: &str) -> TempDir {
    let app = temp_dir("app");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app.0);
    let write = |rel: &str, text: &str| {
        let path = app.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("lib/db_settings.rb", DB_SETTINGS);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", database_yml);
    app
}

fn ingest(app: &TempDir) -> roundhouse::app::App {
    roundhouse::ingest::ingest_app(&app.0).expect("ingest fixture")
}

/// The emitted Ruby tree, written out.
fn emit(app: &roundhouse::app::App) -> TempDir {
    let out = temp_dir("out");
    let files = roundhouse::project::target_files(
        app,
        &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"),
        roundhouse::project::BuildTarget::Ruby,
    )
    .expect("package PostgreSQL Ruby project");
    for (name, content) in files {
        let path = out.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    out
}

const SCENARIO_VARS: &[&str] = &[
    "RAILS_ENV", "RACK_ENV", "DATABASE_URL", "PRIMARY_DATABASE_URL", "PRIMARY_READONLY_DATABASE_URL",
    "ARCHIVE_DATABASE_URL", "FIXTURE_ALT", "SEMIAN_TICKETS", "RESQUE_WORKER_NAME", "DB_FLAVOR",
    "ROUNDHOUSE_DATABASE_URL",
];

fn ruby(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("ruby");
    cmd.args(args).current_dir(cwd);
    for var in SCENARIO_VARS {
        cmd.env_remove(var);
    }
    cmd.envs(env.iter().copied());
    let out = cmd.output().expect("ruby");
    assert!(
        out.status.success(),
        "ruby {args:?} in {}\nstdout={}\nstderr={}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The emitted program's resolution for the scenario: boot.rb (which loads
/// the compiled app, `DatabaseYml` and `DbSettings` included, and never
/// connects), then `DatabaseConfigurations.load` for the boot env.
fn emitted_report(out: &TempDir, env: &[(&str, &str)]) -> String {
    let report = Path::new(MANIFEST_DIR).join("tests/database_yml_report.rb");
    ruby(
        &out.0,
        &[
            "-e",
            r#"require "./boot"
require ARGV.fetch(0)
out = begin
  env = DatabaseConfiguration.default_env
  DatabaseYmlReport.report(DatabaseConfigurations.load(renderer: DatabaseConfiguration.renderer, path: DatabaseConfiguration::PATH, env: env), env)
rescue Exception => e
  DatabaseYmlReport.error(e)
end
puts JSON.generate(out)"#,
            report.to_str().unwrap(),
        ],
        env,
    )
}

fn oracle_report(app: &TempDir, env: &[(&str, &str)]) -> String {
    ruby(
        Path::new(MANIFEST_DIR),
        &["tests/database_yml_oracle.rb", app.0.to_str().unwrap(), "lib/db_settings.rb"],
        env,
    )
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"))
}

#[test]
#[ignore = "requires ruby with the pg gem and native activerecord/railties 8.1.4 for the oracle"]
fn boot_resolution_matches_rails_8_1_4() {
    let app_dir = fixture_app(DATABASE_YML);
    let app = ingest(&app_dir);
    let yml = app.database_yml.as_ref().expect("database.yml compiled");
    assert_eq!(yml.renderer.as_ref().map(|c| c.0.as_str()), Some("DatabaseYml"));
    let refusals: Vec<_> = app.database_refusals.iter().filter(|r| r.construct != "database_adapter").collect();
    assert!(refusals.is_empty(), "{refusals:?}");
    let out = emit(&app);

    let url = "postgres://fixture_user:fixture_secret@db.example:6543/from_url?pool=7&replica=false&sslmode=require";
    let scenarios: Vec<Vec<(&str, &str)>> = vec![
        vec![("RAILS_ENV", "test")],
        vec![("RAILS_ENV", "test"), ("FIXTURE_ALT", "1"), ("SEMIAN_TICKETS", "8"), ("RESQUE_WORKER_NAME", "w")],
        vec![("RAILS_ENV", "test"), ("DATABASE_URL", url), ("ARCHIVE_DATABASE_URL", "postgresql://h2/archive_url?database_tasks=true")],
        vec![("RAILS_ENV", "test"), ("PRIMARY_DATABASE_URL", "postgres:///opaque?schema_dump=false&query_cache=12")],
        vec![("RAILS_ENV", "development"), ("DATABASE_URL", "postgres:///dev_from_url")],
        vec![("RACK_ENV", "test")],
        vec![("RAILS_ENV", "production")],
        vec![("RAILS_ENV", "production"), ("DATABASE_URL", "postgres://h/p")],
        vec![("RAILS_ENV", "test"), ("DB_FLAVOR", "missing")],
    ];
    for env in &scenarios {
        let expected = json(&oracle_report(&app_dir, env));
        let actual = json(&emitted_report(&out, env));
        if expected.get("error").is_some() {
            // Same failure, named: Rails' class, the port's construct.
            assert!(actual.get("error").is_some(), "{env:?}: Rails raised {expected}, emitted resolved {actual}");
            // Raised while loading the file: both carry Rails' prefix.
            let prefix = "Cannot load database configuration:\n";
            assert_eq!(
                actual["message"].as_str().unwrap().starts_with(prefix),
                expected["message"].as_str().unwrap().starts_with(prefix),
                "{env:?}: {expected} vs {actual}"
            );
            continue;
        }
        assert_eq!(actual, expected, "{env:?}");
    }

    // test/primary, the configuration the runtime opens.
    let base = json(&emitted_report(&out, &scenarios[0]));
    let primary = base["configurations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["env_name"] == "test" && c["name"] == "primary")
        .unwrap();
    assert_eq!(
        primary["hash"]["semian"],
        serde_json::json!({"timeout": 0, "success_threshold": 2, "error_threshold": 3, "quota": 0.5, "tickets": 4, "name": "master"})
    );
    assert_eq!(primary["hash"]["schema_search_path"], "runtime_test,public");
    assert_eq!(primary["hash"]["username"], "app_writer");
}

/// What the runtime decides past Rails' resolution: its one primary, the
/// compiled adapter, the harness endpoint over the entry, semian's
/// adapter being loaded. Each refusal is named.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn boot_refusals_are_named() {
    let app_dir = fixture_app(DATABASE_YML);
    let out = emit(&ingest(&app_dir));
    let probe = r#"require "./boot"
def outcome
  yield
rescue DatabaseConfigurations::Error => e
  e.construct
end
_configurations, primary = DatabaseConfiguration.resolve
puts primary.name
puts outcome { DatabaseConfigurations.connection_entry(primary, "host=/tmp port=1 dbname=x") }
require "./runtime/semian_activerecord_postgresql_adapter"
puts outcome { DatabaseConfigurations.connection_entry(primary, "host=/tmp port=1 dbname=x") }
puts outcome { DatabaseConfigurations.connection_entry(primary, "") }
entry = DatabaseConfigurations.apply_endpoint(primary.configuration_hash, "host=/tmp/sock port=25432 user=local dbname=local_db")
p entry.slice(:host, :port, :username, :database, :semian, :schema_search_path, :pool)
p entry.key?(:user) || entry.key?(:dbname)
puts outcome { DatabaseConfigurations.apply_endpoint({}, "not a = = conninfo") }
ENV["DB_FLAVOR"] = "sqlite"
puts outcome { DatabaseConfiguration.resolve }
ENV["DB_FLAVOR"] = "missing"
puts outcome { DatabaseConfiguration.resolve }
ENV.delete("DB_FLAVOR")
puts outcome { DatabaseConfiguration.resolve(env: "production") }
"#;
    let text = ruby(&out.0, &["-e", probe], &[("RAILS_ENV", "test")]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "primary", "{text}");
    assert_eq!(lines[1], "semian_adapter", "{text}");
    // semian's adapter loaded (the real port; nothing connects): the
    // entry is handed over whole.
    assert!(lines[2].starts_with('{'), "{text}");
    assert_eq!(lines[3], "database_endpoint", "{text}");
    assert_eq!(
        lines[4],
        r#"{host: "/tmp/sock", port: "25432", username: "local", database: "local_db", semian: {"timeout" => 0, "success_threshold" => 2, "error_threshold" => 3, "quota" => 0.5, "tickets" => 4, "name" => "master"}, schema_search_path: "runtime_test,public", pool: 3}"#,
        "{text}"
    );
    assert_eq!(lines[5], "false", "{text}");
    assert_eq!(lines[6], "database_endpoint", "{text}");
    assert_eq!(lines[7], "database_adapter", "{text}");
    assert_eq!(lines[8], "database_yml_alias", "{text}");
    assert_eq!(lines[9], "database_primary", "{text}");
}

/// ERB the compiled renderer would not reproduce is refused at check
/// time, at its template position; the emitted boot then refuses rather
/// than resolve without it.
#[test]
fn unsupported_erb_constructs_are_named_refusals() {
    for (body, construct, needle) in [
        ("<%- if ENV[\"X\"] %>\n<% end %>\n", "erb_trim_marker", "<%- if"),
        ("<%= 1 -%>\n", "erb_trim_marker", "<%= 1 -%>"),
        ("<%== 1 %>\n", "erb_raw_output", "<%== 1"),
        ("a: <%= self.class %>\n", "database_yml_erb_context", "self"),
        ("a: <%= @x %>\n", "database_yml_erb_context", "@x"),
        ("a: <%= __FILE__ %>\n", "database_yml_erb_context", "__FILE__"),
        ("a: <%= binding.local_variables %>\n", "database_yml_erb_context", "binding"),
        ("<% _buf = 1 %>\n", "database_yml_erb_context", "_buf"),
        ("a: <%= if %>\n", "database_yml_ruby", "if"),
        ("a: <% x\n", "erb_unterminated_tag", "<% x"),
    ] {
        let app_dir = fixture_app(&format!("test:\n  adapter: postgresql\n{body}"));
        let app = ingest(&app_dir);
        let refusal = app
            .database_refusals
            .iter()
            .find(|r| r.construct == construct)
            .unwrap_or_else(|| panic!("{body:?}: no {construct} in {:?}", app.database_refusals));
        assert!(!refusal.warning);
        let text = std::fs::read_to_string(app_dir.0.join("config/database.yml")).unwrap();
        let at = &text[refusal.span.start as usize..];
        assert!(at.starts_with(needle), "{body:?}: span at {at:?}");
        let yml = app.database_yml.as_ref().unwrap();
        assert!(yml.renderer.is_none(), "{body:?}");
        let files = roundhouse::project::target_files(
            &app,
            &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"),
            roundhouse::project::BuildTarget::Ruby,
        )
        .unwrap();
        let configuration = &files.iter().find(|(p, _)| p == "runtime/database_configuration.rb").unwrap().1;
        assert!(configuration.contains("raise DatabaseConfigurations::Error.new(\"database_yml_erb\""), "{configuration}");
        assert!(!files.iter().any(|(p, _)| p == "app/models/database_yml.rb"));
    }
}

/// Without `database_adapter:` or a manifest the app keeps the
/// single-database selection: nothing is compiled from database.yml.
#[test]
fn without_a_declared_adapter_database_yml_is_not_compiled() {
    let app_dir = fixture_app(DATABASE_YML);
    std::fs::remove_file(app_dir.0.join("roundhouse.yml")).unwrap();
    let app = ingest(&app_dir);
    assert!(app.database_yml.is_none());
    assert!(app.database_refusals.is_empty(), "{:?}", app.database_refusals);
    assert!(!app.library_classes.iter().any(|c| c.name.0.as_str() == "DatabaseYml"));
}

/// The compiled renderer's spans index the template, so a check
/// diagnostic in ERB code points into config/database.yml.
#[test]
fn renderer_spans_point_into_the_template() {
    let app_dir = fixture_app(DATABASE_YML);
    let app = ingest(&app_dir);
    let class = app.library_classes.iter().find(|c| c.name.0.as_str() == "DatabaseYml").unwrap();
    let render = class.methods.iter().find(|m| m.name.as_str() == "render").unwrap();
    let text = std::fs::read_to_string(app_dir.0.join("config/database.yml")).unwrap();
    let mut found = Vec::new();
    fn walk<'a>(e: &'a roundhouse::expr::Expr, text: &str, out: &mut Vec<String>) {
        if let roundhouse::expr::ExprNode::Send { method, .. } = &*e.node {
            if method.as_str() == "user_for" {
                out.push(text[e.span.start as usize..e.span.end as usize].to_string());
            }
        }
        e.node.for_each_child(&mut |c| walk(c, text, out));
    }
    walk(&render.body, &text, &mut found);
    assert_eq!(found, vec!["DbSettings.user_for(:writer)".to_string()]);
}

/// ERB code in config/database.yml is indexed as the top-level code
/// Rails' `ERB#result` runs, so `check` resolves its constants as it
/// does a `.rb` file's: the app's `DbSettings` resolves, and an
/// unresolved name is an error at its position in the template.
#[test]
fn unresolved_erb_constants_are_check_errors_at_the_template() {
    let yml = "test:\n  adapter: postgresql\n  username: <%= DbSettings.user_for(:writer) %>\n  password: <%= Missing::Secret.value %>\n  port: <%= Nope.port %><%= DbSettings.ticket_timeout %>\n";
    let app_dir = fixture_app(yml);
    let mut app = ingest(&app_dir);
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let text = std::fs::read_to_string(app_dir.0.join("config/database.yml")).unwrap();
    let file = app
        .database_yml
        .as_ref()
        .and_then(|_| {
            app.library_classes.iter().find(|c| c.name.0.as_str() == "DatabaseYml")
        })
        .and_then(|c| c.methods.first())
        .map(|m| m.body.span.file)
        .expect("renderer compiled");
    let mut constants: Vec<String> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.span.file == file && d.severity == roundhouse::diagnostic::Severity::Error)
        .map(|d| format!("{}: {}", &text[d.span.start as usize..d.span.end as usize], d.message))
        .collect();
    constants.sort();
    assert_eq!(
        constants,
        vec![
            "Missing::Secret: constant not supported (all targets): Missing::Secret".to_string(),
            "Nope: constant not supported (all targets): Nope".to_string(),
        ],
    );
}
