//! A module an initializer prepends onto another module's singleton class
//! (Core's `UrlHelpers.singleton_class.prepend(OnlineStore::UrlHelpersExtension)`
//! inside `Rails.autoloaders.main.on_load("UrlHelpers")`) supplies instance
//! methods of that singleton class, so its methods keep the instance side
//! though a helper module that includes it flips them class side.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_om175_ext\n";

const EXTENSION: &str = r##"module Ext
  extend T::Helpers

  def default_url_options = { a: 1 }

  def with_memo
    @memo = 1
    yield
  end
end
"##;

const URL_HELPERS: &str = r##"module UrlHelpers
  def self.root_url = "/"
end
"##;

const INITIALIZER: &str = r##"Rails.autoloaders.main.on_load("UrlHelpers") do
  UrlHelpers.singleton_class.prepend(Ext)
end
"##;

fn emitted_ext() -> String {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", DATABASE_YML)
        .write("db/structure.sql", "CREATE TABLE things (\n    id bigserial NOT NULL\n);\n")
        .write("app/models/ext.rb", EXTENSION)
        .write("app/helpers/foo_helper.rb", "module FooHelper\n  include Ext\n\n  def foo = with_memo { 1 }\nend\n")
        .write("app/models/url_helpers.rb", URL_HELPERS)
        .write("config/initializers/url_helpers.rb", INITIALIZER)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{real:?}");
    std::fs::read_to_string(emitted.join("app/models/ext.rb")).unwrap()
}

#[test]
fn a_prepended_extension_keeps_its_instance_methods() {
    let text = emitted_ext();
    assert!(text.contains("\n    def with_memo") || text.contains("\n  def with_memo"), "{text}");
    // The helper's own call `Ext.with_memo` still has its target.
    assert!(text.contains("def self.with_memo"), "{text}");
    assert!(text.matches("def with_memo").count() == 1 && text.matches("def self.with_memo").count() == 1, "{text}");
}
