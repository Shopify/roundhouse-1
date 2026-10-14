//! A `belongs_to class_name: "::Theme"` inside a namespace that holds a
//! namespace-only module of the same name (`OnlineStore::Theme`, which
//! exists only as the directory `online_store/theme/` the autoloader
//! turns into a module) must keep reading the top-level model. The
//! synthesized reader's receiver is the resolved class, which written
//! bare is resolved by Ruby lexically — to `OnlineStore::Theme`, a
//! module with no `find_by`. Core: `OnlineStore::VersionControl::
//! ThemeConfiguration#theme`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE themes (
    id bigint NOT NULL,
    name character varying,
    CONSTRAINT themes_pkey PRIMARY KEY (id)
);
CREATE TABLE theme_configurations (
    id bigint NOT NULL,
    theme_id bigint,
    CONSTRAINT theme_configurations_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_probe_rooted_class_name\n";

const VALIDATOR: &str = "module OnlineStore\n  module Theme\n    class HorizonValidator\n      def valid? = true\n    end\n  end\nend\n";

const CONFIG: &str = r#"module OnlineStore
  module VersionControl
    class ThemeConfiguration < ApplicationRecord
      self.table_name = "theme_configurations"
      belongs_to :theme, class_name: "::Theme", optional: true
    end
  end
end
"#;

const UUID_STRUCTURE: &str = r#"CREATE TABLE themes (
    id uuid NOT NULL,
    name character varying,
    CONSTRAINT themes_pkey PRIMARY KEY (id)
);
CREATE TABLE theme_configurations (
    id bigint NOT NULL,
    theme_id uuid,
    CONSTRAINT theme_configurations_pkey PRIMARY KEY (id)
);
"#;

const UUID_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.uuid_attributes(*names)
    names.each do |name|
      attribute name.to_sym, :uuid_as_integer
    end
  end
end
"#;

const UUID_CONFIG: &str = r#"module OnlineStore
  module VersionControl
    class ThemeConfiguration < ApplicationRecord
      self.table_name = "theme_configurations"
      uuid_attributes :theme_id
      belongs_to :theme, class_name: "::Theme", optional: true
    end
  end
end
"#;

const UUID_TYPE: &str = r#"module Probe
  class UuidAsInteger < ActiveModel::Type::Value
    def type
      :uuid_as_integer
    end
  end
end
"#;

fn base() -> emit_and_run::Overlay {
    [
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/theme.rb", "class Theme < ApplicationRecord\nend\n"),
        ("app/models/online_store/theme/horizon_validator.rb", VALIDATOR),
    ]
    .iter()
    .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn model(app: emit_and_run::Overlay) -> String {
    let (emitted, _errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    std::fs::read_to_string(emitted.join("app/models/online_store/version_control/theme_configuration.rb"))
        .expect("emitted model")
}

/// The statement-reading finder an ordinary integer key lowers to.
#[test]
fn a_rooted_class_name_reader_is_not_captured_by_a_namespace_only_module() {
    let model = model(
        base()
            .write("db/structure.sql", STRUCTURE)
            .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
            .write("app/models/online_store/version_control/theme_configuration.rb", CONFIG),
    );
    assert!(model.contains("result = ::Theme.from_stmt(stmt)"), "{model}");
}

/// Core's shape: `model_uuid_attributes` retypes the key, which reads
/// through `find_by` on the declared type.
#[test]
fn a_rooted_class_name_find_by_reader_is_not_captured_by_a_namespace_only_module() {
    let model = model(
        base()
            .write("db/structure.sql", UUID_STRUCTURE)
            .write("app/models/theme.rb", "class Theme < ApplicationRecord\n  uuid_attributes :id\nend\n")
            .write("app/models/application_record.rb", UUID_RECORD)
            .write("lib/probe/uuid_as_integer.rb", UUID_TYPE)
            .write("config/initializers/uuid_types.rb", "ActiveRecord::Type.register(:uuid_as_integer, Probe::UuidAsInteger, adapter: :postgresql)\n")
            .write("app/models/online_store/version_control/theme_configuration.rb", UUID_CONFIG),
    );
    assert!(model.contains("::Theme.find_by(id: @theme_id)"), "{model}");
}
