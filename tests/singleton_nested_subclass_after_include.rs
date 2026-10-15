//! A nested subclass of a class that includes `Singleton` is declared
//! below the include, so `Singleton`'s `inherited` hook gives it its own
//! instance and mutex. Core's `MarketingEventHierarchyLevel` does, and
//! the emit wrote its nested `Ad`, `AdGroup` and `Campaign` above the
//! include: `Ad.instance` raised "undefined method 'synchronize' for
//! nil" when the constant list mapped them, which failed the schema
//! build.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_nested_subclass_of_a_singleton_class_has_its_own_instance() {
    let run = emit_and_run::empty_app()
        .write(
            "app/models/hierarchy_level.rb",
            "class HierarchyLevel\n  include Singleton\n\n  attr_reader :serialize\n\n  def initialize\n    @serialize = self.class.name.demodulize.underscore.tr(\"_\", \" \")\n  end\n\n  \
             class Ad < HierarchyLevel; end\n  class AdGroup < HierarchyLevel; end\n\n  VALUES = [Ad, AdGroup].map(&:instance).freeze\nend\n",
        )
        .run_ruby("p HierarchyLevel::VALUES.map(&:serialize)\np HierarchyLevel::Ad.instance.equal?(HierarchyLevel::VALUES.first)\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[\"ad\", \"ad group\"]\ntrue\n", "{}", run.stderr);
}
