//! An include whose `self.included(base)` hook sends `base` a class macro
//! runs where the class body has the `include`, after the macro calls
//! above it (Ruby's `Module#include` runs `included` at the call).
//!
//! ```ruby
//! class Item < TypeBase
//!   declare_access :read
//!   include Listing::Checks   # hook: base.guard { ... }
//! ```
//!
//! `guard` raises until `declare_access` has run. The include bucket
//! opened the emitted body, so the hook ran first: the class failed to
//! load, Ruby dropped the half-defined autoload, and the retry then found
//! the nested module's own file already required, a NameError for
//! `Listing::Checks` on the second incarnation of the namespace.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TYPE_BASE: &str = r##"class TypeBase
  def self.declare_access(scope)
    @scope = scope
  end

  def self.scope
    @scope
  end

  def self.guard(&block)
    raise "no access declared for #{name}" unless @scope
    (@guards ||= []) << block
  end

  def self.guards
    @guards || []
  end
end
"##;

/// The namespace file, whose nested module the sibling class includes.
const LISTING: &str = r##"module Listing
  module Checks
    class << self
      def included(base)
        base.guard { |item| item.ok? }
      end
    end
  end
end
"##;

const ITEM: &str = r##"module Listing
  class Item < TypeBase
    declare_access :read
    include Listing::Checks

    def ok?
      true
    end
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class ListingTest < ActiveSupport::TestCase
  test "the include's hook runs after the macro calls above it" do
    assert_equal :read, Listing::Item.scope
    assert_equal 1, Listing::Item.guards.size
    assert Listing::Item.guards.first.call(Listing::Item.new)
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/listing_test.rb", TEST)
        .write("app/models/type_base.rb", TYPE_BASE)
        .write("app/models/listing.rb", LISTING)
        .write("app/models/listing/item.rb", ITEM)
}

#[test]
fn an_include_with_a_macro_sending_hook_follows_the_macro_calls_above_it() {
    let (dir, errors) = app().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let item = std::fs::read_to_string(dir.join("app/models/listing/item.rb")).unwrap();
    let at = |needle: &str| item.find(needle).unwrap_or_else(|| panic!("{needle}: {item}"));
    assert!(at("declare_access :read") < at("include Listing::Checks"), "{item}");
}

#[test]
fn an_include_with_a_macro_sending_hook_loads_and_runs_its_hook() {
    let run = app().run_test("test/models/listing_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("ListingTest: 1 tests passed"), "{}", run.stdout);
}
