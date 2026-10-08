//! Modules and classes declared inside `class << self`.
//!
//! Core's `Podding::Database::SchemaCache::ConnectionSerializer` keeps
//! its `MysqlColumnSerializer` and `PostgresqlColumnSerializer` modules
//! inside `class << self`, and its class methods return them by bare
//! name. Ingest did not look into that body, so both modules were
//! dropped and each read emitted as a `roundhouse: constant not
//! supported` raise. Analysis hoists them into the enclosing scope, as
//! the constants such a body writes already are. The emit writes them
//! in the owner's file (inside its `class << self` on the CRuby tree,
//! `tests/singleton_scoped_classes_cruby.rs`): a file of their own would
//! have to reopen the singleton class, which no autoload reaches.

use roundhouse::analyze::diagnose;
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_app_from_tree;

const SERIALIZER: &str = r#"
module Outer
  module Serializer
    class << self
      def pick(adapter)
        case adapter
        when "a" then ASer
        else BSer
        end
      end

      module ASer
        class << self
          def dump(x) = x.to_s
        end
      end

      class BSer
        def self.dump(x) = x.to_s
      end
    end
  end
end
"#;

fn app() -> roundhouse::App {
    let tree = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/outer/serializer.rb", SERIALIZER),
    ]
    .into_iter()
    .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

#[test]
fn a_module_in_a_singleton_body_is_read_at_its_hoisted_name() {
    let app = app();
    let errors: Vec<String> = diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("Ser")), "{errors:?}");
    let files = emit_library(&app);
    let serializer: String = files
        .iter()
        .filter(|f| f.path.to_string_lossy().ends_with("serializer.rb"))
        .map(|f| f.content.clone())
        .collect();
    assert!(serializer.contains("Outer::Serializer::ASer") && serializer.contains("Outer::Serializer::BSer"), "{serializer}");
    assert!(!serializer.contains("roundhouse:"), "{serializer}");
    // Written in the owner's file, not a file of their own.
    assert!(serializer.contains("module ASer") && serializer.contains("class BSer"), "{serializer}");
    for name in ["/a_ser.rb", "/b_ser.rb"] {
        assert!(!files.iter().any(|f| f.path.to_string_lossy().ends_with(name)), "{name} emitted apart");
    }
}
