//! A constant read through an app constant that aliases a gem's namespace.
//!
//! Core's `Metafields::Types` writes `MeasuredSupport =
//! ShopifyMetafields::Types::MeasuredSupport`, and
//! `VolumeOptionHelper` then writes `SupportedMeasuredUnit =
//! Metafields::Types::MeasuredSupport::MeasuredUnits::SupportedMeasuredUnit`
//! (and `Hash[SupportedMeasuredUnit, String]` as a declared type). The
//! shopify_metafields gem declares that class; the CRuby tree loads the
//! gem, so the read runs there. It was refused ("constant not supported
//! (all targets)"), and the file's assignments emitted as `raise`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    shopify_metafields (0.12.31)\n\nDEPENDENCIES\n  activesupport\n  shopify_metafields\n";

const RBI: &str = r#"# typed: true
module ShopifyMetafields; end
module ShopifyMetafields::Types; end
module ShopifyMetafields::Types::MeasuredSupport; end
module ShopifyMetafields::Types::MeasuredSupport::MeasuredUnits; end
class ShopifyMetafields::Types::MeasuredSupport::MeasuredUnits::SupportedMeasuredUnit < ::T::Enum
  enums do
    LITERS = new
  end
end
"#;

const GEMS: &str = "runtime_gems:\n  - {constant: ShopifyMetafields, gem: shopify_metafields}\n";

const TYPES: &str = "module Metafields\n  module Types\n    MeasuredSupport = ShopifyMetafields::Types::MeasuredSupport\n  end\nend\n";

const HELPER: &str = r#"module Metafields
  module Types
    module VolumeOptionHelper
      SupportedMeasuredUnit = Metafields::Types::MeasuredSupport::MeasuredUnits::SupportedMeasuredUnit

      MAPPING = {
        SupportedMeasuredUnit::LITERS => "LITERS",
      }.freeze #: Hash[SupportedMeasuredUnit, String]
    end
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\nextensions:\n  - ext\n")
        .write("ext/gems.yml", GEMS)
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/shopify_metafields@0.12.31.rbi", RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/metafields/types.rb", TYPES)
        .write("app/models/metafields/types/volume_option_helper.rb", HELPER)
}

#[test]
fn a_read_through_an_alias_of_a_gem_namespace_runs_on_the_cruby_tree() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(
        !errors.iter().any(|e| e.contains("MeasuredUnits::SupportedMeasuredUnit") || e.contains("SupportedMeasuredUnit has no indexed")),
        "{errors:#?}"
    );
    let helper = std::fs::read_to_string(dir.join("app/models/metafields/types/volume_option_helper.rb")).expect("helper");
    assert!(!helper.contains("raise"), "{helper}");
    assert!(helper.contains("Metafields::Types::MeasuredSupport::MeasuredUnits::SupportedMeasuredUnit"), "{helper}");
}
