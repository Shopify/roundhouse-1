//! A class-body `define_method(:name) do … end` in a library class.
//!
//! Ingest left it a class-body call, and the emit of a class it does not
//! replay (every app library class) dropped it with only a warning, so
//! each call to the method was a NoMethodError. It is now the method it
//! defines, with the visibility in effect (Ruby's rule for
//! `define_method`, as for `def`), `module_function` and `class << self`
//! applied as to a `def`. The Ruby family writes a name `def` can spell
//! as that `def`, and any other as the `define_method` it was; the other
//! targets refuse such a name by name. A block that reads the class body
//! (a closure over one of its locals) cannot be a `def` and is refused
//! by name instead of dropped.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const GREETER: &str = r##"module Dmlab
  class Greeter
    PREFIX = "hi"

    define_method(:greet) do |name, punct = "!", *rest, loud: false, **opts, &blk|
      text = "#{PREFIX} #{name}#{punct}"
      text = text.upcase if loud
      [text, rest, opts.keys, blk&.call]
    end

    define_method("tag:group:unit") { :tagged }

    def call_tagged
      public_send("tag:group:unit")
    end

    def doubled(x)
      Util.twice(x)
    end

    private

    define_method(:secret) { :secret }

    def peek
      secret
    end
    public :peek

    class << self
      define_method(:build) { new }
    end
  end
end
"##;

const UTIL: &str = r##"module Dmlab
  module Util
    module_function

    define_method(:twice) { |x| x * 2 }
  end
end
"##;

const OBSERVE: &str = r##"g = Dmlab::Greeter.build
p g.greet("a")
p g.greet("a", "?", 1, 2, loud: true, z: 1) { :b }
p g.call_tagged
p Dmlab::Greeter.private_method_defined?(:secret)
p g.peek
p Dmlab::Greeter.public_method_defined?(:"tag:group:unit")
p Dmlab::Greeter.instance_method(:greet).arity
p Dmlab::Util.twice(4)
p g.doubled(5)
begin
  g.greet
rescue ArgumentError => e
  p e.class
end
begin
  g.secret
rescue NoMethodError => e
  p e.class
end
"##;

#[test]
fn class_body_define_method_runs_as_native() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{GREETER}\n{UTIL}\n{OBSERVE}"))
        .output()
        .expect("spawn ruby");
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    let run = emit_and_run::real_blog()
        .write("app/lib/dmlab/greeter.rb", GREETER)
        .write("app/lib/dmlab/util.rb", UTIL)
        .run_ruby(&format!(
            "require_relative \"app/models/dmlab/greeter\"\nrequire_relative \"app/models/dmlab/util\"\n{OBSERVE}"
        ));
    run.assert_passes();
    assert_eq!(run.stdout, String::from_utf8_lossy(&native.stdout));
    let source = std::fs::read_to_string(run.emitted.join("app/models/dmlab/greeter.rb"))
        .expect("emitted greeter");
    for written in [
        "def greet(name, punct = \"!\", *rest, loud: false, **opts, &blk)",
        "define_method(:\"tag:group:unit\") do",
    ] {
        assert!(source.contains(written), "{written} not written:\n{source}");
    }
}

const CAPTURES: &str = r##"class Captures
  suffix = "!"
  define_method(:shout) { |x| "#{x}#{suffix}" }
end
"##;

#[test]
fn a_block_that_reads_the_class_body_is_refused_by_name() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/captures.rb", CAPTURES)
        .emit(BuildTarget::Ruby);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("`define_method` in the body of `Captures`")),
        "{errors:?}"
    );
}

#[test]
fn other_targets_refuse_a_name_only_define_method_spells() {
    use roundhouse::diagnostic::Severity;
    use roundhouse::emit::diagnostics::scope;
    use roundhouse::project::target_files;

    let tree = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/probe.rb",
            "class Probe\n  define_method(:plain) { 1 }\n  define_method(\"tag:unit\") { 2 }\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let app = ingest_app_from_tree(tree).expect("ingest");
    let probe = app
        .library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == "Probe")
        .expect("Probe ingested");
    let names: Vec<&str> = probe.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["plain", "tag:unit"]);
    assert!(probe.unknown_calls.is_empty(), "{:?}", probe.unknown_calls);
    for target in [
        BuildTarget::Rust,
        BuildTarget::Crystal,
        BuildTarget::Go,
        BuildTarget::Python,
        BuildTarget::Kotlin,
        BuildTarget::Swift,
        BuildTarget::Elixir,
        BuildTarget::Typescript,
        BuildTarget::CSharp,
    ] {
        let (result, diagnostics) =
            scope(|| target_files(&app, std::path::Path::new("not-a-fixture"), target));
        assert!(
            result.is_err(),
            "{} must refuse `tag:unit`",
            target.as_str()
        );
        let named: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.message.contains("define_method_name"))
            .collect();
        assert_eq!(named.len(), 1, "{target:?}: {diagnostics:?}");
        assert_eq!(named[0].severity, Severity::Error);
        assert!(!named[0].span.is_synthetic(), "{:?}", named[0]);
        assert!(
            named[0].message.contains("define_method(\"tag:unit\")"),
            "{:?}",
            named[0]
        );
    }
}
