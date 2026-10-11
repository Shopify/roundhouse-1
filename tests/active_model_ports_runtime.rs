// The ActiveModel ports of the PostgreSQL CRuby tree (runtime/spinel/
// active_model_naming.rb, active_model_attribute_methods.rb,
// active_model_serialization.rb, active_support_string_inflections.rb)
// against the Rails native Core runs (Shopify/rails 5813559f4aed,
// 8.2.0.alpha; tests/support/rails_oracle.rb), and frozen_record 0.27.4,
// the gem that names them, compiled as a gem source over them.
//
// tests/active_model_ports_runtime.rb runs the same models through the
// pinned activemodel and through the ports and requires every
// observation equal. The frozen_record probe vendors the installed gem
// from Core's gem home (`gem_sources:`, the way bin/check-target carries
// api_versioning), emits the PostgreSQL CRuby tree, boots it as Core
// boots (config/application.rb's Bundler.require) and compares a model's
// answers with frozen_record loaded natively on activemodel 8.2.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn core_gem_home() -> String {
    std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into())
}

#[test]
fn active_model_ports_answer_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_model_ports_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and the emitted ActiveModel ports agree"),
        "ports diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 55, "too few observations:\n{stdout}");
    for expected in [
        "model_name Admin::User: [\"Admin::User\", \"admin_user\", \"admin_users\", \"user\", \"admin/users\", \"user\", \"users\", \"user\", :\"admin/user\", \"User\", false, \"admin/users\", \"Admin::User\"]",
        "model_name Sheep: [\"Sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep\", \"sheep_index\",",
        "composite: [\"1-2\", \"1_2\", nil, nil, nil, nil]",
        "generated methods: [:age, :age?, :age_changed?, :clear_age, :clear_name, :clear_nickname, :name,",
        "after undefine: [[], true, \"Bob\", true]",
        "include nested: {\"title\" => \"Dune\", \"author\" => {\"name\" => \"Frank\"}, \"coauthors\" => [{\"name\" => \"Brian\"}, {\"name\" => \"Kevin\"}]}",
        "as_json with root: [{\"book\" => {\"pages\" => 412}}, {\"pages\" => 412}, false]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

const COUNTRY: &str = r##"class Country < FrozenRecord::Base
  self.base_path = File.expand_path("../../data", __dir__)
  add_index :code, unique: true
  scope :european, -> { where(region: "europe") }

  def label = "#{name} (#{code})"
end
"##;

const PROBE: &str = r##"class FrProbe
  def self.lines
    [
      "model_name=#{Country.model_name.class} #{Country.model_name} human=#{Country.model_name.human} underscore=#{Country.name.underscore.pluralize}",
      "names=#{Country.model_name.singular} #{Country.model_name.plural} #{Country.model_name.route_key} #{Country.model_name.param_key} #{Country.model_name.element} #{Country.model_name.collection} #{Country.model_name.i18n_key.inspect}",
      "file=#{File.basename(Country.file_path)}",
      "count=#{Country.count}",
      "all=#{Country.all.map(&:code).inspect}",
      "find=#{Country.find(2).label} #{Country.find_by(code: "ch").name}",
      "attr?=#{Country.find(1).nato?} #{Country.find(3).nato?}",
      "where=#{Country.where(region: "europe").map(&:name).inspect} #{Country.european.pluck(:code).inspect}",
      "find_each=#{codes = []; Country.where(region: "europe").find_each { |c| codes << c.code }; codes.inspect}",
      "attributes=#{Country.attributes.to_a.sort.inspect}",
      "to_key=#{Country.find(1).to_key.inspect} to_param=#{Country.find(1).to_param.inspect} to_model=#{Country.find(1).to_model.equal?(Country.find(1))} persisted=#{Country.find(1).persisted?}",
      "json=#{Country.find(1).serializable_hash.inspect}",
      "json_only=#{Country.find(1).serializable_hash(only: [:code, :name]).inspect}",
      "notfound=#{begin; Country.find(9); rescue FrozenRecord::RecordNotFound => e; e.class.name; end}",
      "load=#{Country.respond_to?(:load)} #{Country.new(code: "xx", name: "X").label}",
      "descendants=#{FrozenRecord::Base.descendants.map(&:name).sort.inspect}",
    ]
  end
end
"##;

/// A record on `FrozenRecord::Compact` (a Concern: `ClassMethods` and
/// ivar-backed attributes), the way Core's LandedCost thresholds are.
const COMPACT_COUNTRY: &str = r##"class CompactCountry < FrozenRecord::Base
  include FrozenRecord::Compact
  self.base_path = File.expand_path("../../data", __dir__)
  self.primary_key = :code
  add_index :code, unique: true

  def self.file_path = File.join(base_path, "countries.yml")

  def initialize(*)
    super
  end

  def label = "#{name} (#{code})"
end
"##;

const COMPACT_PROBE: &str = r##"class CmpProbe
  def self.lines
    c = CompactCountry.find("fr")
    [
      "ancestors=#{CompactCountry.singleton_class.include?(FrozenRecord::Compact::ClassMethods)} #{CompactCountry.include?(FrozenRecord::Compact)}",
      "count=#{CompactCountry.count} all=#{CompactCountry.all.map(&:code).inspect}",
      "find=#{c.label} #{c[:region]} #{c["nato"]} #{c.nato?} #{CompactCountry.find("ch").nato?}",
      "attributes=#{c.attributes.inspect}",
      "ivars=#{c.instance_variables.sort.inspect}",
      "cache=#{CompactCountry._attributes_cache.keys.map(&:inspect).sort.inspect}",
      "where=#{CompactCountry.where(region: "europe").pluck(:name).inspect}",
      "json=#{c.serializable_hash.inspect}",
    ]
  end
end
"##;

const COUNTRIES: &str = "- id: 1\n  code: ca\n  name: Canada\n  region: americas\n  nato: true\n- id: 2\n  code: fr\n  name: France\n  region: europe\n  nato: true\n- id: 3\n  code: ch\n  name: Switzerland\n  region: europe\n  nato: false\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activemodel (8.2.0.alpha)\n      activesupport (= 8.2.0.alpha)\n    activesupport (8.2.0.alpha)\n    frozen_record (0.27.4)\n      activemodel\n\nDEPENDENCIES\n  frozen_record\n";

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

fn frozen_record_app() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rh_frozen_record_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let files = [
        ("Gemfile", "source \"https://rubygems.org\"\ngem \"frozen_record\"\n"),
        ("Gemfile.lock", LOCK),
        ("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  frozen_record: vendor/gems/frozen_record-0.27.4\ndatabase_adapter: postgresql\n"),
        ("config/boot.rb", "require \"bundler/setup\"\n"),
        (
            "config/application.rb",
            "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule FrProbeApp\n  class Application < Rails::Application\n  end\nend\n",
        ),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"),
        ("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n"),
        ("app/models/country.rb", COUNTRY),
        ("app/models/fr_probe.rb", PROBE),
        ("app/models/compact_country.rb", COMPACT_COUNTRY),
        ("app/models/cmp_probe.rb", COMPACT_PROBE),
        ("data/countries.yml", COUNTRIES),
    ];
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let gem = Path::new(&core_gem_home()).join("gems/frozen_record-0.27.4");
    copy_tree(&gem, &dir.join("vendor/gems/frozen_record-0.27.4"));
    dir
}

/// frozen_record loaded natively on the pinned activemodel, as the
/// Rails native Core runs load it.
fn native_lines(app: &Path) -> String {
    let script = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activemodel\")\n\
         $LOAD_PATH.unshift {lib:?}\n$LOAD_PATH.unshift {models:?}\nrequire \"frozen_record\"\nrequire \"country\"\nrequire \"fr_probe\"\n\
         require \"compact_country\"\nrequire \"cmp_probe\"\nputs FrProbe.lines\nputs CmpProbe.lines\n",
        oracle = root().join("tests/support/rails_oracle").display().to_string(),
        lib = app.join("vendor/gems/frozen_record-0.27.4/lib").display().to_string(),
        models = app.join("app/models").display().to_string(),
    );
    let file = app.join("native.rb");
    std::fs::write(&file, script).unwrap();
    let out = Command::new("ruby").arg(&file).output().expect("ruby");
    assert!(out.status.success(), "native frozen_record run failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn frozen_record_compiled_as_a_gem_source_answers_like_native() {
    let app = frozen_record_app();
    let native = native_lines(&app);
    assert!(native.contains("find_each=[\"fr\", \"ch\"]") && native.contains("load=false X (xx)"), "{native}");
    assert!(native.contains("ancestors=true true") && native.contains("ivars=[:@code, :@id, :@name, :@nato, :@region]"), "{native}");

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
    // Every class-body statement frozen_record's Base and Scope run is
    // carried: `alias_method :load, :new` (above its own `def new`) and
    // `alias_method :find_each, :each` (below the `delegate` of `each`).
    // Nor are Compact's `extend ActiveSupport::Concern` (what it does is
    // written at the include) and Yaml's `autoload :ERB, 'erb'` (the
    // tree requires erb where ERB is named) reported as dropped.
    for dropped in ["base.rb:210", "scope.rb:12", "compact.rb:5", "yaml.rb:6"] {
        assert!(!log.lines().any(|l| l.contains(dropped) && l.contains("dropped")), "{dropped}:\n{log}");
    }
    let base = std::fs::read_to_string(out_dir.join("app/models/frozen_record/base.rb")).unwrap();
    let alias = base.find("singleton_class.alias_method :load, :new").expect("the load alias");
    assert!(alias < base.find("def self.new(").expect("Base.new"), "{base}");

    let ruby_path = Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().unwrap();
    let gem_path = format!("{}:{}", core_gem_home(), String::from_utf8_lossy(&ruby_path.stdout));
    let run = Command::new("ruby")
        .args(["-e", "require \"./boot\"; puts FrProbe.lines; puts CmpProbe.lines"])
        .current_dir(&out_dir)
        .env("GEM_PATH", gem_path)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .unwrap();
    let emitted = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted frozen_record run failed:\n{stderr}");
    assert_eq!(native, emitted, "stderr:\n{stderr}");
    std::fs::remove_dir_all(app).unwrap();
}

const API_VERSION: &str = "module Probe\n  class ApiVersion < ApiVersioning::FrozenApplicationRecord\n    self.primary_key = \"handle\"\n  end\nend\n";

const FA_PROBE: &str = r##"class FaProbeLines
  def self.lines
    [
      "base_path=#{ApiVersioning::FrozenApplicationRecord.base_path.class} #{ApiVersioning::FrozenApplicationRecord.base_path.basename} #{ApiVersioning::FrozenApplicationRecord.base_path.frozen?} #{File.basename(Probe::ApiVersion.file_path)}",
      "backend=#{ApiVersioning::FrozenApplicationRecordBackend.filename("Probe::ApiVersion")} #{FrozenRecord::Backends::Yaml.filename("Probe::ApiVersion")}",
      "count=#{Probe::ApiVersion.count}",
      "all=#{Probe::ApiVersion.all.map(&:handle).first(6).inspect} last=#{Probe::ApiVersion.all.map(&:handle).last(3).inspect}",
      "find=#{Probe::ApiVersion.find("2022-04").handle} #{Probe::ApiVersion.find_by(handle: "2021-07").attributes["handle"]}",
      "attrs=#{Probe::ApiVersion.find("2021-01").attributes.keys.sort.inspect}",
      "deadline=#{Probe::ApiVersion.find("2021-01")[:fully_inaccessible].inspect} #{Probe::ApiVersion.find("2021-01")[:fully_inaccessible].class}",
      "abstract=#{ApiVersioning::FrozenApplicationRecord.abstract_class?} #{Probe::ApiVersion.abstract_class?.inspect}",
      "model_name=#{Probe::ApiVersion.model_name.name} #{Probe::ApiVersion.model_name.param_key}",
    ]
  end
end
"##;

const FA_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activemodel (8.2.0.alpha)\n      activesupport (= 8.2.0.alpha)\n    activesupport (8.2.0.alpha)\n    fa_probe (0.1.0)\n      frozen_record\n      sorbet-runtime\n    frozen_record (0.27.4)\n      activemodel\n    sorbet-runtime (0.6.13502)\n\nDEPENDENCIES\n  fa_probe\n  frozen_record\n";

fn gem_path() -> String {
    let ruby_path = Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().unwrap();
    format!("{}:{}", core_gem_home(), String::from_utf8_lossy(&ruby_path.stdout))
}

/// api_versioning 4.6.1's `FrozenApplicationRecord` and its backend,
/// verbatim from Core's gem home with the gem's data dir, carried as a
/// probe gem (`fa_probe`) beside frozen_record: the record class whose
/// subclasses Core's ApiVersionRecord and ApiChangeRecord are, without
/// the rest of api_versioning (ApiVersionRecord includes
/// ActiveModel::Validations, which this tree does not carry yet). The
/// backend `extend`s frozen_record's `extend self` Yaml backend and calls
/// `super` from its `filename`; the base path is a frozen Pathname to
/// the gem's `data`.
#[test]
fn frozen_application_record_loads_api_versioning_data_like_native() {
    let dir = std::env::temp_dir().join(format!("rh_frozen_app_record_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let files = [
        ("Gemfile", "source \"https://rubygems.org\"\ngem \"frozen_record\"\ngem \"fa_probe\"\n"),
        ("Gemfile.lock", FA_LOCK),
        ("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  frozen_record: vendor/gems/frozen_record-0.27.4\n  fa_probe: vendor/gems/fa_probe-0.1.0\ndatabase_adapter: postgresql\n"),
        ("config/boot.rb", "require \"bundler/setup\"\n"),
        (
            "config/application.rb",
            "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule FaProbeApp\n  class Application < Rails::Application\n  end\nend\n",
        ),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: roundhouse_unused\n"),
        ("test/test_helper.rb", "ENV[\"RAILS_ENV\"] ||= \"test\"\n"),
        ("app/models/probe/api_version.rb", API_VERSION),
        ("app/models/fa_probe_lines.rb", FA_PROBE),
        ("vendor/gems/fa_probe-0.1.0/lib/fa_probe.rb", "require \"sorbet-runtime\"\nrequire \"api_versioning/frozen_application_record\"\n"),
        (
            "vendor/gems/fa_probe-0.1.0/fa_probe.gemspec",
            "Gem::Specification.new do |s|\n  s.name = \"fa_probe\"\n  s.version = \"0.1.0\"\n  s.summary = \"probe\"\n  s.authors = [\"probe\"]\n  s.files = Dir[\"lib/**/*.rb\", \"data/*\"]\n  s.add_dependency \"frozen_record\"\n  s.add_dependency \"sorbet-runtime\"\nend\n",
        ),
    ];
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let home = Path::new(&core_gem_home()).join("gems");
    copy_tree(&home.join("frozen_record-0.27.4"), &dir.join("vendor/gems/frozen_record-0.27.4"));
    let api_versioning = home.join("api_versioning-4.6.1");
    copy_tree(&api_versioning.join("data"), &dir.join("vendor/gems/fa_probe-0.1.0/data"));
    for file in ["frozen_application_record.rb", "frozen_application_record_backend.rb"] {
        let to = dir.join("vendor/gems/fa_probe-0.1.0/lib/api_versioning").join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(api_versioning.join("lib/api_versioning").join(file), &to).unwrap();
    }

    let script = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activemodel\")\n\
         $LOAD_PATH.unshift {fr:?}\n$LOAD_PATH.unshift {fa:?}\n$LOAD_PATH.unshift {models:?}\nrequire \"pathname\"\nrequire \"frozen_record\"\nrequire \"fa_probe\"\nrequire \"probe/api_version\"\nrequire \"fa_probe_lines\"\nputs FaProbeLines.lines\n",
        oracle = root().join("tests/support/rails_oracle").display().to_string(),
        fr = dir.join("vendor/gems/frozen_record-0.27.4/lib").display().to_string(),
        fa = dir.join("vendor/gems/fa_probe-0.1.0/lib").display().to_string(),
        models = dir.join("app/models").display().to_string(),
    );
    std::fs::write(dir.join("native.rb"), script).unwrap();
    let out = Command::new("ruby").arg(dir.join("native.rb")).env("GEM_PATH", gem_path()).output().expect("ruby");
    assert!(out.status.success(), "native run failed: {}", String::from_utf8_lossy(&out.stderr));
    let native = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(native.contains("backend=api_versions.yml probe/api_versions.yml") && native.contains("count=25"), "{native}");

    let out_dir = dir.join("emitted");
    let emit = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby", "--allow-unsupported"])
        .arg(&dir)
        .arg("-o")
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
    let run = Command::new("ruby")
        .args(["-e", "require \"./boot\"; puts FaProbeLines.lines"])
        .current_dir(&out_dir)
        .env("GEM_PATH", gem_path())
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted run failed:\n{stderr}");
    assert_eq!(native, String::from_utf8_lossy(&run.stdout), "stderr:\n{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}
