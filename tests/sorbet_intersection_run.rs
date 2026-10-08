//! `T.all(A, B, ...)` (api_versioning 4.6.0's config.rb:
//! `ClientType = T.type_alias { T.all(Class, ApiClient::ClassMethods[T.untyped]) }`,
//! then `T.let(nil, T.nilable(ClientType))`) on the CRuby tree, against
//! the same program run natively on Core's sorbet-runtime (CORE_GEM_HOME,
//! `tests/support/rails_oracle.rb`).
//!
//! sorbet-runtime 0.6.13502's `T::Types::Intersection#valid?` passes a
//! value every member passes; its name is `T.all(...)` over the members'
//! names, sorted; nested intersections flatten. A user generic's `[]` is
//! the module itself (`T::Generic#[]`). `ingest::sorbet_check` builds
//! `SorbetRuntimeCheck.all(...)`. Every observation, message included,
//! agrees with the native run.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const LIB: &str = r##"module Client
  module ClassMethods
    extend T::Generic

    Concrete = type_member { { upper: Client } }

    def client_label
      "client"
    end
  end
end

class RealClient
  include Client
  extend Client::ClassMethods
end

class Config
  ClientType = T.type_alias { T.all(Class, Client::ClassMethods[T.untyped]) }
  LazyClientType = T.type_alias do
    T.nilable(T.any(T.proc.returns(ClientType), ClientType))
  end

  def initialize(values)
    @values = values
    @client_cache = T.let(nil, T.nilable(ClientType))
  end

  def client_cache
    @client_cache
  end

  def comparable_integer
    T.let(@values.first, T.all(Comparable, Integer))
  end

  def client
    T.cast(@values.first, ClientType)
  end

  def nested
    T.let(@values.first, T.nilable(T.all(Kernel, T.all(Comparable, Numeric))))
  end

  def mixed
    T.assert_type!(@values.first, T.any(T.all(Comparable, String), Symbol))
  end

  def client_label
    T.cast(@values.first, ClientType).client_label
  end
end
"##;

/// One observation per line, `label: value`; an exception is its class
/// and its message, the sorbet `Caller:` path from `lib/` on and an
/// object hash masked (it differs from run to run).
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.sub(%r{Caller: .*/(lib/config\.rb:\d+)\z}, 'Caller: \1').gsub(/with hash -?\d+/, "with hash H")]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = {
  "nil" => [], "1" => [1], "1.5" => [1.5], "str" => ["str"], "sym" => [:sym],
  "client" => [RealClient], "class" => [String], "module" => [Client::ClassMethods],
}
%i[comparable_integer client nested mixed].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { v = Config.new(values).public_send(m); v.is_a?(Module) ? v.name : v } }
end
seen("cache") { Config.new([]).client_cache }
seen("client label") { Config.new([RealClient]).client_label }
"##;

const LINES: usize = 4 * 8 + 2;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/config.rb", LIB)
}

/// The program on Core's sorbet-runtime, with the library at
/// `lib/config.rb` as in the app.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_intersection_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/config.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         abort \"native: sorbet-runtime is not 0.6.13502\" unless Gem.loaded_specs[\"sorbet-runtime\"].version.to_s == \"0.6.13502\"\n\
         require_relative \"lib/config\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn intersections_check_like_the_native_run() {
    let native = native(SCRIPT);
    let native_lines: Vec<&str> = native.lines().collect();
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    for expected in [
        "comparable_integer 1: 1",
        "comparable_integer 1.5: [\"TypeError\", \"T.let: Expected type T.all(Comparable, Integer), got type Float with value 1.5\\nCaller: lib/config.rb:34\"]",
        "client client: \"RealClient\"",
        "cache: nil",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    let emitted_lines: Vec<&str> = emitted.stdout.lines().collect();
    assert_eq!(emitted_lines.len(), LINES, "emitted:\n{}", emitted.stdout);
    for (e, n) in emitted_lines.iter().zip(&native_lines) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
}
