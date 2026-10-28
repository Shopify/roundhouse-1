//! runtime/active_support_mem_cache_store.rb requires dalli and
//! connection_pool as it loads, as activesupport's mem_cache_store.rb
//! does. A tree that carries the port (app code names
//! `ActiveSupport::Cache::MemCacheStore`) declares both gems in its
//! Gemfile, with the lock's source and revision for a Git-sourced one.
//! Before, nothing declared dalli: the guarded require in
//! runtime/gem_facades.rb skipped it quietly, and boot stopped at the
//! port's own `require "dalli"` with a LoadError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GIT\n  remote: https://github.com/example/dalli.git\n  revision: 0123456789abcdef0123456789abcdef01234567\n  specs:\n    dalli (3.2.9)\n      logger\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    connection_pool (2.5.3)\n    logger (1.7.0)\n\nDEPENDENCIES\n  connection_pool\n  dalli!\n";

const STORE: &str = "class VersionedStore < ActiveSupport::Cache::MemCacheStore\n  def namespaced(key)\n    \"v1/#{key}\"\n  end\nend\n";

#[test]
fn a_tree_carrying_the_mem_cache_store_port_declares_dalli_and_connection_pool() {
    let (dir, _) = emit_and_run::empty_app()
        .write("Gemfile.lock", LOCK)
        .write("app/models/versioned_store.rb", STORE)
        .emit(BuildTarget::Ruby);
    assert!(dir.join("runtime/active_support_mem_cache_store.rb").exists(), "the tree carries the port");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(
        gemfile.lines().any(|line| line.trim()
            == "gem \"dalli\", git: \"https://github.com/example/dalli.git\", ref: \"0123456789abcdef0123456789abcdef01234567\""),
        "{gemfile}"
    );
    assert!(gemfile.lines().any(|line| line.trim() == "gem \"connection_pool\""), "{gemfile}");
}

#[test]
fn a_tree_without_the_port_declares_neither_gem() {
    let (dir, _) = emit_and_run::empty_app()
        .write("Gemfile.lock", LOCK)
        .write("app/models/plain_store.rb", "class PlainStore\n  def fetch(key)\n    key\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(!dir.join("runtime/active_support_mem_cache_store.rb").exists());
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(!gemfile.contains("gem \"dalli\""), "{gemfile}");
    assert!(!gemfile.contains("gem \"connection_pool\""), "{gemfile}");
}
