//! A nested class the source forward-declares above the class-body
//! statement that reads it, and reopens with its body further down
//! (Core's components/platform/essentials component/schema.rb, e2e
//! round 7):
//!
//! ```ruby
//! class Schema
//!   class Validator; end
//!   private_constant :Validator
//!   class_attribute :_validator_class, …, default: Validator
//!   …
//!   class Validator
//!     include ActiveModel::Validations
//! ```
//!
//! Ruby runs a class body top to bottom, so the empty `class Validator`
//! defines the constant before `class_attribute` reads it. The two
//! declarations merge into one class whose earliest body position is the
//! reopen's, and the emitted block went below the seed
//! (`@__class_attr__validator_class = Component::Schema::Validator`):
//! NameError at load, failing every test that loads a schema.
//!
//! And a module the source writes above the sibling classes that
//! include it (Core's checkouts/one test helper web/client.rb:
//! `module ResultDataForNegotiationInput` then `class
//! SessionNegotiationInputFromResult … include(ResultDataForNegotiationInput)`)
//! stays above them. So does one another file of the class writes
//! above the classes there that include it: rack-oauth2's
//! server/authorize.rb requires authorize/error.rb at its end, which
//! writes `module ErrorHandler` above `class BadRequest …; include
//! ErrorHandler`; merged into authorize.rb, the module followed them.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"module Shape
  class Schema
    extend Comparable

    class InvalidError < RuntimeError
      def initialize(message)
        super(message)
      end
    end

    # Forward declaration, defined at the bottom.
    class Validator; end
    private_constant :Validator

    class_attribute :_validator_class, instance_accessor: false, instance_predicate: false, default: Validator
    class_attribute :_internal_attributes, instance_accessor: false, instance_predicate: false, default: {}

    class << self
      def inherited(subclass)
        subclass._validator_class = Class.new(_validator_class)
        subclass._internal_attributes = _internal_attributes.dup
        super
      end

      def validator_name
        _validator_class.label
      end
    end

    class Attribute
      NO_DEFAULT = Object.new

      def initialize(name)
        @name = name
      end
    end
    private_constant :Attribute

    class Validator
      def self.label
        "validator"
      end

      def initialize(target)
        @target = target
      end
    end
  end
end
"##;

const PERSON: &str = r##"class PersonSchema < Shape::Schema
end
"##;

const CLIENT: &str = r##"class Client
  module SessionHelper
    def build_session(token)
      "session #{token}"
    end
  end

  class Input
    extend(SessionHelper)

    class << self
      def from_token(token)
        new.tap { |input| input.session = build_session(token) }
      end
    end

    attr_accessor :session

    def variables
      { "base" => true }
    end
  end

  module FromResult
    def from_result
      "from #{@result}"
    end
  end

  class InputFromResult < Input
    extend(SessionHelper)
    include(FromResult)

    def initialize(result)
      @result = result
    end
  end
end
"##;

const GATE: &str = r##"class Gate
  class Request
    def label
      "request"
    end
  end
end

require_relative "gate/error"
"##;

const GATE_ERROR: &str = r##"class Gate
  module ErrorHandler
    def handled?
      true
    end
  end

  class BadRequest < StandardError
    include ErrorHandler
  end

  class ServerError < StandardError
    include ErrorHandler
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class NestedForwardDeclarationTest < ActiveSupport::TestCase
  test "a forward-declared nested class is defined before the body reads it" do
    assert_equal "validator", Shape::Schema.validator_name
    assert_equal "validator", PersonSchema.validator_name
    assert_equal Shape::Schema.send(:_validator_class), PersonSchema.send(:_validator_class).superclass
  end

  test "a nested module stays above the sibling that includes it" do
    assert_equal "from r", Client::InputFromResult.new("r").from_result
    assert_equal({ "base" => true }, Client::InputFromResult.new("r").variables)
    assert_equal "session t", Client::Input.from_token("t").session
  end

  test "a nested module another file writes above its includers stays above them" do
    assert Gate::BadRequest.new.handled?
    assert Gate::ServerError.new.handled?
    assert_equal "request", Gate::Request.new.label
  end
end
"##;

#[test]
fn a_forward_declared_nested_class_loads_before_the_body_statements_below_it() {
    emit_and_run::empty_app()
        .write("app/models/shape/schema.rb", SCHEMA)
        .write("app/models/person_schema.rb", PERSON)
        .write("app/models/client.rb", CLIENT)
        .write("app/models/gate.rb", GATE)
        .write("app/models/gate/error.rb", GATE_ERROR)
        .write("test/models/nested_forward_declaration_test.rb", TEST)
        .run_test("test/models/nested_forward_declaration_test.rb")
        .assert_passes();
}

/// A nested module whose body keeps no member of its own (hedwig's
/// configuration/enqueue_retry_handler.rb: `module Interface` holds only
/// `extend T::Sig`, `interface!` and abstract sig-only methods) above the
/// sibling class that includes it. With no position, the module was
/// written after the class: NameError at load, so `Hedwig::Configuration`
/// never loaded and its `run_load_hooks(:hedwig)` never fired.
const HANDLER: &str = r##"class Handler
  module Interface
  end

  class InMemory
    include Interface

    def label
      "in memory"
    end
  end
end
"##;

const HANDLER_TEST: &str = r##"require "test_helper"

class PositionlessNestedModuleTest < ActiveSupport::TestCase
  test "a nested module with no members stays above the sibling that includes it" do
    assert_equal "in memory", Handler::InMemory.new.label
    assert Handler::InMemory.include?(Handler::Interface)
  end
end
"##;

#[test]
fn a_nested_module_with_no_members_loads_before_the_sibling_that_includes_it() {
    emit_and_run::empty_app()
        .write("app/models/handler.rb", HANDLER)
        .write("test/models/positionless_nested_module_test.rb", HANDLER_TEST)
        .run_test("test/models/positionless_nested_module_test.rb")
        .assert_passes();
}

/// A nested module whose constants build the enclosing class, written
/// below that class's `initialize` (Core's checkouts/one
/// web/violation_error_message.rb: `def initialize(code,
/// non_localized_description, msg_args: {}, i18n_key: nil)` then
/// `module Proposal; LineItemLimitReached = ViolationErrorMessage.new(…)`).
/// Placed above the parent's methods, each constant reached
/// `BasicObject#initialize` (given 3, expected 0) at load.
const VIOLATION: &str = r##"module Checkout
  class Violation
    module Codes
      LIMIT = "proposal.limit"
    end

    def self.from(violation)
      violation
    end

    def initialize(code, description, i18n_key: nil)
      @code = code
      @description = description
      @i18n_key = i18n_key
    end

    attr_reader :code, :description, :i18n_key

    module Proposal
      LimitReached = Violation.new(Codes::LIMIT, "Too many lines.", i18n_key: "errors.limit")
      Again = Violation.from(LimitReached)
    end
  end
end
"##;

const VIOLATION_TEST: &str = r##"require "test_helper"

class NestedConstantBuildsParentTest < ActiveSupport::TestCase
  test "a nested module's constant built with the parent's initialize loads after it" do
    assert_equal "proposal.limit", Checkout::Violation::Proposal::LimitReached.code
    assert_equal "errors.limit", Checkout::Violation::Proposal::LimitReached.i18n_key
    assert_same Checkout::Violation::Proposal::LimitReached, Checkout::Violation::Proposal::Again
  end
end
"##;

#[test]
fn a_nested_module_building_its_parent_loads_after_the_parents_defs() {
    emit_and_run::empty_app()
        .write("app/models/checkout/violation.rb", VIOLATION)
        .write("test/models/nested_constant_builds_parent_test.rb", VIOLATION_TEST)
        .run_test("test/models/nested_constant_builds_parent_test.rb")
        .assert_passes();
}
