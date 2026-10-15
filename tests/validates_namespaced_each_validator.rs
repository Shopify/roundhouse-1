//! `validates :attr, http_uri: { .. }` names `HttpUriValidator`, which Core
//! declares as `ActiveModel::Validations::HttpUriValidator`. Rails
//! `const_get`s `"#{key.camelize}Validator"` from the model, whose ancestors
//! include `ActiveModel::Validations`, so the nested class answers. The
//! emitted rule was a bare `HttpUriValidator.new(..).validate(self)` in the
//! model, and a compiled model includes no `ActiveModel::Validations`: every
//! `valid?` of ApiClient, ShippingRateProvider, Script and the like raised
//! `NameError: uninitialized constant ApiClient::HttpUriValidator`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :hooks do |t|\n    t.string :callback_url\n  end\nend\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const HOOK: &str = r#"class Hook < ApplicationRecord
  validates :callback_url, http_uri: { allowed_protocols: ["https"] }, allow_nil: true
end
"#;

const HTTP_URI_VALIDATOR: &str = r#"module ActiveModel
  module Validations
    class HttpUriValidator < ActiveModel::EachValidator
      def validate_each(record, attribute, value)
        uri = URI.parse(value.to_s)
        record.errors.add(attribute, :invalid) unless options[:allowed_protocols].include?(uri.scheme)
      end
    end
  end
end
"#;

const SCRIPT: &str = r#"[nil, "https://shop.example/hook", "ftp://shop.example/hook"].each do |url|
  hook = Hook.new(callback_url: url)
  p [url, hook.valid?, hook.errors.to_a]
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/hook.rb", HOOK)
        .write("app/models/active_model/validations/http_uri_validator.rb", HTTP_URI_VALIDATOR)
}

#[test]
fn namespaced_validator_is_named_in_full_and_runs() {
    let (emitted, _) = app().emit(BuildTarget::Ruby);
    let hook = std::fs::read_to_string(emitted.join("app/models/hook.rb")).expect("emitted hook.rb");
    assert!(
        hook.contains("ActiveModel::Validations::HttpUriValidator.new"),
        "the validator is read bare:\n{hook}"
    );
    let run = app().run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "[nil, true, []]\n\
         [\"https://shop.example/hook\", true, []]\n\
         [\"ftp://shop.example/hook\", false, [\"Callback url is invalid\"]]\n"
    );
}
