//! Constants built by Kernel's conversion functions, and top-level
//! constants read in a `SimpleDelegator` subclass, on the CRuby tree
//! against CRuby.
//!
//! shopify-money writes `DECIMAL_ZERO = BigDecimal(0).freeze` in
//! `Money::Helpers`, and its `Money::Allocator < SimpleDelegator` reads
//! `Float::EPSILON` and `Rational`. Analysis typed no receiverless
//! `BigDecimal(...)` / `Integer(...)`, so every read of such a constant
//! was "constant not supported". It also left a top-level name in a
//! Delegator subclass unresolved. Delegator descends from BasicObject,
//! and Ruby reaches the name through `Delegator.const_missing`
//! (`::Object.const_get`). The emitted tree must print what CRuby
//! prints, line for line, with no diagnostics.
//!
//! The constant read in a plain `BasicObject` subclass has no such hook.
//! Ruby raises NameError there, and analysis still refuses it.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const LIB: &str = r#"require "bigdecimal"
require "delegate"

module Amounts
  extend self

  DECIMAL_ZERO = BigDecimal(0).freeze
  ONE = BigDecimal("1")
  COUNT = Integer("42")
  RATE = Float("0.5")
  LABEL = String(:amounts)
  THIRD = Rational(1, 3)
  MAYBE = Integer("x", exception: false)

  def zero
    DECIMAL_ZERO
  end

  def describe
    [DECIMAL_ZERO.zero?, DECIMAL_ZERO.frozen?, (ONE + DECIMAL_ZERO).to_s("F"), COUNT + 1, RATE * 2, LABEL, THIRD + THIRD, MAYBE.inspect].join(" ")
  end
end

class Splitter < SimpleDelegator
  ONE = BigDecimal("1")

  def check(splits)
    splits = splits.map { |split| split.is_a?(Rational) ? split : split.to_s.to_r }
    total = splits.inject(0, :+)
    raise ArgumentError, "allocations add to more than 100%" if (total - ONE) > Float::EPSILON

    [splits.all? { |split| split.is_a?(Rational) }, total.to_s, __getobj__.to_s]
  end
end
"#;

const SCRIPT: &str = r#"
puts "zero: #{Amounts.zero.inspect}"
puts "describe: #{Amounts.describe}"
s = Splitter.new(10)
puts "check: #{s.check([Rational(1, 3), Rational(2, 3)]).inspect}"
puts "check floats: #{s.check([0.25, 0.5]).inspect}"
begin
  s.check([Rational(2, 3), Rational(2, 3)])
rescue ArgumentError => e
  puts "raised: #{e.message}"
end
"#;

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn conversion_constants_and_delegator_top_level_names_run_as_in_cruby() {
    let scratch = std::env::temp_dir().join(format!("roundhouse-kernel-conversions-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("amounts.rb"), LIB).unwrap();
    let native = emit_and_run::ruby()
        .arg("-W1")
        .arg("-e")
        .arg(format!("load {:?}\n{SCRIPT}", scratch.join("amounts.rb").display().to_string()))
        .output()
        .expect("CRuby control");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    for probe in [
        "zero: 0.0",
        "describe: true true 1.0 43 1.0 amounts 2/3 nil",
        "check: [true, \"1/1\", \"10\"]",
        "check floats: [true, \"3/4\", \"10\"]",
        "raised: allocations add to more than 100%",
    ] {
        assert!(expected.iter().any(|l| l == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/amounts.rb", LIB)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-W1")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed: {stderr}");
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\n{stderr}");
}

/// No hook answers a top-level name in a BasicObject subclass: Ruby
/// raises NameError, so analysis keeps refusing it.
#[test]
fn a_top_level_name_in_a_basic_object_subclass_stays_refused() {
    let errors = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/bare.rb", "class Bare < BasicObject\n  def r\n    Rational\n  end\nend\n")
        .emit(BuildTarget::Ruby)
        .1;
    assert!(errors.iter().any(|e| e.contains("constant not supported") && e.contains("Rational")), "{errors:#?}");
}
