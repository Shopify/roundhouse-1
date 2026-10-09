//! Fixtures load as raw table rows on the CRuby PostgreSQL tree, as Rails'
//! `insert_fixtures_set` (and Core's FasterFixtures) loads them: one
//! column → value row per record, inserted into the set's table with no
//! model built, so no writer, callback or validation runs.
//!
//! - An STI subclass's set (`_fixture: table_name: vehicles`, and one
//!   whose model alone names its base table) inserts into the base table
//!   with that table's columns, the `type` column included.
//! - A model whose writer raises still loads: the loader never calls it.
//! - Ids and references are Rails' `FixtureSet.identify` of the label.
//! - The YAML's nil, empty string, integer and mapping values stay
//!   distinct; a mapping is YAML text in a text column (Rails'
//!   `with_yaml_fallback`) and JSON in a jsonb one.
//! - A column the model ignores (`ignored_columns`, which the compiled
//!   schema leaves out) still loads: Rails and FasterFixtures take the
//!   table's columns from the database, and so does the runtime for a key
//!   the compiled columns lack. Under FasterFixtures a `keeper:` key whose
//!   `keeper_id` column is ignored is that reference.

use std::path::{Path, PathBuf};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const DATABASE_YML: &str = r#"test:
  adapter: postgresql
  encoding: unicode
  database: rh_e2e_fixture_raw_rows
"#;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
  create_table "comments", force: :cascade do |t|
    t.text "body", null: false
    t.bigint "post_id", null: false
  end
  create_table "owners", force: :cascade do |t|
    t.string "name"
    t.datetime "created_at"
  end
  create_table "vehicles", force: :cascade do |t|
    t.string "type"
    t.string "name"
    t.string "nickname"
    t.integer "wheels"
    t.bigint "owner_id"
    t.text "specs"
    t.datetime "created_at"
    t.datetime "updated_at"
  end
  create_table "garages", force: :cascade do |t|
    t.jsonb "layout"
    t.string "legacy_code"
    t.bigint "keeper_id"
  end
end
"#;

/// `_fixture: table_name:` puts Car rows in the base table.
const CARS: &str = r#"_fixture:
  table_name: vehicles
  model_class: Car

beetle:
  type: Car
  name: Beetle
  nickname:
  wheels: 4
  owner: alice
  specs:
    doors: 2
    colors: [red, blue]

mini:
  type: Car
  name: ""
  nickname: $LABEL
  wheels: "4"
"#;

/// No `_fixture:`: Rails' table is the model's, Truck's base table.
const TRUCKS: &str = r#"hauler:
  type: Truck
  name: Hauler
  wheels: 18
"#;

const OWNERS: &str = r#"alice:
  name: Alice
"#;

/// A jsonb column. The probe test never reads a garage back: the emitted
/// row hydration stringifies the driver's decoded jsonb Hash before
/// `RoundhouseJsonColumn.load` parses it, a separate read-side gap.
///
/// `legacy_code` and `keeper_id` are Garage's ignored columns.
const GARAGES: &str = r#"north:
  layout:
    bays: 3
    doors: [front, side]
  legacy_code: G-1
"#;

/// Under FasterFixtures, `keeper: alice` names the ignored `keeper_id`.
const GARAGES_FASTER_FIXTURES: &str = r#"north:
  layout:
    bays: 3
    doors: [front, side]
  legacy_code: G-1
  keeper: alice
"#;

const PROBE_TEST: &str = r#"require "test_helper"

class FixtureRawRowsTest < ActiveSupport::TestCase
  test "fixture rows load" do
    assert_equal "Beetle", cars(:beetle).name
    assert_equal "Alice", owners(:alice).name
    assert_equal "Hauler", trucks(:hauler).name
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

fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fixture-raw-rows-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture_app(tag: &str, faster_fixtures: bool) -> PathBuf {
    let app = scratch(&format!("{tag}-app"));
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app);
    let write = |rel: &str, text: &str| {
        let path = app.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("app/models/vehicle.rb", "class Vehicle < ApplicationRecord\n  belongs_to :owner, optional: true\nend\n");
    write("app/models/car.rb", "class Car < Vehicle\nend\n");
    write("app/models/truck.rb", "class Truck < Vehicle\nend\n");
    write("app/models/garage.rb", "class Garage < ApplicationRecord\n  self.ignored_columns = %w[legacy_code keeper_id]\nend\n");
    if faster_fixtures {
        // Core's loader: its presence selects FasterFixtures' rules.
        write("app/models/faster_fixtures/fixture_set.rb", "module FasterFixtures\n  class FixtureSet\n  end\nend\n");
    }
    // A writer that raises: a loader that assigned attributes would
    // never get past the first row.
    write(
        "app/models/owner.rb",
        "class Owner < ApplicationRecord\n  def name=(value)\n    raise ArgumentError, \"Owner#name= ran while loading fixtures\"\n  end\nend\n",
    );
    write("db/schema.rb", SCHEMA);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", DATABASE_YML);
    write("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n");
    write("test/fixtures/cars.yml", CARS);
    // FasterFixtures' table is `_fixture: table_name`, else the set's name.
    let trucks = if faster_fixtures { format!("_fixture:\n  table_name: vehicles\n\n{TRUCKS}") } else { TRUCKS.to_string() };
    write("test/fixtures/trucks.yml", &trucks);
    write("test/fixtures/owners.yml", OWNERS);
    write("test/fixtures/garages.yml", if faster_fixtures { GARAGES_FASTER_FIXTURES } else { GARAGES });
    write("test/models/fixture_raw_rows_test.rb", PROBE_TEST);
    app
}

fn emit(tag: &str) -> PathBuf {
    emit_with(tag, false)
}

fn emit_with(tag: &str, faster_fixtures: bool) -> PathBuf {
    let app_dir = fixture_app(tag, faster_fixtures);
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    let out = scratch(&format!("{tag}-out"));
    let files = roundhouse::project::target_files(
        &app,
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

/// Rails' `ActiveRecord::FixtureSet.identify`, checked against CRuby's
/// `Zlib.crc32(label) % (2**30 - 1)` and `Digest::UUID.uuid_v5(OID_NAMESPACE, label)`.
#[test]
fn identify_matches_rails() {
    use roundhouse::lower::fixtures::{fixture_identify, fixture_identify_uuid};
    assert_eq!(fixture_identify("one"), 980190962);
    assert_eq!(fixture_identify("snowdevil"), 26371970);
    assert_eq!(fixture_identify("cadetgear"), 506738047);
    assert_eq!(fixture_identify_uuid("snowdevil"), "dac24f35-58f1-52a5-b8fc-ed3048d1fd21");
    assert_eq!(fixture_identify_uuid("one"), "aa67c98c-d81f-5a9c-b0bc-26caa0051aea");
}

#[test]
fn sti_subclass_sets_insert_rows_into_the_base_table() {
    let out = emit("sti");
    let cars = read(&out, "test/fixtures/cars.rb");
    let beetle_id = roundhouse::lower::fixtures::fixture_identify("beetle");
    let alice_id = roundhouse::lower::fixtures::fixture_identify("alice");
    // The base table, with its columns and their casts.
    assert!(
        cars.contains(r#"ActiveRecord::TestFixtures.insert_fixture_rows("vehicles", { "id" => "integer", "type" => "string", "name" => "string", "nickname" => "string", "wheels" => "integer", "owner_id" => "integer", "specs" => "string", "created_at" => "datetime", "updated_at" => "datetime" }, ["created_at", "updated_at"], "#),
        "{cars}"
    );
    // The row: YAML order, the key generated from the label, the
    // `owner: alice` reference as the owner's identify, nil and "" kept
    // apart, the string "4" left to the integer cast, the mapping as a Hash.
    for needle in [
        format!(r#""type" => "Car""#),
        format!(r#""nickname" => nil"#),
        format!(r#""wheels" => 4"#),
        format!(r#""owner_id" => {alice_id}"#),
        format!(r#""id" => {beetle_id}"#),
        format!(r#""specs" => {{ "doors" => 2, "colors" => ["red", "blue"] }}"#),
        format!(r#""name" => """#),
        format!(r#""nickname" => "mini""#),
        format!(r#""wheels" => "4""#),
    ] {
        assert!(cars.contains(&needle), "no {needle} in:\n{cars}");
    }
    assert!(!cars.contains("\"owner\" =>"), "the reference stayed a key:\n{cars}");
    // Each accessor finds its row by the generated key, memoized.
    assert!(cars.contains(&format!("@__fx_beetle = Car.find({beetle_id}) if @__fx_beetle.nil?")), "{cars}");
    // No model is built to load the rows.
    for absent in ["Car.new", "instance.", "save_after_validation"] {
        assert!(!cars.contains(absent), "{absent} in:\n{cars}");
    }

    // A jsonb column casts the mapping as JSON. `legacy_code`, an
    // ignored column, is not in the compiled columns: the runtime finds it
    // in the live table. Under Rails' rules no `faster_fixtures:` flag.
    let garages = read(&out, "test/fixtures/garages.rb");
    let north_id = roundhouse::lower::fixtures::fixture_identify("north");
    assert!(
        garages.contains(&format!(
            r#"insert_fixture_rows("garages", {{ "id" => "integer", "layout" => "jsonb" }}, [], [{{ "layout" => {{ "bays" => 3, "doors" => ["front", "side"] }}, "legacy_code" => "G-1", "id" => {north_id} }}])"#
        )),
        "{garages}"
    );

    // Rails' own rule: Truck's table is its base class's.
    let trucks = read(&out, "test/fixtures/trucks.rb");
    assert!(trucks.contains(r#"insert_fixture_rows("vehicles", "#), "{trucks}");

    // The test helper deletes and loads each set through that table.
    let helper = read(&out, "test/test_helper.rb");
    assert!(helper.contains(r#"["cars", "CarsFixtures", "Car", "vehicles"]"#), "{helper}");
    assert!(helper.contains(r#"["trucks", "TrucksFixtures", "Truck", "vehicles"]"#), "{helper}");
}

/// Under FasterFixtures the compiled schema's ignored `keeper_id` leaves
/// `keeper: alice` unresolved at compile time: the row carries the key
/// and the `faster_fixtures:` flag, and the runtime resolves it against
/// the live table (`raw_rows_load_on_postgresql_under_faster_fixtures`).
#[test]
fn faster_fixtures_rows_leave_ignored_column_keys_to_the_live_table() {
    let out = emit_with("ff", true);
    let garages = read(&out, "test/fixtures/garages.rb");
    let north_id = roundhouse::lower::fixtures::fixture_identify("north");
    assert!(
        garages.contains(&format!(
            r#"insert_fixture_rows("garages", {{ "id" => "integer", "layout" => "jsonb" }}, [], [{{ "layout" => {{ "bays" => 3, "doors" => ["front", "side"] }}, "legacy_code" => "G-1", "keeper" => "alice", "id" => {north_id} }}], faster_fixtures: true, model: "Garage")"#
        )),
        "{garages}"
    );
    let cars = read(&out, "test/fixtures/cars.rb");
    assert!(cars.contains(r#"faster_fixtures: true, model: "Car")"#), "{cars}");
}

#[test]
fn a_model_whose_writer_raises_still_loads() {
    let out = emit("writer");
    let owners = read(&out, "test/fixtures/owners.rb");
    let alice_id = roundhouse::lower::fixtures::fixture_identify("alice");
    assert!(
        owners.contains(&format!(
            r#"ActiveRecord::TestFixtures.insert_fixture_rows("owners", {{ "id" => "integer", "name" => "string", "created_at" => "datetime" }}, ["created_at"], [{{ "name" => "Alice", "id" => {alice_id} }}])"#
        )),
        "{owners}"
    );
    assert!(!owners.contains("name ="), "the loader assigns through the writer:\n{owners}");
    assert!(!owners.contains("Owner.new"), "{owners}");
}

/// The runtime half on a real PostgreSQL: the emitted program's loader
/// inserts each row through the column casts, the authored test reads them
/// back, and the table holds exactly the cast rows afterwards.
/// `ROUNDHOUSE_PG_TEST_URL` is an admin conninfo for a disposable server;
/// the run creates and drops its own database.
#[test]
#[ignore = "requires pg gem and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn raw_rows_load_on_postgresql() {
    let stdout = run_on_postgresql(&emit("pg"));
    // Garage's ignored `legacy_code` loads from the live table's column.
    let north = roundhouse::lower::fixtures::fixture_identify("north");
    let row = format!(r#"garages: [["{north}", "{{\"bays\": 3, \"doors\": [\"front\", \"side\"]}}", "G-1", nil]]"#);
    assert!(stdout.contains(&row), "no {row} in stdout={stdout}");
}

/// The same app under FasterFixtures' rules: `keeper: alice` loads as the
/// ignored `keeper_id` column's reference, alice's identify.
#[test]
#[ignore = "requires pg gem and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn raw_rows_load_on_postgresql_under_faster_fixtures() {
    let stdout = run_on_postgresql(&emit_with("pg-ff", true));
    let north = roundhouse::lower::fixtures::fixture_identify("north");
    let alice = roundhouse::lower::fixtures::fixture_identify("alice");
    let row = format!(r#"garages: [["{north}", "{{\"bays\": 3, \"doors\": [\"front\", \"side\"]}}", "G-1", "{alice}"]]"#);
    assert!(stdout.contains(&row), "no {row} in stdout={stdout}");
}

/// Runs the emitted test through PG_DRIVER, checks the runner's result
/// and the vehicles and owners rows, and answers the driver's stdout.
fn run_on_postgresql(out: &Path) -> String {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let driver = out.join("raw_rows_driver.rb");
    std::fs::write(&driver, PG_DRIVER).unwrap();
    let result = std::process::Command::new("ruby")
        .arg(&driver)
        .arg(out)
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(!stdout.contains("fixture_set_not_loaded") && !stderr.contains("fixture_set_not_loaded"), "stdout={stdout}\nstderr={stderr}");
    // The runner's own result record: the authored test passed with its
    // 3 assertions, and no set was refused.
    let results = std::fs::read_to_string(out.join("results.jsonl")).expect("results.jsonl");
    let result: serde_json::Value = serde_json::from_str(results.trim()).expect("one result");
    assert_eq!(result["id"], "FixtureRawRowsTest#test_fixture_rows_load", "{results}");
    assert_eq!(result["status"], "pass", "{results}\nstdout={stdout}\nstderr={stderr}");
    assert_eq!(result["assertions"], 3, "{results}");
    assert!(result.get("fixture_sets_not_loaded").is_none(), "{results}");
    let beetle = roundhouse::lower::fixtures::fixture_identify("beetle");
    let alice = roundhouse::lower::fixtures::fixture_identify("alice");
    let hauler = roundhouse::lower::fixtures::fixture_identify("hauler");
    let mini = roundhouse::lower::fixtures::fixture_identify("mini");
    for row in [
        format!(r#"["{beetle}", "Car", "Beetle", nil, "4", "{alice}", "---\ndoors: 2\ncolors:\n- red\n- blue\n", "t"]"#),
        format!(r#"["{hauler}", "Truck", "Hauler", nil, "18", nil, nil, "t"]"#),
        format!(r#"["{mini}", "Car", "", "mini", "4", nil, nil, "t"]"#),
        format!(r#"owners: [["{alice}", "Alice"]]"#),
    ] {
        assert!(stdout.contains(&row), "no {row} in stdout={stdout}\nstderr={stderr}");
    }
    stdout
}

/// Creates the database and tables, runs the emitted test, prints the
/// rows the tables hold, and drops the database.
const PG_DRIVER: &str = r##"require "pg"
require "open3"
require "rbconfig"
root = File.expand_path(ARGV.fetch(0))
admin = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL"))
  .each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
db = "rh_e2e_fixture_raw_rows_#{Process.pid}"
conninfo = ->(name) { admin.merge(dbname: name).map { |k, v| "#{k}=#{v}" }.join(" ") }
with = ->(name, &blk) { c = PG.connect(conninfo.(name)); begin; blk.(c); ensure; c.close; end }
status = 1
begin
  with.(admin[:dbname] || "postgres") { |c| c.exec("DROP DATABASE IF EXISTS #{db} WITH (FORCE)"); c.exec("CREATE DATABASE #{db}") }
  with.(db) do |c|
    c.exec('CREATE TABLE "posts" ("id" bigserial PRIMARY KEY, "title" character varying NOT NULL)')
    c.exec('CREATE TABLE "comments" ("id" bigserial PRIMARY KEY, "body" text NOT NULL, "post_id" bigint NOT NULL)')
    c.exec('CREATE TABLE "owners" ("id" bigserial PRIMARY KEY, "name" character varying, "created_at" timestamp(6))')
    c.exec('CREATE TABLE "vehicles" ("id" bigserial PRIMARY KEY, "type" character varying, "name" character varying, "nickname" character varying, "wheels" integer, "owner_id" bigint, "specs" text, "created_at" timestamp(6), "updated_at" timestamp(6))')
    c.exec('CREATE TABLE "garages" ("id" bigserial PRIMARY KEY, "layout" jsonb, "legacy_code" character varying, "keeper_id" bigint)')
  end
  results = File.join(root, "results.jsonl")
  File.delete(results) if File.exist?(results)
  env = %w[RAILS_ENV RACK_ENV DATABASE_URL].to_h { |k| [k, nil] }.merge("ROUNDHOUSE_DATABASE_URL" => conninfo.(db), "ROUNDHOUSE_TEST_RESULTS" => results)
  script = "Dir.chdir(#{root.inspect})\nrequire #{File.join(root, "test/test_helper").inspect}\nload #{File.join(root, "test/models/fixture_raw_rows_test.rb").inspect}\n"
  out, err, st = Open3.capture3(env, RbConfig.ruby, "-e", script, chdir: root)
  print out
  $stderr.print err
  with.(db) do |c|
    c.exec("SELECT id, type, name, nickname, wheels, owner_id, specs::text, (created_at IS NOT NULL AND updated_at IS NOT NULL) FROM vehicles ORDER BY id").values.each { |r| puts r.inspect }
    puts "owners: #{c.exec("SELECT id, name FROM owners ORDER BY id").values.inspect}"
    puts "garages: #{c.exec("SELECT id, layout::text, legacy_code, keeper_id FROM garages ORDER BY id").values.inspect}"
    puts "next vehicle id: #{c.exec("SELECT nextval('vehicles_id_seq')").getvalue(0, 0)}"
  end
  status = st.success? ? 0 : 1
ensure
  with.(admin[:dbname] || "postgres") { |c| c.exec("DROP DATABASE IF EXISTS #{db} WITH (FORCE)") }
end
exit status
"##;
