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
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into())
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
         $LOAD_PATH.unshift {lib:?}\n$LOAD_PATH.unshift {models:?}\nrequire \"frozen_record\"\nrequire \"country\"\nrequire \"fr_probe\"\nputs FrProbe.lines\n",
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
    for dropped in ["base.rb:210", "scope.rb:12"] {
        assert!(!log.lines().any(|l| l.contains(dropped) && l.contains("dropped")), "{dropped}:\n{log}");
    }
    let base = std::fs::read_to_string(out_dir.join("app/models/frozen_record/base.rb")).unwrap();
    let alias = base.find("singleton_class.alias_method :load, :new").expect("the load alias");
    assert!(alias < base.find("def self.new(").expect("Base.new"), "{base}");

    let ruby_path = Command::new("ruby").args(["-e", "print Gem.path.join(\":\")"]).output().unwrap();
    let gem_path = format!("{}:{}", core_gem_home(), String::from_utf8_lossy(&ruby_path.stdout));
    let run = Command::new("ruby")
        .args(["-e", "require \"./boot\"; puts FrProbe.lines"])
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
