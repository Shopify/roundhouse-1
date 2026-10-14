//! A module body's calls on constants an app module assigns other
//! constants to run as written when the module loads.
//!
//! Core's `Metafields::Types` has `AntennaGainType =
//! ShopifyMetafields::Types::AntennaGainType` (a gem's class), and
//! `Metafields::FilterKind` extends one of them with an
//! `attr_accessor` module and writes the accessor for each:
//! `Types::Type.extend(Classification)`, `Types::DateType.filter_kind =
//! :date`. The emitted module dropped the statements, so
//! `Metafields::FilterKind.type_kinds` found no `filter_kind` on any type.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ALIASES: &str = r##"module Cs
  module Types
    Type = ::Object
    TextType = ::String
    ListType = ::Array
  end
end
"##;

const FILTER_KIND: &str = r##"module Cs
  module FilterKind
    module Classification
      #: Symbol?
      attr_accessor :filter_kind
    end

    Types::Type.extend(Classification)

    Types::TextType.filter_kind = :text
    Types::ListType.filter_kind = :list

    class << self
      #: (Class) -> Symbol?
      def for(type)
        type.filter_kind
      end

      #: -> Hash[String, Symbol]
      def type_kinds
        @type_kinds ||= [Types::TextType, Types::ListType].each_with_object({}) do |type, mapping|
          kind = type.filter_kind
          mapping[type.name] = kind if kind
        end.freeze
      end
    end
  end
end
"##;

const SCRIPT: &str = "p Cs::FilterKind.type_kinds\np Cs::FilterKind.for(String)\n";

fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh_alias_constant_body_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.rb"), format!("{ALIASES}{FILTER_KIND}{SCRIPT}")).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    assert!(out.status.success(), "native failed:\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_module_body_calls_on_alias_constants_run_as_written() {
    let expected = native();
    assert_eq!(expected.lines().collect::<Vec<_>>(), ["{\"String\" => :text, \"Array\" => :list}", ":text"], "native control");
    let run = emit_and_run::real_blog()
        .write("app/models/cs/types.rb", ALIASES)
        .write("app/models/cs/filter_kind.rb", FILTER_KIND)
        .run_ruby(SCRIPT);
    assert!(run.success, "emitted program failed:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), expected.lines().collect::<Vec<_>>());
}
