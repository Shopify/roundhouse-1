//! `Rails.application.config.session_store` on the PostgreSQL CRuby tree
//! (runtime/spinel/rails_configuration.rb), against railties 8.2.0.alpha
//! application/configuration.rb:604-622 (Shopify/rails 5eb8c4c45bee). An
//! initializer's `config.session_store(:name)` recorded nothing and raised
//! NoMethodError on Rails::Railtie::Configuration, aborting the
//! initializer file's remaining statements.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

#[test]
fn config_session_store_records_the_store_and_resolves_it_like_rails() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_session_store_config\n")
        .write("config/application.rb", "require \"rails\"\n\nmodule StoreApp\n  class Application < Rails::Application\n  end\nend\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
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
             require_relative 'config/application'\n\
             module ActionDispatch\n\
               class Session\n\
                 class MyCustomStore; end\n\
               end\n\
             end\n\
             config = Rails.application.config\n\
             p config.session_store?\n\
             p config.session_store\n\
             config.session_store(:my_custom_store, key: '_app')\n\
             p config.session_store?\n\
             p config.session_options\n\
             p config.session_store\n\
             config.session_store(:disabled)\n\
             p config.session_store\n\
             config.session_store(Object)\n\
             p config.session_store\n\
             config.session_store(:nope)\n\
             begin\n\
               config.session_store\n\
             rescue => e\n\
               puts e.message.lines.first\n\
             end\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        stdout,
        "nil\nnil\n:my_custom_store\n{key: \"_app\"}\nActionDispatch::Session::MyCustomStore\nnil\nObject\nUnable to resolve session store :nope.\n",
        "{stderr}"
    );
}
