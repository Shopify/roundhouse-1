//! A constant write evaluates its value before it binds the name, so a
//! bare reference to the name inside its own value reads the binding
//! the lookup finds without it: an outer scope's, or an ancestor's.
//!
//! Core's `app_profiler` gem has this shape:
//!
//! ```ruby
//! module WithFirefoxProfiler
//!   include Command
//!   VALID_COMMANDS = [*VALID_COMMANDS, ["git", "clone", ...]]
//! end
//! ```
//!
//! The source index resolved the `VALID_COMMANDS` in the value to the
//! declaration being written. Typed from itself, the value nested one
//! level deeper on every constant registry round and ran the registry to
//! its bound of one round per constant in the app.
use roundhouse::analyze::Analyzer;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::{ClassId, Symbol, Ty};
use std::path::PathBuf;

const BASE: &[(&str, &str)] = &[
    (
        "app/models/application_record.rb",
        "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
    ),
    (
        "db/schema.rb",
        "ActiveRecord::Schema[7.1].define(version: 1) do\n  create_table \"things\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n",
    ),
    (
        "app/models/yarn/command.rb",
        r#"module Yarn
  module Command
    VALID_COMMANDS = [
      ["which", "yarn"],
      ["yarn", "run", "speedscope", /.*\.json/],
    ]
    private_constant(:VALID_COMMANDS)

    def self.commands
      VALID_COMMANDS
    end
  end
end
"#,
    ),
    (
        "app/models/yarn/expected.rb",
        r#"module Yarn
  module Expected
    VALUE = [
      ["which", "yarn"],
      ["yarn", "run", "speedscope", /.*\.json/],
      ["git", "clone", "https://github.com/tenderlove/profiler"],
    ]

    def self.commands
      VALUE
    end
  end
end
"#,
    ),
];

struct Analyzed {
    constant_rounds: usize,
    command: Option<Ty>,
    with_profiler: Option<Ty>,
    expected: Option<Ty>,
}

fn analyzed(with_profiler: &str) -> Analyzed {
    let tree = BASE
        .iter()
        .copied()
        .chain([("app/models/yarn/with_profiler.rb", with_profiler)])
        .map(|(path, src)| (PathBuf::from(path), src.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("fixture ingests");
    let mut analyzer = Analyzer::new(&app);
    analyzer.analyze(&mut app);
    let commands = |class: &str| {
        analyzer
            .class_registry()
            .get(&ClassId(Symbol::from(class)))
            .and_then(|c| c.class_methods.get(&Symbol::from("commands")))
            .cloned()
    };
    Analyzed {
        constant_rounds: analyzer.constant_rounds(),
        command: commands("Yarn::Command"),
        with_profiler: commands("Yarn::WithProfiler"),
        expected: commands("Yarn::Expected"),
    }
}

const WITH_PROFILER: &str = r#"module Yarn
  module WithProfiler
    include Command

    VALID_COMMANDS = [
      *VALID_COMMANDS,
      ["git", "clone", "https://github.com/tenderlove/profiler"],
    ]
    private_constant(:VALID_COMMANDS)

    def self.commands
      VALID_COMMANDS
    end
  end
end
"#;

#[test]
fn a_redefinition_splatting_the_included_value_types_as_their_union() {
    let a = analyzed(WITH_PROFILER);
    let command = a.command.expect("Yarn::Command.commands is typed");
    assert!(matches!(command, Ty::Array { .. }), "{command:?}");
    let expected = a.expected.expect("Yarn::Expected.commands is typed");
    assert_eq!(a.with_profiler.as_ref(), Some(&expected), "the included value's elements plus the new one");
    assert!(a.constant_rounds <= 6, "the registry settles in a few rounds, took {}", a.constant_rounds);
}

#[test]
fn a_reference_in_a_block_of_the_value_reads_the_bound_constant() {
    // The block runs whenever `LOOKUP` is called, after the write binds:
    // its `LOOKUP` is the hash itself, not an ancestor's.
    let a = analyzed(
        r#"module Yarn
  module WithProfiler
    include Command

    VALID_COMMANDS = Hash.new { |hash, key| VALID_COMMANDS.size }

    def self.commands
      VALID_COMMANDS
    end
  end
end
"#,
    );
    let with_profiler = a.with_profiler.expect("Yarn::WithProfiler.commands is typed");
    assert!(matches!(with_profiler, Ty::Hash { .. }), "{with_profiler:?}");
    assert!(a.constant_rounds <= 6, "the registry settles in a few rounds, took {}", a.constant_rounds);
}
