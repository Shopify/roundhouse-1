//! A `Data.define` constant with a block, reopened by a later `class` of
//! the same name in the same file, against native Ruby.
//!
//! Core's managed_markets/adaptive_pricing/breakdown.rb assigns
//! `Breakdown = Data.define(...) do ... end`, then reopens `class Breakdown`
//! to define `Inclusions = Data.define(...)` and `Metadata = Data.define(...)`
//! in it. The emitted file declared an empty `class Breakdown` first and
//! then assigned the constant: "already initialized constant
//! ManagedMarkets::AdaptivePricing::Breakdown", which Core's StrictWarning
//! turns fatal. Ruby defines the constant once, as the Data class, with
//! the block evaluated in it; the reopen adds to that class.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SHAPES: &str = r#"module Shapes
  Box = Data.define(:w, :h, :label) do
    class << self
      def unit
        Box.new(w: 1, h: 1)
      end
    end

    def initialize(w:, h:, label: nil) = super

    def area = w * h

    def +(other)
      Box.new(w: w + other.w, h: h + other.h, label: label)
    end

    def tagged
      Box::Tag.new(name: label || "none")
    end
  end

  class Box
    Tag = Data.define(:name) do
      def shout = name.upcase
    end

    Corner = Data.define(
      :x,
      :y,
    ) do
      def to_s = "(#{x}, #{y})"
    end

    def corner = Corner.new(x: w, y: h)
  end
end
"#;

/// The probe's own lines, for native Ruby and the emitted tree alike.
const SCRIPT: &str = r#"box = Shapes::Box.new(w: 2, h: 3, label: "lid")
sum = box + Shapes::Box.unit
puts [box.area, sum.w, sum.h, sum.label].inspect
puts [Shapes::Box.superclass, Shapes::Box.members].inspect
puts [box.tagged.shout, Shapes::Box::Tag.superclass, box.corner.to_s].inspect
puts Shapes::Box.unit.inspect
"#;

/// What Ruby prints (checked natively), so two equal wrong answers fail.
const EXPECTED: &str = "[6, 3, 4, \"lid\"]\n\
[Data, [:w, :h, :label]]\n\
[\"LID\", Data, \"(2, 3)\"]\n\
#<data Shapes::Box w=1, h=1, label=nil>\n";

#[test]
fn a_data_define_constant_reopened_by_its_class_is_defined_once_as_in_ruby() {
    let dir = std::env::temp_dir().join(format!("rh-data-define-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("shapes.rb");
    std::fs::write(&source, SHAPES).unwrap();
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "module Strict\n  def warn(msg, **) = msg.include?(\"already initialized\") ? raise(msg) : super\nend\nWarning.extend(Strict)\nrequire {:?}\n{SCRIPT}",
            source.display().to_string()
        ))
        .output()
        .expect("ruby");
    let _ = std::fs::remove_dir_all(&dir);
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.status.success(), "native: {native_out}\n{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(native_out, EXPECTED, "native");

    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/shapes/box.rb", SHAPES)
        .run_ruby(&format!(
            "module Strict\n  def warn(msg, **) = msg.include?(\"already initialized\") ? raise(msg) : super\nend\nWarning.extend(Strict)\n{SCRIPT}"
        ));
    assert!(run.errors.is_empty(), "{:#?}", run.errors);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, native_out, "emitted\n{}", run.stderr);
    // Boot loads the file before the script's hook: no constant defined twice.
    assert!(!run.stderr.contains("already initialized"), "{}", run.stderr);
}
