//! A gem module body that `require`s a sibling file and then calls a
//! method of the class that file defines runs the require first.
//!
//! active_kafka 14.2.0's active_kafka.rb requires
//! "active_kafka/format" inside `module ActiveKafka` (Kernel#require in a
//! body runs in source order) and then calls
//! `Format.register_lazy(...)` on the class it defines. The emitted body
//! kept the call and left the require out, so the boot raised
//! `uninitialized constant ActiveKafka::Format`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ACT_KIT: &str = r#"require "act-kit/version"

module ActKit
  require "act-kit/format"

  Format.register_lazy(:json, "act-kit/formats/json")
  Format.register_lazy(:avro, "act-kit/formats/avro")

  def self.formats
    Format.registry.keys
  end
end
"#;
const VERSION: &str = "module ActKit\n  VERSION = \"1.0.0\"\nend\n";
const FORMAT: &str = r#"module ActKit
  class Format
    def self.registry
      @registry ||= {}
    end

    def self.register_lazy(name, path)
      registry[name] = path
    end
  end
end
"#;
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    act-kit (1.0.0)\n\nDEPENDENCIES\n  act-kit\n";

const FILES: [(&str, &str); 3] =
    [("act-kit.rb", ACT_KIT), ("act-kit/version.rb", VERSION), ("act-kit/format.rb", FORMAT)];

#[test]
fn a_gem_module_body_require_runs_before_the_call_that_needs_it() {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  act-kit: vendor/gems/act-kit-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"act-kit\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n");
    for (path, text) in FILES {
        app = app.write(&format!("vendor/gems/act-kit-1.0.0/lib/{path}"), text);
    }
    let run = app.run_ruby("puts ActKit.formats.inspect\n");
    assert!(
        run.success,
        "emitted tree failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}",
        run.emitted.display(),
        run.stdout,
        run.stderr
    );
    assert_eq!(run.stdout.lines().last(), Some("[:json, :avro]"), "{}", run.stderr);
}
