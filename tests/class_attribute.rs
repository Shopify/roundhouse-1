//! ActiveSupport's `class_attribute` through ingest, lowering and Ruby
//! emit, on models and on plain library classes.
//!
//! The class-side reader is inherited and live (a subclass reads its
//! parent's value until it writes its own), the instance reader falls back
//! to the class until the instance writes, and the `instance_accessor`,
//! `instance_reader`, `instance_writer`, `instance_predicate` and
//! `default` options decide which of those methods exist. The defs are
//! public whatever visibility the body is in at the declaration. Before
//! this was modelled, the model call was reported "not lowered" and the
//! library class call "not modelled and dropped": the emitted program had
//! no `Shop.gate` at all.
//!
//! Each probe's control is CRuby running the authored source against the
//! installed ActiveSupport/ActiveRecord; the emitted tree, which loads
//! neither, must print the same lines.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIBRARY_SOURCE: &str = r#"class CaThing
  class_attribute :setting, default: "x"
end

class CaSub < CaThing
end

class CaOther
  def self.base
    5
  end

  private

  class_attribute :knob, instance_writer: false
  self.knob = base + 1
  self.knob += 10
  include Comparable

  public

  def show
    [knob, knob?]
  end
end
"#;

const LIBRARY_SCRIPT: &str = r#"p CaSub.setting
CaSub.setting = "y"
p [CaThing.setting, CaSub.setting, CaThing.new.setting, CaSub.setting?]
o = CaSub.new
o.setting = "own"
p [o.setting, o.setting?, CaSub.new.setting, CaSub.setting]
CaThing.setting = nil
p [CaThing.setting?, CaThing.new.setting?]
p [CaOther.knob, CaOther.new.knob, CaOther.new.show, CaOther.new.respond_to?(:knob=)]
p [CaOther.public_method_defined?(:knob), CaOther.public_method_defined?(:knob?), CaOther.include?(Comparable)]
"#;

const PLAN: &str = r##"class Plan < ApplicationRecord
  def self.start
    :s
  end

  private

  def helper
    1
  end

  class_attribute :tier, :level, default: :basic
  class_attribute :gate, instance_accessor: false, instance_predicate: false
  self.tier = start
  self.gate = :closed

  public

  def tier_label
    "#{tier}-#{level}"
  end
end
"##;

const PRO_PLAN: &str = r#"class ProPlan < Plan
  self.tier = :pro
end
"#;

const MODEL_SCRIPT: &str = r#"p [Plan.tier, Plan.level, ProPlan.tier, ProPlan.level, ProPlan.tier?, Plan.gate, ProPlan.gate]
Plan.level = :gold
p [ProPlan.level, Plan.level]
ProPlan.gate = :open
p [Plan.gate, ProPlan.gate]
p [Plan.respond_to?(:gate?), Plan.method_defined?(:gate), Plan.method_defined?(:gate=), Plan.method_defined?(:tier?)]
p [Plan.public_method_defined?(:tier), Plan.public_method_defined?(:level=), Plan.private_method_defined?(:helper), Plan.public_method_defined?(:tier_label)]
"#;

fn native_lines(prelude: &str, source: &str, script: &str) -> Vec<String> {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{prelude}\n{source}\n{script}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
fn library_class_attribute_behaves_as_in_activesupport_after_emission() {
    let expected = native_lines(
        "require \"active_support\"\nrequire \"active_support/core_ext/class/attribute\"",
        LIBRARY_SOURCE,
        LIBRARY_SCRIPT,
    );
    // The control exercises inheritance, the instance override and the
    // options, or matching it would prove nothing.
    for probe in [
        r#"["x", "y", "x", true]"#,
        r#"["own", true, "y", "y"]"#,
        "[16, 16, [16, true], false]",
        "[true, true, true]",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    let run = emit_and_run::real_blog()
        .write("app/services/class_attribute_probe.rb", LIBRARY_SOURCE)
        .run_ruby(LIBRARY_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from ActiveSupport line for line");
}

#[test]
fn model_class_attribute_behaves_as_in_activerecord_after_emission() {
    let expected = native_lines(
        "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend",
        &format!("{PLAN}\n{PRO_PLAN}"),
        MODEL_SCRIPT,
    );
    for probe in [
        "[:s, :basic, :pro, :basic, true, :closed, :closed]",
        "[:gold, :gold]",
        "[:closed, :open]",
        "[false, false, false, true]",
        "[true, true, true, true]",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    let run = emit_and_run::real_blog()
        .write("app/models/plan.rb", PLAN)
        .write("app/models/pro_plan.rb", PRO_PLAN)
        .run_ruby(MODEL_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from ActiveRecord line for line");
}
