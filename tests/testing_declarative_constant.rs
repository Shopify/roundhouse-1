//! `ActiveSupport::Testing::Declarative` names a constant Core's test
//! support reads at load: metafield_input_examples.rb has `include
//! ActiveSupport::Testing::Declarative` in its module body. The emitted
//! test_helper carried the DSL as `RoundhouseTestDeclarative`, so six
//! tests raised NameError on the constant. The module is Rails' own:
//! `test "name"` defines `test_name`, raising on a name already defined.
//! The script is not analyzed (the constant is what Core's run emits under
//! `--allow-unsupported`), so it reads the emitted tree as the run does.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCRIPT: &str = r#"
$LOAD_PATH.unshift("test")
require "test_helper"

declarative = ActiveSupport::Testing::Declarative
puts "name: #{declarative.name}"
puts "owner: #{declarative.instance_method(:test).owner}"

host = Class.new(ActiveSupport::TestCase) { def self.name = "ProbeTest" }
host.test("adds a method") { :ran }
puts "defined: #{host.instance_methods(false).inspect}"
puts "body: #{host.new.test_adds_a_method.inspect}"
begin
  host.test("adds a method") { }
rescue => e
  puts "dup: #{e.message}"
end
host.test("no block")
puts "no block: #{host.instance_methods(false).include?(:test_no_block)}"

# A module that includes it, as the support module does.
examples = Module.new { include ActiveSupport::Testing::Declarative }
puts "mixed in: #{examples.ancestors.include?(declarative)}"
"#;

#[test]
fn the_declarative_module_is_activesupport_s_constant() {
    let run = emit_and_run::empty_app().run_ruby(SCRIPT);
    run.assert_passes();
    assert!(
        run.stdout.lines().any(|l| l.starts_with("dup: test_adds_a_method is already defined in ")),
        "missing the duplicate-name raise in\n{}",
        run.stdout
    );
    for expected in [
        "name: ActiveSupport::Testing::Declarative",
        "owner: ActiveSupport::Testing::Declarative",
        "defined: [:test_adds_a_method]",
        "body: :ran",
        "no block: true",
        "mixed in: true",
    ] {
        assert!(run.stdout.lines().any(|l| l == expected), "missing {expected:?} in\n{}", run.stdout);
    }
}
