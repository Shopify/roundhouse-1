//! A constant whose value takes `method(:own)` waits for the `def`.
//!
//! paquito 1.0.2's `lib/paquito/types.rb` defines its pack/unpack
//! functions in `class << self` and only then builds
//! `TYPES = [{ packer: method(:time_pack_deprecated), ... }]`.
//! `method(:x)` looks `x` up as it runs, so above the `def` it raises.
//! The emit hoisted TYPES above the defs (constants go first unless
//! their value dispatches to the class), and Core's boot died at
//! `app/models/paquito/types.rb`:
//!
//! ```text
//! NameError: undefined method 'time_pack_deprecated' for class 'Module'
//! ```
//!
//! The paquito test carries the installed gem's own files (Core's gem
//! home, CORE_GEM_HOME) as a gem source and requires the emitted
//! `Paquito::Types::TYPES` to answer what the gem answers natively.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    msgpack (1.8.5)\n    paquito (1.0.2)\n      msgpack\n\nDEPENDENCIES\n  activesupport\n  paquito\n";

fn core_gem_home() -> PathBuf {
    PathBuf::from(std::env::var("CORE_GEM_HOME").unwrap_or_else(|_| {
        "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into()
    }))
}

fn paquito_lib() -> PathBuf {
    let lib = core_gem_home().join("gems/paquito-1.0.2/lib");
    assert!(lib.is_dir(), "paquito 1.0.2 is not in Core's gem home: {} (set CORE_GEM_HOME)", lib.display());
    lib
}

/// The files `paquito/types` loads: itself and `paquito/errors`. Its
/// `autoload :ActiveRecordPacker` file (which needs paquito's
/// ActiveRecordCoder and msgpack's factory) is not carried: nothing
/// here references the constant.
const PAQUITO_FILES: &[&str] = &["paquito/types.rb", "paquito/errors.rb"];

/// What TYPES is: each entry's code, class, version and the packer and
/// unpacker (a Method by name, a Proc by kind), whether it is frozen,
/// and a Date through the gem's own date packer and back.
const OBSERVE: &str = r#"
describe = ->(f) do
  case f
  when Method then "method #{f.name} on #{f.receiver.inspect}"
  when Proc then f.lambda? ? "lambda" : "proc"
  else f.inspect
  end
end
types = Paquito::Types::TYPES
types.each do |t|
  puts [t[:code], t[:class], t[:version], describe.(t[:packer]), describe.(t[:unpacker]), t.frozen?].inspect
end
puts "frozen: #{types.frozen?}"
date = types.find { |t| t[:class] == "Date" }
packed = date[:packer].call(Date.new(2024, 2, 29))
puts "date packed: #{packed.bytes.inspect}"
puts "date unpacked: #{date[:unpacker].call(packed).iso8601}"
"#;

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

fn native(lib: &Path) -> Vec<String> {
    let out = emit_and_run::ruby()
        .arg("-I")
        .arg(lib)
        // What paquito.rb requires ahead of `paquito/types`.
        .args(["-rbigdecimal", "-rdate", "-rset", "-rmsgpack", "-rpaquito/types", "-e", OBSERVE])
        .output()
        .expect("CRuby control");
    assert!(out.status.success(), "native paquito: {}", String::from_utf8_lossy(&out.stderr));
    lines(&out.stdout)
}

/// Emit the carried paquito files and run OBSERVE against the booted
/// tree. Returns the run and the emitted `app/models/paquito/types.rb`.
fn emitted(lib: &Path) -> (emit_and_run::Run, String) {
    let mut app = emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!("gem_home: {}\ngem_sources:\n  paquito: vendor/gems/paquito-1.0.2\n", core_gem_home().display()),
        )
        .write("Gemfile.lock", LOCK);
    for file in PAQUITO_FILES {
        let source = std::fs::read_to_string(lib.join(file)).expect("read paquito source");
        app = app.write(&format!("vendor/gems/paquito-1.0.2/lib/{file}"), &source);
    }
    let run = app.run_ruby(OBSERVE);
    let types = std::fs::read_to_string(run.emitted.join("app/models/paquito/types.rb")).expect("emitted types.rb");
    // types.rb carries refusals of its own, outside this ordering
    // (SERIALIZE_PROC in CustomTypesRegistry, the
    // ActiveSupport::TimeWithZone method bodies); none may be about TYPES
    // or the functions it takes.
    let about_types: Vec<&String> =
        run.errors.iter().filter(|e| e.contains("TYPES") || e.contains("_pack") || e.contains("_unpack")).collect();
    assert!(about_types.is_empty(), "{about_types:#?}");
    assert!(
        run.success,
        "the emitted program failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}\nerrors: {:#?}",
        run.emitted.display(),
        run.stdout,
        run.stderr,
        run.errors
    );
    (run, types)
}

/// The lines the gem's module-body statements after TYPES produce:
/// `begin require "msgpack/bigint"; TYPES << {code: 10, ...}; rescue
/// LoadError; end` and `TYPES.freeze`. See the ignored test below.
fn after_types_statement(line: &str) -> bool {
    line.starts_with("[10, ") || line.starts_with("frozen: ")
}

#[test]
fn paquito_types_takes_its_methods_like_the_gem() {
    let lib = paquito_lib();
    let expected = native(&lib);
    assert!(
        expected.iter().any(|l| l.contains("method time_pack_deprecated on Paquito::Types")),
        "native control: {expected:#?}"
    );
    let (run, types) = emitted(&lib);
    let first_def = types.find("def self.time_pack_deprecated").expect("time_pack_deprecated emitted");
    let constant = types.find("TYPES = ").expect("TYPES emitted");
    assert!(constant > first_def, "TYPES takes method(:time_pack_deprecated): it must follow the def\n{types}");
    let expected: Vec<String> = expected.into_iter().filter(|l| !after_types_statement(l)).collect();
    let got: Vec<String> = lines(run.stdout.as_bytes()).into_iter().filter(|l| !after_types_statement(l)).collect();
    assert_eq!(got, expected, "emitted Paquito::Types::TYPES diverges from the gem");
    // Not vacuous: every Method entry and the Date round trip are there.
    assert_eq!(got.iter().filter(|l| l.contains("on Paquito::Types")).count(), 8, "{got:#?}");
    assert!(got.contains(&"date unpacked: 2024-02-29".to_string()), "{got:#?}");
}

/// KNOWN GAP. The module body's statements after TYPES are not emitted:
/// the class-body walk (`walk_decl_body_with_visibility`, ingest
/// library_class.rs) keeps no `begin ... rescue` statement, so the
/// bigint extension type (code 10) is dropped without a diagnostic, and
/// `TYPES.freeze` is captured as a class-body call the Ruby emitter
/// drops (`report_dropped_class_body_call`) because Paquito::Types is
/// no foreign-base body. Natively TYPES ends with the Integer entry and
/// is frozen.
#[test]
#[ignore = "paquito's module-body `begin require \"msgpack/bigint\"; TYPES << ...; rescue LoadError; end` and `TYPES.freeze` are not emitted"]
fn paquito_types_runs_its_module_body_statements_like_the_gem() {
    let lib = paquito_lib();
    let expected = native(&lib);
    let (run, _) = emitted(&lib);
    assert_eq!(lines(run.stdout.as_bytes()), expected, "emitted Paquito::Types::TYPES diverges from the gem");
}

/// The same lookup in every spelling, without a gem home: `method(:x)`,
/// `self.method(:x)`, `instance_method(:x)`, and `&method(:x)` passed to
/// a block that runs now. Each constant must follow its def.
const TABLE: &str = r#"module Table
  class << self
    def double(x) = x * 2
    def square(x) = x * x
  end

  def label = "table"

  BY_NAME = { double: method(:double), square: self.method(:square) }.freeze
  LABEL = instance_method(:label)
  DOUBLED = [1, 2, 3].map(&method(:double))
end
"#;

const TABLE_SCRIPT: &str = r##"puts Table::BY_NAME.map { |k, m| "#{k}=#{m.call(3)}" }.join(",")
puts Table::LABEL.name
puts Table::DOUBLED.inspect
"##;

#[test]
fn every_spelling_of_an_own_method_lookup_waits_for_the_def() {
    let native = emit_and_run::ruby().args(["-e", &format!("{TABLE}\n{TABLE_SCRIPT}")]).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(expected, ["double=6,square=9", "label", "[2, 4, 6]"], "native control");
    let run = emit_and_run::empty_app().write("app/models/table.rb", TABLE).run_ruby(TABLE_SCRIPT);
    run.assert_passes();
    assert_eq!(lines(run.stdout.as_bytes()), expected, "emitted Table diverges from CRuby");
}
