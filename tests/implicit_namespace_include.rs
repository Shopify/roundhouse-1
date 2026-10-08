//! An include of a module that only nests other declarations.
//!
//! Core's `ShopifyPayments::Kyc::BusinessPlatform` is a Zeitwerk implicit
//! namespace: a directory of classes with no file of its own. Tax invoice
//! builders `include Kyc::BusinessPlatform` and read its classes bare.
//! The include was refused as `includes unresolved` (no class models the
//! module), though the bare read already resolved through it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BUSINESS_TYPE: &str = r#"module Pay
  module Kyc
    module Platform
      class BusinessType
        def self.label = "corp"
      end
    end
  end
end
"#;

const BUILDER: &str = r#"module Pay
  module Tax
    class Builder
      include Kyc::Platform

      def label = BusinessType.label
    end
  end
end
"#;

const MISSING: &str = r#"module Pay
  class Broken
    include Kyc::Nowhere
  end
end
"#;

#[test]
fn an_included_implicit_namespace_resolves_and_runs() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/pay/kyc/platform/business_type.rb", BUSINESS_TYPE)
        .write("app/models/pay/tax/builder.rb", BUILDER)
        .run_ruby("puts Pay::Tax::Builder.new.label\nputs Pay::Tax::Builder.include?(Pay::Kyc::Platform)\n");
    run.assert_passes();
    assert_eq!(run.stdout, "corp\ntrue\n");
    let builder = std::fs::read_to_string(run.emitted.join("app/models/pay/tax/builder.rb")).expect("builder.rb");
    assert!(!builder.contains("include not supported"), "{builder}");
}

// `ShopifyPayments::Kyc` holds only the implicit `Kyc::BusinessPlatform`,
// so the modeled class sits two namespaces down.
const OUTER_BUILDER: &str = r#"module Pay
  module Tax
    class Outer
      include Kyc

      def label = Platform::BusinessType.label
    end
  end
end
"#;

#[test]
fn an_included_namespace_of_namespaces_resolves_and_runs() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/pay/kyc/platform/business_type.rb", BUSINESS_TYPE)
        .write("app/models/pay/tax/outer.rb", OUTER_BUILDER)
        .run_ruby("puts Pay::Tax::Outer.new.label\nputs Pay::Tax::Outer.include?(Pay::Kyc)\n");
    run.assert_passes();
    assert_eq!(run.stdout, "corp\ntrue\n");
    let outer = std::fs::read_to_string(run.emitted.join("app/models/pay/tax/outer.rb")).expect("outer.rb");
    assert!(!outer.contains("include not supported"), "{outer}");
}

#[test]
fn an_include_naming_no_namespace_stays_refused() {
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/pay/kyc/platform/business_type.rb", BUSINESS_TYPE),
        ("app/models/pay/broken.rb", MISSING),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(errors.iter().any(|e| e.contains("includes unresolved Kyc::Nowhere")), "{errors:?}");
}
