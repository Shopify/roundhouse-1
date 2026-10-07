//! `include(WithIds)` names a module nested in the class's superclass.
//!
//! Past the lexical scopes Ruby searches the class's ancestors before
//! the top level. Core's `Customers::CustomerEntity <
//! CustomerFoundations::Entity` opens with `include(WithIdentifiers)`,
//! which is `CustomerFoundations::Entity::WithIdentifiers`. The include
//! was searched only lexically, so it stayed bare and was refused as
//! `includes unresolved WithIdentifiers` on every entity (11 rows).

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::ingest::ingest_app_from_tree;

const ENTITY: &str = "module Cf\n  class Entity\n  end\nend\n";

const WITH_IDS: &str = r#"
module Cf
  class Entity
    module WithIds
      #: -> Integer
      def ident = 1
    end
  end
end
"#;

const THING: &str = r#"
module Cust
  class Thing < Cf::Entity
    include(WithIds)
  end

  class Shadowed < Cf::Entity
    include(WithIds)
  end

  module Shadowed::WithIds
  end
end
"#;

/// A grandchild finds it two steps up the chain.
const SUB: &str = "module Cust\n  class SubThing < Thing\n    include(WithIds)\n  end\nend\n";

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/cf/entity.rb", ENTITY),
        ("app/models/cf/entity/with_ids.rb", WITH_IDS),
        ("app/models/cust/thing.rb", THING),
        ("app/models/cust/sub_thing.rb", SUB),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn includes(app: &roundhouse::App, class: &str) -> Vec<String> {
    app.library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == class)
        .unwrap_or_else(|| panic!("no library class {class}"))
        .includes
        .iter()
        .map(|c| c.0.as_str().to_string())
        .collect()
}

#[test]
fn an_include_resolves_through_the_superclass() {
    let app = app();
    assert_eq!(includes(&app, "Cust::Thing"), ["Cf::Entity::WithIds"]);
    assert_eq!(includes(&app, "Cust::SubThing"), ["Cf::Entity::WithIds"]);
    let errors: Vec<String> = diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("includes unresolved")), "{errors:?}");
}

/// The lexical scopes are searched first: a `WithIds` nested in the
/// class itself wins over the superclass's.
#[test]
fn a_lexical_definition_wins_over_the_superclass() {
    assert_eq!(includes(&app(), "Cust::Shadowed"), ["Cust::Shadowed::WithIds"]);
}
