//! The identifications gem's `has_token :token` runs in the model body of
//! the Ruby tree when the boot requires the gem.
//!
//! Its railtie includes `ModelExtension` into ActiveRecord::Base, so the
//! call is a class method there: it sets `token_prefix` and the
//! `before_create` hook. The emit dropped it ("has_token not supported"),
//! and Core's `GiftCard#generate_token` (`generate_token_for(:token)`)
//! raised `undefined method '+' for nil` from the gem's
//! `prefix + SecureRandom.hex` (token_prefix unset).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const APPLICATION: &str = "require_relative \"boot\"\nrequire \"rails\"\nBundler.require(*Rails.groups)\n";
const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"cards\" do |t|\n    t.string \"token\"\n  end\nend\n";
const CARD: &str = "class Card < ApplicationRecord\n  has_token :token\nend\n";

fn emit(gemfile: &str) -> (String, Vec<String>) {
    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile", gemfile)
        .write("config.ru", "")
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", APPLICATION)
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_has_token_unused\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/card.rb", CARD)
        .emit(BuildTarget::Ruby);
    (std::fs::read_to_string(tree.join("app/models/card.rb")).expect("card.rb"), errors)
}

#[test]
fn has_token_is_replayed_when_the_gem_is_required() {
    let (card, errors) = emit("source \"https://rubygems.org\"\ngem \"rails\"\ngem \"identifications\"\n");
    assert!(card.contains("has_token(:token)") || card.contains("has_token :token"), "{card}");
    assert!(!errors.iter().any(|e| e.contains("has_token")), "{errors:#?}");
}

#[test]
fn has_token_stays_dropped_without_the_gem() {
    let (card, errors) = emit("source \"https://rubygems.org\"\ngem \"rails\"\n");
    assert!(!card.contains("has_token"), "{card}");
    assert!(errors.iter().any(|e| e.contains("has_token")), "{errors:#?}");
}
