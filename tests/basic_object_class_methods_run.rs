//! A class under `BasicObject` finds top-level constants from its class
//! methods, as the source's `class << self` body does.
//!
//! Core's `CheckoutShopUrl < BasicObject` reads `Shopify::Config` from a
//! `class << self` method. Emitted as `def self.protocol` in the class body,
//! the lookup walks CheckoutShopUrl's own ancestors, which hold no `Object`:
//! `uninitialized constant CheckoutShopUrl::Shopify`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"module Skywatch
  module Config
    def self.ssl_available
      true
    end
  end
end

class SkywatchUrl < BasicObject
  private_class_method(:new)

  class << self
    def build(path)
      [protocol, path].join
    end

    private

    def protocol
      Skywatch::Config.ssl_available ? "https://" : "http://"
    end
  end
end
"#;

const SCRIPT: &str = r#"
raise "built #{SkywatchUrl.build("/x").inspect}" unless SkywatchUrl.build("/x") == "https:///x"
puts "basic object class methods passed"
"#;

#[test]
fn a_basic_object_subclass_reads_top_level_constants_from_its_class_methods() {
    emit_and_run::real_blog()
        .write("app/services/skywatch_url.rb", SOURCE)
        .run_ruby(SCRIPT)
        .assert_passes();
}
