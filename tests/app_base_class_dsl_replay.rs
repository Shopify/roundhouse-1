//! A class body calls a class-side DSL method its app base class defines
//! (`describe`, `argument`), where the base also includes
//! ActiveModel::Validations and prepends a module on `inherited`.
//!
//! Core's `Home::Traits::Matchers::MilestoneCompleteBeforeDate < Trait <
//! Matcher` declares `argument :milestone_name, ...`; the calls were
//! dropped as "not modelled", so `Matcher.arguments_list` stayed nil and
//! `Trait#validate_arguments` called `valid_applied_argument?` on nil.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MATCHER: &str = r#"module Home
  module Traits
    class Matcher
      include ActiveModel::Validations

      attr_reader :arguments

      class << self
        attr_reader :arguments_list, :description

        def argument(name, options)
          @arguments_list ||= []
          current_index = @arguments_list.count
          current_argument = Home::Traits::Argument.new(name, **options.slice(:type, :optional))
          @arguments_list << current_argument

          define_method(name) do
            #: self as Matcher
            current_argument.coerce_argument(arguments[current_index])
          end
        end

        def describe(desc)
          @description = desc
        end
      end

      def initialize(*arguments)
        @arguments = arguments
      end

      #: () -> Array[Home::Traits::Argument]
      def arguments_list
        self.class.arguments_list || []
      end
    end
  end
end
"#;

const ARGUMENT: &str = r#"module Home
  module Traits
    class Argument
      attr_reader :name

      def initialize(name, type: :string, optional: false)
        @name = name
        @type = type
        @optional = optional
      end

      #: (untyped value) -> bool
      def valid_applied_argument?(value)
        value.is_a?(String) || (@optional && value.nil?)
      end

      #: (untyped value) -> untyped
      def coerce_argument(value)
        value
      end
    end
  end
end
"#;

const TRAIT: &str = r#"module Home
  module Traits
    class Trait < Matcher
      validate :validate_arguments

      private

      def validate_arguments
        arguments.each.with_index do |applied_argument, index|
          unless arguments_list[index].valid_applied_argument?(applied_argument)
            errors.add(:argument, "bad #{applied_argument.inspect}")
          end
        end
      end
    end
  end
end
"#;

const MILESTONE: &str = r#"module Home
  module Traits
    module Matchers
      class MilestoneCompleteBeforeDate < Trait
        describe "Checks a milestone."
        argument :milestone_name, type: :string
        argument :completed_before, type: :string
      end
    end
  end
end
"#;

#[test]
fn an_app_base_class_s_dsl_runs_in_the_subclass_body() {
    let script = "klass = Home::Traits::Matchers::MilestoneCompleteBeforeDate\n\
        p klass.description, klass.arguments_list.map(&:name)\n\
        ok = klass.new('a', 'b')\n\
        p ok.valid?, ok.milestone_name\n\
        bad = klass.new('a', 3)\n\
        p bad.valid?, bad.errors.size\n\
        p Home::Traits::Matcher.arguments_list\n";
    let expected = "\"Checks a milestone.\"\n[:milestone_name, :completed_before]\ntrue\n\"a\"\nfalse\n1\nnil\n";
    let run = emit_and_run::empty_app()
        .write("app/models/home/traits/argument.rb", ARGUMENT)
        .write("app/models/home/traits/matcher.rb", MATCHER)
        .write("app/models/home/traits/trait.rb", TRAIT)
        .write("app/models/home/traits/matchers/milestone_complete_before_date.rb", MILESTONE)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
