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
