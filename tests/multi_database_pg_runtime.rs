//! Rails' multiple-database model in the emitted CRuby PostgreSQL program
//! (`runtime/spinel/cruby/active_record_connection_handling.rb`): one pool
//! per configuration, `connects_to` binding abstract classes and their
//! descendants, `connected_to` / `while_preventing_writes` /
//! `prohibit_shard_swapping` switching role and shard.
//!
//! The probe app has four configurations (`primary` and `animals`, each with
//! a replica entry naming the same database) and two abstract classes with
//! `connects_to`: `ApplicationRecord` (Post, Comment) and `AnimalsRecord`
//! (Dog). One scenario (`tests/multi_database_pg_scenario.rb`) runs on the
//! emitted program and on native Rails 8.2 (Core's pinned Shopify/rails
//! 5813559f4aed, `tests/support/rails_oracle.rb`) against the same two local
//! PostgreSQL databases, recreated before each side; each side's answers and
//! the rows each database holds afterwards must be identical.

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  primary:
    <<: *default
    database: rh_e2e_mdb_primary
  primary_replica:
    <<: *default
    database: rh_e2e_mdb_primary
    replica: true
  animals:
    <<: *default
    database: rh_e2e_mdb_animals
    migrations_paths: db/animals_migrate
  animals_replica:
    <<: *default
    database: rh_e2e_mdb_animals
    replica: true
"#;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
  create_table "comments", force: :cascade do |t|
    t.text "body", null: false
    t.bigint "post_id", null: false
  end
  create_table "dogs", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#;

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

/// A fresh directory under the test target's scratch space, kept after
/// the run so a failure can be inspected.
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("multi-db-pg-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// tiny-blog with the two abstract classes, Dog under AnimalsRecord, the
/// dogs table, `roundhouse.yml` (postgresql) and the four configurations.
fn fixture_app() -> PathBuf {
    let app = scratch("app");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app);
    let write = |rel: &str, text: &str| {
        let path = app.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "app/models/application_record.rb",
        "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n\n  connects_to database: { writing: :primary, reading: :primary_replica }\nend\n",
    );
    write(
        "app/models/animals_record.rb",
        "class AnimalsRecord < ActiveRecord::Base\n  self.abstract_class = true\n\n  connects_to database: { writing: :animals, reading: :animals_replica }\nend\n",
    );
    write("app/models/dog.rb", "class Dog < AnimalsRecord\n  validates :name, presence: true\nend\n");
    write("db/schema.rb", SCHEMA);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", DATABASE_YML);
    app
}

fn emit(app: &roundhouse::app::App) -> PathBuf {
    let out = scratch("out");
    let files = roundhouse::project::target_files(
        app,
        &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"),
        roundhouse::project::BuildTarget::Ruby,
    )
    .expect("package PostgreSQL Ruby project");
    for (name, content) in files {
        let path = out.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    out
}

fn read(out: &Path, rel: &str) -> String {
    std::fs::read_to_string(out.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The compiler side: both `connects_to` are owned (no refusal), replayed
/// at boot, and the abstract classes stay abstract in the emitted tree.
#[test]
fn connects_to_on_two_abstract_classes_compiles_without_refusal() {
    let app_dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    // `database_adapter` is the check-time note that only the CRuby
    // target carries a PostgreSQL driver (as in tests/database_yml_boot.rs).
    let refusals: Vec<_> = app.database_refusals.iter().filter(|r| r.construct != "database_adapter").collect();
    assert!(refusals.is_empty(), "{refusals:?}");
    let out = emit(&app);
    let configuration = read(&out, "runtime/database_configuration.rb");
    assert!(
        configuration.contains(r#"OWNERSHIP = [["AnimalsRecord", "animals", "animals_replica"], ["ApplicationRecord", "primary", "primary_replica"]]"#)
            || configuration.contains(r#"OWNERSHIP = [["ApplicationRecord", "primary", "primary_replica"], ["AnimalsRecord", "animals", "animals_replica"]]"#),
        "{configuration}"
    );
    assert!(configuration.contains("ActiveRecord::Base.establish_connection"), "{configuration}");
    assert!(out.join("runtime/active_record_connection_handling.rb").exists());
    for model in ["app/models/application_record.rb", "app/models/animals_record.rb"] {
        let text = read(&out, model);
        assert!(text.contains("self.abstract_class = true"), "{model}: {text}");
    }
}

/// The scenario on the emitted program and on native Rails, same PG.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn multiple_databases_route_like_native_rails_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let app_dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    let out = emit(&app);
    let result = Command::new("ruby")
        .arg("tests/multi_database_pg_runtime.rb")
        .arg(&app_dir)
        .arg(&out)
        .current_dir(MANIFEST_DIR)
        .output()
        .expect("ruby");
    // The answers both sides gave, for the run log.
    println!("{}", String::from_utf8_lossy(&result.stdout));
    assert!(
        result.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
