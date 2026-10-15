//! A class body's `with_options(optional: true) do attribute(:x, :string)
//! end` over a class macro an app superclass defines runs as written, on
//! the option merger the validations port carries: it was dropped as "not
//! modelled", and the attribute readers it declared were missing (Core's
//! `Customers::RedactCustomerInput < Component::Schema`, whose
//! `email_to_redact` then raised NoMethodError from the validator).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

fn gem_path() -> String {
    let home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let ruby_path = Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().unwrap();
    format!("{home}:{}", String::from_utf8_lossy(&ruby_path.stdout))
}

const BASE: &str = r##"class FormSchema
  class Validator
    include ActiveModel::Validations
  end

  class << self
    def attribute(name, type, optional: nil)
      (@declared ||= []) << [name, type, optional]
      define_method(name) { @values[name] }
    end

    def declared
      @declared
    end
  end

  def initialize(values)
    @values = values
  end
end
"##;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activemodel (8.2.0.alpha)\n      activesupport (= 8.2.0.alpha)\n    activesupport (8.2.0.alpha)\n      concurrent-ruby (~> 1.0, >= 1.3.1)\n      i18n (>= 1.6, < 2)\n    concurrent-ruby (1.3.8)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  activemodel\n";

const INPUT: &str = r##"class RedactInput < FormSchema
  attribute(:shop_id, :integer)

  with_options(optional: true) do
    attribute(:email_to_redact, :string)
    attribute(:redact_on, :time)
  end
end
"##;

#[test]
fn with_options_around_inherited_macros_declares_the_attributes() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"activemodel\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_with_options_class_macros\n")
        .write("app/models/form_schema.rb", BASE)
        .write("app/models/redact_input.rb", INPUT)
        .emit(BuildTarget::Ruby);
    // `check` names the PostgreSQL adapter as having no runtime of its
    // own in target-independent analysis; the CRuby tree carries one.
    let errors: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let input = std::fs::read_to_string(dir.join("app/models/redact_input.rb")).unwrap();
    assert!(input.contains("with_options(optional: true) do"), "{input}");

    let script = "require './boot'\n\
                  input = RedactInput.new(email_to_redact: \"a@b.c\")\n\
                  puts input.email_to_redact\n\
                  puts RedactInput.declared.inspect\n";
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W:no-deprecated", "-e", script]).env("GEM_PATH", gem_path()).env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        ["a@b.c", "[[:shop_id, :integer, nil], [:email_to_redact, :string, true], [:redact_on, :time, true]]"],
        "{stdout}"
    );
}
