//! A class body statement below a constant that builds the class's own
//! instances runs after that constant, as the class body does.
//!
//! Core's `Attribution::AttributionConfig` defines its readers and
//! `initialize`, then `CONFIGS = { DEFAULT => new(…), … }.freeze`, then
//! guards it: `raise "…" unless CONFIGS.key?(DEFAULT)`. The constant
//! calls `new`, so the emit writes it after the defs (a deferred
//! constant); the guard was written where its source position falls
//! among the defs, above that tail, and every test loading the class
//! failed in setup with `uninitialized constant
//! Attribution::AttributionConfig::CONFIGS`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONFIG: &str = r##"module Tally
  class Config
    DEFAULT = "default"
    ALWAYS = :always

    attr_reader :name, :mode

    def initialize(name:, mode: ALWAYS)
      @name = name
      @mode = mode
    end

    def always?
      mode == ALWAYS
    end

    CONFIGS = {
      DEFAULT => new(name: DEFAULT),
      "relaxed" => new(name: "relaxed", mode: :never),
    }.freeze

    raise "Config::CONFIGS must always define #{DEFAULT}" unless CONFIGS.key?(DEFAULT)

    class << self
      def from_name(name)
        CONFIGS[name]
      end

      def names
        CONFIGS.keys
      end
    end
  end
end
"##;

const OBSERVE: &str = r##"
puts Tally::Config.names.inspect
puts "#{Tally::Config.from_name("default").always?} #{Tally::Config.from_name("relaxed").always?}"
puts Tally::Config::CONFIGS.frozen?
"##;

#[test]
fn a_guard_below_a_self_constructing_constant_runs_after_it() {
    let dir = std::env::temp_dir().join(format!("rh_tally_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.rb"), CONFIG).unwrap();
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("load {:?}\n{OBSERVE}", dir.join("config.rb").to_str().unwrap()))
        .output()
        .expect("CRuby control");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(native.status.success(), "native: {}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "[\"default\", \"relaxed\"]\ntrue false\ntrue\n", "native control");

    let run = emit_and_run::empty_app().write("app/models/tally/config.rb", CONFIG).run_ruby(OBSERVE);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/tally/config.rb")).expect("emitted config.rb");
    let configs = emitted.find("CONFIGS = ").expect("CONFIGS emitted");
    let guard = emitted.find("CONFIGS.key?").expect("the guard emitted");
    assert!(configs < guard, "the guard runs before CONFIGS:\n{emitted}");
    assert!(
        run.success,
        "the emitted program failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}\nerrors: {:#?}",
        run.emitted.display(),
        run.stdout,
        run.stderr,
        run.errors
    );
    assert_eq!(run.stdout, expected, "emitted Tally::Config diverges from Ruby");
}
