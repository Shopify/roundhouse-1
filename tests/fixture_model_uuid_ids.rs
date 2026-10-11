//! FasterFixtures' ids in uuid columns, as Core's
//! ModelUuidFixtureCompatibility gives them
//! (components/platform/essentials/lib/platform/testing/model_uuid_fixture_compatibility.rb,
//! prepended onto FasterFixtures::FixtureSet): a model-uuid column takes
//! `ModelUuidAsInteger.serialize(identify(label, :integer))`, and an
//! explicit Integer in one is serialized the same way; any other uuid
//! column takes `identify(label, :uuid)`.
//!
//! Which columns are model-uuid is decided when the model loads (Core's
//! `model_uuid_attributes` allowlist and boot configuration), so the
//! compiled rows and accessors name the label and column, and the
//! runtime's `TestFixtures.fixture_identifier` asks the app's FixtureSet
//! `normalize_fixture_value` hook.

use std::path::{Path, PathBuf};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
  create_table "comments", force: :cascade do |t|
    t.text "body", null: false
    t.bigint "post_id", null: false
  end
  create_table "owners", id: :uuid, force: :cascade do |t|
    t.string "name"
  end
  create_table "tokens", id: :uuid, force: :cascade do |t|
    t.string "name"
    t.uuid "owner_id"
    t.uuid "holder_id"
  end
end
"#;

const TOKENS: &str = r#"red:
  name: Red
  owner: alice
  holder_id: 7
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fixture-model-uuid-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn emit() -> PathBuf {
    let app_dir = scratch("app");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app_dir);
    let write = |rel: &str, text: &str| {
        let path = app_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("app/models/owner.rb", "class Owner < ApplicationRecord\nend\n");
    write("app/models/token.rb", "class Token < ApplicationRecord\n  belongs_to :owner, optional: true\nend\n");
    // Core's loader: its presence selects FasterFixtures' rules.
    write("app/models/faster_fixtures/fixture_set.rb", "module FasterFixtures\n  class FixtureSet\n  end\nend\n");
    write("db/schema.rb", SCHEMA);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_fixture_model_uuid\n");
    write("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n");
    write("test/fixtures/tokens.yml", TOKENS);
    write("test/fixtures/owners.yml", "alice:\n  name: Alice\n");
    write(
        "test/models/token_test.rb",
        "require \"test_helper\"\n\nclass TokenTest < ActiveSupport::TestCase\n  test \"red\" do\n    assert_equal \"Red\", tokens(:red).name\n  end\nend\n",
    );
    let app = roundhouse::ingest::ingest_app(&app_dir).expect("ingest probe app");
    let out = scratch("out");
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

/// The uuid key and the `owner: alice` reference are the set's
/// `fixture_identifier` of the label; the explicit Integer stays for the
/// loader's `normalize_fixture_value`, which `model:` names.
#[test]
fn uuid_column_ids_are_the_sets_fixture_identifier() {
    let out = emit();
    let tokens = std::fs::read_to_string(out.join("test/fixtures/tokens.rb")).unwrap();
    for needle in [
        r#""id" => ActiveRecord::TestFixtures.fixture_identifier("Token", "id", "red")"#,
        r#""owner_id" => ActiveRecord::TestFixtures.fixture_identifier("Token", "owner_id", "alice")"#,
        r#""holder_id" => 7"#,
        r#"faster_fixtures: true, model: "Token")"#,
        // The accessor finds the inserted row (Rails' Fixture#find, a48a4f88b),
        // whose id is the identifier above.
        r#"@__fx_red = ActiveRecord::TestFixtures.fixture_find(Token, @__fixture_rows, 0) if @__fx_red.nil?"#,
    ] {
        assert!(tokens.contains(needle), "no {needle} in:\n{tokens}");
    }
    assert!(!tokens.contains(&roundhouse::lower::fixtures::fixture_identify_uuid("red")), "{tokens}");
}

/// The runtime half: with the app's FixtureSet hook (FasterFixtures'
/// own, and Core's ModelUuidFixtureCompatibility gate prepended, over a
/// model whose `id` and `owner_id` are model-uuid), a label is
/// `ModelUuidAsInteger.serialize(identify(label))`, an Integer is
/// serialized, and a column the model does not declare keeps
/// `identify(label, :uuid)`. Without the app's FixtureSet, nothing
/// overrides FasterFixtures.
#[test]
fn runtime_fixture_identifier_runs_the_apps_fixture_set_hook() {
    let dir = emit().join("runtime");
    let script = r##"
require_relative "active_record_test_fixtures"
TF = ActiveRecord::TestFixtures
crc = TF.identify("red")
puts "before #{TF.fixture_identifier("Token", "id", "red")}"
# components/platform/essentials/lib/shopify/model_uuid_as_integer.rb#serialize, for an Integer.
class ModelUuidAsInteger
  def serialize(value)
    hex = value.to_s(16).rjust(32, "0")
    "#{hex[0..7]}-#{hex[8..11]}-#{hex[12..15]}-#{hex[16..19]}-#{hex[20..31]}"
  end
end
class Token
  def self.model_uuid_attribute?(name) = %w[id owner_id].include?(name)
  def self.type_for_attribute(_name) = ModelUuidAsInteger.new
end
module FasterFixtures
  class FixtureSet
    private
    def normalize_fixture_value(value, _column, _model_class) = value
  end
end
# model_uuid_fixture_compatibility.rb's normalize_fixture_value and gate.
module ModelUuidFixtureCompatibility
  private
  def normalize_fixture_value(value, column, model_class)
    return super unless value.is_a?(Integer)
    return super unless column.type == :uuid && model_class&.respond_to?(:model_uuid_attribute?) && model_class.model_uuid_attribute?(column.name)
    model_class.type_for_attribute(column.name).serialize(value)
  end
end
FasterFixtures::FixtureSet.prepend(ModelUuidFixtureCompatibility)
puts "id #{TF.fixture_identifier("Token", "id", "red")}"
puts "owner #{TF.fixture_reference("alice", "uuid", true, "Token", "owner_id")}"
puts "holder #{TF.fixture_identifier("Token", "holder_id", "red")}"
puts "int #{TF.normalize_fixture_value("Token", "id", 7)}"
puts "unknown #{TF.fixture_identifier("Missing", "id", "red")}"
puts "crc #{crc}"
"##;
    std::fs::write(dir.join("probe.rb"), script).unwrap();
    let result = std::process::Command::new("ruby").arg(dir.join("probe.rb")).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    let crc = roundhouse::lower::fixtures::fixture_identify("red");
    let alice = roundhouse::lower::fixtures::fixture_identify("alice");
    let as_uuid = |i: i64| {
        let hex = format!("{i:032x}");
        format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
    };
    let red_v5 = roundhouse::lower::fixtures::fixture_identify_uuid("red");
    for line in [
        format!("before {red_v5}"),
        format!("id {}", as_uuid(crc)),
        format!("owner {}", as_uuid(alice)),
        format!("holder {red_v5}"),
        format!("int {}", as_uuid(7)),
        format!("unknown {red_v5}"),
        format!("crc {crc}"),
    ] {
        assert!(stdout.lines().any(|l| l == line), "no {line} in:\n{stdout}");
    }
}

/// Core's ActiveRecordTestHelper::Fixtures (a concern the compiler lowers
/// to a plain module) includes ActiveRecord::TestFixtures: as with
/// Concern#append_features, TestFixtures' class setup waits for the class
/// that includes the module, and sits behind the module, whose
/// `teardown_fixtures` calls `super` into it.
#[test]
fn test_fixtures_included_through_a_module_sets_up_its_class() {
    let dir = emit().join("runtime");
    let script = r##"
require_relative "active_record_test_fixtures"
module Helper
  include ActiveRecord::TestFixtures
  def teardown_fixtures = :helper
end
class Case; include Helper; end
puts "case #{Case.ancestors.take(3).inspect} #{Case.respond_to?(:fixture_table_names)} #{Case.new.teardown_fixtures}"
class Sub < Case; include Helper; end
puts "sub #{Sub.ancestors.take(3).inspect}"
"##;
    std::fs::write(dir.join("concern_probe.rb"), script).unwrap();
    let result = std::process::Command::new("ruby").arg(dir.join("concern_probe.rb")).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    assert!(stdout.contains("case [Case, Helper, ActiveRecord::TestFixtures] true helper"), "{stdout}");
    assert!(stdout.contains("sub [Sub, Case, Helper]"), "{stdout}");
}

/// FasterFixtures builds each file row with `Fixture.new(row, model_class,
/// association_map)`, whose `initialize` an app may use to rewrite the row
/// before a column is cast: the runtime runs the app's own `initialize` on
/// the row, for the set's model, and nothing when the app has none.
#[test]
fn runtime_fixture_row_hook_runs_the_apps_fixture_initializer() {
    let dir = emit().join("runtime");
    let script = r##"
require_relative "active_record_test_fixtures"
# activerecord/lib/active_record/fixtures.rb's Fixture#initialize.
class ActiveRecord::Fixture
  def initialize(fixture, model_class)
    @fixture = fixture
    @model_class = model_class
  end
end
TF = ActiveRecord::TestFixtures
row = { "body" => "text" }
TF.fixture_row_hook(row, "Doc")
puts "no app class #{row.inspect}"
module FasterFixtures
  class Fixture < ActiveRecord::Fixture
    def initialize(fixture, model_class, association_map)
      fixture["body"] = fixture["body"].upcase if model_class == "Doc" && fixture.key?("body")
      super(fixture, model_class)
    end
    private :initialize
  end
end
doc = { "body" => "text" }
other = { "body" => "text" }
bare = { "title" => "t" }
TF.fixture_row_hook(doc, "Doc")
TF.fixture_row_hook(other, "Other")
TF.fixture_row_hook(bare, "Doc")
puts "doc #{doc.inspect}"
puts "other #{other.inspect}"
puts "bare #{bare.inspect}"
"##;
    std::fs::write(dir.join("row_hook_probe.rb"), script).unwrap();
    let result = std::process::Command::new("ruby").arg(dir.join("row_hook_probe.rb")).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    for line in [
        r#"no app class {"body" => "text"}"#,
        r#"doc {"body" => "TEXT"}"#,
        r#"other {"body" => "text"}"#,
        r#"bare {"title" => "t"}"#,
    ] {
        assert!(stdout.lines().any(|l| l == line), "no {line} in:\n{stdout}");
    }
}
