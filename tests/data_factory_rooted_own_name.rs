//! A class that names its own value object `Data`, built from the rooted
//! `::Data` (Core's `GraphApi::Admin::Market::ConsolidatedShippingOption`):
//! the file declares a constant called `Data`, but the factory reads the
//! core class, so the file's classes are not refused. The whole class was
//! left out of the emitted app, and `Market.consolidatedShippingOptions`
//! failed to build its return type.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_constant_named_data_built_from_rooted_data_is_not_a_shadowed_generator() {
    let run = emit_and_run::empty_app()
        .write(
            "app/models/shipping_option.rb",
            "class ShippingOption\n  Data = ::Data.define(:name) do\n    def id = name\n  end\n\n  def self.build(name) = Data.new(name: name)\nend\n",
        )
        .run_ruby("o = ShippingOption.build(\"standard\")\np [ShippingOption::Data.members, o.id]\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[:name], \"standard\"]\n", "{}", run.stderr);
}
