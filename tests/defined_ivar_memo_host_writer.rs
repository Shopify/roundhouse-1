//! `lower::defined_ivar_memo` must see every writer of `@x`, not just
//! the guarding class's. Core's `GraphApi::HasNestedBreadthResolver`
//! (extended into `FieldDefinition`) memoises `breadth_resolver` behind
//! `defined?(@breadth_resolver)`; the value is set by the inherited
//! `Resolver.breadth_resolver=`. A per-class rewrite gave the module a
//! flag the superclass setter never set, so the reader overwrote every
//! declared resolver with nil and 106 unit-1000 ids failed at load with
//! "definition … must declare a breadth_resolver".

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const NESTED: &str = r#"module NestedResolver
  def resolver
    return @resolver if defined?(@resolver)

    @resolver = nested_resolver
  end

  private

  def nested_resolver
    return unless const_defined?(:Inner, false)

    const_get(:Inner, false).new
  end
end
"#;

const BASE: &str = r#"class BaseResolver
  class << self
    attr_reader :resolver

    def resolver=(val)
      @resolver = val
    end
  end
end
"#;

const DEFINITION: &str = r#"class Definition < BaseResolver
  extend NestedResolver

  def self.resolver(value = nil)
    self.resolver = value if value

    super()
  end
end
"#;

const DECLARED: &str = r#"class Declared < Definition
  resolver :declared
end
"#;

const NESTING: &str = r#"class Nesting < Definition
  class Inner
  end
end
"#;

#[test]
fn a_module_guard_sees_the_hosts_inherited_writer() {
    let script = "p Declared.resolver\np Nesting.resolver.class\np Definition.resolver\n";
    let all = format!("{NESTED}\n{BASE}\n{DEFINITION}\n{DECLARED}\n{NESTING}\n{script}");
    let native = emit_and_run::ruby().args(["-W:no-deprecated", "-e", &all]).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), ":declared\nNesting::Inner\nnil\n");

    let (tree, errors) = emit_and_run::empty_app()
        .write("app/models/nested_resolver.rb", NESTED)
        .write("app/models/base_resolver.rb", BASE)
        .write("app/models/definition.rb", DEFINITION)
        .write("app/models/declared.rb", DECLARED)
        .write("app/models/nesting.rb", NESTING)
        .emit(BuildTarget::Ruby);
    let module = tree.join("app/models/nested_resolver.rb");
    assert!(module.exists(), "no {}: {errors:#?}", module.display());

    let run = std::process::Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{script}"))
        .output()
        .expect("spawn ruby");
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&native.stdout));
}
