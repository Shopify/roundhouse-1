//! `defined?(Const)` of a constant only a Rails-dependent gem declares
//! answers nil: every read of the constant is refused by name
//! (gem_requires_rails), so the tree runs without the gem, and a guard
//! like Shopify Core's memcached_helper.rb
//! `return unless defined?(IdentityClient)` takes the gem-absent branch.
//! Core's native `require "identity_client"` raised part way through the
//! gem (its `require "active_support/core_ext"` is not ported) after
//! defining `module IdentityClient`, so the guard answered "constant"
//! and the next line raised gem_requires_rails in 22 tests' setup.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    identity_client (59.3.0)\n\nDEPENDENCIES\n  activesupport\n  identity_client\n";

/// A gem whose load closure reaches Rails (`rails_dependent`).
const GEMS: &str = "rails_dependent_by_name: true\nrails_dependent:\n  - {gem: identity_client, why: \"requiring it loads 199 Rails framework files\"}\n";

const RBI: &str = "# typed: true\nmodule IdentityClient\n  class << self\n    def cache; end\n    def cache=(value); end\n  end\nend\n";

const HELPER: &str = "module CacheSwap\n  def self.swap(cache)\n    return :without_gem unless defined?(IdentityClient)\n\n    IdentityClient.cache = cache\n    :swapped\n  end\n\n  def self.comparable\n    defined?(Comparable)\n  end\nend\n";

#[test]
fn a_refused_gem_constant_is_not_defined() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "extensions:\n  - ext\n")
        .write("ext/gems.yml", GEMS)
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/identity_client@59.3.0.rbi", RBI)
        .write("lib/cache_swap.rb", HELPER)
        // The half-loaded gem's module, as the native require leaves it.
        .run_ruby("module IdentityClient; end\np CacheSwap.swap(:memory)\np CacheSwap.comparable\n");
    assert!(run.errors.iter().any(|e| e.contains("gem_requires_rails") && e.contains("IdentityClient is declared by the identity_client gem")), "{:#?}", run.errors);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, ":without_gem\n\"constant\"\n");
}
