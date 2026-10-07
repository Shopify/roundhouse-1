//! Rails' transactional tests in the emitted CRuby PostgreSQL program
//! (`runtime/spinel/cruby/test/active_record_test_fixtures.rb` over the
//! TransactionManager of `runtime/spinel/cruby/active_record_transactions.rb`):
//! with `use_transactional_tests` every writing pool is pinned inside a
//! non-joinable transaction for the test, a pool established during the
//! test included, and rolled back at teardown; `self.use_transactional_tests
//! = false` leaves the test's writes committed. Inside the test,
//! `transaction` nests as Rails' does: savepoints, `requires_new:`,
//! `joinable: false`, ActiveRecord::Rollback.
//!
//! The probe app has three configurations (`primary`, `animals` and
//! `extra`, the last established only by a test, as Core's
//! Podding::Database::ConnectionFactory establishes its pods) and two
//! authored test files. They run unchanged on the emitted program and on
//! native Rails 8.2 (Core's pinned Shopify/rails 5813559f4aed, with
//! ActiveRecord::TestFixtures in ActiveSupport::TestCase as rails/test_help
//! includes it) against the same local PostgreSQL databases, recreated
//! before each side; each side's answers, the transaction statements each
//! database received, and the rows each database holds afterwards must be
//! identical.

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  primary:
    <<: *default
    database: rh_e2e_txf_primary
  animals:
    <<: *default
    database: rh_e2e_txf_animals
    migrations_paths: db/animals_migrate
  extra:
    <<: *default
    database: rh_e2e_txf_extra
    database_tasks: false
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

/// Authored, unchanged on both sides. `report` prints one answer.
const TRANSACTIONAL_TEST: &str = r##"require "test_helper"

class TransactionalProbeTest < ActiveSupport::TestCase
  test "fixture transactions hold every writing pool" do
    report("start", [ActiveRecord::Base.connection.open_transactions, AnimalsRecord.connection.open_transactions, ActiveRecord::Base.connection.transaction_open?])
    ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('outer')")
    ActiveRecord::Base.transaction(requires_new: true) do
      ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('inner')")
      raise ActiveRecord::Rollback
    end
    depth = ActiveRecord::Base.transaction { ActiveRecord::Base.connection.open_transactions }
    nested = ActiveRecord::Base.transaction(joinable: false) do
      ActiveRecord::Base.transaction { ActiveRecord::Base.connection.open_transactions }
    end
    joined = ActiveRecord::Base.transaction do
      ActiveRecord::Base.transaction { ActiveRecord::Base.connection.open_transactions }
    end
    report("depths", [depth, nested, joined])
    isolated = ActiveRecord::Base.transaction(isolation: :serializable) { ActiveRecord::Base.connection.open_transactions }
    begin
      ActiveRecord::Base.transaction(isolation: :bogus) { }
    rescue ActiveRecord::TransactionIsolationError => e
      report("isolation", [isolated, e.message])
    end
    ActiveRecord::Base.transaction do
      ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('nested commit')")
      ActiveRecord::Base.transaction(requires_new: true) do
        ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('nested rollback')")
        raise ActiveRecord::Rollback
      end
    end
    begin
      ActiveRecord::Base.transaction do
        ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('raised')")
        raise ArgumentError, "boom"
      end
    rescue ArgumentError => e
      report("raised", e.message)
    end
    AnimalsRecord.connection.execute("INSERT INTO dogs (name) VALUES ('rolled back')")
    report("posts", ActiveRecord::Base.connection.select_values("SELECT title FROM posts ORDER BY id"))
    report("after", [ActiveRecord::Base.connection.open_transactions, ActiveRecord::Base.connection.transaction_open?])
  end

  test "pool established during the test is pinned" do
    pool = ActiveRecord::Base.connection_handler.establish_connection(:extra, shard: :default, role: :writing, owner_name: "probe:extra")
    report("extra", [pool.lease_connection.open_transactions, pool.lease_connection.transaction_open?])
    pool.lease_connection.execute("INSERT INTO posts (title) VALUES ('extra rolled back')")
  end

  private

  def report(label, value)
    puts "#{label}: #{value.inspect}"
  end
end
"##;

const NON_TRANSACTIONAL_TEST: &str = r##"require "test_helper"

class NonTransactionalProbeTest < ActiveSupport::TestCase
  self.use_transactional_tests = false

  test "writes are committed" do
    report("non-transactional", [ActiveRecord::Base.connection.open_transactions, AnimalsRecord.connection.open_transactions])
    ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('kept')")
    ActiveRecord::Base.transaction(requires_new: true) do
      ActiveRecord::Base.connection.execute("INSERT INTO posts (title) VALUES ('kept inner')")
      report("non-transactional depth", ActiveRecord::Base.connection.open_transactions)
    end
    AnimalsRecord.connection.execute("INSERT INTO dogs (name) VALUES ('kept')")
  end

  private

  def report(label, value)
    puts "#{label}: #{value.inspect}"
  end
end
"##;

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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("txf-pg-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// tiny-blog with ApplicationRecord on `primary`, AnimalsRecord on
/// `animals`, the dogs table, the three configurations and the two
/// authored test files.
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
        "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n\n  connects_to database: { writing: :primary }\nend\n",
    );
    write(
        "app/models/animals_record.rb",
        "class AnimalsRecord < ActiveRecord::Base\n  self.abstract_class = true\n\n  connects_to database: { writing: :animals }\nend\n",
    );
    write("app/models/dog.rb", "class Dog < AnimalsRecord\n  validates :name, presence: true\nend\n");
    write("db/schema.rb", SCHEMA);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", DATABASE_YML);
    write("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n");
    write("test/models/transactional_probe_test.rb", TRANSACTIONAL_TEST);
    write("test/models/non_transactional_probe_test.rb", NON_TRANSACTIONAL_TEST);
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

/// The compiler side: `self.use_transactional_tests = false` is the
/// class's reader, `transaction` with options and `connection_handler`
/// are not refused, and the test helper loads the transactional fixtures.
#[test]
fn transactional_tests_compile_on_the_postgresql_tree() {
    let app_dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    let refusals: Vec<_> = app.database_refusals.iter().filter(|r| r.construct != "database_adapter").collect();
    assert!(refusals.is_empty(), "{refusals:?}");
    let out = emit(&app);
    let non_transactional = read(&out, "test/models/non_transactional_probe_test.rb");
    assert!(non_transactional.contains("def self.use_transactional_tests"), "{non_transactional}");
    let transactional = read(&out, "test/models/transactional_probe_test.rb");
    assert!(!transactional.contains("use_transactional_tests"), "{transactional}");
    assert!(!transactional.contains("roundhouse: "), "{transactional}");
    let helper = read(&out, "test/test_helper.rb");
    assert!(helper.contains("require_relative \"../runtime/active_record_test_fixtures\""), "{helper}");
    assert!(out.join("runtime/active_record_transactions.rb").exists());
    assert!(out.join("runtime/active_record_test_fixtures.rb").exists());
}

/// The authored tests on the emitted program and on native Rails, same PG.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn transactional_tests_match_native_rails_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let app_dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    let out = emit(&app);
    let result = Command::new("ruby")
        .arg("tests/transactional_fixtures_pg_runtime.rb")
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
