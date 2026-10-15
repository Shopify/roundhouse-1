//! om131: model-surface gaps found by the unit-1000-v2 sample.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_om131\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const STRUCTURE: &str = r#"CREATE TABLE codes (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT codes_pkey PRIMARY KEY (id)
);

CREATE TABLE applications (
    id bigserial NOT NULL,
    code_id bigint,
    CONSTRAINT applications_pkey PRIMARY KEY (id)
);
"#;

fn overlay(models: &[(&str, &str)]) -> emit_and_run::Overlay {
    let base = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile.lock", LOCK)
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", APPLICATION_RECORD);
    models.iter().fold(base, |app, (path, text)| app.write(path, text))
}

fn emitted_file(models: &[(&str, &str)], path: &str) -> String {
    let (emitted, _errors) = overlay(models).emit(roundhouse::project::BuildTarget::Ruby);
    std::fs::read_to_string(emitted.join("out").join(path))
        .or_else(|_| std::fs::read_to_string(emitted.join(path)))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `unsafe_self.alias_method(:code, :price_rule_code)` below a
/// `belongs_to`: Rails defines the association reader in a generated
/// module before the body continues, so the alias finds it.
#[test]
fn alias_of_an_association_reader_follows_the_reader() {
    let src = emitted_file(
        &[
            ("app/models/code.rb", "class Code < ApplicationRecord\nend\n"),
            ("app/models/base_application.rb", "class BaseApplication < ApplicationRecord\n  self.table_name = \"applications\"\nend\n"),
            ("app/models/scoped.rb", "module Scoped\n  extend ActiveSupport::Concern\nend\n"),
            (
                "app/models/sales/application.rb",
                "module Sales\n  class Application < BaseApplication\n    include Scoped\n\n    belongs_to :price_rule_code, -> { all }, class_name: \"::Code\", foreign_key: :code_id\n\n    unsafe_self = self #: as untyped\n    unsafe_self.alias_method(:code, :price_rule_code)\n    unsafe_self.alias_method(:code=, :price_rule_code=)\n\n    class << self\n      def sti_name\n        :code\n      end\n    end\n  end\nend\n",
            ),
        ],
        "app/models/sales/application.rb",
    );
    let reader = src.find("def price_rule_code\n").unwrap_or_else(|| panic!("{src}"));
    let alias = src.find("alias_method(:code, :price_rule_code)").unwrap_or_else(|| panic!("{src}"));
    assert!(reader < alias, "the alias follows the reader:\n{src}");
    let writer = src.find("def price_rule_code=").unwrap_or_else(|| panic!("{src}"));
    let alias = src.find("alias_method(:code=, :price_rule_code=)").unwrap_or_else(|| panic!("{src}"));
    assert!(writer < alias, "the alias follows the writer:\n{src}");
}

/// `repositories.extensions.create(...)` in a service whose bare
/// `repositories` is its own reader (a SmartProperties property holding
/// a module): one model declares `has_many :extensions`, but the by-name
/// guess that `repositories` is that model's record wrote
/// `commitment_id: repositories.id`, NoMethodError for the module.
#[test]
fn own_reader_owner_is_not_guessed_to_be_the_record_declaring_the_association() {
    let src = emitted_file(
        &[
            ("app/models/code.rb", "class Code < ApplicationRecord\n  has_many :things, foreign_key: :code_id\nend\n"),
            ("app/models/thing.rb", "class Thing < ApplicationRecord\n  belongs_to :code\nend\n"),
            (
                "app/models/repos.rb",
                "module Repos\n  class << self\n    def things\n      Thing\n    end\n  end\nend\n",
            ),
            (
                "app/models/creator.rb",
                "class Creator\n  def repositories\n    Repos\n  end\n\n  def perform\n    repositories.things.create(name: \"x\")\n  end\nend\n",
            ),
        ],
        "app/models/creator.rb",
    );
    assert!(src.contains("repositories.things.create"), "the call keeps its source shape:\n{src}");
    assert!(!src.contains("repositories.id"), "no `repositories.id` is read:\n{src}");
}

#[test]
#[ignore]
fn scratch_real_condition() {
    let real = std::fs::read_to_string(std::env::var("OM131_FILE").unwrap()).unwrap();
    let src = emitted_file(&[("app/models/markets/platform/condition.rb", &real)], "app/models/markets/platform/condition.rb");
    std::fs::write("/tmp/claude-1000/om131/scratch.out.rb", &src).unwrap();
    assert!(src.contains("force_condition_uniqueness_check"), "missing");
}

/// `attribute :flag, :boolean, default: false` on a name with no column
/// is a virtual attribute: assignable at construction, reading its
/// default until assigned (Core's `Markets::Platform::Condition`,
/// `force_condition_uniqueness_check`: `unknown attribute` on `new`).
#[test]
fn virtual_attribute_with_a_literal_default_gets_a_reader_and_writer() {
    let src = emitted_file(
        &[(
            "app/models/code.rb",
            "class Code < ApplicationRecord\n  attribute :force_flag, :boolean, default: false\n  attribute :label, :string, default: \"none\"\n  attribute :name, :string, default: \"column wins\"\nend\n",
        )],
        "app/models/code.rb",
    );
    for needed in ["def force_flag\n", "def force_flag=", "def label\n", "def label="] {
        assert!(src.contains(needed), "{needed:?} missing:\n{src}");
    }
    assert!(src.contains("defined?(@force_flag)"), "the reader answers the default until assigned:\n{src}");
}
