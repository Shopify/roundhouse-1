//! A module reopened across files keeps the authored order of its
//! constants.
//!
//! `card.rb` defines `Card::EMPTY`, then requires `card/errors`, whose
//! reopen of `module Card` builds `UNREPORTED = Err.new(...)`. `Err`'s
//! `initialize` reads `Card::EMPTY` through a default argument, so the
//! reopen's constant must run after the primary file's. The walk sorts
//! `card/errors.rb` before `card.rb` (a path component `card` sorts before
//! `card.rb`), and the merged module used to list `UNREPORTED` first:
//! `NameError: uninitialized constant Card::EMPTY` at load.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CARD: &str = r#"module Card
  EMPTY = [].freeze
  LABEL = "card".freeze
end

require_relative "card/errors"
require_relative "card/version"
"#;

const ERRORS: &str = r#"module Card
  class Err < StandardError
    attr_reader :nodes

    def initialize(msg = nil, nodes: Card::EMPTY)
      super(msg)
      @nodes = nodes
    end
  end

  UNREPORTED = Err.new("x").freeze
end
"#;

const VERSION: &str = r##"module Card
  VERSION = "#{LABEL}-1.0"
end
"##;

#[test]
fn a_reopen_constant_runs_after_the_primary_files_constants() {
    let run = emit_and_run::empty_app()
        .write("app/models/card.rb", CARD)
        .write("app/models/card/errors.rb", ERRORS)
        .write("app/models/card/version.rb", VERSION)
        .run_ruby("p Card::UNREPORTED.nodes\np Card::VERSION\n");
    run.assert_passes();
    assert_eq!(run.stdout, "[]\n\"card-1.0\"\n");
}

/// A require written ABOVE the module body loads the required file first,
/// so its constants come first, as Ruby runs them.
const LEADING: &str = r#"require_relative "leading/base"

module Leading
  DOUBLE = BASE * 2
end
"#;

const LEADING_BASE: &str = r#"module Leading
  BASE = 21
end
"#;

#[test]
fn a_leading_require_runs_the_required_files_constants_first() {
    let run = emit_and_run::empty_app()
        .write("app/models/leading.rb", LEADING)
        .write("app/models/leading/base.rb", LEADING_BASE)
        .run_ruby("p Leading::DOUBLE\n");
    run.assert_passes();
    assert_eq!(run.stdout, "42\n");
}
