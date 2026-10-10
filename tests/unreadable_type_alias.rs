//! A sorbet type alias with a member no check reads, Core's
//! `Observe::AllowedValueType` (components/platform/essentials/lib/observe.rb):
//!
//! ```ruby
//! AllowedValueType = T.type_alias do
//!   T.any(String, Symbol, ..., T::Enum, ...)
//! end
//!
//! def with_exception_attributes(exception, ...)
//!   formatted_attributes = {} #: Hash[AllowedKeyType, AllowedValueType]
//! ```
//!
//! `T::Enum` names sorbet-runtime's own class, which no tree loads, so the
//! alias was not carried at all, and the `#:` annotation (inert natively)
//! named an unknown constant: the emitted method replaced `{}` with a raise
//! ("declared type not supported: AllowedValueType has no indexed
//! declaration"), and every Observe.log_exception caller failed. The alias
//! now reads as untyped for annotations and signatures, while a sorbet
//! check that reads it still refuses, by name, at the alias.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIB: &str = r##"module Obs
  extend T::Sig
  Key = T.type_alias { T.any(String, Symbol) }
  Value = T.type_alias do
    T.any(
      Symbol,
      Integer,
      T::Enum,
    )
  end

  #: (Integer x) -> Hash[Key, Value]
  def self.attributes(x)
    formatted = {} #: Hash[Key, Value]
    formatted[:count] = x
    formatted
  end
end
"##;

const CHECKED: &str = r##"module Checked
  Value = T.type_alias { T.any(Integer, T::Enum) }

  def self.check(x)
    T.let(x, Value)
  end
end
"##;

fn app(lib: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app().write("app/.keep", "").write("lib/obs.rb", lib)
}

#[test]
fn an_annotation_naming_an_unreadable_alias_runs_the_authored_value() {
    let run = app(LIB).run_ruby("p Obs.attributes(3)\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "{count: 3}", "{}", run.stderr);
}

#[test]
fn a_check_reading_an_unreadable_alias_is_refused_by_name() {
    let run = app(CHECKED).run_ruby("begin\n  Checked.check(1)\nrescue => e\n  puts e.message\nend\n");
    assert!(
        run.errors.iter().any(|e| e.contains("sorbet type check") && e.contains("`T::Enum` names sorbet-runtime's own constant")),
        "{:?}",
        run.errors
    );
    assert!(
        run.stdout.contains("roundhouse: sorbet type check not supported"),
        "stdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}
