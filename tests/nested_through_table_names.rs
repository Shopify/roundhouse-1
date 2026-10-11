//! A `has_many :through` whose join model and target are nested in
//! namespaces (`class_name: "Shelving::Slots::Slot"`): Rails names a
//! nested model's table from its demodulized class name
//! (`compute_table_name`), so the through reader's join SQL must too.
//! The probe runs natively on Core's pinned activerecord over in-memory
//! SQLite (tests/support/rails_oracle.rb) and emitted on CRuby, and
//! must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "collections", force: :cascade do |t|
    t.string "title"
  end
  create_table "slots", force: :cascade do |t|
    t.string "label"
  end
  create_table "slots_collections", force: :cascade do |t|
    t.integer "slot_id"
    t.integer "collection_id"
    t.string "type"
  end
  create_table "accounts", force: :cascade do |t|
    t.string "name"
  end
  create_table "profiles", force: :cascade do |t|
    t.integer "account_id"
    t.boolean "main", default: false
  end
  create_table "badges", force: :cascade do |t|
    t.integer "profile_id"
    t.string "label"
    t.boolean "active", default: true
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    (
        "concerns/slotted",
        "module Slotted\n  extend ActiveSupport::Concern\n  included do\n    with_options foreign_key: :collection_id, inverse_of: false do\n      has_many :sti_live_slots, -> { merge(Shelving::Slots::SlotsCollection.live) }, class_name: \"Shelving::Slots::PinnedSlotsCollection\"\n      has_many :live_slots, -> { merge(Shelving::Slots::SlotsCollection.live) }, class_name: \"Shelving::Slots::SlotsCollection\"\n    end\n  end\nend\n",
    ),
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "shelving/slots/slot",
        "module Shelving\n  module Slots\n    class Slot < ApplicationRecord\n      has_many :slots_collections, class_name: \"Shelving::Slots::SlotsCollection\"\n      has_many :collections, through: :slots_collections\n    end\n  end\nend\n",
    ),
    (
        "shelving/slots/slots_collection",
        "module Shelving\n  module Slots\n    class SlotsCollection < ApplicationRecord\n      belongs_to :slot, class_name: \"Shelving::Slots::Slot\"\n      belongs_to :collection\n      scope :live, -> { where.not(slot_id: nil) }\n    end\n  end\nend\n",
    ),
    (
        "shelving/slots/pinned_slots_collection",
        "module Shelving\n  module Slots\n    class PinnedSlotsCollection < SlotsCollection\n    end\n  end\nend\n",
    ),
    (
        "account",
        "class Account < ApplicationRecord\n  has_one :primary_profile, -> { where(\"profiles.main = 1\") }, class_name: \"Profile\"\n  has_many :badges, through: :primary_profile\nend\n",
    ),
    ("profile", "class Profile < ApplicationRecord\n  belongs_to :account\n  has_many :badges, -> { merge(Badge.live) }\nend\n"),
    ("badge", "class Badge < ApplicationRecord\n  belongs_to :profile\n  scope :live, -> { where(active: true) }\nend\n"),
    (
        "collection",
        "class Collection < ApplicationRecord\n  include Slotted\n  has_many :slots_collections, class_name: \"Shelving::Slots::SlotsCollection\"\n  has_many :slots, class_name: \"Shelving::Slots::Slot\", through: :slots_collections\nend\n",
    ),
];

const PROBE: &str = r##"module ThroughProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    collection = Collection.create!(title: "c")
    slot = Shelving::Slots::Slot.create!(label: "s")
    Shelving::Slots::SlotsCollection.create!(slot: slot, collection: collection)
    show("through reader") { Collection.find(collection.id).slots.map(&:label) }
    show("through from the other side") { slot.collections.map(&:title) }
    show("through where") { collection.slots.where(label: "s").count }
    show("preload") { Collection.includes(:slots).find(collection.id).slots.map(&:label) }
    show("scoped association scope") { collection.association(:live_slots).scope.count }
    show("scoped association reader") { collection.live_slots.size }
    show("sti scoped association scope") { collection.association(:sti_live_slots).scope.count }
    show("sti scoped association reader") { collection.sti_live_slots.size }
    show("through a has_one with scopes") do
      account = Account.create!(name: "a")
      main = Profile.create!(account: account, main: true)
      other = Profile.create!(account: account, main: false)
      Badge.create!(profile: main, label: "m1")
      Badge.create!(profile: main, label: "gone", active: false)
      Badge.create!(profile: other, label: "o1")
      [Account.find(account.id).badges.map(&:label).sort, account.association(:badges).scope.count]
    end
    show("joins through") { Collection.joins(:slots).where(title: "c").count }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/through_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_nested_through_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("through_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        let path = dir.join(format!("{name}.rb"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\nrequire \"active_support/core_ext/object/with_options\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"through_probe\"\n\
         ThroughProbe.run\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn a_through_over_namespaced_models_joins_their_tables() {
    let run = app().run_ruby("ThroughProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted through reader vs native activerecord");
}
