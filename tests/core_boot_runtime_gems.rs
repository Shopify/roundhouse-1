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

/// Core's Bundler.require list, in its Gemfile order.
const CORE_REQUIRES: [&str; 27] = [
    "rack", "panama_client", "redis", "hiredis-client", "stack_frames", "snappy", "faraday", "faraday/gzip",
    "opentelemetry-instrumentation-trilogy", "opentelemetry-instrumentation-pg", "lru_redux", "secure_headers",
    "trilogy", "shopify_bots", "shopify_bots/agent_classification_identifier", "shopify_bots/rack/middleware",
    "shopify_bots/request_extension", "shopify_bots/session/object_agent_classification_record",
    "shopify_bots/session/agent_classification_updater", "sabertooth", "sabertooth/prelude", "shopify-opensrs",
    "customer_privacy_api_ruby", "panama_metadata", "opentelemetry-shopify", "magellan-ruby", "shopify-money",
];

/// A blog-shaped app whose lock names none of Core's gems requires none
/// of them, whatever the machine has installed: secure_headers 5.2
/// calls `ActiveSupport.on_load` when required, which the LoadError
/// guard does not rescue.
#[test]
fn an_app_whose_lock_omits_core_s_gems_requires_none_of_them() {
    for lock in [Some(OWN_MONEY_LOCK), None] {
        let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
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

/// Core's lock gets the whole list, in Gemfile order, though the lock
/// lists its specs alphabetically.
#[test]
fn core_s_lock_requires_its_bundler_require_list_in_gemfile_order() {
    let mut gems: Vec<&str> = CORE_REQUIRES
        .iter()
        .map(|f| match *f {
            "faraday/gzip" => "faraday-gzip",
            f if f.starts_with("shopify_bots") => "shopify_bots",
            f if f.starts_with("sabertooth") => "sabertooth",
            f => f,
        })
        .collect();
    gems.sort();
    gems.dedup();
    let specs: String = gems.iter().map(|g| format!("    {g} (1.0.0)\n")).collect();
    let deps: String = gems.iter().map(|g| format!("  {g}\n")).collect();
    let lock = format!("GEM\n  remote: https://rubygems.org/\n  specs:\n{specs}\nDEPENDENCIES\n{deps}");
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", &lock)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    let at: Vec<usize> = CORE_REQUIRES
        .iter()
        .map(|f| facades.find(&format!("\"{f}\"")).unwrap_or_else(|| panic!("{f}: {facades}")))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "out of Gemfile order: {facades}");
}
