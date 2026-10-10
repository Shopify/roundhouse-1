//! Bootstrap's `:initialize_cache` on the PostgreSQL CRuby tree (railties
//! 8.2.0.alpha application/bootstrap.rb): a store object an initializer
//! names in `config.cache_store` before that step becomes `Rails.cache`.
//! Core's essentials engine sets its TestProxyCacheStore so
//! (`initializer "platform.essentials.initialize_proxy_cache_store",
//! before: :initialize_cache`), and app code calls the store's own API:
//! r8 peek, 10 tests raised `undefined method
//! 'with_high_throughput_local_cache' for an instance of Rails::MemoryStore`.
//! A store named by a symbol keeps the tree's own store.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = r#"module Foo
  class Engine < Rails::Engine
    initializer "foo.initialize_proxy_cache_store", before: :initialize_cache do
      Rails.configuration.cache_store = Foo::LocalStore.new
    end
  end
end
"#;

const STORE: &str = r#"module Foo
  class LocalStore < ::ActiveSupport::Cache::MemoryStore
    def with_high_throughput_local_cache(ttl: nil)
      yield
    end
  end
end
"#;

#[test]
fn a_store_an_initializer_configures_before_initialize_cache_is_rails_cache() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_initialize_cache\n")
        .write("lib/foo/engine.rb", ENGINE)
        .write("app/models/foo/local_store.rb", STORE)
        .emit(BuildTarget::Ruby);
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             require_relative 'runtime/rails_configuration'\n\
             Rails.env_name = 'test'\n\
             require_relative 'runtime/rails_railtie'\n\
             require_relative 'runtime/rails_engine'\n\
             require_relative 'runtime/active_support_deprecation'\n\
             require_relative 'runtime/active_support_cache'\n\
             require_relative 'app/models'\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)\n\
             puts Rails.cache.class\n\
             Rails.cache.write('k', 1)\n\
             puts Rails.cache.with_high_throughput_local_cache(ttl: 5) { Rails.cache.read('k') }\n\
             puts Rails.cache.equal?(Rails.configuration.cache_store)\n\
             Rails.instance_variable_set(:@cache_store, nil)\n\
             Rails.configuration.cache_store = :null_store\n\
             Rails::Railtie.send(:initialize_cache_step, Rails.application)\n\
             puts Rails.instance_variable_get(:@cache_store).inspect\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "Foo::LocalStore\n1\ntrue\nnil\n", "{stderr}");
}
