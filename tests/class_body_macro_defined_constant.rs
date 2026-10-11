//! A constant whose value reads a class method that a macro call earlier in
//! the same body defines (`enum :status, …` then `VALID = statuses`, Rails
//! `ActiveRecord::Enum#enum` defining the plural class method) must be
//! written after the macro call runs, not at the top of the body with the
//! other eager constants.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MACRO: &str = r##"module Listable
  #: (Symbol, Array[Symbol], **untyped) -> void
  def listable(column, possible_values, **options)
    singleton_class.send(:define_method, column.to_s.pluralize) do
      possible_values
    end
  end
end
"##;

const HOLDER: &str = r##"module Shelf
  class Holder
    extend Listable

    OPEN = :open
    CLOSED = :closed

    listable :status, [
      OPEN,
      CLOSED,
    ]

    VALID = statuses #: Array[Symbol]
    FINAL = [CLOSED].freeze #: Array[Symbol]
  end
end
"##;

#[test]
fn a_constant_reading_a_macro_defined_class_method_is_written_after_the_macro_call() {
    let run = emit_and_run::empty_app()
        .write("lib/listable.rb", MACRO)
        .write("lib/shelf/holder.rb", HOLDER)
        .run_ruby("p Shelf::Holder::VALID\np Shelf::Holder.statuses\n");
    assert!(run.success, "emitted tree failed in {}: {}\nerrors: {:#?}", run.emitted.display(), run.stderr, run.errors);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["[:open, :closed]", "[:open, :closed]"], "{}", run.stderr);
}
