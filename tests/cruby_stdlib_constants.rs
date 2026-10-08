//! Ruby's own classes Shopify Core's test boot names: `Errno::*`,
//! `Interrupt`, `Timeout`, `Marshal`, `FileUtils` and `YAML`
//! (coverage_warden, faster_fixtures, the ci-queue server). CRuby and
//! JRuby define them natively, so the analysis admits them and the Ruby
//! tree writes the default-gem requires the emit dropped. No other
//! runtime defines them, so those trees refuse the constant.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STORE: &str = r#"class Store
  def initialize(dir)
    @dir = dir
  end

  def write(name, value)
    FileUtils.mkdir_p(@dir)
    File.write(File.join(@dir, name), YAML.dump(value))
  end

  def read(name)
    YAML.load_file(File.join(@dir, name))
  rescue Errno::ENOENT
    nil
  end

  def copy(value)
    Marshal.load(Marshal.dump(value))
  end

  def quick
    Timeout.timeout(5) { 42 }
  rescue Timeout::Error, Interrupt, IO::TimeoutError
    nil
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/store.rb", STORE)
        .write("test/store_test.rb", r#"require "test_helper"
require "store"
class StoreTest < ActiveSupport::TestCase
  test "stdlib classes the interpreter defines" do
    store = Store.new(File.join(ENV.fetch("TMPDIR", "/tmp"), "rh_store_#{Process.pid}"))
    store.write("a.yml", { "k" => [1, 2] })
    assert_equal({ "k" => [1, 2] }, store.read("a.yml"))
    assert_nil store.read("missing.yml")
    assert_equal [1, "x"], store.copy([1, "x"])
    assert_equal 42, store.quick
  end
end
"#)
}

#[test]
fn cruby_stdlib_constants_are_admitted() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    fn sources(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rb") {
                out.push(std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut files = Vec::new();
    sources(&dir, &mut files);
    let store = files.iter().find(|f| f.contains("class Store")).expect("emitted Store");
    for feature in ["fileutils", "timeout", "yaml"] {
        assert!(store.contains(&format!("require {feature:?}")), "{feature}: {store}");
    }
}

#[test]
fn cruby_stdlib_constants_run_in_emitted_ruby() {
    app().run_test("test/models/store_test.rb").assert_passes();
}

#[test]
fn a_runtime_without_them_refuses_them() {
    let (_, errors) = app().emit(BuildTarget::Rust);
    for name in ["FileUtils", "YAML", "Errno::ENOENT", "Marshal", "Timeout::Error", "Interrupt"] {
        let needle = format!("{name} is CRuby's own");
        assert!(errors.iter().any(|e| e.contains(&needle)), "{name}: {errors:#?}");
    }
}

/// A core module Roundhouse models with a constant table (`Process`, for
/// the four clock ids Spinel lowers) is still the module Ruby's
/// signatures declare: `Process::WNOHANG` and `Process::WUNTRACED`
/// (`rbs/core/process.rbs`) are its Integers on CRuby, as in Core's
/// ci-queue executor reaping its forked workers. The expected values are
/// the native interpreter's (`ruby -e 'p [Process::WNOHANG, ...]'`), and
/// the reap counts real exited children.
const REAPER: &str = r#"class Reaper
  def reap
    total = 0
    while (result = Process.wait2(-1, Process::WNOHANG))
      total += result[1].exitstatus
    end
    total
  rescue Errno::ECHILD
    total
  end

  def flags
    [Process::WNOHANG, Process::WUNTRACED, Process::CLOCK_MONOTONIC]
  end
end
"#;

fn reaper_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/reaper.rb", REAPER)
        .write("test/reaper_test.rb", r#"require "test_helper"
require "reaper"
class ReaperTest < ActiveSupport::TestCase
  test "process constants the signatures declare" do
    assert_equal [1, 2, 1], Reaper.new.flags
    fork { exit!(3) }
    fork { exit!(4) }
    deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 5
    total = 0
    while total < 7 && Process.clock_gettime(Process::CLOCK_MONOTONIC) < deadline
      total += Reaper.new.reap
      sleep 0.01
    end
    assert_equal 7, total
  end
end
"#)
}

#[test]
fn signature_declared_process_constants_run_in_emitted_ruby() {
    reaper_app().run_test("test/models/reaper_test.rb").assert_passes();
}

#[test]
fn a_runtime_without_process_constants_refuses_them_by_name() {
    let (_, errors) = reaper_app().emit(BuildTarget::Rust);
    for name in ["Process::WNOHANG", "Process::WUNTRACED"] {
        let needle = format!("{name} is CRuby's own");
        assert!(errors.iter().any(|e| e.contains(&needle)), "{name}: {errors:#?}");
    }
    assert!(!errors.iter().any(|e| e.contains("constant not supported (all targets)")), "{errors:#?}");
}
