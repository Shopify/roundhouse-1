//! `Podding::Redis::ClientFactory.register(owner: self, accessor: :redis)
//! { make_podded_proxy(...) }` in a plain class's body (Core's
//! `LeakyThrottle`): the call binds the class's `redis` accessor to the
//! pod's client and must run as the class loads, not be dropped (a nil
//! `redis` made `Resiliency.find_redis(LeakyThrottle)` answer nothing).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FACTORY: &str = r##"module Podding
  module Redis
    class Factory
      def register(owner:, accessor: :store, type: nil, &builder)
        builder ||= -> { make_podded_proxy(type: type) }
        owner.send("#{accessor}=", instance_exec(&builder))
      end

      def make_podded_proxy(type:, reject_default: true)
        "proxy:#{type}:#{reject_default}"
      end
    end

    ClientFactory = Factory.new
  end
end
"##;

const THROTTLE: &str = r#"class Throttle
  cattr_accessor(:redis)

  Podding::Redis::ClientFactory.register(owner: self, accessor: :redis) do
    make_podded_proxy(type: :core_throttle, reject_default: false)
  end

  def level
    redis
  end
end
"#;

const TYPED: &str = r#"class DisposableStore
  cattr_accessor(:redis)

  Podding::Redis::ClientFactory.register(owner: self, accessor: :redis, type: :disposable)
end
"#;

#[test]
fn a_plain_class_binds_its_redis_accessor_through_the_client_factory() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/podding/redis.rb", FACTORY)
        .write("app/models/throttle.rb", THROTTLE)
        .write("app/models/disposable_store.rb", TYPED)
        .run_ruby("puts Throttle.redis.inspect\nputs Throttle.new.level.inspect\nputs DisposableStore.redis.inspect");
    run.assert_passes();
    assert_eq!(run.stdout, "\"proxy:core_throttle:false\"\n\"proxy:core_throttle:false\"\n\"proxy:disposable:true\"\n");
}
