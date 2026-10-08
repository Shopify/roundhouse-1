//! A module a boot initializer defines and then mixes in, against native
//! Rails 8.2 on the same local PostgreSQL database.
//!
//! Shopify Core's `config/initializers/active_record_composite_ids_methods_patch.rb`
//! defines `module ActiveRecordCompositeIdsMethodsPatch` at its top level
//! and prepends it inside `ActiveSupport.on_load(:active_record) { if … }`.
//! The module body was refused ("a `module` body in an initializer"), so
//! the carried prepend raised NameError at boot. The module is now carried
//! as the file's own definition (`app::keep_initializer_defined`) and the
//! initializer's program reads its constant where the definition stands,
//! so it is defined there, as Rails runs it, before the hook prepends it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_initializer_module
"#;

/// The composite ids patch's shape: a module, then a guarded prepend of it
/// onto a framework class in the load hook.
const INITIALIZER: &str = r##"# @requires_ancestor: ActiveRecord::Associations::CollectionAssociation
module CollectionIdsTag
  def ids_tag
    "tag:#{self.class.superclass.name}"
  end
end

ActiveSupport.on_load(:active_record) do
  unless ENV["SKIP_COLLECTION_IDS_TAG"]
    ActiveRecord::Associations::CollectionAssociation.prepend(CollectionIdsTag)
  end
end
"##;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHOP: &str = "class Shop < ApplicationRecord\nend\n";

const PROBE: &str = r##"class InitializerModuleProbe
  def self.lines
    out = []
    out << "defined: #{defined?(CollectionIdsTag).inspect}"
    out << "prepended: #{ActiveRecord::Associations::CollectionAssociation.ancestors.first == CollectionIdsTag}"
    out << "call: #{ActiveRecord::Associations::CollectionAssociation.allocate.ids_tag}"
    out << "shop: #{Shop.create!(name: "abc").name}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 4] = [
    r#"defined: "constant""#,
    "prepended: true",
    "call: tag:ActiveRecord::Associations::Association",
    "shop: abc",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("config/initializers/collection_ids_tag.rb", INITIALIZER),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/initializer_module_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn an_initializer_module_prepended_in_a_load_hook_matches_native_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "initializer-module",
        &FILES,
        &emitted,
        "rh_e2e_initializer_module",
        "InitializerModuleProbe",
        &[
            "config/initializers/collection_ids_tag",
            "app/models/application_record",
            "app/models/shop",
            "app/models/initializer_module_probe",
        ],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The module is carried and loaded at its statement, ahead of the hook.
#[test]
fn the_initializer_module_is_carried_and_loaded_at_its_statement() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let module = std::fs::read_to_string(emitted.join("app/models/collection_ids_tag.rb")).expect("collection_ids_tag.rb");
    assert!(module.contains("module CollectionIdsTag") && module.contains("def ids_tag"), "{module}");
    let holder = std::fs::read_to_string(emitted.join("app/models/roundhouse_initializer_config_initializers_collection_ids_tag.rb"))
        .expect("initializer holder");
    let read = holder.find("CollectionIdsTag\n").expect(&holder);
    let hook = holder.find("ActiveSupport.on_load").expect(&holder);
    assert!(read < hook, "{holder}");
}

fn ingest(files: &[(&str, &str)]) -> roundhouse::App {
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec())).collect();
    roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest")
}

/// The module is a library class, and its statement is the initializer
/// program's first, not a gap.
#[test]
fn the_initializer_module_is_ingested_as_the_files_definition() {
    let app = ingest(&FILES);
    let module = app.library_classes.iter().find(|lc| lc.name.0.as_str() == "CollectionIdsTag").expect("CollectionIdsTag");
    assert!(module.is_module && module.methods.iter().any(|m| m.name.as_str() == "ids_tag"));
    let program = app.initializer_programs.iter().find(|p| p.file.ends_with("collection_ids_tag.rb")).expect("program");
    assert_eq!(program.statements, 2, "the definition and the hook's guarded prepend");
    assert!(app.initializer_gaps.is_empty(), "{:?}", app.initializer_gaps.iter().map(|g| &g.detail).collect::<Vec<_>>());
}

/// A module of a namespace an app class sits under is a reopen of what
/// the autoloader defines, which the tree could only define fresh: it
/// stays out, and its statement stays a gap.
#[test]
fn a_mixed_in_namespace_of_an_app_class_stays_out() {
    let initializer = INITIALIZER.replace("CollectionIdsTag", "Shops");
    let mut files: Vec<(&str, &str)> = FILES.iter().copied().filter(|(p, _)| !p.ends_with("collection_ids_tag.rb")).collect();
    files.push(("config/initializers/collection_ids_tag.rb", &initializer));
    files.push((
        "app/models/shops/item.rb",
        "module Shops\n  class Item < ApplicationRecord\n    self.table_name = \"shops\"\n  end\nend\n",
    ));
    let app = ingest(&files);
    assert!(
        !app.library_classes.iter().any(|lc| lc.name.0.as_str() == "Shops" && lc.methods.iter().any(|m| m.name.as_str() == "ids_tag")),
        "the initializer's module is not carried"
    );
    assert!(
        app.initializer_gaps.iter().any(|g| g.construct == "module" && g.detail.contains("a `module` body in an initializer")),
        "{:?}",
        app.initializer_gaps.iter().map(|g| (&g.construct, &g.detail)).collect::<Vec<_>>()
    );
}
