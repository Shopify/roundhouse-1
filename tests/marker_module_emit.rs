//! A module that declares nothing but itself is a type other code names:
//! `module ReferenceFilterPathSegment; end` (Shopify Core's
//! Pagination::Ast) and a member-less Sorbet interface
//! (`module AdyenResponse; extend T::Helpers; interface!; end`). Both are
//! included by `Struct.new` factory classes. The module ingest kept only
//! modules with a member, so these were not emitted; the include did not
//! resolve, and the factory class's body raised at load ("include not
//! supported").
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SHAPES: &str = r#"module Shapes
  # Opaque marker.
  module Marker; end

  module Iface
    extend T::Helpers

    interface!
  end

  Point = Struct.new(:x, :y) do
    include Marker
    include Iface

    def sum
      x + y
    end
  end

  class Circle
    include Marker

    def initialize(r)
      @r = r
    end

    def r
      @r
    end
  end
end
"#;

const TAGGED: &str = "module Tagged\nend\n";

const USER: &str = r#"class Labeler
  include Tagged

  def label(shape)
    shape.is_a?(Shapes::Marker) ? "marked" : "plain"
  end
end
"#;

const SCRIPT: &str = r#"point = Shapes::Point.new(1, 2)
puts point.sum
puts Shapes::Point.include?(Shapes::Marker)
puts Shapes::Point.include?(Shapes::Iface)
puts Shapes::Circle.new(3).is_a?(Shapes::Marker)
puts Labeler.new.label(point)
puts Labeler.include?(Tagged)
puts Shapes::Marker.class
"#;

#[test]
fn member_less_modules_are_emitted_and_their_includers_load() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/shapes.rb", SHAPES)
        .write("lib/tagged.rb", TAGGED)
        .write("lib/labeler.rb", USER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, "3\ntrue\ntrue\ntrue\nmarked\ntrue\nModule\n");
    let point = std::fs::read_to_string(run.emitted.join("app/models/shapes/point.rb")).expect("point.rb");
    assert!(!point.contains("include not supported"), "{point}");
    for marker in ["app/models/shapes/marker.rb", "app/models/shapes/iface.rb", "app/models/tagged.rb"] {
        let source = std::fs::read_to_string(run.emitted.join(marker)).unwrap_or_else(|e| panic!("{marker}: {e}"));
        assert!(source.contains("module "), "{source}");
    }
}

/// Hedwig's `CommandTable::FailedCommandError` is only `extend
/// T::Helpers; requires_ancestor { StandardError }`; its error classes
/// include it and callers rescue by it. `requires_ancestor` takes a
/// block, which the marker rule refused, so the module was dropped and
/// both the include and the rescue were refused.
const FAILURES: &str = r#"module Table
  module FailedCommandError
    extend T::Helpers

    requires_ancestor { StandardError }
  end

  class Boom < StandardError
    include FailedCommandError
  end

  class Lock
    def take
      raise Boom, "x"
    rescue FailedCommandError => e
      "rescued #{e.message}"
    end
  end
end
"#;

#[test]
fn a_requires_ancestor_module_is_emitted_and_rescues_its_includers() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/table.rb", FAILURES)
        .run_ruby("puts Table::Lock.new.take\n");
    run.assert_passes();
    assert_eq!(run.stdout, "rescued x\n");
    let source = std::fs::read_to_string(run.emitted.join("app/models/table/failed_command_error.rb")).expect("module emitted");
    assert!(!source.contains("requires_ancestor"), "{source}");
}
