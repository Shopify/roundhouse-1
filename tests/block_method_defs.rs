//! A `def` inside a block: `Class.new do … end`, `Class.new(Parent) do
//! … end`, `class_eval do … end` and a `class << self` within one.
//!
//! Ingest lifted each such `def` to `nil`, so the anonymous class lacked
//! its methods and a call to one died at runtime (Core's
//! `WebUrlHelpersFactory` defines `replace_named_args` this way, and
//! every helper it builds calls it). It is now a `MethodDefExpr`: the
//! Ruby family writes it verbatim, with its formals as the source wrote
//! them, and every other target refuses it by name. What the mapping
//! cannot reach (a `def` nested in a `class << self` body's other
//! statements) is refused at ingest by name.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const PROBE: &str = r##"module Defblocklab
  class Base
    def greet(name)
      "hello #{name}"
    end
  end

  class Probe
    def self.anonymous
      klass = Class.new do
        def plain(a, b = 2, *rest, key:, opt: 4, **more, &blk)
          [a, b, rest, key, opt, more.keys, blk&.call]
        end

        def flattened(x, style: :time)
          [x, style]
        end

        class << self
          def build
            new
          end
        end

        private

        def secret
          :secret
        end
      end
      obj = klass.build
      [
        obj.plain(1, key: 3) { :blk },
        obj.plain(1, 5, 6, key: 3, z: 9),
        obj.flattened(1),
        obj.flattened(1, style: :date),
        klass.private_method_defined?(:secret),
        obj.send(:secret),
      ]
    end

    def self.subclass
      klass = Class.new(Base) do
        def greet(name)
          super.upcase
        end
      end
      klass.new.greet("x")
    end

    def self.reopened
      klass = Class.new
      klass.class_eval do
        def value
          :reopened
        end
      end
      klass.new.value
    end

    def self.value_of_def
      Class.new { @name = def named; end }.instance_variable_get(:@name)
    end
  end
end
"##;

const OBSERVE: &str = "%i[anonymous subclass reopened value_of_def].each do |m|\n  \
     puts \"#{m}: #{Defblocklab::Probe.public_send(m).inspect}\"\n\
     end\n";

#[test]
fn defs_inside_blocks_run_as_native() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{PROBE}\n{OBSERVE}"))
        .output()
        .expect("spawn ruby");
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    let run = emit_and_run::real_blog()
        .write("app/lib/defblocklab/probe.rb", PROBE)
        .run_ruby(&format!(
            "require_relative \"app/models/defblocklab/base\"\nrequire_relative \"app/models/defblocklab/probe\"\n{OBSERVE}"
        ));
    run.assert_passes();
    assert_eq!(run.stdout, String::from_utf8_lossy(&native.stdout));
    let source = std::fs::read_to_string(run.emitted.join("app/models/defblocklab/probe.rb"))
        .expect("emitted probe");
    for written in [
        "def plain(a, b = 2, *rest, key:, opt: 4, **more, &blk)",
        "def flattened(x, style: :time)",
        "def self.build",
    ] {
        assert!(source.contains(written), "{written} not written:\n{source}");
    }
}

const NESTED: &str = r##"class Nested
  def self.run
    Class.new do
      class << self
        if true
          def hidden
            1
          end
        end
      end
    end
  end
end
"##;

#[test]
fn a_def_the_mapping_cannot_reach_is_refused_by_name() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/nested.rb", NESTED)
        .emit(BuildTarget::Ruby);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("`def hidden` inside a block nested in a `class << self` body")),
        "{errors:?}"
    );
}

#[test]
fn other_targets_refuse_block_defs_by_name() {
    use roundhouse::diagnostic::Severity;
    use roundhouse::emit::diagnostics::scope;
    use roundhouse::project::target_files;

    let tree = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/probe.rb",
            "class Probe\n  def self.run\n    Class.new do\n      def helper\n        1\n      end\n    end\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let app = ingest_app_from_tree(tree).expect("ingest");
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
            "{} must refuse a def in a block",
            target.as_str()
        );
        let named: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.message.contains("block_method_def"))
            .collect();
        assert_eq!(named.len(), 1, "{target:?}: {diagnostics:?}");
        assert_eq!(named[0].severity, Severity::Error);
        assert!(!named[0].span.is_synthetic(), "{:?}", named[0]);
        assert!(named[0].message.contains("`def helper`"), "{:?}", named[0]);
    }
}
