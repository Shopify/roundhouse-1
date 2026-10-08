//! ActiveModel classes that validate, outside `app/models`, against
//! Core's pinned activemodel.
//!
//! Core keeps about 170 superclass-less classes that include
//! `ActiveModel::Validations` or `ActiveModel::Model` in `app/services`,
//! `app/validators` and other support layers (`Media::ImageProcessor`,
//! `Apps::CustomData::DeclarativeManagement::DeclaredMetaobjectDefinition`).
//! Ingest made tableless models of them only under `app/models`. Anywhere
//! else they were library classes: each `validates` was dropped with an
//! emit warning `check` never saw, and a Concern's `included do validates`
//! was refused on a "plain" includer. They are tableless models now
//! wherever they live, so `valid?`, `invalid?`, `errors.full_messages` and
//! `errors[:attr]` answer as ActiveModel does, a Concern's rules
//! included, in declaration order. A subclass adds its own rules.
//!
//! A validation that is still not lowered is a `check` error, not a
//! warning: a class with a superclass that is no model
//! (`class Promo < BaseForm; include ActiveModel::Validations`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const SIGNUP: &str = r#"module Forms
  class Signup
    include ActiveModel::Model

    attr_accessor :name, :email, :age

    validates :name, presence: true
    validates :email, format: { with: /@/ }, allow_nil: true
    validates :age, numericality: { greater_than: 17 }, allow_nil: true
    validate :not_banned

    def not_banned
      errors.add(:name, "is banned") if name == "mallory"
    end
  end
end
"#;

const VIP_SIGNUP: &str = r#"module Forms
  class VipSignup < Signup
    validates :email, presence: true
  end
end
"#;

const CODED: &str = r#"module Coded
  extend ActiveSupport::Concern

  included do
    validates :code, presence: true, length: { maximum: 4 }
  end
end
"#;

const VOUCHER: &str = r#"class Voucher
  include ActiveModel::Validations
  include Coded

  attr_reader :code, :amount

  validates :amount, numericality: { only_integer: true }

  def initialize(code, amount)
    @code = code
    @amount = amount
  end
end
"#;

const PROBE: &str = r##"class ValidationsProbe
  def self.lines
    out = []
    s = Forms::Signup.new(name: "ann", email: "a@b", age: 30)
    out << "signup ok: #{s.valid?} #{s.errors.full_messages.inspect}"
    s = Forms::Signup.new(email: "nope", age: 12)
    out << "signup bad: #{s.valid?} #{s.errors.full_messages.inspect}"
    s = Forms::Signup.new(name: "mallory")
    out << "signup banned: #{s.valid?} #{s.invalid?} #{s.errors.full_messages.inspect}"
    v = Voucher.new("AB", 5)
    out << "voucher ok: #{v.valid?} #{v.errors.full_messages.inspect}"
    v = Voucher.new("", "x")
    out << "voucher bad: #{v.valid?} #{v.errors.full_messages.inspect}"
    v = Voucher.new("TOOLONG", 2)
    out << "voucher long: #{v.valid?} #{v.errors.full_messages.inspect} #{v.errors[:code].inspect}"
    s = Forms::VipSignup.new(name: "vic")
    out << "vip bad: #{s.valid?} #{s.errors.full_messages.inspect}"
    s = Forms::VipSignup.new(name: "vic", email: "v@x")
    out << "vip ok: #{s.valid?} #{s.errors.full_messages.inspect}"
    out
  end
end
"##;

const EXPECTED: &[&str] = &[
    "signup ok: true []",
    r#"signup bad: false ["Name can't be blank", "Email is invalid", "Age must be greater than 17"]"#,
    r#"signup banned: false true ["Name is banned"]"#,
    "voucher ok: true []",
    r#"voucher bad: false ["Code can't be blank", "Amount is not a number"]"#,
    r#"voucher long: false ["Code is too long (maximum is 4 characters)"] ["is too long (maximum is 4 characters)"]"#,
    r#"vip bad: false ["Email can't be blank"]"#,
    "vip ok: true []",
];

/// In load order: a Concern before its includer, a class before its
/// subclass.
const FILES: &[(&str, &str)] = &[
    ("app/services/forms/signup.rb", SIGNUP),
    ("app/services/forms/vip_signup.rb", VIP_SIGNUP),
    ("app/services/concerns/coded.rb", CODED),
    ("app/services/voucher.rb", VOUCHER),
    ("app/models/validations_probe.rb", PROBE),
];

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\nend\n";

fn app() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

/// The files on Core's activemodel (through its activerecord).
fn native() -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_model_outside_models_{}", std::process::id()));
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activemodel is not Core's\" unless Gem.loaded_specs[\"activemodel\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         {loads}puts ValidationsProbe.lines\n"
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
    stdout.lines().map(str::to_string).collect()
}

fn emitted() -> Vec<String> {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\nputs ValidationsProbe.lines\n";
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
        std::fs::read_to_string(dir.join("app/models/voucher.rb")).unwrap_or_default()
    );
    stdout.lines().map(str::to_string).collect()
}

#[test]
fn support_layer_active_model_classes_validate_like_activemodel() {
    let native = native();
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted(), native, "emitted");
}

/// A class whose superclass is no model stays a library class, and its
/// validation is a check error at the declaration.
#[test]
fn an_unlowered_validation_is_a_check_error() {
    let (_dir, errors) = app()
        .write("app/services/base_form.rb", "class BaseForm\n  def submit\n    valid?\n  end\nend\n")
        .write(
            "app/services/promo.rb",
            "class Promo < BaseForm\n  include ActiveModel::Validations\n\n  attr_accessor :code\n\n  validates :code, presence: true\nend\n",
        )
        .emit(BuildTarget::Ruby);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(
        errors[0].contains("validation on `Promo` not lowered and dropped")
            && errors[0].contains("superclass `BaseForm` is no model"),
        "{errors:#?}"
    );
}
