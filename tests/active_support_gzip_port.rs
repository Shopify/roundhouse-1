//! `ActiveSupport::Gzip` (active_support/gzip.rb), which native Rails
//! autoloads and app code names with no require: Core's
//! `Webhooks::PayloadEnvelope#encode` is `Base64.encode64(
//! ActiveSupport::Gzip.compress(JSON.generate(envelope_data)))`, and the
//! PostgreSQL CRuby tree emitted `raise "constant not supported"` in its
//! place. The port is carried where emitted app code names the module, and
//! answers as Rails does (tests/active_support_utilities_runtime.rb holds
//! it to native ActiveSupport).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\n\
DEPENDENCIES\n  i18n\n";

const ENVELOPE: &str = "require \"base64\"\nrequire \"json\"\n\nclass PayloadEnvelope\n  def encode(data)\n    Base64.encode64(ActiveSupport::Gzip.compress(JSON.generate(data)))\n  end\n\n  def decode(text)\n    JSON.parse(ActiveSupport::Gzip.decompress(Base64.decode64(text)))\n  end\nend\n";

fn app(model: Option<&str>) -> emit_and_run::Overlay {
    let app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_gzip_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    match model {
        Some(text) => app.write("app/models/payload_envelope.rb", text),
        None => app,
    }
}

#[test]
fn gzip_is_carried_where_app_code_names_it() {
    let (tree, errors) = app(Some(ENVELOPE)).emit(BuildTarget::Ruby);
    let constant: Vec<&String> = errors.iter().filter(|e| e.contains("ActiveSupport::Gzip")).collect();
    assert!(constant.is_empty(), "{constant:#?}");
    assert!(tree.join("runtime/active_support_gzip.rb").exists(), "port not carried");
    let envelope = std::fs::read_to_string(tree.join("app/models/payload_envelope.rb")).unwrap();
    assert!(envelope.contains("ActiveSupport::Gzip.compress"), "{envelope}");
    assert!(!envelope.contains("constant not supported"), "{envelope}");
}

#[test]
fn gzip_is_not_carried_where_no_app_code_names_it() {
    let (tree, _) = app(None).emit(BuildTarget::Ruby);
    assert!(!tree.join("runtime/active_support_gzip.rb").exists());
}
