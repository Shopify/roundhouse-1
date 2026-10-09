//! Rails constants no emitted tree carries, refused by name.
//!
//! An unresolved constant is reported as `constant not supported`; for
//! these the report also says why the framework class is absent. Each one
//! is defined by Rails 8.2 (activerecord, activesupport and actionview 8.2.0.alpha), and none has a
//! runtime port: what it does is done at build time or outside the
//! program. An app that defines one of these names itself (a reopened
//! class Rubydex indexes) resolves before this table is consulted, and a
//! class a registry models (`ActiveRecord::Reflection::AssociationReflection`)
//! never reaches it.

/// `(name, namespace, reason)`: `namespace` also covers every constant
/// nested under `name` (`ActiveRecord::Tasks::DatabaseTasks`).
const REFUSED: &[(&str, bool, &str)] = &[
    // Not fixtures.rb or test_fixtures.rb: the PostgreSQL tree carries
    // ActiveRecord::FixtureSet, its File and ActiveRecord::Fixture
    // (runtime/spinel/cruby/active_record_fixture_set.rb, gated to that
    // tree by analyze::runtime_reads) and ActiveRecord::TestFixtures'
    // transactional tests (runtime/spinel/cruby/test/active_record_test_fixtures.rb,
    // indexed where the app carries that runtime).
    // tasks/*.rb.
    ("ActiveRecord::Tasks", true, DATABASE_TASKS),
    // connection_adapters: the adapter's schema reflection objects.
    ("ActiveRecord::ConnectionAdapters::Column", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::NullColumn", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::SqlTypeMetadata", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::IndexDefinition", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::ForeignKeyDefinition", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::CheckConstraintDefinition", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::SchemaReflection", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::BoundSchemaReflection", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::SchemaCache", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::PostgreSQL::Column", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::PostgreSQL::TypeMetadata", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::PostgreSQL::Name", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::MySQL::Column", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::MySQL::TypeMetadata", false, SCHEMA_REFLECTION),
    ("ActiveRecord::ConnectionAdapters::MySQL::IndexDefinition", false, SCHEMA_REFLECTION),
    // reflection.rb.
    ("ActiveRecord::Reflection", true, REFLECTION),
    // activesupport current_attributes.rb.
    ("ActiveSupport::CurrentAttributes", true, CURRENT_ATTRIBUTES),
    // actionview helpers/asset_url_helper.rb. Essentials'
    // lib/patches/action_view_ext.rb reopens it with `module_eval`, and
    // two Core services include it.
    ("ActionView::Helpers::AssetUrlHelper", true, ASSET_URL_HELPER),
];

const DATABASE_TASKS: &str = "Rails' database tasks are not carried: the schema is compiled from the app's schema file, \
    and the database is created and prepared outside the program";
const SCHEMA_REFLECTION: &str = "the adapter's schema reflection objects are not carried: the schema is compiled from the \
    app's schema file, and the runtime adapter builds no columns, type metadata, indexes or schema cache";
const REFLECTION: &str = "Rails' reflection classes are not carried: associations compile to methods, and the runtime \
    holds no reflection objects";
const CURRENT_ATTRIBUTES: &str = "Rails' CurrentAttributes base class is not carried: each subclass compiles to a plain class \
    with its own instance and accessors (ingest::current_attributes), so the runtime has no base class, \
    descendants list, defaults or NOT_SET";

const ASSET_URL_HELPER: &str = "Rails' AssetUrlHelper module is not carried: a view's asset_path, image_path and the \
    other asset helpers compile to calls on the runtime's ViewHelpers (runtime/ruby/action_view/view_helpers.rb), \
    which answers `/assets/<source>` with no asset host or digest, so there is no module to include or reopen";

/// Why `name` (as written, without a leading `::`) is refused, when it is
/// one of the framework constants above.
pub(in crate::analyze) fn refusal(name: &str) -> Option<&'static str> {
    let name = name.trim_start_matches("::");
    REFUSED.iter().find_map(|(refused, namespace, reason)| {
        let nested = *namespace && name.strip_prefix(refused).is_some_and(|rest| rest.starts_with("::"));
        (name == *refused || nested).then_some(*reason)
    })
}

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn exact_and_nested_names() {
        // The PostgreSQL tree's runtime carries the fixture classes.
        assert_eq!(refusal("ActiveRecord::FixtureSet"), None);
        assert_eq!(refusal("::ActiveRecord::Fixture::FormatError"), None);
        assert_eq!(refusal("ActiveRecord::Tasks::DatabaseTasks"), Some(super::DATABASE_TASKS));
        assert_eq!(refusal("ActiveRecord::ConnectionAdapters::PostgreSQL::TypeMetadata"), Some(super::SCHEMA_REFLECTION));
        // Exact entries do not cover their namespace, nor a longer name.
        assert_eq!(refusal("ActiveRecord::ConnectionAdapters::PostgreSQL::OID"), None);
        assert_eq!(refusal("ActiveRecord::ConnectionAdapters::ColumnDefinition"), None);
        assert_eq!(refusal("ActiveRecord::Base"), None);
        assert_eq!(refusal("ActiveSupport::CurrentAttributes::NOT_SET"), Some(super::CURRENT_ATTRIBUTES));
        assert_eq!(refusal("::ActionView::Helpers::AssetUrlHelper"), Some(super::ASSET_URL_HELPER));
        assert_eq!(refusal("ActionView::Helpers::AssetUrlHelper::URI_REGEXP"), Some(super::ASSET_URL_HELPER));
    }

    /// Each refused name is a constant native Rails defines: the Rails
    /// Core runs (tests/support/rails_oracle.rb), with the files that
    /// define them loaded.
    #[test]
    #[ignore = "requires ruby and Core's pinned activerecord"]
    fn every_refused_name_is_defined_by_native_rails() {
        let names: Vec<&str> = super::REFUSED.iter().map(|(name, _, _)| *name).collect();
        // A file, not `-e`: the oracle re-executes its script, with its
        // arguments, under the pinned gem home's jemalloc.
        let script = r#"require ARGV.first
RailsOracle.activate("activesupport", "activemodel", "activerecord", "actionview")
require "active_record"
%w[action_view action_view/helpers active_support/current_attributes active_record/fixtures active_record/tasks/database_tasks
   active_record/connection_adapters/postgresql_adapter active_record/connection_adapters/mysql/column
   active_record/connection_adapters/mysql/type_metadata active_record/connection_adapters/mysql/schema_definitions].each { require _1 }
missing = ARGV.drop(1).reject { Object.const_defined?(_1) }
puts missing.empty? ? "ok" : "missing #{missing.join(", ")}"
"#;
        let path = std::env::temp_dir().join(format!("rh_refused_constants_{}.rb", std::process::id()));
        std::fs::write(&path, script).expect("write script");
        let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
        let out = std::process::Command::new("ruby")
            .arg(&path)
            .arg(oracle)
            .args(&names)
            .output()
            .expect("ruby");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(stdout.trim(), "ok");
    }
}
