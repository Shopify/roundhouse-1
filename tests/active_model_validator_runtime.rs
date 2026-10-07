//! ActiveModel::Validator / EachValidator / BlockValidator
//! (runtime/spinel/active_model_validator.rb) against activemodel
//! 8.2.0.alpha, Core's pinned Shopify/rails 5813559f.
//!
//! Core subclasses these bases about 130 times (app/validators/*,
//! `validates_with` targets). They used to be dropped at emit as
//! unported Rails bases, so nothing that named one loaded.
//! tests/active_model_validator_runtime.rb runs the same observations natively
//! and on the port and requires every observation equal; the emitted app
//! below runs app validators the way Core writes them, and its expected
//! lines are native ActiveModel's for the same classes.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;
use std::process::Command;

#[test]
fn validators_answer_like_activemodel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/active_model_validator_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveModel and the emitted validators agree"),
        "validators diverged from activemodel\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 300, "too few observations: {}", stdout.lines().count());
    for seen in [
        "kind: [:presence_probe, :httpurl, :strict, :validator, :each, :block, nil, nil]",
        "each attributes blank: ArgumentError: :attributes cannot be blank",
        "check_validity!: ArgumentError: need :max",
        "allow {allow_blank: :a_proc} flag=false value=\"  \": [[:read, :a]]",
        "block validate: [[Rec, :a, \"x\"]]",
    ] {
        assert!(stdout.contains(seen), "missing observation {seen:?}");
    }
}

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :shops do |t|\n    t.string :name\n    t.string :domain\n  end\nend\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const SHOP: &str = r#"class Shop < ApplicationRecord
  def lowercase_report
    found = []
    LowercaseValidator.new(attributes: [:name, :domain], allow_nil: true, found: found).validate(self)
    found
  end

  def whole_report
    DomainShapeValidator.new(suffix: ".example").validate(self)
  end
end
"#;

const LOWERCASE: &str = r#"class LowercaseValidator < ActiveModel::EachValidator
  def check_validity!
    raise ArgumentError, "found: is required" unless options.key?(:found)
  end

  def validate_each(record, attribute, value)
    options[:found] << [attribute, value] unless value == value.downcase
  end
end
"#;

const DOMAIN_SHAPE: &str = r#"class DomainShapeValidator < ActiveModel::Validator
  def validate(record)
    [kind, options, record.domain.to_s.end_with?(options[:suffix])]
  end
end
"#;

const SCRIPT: &str = r#"shop = Shop.new(name: "Acme", domain: "acme.example")
p shop.lowercase_report
p Shop.new(name: "acme", domain: nil).lowercase_report
p shop.whole_report
p [LowercaseValidator.kind, DomainShapeValidator.kind, LowercaseValidator.ancestors.take(3).map(&:name)]
begin
  LowercaseValidator.new(attributes: [:name])
rescue ArgumentError => e
  p e.message
end
"#;

#[test]
fn app_validators_emit_and_run() {
    let run = emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/shop.rb", SHOP)
        .write("app/validators/lowercase_validator.rb", LOWERCASE)
        .write("app/validators/domain_shape_validator.rb", DOMAIN_SHAPE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "[[:name, \"Acme\"]]\n\
         []\n\
         [:domain_shape, {suffix: \".example\"}, true]\n\
         [:lowercase, :domain_shape, [\"LowercaseValidator\", \"ActiveModel::EachValidator\", \"ActiveModel::Validator\"]]\n\
         \"found: is required\"\n"
    );
}

const ERRORS_SHOP: &str = r#"class Shop < ApplicationRecord
  validates_with LowercaseErrorsValidator, attributes: [:name, :domain], allow_nil: true
  validates_with DomainShapeErrorsValidator, suffix: ".example", if: :check_domain?

  def check_domain?
    name != "skip"
  end
end
"#;

const LOWERCASE_ERRORS: &str = r#"class LowercaseErrorsValidator < ActiveModel::EachValidator
  def validate_each(record, attribute, value)
    record.errors.add(attribute, "must be lowercase") unless value == value.downcase
  end
end
"#;

const DOMAIN_SHAPE_ERRORS: &str = r##"class DomainShapeErrorsValidator < ActiveModel::Validator
  def validate(record)
    record.errors.add(:base, "Domain must end with #{options[:suffix]}") unless record.domain.to_s.end_with?(options[:suffix])
    record.errors.add(:domain, :invalid) if record.domain.to_s.include?(" ")
    record.errors.add(:name, :blank) if record.name.to_s.empty?
  end
end
"##;

const ERRORS_SCRIPT: &str = r#"[["Acme", "acme.example"], ["acme", "Bad Host"], ["skip", "X"], ["acme", nil], ["", "a.example"], ["acme", "acme.example"]].each do |name, domain|
  shop = Shop.new(name: name, domain: domain)
  # ActiveModel::Errors#to_a is full_messages.
  p [shop.valid?, shop.errors.to_a]
end
"#;

/// `validates_with` runs each validator with the model's validations,
/// and `record.errors.add` from a validator (a parameter field, a
/// Symbol error type) lands as Rails' full message. The expected lines
/// are native ActiveRecord 8.2.0.alpha's for the same app.
#[test]
fn validates_with_runs_validators_that_add_errors() {
    let run = emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/shop.rb", ERRORS_SHOP)
        .write("app/validators/lowercase_errors_validator.rb", LOWERCASE_ERRORS)
        .write("app/validators/domain_shape_errors_validator.rb", DOMAIN_SHAPE_ERRORS)
        .run_ruby(ERRORS_SCRIPT);
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "[false, [\"Name must be lowercase\"]]\n\
         [false, [\"Domain must be lowercase\", \"Domain must end with .example\", \"Domain is invalid\"]]\n\
         [false, [\"Domain must be lowercase\"]]\n\
         [false, [\"Domain must end with .example\"]]\n\
         [false, [\"Name can't be blank\"]]\n\
         [true, []]\n"
    );
}

const LOCALE_OVERRIDES: &str = "en:\n  errors:\n    messages:\n      invalid: \"looks wrong\"\n  activerecord:\n    errors:\n      models:\n        shop:\n          attributes:\n            name:\n              blank: \"needs a name\"\n";

const TYPED_ERRORS: &str = r#"class DomainShapeErrorsValidator < ActiveModel::Validator
  def validate(record)
    record.errors.add(:domain, :invalid) if record.domain.to_s.include?(" ")
    record.errors.add(:name, :blank) if record.name.to_s.empty?
  end
end
"#;

/// An app that ships locale data decides what an error TYPE reads.
/// Native ActiveRecord 8.2.0.alpha, with this en.yml on the load path,
/// prints `[false, ["Domain looks wrong", "Name needs a name"]]` for
/// `Shop.new(name: "", domain: "Bad Host")`, not Rails' defaults ("is
/// invalid", "can't be blank"), and which model the record is is only
/// known at run time. So with locale files present the type is not
/// baked: each site stays `errors.add` and is named on the residue
/// ledger with the locale data it resolves through.
#[test]
fn app_locale_data_keeps_error_types_from_baking() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rh-validator-locale-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, content) in [
        ("db/schema.rb", SCHEMA),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/shop.rb", "class Shop < ApplicationRecord\n  validates_with DomainShapeErrorsValidator\nend\n"),
        ("app/validators/domain_shape_errors_validator.rb", TYPED_ERRORS),
        ("config/locales/en.yml", LOCALE_OVERRIDES),
    ] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let mut app = roundhouse::ingest::ingest_app(&dir).expect("ingest");
    assert_eq!(app.locale_files, vec!["config/locales/en.yml".to_string()]);
    let diags = roundhouse::session::analyze_and_lower(&mut app);
    let _ = std::fs::remove_dir_all(&dir);
    let named: Vec<_> = diags
        .iter()
        .map(|d| d.message.clone())
        .filter(|m| m.contains("resolves through the app's locale data (config/locales/en.yml)"))
        .collect();
    assert_eq!(named.len(), 2, "{diags:#?}");
    assert!(named.iter().any(|m| m.contains("error type :invalid")), "{named:?}");
    assert!(named.iter().any(|m| m.contains("error type :blank")), "{named:?}");
    let validator = app
        .library_classes
        .iter()
        .find(|c| c.name.0.as_str() == "DomainShapeErrorsValidator")
        .expect("the validator is carried");
    let body = format!("{:?}", validator.methods);
    assert!(!body.contains("is invalid") && !body.contains("can't be blank"), "a default was baked: {body}");
}
