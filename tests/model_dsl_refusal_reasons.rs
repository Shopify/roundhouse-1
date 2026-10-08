//! A `default_scope` the lowered model does not reproduce (anything but
//! a parameterless block or lambda over a `where` / `where.not` /
//! `order` / `extending` / `preload` / `includes` chain, which
//! `default_scope_pg_runtime` covers), and an `alias_attribute` it does
//! not compile (anything but literal names over a column, which
//! `alias_attribute_pg_runtime` covers), are refused with the Rails
//! behavior the lowered model lacks, not the generic "the class does
//! not run it".
//!
//! Rails merges a default_scope into every relation of the class and
//! seeds new records with its `where` values, and translates an
//! attribute alias to its column in `new`, `where`, `order` and `pluck`.
//! The native run below pins the behavior the reasons describe, on
//! Core's pinned activerecord.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const WIDGET: &str = r#"class Widget < ApplicationRecord
  default_scope { where(live: true) }
  alias_attribute :title, :name
end
"#;

/// An STI subclass's own default_scope.
const SPECIAL: &str = "class Special < Widget\n  default_scope { order(:name) }\nend\n";

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  create_table :widgets do |t|\n    t.string :name\n    t.string :type\n    t.boolean :live, default: false\n  end\nend\n";

const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/widget.rb", WIDGET),
    ("app/models/special.rb", SPECIAL),
];

const PROBE: &str = r#"Widget.unscoped.create!(name: "dead")
w = Widget.create!(title: "x")
puts "count: #{Widget.count} #{Widget.unscoped.count}"
puts "seeded: #{w.live}"
puts "found: #{Widget.find_by(title: "x")&.name.inspect}"
puts "where: #{Widget.where(title: "x").to_sql}"
puts "order: #{Widget.order(:title).to_sql}"
puts "pluck: #{Widget.pluck(:title).inspect}"
puts "special: #{Special.all.to_sql}"
"#;

const EXPECTED: &str = r#"count: 1 2
seeded: true
found: "x"
where: SELECT "widgets".* FROM "widgets" WHERE "widgets"."live" = TRUE AND "widgets"."name" = 'x'
order: SELECT "widgets".* FROM "widgets" WHERE "widgets"."live" = TRUE ORDER BY "widgets"."name" ASC
pluck: ["x"]
special: SELECT "widgets".* FROM "widgets" WHERE "widgets"."type" = 'Special' AND "widgets"."live" = TRUE ORDER BY "widgets"."name" ASC
"#;

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_model_dsl_refusal_reasons_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}{PROBE}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn rails_applies_the_default_scope_and_translates_the_alias() {
    assert_eq!(native(), EXPECTED);
}

/// `default_scope` forms `lower::default_scope` does not reproduce: an
/// option (Rails' `all_queries: true` also scopes a record's own update,
/// delete and reload) and a chain link the runtime scope does not apply.
const WIDGET_UNPORTED: &str =
    "class Widget < ApplicationRecord\n  default_scope -> { where(live: true) }, all_queries: true\n  alias_attribute :title, :name\nend\n";
const SPECIAL_UNPORTED: &str = "class Special < Widget\n  default_scope { order(:name).limit(5) }\nend\n";

#[test]
fn default_scope_and_alias_attribute_are_refused_with_the_reason() {
    let app = emit_and_run::empty_app()
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/widget.rb", WIDGET_UNPORTED)
        .write("app/models/special.rb", SPECIAL_UNPORTED)
        .write("app/models/plain.rb", "class Plain < ApplicationRecord\n  self.table_name = \"widgets\"\n  default_scope { where(live: true) }\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/gizmo.rb", "class Gizmo < ApplicationRecord\n  self.table_name = \"widgets\"\n  alias_attribute :label, :headline\nend\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n");
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    let scope = "Rails applies a default_scope to every query of the class";
    assert!(
        errors.iter().any(|e| e.contains("default_scope not supported") && e.contains("on `Widget`") && e.contains(scope)),
        "{errors:#?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("default_scope not supported") && e.contains("on `Special`") && e.contains(scope)),
        "{errors:#?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("alias_attribute not supported")
            && e.contains("on `Gizmo`")
            && e.contains("only for a literal new name over a literal column")),
        "{errors:#?}"
    );
    assert!(!errors.iter().any(|e| e.contains("on `Widget`") && e.contains("alias_attribute")), "{errors:#?}");
    // The reproduced form is claimed, not refused.
    assert!(!errors.iter().any(|e| e.contains("on `Plain`")), "{errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("the class does not run it")), "{errors:#?}");
}
