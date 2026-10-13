//! A constant's value that reads its own name reads the name Ruby finds
//! BEFORE the assignment binds it: lexically outward, then in the
//! class's ancestors.
//!
//! app_profiler 0.6.0's `AppProfiler::Yarn::WithFirefoxProfiler`
//! includes `Command` and writes `VALID_COMMANDS = [*VALID_COMMANDS,
//! [...]]`, which extends `Command::VALID_COMMANDS`. Rubydex answers the
//! read with the declaration being assigned, and typed as that the value
//! nested itself one level deeper on every constant-registry round: on
//! the full Shopify Core closure the registry never settled (one
//! declaration changed per round, for as many rounds as there are
//! constants).

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ident::{ClassId, Symbol};
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::ty::Ty;

fn app_with(files: &[(&str, &str)]) -> roundhouse::App {
    let mut tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"gauges\", force: :cascade do |t|\n    t.string \"label\", null: false\n  end\nend\n",
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\n  get \"/gauges\", to: \"gauges#index\"\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    for (p, c) in files {
        tree.insert(PathBuf::from(p), c.as_bytes().to_vec());
    }
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn library_ret(app: &roundhouse::App, class: &str, method: &str) -> Ty {
    let lc = app
        .library_classes
        .iter()
        .find(|lc| lc.name == ClassId(Symbol::new(class)))
        .unwrap_or_else(|| panic!("{class} is a library class"));
    let m = lc.methods.iter().find(|m| m.name == Symbol::new(method)).unwrap_or_else(|| panic!("{class}#{method}"));
    match m.signature.clone().expect("inference wrote a signature") {
        Ty::Fn { ret, .. } => (*ret).clone(),
        other => panic!("not a Fn: {other:?}"),
    }
}

const COMMAND: &str = r#"module Yarn
  module Command
    VALID_COMMANDS = [
      ["which", "yarn"],
      ["yarn", "init", "--yes"],
    ]

    def valid_commands
      VALID_COMMANDS
    end
  end
end
"#;

const WITH_PROFILER: &str = r#"module Yarn
  module WithProfiler
    include Command

    VALID_COMMANDS = [
      *VALID_COMMANDS,
      ["git", "clone", "https://example.com/profiler", "profiler"],
    ]

    def valid_commands
      VALID_COMMANDS
    end
  end
end
"#;

fn array_of_string_arrays() -> Ty {
    Ty::Array { elem: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Str) }) }
}

/// The same value, spelled with the constant it extends: no read of
/// the name being assigned.
const EXPLICIT: &str = r#"module Yarn
  module Explicit
    VALID_COMMANDS = [
      *Yarn::Command::VALID_COMMANDS,
      ["git", "clone", "https://example.com/profiler", "profiler"],
    ]

    def valid_commands
      VALID_COMMANDS
    end
  end
end
"#;

#[test]
fn a_value_extending_an_included_modules_constant_reads_that_constant() {
    let app = app_with(&[
        ("lib/yarn/command.rb", COMMAND),
        ("lib/yarn/with_profiler.rb", WITH_PROFILER),
        ("lib/yarn/explicit.rb", EXPLICIT),
    ]);
    assert_eq!(library_ret(&app, "Yarn::Command", "valid_commands"), array_of_string_arrays());
    // Typed as the explicit spelling is: one array literal over the
    // included module's value, not a value nested in itself.
    let explicit = library_ret(&app, "Yarn::Explicit", "valid_commands");
    assert!(matches!(&explicit, Ty::Array { .. }), "{explicit:?}");
    assert_eq!(library_ret(&app, "Yarn::WithProfiler", "valid_commands"), explicit);
}

const OUTER: &str = r#"module Limits
  MAX = 10

  class Tight
    MAX = MAX - 5

    def max
      MAX
    end
  end
end
"#;

#[test]
fn a_value_reading_an_outer_namespaces_constant_reads_that_constant() {
    let app = app_with(&[("lib/limits.rb", OUTER)]);
    assert_eq!(library_ret(&app, "Limits::Tight", "max"), Ty::Int);
}

