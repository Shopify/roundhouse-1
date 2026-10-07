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

/// Essentials' test config (`config/test.rb`): an in-process Redis, the
/// Panama fake client over the metadata registry, and Snappy-compressed
/// bodies. Core's Gemfile Bundler.requires redis, hiredis-client,
/// panama_client, panama_metadata and snappy; redis-client comes with redis.
const TEST_CONFIG_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hiredis-client (0.28.0)\n      redis-client (= 0.28.0)\n    panama_client (3.2.3)\n      panama_metadata (>= 0.3.2, < 1.0.0)\n    panama_metadata (0.3.19.1687)\n    redis (5.4.0)\n      redis-client (>= 0.22.0)\n    redis-client (0.28.0)\n    snappy (0.5.1)\n\nDEPENDENCIES\n  activesupport\n  hiredis-client\n  panama_client\n  panama_metadata\n  redis\n  snappy\n";

fn test_config_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", TEST_CONFIG_LOCK)
        .write("sorbet/rbi/gems/redis@5.4.0.rbi", "# typed: true\nclass Redis\n  def initialize(options = {}); end\nend\nclass Redis::BaseError < ::StandardError; end\nclass Redis::CommandError < ::Redis::BaseError; end\n")
        .write("sorbet/rbi/gems/redis-client@0.28.0.rbi", "# typed: true\nclass RedisClient\n  class << self\n    def register(middleware); end\n  end\nend\n")
        .write("sorbet/rbi/gems/panama_client@3.2.3.rbi", "# typed: true\nmodule PanamaClient; end\nclass PanamaClient::NotFoundError < ::StandardError; end\n")
        .write("sorbet/rbi/gems/panama_metadata@0.3.19.1687.rbi", "# typed: true\nmodule PanamaMetadata; end\nclass PanamaMetadata::Registry\n  def initialize(path:); end\nend\n")
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

  def registry
    PanamaMetadata::Registry.new(path: "panama.yml")
  end

  def missing
    raise PanamaClient::NotFoundError, "missing"
  end

  def compress(body)
    Snappy.deflate(body)
  end
end
"#)
}

const TEST_CONFIG_GEMS: [&str; 5] = ["redis", "redis-client", "panama_client", "panama_metadata", "snappy"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_test_config_gems() {
    let (dir, errors) = test_config_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in TEST_CONFIG_GEMS.iter().chain(&["hiredis-client"]) {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["panama_client", "redis", "hiredis-client", "snappy", "panama_metadata"] {
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

/// panama_client is a GEM spec, but it depends on Core's grpc fork, a GIT
/// spec RubyGems cannot find on its own (`Gem::MissingSpecError: Could not
/// find 'grpc'`, swallowed by the guarded require). The git closure of
/// the needed gems is activated, dependencies first.
#[test]
fn a_git_dependency_of_a_needed_gem_is_activated_before_it() {
    let lock = TEST_CONFIG_LOCK.replace(
        "    panama_client (3.2.3)\n      panama_metadata (>= 0.3.2, < 1.0.0)\n",
        "    panama_client (3.2.3)\n      grpc (>= 1.40)\n      panama_metadata (>= 0.3.2, < 1.0.0)\n",
    );
    let lock = format!(
        "GIT\n  remote: https://github.com/Shopify/grpc.git\n  revision: d5dd2a32be6bfd2e9abd7d10ccfb87d129e868de\n  branch: shopify-fixes\n  submodules: true\n  specs:\n    grpc (1.85.0.dev)\n\n{lock}"
    );
    let (dir, errors) = test_config_app().write("Gemfile.lock", &lock).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let git_gems = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems");
    assert!(git_gems.contains("[\"grpc\", \"grpc-d5dd2a32be6b\", \"https://github.com/Shopify/grpc.git\""), "{git_gems}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(gemfile.contains("gem \"panama_client\"\n"), "{gemfile}");
}

/// Essentials' boot path through Rack, Faraday, Trilogy, Sabertooth,
/// OpenSRS, Magellan and OpenTelemetry, all Bundler.required by Core and
/// Rails-free. Core reopens `class Trilogy` (patches/trilogy_error.rb), so
/// `Trilogy::ProtocolError` is typed only from the gem's RBI. Prism is
/// demand only: Core's own `require "prism"` loads it.
const PLATFORM_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    faraday (2.14.3)\n    magellan-ruby (0.2.16)\n    opentelemetry-api (1.10.1)\n    opentelemetry-shopify (1.22.0)\n      opentelemetry-api (~> 1.0)\n    prism (1.9.0)\n    rack (2.2.24)\n    sabertooth (0.1.6)\n    shopify-opensrs (0.5.0)\n    trilogy (2.9.0)\n\nDEPENDENCIES\n  activesupport\n  faraday (>= 2)\n  magellan-ruby (= 0.2.16)\n  opentelemetry-shopify (~> 1.22)\n  rack (= 2.2.24)\n  sabertooth (~> 0.1)\n  shopify-opensrs (~> 0.5)\n  trilogy (= 2.9.0)\n";

fn platform_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
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

const PLATFORM_GEMS: [&str; 9] =
    ["rack", "faraday", "trilogy", "sabertooth", "shopify-opensrs", "magellan-ruby", "opentelemetry-shopify", "opentelemetry-api", "prism"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_platform_gems() {
    let (dir, errors) = platform_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in PLATFORM_GEMS {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["rack", "faraday", "trilogy", "sabertooth", "shopify-opensrs", "magellan-ruby", "opentelemetry-shopify"] {
        assert!(facades.contains(&format!("\"{feature}\"")), "{feature}: {facades}");
    }
    assert!(!facades.contains("\"prism\""), "prism is required by the app, not Bundler: {facades}");
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
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", MIDDLEWARE_LOCK)
        .write("sorbet/rbi/gems/stack_frames@0.1.3.rbi", &rbi("module StackFrames; end\nclass StackFrames::Buffer\n  def initialize(size); end\nend\n"))
        .write("sorbet/rbi/gems/lru_redux@1.1.0.rbi", &rbi("module LruRedux; end\nclass LruRedux::Cache\n  def initialize(max_size); end\nend\nclass LruRedux::ThreadSafeCache < ::LruRedux::Cache; end\n"))
        .write("sorbet/rbi/gems/secure_headers@5.2.0.rbi", &rbi("module SecureHeaders; end\nclass SecureHeaders::StrictTransportSecurity; end\nSecureHeaders::StrictTransportSecurity::HEADER_NAME = T.let(T.unsafe(nil), String)\n"))
        .write("sorbet/rbi/gems/shopify_bots@0.59.0.rbi", &rbi("module ShopifyBots; end\nmodule ShopifyBots::Rack; end\nclass ShopifyBots::Rack::Middleware; end\nShopifyBots::Rack::Middleware::REQUEST_PRIORITY = T.let(T.unsafe(nil), String)\n"))
        .write("sorbet/rbi/gems/customer_privacy_api_ruby@42.1.0.rbi", &rbi("module CustomerPrivacyApiRuby; end\nmodule CustomerPrivacyApiRuby::Storage; end\nCustomerPrivacyApiRuby::Storage::VISIT_COOKIE_EXPIRATION_TIME_SECONDS = T.let(T.unsafe(nil), Integer)\n"))
        .write("lib/middleware_settings.rb", r#"class MiddlewareSettings
  FRAMES = StackFrames::Buffer.new(32)
  CACHE = LruRedux::ThreadSafeCache.new(100)
  HSTS = SecureHeaders::StrictTransportSecurity::HEADER_NAME
  PRIORITY = ShopifyBots::Rack::Middleware::REQUEST_PRIORITY
  VISIT_TTL = CustomerPrivacyApiRuby::Storage::VISIT_COOKIE_EXPIRATION_TIME_SECONDS

  def headers
    { HSTS => PRIORITY, "ttl" => VISIT_TTL.to_s }
  end
end
"#)
}

const MIDDLEWARE_GEMS: [&str; 5] = ["stack_frames", "lru_redux", "secure_headers", "shopify_bots", "customer_privacy_api_ruby"];

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

/// Core's Money is shopify-money's. Another app's `Money` (RubyMoney's,
/// or its own class) is named the same, so the row is demanded only
/// where the lock names shopify-money.
const PRICE: &str = "class Price\n  ZERO = Money.new(0, \"USD\")\n\n  def zero\n    ZERO\n  end\nend\n";

const SHOPIFY_MONEY_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    shopify-money (4.1.1)\n\nDEPENDENCIES\n  activesupport\n  shopify-money (~> 4.1.1)\n";

const OWN_MONEY_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n\nDEPENDENCIES\n  activesupport\n";

#[test]
fn the_emitted_ruby_tree_declares_shopify_money_where_the_lock_names_it() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", SHOPIFY_MONEY_LOCK)
        .write("sorbet/rbi/gems/shopify-money@4.1.1.rbi", "# typed: true\nclass Money\n  def initialize(value, currency); end\n  def currency; end\nend\n")
        .write("lib/price.rb", PRICE)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(gemfile.contains("gem \"shopify-money\""), "{gemfile}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("\"shopify-money\""), "{facades}");
}

#[test]
fn an_app_s_own_money_does_not_declare_shopify_money() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", OWN_MONEY_LOCK)
        .write("lib/money.rb", "class Money\n  def initialize(value, currency)\n    @value = value\n  end\nend\n")
        .write("lib/price.rb", PRICE)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(std::fs::read_to_string(dir.join("app/models/price.rb")).unwrap().contains("Money.new"));
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(!gemfile.contains("shopify-money"), "{gemfile}");
}

const SERVICES_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    elasticsearch-shopify (6.0.1)\n    graphql (2.6.11)\n    shopify-adt (1.2.0)\n\nDEPENDENCIES\n  activesupport\n  elasticsearch-shopify (= 6.0.1)\n  graphql (~> 2.6.11)\n  shopify-adt (~> 1.2.0)\n";

fn services_app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", SERVICES_LOCK)
        .write("sorbet/rbi/gems/shopify-adt@1.2.0.rbi", &rbi("module Shopify; end\nmodule Shopify::Adt; end\nclass Shopify::Adt::Optional\n  def present?; end\nend\n"))
        .write("sorbet/rbi/gems/elasticsearch-shopify@6.0.1.rbi", &rbi("module ElasticSearch; end\nmodule ElasticSearch::Shopify; end\nclass ElasticSearch::Shopify::Client\n  def search(index, body); end\nend\n"))
        .write("sorbet/rbi/gems/graphql@2.6.11.rbi", &rbi("module GraphQL; end\nmodule GraphQL::Language; end\nmodule GraphQL::Language::Nodes; end\nclass GraphQL::Language::Nodes::AbstractNode; end\nclass GraphQL::Language::Nodes::Field < ::GraphQL::Language::Nodes::AbstractNode\n  def name; end\nend\n"))
        .write("lib/service_reader.rb", r#"class ServiceReader
  def optional?(value)
    value.is_a?(Shopify::Adt::Optional)
  end

  def client?(value)
    value.is_a?(ElasticSearch::Shopify::Client)
  end

  def field?(node)
    node.is_a?(GraphQL::Language::Nodes::Field)
  end
end
"#)
}

const SERVICES_GEMS: [&str; 3] = ["shopify-adt", "elasticsearch-shopify", "graphql"];

#[test]
fn the_emitted_ruby_tree_declares_and_loads_the_service_gems() {
    let (dir, errors) = services_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    for gem in SERVICES_GEMS {
        assert!(gemfile.contains(&format!("gem \"{gem}\"")), "{gem}: {gemfile}");
    }
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    for feature in ["shopify/adt", "elastic_search/shopify", "graphql"] {
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
