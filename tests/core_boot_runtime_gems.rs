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

/// Core's gem facts are the project extension `extensions/core` of the
/// core-transpile checkout (docs/PROJECT_EXTENSIONS.md), not the compiler's:
/// every app here lists it, so these tests run against the same rows the
/// harness emits with. `ROUNDHOUSE_CORE_EXTENSION` overrides the path.
fn core_extension() -> String {
    let dir = std::env::var("ROUNDHOUSE_CORE_EXTENSION").unwrap_or_else(|_| "/home/tobi/core-transpile/extensions/core".into());
    assert!(std::path::Path::new(&dir).join("gems.yml").exists(), "Core's gem facts are not at {dir}/gems.yml (ROUNDHOUSE_CORE_EXTENSION)");
    dir
}

/// The test app's roundhouse.yml: test roots, `extra` keys, Core's extension.
fn config(extra: &str) -> String {
    format!("test_paths:\n  - test\n{extra}extensions:\n  - {}\n", core_extension())
}

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
        .write("roundhouse.yml", &config(""))
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

/// Essentials' test config (`config/test.rb`): an in-process Redis and
/// Snappy-compressed bodies. Core's Gemfile Bundler.requires redis,
/// hiredis-client and snappy; redis-client comes with redis. (Its Panama
/// fake client is `RAILS_DEPENDENT_GEMS`, below.)
const TEST_CONFIG_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hiredis-client (0.28.0)\n      redis-client (= 0.28.0)\n    redis (5.4.0)\n      redis-client (>= 0.22.0)\n    redis-client (0.28.0)\n    snappy (0.5.1)\n\nDEPENDENCIES\n  activesupport\n  hiredis-client\n  redis\n  snappy\n";

fn test_config_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", TEST_CONFIG_LOCK)
        .write("sorbet/rbi/gems/redis@5.4.0.rbi", "# typed: true\nclass Redis\n  def initialize(options = {}); end\nend\nclass Redis::BaseError < ::StandardError; end\nclass Redis::CommandError < ::Redis::BaseError; end\n")
        .write("sorbet/rbi/gems/redis-client@0.28.0.rbi", "# typed: true\nclass RedisClient\n  class << self\n    def register(middleware); end\n  end\nend\n")
        .write("sorbet/rbi/gems/snappy@0.5.1.rbi", "# typed: true\nmodule Snappy\n  class << self\n    def deflate(source); end\n  end\nend\n")
        .write("lib/test_services.rb", r#"module RedisLogging
  def call(command, config)
    yield
  end
end

class TestServices
  def initialize
    RedisClient.register(RedisLogging)
  end

  def redis
    Redis.new(url: "redis://localhost:6379")
  rescue Redis::CommandError
    nil
  end

  def compress(body)
    Snappy.deflate(body)
  end
end
"#)
}

const TEST_CONFIG_GEMS: [&str; 3] = ["redis", "redis-client", "snappy"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_test_config_gems() {
    let (dir, errors) = test_config_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in TEST_CONFIG_GEMS.iter().chain(&["hiredis-client"]) {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["redis", "hiredis-client", "snappy"] {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
}

#[test]
fn the_test_config_gems_are_refused_as_the_gems_where_no_tree_loads_them() {
    let (_, errors) = test_config_app().emit(BuildTarget::Rust);
    for gem in TEST_CONFIG_GEMS {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))),
            "{gem}: {errors:#?}"
        );
    }
}

/// A needed GEM spec can depend on a GIT spec RubyGems cannot find on its
/// own (Core's panama_client on its grpc fork: `Gem::MissingSpecError:
/// Could not find 'grpc'`, swallowed by the guarded require). The git
/// closure of the needed gems is activated, dependencies first. Shown here
/// on redis, since panama_client is now a `RAILS_DEPENDENT_GEMS` refusal.
#[test]
fn a_git_dependency_of_a_needed_gem_is_activated_before_it() {
    let lock = TEST_CONFIG_LOCK.replace(
        "    redis (5.4.0)\n      redis-client (>= 0.22.0)\n",
        "    redis (5.4.0)\n      grpc (>= 1.40)\n      redis-client (>= 0.22.0)\n",
    );
    let lock = format!(
        "GIT\n  remote: https://github.com/Shopify/grpc.git\n  revision: d5dd2a32be6bfd2e9abd7d10ccfb87d129e868de\n  branch: shopify-fixes\n  submodules: true\n  specs:\n    grpc (1.85.0.dev)\n\n{lock}"
    );
    let (dir, errors) = test_config_app().write("Gemfile.lock", &lock).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let git_gems = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems");
    assert!(git_gems.contains("[\"grpc\", \"grpc-d5dd2a32be6b\", \"https://github.com/Shopify/grpc.git\""), "{git_gems}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    // The locked-external wiring pins the declaration to the locked version.
    assert!(gemfile.contains("gem \"redis\", \"= 5.4.0\"\n"), "{gemfile}");
    assert_eq!(gemfile.matches("gem \"redis\"").count(), 1, "{gemfile}");
}

/// Essentials' boot path through Rack, Faraday, Trilogy, Sabertooth,
/// OpenSRS, Magellan and OpenTelemetry's API, all Rails-free and (but
/// for the API, which Core's instrumentations require) Bundler.required
/// by Core. Core reopens `class Trilogy` (patches/trilogy_error.rb), so
/// `Trilogy::ProtocolError` is typed only from the gem's RBI. Prism is
/// demand only: Core's own `require "prism"` loads it.
const PLATFORM_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    faraday (2.14.3)\n    magellan-ruby (0.2.16)\n    opentelemetry-api (1.10.1)\n    opentelemetry-shopify (1.22.0)\n      opentelemetry-api (~> 1.0)\n    prism (1.9.0)\n    rack (2.2.24)\n    sabertooth (0.1.6)\n    shopify-opensrs (0.5.0)\n    trilogy (2.9.0)\n\nDEPENDENCIES\n  activesupport\n  faraday (>= 2)\n  magellan-ruby (= 0.2.16)\n  opentelemetry-shopify (~> 1.22)\n  rack (= 2.2.24)\n  sabertooth (~> 0.1)\n  shopify-opensrs (~> 0.5)\n  trilogy (= 2.9.0)\n";

fn platform_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", PLATFORM_LOCK)
        .write("sorbet/rbi/gems/rack@2.2.24.rbi", &rbi("module Rack; end\nclass Rack::Request\n  def initialize(env); end\n  def path; end\nend\n"))
        .write("sorbet/rbi/gems/faraday@2.14.3.rbi", &rbi("module Faraday; end\nclass Faraday::Error < ::StandardError; end\nclass Faraday::TimeoutError < ::Faraday::Error; end\n"))
        .write("sorbet/rbi/gems/trilogy@2.9.0.rbi", &rbi("class Trilogy\n  def initialize(options = {}); end\nend\nclass Trilogy::ProtocolError < ::StandardError; end\n"))
        .write("sorbet/rbi/gems/sabertooth@0.1.6.rbi", &rbi("module Sabertooth\n  class << self\n    def script(name); end\n  end\nend\n"))
        .write("sorbet/rbi/gems/shopify-opensrs@0.5.0.rbi", &rbi("module OpenSRS; end\nclass OpenSRS::Server\n  def initialize(options = {}); end\nend\n"))
        .write("sorbet/rbi/gems/magellan-ruby@0.2.16.rbi", &rbi("module Magellan; end\nclass Magellan::ServiceNotPolledYetError < ::StandardError; end\n"))
        .write("sorbet/rbi/gems/opentelemetry-api@1.10.1.rbi", &rbi("module OpenTelemetry; end\nclass OpenTelemetry::Context\n  class << self\n    def current; end\n  end\nend\n"))
        .write("sorbet/rbi/gems/prism@1.9.0.rbi", &rbi("module Prism\n  class << self\n    def parse(source); end\n  end\nend\n"))
        .write("lib/trilogy_error.rb", r#"class Trilogy
  module ConnectionError; end
end
"#)
        .write("lib/platform_services.rb", r#"class PlatformServices
  def request_path(env)
    Rack::Request.new(env).path
  end

  def timeout?(error)
    error.is_a?(Faraday::TimeoutError)
  end

  def connect
    Trilogy.new(host: "localhost")
  rescue Trilogy::ProtocolError
    nil
  end

  def script
    Sabertooth.script("release")
  end

  def domains
    OpenSRS::Server.new(server: "https://opensrs.example")
  end

  def polled?
    true
  rescue Magellan::ServiceNotPolledYetError
    false
  end

  def context
    OpenTelemetry::Context.current
  end

  def parse(source)
    require "prism"
    Prism.parse(source)
  end
end
"#)
}

const PLATFORM_GEMS: [&str; 8] =
    ["rack", "faraday", "trilogy", "sabertooth", "shopify-opensrs", "magellan-ruby", "opentelemetry-api", "prism"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_platform_gems() {
    let (dir, errors) = platform_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in PLATFORM_GEMS {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["rack", "faraday", "trilogy", "sabertooth", "shopify-opensrs", "magellan-ruby"] {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
    assert!(!facades.contains("\"prism\""), "prism is required by the app, not Bundler: {facades}");
}

/// A tree whose lock pins the OpenTelemetry API loads the real gem, so its
/// `Trace.current_span(context = nil)` is not shadowed by the non-recording
/// stand-in; the stand-in stays only as the LoadError fallback.
#[test]
fn the_emitted_ruby_tree_prefers_the_real_telemetry_api_over_the_stand_in() {
    let (dir, errors) = platform_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    let guarded = "begin\n  require \"opentelemetry\"\nrescue LoadError\n  require_relative \"open_telemetry_facade\"\nend\n";
    assert!(facades.contains(guarded), "{facades}");
    assert_eq!(facades.matches("require_relative \"open_telemetry_facade\"").count(), 1, "{facades}");
}

#[test]
fn a_ruby_tree_without_the_telemetry_api_in_its_lock_requires_the_stand_in_directly() {
    let (dir, errors) = emit_and_run::empty_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("require_relative \"open_telemetry_facade\"\n"), "{facades}");
    assert!(!facades.contains("require \"opentelemetry\"\nrescue"), "{facades}");
}

#[test]
fn the_platform_gems_are_refused_as_the_gems_where_no_tree_loads_them() {
    let (_, errors) = platform_app().emit(BuildTarget::Rust);
    for gem in ["rack", "faraday", "sabertooth", "shopify-opensrs", "magellan-ruby", "opentelemetry-api", "prism"] {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))),
            "{gem}: {errors:#?}"
        );
    }
}

/// Constants Essentials' middlewares, query capture and customer account
/// read at load time, from gems Core Bundler.requires and that load
/// without Rails. ShopifyBots' middleware is one of its listed requires.
const MIDDLEWARE_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    customer_privacy_api_ruby (42.1.0)\n    lru_redux (1.1.0)\n    secure_headers (5.2.0)\n    shopify_bots (0.59.0)\n    stack_frames (0.1.3)\n\nDEPENDENCIES\n  activesupport\n  customer_privacy_api_ruby (= 42.1.0)\n  lru_redux (~> 1.1)\n  secure_headers (~> 5.2)\n  shopify_bots (= 0.59.0)\n  stack_frames\n";

fn middleware_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", MIDDLEWARE_LOCK)
        .write("sorbet/rbi/gems/stack_frames@0.1.3.rbi", &rbi("module StackFrames; end\nclass StackFrames::Buffer\n  def initialize(size); end\nend\n"))
        .write("sorbet/rbi/gems/lru_redux@1.1.0.rbi", &rbi("module LruRedux; end\nclass LruRedux::Cache\n  def initialize(max_size); end\nend\nclass LruRedux::ThreadSafeCache < ::LruRedux::Cache; end\n"))
        .write("sorbet/rbi/gems/shopify_bots@0.59.0.rbi", &rbi("module ShopifyBots; end\nmodule ShopifyBots::Rack; end\nclass ShopifyBots::Rack::Middleware; end\nShopifyBots::Rack::Middleware::REQUEST_PRIORITY = T.let(T.unsafe(nil), String)\n"))
        .write("sorbet/rbi/gems/customer_privacy_api_ruby@42.1.0.rbi", &rbi("module CustomerPrivacyApiRuby; end\nmodule CustomerPrivacyApiRuby::Storage; end\nCustomerPrivacyApiRuby::Storage::VISIT_COOKIE_EXPIRATION_TIME_SECONDS = T.let(T.unsafe(nil), Integer)\n"))
        .write("lib/middleware_settings.rb", r#"class MiddlewareSettings
  FRAMES = StackFrames::Buffer.new(32)
  CACHE = LruRedux::ThreadSafeCache.new(100)
  PRIORITY = ShopifyBots::Rack::Middleware::REQUEST_PRIORITY
  VISIT_TTL = CustomerPrivacyApiRuby::Storage::VISIT_COOKIE_EXPIRATION_TIME_SECONDS

  def headers
    { "priority" => PRIORITY, "ttl" => VISIT_TTL.to_s }
  end
end
"#)
}

const MIDDLEWARE_GEMS: [&str; 4] = ["stack_frames", "lru_redux", "shopify_bots", "customer_privacy_api_ruby"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_middleware_gems() {
    let (dir, errors) = middleware_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in MIDDLEWARE_GEMS {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in MIDDLEWARE_GEMS.iter().copied().chain(["shopify_bots/rack/middleware"]) {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
}

#[test]
fn the_middleware_gems_are_refused_as_the_gems_where_no_tree_loads_them() {
    let (_, errors) = middleware_app().emit(BuildTarget::Rust);
    for gem in MIDDLEWARE_GEMS {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))),
            "{gem}: {errors:#?}"
        );
    }
}

/// Gems in Core's Bundler.require list whose load closure reaches Rails
/// once `Rails`, `ActiveSupport` or `ActiveRecord` is defined, as the
/// emitted runtime defines them: never required natively, their
/// constants refused by name until their source is compiled as app code.
const RAILS_DEPENDENT_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    app_profiler (0.6.0)\n    cusco (3.4.0)\n    elasticsearch-shopify (6.0.1)\n    opentelemetry-shopify (1.22.0)\n    panama_client (3.2.3)\n      panama_metadata (>= 0.3.2, < 1.0.0)\n    panama_metadata (0.3.19.1687)\n    secure_headers (5.2.0)\n    shopify-money (4.1.1)\n\nDEPENDENCIES\n  activesupport\n  app_profiler\n  cusco\n  elasticsearch-shopify (= 6.0.1)\n  opentelemetry-shopify (~> 1.22)\n  panama_client\n  panama_metadata\n  secure_headers (~> 5.2)\n  shopify-money (~> 4.1.1)\n";

fn rails_dependent_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("{}  - ext\n", config("")))
        // The tree is still written, each read raising by name at run time:
        // the project's own opt-in, kept in the fixture so this test does not
        // depend on whether Core's live gems.yml carries the key.
        .write("ext/gems.yml", "rails_dependent_by_name: true\n")
        .write("Gemfile.lock", RAILS_DEPENDENT_LOCK)
        .write("sorbet/rbi/gems/secure_headers@5.2.0.rbi", &rbi("module SecureHeaders; end\nclass SecureHeaders::StrictTransportSecurity; end\nSecureHeaders::StrictTransportSecurity::HEADER_NAME = T.let(T.unsafe(nil), String)\n"))
        .write("sorbet/rbi/gems/shopify-money@4.1.1.rbi", &rbi("class Money\n  def initialize(value, currency); end\nend\n"))
        .write("sorbet/rbi/gems/elasticsearch-shopify@6.0.1.rbi", &rbi("module ElasticSearch; end\nmodule ElasticSearch::Shopify; end\nclass ElasticSearch::Shopify::Client; end\n"))
        .write("sorbet/rbi/gems/opentelemetry-shopify@1.22.0.rbi", &rbi("module OpenTelemetry; end\nmodule OpenTelemetry::Shopify\n  class << self\n    def configure(**options); end\n  end\nend\n"))
        .write("sorbet/rbi/gems/panama_client@3.2.3.rbi", &rbi("module PanamaClient; end\nclass PanamaClient::NotFoundError < ::StandardError; end\n"))
        .write("sorbet/rbi/gems/panama_metadata@0.3.19.1687.rbi", &rbi("module PanamaMetadata; end\nclass PanamaMetadata::Registry; end\n"))
        .write("sorbet/rbi/gems/app_profiler@0.6.0.rbi", &rbi("module AppProfiler; end\nclass AppProfiler::Middleware; end\n"))
        .write("sorbet/rbi/gems/cusco@3.4.0.rbi", &rbi("module Cusco; end\nclass Cusco::Client; end\n"))
        .write("lib/rails_dependent.rb", r#"class RailsDependent
  def cusco?(value)
    value.is_a?(Cusco::Client)
  end

  def hsts
    SecureHeaders::StrictTransportSecurity::HEADER_NAME
  end

  def zero
    Money.new(0, "USD")
  end

  def client?(value)
    value.is_a?(ElasticSearch::Shopify::Client)
  end

  def tracing
    OpenTelemetry::Shopify.configure
  end

  def missing
    raise PanamaClient::NotFoundError, "missing"
  end

  def registry?(value)
    value.is_a?(PanamaMetadata::Registry)
  end

  def profiler
    AppProfiler::Middleware
  end
end
"#)
}

const RAILS_DEPENDENT: [(&str, &str); 8] = [
    ("app_profiler", "app_profiler"),
    // From the probe: requiring it loads 59 Rails framework files.
    ("cusco", "cusco"),
    ("secure_headers", "secure_headers"),
    ("shopify-money", "shopify-money"),
    ("elasticsearch-shopify", "elastic_search/shopify"),
    ("opentelemetry-shopify", "opentelemetry-shopify"),
    ("panama_client", "panama_client"),
    ("panama_metadata", "panama_metadata"),
];

#[test]
fn a_rails_dependent_gem_is_refused_by_name_and_never_required() {
    let (dir, errors) = rails_dependent_app().emit(BuildTarget::Ruby);
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for (gem, feature) in RAILS_DEPENDENT {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem, whose load closure requires Rails"))),
            "{gem}: {errors:#?}"
        );
        assert!(!gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
        assert!(!facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
}

/// Core's Money is shopify-money's. Another app's own `Money` is named
/// the same and declares no gem.
const SEMIAN_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    semian (0.28.3)\n\nDEPENDENCIES\n  activesupport\n  semian (~> 0.27)\n";

/// Core's semian, as essentials and billing name it. The PostgreSQL
/// CRuby tree carries the runtime port (runtime/spinel/semian*.rb), so
/// the names it declares run as the port; the gem's net_http adapter is
/// not ported.
fn semian_app(postgresql: bool) -> emit_and_run::Overlay {
    let config = config(if postgresql { "database_adapter: postgresql\n" } else { "" });
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", &config)
        .write("Gemfile.lock", SEMIAN_LOCK)
        .write("sorbet/rbi/gems/semian@0.28.3.rbi", "# typed: true\nmodule Semian; end\nclass Semian::BaseError < ::StandardError; end\nclass Semian::TimeoutError < ::Semian::BaseError; end\nclass Semian::OpenCircuitError < ::Semian::BaseError; end\nmodule Semian::AdapterError; end\nmodule Semian::NetHTTP; end\nmodule Semian::Redis; end\n")
        .write("lib/resilience.rb", r#"class Resilience
  def timeout?(error)
    error.is_a?(Semian::TimeoutError)
  end

  def open_circuit
    Semian::OpenCircuitError
  end

  def adapter_error?(error)
    error.is_a?(Semian::AdapterError)
  end
end
"#);
    if postgresql {
        app = app.write("config/database.yml", "test:\n  adapter: postgresql\n  database: app_test\n");
    }
    app
}

/// ingest's standing `database_adapter` refusal for a PostgreSQL app
/// (`DatabaseAdapter::runtime_refusal`), as in tests/database_yml_boot.rs.
fn is_adapter_refusal(error: &String) -> bool {
    error.contains("database_adapter not supported")
}

#[test]
fn a_ported_gem_s_constants_run_as_its_runtime_port() {
    let (dir, errors) = semian_app(true).emit(BuildTarget::Ruby);
    assert!(errors.iter().all(is_adapter_refusal), "{errors:#?}");
    let port = std::fs::read_to_string(dir.join("runtime/semian.rb")).expect("semian port");
    assert!(port.contains("TimeoutError = Class.new(BaseError)"), "{port}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(!facades.contains("\"semian\""), "the gem's own semian.rb is never required: {facades}");
    let lib = std::fs::read_to_string(dir.join("app/models/resilience.rb")).expect("resilience");
    let run = emit_and_run::ruby()
        .args(["-e", "require \"./runtime/semian\"\nrequire \"./app/models/resilience\"\nr = Resilience.new\np [r.timeout?(Semian::TimeoutError.new), r.open_circuit.equal?(Semian::OpenCircuitError), r.adapter_error?(RuntimeError.new), Semian::OpenCircuitError.superclass]\np $LOADED_FEATURES.grep(%r{/gems/semian}).empty?"])
        .current_dir(&dir)
        // Names only: the bulkhead's extension is the semian gem's, which
        // this machine's ruby need not have (tests/database_yml_boot.rs
        // runs the bulkhead).
        .env("SEMIAN_SEMAPHORES_DISABLED", "1")
        .output()
        .expect("spawn ruby");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(stdout, "[true, true, false, Semian::BaseError]\ntrue\n", "{}\n{lib}", String::from_utf8_lossy(&run.stderr));
}

/// A name only the gem declares has no port to run it; a tree without
/// the port (no PostgreSQL bootstrap) and a non-CRuby tree refuse even
/// the ported names.
#[test]
fn a_ported_gem_s_unported_names_and_portless_trees_are_refused() {
    let gap = semian_app(true).write("lib/redis_adapter.rb", "class RedisAdapter\n  def adapter\n    Semian::Redis\n  end\nend\n");
    let (_, errors) = gap.emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Semian::Redis is declared by the semian gem, which runs as its runtime port (") && e.contains("), and the port does not declare it")),
        "{errors:#?}"
    );
    assert_eq!(errors.iter().filter(|e| !is_adapter_refusal(*e)).count(), 1, "{errors:#?}");

    let (_, errors) = semian_app(false).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Semian::TimeoutError is declared by the semian gem's runtime port runtime/semian.rb, which only the PostgreSQL CRuby tree carries")),
        "{errors:#?}"
    );
    let (_, errors) = semian_app(false).emit(BuildTarget::Rust);
    assert!(
        errors.iter().any(|e| e.contains("Semian::TimeoutError is declared by the semian gem, which the rust tree does not load")),
        "{errors:#?}"
    );
}

const GLOBALID_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    globalid (1.4.0)\n      activesupport (>= 6.1)\n\nDEPENDENCIES\n  activesupport\n  globalid\n";

/// Core's essentials, as t_enum_global_id.rb and observe.rb name
/// globalid. Every CRuby tree carries the overlay's port
/// (runtime/global_id.rb); SignedGlobalID is not ported.
fn globalid_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", GLOBALID_LOCK)
        .write("sorbet/rbi/gems/globalid@1.4.0.rbi", "# typed: true\nclass GlobalID\n  def initialize(gid, options = T.unsafe(nil)); end\n  def model_id; end\nend\nmodule GlobalID::Identification; end\nclass SignedGlobalID < ::GlobalID; end\n")
        .write("lib/identified.rb", r#"class Identified
  def identification
    GlobalID::Identification
  end

  def model_id(gid)
    GlobalID.new(gid).model_id
  end
end
"#)
}

#[test]
fn an_overlay_ported_gem_s_constants_run_as_its_port() {
    let (dir, errors) = globalid_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let run = emit_and_run::ruby()
        // In boot.rb's order: the port uses the core_ext's `blank?`.
        .args(["-e", "require \"./runtime/active_support_core_ext\"\nrequire \"./runtime/global_id\"\nrequire \"./app/models/identified\"\ni = Identified.new\np [i.identification.equal?(GlobalID::Identification), i.model_id(\"gid://core/Shop/7\")]\np $LOADED_FEATURES.grep(%r{/gems/globalid}).empty?"])
        .current_dir(&dir)
        .output()
        .expect("spawn ruby");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[true, \"7\"]\ntrue\n", "{}", String::from_utf8_lossy(&run.stderr));

    let gap = globalid_app().write("lib/signed.rb", "class Signed\n  def signed\n    SignedGlobalID\n  end\nend\n");
    let (_, errors) = gap.emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("SignedGlobalID is declared by the globalid gem, which runs as its runtime port (runtime/global_id.rb), and the port does not declare it")),
        "{errors:#?}"
    );
    assert_eq!(errors.len(), 1, "{errors:#?}");
}

const PRICE: &str = "class Price\n  ZERO = Money.new(0, \"USD\")\n\n  def zero\n    ZERO\n  end\nend\n";

const OWN_MONEY_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n\nDEPENDENCIES\n  activesupport\n";

#[test]
fn an_app_s_own_money_does_not_declare_shopify_money() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", OWN_MONEY_LOCK)
        .write("lib/money.rb", "class Money\n  def initialize(value, currency)\n    @value = value\n  end\nend\n")
        .write("lib/price.rb", PRICE)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(std::fs::read_to_string(dir.join("app/models/price.rb")).unwrap().contains("Money.new"));
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(!gemfile.contains("shopify-money"), "{gemfile}");
}

const SERVICES_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    graphql (2.6.11)\n    shopify-adt (1.2.0)\n\nDEPENDENCIES\n  activesupport\n  graphql (~> 2.6.11)\n  shopify-adt (~> 1.2.0)\n";

fn services_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", SERVICES_LOCK)
        .write("sorbet/rbi/gems/shopify-adt@1.2.0.rbi", &rbi("module Shopify; end\nmodule Shopify::Adt; end\nclass Shopify::Adt::Optional\n  def present?; end\nend\n"))
        .write("sorbet/rbi/gems/graphql@2.6.11.rbi", &rbi("module GraphQL; end\nmodule GraphQL::Language; end\nmodule GraphQL::Language::Nodes; end\nclass GraphQL::Language::Nodes::AbstractNode; end\nclass GraphQL::Language::Nodes::Field < ::GraphQL::Language::Nodes::AbstractNode\n  def name; end\nend\n"))
        .write("lib/service_reader.rb", r#"class ServiceReader
  def optional?(value)
    value.is_a?(Shopify::Adt::Optional)
  end

  def field?(node)
    node.is_a?(GraphQL::Language::Nodes::Field)
  end
end
"#)
}

const SERVICES_GEMS: [&str; 2] = ["shopify-adt", "graphql"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_service_gems() {
    let (dir, errors) = services_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in SERVICES_GEMS {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["shopify/adt", "graphql"] {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
}

#[test]
fn the_service_gems_are_refused_as_the_gems_where_no_tree_loads_them() {
    let (_, errors) = services_app().emit(BuildTarget::Rust);
    for gem in SERVICES_GEMS {
        assert!(
            errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))),
            "{gem}: {errors:#?}"
        );
    }
}

/// Core's trilogy: its C extension requires "bigdecimal" with its own
/// `rb_require`, which RubyGems cannot activate a gem for, so on plain
/// RubyGems it fails although bigdecimal is installed. The bundled gems
/// the lock pins are activated at the pinned version first, as
/// Bundler.setup puts them on the load path, and a guarded require that
/// fails part way names the missing feature instead of swallowing it.
#[test]
fn the_locked_bundled_gems_are_on_the_load_path_and_a_partial_gem_load_is_named() {
    let lock = LOCK.replace("    dalli (3.2.9)\n", "    csv (9.0.0)\n    dalli (3.2.9)\n");
    let (dir, errors) = boot_app().write("Gemfile.lock", &lock).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.starts_with("require_relative \"bundled_gems\"\n"), "{facades}");
    let pins = std::fs::read_to_string(dir.join("runtime/bundled_gems.rb")).expect("bundled_gems");
    assert!(pins.contains("[\"csv\", \"9.0.0\"]"), "{pins}");
    assert!(!pins.contains("dalli"), "only bundled gems: {pins}");

    // A gem home holding csv at the pinned and a newer version, and a
    // dalli whose entry requires a feature no gem provides.
    let home = dir.join("tmp-bundled-gem-home");
    for version in ["9.0.0", "9.1.0"] {
        let lib = home.join(format!("gems/csv-{version}/lib"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("csv.rb"), format!("CSV_FROM = {version:?}\n")).unwrap();
    }
    std::fs::create_dir_all(home.join("gems/dalli-3.2.9/lib")).unwrap();
    std::fs::write(home.join("gems/dalli-3.2.9/lib/dalli.rb"), "require \"roundhouse_no_such_feature\"\n").unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    for (name, version) in [("csv", "9.0.0"), ("csv", "9.1.0"), ("dalli", "3.2.9")] {
        std::fs::write(
            home.join(format!("specifications/{name}-{version}.gemspec")),
            format!("Gem::Specification.new do |s|\n  s.name = {name:?}\n  s.version = {version:?}\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n"),
        )
        .unwrap();
    }
    let out = std::process::Command::new("ruby")
        .current_dir(&dir)
        .env("GEM_PATH", &home)
        .env("GEM_HOME", &home)
        .args([
            "-e",
            "require_relative 'runtime/bundled_gems'; p Gem.loaded_specs['csv'].version.to_s; \
             p $LOAD_PATH.include?(File.join(Gem.loaded_specs['csv'].full_gem_path, 'lib')); p defined?(CSV_FROM); \
             require 'csv'; p CSV_FROM",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ["\"9.0.0\"", "true", "nil", "\"9.0.0\""], "{stdout}");

    let out = std::process::Command::new("ruby")
        .current_dir(&dir)
        .env("GEM_PATH", &home)
        .env("GEM_HOME", &home)
        .args(["-e", "require_relative 'runtime/gem_facades'"])
        .output()
        .expect("ruby");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("roundhouse: require \"dalli\" failed: cannot load such file -- roundhouse_no_such_feature"),
        "{stderr}"
    );
    assert!(!stderr.contains("require \"bcrypt\""), "an absent gem stays quiet: {stderr}");
}

/// Part of the Rails-free half of Core's Bundler.require list, in its
/// Gemfile order.
const CORE_REQUIRES: [&str; 28] = [
    "rack", "redis", "hiredis-client", "stack_frames", "snappy", "graphql", "liquid", "stripe", "faraday",
    "faraday/gzip", "opentelemetry-instrumentation-trilogy", "opentelemetry-instrumentation-pg", "lru_redux",
    "trilogy", "shopify_bots", "shopify_bots/agent_classification_identifier", "shopify_bots/rack/middleware",
    "shopify_bots/request_extension", "shopify_bots/session/object_agent_classification_record",
    "shopify_bots/session/agent_classification_updater", "sabertooth", "sabertooth/prelude", "shopify-opensrs",
    "customer_privacy_api_ruby", "shopify/adt", "magellan-ruby", "shopify_session", "colorize",
];

const BUNDLED_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    jwt (2.10.1)\n    shopify_session (7.40.0)\n      jwt (>= 2.0)\n    stripe (18.4.2)\n\nDEPENDENCIES\n  activesupport\n  shopify_session\n  stripe (~> 18.4.2)\n";

fn bundled_app(lock: &str) -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", lock)
        .write("sorbet/rbi/gems/stripe@18.4.2.rbi", &rbi("module Stripe; end\nclass Stripe::Customer\n  def id; end\nend\n"))
        .write("sorbet/rbi/gems/jwt@2.10.1.rbi", &rbi("module JWT; end\nclass JWT::DecodeError < StandardError; end\n"))
        .write("lib/billing_reader.rb", r#"class BillingReader
  def customer?(value)
    value.is_a?(Stripe::Customer)
  end

  def bad_token?(error)
    error.is_a?(JWT::DecodeError)
  end
end
"#)
}

/// A Gemfile gem Core's Bundler.require loads runs as the gem on the
/// CRuby tree, and so does every spec it depends on: jwt is not in the
/// list, but shopify_session requires it.
#[test]
fn a_bundler_required_gem_and_its_dependencies_run_as_the_gems() {
    let (dir, errors) = bundled_app(BUNDLED_LOCK).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["stripe", "shopify_session"] {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
    assert!(!facades.contains("\"jwt\""), "loaded by shopify_session, not required itself: {facades}");

    let (_, errors) = bundled_app(BUNDLED_LOCK).emit(BuildTarget::Rust);
    for gem in ["stripe", "jwt"] {
        assert!(errors.iter().any(|e| e.contains(&format!("declared by the {gem} gem"))), "{gem}: {errors:#?}");
    }
}

/// Core's sprockets is a fork from a GIT source, which plain RubyGems
/// cannot find: the Bundler.require entry activates its checkout first,
/// though no app constant demands it.
#[test]
fn a_git_sourced_bundler_required_gem_is_activated() {
    let lock = format!(
        "GIT\n  remote: https://github.com/Shopify/sprockets.git\n  revision: 0123456789abcdef0123456789abcdef01234567\n  specs:\n    sprockets (4.2.1)\n\n{}",
        BUNDLED_LOCK.replace("  stripe (~> 18.4.2)\n", "  sprockets!\n  stripe (~> 18.4.2)\n")
    );
    let (dir, errors) = bundled_app(&lock).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let git = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems");
    assert!(git.contains("[\"sprockets\", \"sprockets-0123456789ab\""), "{git}");
}

/// Without the gem that loads it, a dependency's constants have nothing
/// to run them, and are refused by its name.
#[test]
fn a_dependency_no_required_gem_loads_is_refused() {
    let lock = BUNDLED_LOCK.replace("    shopify_session (7.40.0)\n      jwt (>= 2.0)\n", "").replace("  shopify_session\n", "  jwt\n");
    let (_, errors) = bundled_app(&lock).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("JWT::DecodeError is declared by the jwt gem, which no emitted tree loads")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("stripe")), "{errors:#?}");
}

/// A lockfile dependency is not a load. Core's payment_service_client
/// depends on cusco and autoloads it, and cusco requires Rails, so
/// cusco's constants stay refused. So does a dependency no probe saw
/// loaded (google-apis-core, behind stripe here).
#[test]
fn a_lockfile_dependency_requiring_does_not_load_is_refused() {
    let lock = BUNDLED_LOCK
        .replace("    stripe (18.4.2)\n", "    cusco (3.4.0)\n    google-apis-core (0.18.0)\n    payment_service_client (1.91.0)\n      cusco (>= 3.0)\n    stripe (18.4.2)\n      google-apis-core (>= 0.1)\n")
        .replace("  stripe (~> 18.4.2)\n", "  payment_service_client\n  stripe (~> 18.4.2)\n");
    let (_, errors) = bundled_app(&lock)
        .write("sorbet/rbi/gems/cusco@3.4.0.rbi", "# typed: true\nmodule Cusco; end\nclass Cusco::Client; end\n")
        .write("sorbet/rbi/gems/google-apis-core@0.18.0.rbi", "# typed: true\nmodule Google::Apis; end\nclass Google::Apis::Error < StandardError; end\n")
        .write("lib/payments_reader.rb", "class PaymentsReader\n  def cusco?(value)\n    value.is_a?(Cusco::Client)\n  end\n\n  def api_error?(error)\n    error.is_a?(Google::Apis::Error)\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("Cusco::Client is declared by the cusco gem, whose load closure requires Rails")),
        "{errors:#?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("Google::Apis::Error is declared by the google-apis-core gem, which no emitted tree loads")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("stripe") || e.contains("jwt")), "{errors:#?}");
}

/// A blog-shaped app whose lock names none of Core's gems requires none
/// of them, whatever the machine has installed.
#[test]
fn an_app_whose_lock_omits_core_s_gems_requires_none_of_them() {
    for lock in [Some(OWN_MONEY_LOCK), None] {
        let mut app = emit_and_run::empty_app().write("roundhouse.yml", &config(""));
        if let Some(lock) = lock {
            app = app.write("Gemfile.lock", lock);
        }
        let (dir, errors) = app.emit(BuildTarget::Ruby);
        assert!(errors.is_empty(), "{errors:#?}");
        let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
        assert!(facades.contains("\"bcrypt\""), "{facades}");
        for feature in CORE_REQUIRES {
            assert!(!facades.contains(&format!("\"{feature}\"")), "{feature} ({lock:?}): {facades}");
        }
    }
}

/// Core's lock gets the Rails-free list, in Gemfile order, though the
/// lock lists its specs alphabetically, and none of the Rails-dependent
/// gems it also resolves.
#[test]
fn core_s_lock_requires_its_bundler_require_list_in_gemfile_order() {
    let mut gems: Vec<&str> = CORE_REQUIRES
        .iter()
        .map(|f| match *f {
            "faraday/gzip" => "faraday-gzip",
            "shopify/adt" => "shopify-adt",
            f if f.starts_with("shopify_bots") => "shopify_bots",
            f if f.starts_with("sabertooth") => "sabertooth",
            f => f,
        })
        .chain(RAILS_DEPENDENT.iter().map(|(gem, _)| *gem))
        .collect();
    gems.sort();
    gems.dedup();
    let specs: String = gems.iter().map(|g| format!("    {g} (1.0.0)\n")).collect();
    let deps: String = gems.iter().map(|g| format!("  {g}\n")).collect();
    let lock = format!("GEM\n  remote: https://rubygems.org/\n  specs:\n{specs}\nDEPENDENCIES\n{deps}");
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", &config(""))
        .write("Gemfile.lock", &lock)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    let at: Vec<usize> = CORE_REQUIRES
        .iter()
        .map(|f| facades.find(&format!("\"{f}\"")).unwrap_or_else(|| panic!("{f}: {facades}")))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "out of Gemfile order: {facades}");
    for (_, feature) in RAILS_DEPENDENT {
        assert!(!facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
}
