//! A gem file that defines a module with a nested module, and a sibling
//! class that includes the module, loads in source order.
//!
//! identifications 3.4.4's api_change_interface.rb defines
//! `ApiChangeInterface` (an `included` hook extending `ClassMethods`), the
//! nested `ClassMethods`, and `ApiChangeSkeleton` which includes the
//! interface. The emitted tree split the file into three and loaded the
//! skeleton first, then the interface, then `ClassMethods`, so the hook
//! fired before the constant existed: `uninitialized constant
//! Identifications::ApiChangeInterface::ClassMethods`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const IDENTS: &str = "require \"idents/interface\"\nrequire \"idents/errors\"\n\nmodule Idents\n  def self.loaded\n    true\n  end\nend\n";
const INTERFACE: &str = r#"# frozen_string_literal: true

module Idents
  module Interface
    include Comparable

    def self.included(base)
      base.extend(ClassMethods)
    end

    module ClassMethods
      def in_effect?
        raise Interface::NotImplemented
      end
    end

    def handle
      raise Interface::NotImplemented
    end
  end

  class Skeleton
    include Interface
  end
end
"#;
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    idents (1.0.0)\n\nDEPENDENCIES\n  idents\n";

#[test]
fn a_nested_module_loads_before_a_sibling_class_includes_its_owner() {
    let app = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  idents: vendor/gems/idents-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"idents\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("vendor/gems/idents-1.0.0/lib/idents.rb", IDENTS)
        .write("vendor/gems/idents-1.0.0/lib/idents/interface.rb", INTERFACE)
        .write(
            "vendor/gems/idents-1.0.0/lib/idents/errors.rb",
            "module Idents\n  module Interface\n    class NotImplemented < StandardError; end\n  end\nend\n",
        );
    let run = app.run_ruby(
        "begin\n  Idents::Skeleton.in_effect?\nrescue StandardError => e\n  puts e.class.name\nend\nputs Idents::Skeleton.singleton_class.include?(Idents::Interface::ClassMethods)\n",
    );
    assert!(
        run.success,
        "emitted tree failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}",
        run.emitted.display(),
        run.stdout,
        run.stderr
    );
    let lines: Vec<&str> = run.stdout.lines().rev().take(2).collect();
    assert_eq!(lines, ["true", "Idents::Interface::NotImplemented"], "{}", run.stderr);
}
