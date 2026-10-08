//! A model's own instance `def validate`.
//!
//! ActiveModel's instance `validate` is an alias of `valid?`, so in Rails
//! a model that writes `def validate` replaces that alias with a plain
//! method: `valid?` and `save` run the `validates` / `validate :m` rules
//! and never call it, and `record.validate(...)` runs it. The native run
//! below pins that on Core's pinned activerecord.
//!
//! The lowered runtime runs a model's rules through a method of that same
//! name (`valid?` calls `validate`), so the user's method either lost to
//! the synthesized rules (silently dropped) or, on a model without rules,
//! was called by every `valid?` and `save`. Until `valid?` runs the rules
//! through a hook of its own, both are check errors at the `def`; a class
//! outside any hierarchy that declares no rules keeps its method, since no
//! synthesized `valid?` calls it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const LOG: &str = "class ValidateLog\n  def self.lines\n    @lines ||= []\n  end\nend\n";

/// Rules and its own `validate`.
const WIDGET: &str = r#"class Widget < ApplicationRecord
  validates :name, presence: true

  def validate
    ValidateLog.lines << "widget-validate"
    :mine
  end
end
"#;

/// No rules, its own `validate`.
const GIZMO: &str = r#"class Gizmo < ApplicationRecord
  def validate
    ValidateLog.lines << "gizmo-validate"
    :gizmo
  end
end
"#;

/// A tableless class with rules and a `validate` that takes arguments.
const FORM: &str = r#"class UrlsForm
  include ActiveModel::Validations
  attr_accessor :urls
  validates :urls, presence: true

  def validate(a, b)
    ValidateLog.lines << "form-validate(#{a},#{b})"
    :form
  end
end
"#;

/// A tableless class with no rules: its `validate` is the entry point.
const CHECKER: &str = r#"class Checker
  include ActiveModel::Validations

  def validate
    errors.add(:base, "checked")
    :checked
  end
end
"#;

const PROBE: &str = r#"class Probe
  def self.lines
    out = []
    log = ValidateLog.lines
    w = Widget.new
    out << "widget valid?: #{w.valid?} #{w.errors.full_messages.inspect} #{log.inspect}"
    log.clear
    out << "widget save: #{w.save} #{log.inspect}"
    log.clear
    w.name = "x"
    out << "widget save named: #{w.save} #{log.inspect}"
    log.clear
    out << "widget validate: #{w.validate.inspect} #{log.inspect}"
    log.clear
    g = Gizmo.new
    out << "gizmo save: #{g.save} #{log.inspect}"
    log.clear
    out << "gizmo validate: #{g.validate.inspect} #{log.inspect}"
    log.clear
    f = UrlsForm.new
    out << "form valid?: #{f.valid?} #{f.errors.full_messages.inspect} #{log.inspect}"
    log.clear
    out << "form validate: #{f.validate(1, 2).inspect} #{log.inspect}"
    c = Checker.new
    out << "checker validate: #{c.validate.inspect} #{c.errors.full_messages.inspect}"
    out
  end
end
"#;

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table :widgets do |t|
    t.string :name
  end
  create_table :gizmos do |t|
    t.string :name
  end
end
"#;

const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/validate_log.rb", LOG),
    ("app/models/widget.rb", WIDGET),
    ("app/models/gizmo.rb", GIZMO),
    ("app/models/urls_form.rb", FORM),
    ("app/models/checker.rb", CHECKER),
    ("app/models/probe.rb", PROBE),
];

/// What Rails answers: the rules run, the user's method never does
/// unless called.
const EXPECTED: &str = "widget valid?: false [\"Name can't be blank\"] []
widget save: false []
widget save named: true []
widget validate: :mine [\"widget-validate\"]
gizmo save: true []
gizmo validate: :gizmo [\"gizmo-validate\"]
form valid?: false [\"Urls can't be blank\"] []
form validate: :form [\"form-validate(1,2)\"]
checker validate: :checked [\"checked\"]
";

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_user_validate_method_native_{}", std::process::id()));
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
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}puts Probe.lines\n"
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
    stdout
}

#[test]
fn rails_treats_a_model_validate_method_as_a_plain_method() {
    assert_eq!(native(), EXPECTED);
}

#[test]
fn a_validate_method_the_rules_would_shadow_or_call_is_a_check_error() {
    let app = FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let app = app
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n");
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    let refused: Vec<&String> = errors.iter().filter(|e| e.contains("`def validate` not supported")).collect();
    for class in ["Widget", "Gizmo", "UrlsForm"] {
        assert!(
            refused.iter().any(|e| e.contains(&format!("on `{class}`"))),
            "{class} refused:\n{errors:#?}"
        );
    }
    assert!(!refused.iter().any(|e| e.contains("on `Checker`")), "{errors:#?}");
    assert_eq!(refused.len(), 3, "{errors:#?}");
}
