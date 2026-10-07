//! A tableless ActiveModel class nested in a namespace module whose body
//! also declares constants, a singleton method and a plain class — Core's
//! `inventory/restock_calculation_schema.rb`:
//!
//! ```ruby
//! module Inventory
//!   module RestockCalculationSchema
//!     NO_RESTOCK_TYPE = "no_restock"
//!     class RestockRequest
//!       include ActiveModel::Validations
//! ```
//!
//! The include makes the file a model, and the model file kept only the
//! classes nested in the model: the wrapper module, and everything its
//! body declares, was dropped, so every read of `NO_RESTOCK_TYPE` was
//! "constant not supported", the model's own methods reading the
//! wrapper's constants included. Without the include
//! the same file took the library path and every constant resolved.
//!
//! Checked clean, emitted, and run with CRuby: from another class, from
//! the nested ActiveModel class, and from a test; plus the same reads on
//! the emitted tree against the files run natively.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r##"module Inventory
  module RestockCalculationSchema
    NO_RESTOCK_TYPE = "no_restock"
    RETURN_TYPE = "return"
    CANCEL_TYPE = "cancel"
    LEGACY_TYPE = "legacy_restock"

    RESTOCKING_RESTOCK_TYPES = [CANCEL_TYPE, RETURN_TYPE, LEGACY_TYPE].freeze #: Array[String]
    ALL_RESTOCK_TYPES = RESTOCKING_RESTOCK_TYPES + [NO_RESTOCK_TYPE] #: Array[String]
    RESTOCK_TYPES = ALL_RESTOCK_TYPES - [LEGACY_TYPE] #: Array[String]

    #: (String) -> bool
    def self.known?(type)
      ALL_RESTOCK_TYPES.include?(type)
    end

    class RestockRequest
      include ActiveModel::Validations

      #: String?
      attr_reader :restock_type

      validates :restock_type, presence: true

      #: -> bool
      def known?
        RESTOCK_TYPES.include?(restock_type)
      end

      #: (String?) -> void
      def initialize(restock_type)
        super()
        @restock_type = restock_type
      end

      #: -> bool
      def restock?
        RESTOCKING_RESTOCK_TYPES.include?(restock_type)
      end

      #: -> bool
      def no_restock?
        restock_type == NO_RESTOCK_TYPE
      end
    end

    class Tally
      #: (Array[String]) -> Integer
      def self.restocking(types)
        types.count { |type| RESTOCKING_RESTOCK_TYPES.include?(type) }
      end
    end
  end
end
"##;

/// Another class, reading the wrapper's constants by their full path.
const REFUND_NOTE: &str = r##"class RefundNote
  NO_RESTOCK_TYPE = Inventory::RestockCalculationSchema::NO_RESTOCK_TYPE
  RESTOCKING_RESTOCK_TYPES = Inventory::RestockCalculationSchema::RESTOCKING_RESTOCK_TYPES

  #: (String) -> bool
  def self.no_restock?(type)
    type == NO_RESTOCK_TYPE
  end

  #: -> Integer
  def self.restocking_count
    RESTOCKING_RESTOCK_TYPES.size
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class RestockTest < ActiveSupport::TestCase
  test "the wrapper module's constants and methods" do
    assert_equal "no_restock", Inventory::RestockCalculationSchema::NO_RESTOCK_TYPE
    assert_equal 4, Inventory::RestockCalculationSchema::ALL_RESTOCK_TYPES.size
    assert_equal ["cancel", "return", "no_restock"], Inventory::RestockCalculationSchema::RESTOCK_TYPES
    assert Inventory::RestockCalculationSchema.known?("legacy_restock")
    assert_not Inventory::RestockCalculationSchema.known?("lost")
    assert_equal 2, Inventory::RestockCalculationSchema::Tally.restocking(["cancel", "no_restock", "return"])
  end

  test "another class reads them" do
    assert RefundNote.no_restock?("no_restock")
    assert_not RefundNote.no_restock?("cancel")
    assert_equal 3, RefundNote.restocking_count
  end

  test "the nested ActiveModel class reads them" do
    request = Inventory::RestockCalculationSchema::RestockRequest.new("cancel")
    assert request.restock?
    assert_not request.no_restock?
    assert request.valid?
    assert Inventory::RestockCalculationSchema::RestockRequest.new("no_restock").no_restock?
    assert request.known?
    assert_not Inventory::RestockCalculationSchema::RestockRequest.new("legacy_restock").known?
    assert_not Inventory::RestockCalculationSchema::RestockRequest.new(nil).valid?
  end
end
"##;

const FILES: [(&str, &str); 2] = [
    ("app/models/inventory/restock_calculation_schema.rb", SCHEMA),
    ("app/models/refund_note.rb", REFUND_NOTE),
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/restock_test.rb", TEST);
    for (path, source) in FILES {
        app = app.write(path, source);
    }
    app
}

/// Reads that need no ActiveModel, so plain Ruby answers them too.
const SCRIPT: &str = r##"schema = Inventory::RestockCalculationSchema
p [schema::NO_RESTOCK_TYPE, schema::ALL_RESTOCK_TYPES, schema::RESTOCK_TYPES]
p [schema.known?("return"), schema.known?("lost"), schema::Tally.restocking(["cancel", "x", "legacy_restock"])]
p [RefundNote.no_restock?("no_restock"), RefundNote.restocking_count, RefundNote::NO_RESTOCK_TYPE.equal?(schema::NO_RESTOCK_TYPE)]
p [schema.constants.sort.first(3), schema.const_defined?(:RestockRequest), schema.singleton_methods]
"##;

const EXPECTED: [&str; 4] = [
    r#"["no_restock", ["cancel", "return", "legacy_restock", "no_restock"], ["cancel", "return", "no_restock"]]"#,
    "[true, false, 2]",
    "[true, 3, true]",
    "[[:ALL_RESTOCK_TYPES, :CANCEL_TYPE, :LEGACY_TYPE], true, [:known?]]",
];

/// The same files on plain Ruby with a stand-in `ActiveModel::Validations`
/// that only accepts the class-level `validates` call (nothing in
/// `SCRIPT` validates).
fn native() -> String {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_am_wrapper_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("app/models/inventory")).unwrap();
    let mut program = String::from(
        "module ActiveModel\n  module Validations\n    def self.included(base) = base.extend(ClassMethods)\n    module ClassMethods\n      def validates(*) = nil\n    end\n  end\nend\n",
    );
    for (path, source) in FILES {
        std::fs::write(dir.join(path), source).unwrap();
        program.push_str(&format!("require_relative {:?}\n", path.trim_end_matches(".rb")));
    }
    program.push_str(SCRIPT);
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

/// The authored test: wrapper constants read from a test, from another
/// class, and from the nested ActiveModel class (validation included).
#[test]
fn the_wrapper_module_of_an_active_model_class_keeps_its_body() {
    let run = app().run_test("test/models/restock_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("RestockTest: 3 tests passed"), "{}", run.stdout);
}

#[test]
fn the_wrapper_module_reads_like_the_native_run() {
    let native = native();
    assert_eq!(native.lines().collect::<Vec<_>>(), EXPECTED, "native:\n{native}");
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    assert_eq!(emitted.stdout.lines().collect::<Vec<_>>(), EXPECTED, "emitted:\n{}", emitted.stdout);
}

/// The same shape with an ActiveRecord model in the wrapper: the
/// model path took only the model then too.
#[test]
fn the_wrapper_module_of_a_record_keeps_its_constants() {
    let (_, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table \"shipping_boxes\", force: :cascade do |t|\n    t.string \"unit\"\n  end\nend\n",
        )
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        )
        .write(
            "app/models/shipping/box.rb",
            "module Shipping\n  UNITS = [\"cm\", \"in\"].freeze #: Array[String]\n\n  class Box < ApplicationRecord\n    self.table_name = \"shipping_boxes\"\n    #: -> bool\n    def known_unit? = UNITS.include?(unit)\n  end\nend\n",
        )
        .write("app/models/label.rb", "class Label\n  #: -> Integer\n  def self.units = Shipping::UNITS.size\nend\n")
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
}

const BOX_SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  create_table \"shipping_boxes\", force: :cascade do |t|\n    t.string \"name\"\n  end\n  create_table \"shipping_box_measurements\", force: :cascade do |t|\n    t.string \"unit\"\n  end\nend\n";

const BOX: &str = r#"module Shipping
  class Box < ApplicationRecord
    self.table_name = "shipping_boxes"

    #: -> String
    def label = "box " + name.to_s
  end
end
"#;

/// `class Box` here reopens the model only to nest another one, beside
/// a plain helper class and a second record class.
const MEASUREMENT: &str = r#"module Shipping
  class Box
    class Measurement < ApplicationRecord
      self.table_name = "shipping_box_measurements"

      #: -> bool
      def known_unit? = Units.all.include?(unit)
    end

    class Units
      #: -> Array[String]
      def self.all = ["cm", "in"]
    end
  end
end
"#;

fn boxes(measurement: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("db/schema.rb", BOX_SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/shipping/box.rb", BOX)
        .write("app/models/shipping/box/measurement.rb", measurement)
        .write(
            "test/box_test.rb",
            "require \"test_helper\"\n\nclass BoxTest < ActiveSupport::TestCase\n  test \"box\" do\n    assert_equal 2, Shipping::Box::Units.all.size\n    assert Shipping::Box::Measurement.new(unit: \"cm\").known_unit?\n    box = Shipping::Box.create!(name: \"b\")\n    assert_equal \"box b\", Shipping::Box.find(box.id).label\n  end\nend\n",
        )
}

/// A wrapping CLASS is a reopen of the model of that name, not a
/// namespace this file owns: the model keeps its file and its
/// persistence, and a plain class beside the nested model still
/// registers.
#[test]
fn a_class_wrapper_of_a_model_leaves_the_model_whole() {
    let run = boxes(MEASUREMENT).run_test("test/models/box_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("BoxTest: 1 tests passed"), "{}", run.stdout);
}
