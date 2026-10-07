//! shopify-money 4.1.1, compiled as app code (`gem_sources:`), on the
//! CRuby tree, against CRuby running the installed gem directly.
//!
//! Core reaches the gem through `Bundler.require`, and the gem needs Rails
//! pieces, so the emitted tree carries its source (docs RUNTIME_GEMS.md).
//! The probe walks what Core leans on: `Money::Currency.find!` (an
//! `alias_method` of a `new` that calls `super`, behind `@@mutex` and
//! `@@loaded_currencies`), `Money.new` / `from_amount` and the
//! `@@zero_money` memo, arithmetic, `Money::NULL_CURRENCY`, `allocate`,
//! `split` (`Money::Splitter`'s `protected attr_writer(:split)`),
//! formatting, equality and `as_json`. Each line the emitted tree prints
//! must equal CRuby's, in order.
//!
//! The gem is not vendored here. Point `SHOPIFY_MONEY_GEM_DIR` at an
//! installed shopify-money-4.1.1 (the directory holding `lib/` and
//! `config/`) and run with `--ignored`. Without it the test fails rather
//! than skips. `MONEY_PROBE_DEBUG=1` prints the emitted run's output
//! before the assertions.
//!
//! STATUS: this test does not pass yet. The emitted tree boots and its
//! first four lines (Currency `find!`/`new`, `find`, `UnknownCurrency`)
//! equal CRuby's. `Money.new` then raises "unknown format: 'iso4217'":
//! the gem registers each converter with a file-level
//! `Money::Converters.register(…)` call, which the tree does not run
//! (reported as `file_statement_unsupported`, as is `deprecations.rb`'s
//! file-level `Money.class_eval do … end`). Analysis also still
//! refuses the probe's `as_json`. `Money::Helpers::DECIMAL_ZERO`
//! (`BigDecimal(0).freeze`), `Float::EPSILON` and `Rational` in
//! `Money::Allocator < SimpleDelegator`, and `Accounting#parse`'s
//! `super(..., **options)` now analyze; see
//! `kernel_conversion_constants_cruby` and `namespaced_superclass_cruby`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

const PROBE: &str = r##"require "money"

module MoneyProbe
  def self.run
    usd = Money::Currency.find!("usd")
    puts "currency: #{usd.iso_code} #{usd.name} minor=#{usd.minor_units} subunit=#{usd.subunit_to_unit}"
    puts "find! memoized: #{Money::Currency.find!("USD").equal?(usd)} new=#{Money::Currency.new("usd").equal?(usd)}"
    puts "find unknown: #{Money::Currency.find("zzz").inspect}"
    begin
      Money::Currency.find!("zzz")
    rescue Money::Currency::UnknownCurrency => e
      puts "find! unknown: #{e.class}: #{e.message}"
    end
    a = Money.new("10.005", "USD")
    b = Money.from_amount(2.5, "usd")
    puts "new: #{a.inspect} value=#{a.value.to_s("F")} subunits=#{a.subunits}"
    puts "from_amount: #{b.inspect} #{b.currency.iso_code}"
    puts "sum: #{(a + b).to_s} diff: #{(a - b).to_s} times: #{(b * 3).to_s} neg: #{(-b).to_s}"
    puts "compare: #{a > b} #{a == Money.new("10.01", "USD")} #{[a, b].max.to_s}"
    puts "zero memo: #{Money.new(0, "USD").equal?(Money.new(0, "USD"))} zero?=#{Money.new(0, "USD").zero?}"
    puts "null currency: #{Money.new(5, Money::NULL_CURRENCY).currency.class} #{Money::NULL_CURRENCY.iso_code.inspect} #{(Money.new(5, Money::NULL_CURRENCY) + a).to_s} #{(Money.new(5, Money::NULL_CURRENCY) + a).currency.iso_code}"
    puts "allocate: #{Money.new(10, "USD").allocate([Rational(1, 3), Rational(2, 3)]).map(&:to_s).inspect}"
    puts "split: #{Money.new(100, "USD").split(3).map(&:to_s).inspect}"
    puts "split first: #{Money.new(100, "USD").split(3).first.to_s} size=#{Money.new(100, "USD").split(3).size}"
    puts "subunits: #{Money.from_subunits(1234, "JPY").to_s} #{Money.from_subunits(1234, "USD").to_s}"
    puts "to_fs: #{a.to_fs(:legacy_dollars)} #{Money.new("1234.5", "EUR").to_s}"
    puts "round: #{Money.new("1.005", "USD").round.to_s} abs=#{Money.new(-3, "USD").abs.to_s}"
    puts "hash/eql: #{a.eql?(Money.new("10.01", "USD"))} #{{a => 1}.key?(Money.new("10.01", "USD"))}"
    puts "as_json: #{Money.new(1, "CAD").as_json.inspect} to_h=#{Money.new(1, "CAD").to_h.inspect}"
  end
end
"##;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    shopify-money (4.1.1)\n\nDEPENDENCIES\n  shopify-money\n";

const TEST: &str = r#"require "test_helper"
require "money_probe"
class MoneyProbeTest < ActiveSupport::TestCase
  test "the carried gem answers" do
    MoneyProbe.run
  end
end
"#;

fn gem_dir() -> PathBuf {
    let dir = std::env::var_os("SHOPIFY_MONEY_GEM_DIR")
        .map(PathBuf::from)
        .expect("SHOPIFY_MONEY_GEM_DIR must name an installed shopify-money-4.1.1");
    assert!(dir.join("lib/money.rb").is_file(), "{} holds no lib/money.rb", dir.display());
    dir
}

/// The gem's Ruby files CRuby loads for the probe (`$LOADED_FEATURES`),
/// relative to the gem, plus its currency data. `money_column` and the
/// RuboCop cops load only `if defined?(ActiveRecord)` / `RuboCop`, which
/// the native control has not, so the app carries the same closure the
/// control runs.
fn carried_files(gem: &Path) -> Vec<(String, String)> {
    let lib = gem.join("lib");
    let loaded = emit_and_run::ruby()
        .arg("-I")
        .arg(&lib)
        .args(["-r", "money", "-e", "puts $LOADED_FEATURES"])
        .output()
        .expect("CRuby load closure");
    assert!(loaded.status.success(), "{}", String::from_utf8_lossy(&loaded.stderr));
    let mut out: Vec<(String, String)> = String::from_utf8_lossy(&loaded.stdout)
        .lines()
        .filter_map(|line| Path::new(line).strip_prefix(gem).ok().map(Path::to_path_buf))
        .map(|rel| {
            let content = std::fs::read_to_string(gem.join(&rel)).expect("read gem file");
            (rel.to_string_lossy().into_owned(), content)
        })
        .collect();
    let mut data: Vec<_> = std::fs::read_dir(gem.join("config")).expect("gem config").flatten().map(|e| e.path()).collect();
    data.sort();
    for path in data {
        let rel = path.strip_prefix(gem).unwrap().to_string_lossy().into_owned();
        out.push((rel, std::fs::read_to_string(&path).expect("read gem data")));
    }
    assert!(out.iter().any(|(rel, _)| rel == "lib/money/currency.rb"), "closure lacks currency.rb: {:?}", out.iter().map(|f| &f.0).collect::<Vec<_>>());
    out
}

fn native_lines(gem: &Path) -> Vec<String> {
    let scratch = std::env::temp_dir().join(format!("roundhouse-money-native-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("money_probe.rb"), PROBE).unwrap();
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(gem.join("lib"))
        .arg("-I")
        .arg(&scratch)
        .args(["-r", "money_probe", "-e", "MoneyProbe.run"])
        .output()
        .expect("CRuby control");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
#[ignore = "needs SHOPIFY_MONEY_GEM_DIR (an installed shopify-money-4.1.1)"]
fn shopify_money_runs_as_in_cruby_after_emission() {
    let gem = gem_dir();
    let expected = native_lines(&gem);
    for probe in [
        "find! memoized: true new=true",
        "find! unknown: Money::Currency::UnknownCurrency: Invalid iso4217 currency 'zzz'",
        "zero memo: true zero?=true",
        "split: [\"33.34\", \"33.33\", \"33.33\"]",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  shopify-money: vendor/gems/shopify-money-4.1.1\n")
        .write("Gemfile.lock", LOCK)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/money_probe.rb", PROBE)
        .write("test/money_probe_test.rb", TEST);
    for (rel, content) in carried_files(&gem) {
        app = app.write(&format!("vendor/gems/shopify-money-4.1.1/{rel}"), &content);
    }
    let run = app.run_test("test/models/money_probe_test.rb");
    let actual: Vec<&str> = run.stdout.lines().filter(|l| expected.iter().any(|e| e == l) || !is_minitest_noise(l)).collect();
    if std::env::var_os("MONEY_PROBE_DEBUG").is_some() {
        eprintln!("=== success {} ===\n{}\n=== stderr ===\n{}", run.success, run.stdout, run.stderr);
    }
    run.assert_passes();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line\n=== stdout ===\n{}", run.stdout);
}

/// Minitest's own report lines around the probe's output.
fn is_minitest_noise(line: &str) -> bool {
    line.is_empty()
        || line.starts_with("Run options:")
        || line.starts_with("# Running")
        || line.starts_with("Finished in")
        || line.contains(" runs, ")
        || line.chars().all(|c| matches!(c, '.' | 'F' | 'E' | 'S'))
}
