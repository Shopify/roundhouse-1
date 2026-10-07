//! A constant read of a source module that only nests other declarations.
//!
//! `module AuditTrail` holding only `Trail` and `Step` models no class of
//! its own, so reading it as a value (`AuditTrail =
//! Merchandising::PricingEngine::AuditTrail`, Core's step builders) had
//! no type and was refused as `constant not supported`. The module
//! exists wherever its nested declarations are emitted.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::ingest::ingest_app_from_tree;

const STEP: &str = r#"
module Merch
  module Engine
    module Trail
      class Step
        #: -> Integer
        def n = 1
      end
    end
  end
end
"#;

const USER: &str = r#"
module Merch
  module Engine
    class User
      Trail = Merch::Engine::Trail

      #: -> Integer
      def go = Trail::Step.new.n
    end
  end
end
"#;

/// A name no source declares stays refused.
const STRAY: &str = r#"
module Merch
  class Stray
    #: -> untyped
    def go = Merch::Engine::Nope
  end
end
"#;

fn errors() -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/merch/engine/trail/step.rb", STEP),
        ("app/models/merch/engine/user.rb", USER),
        ("app/models/merch/stray.rb", STRAY),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    diagnose(&app).into_iter().map(|d| d.to_string()).collect()
}

#[test]
fn a_namespace_only_module_reads_as_the_module() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("Merch::Engine::Trail")), "{errors:?}");
}

#[test]
fn an_undeclared_name_stays_refused() {
    let errors = errors();
    assert!(errors.iter().any(|e| e.contains("Nope")), "{errors:?}");
}
