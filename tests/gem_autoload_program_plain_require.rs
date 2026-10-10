//! A gem file a sibling requires by a plain `require`, whose constant the
//! gem's autoload also registers, defines it without Ruby's "Expected
//! <file> to define <Const> but it didn't" warning.
//!
//! The constant's autoload loads the file's program (its `require`s run
//! before the class). A plain `require` of that program from another gem
//! file runs its declarations' `class X` while X still autoloads the same
//! in-flight feature: Ruby resolves `class X` through that autoload,
//! re-requires the program (a circular require) and warns it did not
//! define X (variable.c, `autoload_try_load`); a strict-warning policy
//! raises that at boot. Natively the gem's `require` just defines the
//! class. kafka-shopify 5.2.3's validator.rb requires
//! errors/data_validation_error.rb, which requires "sorbet-runtime"
//! first, while its `autoload` entry registers the same constant.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ERR_KIT: &str = "require \"err-kit/errors\"\nrequire \"err-kit/validator\"\n";
const ERRORS: &str = r#"module ErrKit
  module Errors
    autoload(:DataError, "err-kit/errors/data_error")
  end
end
"#;
const DATA_ERROR: &str = "require \"set\"\n\nmodule ErrKit\n  module Errors\n    class DataError < StandardError\n      def label = \"data\"\n    end\n  end\nend\n";
const VALIDATOR: &str = r#"require "err-kit/errors/data_error"

module ErrKit
  class Validator
    def check
      raise ErrKit::Errors::DataError, "bad"
    rescue ErrKit::Errors::DataError => e
      e.label
    end
  end
end
"#;
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    err-kit (1.0.0)\n\nDEPENDENCIES\n  err-kit\n";

const FILES: [(&str, &str); 4] = [
    ("err-kit.rb", ERR_KIT),
    ("err-kit/errors.rb", ERRORS),
    ("err-kit/errors/data_error.rb", DATA_ERROR),
    ("err-kit/validator.rb", VALIDATOR),
];

#[test]
fn a_plain_require_of_an_autoloaded_gem_program_defines_its_constant_without_a_warning() {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  err-kit: vendor/gems/err-kit-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"err-kit\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n");
    for (path, text) in FILES {
        app = app.write(&format!("vendor/gems/err-kit-1.0.0/lib/{path}"), text);
    }
    let run = app.run_ruby_env(
        "require \"err-kit\" unless defined?(ErrKit::Validator)\nputs ErrKit::Validator.new.check\n",
        &[("RUBYOPT", "-w")],
    );
    assert!(
        run.success,
        "emitted tree failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}",
        run.emitted.display(),
        run.stdout,
        run.stderr
    );
    assert_eq!(run.stdout.lines().last(), Some("data"), "{}", run.stderr);
    assert!(!run.stderr.contains("to define"), "autoload warning in the emitted boot\n{}", run.stderr);
}
