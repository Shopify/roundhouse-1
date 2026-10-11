//! A gem compiled as source runs its class bodies as its Ruby does.
//!
//! worldwide 1.29.0 (Core's first fatal at boot once it is compiled
//! rather than loaded natively) wrote, in `Worldwide::Currencies`:
//!
//! ```ruby
//! def numeric_code_for(alpha_code) ... end
//! ALL_CURRENCIES = currencies["en"]["currencies"].map { |code, _| Currency.new(code: code) }
//! ALL_CURRENCIES.sort_by!(&:currency_code)
//! ALL_CURRENCIES.freeze
//! ```
//!
//! and `Currency#initialize` calls `Currencies.numeric_code_for`. The
//! emit hoisted ALL_CURRENCIES above the defs (`undefined method
//! 'numeric_code_for' for module Worldwide::Currencies` at load) and
//! dropped the two statements after it, so `Currencies.all` kept the
//! CLDR file's order, unfrozen. `Locales`'s `class << self; include
//! Enumerable; alias_method :known, :to_a` was dropped too
//! (`Locales.known` NoMethodError), as was `Calendar`'s
//! `FIRST_DAY_DATA.freeze`. And `Plurals` evals the gem's
//! `data/cldr/locales/*/plurals.rb`, Ruby the emitter did not carry
//! because it skipped every `.rb` of a gem source as compiled code.
//!
//! `Orbis` here is that shape, small, carried as a gem source
//! (`gem_sources:`); the test requires the emitted tree to answer what
//! the same files answer natively.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    orbis (1.0.0)\n\nDEPENDENCIES\n  orbis\n";

const FILES: &[(&str, &str)] = &[
    (
        "lib/orbis.rb",
        "require_relative \"orbis/paths\"\nrequire_relative \"orbis/currency\"\nrequire_relative \"orbis/currencies\"\nrequire_relative \"orbis/locales\"\nrequire_relative \"orbis/calendar\"\nrequire_relative \"orbis/plurals\"\n",
    ),
    (
        "lib/orbis/paths.rb",
        "module Orbis\n  module Paths\n    GEM_ROOT = File.expand_path(File.join(__dir__, \"../..\"))\n    DATA_ROOT = File.join(GEM_ROOT, \"data\")\n  end\nend\n",
    ),
    (
        "lib/orbis/currency.rb",
        "module Orbis\n  class Currency\n    attr_reader :code, :numeric\n\n    def initialize(code:)\n      @code = code\n      @numeric = Currencies.numeric_code_for(code)\n    end\n  end\nend\n",
    ),
    (
        "lib/orbis/currencies.rb",
        "module Orbis\n  module Currencies\n    include Enumerable\n\n    extend self\n\n    CODES = { \"USD\" => 840, \"EUR\" => 978, \"CAD\" => 124 }.freeze\n\n    def each(&block)\n      ALL.each(&block)\n    end\n\n    def numeric_code_for(code)\n      CODES[code]\n    end\n\n    ALL = CODES.keys.map do |code|\n      Currency.new(code: code)\n    end\n    ALL.sort_by!(&:code)\n    ALL.freeze\n  end\nend\n",
    ),
    (
        "lib/orbis/locales.rb",
        "module Orbis\n  module Locales\n    class << self\n      include Enumerable\n\n      alias_method :known, :to_a\n\n      def each(&block)\n        %i[fr en de].each(&block)\n      end\n    end\n  end\nend\n",
    ),
    (
        "lib/orbis/calendar.rb",
        "module Orbis\n  module Calendar\n    FIRST = { US: 0, FR: 1 }.each_with_object({}) do |(code, day), memo|\n      memo[code] = day\n    end\n    FIRST.freeze\n\n    class << self\n      def first_week_day(code)\n        FIRST.fetch(code.to_sym, 1)\n      end\n\n      def table_frozen?\n        FIRST.frozen?\n      end\n    end\n  end\nend\n",
    ),
    (
        "lib/orbis/plurals.rb",
        "module Orbis\n  module Plurals\n    def self.rule(locale, n)\n      path = File.join(Paths::DATA_ROOT, \"locales\", locale.to_s, \"plurals.rb\")\n      return unless File.exist?(path)\n\n      eval(File.read(path)).dig(locale, :rule).call(n)\n    end\n  end\nend\n",
    ),
    ("data/locales/fr/plurals.rb", "{ fr: { rule: lambda { |n| n < 2 ? :one : :other } } }\n"),
];

const OBSERVE: &str = r#"
puts Orbis::Currencies.map { |c| [c.code, c.numeric] }.inspect
puts "frozen=#{Orbis::Currencies::ALL.frozen?}"
puts Orbis::Locales.known.inspect
puts "include=#{Orbis::Locales.include?(:en)} first=#{Orbis::Locales.first}"
puts "week=#{Orbis::Calendar.first_week_day("US")} #{Orbis::Calendar.first_week_day("ZZ")} frozen=#{Orbis::Calendar.table_frozen?}"
puts "plural=#{Orbis::Plurals.rule(:fr, 1).inspect} #{Orbis::Plurals.rule(:fr, 5).inspect} #{Orbis::Plurals.rule(:de, 1).inspect}"
"#;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn a_gem_source_body_runs_in_source_order_like_the_gem() {
    let gem = std::env::temp_dir().join(format!("rh_orbis_native_{}", std::process::id()));
    for (path, text) in FILES {
        let file = gem.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby().arg("-I").arg(gem.join("lib")).args(["-rorbis", "-e", OBSERVE]).output().expect("CRuby control");
    assert!(native.status.success(), "native orbis: {}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&String::from_utf8_lossy(&native.stdout));
    std::fs::remove_dir_all(&gem).unwrap();
    assert_eq!(
        expected,
        [
            "[[\"CAD\", 124], [\"EUR\", 978], [\"USD\", 840]]",
            "frozen=true",
            "[:fr, :en, :de]",
            "include=true first=fr",
            "week=0 1 frozen=true",
            "plural=:one :other nil",
        ],
        "native control"
    );

    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "gem_sources:\n  orbis: vendor/gems/orbis-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("config/application.rb", "require \"orbis\"\n");
    for (path, text) in FILES {
        app = app.write(&format!("vendor/gems/orbis-1.0.0/{path}"), text);
    }
    let run = app.run_ruby(OBSERVE);
    let currencies = std::fs::read_to_string(run.emitted.join("app/models/orbis/currencies.rb")).expect("emitted currencies.rb");
    let def = currencies.find("def numeric_code_for").expect("numeric_code_for emitted (an `extend self` def, written as in the source)");
    let all = currencies.find("ALL = ").expect("ALL emitted");
    let sort = currencies.find("ALL.sort_by!").expect("ALL.sort_by! emitted");
    let freeze = currencies.find("ALL.freeze").expect("ALL.freeze emitted");
    assert!(def < all && all < sort && sort < freeze, "source order lost:\n{currencies}");
    assert!(
        run.emitted.join("vendor/gems/orbis-1.0.0/data/locales/fr/plurals.rb").is_file(),
        "the gem's data Ruby is not carried"
    );
    assert!(!run.emitted.join("vendor/gems/orbis-1.0.0/lib/orbis.rb").exists(), "the gem's code is compiled, not copied");
    assert!(
        run.success,
        "the emitted program failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}\nerrors: {:#?}",
        run.emitted.display(),
        run.stdout,
        run.stderr,
        run.errors
    );
    assert_eq!(lines(&run.stdout), expected, "emitted Orbis diverges from the gem");
}

/// The same statements in an app's own module are carried too
/// (e05b798bd, `carries_body_statement`): Ruby runs a module body's
/// statements in source order whoever wrote the module, so `ROWS.sort!`
/// sorts the constant it follows.
#[test]
fn an_app_modules_body_statements_run_in_source_order() {
    let run = emit_and_run::empty_app()
        .write("app/models/table.rb", "module Table\n  ROWS = [3, 1, 2]\n  ROWS.sort!\nend\n")
        .run_ruby("puts Table::ROWS.inspect\n");
    let table = std::fs::read_to_string(run.emitted.join("app/models/table.rb")).expect("emitted table.rb");
    let rows = table.find("ROWS = [3, 1, 2]").expect("ROWS emitted");
    let sort = table.find("ROWS.sort!").expect("the body statement is carried");
    assert!(rows < sort, "source order lost:\n{table}");
    assert!(run.success, "the emitted program failed:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.trim(), "[1, 2, 3]", "the statement did not run:\n{table}");
}

fn core_gem_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CORE_GEM_HOME").unwrap_or_else(|_| {
        "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into()
    }))
}

fn jemalloc() -> String {
    std::env::var("RH_JEMALLOC")
        .unwrap_or_else(|_| "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into())
}

/// A class or module a natively loaded gem opens (i18n's `I18n::Config`)
/// is a class object to `check`: worldwide 1.29.0's cldr.rb asks
/// `I18n::Config.method_defined?(:owned_by?)` at load, which `check`
/// refused as the Module protocol on a non-class value.
#[test]
fn a_native_gems_class_answers_the_module_protocol() {
    let home = core_gem_home();
    assert!(home.join("gems/i18n-1.15.2").is_dir(), "i18n 1.15.2 is not in Core's gem home: {} (set CORE_GEM_HOME)", home.display());
    let lock = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n    orbis (1.0.0)\n      i18n\n\nDEPENDENCIES\n  orbis\n";
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("gem_home: {}\ngem_sources:\n  orbis: vendor/gems/orbis-1.0.0\n", home.display()))
        .write("Gemfile.lock", lock)
        .write("config/application.rb", "require \"orbis\"\n")
        .write(
            "vendor/gems/orbis-1.0.0/lib/orbis.rb",
            "require \"i18n\"\n\nmodule Orbis\n  OWNED = I18n::Config.method_defined?(:owned_by?)\n\n  def self.answers\n    [OWNED, I18n::Config.method_defined?(:locale), I18n::Config.instance_method(:locale).owner]\n  end\nend\n",
        )
        // Core's gem home links its C extensions against jemalloc
        // (tests/support/rails_oracle.rb's `core_gems_env`).
        .run_ruby_env("puts Orbis.answers.inspect\n", &[("GEM_PATH", home.to_str().unwrap()), ("LD_PRELOAD", &jemalloc())]);
    let protocol: Vec<&String> = run.errors.iter().filter(|e| e.contains("method_defined?") || e.contains("Module")).collect();
    assert!(protocol.is_empty(), "{protocol:#?}");
    assert!(run.success, "emitted program failed in {}\n{}\n{}\n{:#?}", run.emitted.display(), run.stdout, run.stderr, run.errors);
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &home)
        .env("LD_PRELOAD", jemalloc())
        .args(["-e", "gem \"i18n\", \"1.15.2\"; require \"i18n\"; p [I18n::Config.method_defined?(:owned_by?), I18n::Config.method_defined?(:locale), I18n::Config.instance_method(:locale).owner]"])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "native i18n: {}", String::from_utf8_lossy(&native.stderr));
    let native = String::from_utf8_lossy(&native.stdout).trim().to_string();
    assert_eq!(native, "[true, true, I18n::Config]", "native control");
    assert_eq!(run.stdout.trim(), native, "emitted Orbis diverges from i18n natively");
}
