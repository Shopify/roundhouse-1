//! Test-support helper modules: module-only files under a test path
//! (Core's `components/*/test/support/helpers/**`), which tests
//! `require "support/helpers/…"` and `include` / `extend`. They are
//! carried as library modules the emitted tests load, so the tests run
//! with the same ids and outcomes as natively (a native Minitest oracle
//! over the same sources), and an `extend` of a module the app does not
//! declare is refused by name instead of dropped.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

/// `Apps::TestHelper`'s shape: a namespaced helper with instance helpers,
/// a constant, and a nested `ClassMethods` the test extends itself with.
const FOO_TEST_HELPER: &str = r##"
module Foo
  module TestHelper
    GREETING = "hello"

    def make_x(n)
      "x#{n}"
    end

    def greeting
      GREETING + "!"
    end

    module ClassMethods
      def widget_kind
        :sprocket
      end
    end
  end
end
"##;

const WIDGET_TEST: &str = r##"
module Foo
  class WidgetTest < ActiveSupport::TestCase
    include Foo::TestHelper
    extend Foo::TestHelper::ClassMethods

    test "uses an instance helper" do
      assert_equal "x3", make_x(3)
      assert_equal "hello!", greeting
    end

    test "uses a class helper" do
      assert_equal :sprocket, self.class.widget_kind
      assert_equal "hello", Foo::TestHelper::GREETING
    end
  end
end
"##;

/// A helper `ActiveSupport::Concern` (`Sales::SaleTestHelper`'s shape):
/// its `class_methods do` and `ClassMethods` reach the including test
/// class, and an `included do` method its instances.
const CONCERN_HELPER: &str = r##"
module Bar
  module ConcernHelper
    extend ActiveSupport::Concern

    included do
      def thing_from_included
        :ti
      end
    end

    class_methods do
      def from_class_methods_do
        :cm_do
      end
    end

    module ClassMethods
      def from_class_methods_module
        :cm_mod
      end
    end

    def inst
      :inst
    end
  end
end
"##;

/// A top-level helper (`TouchHelper`) included from a test nested in
/// another namespace, whose file requires a sibling helper by its
/// test-load-path feature.
const TOUCH_HELPER: &str = r##"
module TouchHelper
  def touched
    :touched
  end

  def concern_helper_loaded
    defined?(Bar::ConcernHelper) ? :yes : :no
  end
end
"##;

const BAR_TEST: &str = r##"
module Admin
  class BarTest < ActiveSupport::TestCase
    include Bar::ConcernHelper
    include TouchHelper

    test "reaches the concern and the top-level helper" do
      assert_equal :inst, inst
      assert_equal :cm_mod, self.class.from_class_methods_module
      assert_equal :cm_do, self.class.from_class_methods_do
      assert_equal :ti, thing_from_included
      assert_equal :touched, touched
      assert_equal :yes, concern_helper_loaded
    end
  end
end
"##;

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

/// `id => status` for every test Minitest runs natively in `classes`,
/// the sources loaded in order (as their `require`s would).
fn native_ids(sources: &[&str], classes: &[&str]) -> BTreeMap<String, String> {
    let mut oracle = format!(
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\n",
        minitest_version()
    );
    for source in sources {
        oracle.push_str(source);
    }
    oracle.push_str(&format!(
        r##"
[{}].each do |klass|
  klass.runnable_methods.each do |name|
    result = klass.new(name).run
    status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
    puts JSON.generate(id: "#{{klass.name}}##{{name}}", status: status)
  end
end
"##,
        classes.join(", ")
    ));
    let out = emit_and_run::ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    rows(&String::from_utf8_lossy(&out.stdout))
}

fn rows(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        assert!(map.insert(id.clone(), row["status"].as_str().unwrap().to_string()).is_none(), "{id} reported twice");
    }
    map
}

/// Run every emitted test file listed in `test/roundhouse_tests.json`,
/// one process each as the target runner does; `id => status`.
fn emitted_ids(emitted: &std::path::Path) -> BTreeMap<String, String> {
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index"))
            .unwrap();
    let mut files: Vec<String> = json["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["classes"].as_array().unwrap().iter().map(|c| c["file"].as_str().unwrap().to_string()))
        .collect();
    files.sort();
    files.dedup();
    let mut ran = BTreeMap::new();
    for file in files {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!(
            "{file}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{log}");
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    ran
}

#[test]
fn component_helper_module_is_included_and_its_class_methods_extended() {
    let native = native_ids(&[FOO_TEST_HELPER, WIDGET_TEST], &["Foo::WidgetTest"]);
    let want: Vec<String> =
        ["Foo::WidgetTest#test_uses_a_class_helper", "Foo::WidgetTest#test_uses_an_instance_helper"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    assert_eq!(native.keys().cloned().collect::<Vec<_>>(), want, "native oracle");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        // As the Core slice lists the directories of required test files.
        .write("roundhouse.yml", "test_paths:\n  - components/foo/test/support/helpers\n  - components/foo/test/models/foo\n")
        .write(
            "components/foo/test/support/helpers/foo/test_helper.rb",
            &format!("# frozen_string_literal: true\n{FOO_TEST_HELPER}"),
        )
        .write(
            "components/foo/test/models/foo/widget_test.rb",
            &format!("# frozen_string_literal: true\n\nrequire \"test_helper\"\nrequire \"support/helpers/foo/test_helper\"\n{WIDGET_TEST}"),
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    // The helper is a library module of the app, not part of a test.
    let helper = std::fs::read_to_string(emitted.join("app/models/foo/test_helper.rb")).expect("emitted helper module");
    assert!(helper.contains("def make_x"), "{helper}");
    assert_eq!(emitted_ids(&emitted), native, "emitted results vs native");
}

#[test]
fn helper_concern_and_required_top_level_helper_reach_a_nested_test() {
    let native = native_ids(&[CONCERN_HELPER, TOUCH_HELPER, BAR_TEST], &["Admin::BarTest"]);
    assert_eq!(
        native.keys().cloned().collect::<Vec<_>>(),
        vec!["Admin::BarTest#test_reaches_the_concern_and_the_top-level_helper".to_string()],
        "native oracle"
    );
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n")
        .write("test/support/helpers/bar/concern_helper.rb", CONCERN_HELPER)
        // Required by the test-load-path feature, not relative.
        .write(
            "test/support/helpers/touch_helper.rb",
            &format!("require \"support/helpers/bar/concern_helper\"\n{TOUCH_HELPER}"),
        )
        // The test requires only the helper that requires the concern.
        .write(
            "test/models/bar_test.rb",
            &format!("require \"test_helper\"\nrequire \"support/helpers/touch_helper\"\n{BAR_TEST}"),
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    // The helper file's program loads its required sibling.
    let program =
        std::fs::read_to_string(emitted.join("app/models/roundhouse_files/test/support/helpers/touch_helper.rb"))
            .expect("touch_helper program");
    assert!(program.contains("require_relative \"../../../../bar/concern_helper\""), "{program}");
    assert_eq!(emitted_ids(&emitted), native, "emitted results vs native");
}

#[test]
fn extend_of_a_module_the_app_does_not_declare_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!("rh-tsh-refuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, content) in [
        ("app/controllers/application_controller.rb", APP_CONTROLLER),
        ("config/routes.rb", ROUTES),
        (
            "test/models/gem_test.rb",
            "require \"test_helper\"\n\nclass GemTest < ActiveSupport::TestCase\n  extend SomeGem::Macros\n\n  test \"x\" do\n    assert true\n  end\nend\n",
        ),
    ] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let result = roundhouse::ingest::ingest_app(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let err = result.err().expect("the extend is refused, not dropped");
    let message = err.to_string();
    assert!(
        message.contains("test class `GemTest` extends `SomeGem::Macros`, which is not a module the tree carries"),
        "{message}"
    );
}
