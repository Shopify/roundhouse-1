//! An association's save runs through Rails' public entry point
//! `autosave_associated_records_for_<name>`, so a model that wraps it at
//! class load changes what its save runs, against Core's pinned
//! activerecord on SQLite and on the same local PostgreSQL database.
//!
//! Rails defines that method for every has_many and has_one
//! (`define_non_cyclic_method`) and registers it by name as the
//! after_create / after_update callback; the collection's reads whether
//! the owner was new from `@new_record_before_save`, which
//! `around_save_collection_association` sets for the save and restores
//! after it. Core's `Podding::Database::DeferCallback` aliases it away and
//! defines a replacement that defers the save to after commit
//! (`Shop.defer_autosave_to_after_transaction :users, :conversion, ...`).
//! The emitted models saved through a private name only, so Shop's class
//! load raised "undefined method 'autosave_associated_records_for_users'
//! for class 'Shop'" at that alias.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

/// The wrap run at class load by the model's own class macro.
const SHOP: &str = r##"class Shop < ApplicationRecord
  has_many :users
  has_one :profile

  def self.log
    @log ||= []
  end

  def self.log_autosave(name)
    method_name = :"autosave_associated_records_for_#{name}"
    without = :"without_log_#{method_name}"
    alias_method(without, method_name)
    define_method(method_name) do
      Shop.log << "#{name} new_record_before_save=#{instance_variable_get(:@new_record_before_save).inspect} persisted=#{persisted?}"
      send(without)
    end
  end

  log_autosave :users
end
"##;

const USER: &str = r##"class User < ApplicationRecord
  belongs_to :shop
  validates :name, presence: true
end
"##;

const PROFILE: &str = r##"class Profile < ApplicationRecord
  belongs_to :shop
end
"##;

/// Core's shape (`Podding::Database::DeferCallback`, its class methods
/// verbatim but for the deferral, `defer_call` here): a concern's class method aliases
/// and redefines the entry points, deferring the save to after commit.
const DEFER_AUTOSAVE: &str = r##"module DeferAutosave
  extend ActiveSupport::Concern

  included do
    before_save :setup_deferred
    after_commit :finish_deferred
  end

  def setup_deferred
    @deferred = [] if @deferred.nil? #: Array[Symbol]?
  end

  def finish_deferred
    deferred = @deferred || [] #: Array[Symbol]
    @deferred = nil
    deferred.each { |method| send(method) }
  end

  def defer_call(method)
    return false if @deferred.nil?

    @deferred << method
    true
  end

  module ClassMethods
    def defer_autosave_to_after_transaction(*names)
      names.each do |name|
        method_name = :"autosave_associated_records_for_#{name}"
        defer_method_to_after_transaction(method_name)
      end
    end

    def defer_method_to_after_transaction(method_name)
      method_name_without_defer = :"without_defer_#{method_name}"
      return if self.method_defined?(method_name_without_defer)

      self.alias_method(method_name_without_defer, method_name)
      self.define_method(method_name) do
        if defer_call(method_name_without_defer)
          Store.log << "deferred #{method_name} new_record_before_save=#{instance_variable_get(:@new_record_before_save).inspect}"
        else
          self.send(method_name_without_defer)
        end
      end
    end
  end
end
"##;

const STORE: &str = r##"class Store < ApplicationRecord
  include DeferAutosave

  has_many :items
  has_one :sign

  def self.log
    @log ||= []
  end

  defer_autosave_to_after_transaction :items, :sign
end
"##;

const ITEM: &str = r##"class Item < ApplicationRecord
  belongs_to :store
  validates :name, presence: true
end
"##;

const SIGN: &str = r##"class Sign < ApplicationRecord
  belongs_to :store
end
"##;

const PROBE: &str = r##"class AutosaveEntryProbe
  def self.lines
    out = []
    out << "public: #{Shop.public_method_defined?(:autosave_associated_records_for_users)} #{Shop.public_method_defined?(:autosave_associated_records_for_profile)} #{Shop.method_defined?(:without_log_autosave_associated_records_for_users)}"
    shop = Shop.new(name: "a")
    shop.users.build(name: "u1")
    shop.save!
    out << "create: #{Shop.log.inspect} #{User.where(shop_id: shop.id).count}"
    Shop.log.clear
    shop.users.build(name: "u2")
    shop.name = "b"
    shop.save!
    out << "update: #{Shop.log.inspect} #{User.where(shop_id: shop.id).count}"
    Shop.log.clear
    out << "after save: #{shop.instance_variable_get(:@new_record_before_save).inspect}"
    bad = Shop.new(name: "bad")
    bad.users.build(name: "")
    out << "invalid: #{bad.save} #{Shop.log.inspect} #{bad.errors.full_messages.inspect}"
    Shop.log.clear

    Profile.create!(shop: shop, bio: "x")
    shop = Shop.find(shop.id)
    profile = shop.profile
    profile.bio = "changed"
    shop.name = "c"
    shop.save!
    out << "has_one unchanged not saved: #{Profile.find(profile.id).bio}"
    profile.shop_id = 0
    shop.name = "d"
    shop.save!
    out << "has_one key changed saved: #{Profile.find(profile.id).bio} #{Profile.find(profile.id).shop_id == shop.id}"
    Shop.log.clear

    store = Store.new(name: "s")
    store.items.build(name: "i1")
    store.save!
    out << "deferred create: #{Store.log.inspect} #{Item.where(store_id: store.id).count}"
    Store.log.clear
    store.items.build(name: "i2")
    store.name = "t"
    store.save!
    out << "deferred update: #{Store.log.inspect} #{Item.where(store_id: store.id).count}"
    Store.log.clear
    out << "outside save: #{store.autosave_associated_records_for_items.nil?.inspect} #{Store.log.inspect}"
    out
  end
end
"##;

/// Core's activerecord on SQLite; the emitted trees must print the same
/// (the tests compare with these too, on SQLite and PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: &[&str] = &[
    "public: true true true",
    "create: [\"users new_record_before_save=true persisted=true\"] 1",
    "update: [\"users new_record_before_save=false persisted=true\"] 2",
    "after save: false",
    "invalid: false [] [\"Users is invalid\"]",
    "has_one unchanged not saved: x",
    "has_one key changed saved: changed true",
    "deferred create: [\"deferred autosave_associated_records_for_items new_record_before_save=true\", \"deferred autosave_associated_records_for_sign new_record_before_save=true\"] 1",
    "deferred update: [\"deferred autosave_associated_records_for_items new_record_before_save=false\", \"deferred autosave_associated_records_for_sign new_record_before_save=false\"] 2",
    "outside save: false []",
];

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "shops", force: :cascade do |t|
    t.string "name"
  end
  create_table "users", force: :cascade do |t|
    t.integer "shop_id"
    t.string "name"
  end
  create_table "profiles", force: :cascade do |t|
    t.integer "shop_id"
    t.string "bio"
  end
  create_table "stores", force: :cascade do |t|
    t.string "name"
  end
  create_table "items", force: :cascade do |t|
    t.integer "store_id"
    t.string "name"
  end
  create_table "signs", force: :cascade do |t|
    t.integer "store_id"
    t.string "text"
  end
end
"#;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
CREATE TABLE users (
    id bigserial NOT NULL,
    shop_id bigint,
    name character varying,
    CONSTRAINT users_pkey PRIMARY KEY (id)
);
CREATE TABLE profiles (
    id bigserial NOT NULL,
    shop_id bigint,
    bio character varying,
    CONSTRAINT profiles_pkey PRIMARY KEY (id)
);
CREATE TABLE stores (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT stores_pkey PRIMARY KEY (id)
);
CREATE TABLE items (
    id bigserial NOT NULL,
    store_id bigint,
    name character varying,
    CONSTRAINT items_pkey PRIMARY KEY (id)
);
CREATE TABLE signs (
    id bigserial NOT NULL,
    store_id bigint,
    text character varying,
    CONSTRAINT signs_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str =
    "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_autosave_entry\n";

const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/concerns/defer_autosave.rb", DEFER_AUTOSAVE),
    ("app/models/shop.rb", SHOP),
    ("app/models/user.rb", USER),
    ("app/models/profile.rb", PROFILE),
    ("app/models/store.rb", STORE),
    ("app/models/item.rb", ITEM),
    ("app/models/sign.rb", SIGN),
    ("app/models/autosave_entry_probe.rb", PROBE),
];

/// The model files on Core's activerecord over in-memory SQLite.
fn native_sqlite() -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_autosave_entry_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}puts AutosaveEntryProbe.lines\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout.lines().map(str::to_string).collect()
}

/// The same files emitted on the SQLite CRuby tree.
fn emitted_sqlite() -> Vec<String> {
    let app = FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n");
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\nputs AutosaveEntryProbe.lines\n";
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(dir.join("app/models/shop.rb")).unwrap_or_default()
    );
    stdout.lines().map(str::to_string).collect()
}

#[test]
fn a_wrapped_autosave_entry_point_is_what_the_save_runs_on_sqlite() {
    let native = native_sqlite();
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_sqlite(), native, "emitted");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_wrapped_autosave_entry_point_is_what_the_save_runs_on_postgresql() {
    let mut files = FILES.to_vec();
    files.extend([
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
    ]);
    let (emitted, errors) =
        files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text)).emit(BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let loads: Vec<&str> = FILES.iter().map(|(path, _)| path.trim_end_matches(".rb")).collect();
    let (native, emitted) = pg_probe::native_and_emitted(
        "autosave-entry",
        &files,
        &emitted,
        "rh_e2e_autosave_entry",
        "AutosaveEntryProbe",
        &loads,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
