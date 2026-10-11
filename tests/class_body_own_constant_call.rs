//! A call in a module body on a constant the same body assigned runs as
//! Ruby runs it when the body loads: `REGISTRY = Registry.new` followed
//! by `REGISTRY.register(:a) { … }` (the registration is the point of
//! the file). The Ruby family dropped the calls with a `lower_residue`
//! warning, so the constant was written but never populated.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const REGISTRY: &str = r##"class Registry
  #: () -> void
  def initialize
    @entries = {}
  end

  #: (Symbol, ?String?) ?{ (String) -> String } -> void
  def register(name, label = nil, &block)
    @entries[name] = block || ->(arg) { "#{label}:#{arg}" }
  end

  #: (Symbol, String) -> String
  def call(name, arg) = @entries.fetch(name).call(arg)
end
"##;

const HOLDER: &str = r##"module Holder
  REGISTRY = Registry.new

  REGISTRY.register(:blocky) { |arg| "blocky:#{arg}" }
  REGISTRY.register(:labelled, "label")
end
"##;

#[test]
fn a_call_on_a_constant_the_body_assigned_runs_when_the_body_loads() {
    let run = emit_and_run::empty_app()
        .write("lib/registry.rb", REGISTRY)
        .write("lib/holder.rb", HOLDER)
        .run_ruby("puts Holder::REGISTRY.call(:blocky, \"x\")\nputs Holder::REGISTRY.call(:labelled, \"y\")\n");
    assert!(run.success, "emitted tree failed in {}: {}\nerrors: {:#?}", run.emitted.display(), run.stderr, run.errors);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["blocky:x", "label:y"], "{}", run.stderr);
}
