//! A value constant a class body declares binds its name lexically, just
//! as a nested class does. Core's `GuideSurface < T::Enum` declares
//! `Home = new("home")` inside `module Home; module Onboarding`, so a
//! written `Home::Onboarding::GuideSurface::ShopifyEmail` in that class
//! (in a method, or a `class << self` hash) starts at the enum value
//! `Home`, not the namespace. The emitted reference must be rooted
//! (`::Home::...`) so it resolves as native Ruby does.
//!
//! Native Ruby is the oracle: the same source, required directly, must
//! print what the emitted tree prints. The test also pins a path that
//! ends in a value constant, which must keep resolving to that value.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SURFACE: &str = r#"module Home
  module Onboarding
    class GuideSurface
      attr_reader :serialized

      def initialize(serialized)
        @serialized = serialized
      end
      private :initialize

      def self.values
        [Home, ShopifyEmail]
      end

      def supports_sorting?
        self == Home
      end

      Home = new("home")
      ShopifyEmail = new("shopify-email")

      class << self
        ALIASES = { ShopifyEmail => ["shopify-messaging"] }.freeze

        def aliased(value)
          ALIASES.find { |_, aliases| aliases.include?(value) }&.first
        end
      end
    end
  end
end
"#;

/// Outside the enum, `Home` is the namespace again: a path that ends in a
/// value constant must still read that value, not be rooted away or
/// misresolved.
const READER: &str = r#"module Home
  module Onboarding
    class SurfaceReader
      def self.default
        Home::Onboarding::GuideSurface::Home
      end
    end
  end
end
"#;

const READER_PATH: &str = "app/lib/home/onboarding/surface_reader.rb";

const REPORT: &str = r#"surface = Home::Onboarding::GuideSurface
puts surface.aliased("shopify-messaging").serialized
puts surface.aliased("nope").inspect
puts surface::Home.supports_sorting?
puts surface::ShopifyEmail.supports_sorting?
puts surface.singleton_class::ALIASES.keys.map(&:serialized).join(",")
puts Home::Onboarding::SurfaceReader.default.serialized
"#;

const EXPECTED: &str = "shopify-email\nnil\ntrue\nfalse\nshopify-email\nhome\n";

const PATH: &str = "app/lib/home/onboarding/guide_surface.rb";

#[test]
fn native_ruby_resolves_enum_value_shadowing() {
    let app = std::env::temp_dir().join(format!("roundhouse-enum-shadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&app);
    let mut requires = String::new();
    for (path, text) in [(PATH, SURFACE), (READER_PATH, READER)] {
        let full = app.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
        requires.push_str(&format!("require {:?}\n", full.display().to_string()));
    }
    let output = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{requires}{REPORT}"))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&app);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_ruby_roots_references_an_enum_value_shadows() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write(PATH, SURFACE)
        .write(READER_PATH, READER)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}
