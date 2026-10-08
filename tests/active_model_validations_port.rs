//! The ActiveModel::Validations port of the PostgreSQL CRuby tree
//! (runtime/spinel/active_model_validations.rb, with
//! active_support_option_merger.rb) against the Rails native Core runs
//! (Shopify/rails 5813559f4aed, 8.2.0.alpha; tests/support/rails_oracle.rb),
//! and api_versioning 4.6.0, whose FrozenRecord classes (ApiVersionRecord,
//! ApiChangeRecord) include it, compiled as a gem source over it.
//!
//! tests/active_model_validations_port.rb runs the same models through
//! the pinned activemodel and through the port and requires every
//! observation equal. The api_versioning probe vendors api_versioning and
//! frozen_record from Core's gem home (`gem_sources:`, the way
//! bin/check-target carries them), emits the PostgreSQL CRuby tree, boots
//! it as Core boots and compares a record's validation answers with
//! api_versioning loaded natively: `validates` / `validate` /
//! `with_options` replay as written in the class bodies, and
//! ApiVersionValidations' `errors.add(:handle, msg)` reaches Rails'
//! ActiveModel::Errors rather than the models' Array accumulator.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn core_gem_home() -> String {
    std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into())
}

fn gem_path() -> String {
    let ruby_path = Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().unwrap();
    format!("{}:{}", core_gem_home(), String::from_utf8_lossy(&ruby_path.stdout))
}

#[test]
fn validations_port_answers_like_activemodel() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_model_validations_port.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and the emitted ActiveModel::Validations port agree"),
        "the port diverged from activemodel\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 45, "too few observations:\n{stdout}");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
            let mut perms = std::fs::metadata(&target).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(&target, perms).unwrap();
        }
    }
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activemodel (8.2.0.alpha)\n      activesupport (= 8.2.0.alpha)\n    activesupport (8.2.0.alpha)\n      concurrent-ruby (~> 1.0, >= 1.3.1)\n      i18n (>= 1.6, < 2)\n    api_versioning (4.6.0)\n      activemodel (>= 5.2)\n      activesupport (>= 5.2)\n      frozen_record (>= 0.26.1)\n      sorbet-runtime\n    concurrent-ruby (1.3.8)\n    frozen_record (0.27.4)\n      activemodel\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n    sorbet-runtime (0.6.13502)\n\nDEPENDENCIES\n  api_versioning\n  frozen_record\n";

const API_VERSION: &str = "module Probe\n  class ApiVersion < ApiVersioning::ApiVersionRecord\n  end\nend\n";

// `new` types as the receiving record: frozen_record's own `new` calls
// `load`, its `class << self` alias of Class#new. `find` reaches the
// analyzer untyped, so its records are ascribed: an `errors` read on an
// untyped receiver is refused while the tree carries the port (see
// `untyped_errors_reads_on_a_port_tree_are_refused`).
const PROBE: &str = r##"class AvProbe
  def self.bad_lines
    bad = Probe::ApiVersion.new("handle" => "20x1", "fully_inaccessible" => "soon")
    blank = Probe::ApiVersion.new("handle" => "")
    [bad.valid?, bad.errors.full_messages, bad.errors.attribute_names, bad.invalid?, blank.valid?, blank.errors.full_messages,
     begin; blank.validate!; rescue ActiveModel::ValidationError => e; e.message; end]
  end

  def self.lines
    found = Probe::ApiVersion.find("2021-01") #: Probe::ApiVersion
    unstable = Probe::ApiVersion.find("unstable") #: Probe::ApiVersion
    [
      "count=#{Probe::ApiVersion.count}",
      "find=#{Probe::ApiVersion.find("2022-04").handle} #{Probe::ApiVersion["2021-07"].inspect}",
      "model_name=#{Probe::ApiVersion.model_name.name} #{Probe::ApiVersion.model_name.param_key}",
      "valid=#{Probe::ApiVersion.all.map(&:valid?).tally.inspect} #{found.errors.full_messages.inspect}",
      "validators=#{Probe::ApiVersion.validators.map(&:kind).inspect} #{Probe::ApiVersion.validators_on(:handle).map(&:class).inspect}",
      "bad=#{bad_lines.inspect}",
      "unstable=#{unstable.valid?} #{unstable.errors.size}",
      "changes=#{ApiVersioning::ApiChangeRecord.validators.map(&:kind).uniq.sort.inspect}",
    ]
  end
end
"##;

fn api_versioning_app(tag: &str, probe: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_am_validations_{tag}_{}", std::process::id()));
    let files = [
        ("Gemfile", "source \"https://rubygems.org\"\ngem \"frozen_record\"\ngem \"api_versioning\"\n"),
        ("Gemfile.lock", LOCK),
        (
            "roundhouse.yml",
            "test_paths:\n  - test\ngem_sources:\n  frozen_record: vendor/gems/frozen_record-0.27.4\n  api_versioning: vendor/gems/api_versioning-4.6.0\ndatabase_adapter: postgresql\n",
        ),
        ("config/boot.rb", "require \"bundler/setup\"\n"),
        (
            "config/application.rb",
            "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule AvProbeApp\n  class Application < Rails::Application\n  end\nend\n",
        ),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"),
        ("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n"),
        ("app/models/probe/api_version.rb", API_VERSION),
        ("app/models/av_probe.rb", probe),
    ];
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let gems = Path::new(&core_gem_home()).join("gems");
    for gem in ["frozen_record-0.27.4", "api_versioning-4.6.0"] {
        copy_tree(&gems.join(gem), &dir.join("vendor/gems").join(gem));
    }
    dir
}

#[test]
fn api_versioning_records_validate_like_native() {
    let app = api_versioning_app("probe", PROBE);
    let script = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activemodel\")\n\
         $LOAD_PATH.unshift {fr:?}\n$LOAD_PATH.unshift {av:?}\n$LOAD_PATH.unshift {models:?}\nrequire \"pathname\"\nrequire \"frozen_record\"\nrequire \"api_versioning\"\nrequire \"probe/api_version\"\nrequire \"av_probe\"\nputs AvProbe.lines\n",
        oracle = root().join("tests/support/rails_oracle").display().to_string(),
        fr = app.join("vendor/gems/frozen_record-0.27.4/lib").display().to_string(),
        av = app.join("vendor/gems/api_versioning-4.6.0/lib").display().to_string(),
        models = app.join("app/models").display().to_string(),
    );
    std::fs::write(app.join("native.rb"), script).unwrap();
    let out = Command::new("ruby").arg(app.join("native.rb")).env("GEM_PATH", gem_path()).output().expect("ruby");
    assert!(out.status.success(), "native run failed: {}", String::from_utf8_lossy(&out.stderr));
    let native = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        native.contains("valid={true => 25} []") && native.contains("Handle can't be blank"),
        "{native}"
    );

    let out_dir = app.join("emitted");
    let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby", "--allow-unsupported"])
        .arg(&app)
        .arg("-o")
        .arg(&out_dir)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&emit.stderr).into_owned();
    assert!(emit.status.success(), "{log}");
    // Every validation in the two records' bodies is carried, none
    // dropped, and the probe's reads of Rails' collection are not
    // refused.
    for line in log.lines() {
        assert!(
            !(line.contains("api_version_record.rb") || line.contains("api_change_record.rb")) || !line.contains("dropped"),
            "{line}"
        );
        assert!(!line.contains("ActiveModel::Errors not supported"), "{line}");
    }
    let validations = std::fs::read_to_string(out_dir.join("app/models/api_versioning/api_version_validations.rb")).unwrap();
    assert!(validations.contains("errors.add(:handle"), "{validations}");

    let run = Command::new("ruby")
        .args(["-e", "require \"./boot\"; puts AvProbe.lines"])
        .current_dir(&out_dir)
        .env("GEM_PATH", gem_path())
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted run failed:\n{stderr}");
    assert_eq!(native, String::from_utf8_lossy(&run.stdout), "stderr:\n{stderr}");
    std::fs::remove_dir_all(app).unwrap();
}

/// Without i18n in the lockfile the port has no I18n to translate its
/// messages through: the PostgreSQL bootstrap refuses rather than
/// carrying a Validations that cannot answer `full_messages`.
#[test]
fn validations_without_i18n_in_the_lockfile_are_refused() {
    let app = api_versioning_app("noi18n", PROBE);
    let lock = LOCK
        .replace("      concurrent-ruby (~> 1.0, >= 1.3.1)\n      i18n (>= 1.6, < 2)\n", "")
        .replace("    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n", "");
    std::fs::write(app.join("Gemfile.lock"), lock).unwrap();
    let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby", "--allow-unsupported"])
        .arg(&app)
        .arg("-o")
        .arg(app.join("emitted"))
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&emit.stderr);
    assert!(!emit.status.success(), "{log}");
    assert!(log.contains("includes ActiveModel::Validations, whose messages translate through I18n, but the lockfile has no i18n"), "{log}");
    std::fs::remove_dir_all(app).unwrap();
}

/// frozen_record's `find` reaches the analyzer untyped: whether its
/// record's `errors` is the port's ActiveModel::Errors or a model's
/// Array accumulator is unknown, so each errors spelling the lowerings
/// would ground for the accumulator is refused rather than guessed.
#[test]
fn untyped_errors_reads_on_a_port_tree_are_refused() {
    let probe = "class AvProbe\n  def self.lines\n    found = Probe::ApiVersion.find(\"2021-01\")\n    \
                 typed = Probe::ApiVersion.new(\"handle\" => \"\")\n    \
                 found.errors.add(:base, \"gone\")\n    \
                 [found.errors.full_messages, found.errors[:handle], typed.errors.full_messages, typed.errors[:handle]]\n  end\nend\n";
    let app = api_versioning_app("untyped", probe);
    let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby"])
        .arg(&app)
        .arg("-o")
        .arg(app.join("emitted"))
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&emit.stderr);
    assert!(!emit.status.success(), "{log}");
    let refusals: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("av_probe.rb") && l.contains("the receiver's type is unknown"))
        .collect();
    for (line, spelling) in [(5, "errors.add"), (6, "errors.full_messages"), (6, "errors[]")] {
        assert!(
            refusals.iter().any(|l| l.contains(&format!("av_probe.rb:{line}:")) && l.contains(&format!("error[unsupported]: {spelling} not supported"))),
            "{spelling} at line {line} not refused:\n{log}"
        );
    }
    // The records `new` built are typed: their reads stand as the port's.
    assert_eq!(refusals.len(), 3, "{log}");
    std::fs::remove_dir_all(app).unwrap();
}
