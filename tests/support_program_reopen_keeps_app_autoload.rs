//! A test-support file program that reopens an app constant around its
//! own module leaves the app file that constant's autoload, and a class
//! method's default may call another class method.
//!
//! Core's query_engine_fixtures.rb (`require "pg"`, then `module
//! Merchandising; class Taxonomy; module QueryEngineReader; module
//! QueryEngineFixtures`) is a file program. Because no support file's
//! path named Merchandising::Taxonomy::QueryEngineReader and every
//! support file declaring it was a program, `apply_models_autoload` took
//! the constant as one declared only inside file programs and gave it no
//! autoload, although components/merchandising/app/models/merchandising/
//! taxonomy/query_engine_reader.rb names it: QueryEngineReader was an
//! empty implicit module (`undefined method 'configure'`, 7 unit-1000
//! setups). The fixture has the same shape one level up: the support
//! program reopens `class Taxonomy`, a model.
//!
//! TaxonomyQueryEngineReaderTestConfiguration's `def apply!(reader =
//! query_engine_reader)` in `class << self` names the private class
//! method below it; dead-default grounding looked it up among the
//! instance methods and rewrote the default to `raise "undefined local
//! variable or method 'query_engine_reader'"` (20+ unit-1000 teardowns).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const READER: &str = r#"module Acme
  class Taxonomy
    module Reader
      class << self
        def configure
          :configured
        end
      end
    end
  end
end
"#;

const FIXTURES: &str = r#"require "yaml"

module Acme
  class Taxonomy
    module Reader
      module Fixtures
        def self.loaded
          :fixtures
        end
      end
    end
  end
end
"#;

const CONFIGURATION: &str = r#"module Cfg
  class << self
    def apply!(reader = default_reader)
      reader
    end

    private

    def default_reader
      :default
    end
  end
end
"#;

const TEST: &str = r#"require "test_helper"

class AcmeTest < ActiveSupport::TestCase
  test "the app file is the autoload of the constant a support program reopens" do
    assert_equal :configured, Acme::Taxonomy::Reader.configure
    assert_equal "taxonomies", Acme::Taxonomy.table_name
  end

  test "a class method default calls a private class method" do
    assert_equal :default, Cfg.apply!
    assert_equal :given, Cfg.apply!(:given)
  end
end
"#;

#[test]
fn a_support_program_reopen_keeps_the_app_files_autoload() {
    let run = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/acme/taxonomy.rb", "module Acme\n  class Taxonomy < ApplicationRecord\n  end\nend\n")
        .write("app/models/acme/taxonomy/reader.rb", READER)
        .write("app/models/cfg.rb", CONFIGURATION)
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table :taxonomies do |t|\n    t.string :name\n  end\nend\n",
        )
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n  - test/models\n")
        .write("test/support/helpers/acme/taxonomy/reader/fixtures.rb", FIXTURES)
        .write("test/models/acme_test.rb", TEST)
        .run_test("test/models/acme_test.rb");
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(
        models.contains("[:Taxonomy, \"models/acme/taxonomy\", false]"),
        "Acme::Taxonomy keeps its autoload:\n{}",
        models.lines().filter(|l| l.contains("Acme")).collect::<Vec<_>>().join("\n")
    );
    run.assert_passes();
    assert!(run.stdout.contains("2 tests passed") || run.stdout.contains("2 runs"), "{}", run.stdout);
}
