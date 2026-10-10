//! An include of a module through an alias constant.
//!
//! Core's `Metafields::Types` aliases the gem's module (`OptionHelpers =
//! ShopifyMetafields::Types::OptionHelpers`), and its reference types
//! include it by the alias (`include ::Metafields::Types::OptionHelpers`,
//! or `include OptionHelpers` from inside the namespace). The alias names
//! no class, so the include was refused as unresolved and the module's
//! methods were missing from the includer.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::ingest::ingest_app_from_tree;

/// The gem the tree's `require "shopify_metafields"` loads, gated on the lockfile.
const GEMS: &str = "lock_gated_requires:\n  - {require: shopify_metafields, gem: shopify_metafields}\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    shopify_metafields (0.12.31)\n\nDEPENDENCIES\n  shopify_metafields\n";

const RBI: &str = r#"# typed: true
module ShopifyMetafields; end
module ShopifyMetafields::Types; end
module ShopifyMetafields::Types::OptionHelpers
  sig { returns(String) }
  def option_label; end
end
"#;

const TYPES: &str = r#"
module Metafields
  module Types
    OptionHelpers = ShopifyMetafields::Types::OptionHelpers
    Missing = Nowhere::Helpers
  end
end
"#;

const ROOTED: &str = r#"
module Metafields
  module Types
    class CompanyReferenceType
      include ::Metafields::Types::OptionHelpers

      #: -> String
      def label = option_label
    end
  end
end
"#;

const RELATIVE: &str = r#"
module Metafields
  module Types
    class PageReferenceType
      include OptionHelpers

      #: -> String
      def label = option_label
    end
  end
end
"#;

const MISSING: &str = r#"
module Metafields
  module Types
    class BrokenType
      include Missing
    end
  end
end
"#;

fn errors() -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("Gemfile.lock", LOCK),
        ("roundhouse.yml", "extensions:\n  - ext\n"),
        ("ext/gems.yml", GEMS),
        ("sorbet/rbi/gems/shopify_metafields@0.12.31.rbi", RBI),
        ("app/models/metafields/types.rb", TYPES),
        ("app/models/metafields/types/company_reference_type.rb", ROOTED),
        ("app/models/metafields/types/page_reference_type.rb", RELATIVE),
        ("app/models/metafields/types/broken_type.rb", MISSING),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    diagnose(&app).into_iter().map(|d| d.to_string()).collect()
}

#[test]
fn an_include_by_alias_includes_the_aliased_module() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("OptionHelpers") || e.contains("option_label")), "{errors:?}");
}

#[test]
fn an_alias_of_an_unknown_module_stays_refused() {
    let errors = errors();
    assert!(errors.iter().any(|e| e.contains("BrokenType includes unresolved")), "{errors:?}");
}
