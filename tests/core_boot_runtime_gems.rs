//! dalli, google-protobuf and proto, as Shopify Core's Essentials boot
//! reaches them: `Config#initialize` registers Dalli middleware and
//! `Podding::Discovery::Island` requires a generated proto message.
//!
//! The CRuby tree declares all three (`project.rs::RUNTIME_GEMS`) and
//! loads dalli and google-protobuf, which Core's Gemfile Bundler.requires.
//! proto is `require: false` in Core: the app's own require loads it, so
//! the façade does not. Each gem's RBI types its constants, and every
//! other tree refuses them as the gem's.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    dalli (3.2.9)\n    google-protobuf (3.25.8)\n    proto (2026.10.02.63343)\n      google-protobuf (>= 3.25, < 5.0)\n\nDEPENDENCIES\n  activesupport\n  dalli\n  google-protobuf\n  proto\n";

/// The parts of Core's Tapioca RBIs this app reaches.
const DALLI_RBI: &str = r#"# typed: true
module Dalli
  class << self
    def register(middleware); end
  end
end
module Dalli::OpentelemetryMiddleware; end
"#;

const PROTOBUF_RBI: &str = r#"# typed: true
module Google::Protobuf; end
class Google::Protobuf::DescriptorPool
  def self.generated_pool; end
end
"#;

const PROTO_RBI: &str = r#"# typed: true
class Proto::Shopify::Common::IslandName
  class << self
    sig { params(value: ::Symbol).returns(::Integer) }
    def resolve(value); end
  end
end
"#;

fn boot_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/dalli@3.2.9.rbi", DALLI_RBI)
        .write("sorbet/rbi/gems/google-protobuf@3.25.8.rbi", PROTOBUF_RBI)
        .write("sorbet/rbi/gems/proto@2026.10.02.63343.rbi", PROTO_RBI)
        .write("lib/cache_config.rb", r#"class CacheConfig
  def initialize
    Dalli.register(Dalli::OpentelemetryMiddleware)
  end

  def pool
    Google::Protobuf::DescriptorPool.generated_pool
  end

  def default_island
    Proto::Shopify::Common::IslandName.resolve(:island_name_unspecified)
  end
end
"#)
}

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_boot_gems() {
    let (dir, errors) = boot_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in ["dalli", "google-protobuf", "proto"] {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("\"dalli\""), "{facades}");
    assert!(facades.contains("\"google/protobuf\""), "{facades}");
    assert!(!facades.contains("\"proto\""), "proto is require: false in Core: {facades}");
}

#[test]
fn the_boot_gems_are_refused_as_the_gems_where_no_tree_loads_them() {
    let (_, errors) = boot_app().emit(BuildTarget::Rust);
    for gem in ["dalli", "google-protobuf", "proto"] {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))),
            "{gem}: {errors:#?}"
        );
    }
}

/// Core's dalli is the Shopify fork, a `GIT` source in Gemfile.lock.
const GIT_LOCK: &str = "GIT\n  remote: https://github.com/Shopify/dalli.git\n  revision: 262698ca57e937a3cb4420d169db2adce428adcc\n  ref: 262698ca57e937a3cb4420d169db2adce428adcc\n  specs:\n    dalli (3.2.9)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    google-protobuf (3.25.8)\n    proto (2026.10.02.63343)\n      google-protobuf (>= 3.25, < 5.0)\n\nDEPENDENCIES\n  activesupport\n  dalli!\n  google-protobuf\n  proto\n";

#[test]
fn a_git_sourced_gem_is_declared_with_its_source_and_activated_from_its_bundler_checkout() {
    let (dir, errors) = boot_app().write("Gemfile.lock", GIT_LOCK).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(
        gemfile.contains(
            "gem \"dalli\", git: \"https://github.com/Shopify/dalli.git\", ref: \"262698ca57e937a3cb4420d169db2adce428adcc\""
        ),
        "{gemfile}"
    );
    assert!(gemfile.contains("gem \"google-protobuf\"\n"), "a GEM spec stays bare: {gemfile}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.starts_with("require_relative \"git_gems\"\n"), "{facades}");
    let git_gems = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems");
    assert!(git_gems.contains("\"dalli-262698ca57e9\""), "{git_gems}");
    assert!(!git_gems.contains("google-protobuf"), "only git specs: {git_gems}");

    // The loader against a gem dir laid out as Bundler installs a git gem.
    let gems = dir.join("tmp-gem-home");
    let checkout = gems.join("bundler/gems/dalli-262698ca57e9");
    std::fs::create_dir_all(checkout.join("lib")).unwrap();
    std::fs::write(
        checkout.join("dalli.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"dalli\"\n  s.version = \"3.2.9\"\n  s.summary = \"fork\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/dalli.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    std::fs::write(checkout.join("lib/dalli.rb"), "module Dalli; FROM = __FILE__; end\n").unwrap();
    let run = |gem_path: &std::path::Path| {
        std::process::Command::new("ruby")
            .current_dir(&dir)
            .env("GEM_PATH", gem_path)
            .env("GEM_HOME", gem_path)
            .args(["-e", "require_relative 'runtime/git_gems'; require 'dalli'; puts Dalli::FROM; puts Gem.loaded_specs['dalli'].version"])
            .output()
            .expect("ruby")
    };
    let out = run(&gems);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("bundler/gems/dalli-262698ca57e9/lib/dalli.rb"), "{stdout}");
    assert!(stdout.contains("3.2.9"), "{stdout}");

    // No checkout: a warning naming the gem and revision, not a quiet miss.
    let empty = dir.join("tmp-empty-gem-home");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run(&empty);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("roundhouse: git gem dalli (https://github.com/Shopify/dalli.git at 262698ca57e937a3cb4420d169db2adce428adcc) has no checkout bundler/gems/dalli-262698ca57e9"),
        "{stderr}"
    );
}
