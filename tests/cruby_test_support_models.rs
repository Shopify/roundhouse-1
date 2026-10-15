//! ActiveRecord models declared in a test-support file (Core's
//! `Markets::Platform::ResponderTestHelper`: three dummy models over the
//! `catalogs` table beside the responder types that name them) are
//! ingested as models, not refused ("a model under a test path is not
//! ingested as one"). The emit carries each as an app model, what else the
//! file declares beside them, and its `require`s as the file's program,
//! and answers the file's test-load-path feature with all of them. The
//! runtime side, against native Rails, is `test_support_models_pg_runtime`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const HELPER: &str = r#"# frozen_string_literal: true

require "set"

module ResponderHelper
  class TestModel < ApplicationRecord
    self.table_name = "catalogs"

    class IDGenerator
      def next_id = TestModel.order(:id).limit(1).ids.first
    end
  end

  class TestSecondModel < ApplicationRecord
    self.table_name = "catalogs"
  end

  class TestType
    class << self
      def model = TestModel
    end
  end
end
"#;

#[test]
fn models_of_a_test_support_file_are_carried_with_the_file() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\ntest_paths:\n  - test/support/helpers\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "db/structure.sql",
            "CREATE TABLE catalogs (\n    id bigserial NOT NULL,\n    title character varying,\n    CONSTRAINT catalogs_pkey PRIMARY KEY (id)\n);\n",
        )
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("test/support/helpers/responder_helper.rb", HELPER)
        .write(
            "test/models/responder_test.rb",
            "require \"test_helper\"\nrequire \"support/helpers/responder_helper\"\n\nclass ResponderTest < ActiveSupport::TestCase\n  test \"models\" do\n    assert ResponderHelper::TestType.model\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    let real: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let read = |path: &str| std::fs::read_to_string(emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    for model in ["test_model", "test_second_model"] {
        let text = read(&format!("app/models/responder_helper/{model}.rb"));
        assert!(text.contains("< ApplicationRecord"), "{model}: {text}");
        assert!(text.contains("\"catalogs\""), "{model}: {text}");
    }
    assert!(read("app/models/responder_helper/test_model/id_generator.rb").contains("class IDGenerator"));
    assert!(read("app/models/responder_helper/test_type.rb").contains("def self.model"));
    // The file's own `require` and each class it declares load with its feature.
    let program = read("app/models/roundhouse_files/test/support/helpers/responder_helper.rb");
    assert!(program.contains("require \"set\""), "{program}");
    assert!(program.contains("responder_helper/test_model\")"), "{program}");
    assert!(program.contains("responder_helper/test_second_model\")"), "{program}");
    let autoload = read("app/models.rb");
    assert!(
        autoload.contains(
            "\"support/helpers/responder_helper\" => [\"models/roundhouse_files/test/support/helpers/responder_helper\"]"
        ),
        "the test load path answers the helper's feature"
    );
}
