//! A gem a gem source's file requires as it loads, where the gem source's
//! entry does not reach that file by `require`.
//!
//! hedwig 1.68.0 is a GEM-section gem Core compiles as a gem source
//! (`gem_sources:`). Its lib/hedwig.rb reaches lib/hedwig/configuration.rb
//! only through `autoload :Configuration`, and that file runs `require
//! "mono_logger"` before `Configuration#initialize` reads
//! `MonoLogger.new($stdout)`. The compiler read the gem home's gems only
//! for the features the entry's `require` walk reached, so mono_logger, a
//! pure-Ruby lock dependency of hedwig requiring nothing from Rails, was
//! never counted as loaded: `MonoLogger is declared by the mono_logger gem,
//! which no emitted tree loads`, and the emitted initialize raised
//! `constant not supported`. Every gem-source file's load-level requires
//! now count; the compiled program carries the require where Ruby runs it,
//! so the gem is loaded before the file's own code reads its constants.
//! A require under a condition, or of a gem whose lock closure reaches
//! Rails, still leaves the gem unloaded.
//!
//! A lock dependency from a GIT section is Bundler's checkout, whose own
//! gemspec names its require paths: grpc's `src/ruby/lib`, which
//! shopify_spanner 4.9.0's defaults.rb (`require "grpc"`) reaches, so
//! panama_client's and shopify_spanner's `GRPC::*` reads were refused
//! while the gem home had no `specifications/grpc-*.gemspec` to read.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

/// hedgy is hedwig's shape: a GEM-section gem depending on Rails (so it
/// is compiled as a gem source) and on monolog, a pure-Ruby gem.
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hedgy (1.0.0)\n      activesupport (>= 7.2)\n      monolog (~> 1.0)\n      railsy\n    monolog (1.1.2)\n    railsy (1.0.0)\n      activesupport\n\nDEPENDENCIES\n  activesupport\n  hedgy\n";

const ENTRY: &str = "module Hedgy\n  autoload :Configuration, \"hedgy/configuration\"\n\n  def self.configuration\n    @configuration ||= Configuration.new\n  end\nend\n";

const CONFIGURATION: &str = "require \"monolog\"\n\nmodule Hedgy\n  class Configuration\n    attr_reader :logger\n\n    def initialize\n      @logger = MonoLog.new($stdout)\n    end\n  end\nend\n";

const SCRIPT: &str = "logger = Hedgy.configuration.logger\nputs logger.class.name\nputs logger.class.superclass.name\nputs logger.mono?\n";

fn fake_gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "rh_gem_source_autoloaded_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let spec = |name: &str, version: &str| {
        format!(
            "Gem::Specification.new do |s|\n  s.name = \"{name}\".freeze\n  s.version = \"{version}\".freeze\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\".freeze]\nend\n"
        )
    };
    let files: Vec<(String, String)> = vec![
        ("specifications/monolog-1.1.2.gemspec".into(), spec("monolog", "1.1.2")),
        (
            "gems/monolog-1.1.2/lib/monolog.rb".into(),
            "require \"logger\"\n\nclass MonoLog < Logger\n  def mono?\n    true\n  end\nend\n".into(),
        ),
        // Required too, but its lock closure reaches activesupport: its
        // constants are not the tree's.
        ("specifications/railsy-1.0.0.gemspec".into(), spec("railsy", "1.0.0")),
        ("gems/railsy-1.0.0/lib/railsy.rb".into(), "module Railsy\n  class Hook; end\nend\n".into()),
    ];
    for (path, content) in files {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

fn app(home: &Path, configuration: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!("test_paths:\n  - test\ngem_home: {}\ngem_sources:\n  hedgy: vendor/gems/hedgy-1.0.0\n", home.display()),
        )
        .write("Gemfile.lock", LOCK)
        // The app's Bundler.require loads the gem source, as `-rhedgy`
        // does natively.
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"hedgy\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        // Core's Tapioca RBIs type the gems' constants, as
        // sorbet/rbi/gems/mono_logger@1.1.2.rbi does MonoLogger: the
        // declaring gem decides whether the read can run.
        .write("sorbet/rbi/gems/monolog@1.1.2.rbi", "# typed: true\n\nclass MonoLog < ::Logger\n  def mono?; end\nend\n")
        .write("sorbet/rbi/gems/railsy@1.0.0.rbi", "# typed: true\n\nmodule Railsy; end\nclass Railsy::Hook; end\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("vendor/gems/hedgy-1.0.0/lib/hedgy.rb", ENTRY)
        .write("vendor/gems/hedgy-1.0.0/lib/hedgy/configuration.rb", configuration)
}

fn constant_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("constant not supported") || e.contains("no indexed declaration")).collect()
}

/// The fixture gem home ahead of the interpreter's own, so the emitted
/// tree's other gems (sqlite3) still load.
fn gem_path(home: &Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_gem_an_autoloaded_gem_source_file_requires_is_loaded() {
    let home = fake_gem_home();
    let gem_path = gem_path(&home);
    // CRuby on the gem source itself is the oracle.
    let native_root = home.join("native-hedgy");
    std::fs::create_dir_all(native_root.join("lib/hedgy")).unwrap();
    std::fs::write(native_root.join("lib/hedgy.rb"), ENTRY).unwrap();
    std::fs::write(native_root.join("lib/hedgy/configuration.rb"), CONFIGURATION).unwrap();
    let native = emit_and_run::ruby()
        .env("GEM_PATH", &gem_path)
        .arg("-I")
        .arg(native_root.join("lib"))
        .args(["-rhedgy", "-e", SCRIPT])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    assert_eq!(expected, ["MonoLog", "Logger", "true"], "native control");

    let (tree, errors) = app(&home, CONFIGURATION).emit(BuildTarget::Ruby);
    assert!(constant_errors(&errors).is_empty(), "{errors:#?}");
    let emitted = std::fs::read_to_string(tree.join("app/models/hedgy/configuration.rb")).expect("configuration.rb");
    assert!(emitted.contains("MonoLog.new($stdout)"), "{emitted}");
    assert!(!emitted.contains("roundhouse:"), "{emitted}");
    let run = Command::new("ruby")
        .current_dir(&*tree)
        .env("GEM_PATH", &gem_path)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed in {}: {stderr}\n{emitted}", tree.display());
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby\n{emitted}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_conditional_or_rails_reaching_require_leaves_the_gem_unloaded() {
    let conditional = CONFIGURATION.replace("require \"monolog\"\n", "require \"monolog\" if ENV[\"HEDGY_LOG\"]\n");
    let rails = CONFIGURATION.replace("require \"monolog\"\n", "require \"monolog\"\nrequire \"railsy\"\n").replace(
        "      @logger = MonoLog.new($stdout)\n",
        "      @logger = MonoLog.new($stdout)\n      @hook = Railsy::Hook\n",
    );
    for (source, refused, kept) in [(conditional, "MonoLog", None), (rails, "Railsy::Hook", Some("MonoLog"))] {
        let home = fake_gem_home();
        let (_, errors) = app(&home, &source).emit(BuildTarget::Ruby);
        let _ = std::fs::remove_dir_all(&home);
        let constants = constant_errors(&errors);
        assert!(constants.iter().any(|e| e.contains(refused)), "{refused}: {errors:#?}");
        if let Some(kept) = kept {
            assert!(!constants.iter().any(|e| e.contains(&format!("{kept} ")) || e.ends_with(kept)), "{kept}: {errors:#?}");
        }
    }
}

/// gitlog is grpc's shape: a GIT-section lock dependency of the gem
/// source whose checkout's gemspec puts its files under src/ruby/lib.
const GIT_LOCK: &str = "GIT\n  remote: https://github.com/example/gitlog.git\n  revision: 0123456789abcdef0123456789abcdef01234567\n  specs:\n    gitlog (2.0.0.dev)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hedgy (1.0.0)\n      activesupport (>= 7.2)\n      gitlog\n\nDEPENDENCIES\n  activesupport\n  gitlog!\n  hedgy\n";

const GIT_CONFIGURATION: &str = "require \"gitlog\"\n\nmodule Hedgy\n  class Configuration\n    attr_reader :logger\n\n    def initialize\n      @logger = GitLog::Channel.new\n    end\n  end\nend\n";

#[test]
fn a_git_gem_s_checkout_gemspec_names_its_require_paths() {
    let home = fake_gem_home();
    let checkout = home.join("bundler/gems/gitlog-0123456789ab");
    std::fs::create_dir_all(checkout.join("src/ruby/lib")).unwrap();
    std::fs::write(
        checkout.join("gitlog.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"gitlog\".freeze\n  s.version = \"2.0.0.dev\".freeze\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"src/ruby/lib\".freeze]\nend\n",
    )
    .unwrap();
    std::fs::write(checkout.join("src/ruby/lib/gitlog.rb"), "module GitLog\n  class Channel; end\nend\n").unwrap();
    let (_, errors) = app(&home, GIT_CONFIGURATION)
        .write("Gemfile.lock", GIT_LOCK)
        .write("sorbet/rbi/gems/gitlog@2.0.0.dev-0123456789abcdef0123456789abcdef01234567.rbi", "# typed: true\n\nmodule GitLog; end\nclass GitLog::Channel; end\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(constant_errors(&errors).is_empty(), "{errors:#?}");
}

/// carriers_db 2.29.0's shape: a gem source file whose program requires
/// tracking_number, a gem whose lock closure reaches activesupport only
/// through `require 'active_support/all'`, which the runtime's feature
/// port answers (measured: `project`'s `PORT_ANSWERED_RAILS_DEPENDENTS`).
/// The compiled program runs that require itself, so the gem is loaded
/// and `TrackingNumber.search` reads as the gem's.
const TRACKING_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    hedgy (1.0.0)\n      activesupport (>= 7.2)\n      monolog (~> 1.0)\n      tracking_number\n    monolog (1.1.2)\n    tracking_number (3.0.0)\n      activesupport (>= 4.2.5)\n\nDEPENDENCIES\n  activesupport\n  hedgy\n";

const TRACKING_CONFIGURATION: &str = "require \"tracking_number\"\n\nmodule Hedgy\n  class Configuration\n    def detect(number)\n      TrackingNumber.search(number)\n    end\n  end\nend\n";

#[test]
fn a_gem_measured_to_load_over_the_ports_is_loaded_where_a_file_program_requires_it() {
    let home = fake_gem_home();
    let spec = "Gem::Specification.new do |s|\n  s.name = \"tracking_number\".freeze\n  s.version = \"3.0.0\".freeze\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\".freeze]\nend\n";
    std::fs::write(home.join("specifications/tracking_number-3.0.0.gemspec"), spec).unwrap();
    std::fs::create_dir_all(home.join("gems/tracking_number-3.0.0/lib")).unwrap();
    std::fs::write(
        home.join("gems/tracking_number-3.0.0/lib/tracking_number.rb"),
        "require \"active_support/all\"\n\nmodule TrackingNumber\n  def self.search(_number) = []\nend\n",
    )
    .unwrap();
    let (tree, errors) = app(&home, TRACKING_CONFIGURATION)
        .write("Gemfile.lock", TRACKING_LOCK)
        .write("sorbet/rbi/gems/tracking_number@3.0.0.rbi", "# typed: true\n\nmodule TrackingNumber\n  class << self\n    def search(body); end\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(constant_errors(&errors).is_empty(), "{errors:#?}");
    let emitted = std::fs::read_to_string(tree.join("app/models/hedgy/configuration.rb")).expect("configuration.rb");
    assert!(emitted.contains("TrackingNumber.search(number)"), "{emitted}");
}
