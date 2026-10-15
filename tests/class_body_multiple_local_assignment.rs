//! A class body's multiple assignment of locals, read by the statements
//! after it: Core's `GraphApi::Admin::ShopifyPayments::CapabilityType`
//! partitions `ChargeCapabilityType.values` into `gated, ungated` and
//! then declares a value for each. The assignment was dropped and the
//! reads were bare method calls: `undefined local variable or method
//! 'ungated_charge_capabilities'`, which failed the schema build.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_multiple_assignment_of_locals_is_read_by_the_statements_after_it() {
    let run = emit_and_run::empty_app()
        .write(
            "app/models/capability_type.rb",
            "class CapabilityType\n  NAMES = %w[card debit payout alipay refund].freeze\n\n  retired = [\"alipay\"]\n\n  \
             gated, ungated =\n    NAMES.partition do |name|\n      name.start_with?(\"d\", \"p\") && !retired.include?(name)\n    end\n\n  \
             GATED = gated.map(&:upcase)\n  UNGATED = ungated.map(&:upcase)\n  ungated.each { |name| (VALUES ||= []) << name }\nend\n",
        )
        .run_ruby("p CapabilityType::GATED\np CapabilityType::UNGATED\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[\"DEBIT\", \"PAYOUT\"]\n[\"CARD\", \"ALIPAY\", \"REFUND\"]\n", "{}", run.stderr);
}
