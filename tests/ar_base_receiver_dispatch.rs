//! A receiver typed as `ActiveRecord::Base` itself, as Core's
//! BumpsShopVersion concern writes it (`record = self #: as
//! ActiveRecord::Base`), is a model whose ancestry reaches the runtime's
//! Base, and with `.class` that model's class. On the PostgreSQL CRuby
//! tree a method that Base defines (`record.class.current_transaction`,
//! `record.id`, `record.class.table_name`) is a plain send Ruby
//! dispatches, as on native Rails 8.2; one no runtime defines stays a
//! named gap (`analyze::body::runtime_base`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    shop_id bigint,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_ar_base_receiver
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// BumpsShopVersion's reads (components/platform/essentials/app/models/
/// concerns/bumps_shop_version.rb): the transaction a version bump joins,
/// the table and id it bumps, and the attribute it keys on.
const CONCERN: &str = r#"module Versioned
  extend ActiveSupport::Concern

  def version_row
    record = self #: as ActiveRecord::Base
    transaction = record.class.current_transaction
    [record.class.table_name, record.id, transaction.open?, record.class.primary_key, record.has_attribute?(:shop_id), record.has_attribute?(:missing)]
  end
end
"#;

/// A model class read through `singleton(ActiveRecord::Base)`, as Core's
/// GenerateOid and pagination backends take one: one flat type for the
/// class and its instances, answered from either side.
const TABLE_OF: &str = r#"module TableOf
  #: (singleton(ActiveRecord::Base)) -> String
  def self.call(klass)
    klass.table_name
  end

  def self.ascribed(model)
    klass = model #: as singleton(ActiveRecord::Base)
    [klass.table_name, klass.primary_key, klass.type_for_attribute("name").type]
  end
end
"#;

const UNKNOWN: &str = r#"module Unversioned
  def unknown_class_api
    record = self #: as ActiveRecord::Base
    record.class.some_unknown_ar_api
  end

  def unknown_instance_api
    record = self #: as ActiveRecord::Base
    record.some_unknown_instance_api
  end

  def unknown_value_api(model)
    klass = model #: as singleton(ActiveRecord::Base)
    klass.some_unknown_value_api
  end
end
"#;

const PROBE: &str = r#"class VersionedProbe
  def self.lines
    widget = Widget.create!(name: "a", shop_id: 7)
    out = []
    out << "row: #{widget.version_row.inspect}"
    Widget.transaction { out << "in transaction: #{widget.version_row[2].inspect}" }
    out << "after: #{widget.version_row[2].inspect}"
    out << "table of: #{TableOf.call(Widget).inspect} #{TableOf.ascribed(Widget).inspect}"
    out
  end
end
"#;

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/concerns/versioned.rb", CONCERN),
    ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Versioned\nend\n"),
    ("app/models/table_of.rb", TABLE_OF),
    ("app/models/versioned_probe.rb", PROBE),
];

fn overlay(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

fn refusal(method: &str) -> String {
    format!("ActiveRecord::Base not supported (all targets): {method} lacks a shared runtime/lowering implementation")
}

#[test]
fn base_typed_receivers_dispatch_what_the_postgresql_runtime_defines() {
    let mut files = FILES.to_vec();
    files.push(("app/models/concerns/unversioned.rb", UNKNOWN));
    files.push(("app/models/gadget.rb", "class Gadget < ApplicationRecord\n  self.table_name = \"widgets\"\n  include Unversioned\nend\n"));
    let (emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    let errors = real_errors(&errors);
    let concern = std::fs::read_to_string(emitted.join("app/models/versioned.rb")).expect("emitted concern");
    for send in [
        "transaction = record.class.current_transaction",
        "record.class.table_name",
        "record.id",
        "transaction.open?",
        "record.class.primary_key",
        "record.has_attribute?(:shop_id)",
    ] {
        assert!(concern.contains(send), "no plain {send:?} in:\n{concern}");
    }
    assert!(!concern.contains("not supported"), "{concern}");
    let table_of = std::fs::read_to_string(emitted.join("app/models/table_of.rb")).expect("emitted module");
    for send in ["klass.table_name", "klass.primary_key", "klass.type_for_attribute(\"name\")"] {
        assert!(table_of.contains(send), "no plain {send:?} in:\n{table_of}");
    }
    assert!(!table_of.contains("not supported"), "{table_of}");
    // Only the methods no runtime defines, each by name.
    let refused: Vec<&String> = errors.iter().copied().filter(|e| e.contains("ActiveRecord::Base not supported")).collect();
    assert_eq!(refused.len(), 3, "{errors:#?}");
    for method in ["some_unknown_ar_api", "some_unknown_instance_api", "some_unknown_value_api"] {
        assert!(refused.iter().any(|e| e.contains(&refusal(method))), "{method}: {refused:#?}");
    }
    assert_eq!(errors.len(), 3, "{errors:#?}");
    let gadget = std::fs::read_to_string(emitted.join("app/models/unversioned.rb")).expect("emitted concern");
    assert_eq!(gadget.matches("roundhouse: ActiveRecord::Base not supported").count(), 3, "{gadget}");
}

/// Without the PostgreSQL runtime nothing reads its Base: a method only
/// it defines stays the named gap. A class method the registry models
/// for every Ruby-family tree answers through `record.class` too.
#[test]
fn base_typed_receivers_stay_named_gaps_without_the_postgresql_runtime() {
    const READER: &str = r#"module Versioned
  def version_row
    record = self #: as ActiveRecord::Base
    [record.class.current_transaction, record.class.type_for_attribute(:name)]
  end
end
"#;
    let files = [
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\n  create_table \"widgets\" do |t|\n    t.string \"name\"\n  end\nend\n"),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/concerns/versioned.rb", READER),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  include Versioned\nend\n"),
    ];
    let (_emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    let refused: Vec<&String> = errors.iter().filter(|e| e.contains("ActiveRecord::Base not supported")).collect();
    assert_eq!(refused.len(), 1, "{errors:#?}");
    assert!(refused[0].contains(&refusal("current_transaction")), "{refused:#?}");
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn base_typed_receivers_answer_as_natively_on_postgresql() {
    let (emitted, errors) = overlay(&FILES).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "ar-base-receiver",
        &FILES,
        &emitted,
        "rh_e2e_ar_base_receiver",
        "VersionedProbe",
        &["app/models/application_record", "app/models/concerns/versioned", "app/models/widget", "app/models/table_of", "app/models/versioned_probe"],
    );
    // Native Rails 8.2 on the task PostgreSQL.
    assert_eq!(
        native,
        [
            r#"row: ["widgets", 1, false, "id", true, false]"#,
            "in transaction: true",
            "after: false",
            r#"table of: "widgets" ["widgets", "id", :string]"#,
        ],
        "native"
    );
    assert_eq!(emitted_lines, native, "emitted");
}
