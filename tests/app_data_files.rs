//! The app's data files reach the emitted tree at their Rails.root-
//! relative paths, so app code that reads one by a path it builds runs
//! as it does on the source tree. Shopify Core's `Markets::DefaultMarkets`
//! loads `components/markets/config/countries_for_currency_overrides.yml`
//! in its class body; the emitted tree had no data files at all, and
//! boot died with Errno::ENOENT.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_class_body_reads_a_data_file_under_rails_root() {
    let run = emit_and_run::empty_app()
        .write("components/markets/config/overrides.yml", "EUR:\n  - FR\n  - DE\n")
        .write("db/data/fonts.json", "{\"serif\": [\"Georgia\"]}\n")
        .write("lib/templates/page.yml.erb", "title: <%= 1 + 1 %>\n")
        .write(
            "lib/default_markets.rb",
            "require \"yaml\"\nrequire \"json\"\nrequire \"erb\"\n\nclass DefaultMarkets\n  OVERRIDES = YAML.safe_load_file(File.join(Rails.root, \"components\", \"markets\", \"config\", \"overrides.yml\")).freeze\n  FONTS = JSON.parse(File.read(File.join(Rails.root, \"db\", \"data\", \"fonts.json\"))).freeze\n  PAGE = YAML.safe_load(ERB.new(File.read(File.join(Rails.root, \"lib\", \"templates\", \"page.yml.erb\"))).result).freeze\nend\n",
        )
        .run_ruby("p DefaultMarkets::OVERRIDES\np DefaultMarkets::FONTS\np DefaultMarkets::PAGE\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "{\"EUR\" => [\"FR\", \"DE\"]}\n{\"serif\" => [\"Georgia\"]}\n{\"title\" => 2}\n");
}

#[test]
fn secrets_credentials_keys_and_type_declarations_are_not_carried() {
    let (emitted, _) = emit_and_run::empty_app()
        .write("config/data.yml", "a: 1\n")
        .write("config/credentials.yml", "secret: x\n")
        .write("config/credentials/test.yml", "secret: x\n")
        .write("config/master.key", "x\n")
        .write("config/settings.ejson", "{}\n")
        .write("config/secrets.test.json", "{}\n")
        .write("config/secrets.development.json", "{}\n")
        .write("config/SECRETS/data.yml", "a: 1\n")
        .write("config/api_token.yml", "a: 1\n")
        .write("db/data/passwords.json", "{}\n")
        .write("config/private_keys.yml", "a: 1\n")
        .write("config/vault.json", "{}\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
        .write("sorbet/config.yml", "a: 1\n")
        .write("vendor/gems/g/data.yml", "a: 1\n")
        .write("lib/thing.rb", "class Thing\nend\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(emitted.join("config/data.yml").is_file());
    assert!(emitted.join("config/vault.json").is_file());
    for path in [
        "config/credentials.yml",
        "config/credentials/test.yml",
        "config/master.key",
        "config/settings.ejson",
        "config/secrets.test.json",
        "config/secrets.development.json",
        "config/SECRETS/data.yml",
        "config/api_token.yml",
        "db/data/passwords.json",
        "config/private_keys.yml",
        "config/database.yml",
        "sorbet/config.yml",
        "vendor/gems/g/data.yml",
    ] {
        assert!(!emitted.join(path).exists(), "{path} was carried");
    }
}

/// A slice carries the non-Ruby files its manifest lists as data, any
/// extension: Core's `CheckoutQueue::Lua::ScriptRunner` reads
/// `File.join(SCRIPT_PATH, "enter_queue.lua")` (`SCRIPT_PATH =
/// File.expand_path(__dir__)`) and `CheckoutQueue::Middleware` a
/// `.graphql` schema beside it, in their class bodies; both boots raised
/// Errno::ENOENT in the r17-pre2 survey. A non-YAML file the manifest
/// does not list stays out.
#[test]
fn a_slice_carries_the_data_files_its_manifest_names_whatever_the_extension() {
    use roundhouse::diagnostic::Severity;
    let base = std::env::temp_dir().join(format!("roundhouse-slice-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("slice/root");
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("lib/queue/lua/enter_queue.lua", "return redis.call('GET', KEYS[1])\n");
    write("lib/queue/schema/queue.graphql", "type Query { position: Int }\n");
    write("lib/queue/notes.txt", "unlisted\n");
    write(
        "lib/queue/script_runner.rb",
        "module Lineup\n  class ScriptRunner\n    SCRIPT_PATH = File.expand_path(\"lua\", __dir__)\n    ENTER = File.read(File.join(SCRIPT_PATH, \"enter_queue.lua\"))\n    ROOT_DIR = __dir__\n    SCHEMA = File.read(\"#{ROOT_DIR}/schema/queue.graphql\")\n  end\nend\n",
    );
    let entry = |path: &str, role: &str| format!("{{\"role\": {role:?}, \"core_path\": {path:?}, \"slice_path\": {path:?}}}");
    let manifest = format!(
        "{{\"files\": [{}, {}, {}]}}",
        entry("lib/queue/script_runner.rb", "autoload"),
        entry("lib/queue/lua/enter_queue.lua", "data"),
        entry("lib/queue/schema/queue.graphql", "boot-config"),
    );
    std::fs::write(base.join("slice/slice-manifest.json"), manifest).unwrap();

    let mut app = roundhouse::ingest::ingest_app(&root).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&app, &root, roundhouse::project::BuildTarget::Ruby)
    });
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.message.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let files = files.expect("target files");
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).filter(|p| p.starts_with("lib/queue/")).collect();
    assert_eq!(paths, vec!["lib/queue/lua/enter_queue.lua", "lib/queue/schema/queue.graphql"]);

    let emitted = base.join("emitted");
    roundhouse::project::write_to_dir(&files, &emitted).expect("write tree");
    let out = emit_and_run::ruby()
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .args(["-e", "require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\np Lineup::ScriptRunner::ENTER\np Lineup::ScriptRunner::SCHEMA\n"])
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&base);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "\"return redis.call('GET', KEYS[1])\\n\"\n\"type Query { position: Int }\\n\"\n");
}

/// A Lua script Core reads on Rails.root in a class body: essentials'
/// RedisLocking::SharedLock builds `Sabertooth::Script.from_file(
/// Rails.root.join("lib/redis_scripts/acquire_shared_locks.lua"))` at
/// load. The slicer (v3.36) lists it as data; the emit carries it at its
/// Rails.root-relative path, and the emitted `Rails.root.join` finds it
/// there. Before v3.36 the file was not carried and the body raised
/// Errno::ENOENT inside every StatsD assertion that reached the lock.
#[test]
fn a_rails_root_lua_script_read_at_load_resolves_in_the_tree() {
    use roundhouse::diagnostic::Severity;
    let base = std::env::temp_dir().join(format!("roundhouse-slice-lua-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("slice/root");
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("lib/redis_scripts/acquire_shared_locks.lua", "return redis.call('SET', KEYS[1], ARGV[1])\n");
    write("lib/redis_scripts/unread.lua", "return 0\n");
    write(
        "app/models/shared_lock.rb",
        "class SharedLock\n  ACQUIRE = File.read(Rails.root.join(\"lib/redis_scripts/acquire_shared_locks.lua\"))\nend\n",
    );
    let entry = |path: &str, role: &str| format!("{{\"role\": {role:?}, \"core_path\": {path:?}, \"slice_path\": {path:?}}}");
    let manifest = format!(
        "{{\"files\": [{}, {}]}}",
        entry("app/models/shared_lock.rb", "autoload"),
        entry("lib/redis_scripts/acquire_shared_locks.lua", "data"),
    );
    std::fs::write(base.join("slice/slice-manifest.json"), manifest).unwrap();

    let mut app = roundhouse::ingest::ingest_app(&root).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&app, &root, roundhouse::project::BuildTarget::Ruby)
    });
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.message.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let files = files.expect("target files");
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).filter(|p| p.starts_with("lib/redis_scripts/")).collect();
    assert_eq!(paths, vec!["lib/redis_scripts/acquire_shared_locks.lua"]);

    let emitted = base.join("emitted");
    roundhouse::project::write_to_dir(&files, &emitted).expect("write tree");
    let out = emit_and_run::ruby()
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .args(["-e", "require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\np SharedLock::ACQUIRE\n"])
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&base);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "\"return redis.call('SET', KEYS[1], ARGV[1])\\n\"\n");
}

/// A binary data file the manifest names is carried byte for byte: Core's
/// `Shopify::GeoIP` opens `GeoIP2-City.mmdb` from
/// `components/platform/test/fixtures/geoip` (a MaxMind test database,
/// not UTF-8), and raised "Missing geoip database" without it. A binary
/// file the manifest does not list, and one named for secrets, stay out.
#[test]
fn a_slice_carries_a_binary_data_file_its_manifest_names() {
    let base = std::env::temp_dir().join(format!("roundhouse-slice-binary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("slice/root");
    let bytes: &[u8] = &[0xAB, 0xCD, 0x00, 0xFF, 0xFE, 0x80];
    let write = |rel: &str, content: &[u8]| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("fixtures/geoip/City.mmdb", bytes);
    write("fixtures/geoip/Unlisted.mmdb", bytes);
    write("fixtures/geoip/signing.key", bytes);
    write("app/models/geo.rb", b"class Geo\nend\n");
    let entry = |path: &str, role: &str| format!("{{\"role\": {role:?}, \"core_path\": {path:?}, \"slice_path\": {path:?}}}");
    let manifest = format!(
        "{{\"files\": [{}, {}, {}]}}",
        entry("app/models/geo.rb", "autoload"),
        entry("fixtures/geoip/City.mmdb", "data"),
        entry("fixtures/geoip/signing.key", "data"),
    );
    std::fs::write(base.join("slice/slice-manifest.json"), manifest).unwrap();

    let app = roundhouse::ingest::ingest_app(&root).expect("ingest");
    let carried: Vec<(&str, &[u8])> = app
        .binary_assets
        .iter()
        .filter(|(p, _)| p.starts_with("fixtures/"))
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(carried, vec![("fixtures/geoip/City.mmdb", bytes)]);
}

/// A Cusco client built without `schema_file:` reads its schema from
/// `<caller dir>/<caller name>/schema.graphql` (`caller_locations`). The
/// emit moves the class to `app/models/<name>.rb`, so the schema the
/// slicer (v3.41) carries beside the authored file is also written
/// beside the emitted one: Core's `TaxesApi` raised "schema.graphql or
/// schema.json must exist in .../app/models/taxes_api" in four tests.
/// A schema in a directory no class file is named for is not copied.
#[test]
fn a_client_schema_beside_its_authored_class_file_is_also_beside_the_emitted_one() {
    use roundhouse::diagnostic::Severity;
    let base = std::env::temp_dir().join(format!("roundhouse-client-schema-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("slice/root");
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("lib/clients/lineup_api.rb", "module LineupApi\n  def self.schema_file\n    dir = File.dirname(caller_locations(1, 1).first.absolute_path)\n    File.join(dir, \"lineup_api\", \"schema.graphql\")\n  end\n  SCHEMA = File.read(schema_file)\nend\n");
    write("lib/clients/lineup_api/schema.graphql", "type Query { position: Int }\n");
    write("lib/clients/other/schema.graphql", "type Query { other: Int }\n");
    let entry = |path: &str, role: &str| format!("{{\"role\": {role:?}, \"core_path\": {path:?}, \"slice_path\": {path:?}}}");
    let manifest = format!(
        "{{\"files\": [{}, {}, {}]}}",
        entry("lib/clients/lineup_api.rb", "autoload"),
        entry("lib/clients/lineup_api/schema.graphql", "data"),
        entry("lib/clients/other/schema.graphql", "data"),
    );
    std::fs::write(base.join("slice/slice-manifest.json"), manifest).unwrap();

    let mut app = roundhouse::ingest::ingest_app(&root).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&app, &root, roundhouse::project::BuildTarget::Ruby)
    });
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.message.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let files = files.expect("target files");
    let has = |p: &str| files.iter().any(|(path, _)| path == p);
    assert!(has("lib/clients/lineup_api/schema.graphql"));
    assert!(has("app/models/lineup_api/schema.graphql"), "no schema beside the emitted class file");
    assert!(!has("app/models/other/schema.graphql"));

    let emitted = base.join("emitted");
    roundhouse::project::write_to_dir(&files, &emitted).expect("write tree");
    let out = emit_and_run::ruby()
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .args(["-e", "require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\np LineupApi::SCHEMA\n"])
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&base);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "\"type Query { position: Int }\\n\"\n");
}
