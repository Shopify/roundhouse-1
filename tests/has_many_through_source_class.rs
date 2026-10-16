//! `has_many :xs, through: :links, source: :x`: the class is the source
//! association's (`belongs_to :x, class_name: "Profiles::Profile"` on the
//! through model), not one camelized from the source name (activerecord
//! reflection.rb `ThroughReflection#klass`), and the reader joins the
//! through model. Native activerecord (tests/support/rails_oracle.rb)
//! and emitted CRuby must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "accounts", force: :cascade do |t|
    t.string "name"
  end
  create_table "profiles", force: :cascade do |t|
    t.string "label"
  end
  create_table "account_links", force: :cascade do |t|
    t.integer "account_id"
    t.integer "main_profile_id"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "account",
        "class Account < ApplicationRecord\n  has_many :account_links\n  has_many :main_profiles, through: :account_links, source: :main_profile\n  has_many :main_profile_rows, through: :account_links, source: :main_profile\nend\n",
    ),
    (
        "account_link",
        "class AccountLink < ApplicationRecord\n  belongs_to :account\n  belongs_to :main_profile, class_name: \"Profiles::Profile\", optional: true\nend\n",
    ),
    (
        "profiles/profile",
        "module Profiles\n  class Profile < ApplicationRecord\n    self.table_name = \"profiles\"\n  end\nend\n",
    ),
];

const PROBE: &str = r##"module HasManyThroughProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    account = Account.create!(name: "a")
    bare = Account.create!(name: "b")
    first = Profiles::Profile.create!(label: "p1")
    second = Profiles::Profile.create!(label: "p2")
    AccountLink.create!(account: account, main_profile: first)
    AccountLink.create!(account: account, main_profile: second)
    AccountLink.create!(account: bare)
    show("reader") { Account.find(account.id).main_profiles.map(&:label).sort }
    show("reader class") { Account.find(account.id).main_profiles.first.class.name }
    show("second name") { Account.find(account.id).main_profile_rows.map(&:label).sort }
    show("no linked row") { Account.find(bare.id).main_profiles.to_a }
    show("unsaved owner") { Account.new.main_profiles.to_a }
    show("count") { Account.find(account.id).main_profiles.count }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/has_many_through_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_has_many_through_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("has_many_through_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        let path = dir.join(format!("{name}.rb"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"has_many_through_probe\"\n\
         HasManyThroughProbe.run\n"
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
fn a_has_many_through_reads_the_source_associations_class() {
    let run = app().run_ruby("HasManyThroughProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted has_many through vs native activerecord");
}
