//! `X = Class.new(Parent) do … end` and `X = Class.new do … end` whose
//! block defines methods (Shopify Core's `CartLineItemUpdater`
//! inventory policies: `class << self` bodies, one policy subclassing
//! another) are the classes they build (`ingest::factory_class`), not a
//! file the ingest drops.
//!
//! CRuby is the oracle: the same script runs on the source and on the
//! emitted app, and the outputs must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::ingest::ingest_library_classes;

const OUTER: &str = r##"
class Outer
  NONE = Class.new do
    class << self
      def update(item:, asked:, **)
        item[:quantity] = asked
      end
    end
  end

  GUARD = Class.new do
    INNER = 10

    class << self
      def guard_update(item:, asked:)
        available = availability_for(item)
        if available >= asked
          item[:quantity] = asked
        else
          yield available
        end
      end

      def availability_for(item) = [item[:stock], INNER].min
    end

    def describe = "#{self.class.name}:#{secret}"

    private

    def secret = INNER + 1
  end

  CLAMP = Class.new(GUARD) do
    class << self
      def update(item:, asked:)
        guard_update(item: item, asked: asked) { |available| item[:quantity] = available }
        item
      end
    end
  end

  def self.run(policy, stock, asked)
    item = { stock: stock, quantity: 0 }
    policy.update(item: item, asked: asked)
    item[:quantity]
  end
end
"##;

const TOP: &str = r##"Top = Class.new(Outer::GUARD) do
  class << self
    LIMIT = 3
    def update(item:, asked:)
      guard_update(item: item, asked: [asked, LIMIT].min) { |available| raise ArgumentError, "only #{available}" }
    end
  end
end
"##;

const SCRIPT: &str = r##"
p Outer.run(Outer::NONE, 5, 7), Outer.run(Outer::CLAMP, 5, 7), Outer.run(Outer::CLAMP, 50, 7)
p Outer::INNER, Outer::GUARD.const_defined?(:INNER, false), Outer::CLAMP.name, Outer::CLAMP.superclass.name
p Outer::GUARD.new.describe, Outer::CLAMP.new.describe, Outer::GUARD.private_method_defined?(:secret)
p Outer.respond_to?(:update), Outer.respond_to?(:guard_update)
p Outer.run(Top, 5, 9), Top.superclass.name
begin
  Outer.run(Top, 1, 9)
rescue ArgumentError => e
  p e.message
end
"##;

#[test]
fn class_new_blocks_are_classes_with_their_class_methods() {
    let expected = "7\n5\n7\n10\nfalse\n\"Outer::CLAMP\"\n\"Outer::GUARD\"\n\"Outer::GUARD:11\"\n\"Outer::CLAMP:11\"\n\
                    true\nfalse\nfalse\n3\n\"Outer::GUARD\"\n\"only 1\"\n";
    let native = Command::new("ruby")
        .arg("-W0")
        .arg("-e")
        .arg(format!("{OUTER}\n{TOP}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/outer.rb", OUTER)
        .write("app/lib/top.rb", TOP)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

#[test]
fn class_new_block_shapes_a_class_cannot_hold_are_named_refusals() {
    for (source, message) in [
        (
            "class Outer\n  limit = 3\n  K = Class.new do\n    define_method(:limit) { limit }\n    def x = 1\n  end\nend\n",
            "a `Class.new do … end` body block that reads a local variable of the enclosing scope",
        ),
        (
            "class Outer\n  K = Class.new(base) do\n    def x = 1\n  end\nend\n",
            "`Class.new(parent) do … end` whose parent is not a constant path",
        ),
        (
            "class Outer\n  K = Class.new do |klass|\n    klass.attr_reader :n\n    def x = 1\n  end\nend\n",
            "block parameters on a Class factory body are not modeled",
        ),
        (
            "class Outer\n  K = Class.new do\n    class << self\n      C = 1\n      def c = C\n    end\n  end\nend\n",
            "constant written in a `class << self` of a Class factory body inside a class or module body",
        ),
    ] {
        let error = ingest_library_classes(source.as_bytes(), "outer.rb")
            .err()
            .unwrap_or_else(|| panic!("accepted {source}"))
            .to_string();
        assert!(error.contains(message), "{source}: {error}");
    }
    // A block's own locals, and a nested block's, are not the enclosing
    // scope's.
    let source = "class Outer\n  K = Class.new do\n    n = 2\n    define_method(:twice) { |x| y = x; y * n }\n    def x = 1\n  end\nend\n";
    assert!(ingest_library_classes(source.as_bytes(), "outer.rb").is_ok());
}
