//! A module body's `begin; require "lib"; … rescue LoadError; end`, which
//! Ruby runs where it stands when the body loads.
//!
//! paquito 1.0.2's `Paquito::Types` appends its oversized-Integer type to
//! `TYPES` this way, between the literal and `TYPES.freeze`:
//!
//!     begin
//!       require "msgpack/bigint"
//!       TYPES << { code: 10, class: "Integer", … }
//!     rescue LoadError
//!     end
//!
//! The body-statement carrier refused it because `require` was not among
//! the calls any class answers, so the emitted module lacked the type and
//! every `Paquito::Types.register(factory, [Integer, …])` raised
//! `KeyError: No type found for "Integer" with format_version=1` (Core's
//! `VersionedMemcacheCoder` from `TestCacheStore#write_entry`). The
//! emitted tree must print what CRuby prints: a library the process can
//! load is required and the append runs; a missing one is rescued. The
//! `LIST.freeze` below the statements, a call on a constant that a plain
//! module body drops (`warning[lower_residue]`), no longer keeps them
//! from their place.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const REGISTRY: &str = r#"module Registry
  #: (untyped) -> String
  def self.dump(value)
    value.to_s
  end

  LIST = [
    { name: "Symbol", dump: method(:dump) }.freeze,
  ]
  begin
    require "json"

    LIST << { name: "Integer", dump: JSON.method(:generate) }
  rescue LoadError
    # an older Ruby
  end
  begin
    require "roundhouse_no_such_library"

    LIST << { name: "Never", dump: method(:dump) }
  rescue LoadError
    LIST << { name: "Rescued", dump: method(:dump) }
  end
  LIST.freeze

  #: (String) -> Hash[Symbol, untyped]
  def self.find(name)
    LIST.find { |t| t[:name] == name } || raise(KeyError, "No type found for #{name.inspect}")
  end
end
"#;

const SCRIPT: &str = r#"puts Registry::LIST.map { |t| t[:name] }.join(",")
puts Registry.find("Integer")[:dump].call([1, 2])
puts Registry.find("Symbol")[:dump].call(:sym)
"#;

const EXPECTED: &[&str] = &["Symbol,Integer,Rescued", "[1,2]", "sym"];

#[test]
fn a_module_body_begin_requiring_a_library_runs_in_place_as_in_cruby() {
    let native = emit_and_run::ruby().arg("-e").arg(format!("{REGISTRY}\n{SCRIPT}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let lines = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(lines(&String::from_utf8_lossy(&native.stdout)), EXPECTED, "native control");

    let run = emit_and_run::empty_app().write("lib/registry.rb", REGISTRY).run_ruby(SCRIPT);
    assert!(!run.stderr.contains("file_statement_not_carried"), "{}", run.stderr);
    run.assert_passes();
    assert_eq!(lines(&run.stdout), EXPECTED, "emitted Ruby diverges from CRuby\n{}", run.stderr);
}
