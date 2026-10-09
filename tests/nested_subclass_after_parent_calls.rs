//! A nested subclass the source writes below its parent's class-body
//! calls (Core's customer_foundations/phone_number.rb, e2e round 7):
//!
//! ```ruby
//! class PhoneNumber < SimpleValueObject
//!   attribute(:value, :string)
//!   …
//!   class Valid < PhoneNumber
//!     transform(:value, :perform_compatibility_decomposition)
//! ```
//!
//! ComplexStruct's `inherited` hook copies the parent's attributes into
//! the subclass when `class Valid < PhoneNumber` runs, so `Valid` has
//! `:value` only if the parent's `attribute` ran first. The emitted
//! class spliced its nested classes right after its header, above the
//! replayed `attribute(:value, :string)`, and `transform` raised
//! "Attribute value is not defined" at load. A nested class now follows
//! the class-body calls the source writes above it, and stays above the
//! ones it writes below it (`attribute :address, Address`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const VALUE_OBJECT: &str = r##"class ValueObject
  def self.attributes_table
    @attributes_table ||= {}
  end

  def self.attributes_table=(value)
    @attributes_table = value
  end

  def self.inherited(subclass)
    subclass.attributes_table = attributes_table.dup
    super
  end

  def self.attribute(name, type = nil)
    raise "Attempted to redefine attribute #{name}" if attributes_table.key?(name)

    attributes_table[name] = type
  end

  def self.transform(name)
    raise "Attribute #{name} is not defined" unless attributes_table.key?(name)

    (@transforms ||= []) << name
  end

  def self.transforms
    @transforms || []
  end
end
"##;

const PHONE_NUMBER: &str = r##"class PhoneNumber < ValueObject
  attribute(:value, :string)

  def self.kinds
    [Valid, Invalid]
  end

  class Valid < PhoneNumber
    transform(:value)
  end

  class Invalid < PhoneNumber
    transform(:value)
  end
end
"##;

const CUSTOMER: &str = r##"class Customer < ValueObject
  class Address < ValueObject
    attribute(:city, :string)
  end

  attribute(:email, :string)
  attribute(:address, Address)

  class Primary < Customer
    transform(:email)
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class PhoneNumberTest < ActiveSupport::TestCase
  test "a nested subclass inherits the attributes its parent declared above it" do
    assert_equal [:value], PhoneNumber::Valid.attributes_table.keys
    assert_equal [:value], PhoneNumber::Invalid.transforms
    assert_equal [PhoneNumber::Valid, PhoneNumber::Invalid], PhoneNumber.kinds
  end

  test "a nested class the parent's calls read stays above them" do
    assert_equal Customer::Address, Customer.attributes_table[:address]
    assert_equal [:email, :address], Customer::Primary.attributes_table.keys
    assert_equal [:city], Customer::Address.attributes_table.keys
  end
end
"##;

#[test]
fn a_nested_subclass_follows_the_parent_calls_the_source_writes_above_it() {
    emit_and_run::empty_app()
        .write("app/models/value_object.rb", VALUE_OBJECT)
        .write("app/models/phone_number.rb", PHONE_NUMBER)
        .write("app/models/customer.rb", CUSTOMER)
        .write("test/models/phone_number_test.rb", TEST)
        .run_test("test/models/phone_number_test.rb")
        .assert_passes();
}
