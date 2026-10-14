//! Audit: a Ruby-family emit writes every block signature the ingest had.
//!
//! Lambda IR carries a block's optional, keyword and keyword-rest
//! parameters in `signature` (`|a, b = 1, key:, **rest|`), and most of
//! the `Lambda { .. }` constructions outside ingest rebuild a Lambda
//! without binding it. One that rebuilds a signature-bearing block and
//! takes `Default::default()` writes `|a|` where the source had `|a, b =
//! 1|`, and the body reads a name nothing binds.
//! `emit::ruby::audit_block_signatures` matches every Lambda the Ruby
//! emitter writes to the block the ingest built, by the body's source
//! span, and reports each one written with other parameters. This test
//! runs it over blocks in the positions the lowerings rebuild: library
//! class methods and constants, a model's scopes, callbacks and
//! broadcasts, a controller, a helper, a concern spliced into its
//! includer, `Class.new`/`Struct.new` blocks and a `define_method` loop.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::emit::ruby::audit_block_signatures;
use roundhouse::project::BuildTarget;

const PROBE: &str = r##"module Sigaudit
  class Probe
    HANDLER = ->(a, b = 1, key: 2, **rest) { [a, b, key, rest] }
    TABLE = { run: lambda { |x, y = 3| x + y } }

    %i[first second].each do |name|
      define_method(name) { |value, suffix: "!"| "#{name}#{value}#{suffix}" }
    end

    def each_style(items)
      items.each_with_object([]) { |item, acc, extra = nil| acc << item }
    end

    def mapped(items)
      items.map { |x, scale: 2| x * scale }
    end

    def selected(items)
      items.select do |x, min = 0|
        x > min
      end
    end

    def reduced(items)
      items.inject(0) { |sum, x, extra = 0| sum + x + extra }
    end

    def tapped(value)
      value.tap { |v, note: nil| note }
    end

    def proc_style
      proc { |a, b = 2, **opts| [a, b, opts] }
    end

    def hash_default
      Hash.new { |h, k, fallback: 0| h[k] = fallback }
    end

    def nested(items)
      items.map { |x| items.map { |y, z = 1| x + y + z } }
    end

    def yielding
      yield 1
    end

    def caller_of_yield
      yielding { |a, b = 5| a + b }
    end

    def then_style(v)
      v.then { |x, y: 1| x + y }
    end

    def anonymous_class
      Class.new do
        def call(list)
          list.map { |a, b = 1| a + b }
        end
      end
    end

    def struct
      Struct.new(:items) do
        def total
          items.sum { |x, weight: 1| x * weight }
        end
      end
    end
  end
end
"##;

const CONCERN: &str = r##"module Sigaudited
  extend ActiveSupport::Concern

  def sig_tags(items)
    items.map { |tag, prefix: "#"| "#{prefix}#{tag}" }
  end
end
"##;

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .write("app/lib/sigaudit/probe.rb", PROBE)
        .write("app/models/concerns/sigaudited.rb", CONCERN)
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            "  include Sigaudited\n\n  scope :titled, ->(title, exact: true) { where(title: title) }\n\n  validates :title, presence: true\n\n  def sig_words\n    body.to_s.split.map { |w, upcase: false| upcase ? w.upcase : w }\n  end\n",
        )
        .edit(
            "app/controllers/articles_controller.rb",
            "  # GET /articles/new\n",
            "  def sig_probe(items)\n    items.map { |x, scale: 1| x * scale }\n  end\n\n  # GET /articles/new\n",
        )
        .write(
            "app/helpers/articles_helper.rb",
            "module ArticlesHelper\n  def sig_join(items)\n    items.map { |i, sep: \",\"| \"#{i}#{sep}\" }.join\n  end\nend\n",
        )
}

#[test]
fn ruby_family_emit_keeps_every_ingested_block_signature() {
    for target in [BuildTarget::Ruby, BuildTarget::Spinel] {
        let ((emitted, errors), audit) = audit_block_signatures(|| overlay().emit(target));
        if target == BuildTarget::Ruby {
            assert!(errors.is_empty(), "{errors:#?}");
        }
        assert!(audit.losses.is_empty(), "{target:?}: {:#?}", audit.losses);
        // Not vacuous: every probe block but two is written as a Lambda.
        // The scope becomes a class method and the `define_method` loop
        // its methods, each with the keyword as a method parameter.
        assert_eq!(audit.ingested, 20, "{target:?}: {audit:#?}");
        assert_eq!(audit.unwritten.len(), 2, "{target:?}: {audit:#?}");
        assert!(
            audit.written >= audit.ingested - 2,
            "{target:?}: {audit:#?}"
        );
        let article = std::fs::read_to_string(emitted.join("app/models/article.rb"))
            .expect("emitted article");
        assert!(
            article.contains("def self.titled(title, ") && article.contains("exact: true)"),
            "{article}"
        );
    }
}

/// A block-form lifecycle hook whose block takes a required keyword
/// cannot be spliced into the parameterless hook method (Rails never
/// passes it); it was dropped with no diagnostic once the signature was
/// carried. An optional keyword takes its default (the optional-keyword
/// shape was refused before 98e999ace).
#[test]
fn a_hook_block_with_a_signature_is_refused_by_name() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            "  before_save { |record, touch:| record }\n\n  validates :title, presence: true\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("block_callback_signature")
            && e.contains("`before_save { |…| … }` on `Article` takes the required keyword `touch:`")),
        "{errors:#?}"
    );
}
