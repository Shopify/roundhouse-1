//! A class-body constant built with the implicit-self `new` of a class
//! whose own `self.new` is private: Ruby sends `new(...)` to `self`, so a
//! private method is callable; `Owner.new(...)` is a NoMethodError
//! (`lower::class_body_new::private_constructors`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ENUM: &str = r#"module Registry
  class Enum
    attr_reader :serialize

    private_class_method :new

    def initialize(value)
      @serialize = value
    end

    def self.inherited(subclass)
      super
      subclass.instance_variable_set(:@values, [])
    end

    def self.values
      @values.dup
    end

    def self.new(value)
      instance = super
      @values << instance
      instance.freeze
    end
    private_class_method :new
  end

  class Kind < Enum
    Card = new("card") #: Kind
    Wallet = new("wallet") #: Kind
  end
end
"#;

#[test]
fn constants_built_by_a_subclass_of_a_class_with_its_own_private_new() {
    let run = emit_and_run::real_blog().write("app/models/registry.rb", ENUM).run_ruby(
        "raise \"values #{Registry::Kind.values.size}\" unless Registry::Kind.values.size == 2\n\
         raise \"serialize\" unless Registry::Kind::Card.serialize == \"card\"\n\
         begin\n  Registry::Kind.new(\"x\")\n  raise \"new was public\"\nrescue NoMethodError\nend\n\
         puts \"ok\"\n",
    );
    assert!(run.success && run.stdout.contains("ok"), "{}\n{}", run.stdout, run.stderr);
}

const COMBINE: &str = r#"module Mix
  module Combine
    extend ActiveSupport::Concern

    class_methods do
      def combine(list)
        new(list.sum)
      end

      alias_method :best, :combine
    end
  end

  class Total
    include Combine

    attr_reader :n

    def initialize(n)
      @n = n
    end
  end
end
"#;

#[test]
fn class_methods_block_alias_lands_on_the_includers_singleton() {
    let run = emit_and_run::real_blog().write("app/models/mix.rb", COMBINE).run_ruby(
        "raise \"combine\" unless Mix::Total.combine([1, 2]).n == 3\n\
         raise \"best\" unless Mix::Total.best([1, 2]).n == 3\n\
         puts \"ok\"\n",
    );
    assert!(run.success && run.stdout.contains("ok"), "{}\n{}", run.stdout, run.stderr);
}

const TYPES: &str = r#"module Kinds
  class Base
  end

  class Text < Base
  end

  class Number < Base
  end
end
"#;

const CLASSIFICATION: &str = r#"module Kinds
  module Classification
    module Accessor
      attr_accessor :kind
    end

    Base.extend(Accessor)

    Text.kind = :text
    Number.kind = :number

    def self.for(type)
      type.kind
    end
  end
end
"#;

#[test]
fn a_module_body_extends_app_classes_and_writes_through_the_accessor() {
    let run = emit_and_run::real_blog()
        .write("app/models/kinds.rb", TYPES)
        .write("app/models/kinds/classification.rb", CLASSIFICATION)
        .run_ruby(
            "raise \"text\" unless Kinds::Classification.for(Kinds::Text) == :text\n\
             raise \"number\" unless Kinds::Number.kind == :number\n\
             puts \"ok\"\n",
        );
    assert!(run.success && run.stdout.contains("ok"), "{}\n{}", run.stdout, run.stderr);
}

const CYCLE_MODEL: &str = r#"module Cyc
  class Widget
    KIND = "a"

    def label
      "x"
    end

    def self.build
      new
    end

    RESPONSES = { KIND => Cyc::WidgetResponse }.freeze

    def tail
      "t"
    end
  end
end
"#;

const CYCLE_RESPONSE: &str = r#"module Cyc
  class WidgetResponse
    missing = [:label] - Widget.instance_methods
    raise ArgumentError, "Methods #{missing.inspect} don't exist on #{Widget}." unless missing.empty?
    raise "a def below the constant exists early" if Widget.method_defined?(:tail)
  end
end
"#;

#[test]
fn defs_above_a_constant_exist_when_its_load_time_require_asks_the_class() {
    let run = emit_and_run::real_blog()
        .write("app/models/cyc/widget.rb", CYCLE_MODEL)
        .write("app/models/cyc/widget_response.rb", CYCLE_RESPONSE)
        .run_ruby("Cyc::Widget\nputs \"ok\"\n");
    assert!(run.success && run.stdout.contains("ok"), "{}\n{}", run.stdout, run.stderr);
}
