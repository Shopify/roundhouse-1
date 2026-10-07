//! A Gemfile gem the app's `Bundler.require(*Rails.groups)` requires in
//! the test environment, and whose require is measured Rails-free, is
//! loaded there, so its constants run as the gem's: one in the `:test`
//! group (Core's ci-queue, Gemfile:665), one in `:development, :test`
//! (dev_server, Gemfile:601), and one at top level. One declared
//! `require: false` still needs the app's own require, one only in
//! `:development` is not loaded under test, and runtime-engine-client
//! (Gemfile:509) stays refused: Bundler's `runtime-engine-client` and
//! `runtime/engine/client` both miss, so native Bundler.require skips it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const GEMFILE: &str = r#"source "https://rubygems.org"

gem "rbi"
gem("runtime-engine-client", "0.46.1")

group :development do
  gem "letter_opener"
end

group :development, :test do
  gem "dev_server"
end

group :test do
  gem "ci-queue", "~> 0.101.0"
  gem "vcr", require: false
end
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    ci-queue (0.101.0)\n    dev_server (1.5.2)\n    letter_opener (1.10.0)\n    rbi (0.3.6)\n    runtime-engine-client (0.46.1)\n    vcr (6.3.1)\n\nDEPENDENCIES\n  ci-queue\n  dev_server\n  letter_opener\n  rbi\n  runtime-engine-client\n  vcr\n";

fn app() -> emit_and_run::Overlay {
    let rbi = |body: &str| format!("# typed: true\n{body}");
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile", GEMFILE)
        .write("Gemfile.lock", LOCK)
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("sorbet/rbi/gems/rbi@0.3.6.rbi", &rbi("module RBI; end\nclass RBI::Tree; end\n"))
        .write("sorbet/rbi/gems/ci-queue@0.101.0.rbi", &rbi("module CI::Queue; end\nclass CI::Queue::Static; end\n"))
        .write("sorbet/rbi/gems/dev_server@1.5.2.rbi", &rbi("module DevServer; end\nclass DevServer::Client; end\n"))
        .write("sorbet/rbi/gems/vcr@6.3.1.rbi", &rbi("module VCR; end\nclass VCR::Cassette; end\n"))
        .write("sorbet/rbi/gems/letter_opener@1.10.0.rbi", &rbi("module LetterOpener; end\nclass LetterOpener::Message; end\n"))
        .write(
            "sorbet/rbi/gems/runtime-engine-client@0.46.1.rbi",
            &rbi("module RuntimeEngine; end\nmodule RuntimeEngine::Client; end\n"),
        )
        .write(
            "lib/stub_reader.rb",
            "class StubReader\n  def tree?(v) = v.is_a?(RBI::Tree)\n  def queue?(v) = v.is_a?(CI::Queue::Static)\n  def server?(v) = v.is_a?(DevServer::Client)\n  def cassette?(v) = v.is_a?(VCR::Cassette)\n  def message?(v) = v.is_a?(LetterOpener::Message)\n  def engine?(v) = v.is_a?(RuntimeEngine::Client)\nend\n",
        )
}

#[test]
fn bundler_required_test_env_gems_are_loaded_and_require_false_ones_are_not() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    let refused = |konst: &str, gem: &str| {
        errors.iter().any(|e| e.contains(&format!("{konst} is declared by the {gem} gem, which no emitted tree loads")))
    };
    assert!(!refused("RBI::Tree", "rbi"), "{errors:#?}");
    assert!(!refused("CI::Queue::Static", "ci-queue"), "{errors:#?}");
    assert!(!refused("DevServer::Client", "dev_server"), "{errors:#?}");
    // Refused, whether named for the gem or not (the gem fates leave
    // vcr's and letter_opener's RBIs unread where no tree loads them).
    let unsupported = |konst: &str| errors.iter().any(|e| e.contains(&format!("constant not supported (all targets): {konst}")));
    assert!(unsupported("VCR::Cassette"), "{errors:#?}");
    assert!(unsupported("LetterOpener::Message"), "{errors:#?}");
    assert!(refused("RuntimeEngine::Client", "runtime-engine-client"), "{errors:#?}");

    // The emitted tree requires them as Bundler would, in the
    // environments their groups name (ci-queue: `ci-queue`, then
    // `ci/queue`); letter_opener in development only.
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(facades.contains("  [[\"rbi\"], []], # Gemfile:3\n"), "{facades}");
    assert!(facades.contains("  [[\"letter_opener\"], [\"development\"]], # Gemfile:7\n"), "{facades}");
    assert!(facades.contains("  [[\"dev_server\"], [\"development\", \"test\"]], # Gemfile:11\n"), "{facades}");
    assert!(facades.contains("  [[\"ci-queue\", \"ci/queue\"], [\"test\"]], # Gemfile:15\n"), "{facades}");
    assert!(!facades.contains("\"vcr\""), "{facades}");
    assert!(!facades.contains("runtime-engine-client"), "{facades}");
}

/// The emitted boot runs the list: a gem the machine does not have is
/// quiet, as for the rest of gem_facades.rb.
#[test]
fn the_emitted_bundler_require_list_boots() {
    let run = app().run_ruby("puts \"booted\"\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\n", "{}", run.stderr);
    assert!(!run.stderr.contains("roundhouse: require"), "{}", run.stderr);
}

/// Ruby 4.0's stdlib cgi.rb is a stub (cgi/escape only, and a warning
/// under $VERBOSE) that a plain `require "cgi"` finds before the gem.
/// Under Bundler the lock's cgi is on the load path; vcr 6.3.1, in Core's
/// test group, calls `CGI.method(:parse)` as it loads. The emitted tree
/// activates the locked cgi before the Bundler.require list runs.
#[test]
fn a_bundler_required_gem_loads_the_locked_cgi_not_the_stdlib_stub() {
    let lock = LOCK
        .replace("    ci-queue (0.101.0)\n", "    cgi (0.4.2)\n    ci-queue (0.101.0)\n")
        .replace("    rbi (0.3.6)\n", "    rbi (0.3.6)\n      cgi\n");
    let (dir, errors) = app().write("Gemfile.lock", &lock).emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("RBI::Tree")), "{errors:#?}");
    let pins = std::fs::read_to_string(dir.join("runtime/bundled_gems.rb")).expect("bundled_gems.rb");
    assert!(pins.contains("  [\"cgi\", \"0.4.2\"],\n"), "{pins}");

    // A gem home with the locked cgi and an rbi whose entry needs the
    // gem's CGI.parse, ahead of the interpreter's own gems.
    let home = dir.join("tmp-gem-home");
    let gem = |name: &str, version: &str, file: &str, body: &str| {
        let lib = home.join(format!("gems/{name}-{version}/lib"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join(file), body).unwrap();
        std::fs::create_dir_all(home.join("specifications")).unwrap();
        std::fs::write(
            home.join(format!("specifications/{name}-{version}.gemspec")),
            format!("Gem::Specification.new do |s|\n  s.name = {name:?}\n  s.version = {version:?}\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n"),
        )
        .unwrap();
    };
    gem("cgi", "0.4.2", "cgi.rb", "class CGI\n  def self.parse(query) = {\"q\" => [query]}\nend\n");
    gem("rbi", "0.3.6", "rbi.rb", "require \"cgi\"\nmodule RBI\n  PARSER = CGI.method(:parse)\nend\n");
    let own = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    let gem_path = format!("{}:{}", home.display(), String::from_utf8_lossy(&own.stdout));
    let out = std::process::Command::new("ruby")
        .current_dir(&dir)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .args(["-W1", "-e", "require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\np RBI::PARSER.call(\"x\")\n"])
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"q\" => [\"x\"]}\n", "{stderr}");
    assert!(!stderr.contains("CGI library is removed"), "{stderr}");
}
