//! `Rails.cache = store` — railties' `Rails.cache=` (railties
//! lib/rails.rb, `attr_writer :cache` on the Rails singleton), which
//! the shared runtime answers (runtime/ruby/rails.rb). Core's
//! TestMacros::MemcachedHelper swaps the store and reattaches the
//! Rack-stack LocalCache middleware to it:
//!
//!     middleware = Rails.cache.middleware
//!     Rails.cache = @_original_rails_cache
//!     middleware.cache = Rails.cache
//!     Rails.cache.middleware = middleware
//!
//! The write is a plain send whatever the value's type, not a
//! `Rails not supported` refusal.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SWAP: &str = r#"class CacheSwap
  def self.restore(original)
    middleware = Rails.cache.middleware
    Rails.cache = original
    middleware.cache = Rails.cache
    Rails.cache.middleware = middleware
  end

  def swap
    @original ||= Rails.cache
    Rails.cache = @original
    Rails.cache
  end
end
"#;

#[test]
fn rails_cache_assignment_is_a_plain_send() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/cache_swap.rb", SWAP)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/cache_swap.rb")).expect("emitted model");
    assert!(!text.contains("not supported"), "{text}");
    assert_eq!(text.matches("Rails.cache = ").count(), 2, "{text}");
    assert!(!errors.iter().any(|e| e.contains("Rails not supported")), "{errors:?}");
}
