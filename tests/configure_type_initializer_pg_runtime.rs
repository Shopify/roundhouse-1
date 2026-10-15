//! `GraphApi::Admin.configure_query_root do field … end` in a boot
//! initializer, against native Rails 8.2 on the same local PostgreSQL
//! database.
//!
//! graphql-shopify's `ConfigureType` gives a module that extends it a
//! `configure_<type>` method whose block is an `ActiveSupport.on_load`
//! block of the type, run with `self` the type when the type's class body
//! ends in `ConfigureType.run_hooks_for(self)`. Core declares the root
//! type's fields that way in 72 initializer files; their blocks' bare
//! `field` calls were refused as calls on `main`, so the schema's
//! QueryRoot had no fields ("Object types must have fields").
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
  database: rh_e2e_configure_type
"#;

/// graphql-shopify's `ConfigureType` (lib/graphql/shopify/configure_type.rb),
/// minus the sorbet annotations.
const CONFIGURE_TYPE: &str = r##"module GraphQL
  module Shopify
    module ConfigureType
      class << self
        def run_hooks_for(type)
          ActiveSupport.run_load_hooks("graphql_configure_#{type.name}", type)
        end
      end

      def configure_type(*types)
        types.each do |type|
          type = type.to_s
          type_name = "#{name}::#{type}"
          define_singleton_method("configure_#{type.underscore}") do |&block|
            ActiveSupport.on_load("graphql_configure_#{type_name}", &block)
          end
        end
      end
    end
  end
end
"##;

const SCHEMA_MODULE: &str = r#"module Api
  extend GraphQL::Shopify::ConfigureType
  configure_type :QueryRoot
end
"#;

const QUERY_ROOT: &str = r#"module Api
  class QueryRoot
    class << self
      def fields
        @fields ||= {}
      end

      def field(name, **options)
        fields[name] = options
      end
    end

    GraphQL::Shopify::ConfigureType.run_hooks_for(self)
  end
end
"#;

const INITIALIZER: &str = r#"Api.configure_query_root do
  field :shop, type: "Shop"
  field :products, type: "ProductConnection"
end
"#;

const PROBE: &str = r##"class ConfigureTypeProbe
  def self.lines
    Api::QueryRoot.fields.map { |name, options| "#{name}: #{options[:type]}" }
  end
end
"##;

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("config/initializers/api_fields.rb", INITIALIZER),
    ("db/structure.sql", STRUCTURE),
    ("app/models/graphql/shopify/configure_type.rb", CONFIGURE_TYPE),
    ("app/models/api.rb", SCHEMA_MODULE),
    ("app/models/api/query_root.rb", QUERY_ROOT),
    ("app/models/configure_type_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_configure_type_block_in_an_initializer_declares_the_fields_like_native_rails() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "configure-type",
        &FILES,
        &emitted,
        "rh_e2e_configure_type",
        "ConfigureTypeProbe",
        &[
            "app/models/graphql/shopify/configure_type",
            "app/models/api",
            "config/initializers/api_fields",
            "app/models/api/query_root",
            "app/models/configure_type_probe",
        ],
    );
    assert_eq!(native, ["shop: Shop", "products: ProductConnection"], "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

fn ingest(files: &[(&str, &str)]) -> roundhouse::App {
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec())).collect();
    roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest")
}

/// Both fields' statements are the initializer program's, not gaps.
#[test]
fn the_configure_blocks_statements_are_carried() {
    let app = ingest(&FILES);
    let program = app.initializer_programs.iter().find(|p| p.file.ends_with("api_fields.rb")).expect("program");
    assert_eq!(program.statements, 2, "{:?}", app.initializer_programs);
    assert!(app.initializer_gaps.is_empty(), "{:?}", app.initializer_gaps.iter().map(|g| &g.detail).collect::<Vec<_>>());
}

/// A `configure_*` call on a module that does not extend `ConfigureType`
/// stays refused by name: its block's `self` is not known.
#[test]
fn a_configure_call_on_another_module_stays_refused() {
    let mut files: Vec<(&str, &str)> = FILES.iter().copied().filter(|(p, _)| !p.ends_with("api.rb")).collect();
    files.push(("app/models/api.rb", "module Api\nend\n"));
    let app = ingest(&files);
    assert!(
        app.initializer_gaps.iter().any(|g| g.detail.contains("`field` has an implicit receiver")),
        "{:?}",
        app.initializer_gaps.iter().map(|g| &g.detail).collect::<Vec<_>>()
    );
}
