//! Emitted code answers `source_location` / `const_source_location`
//! with the authored file under `Rails.root`, as natively. Core's
//! MochaStrictStubbing keeps legacy Mocha stubbing for tests whose
//! source paths are in an opted-out component (`components/checkouts/…`);
//! in the emitted tree every test runs from `test/models/…`, so strict
//! stubbing applied and never-called stubs raised at teardown.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const HELPER: &str = "module CheckoutPathsHelper\n  def checkout_kind\n    :checkout\n  end\nend\n";

const WIDGET: &str = "class Widget\n  def self.kind\n    :widget\n  end\nend\n";

const TEST: &str = r##"require "test_helper"
require_relative "../support/helpers/checkout_paths_helper"

# The component's own test.
class StubPathsTest < ActiveSupport::TestCase
  include CheckoutPathsHelper

  test "source paths name the authored files" do
    root = "#{Rails.root}/"
    assert_equal :checkout, checkout_kind
    assert_equal "components/checkouts/test/models/stub_paths_test.rb", method(name).source_location.first.delete_prefix(root)
    assert_equal ["components/checkouts/test/models/stub_paths_test.rb", 5], [Object.const_source_location("StubPathsTest").first.delete_prefix(root), Object.const_source_location("StubPathsTest").last]
    assert_equal ["components/checkouts/test/support/helpers/checkout_paths_helper.rb", 1], [Object.const_source_location("CheckoutPathsHelper").first.delete_prefix(root), Object.const_source_location("CheckoutPathsHelper").last]
    assert_equal "app/models/widget.rb", Object.const_source_location("Widget").first.delete_prefix(root)
    assert_equal "components/checkouts/test/support/helpers/checkout_paths_helper.rb", CheckoutPathsHelper.instance_method(:checkout_kind).source_location.first.delete_prefix(root)
    assert File.expand_path(Rails.root.to_s).start_with?("/")
  end
end
"##;

#[test]
fn moved_test_files_answer_their_authored_source_paths() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/models/widget.rb", WIDGET)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("components/checkouts/test/support/helpers/checkout_paths_helper.rb", HELPER)
        .write("components/checkouts/test/models/stub_paths_test.rb", TEST)
        .write(
            "roundhouse.yml",
            "test_paths:\n  - components/checkouts/test/models\n  - components/checkouts/test/support/helpers\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).unwrap()).unwrap();
    let file = index["sources"][0]["classes"][0]["file"].as_str().unwrap().to_string();
    assert_ne!(file, "components/checkouts/test/models/stub_paths_test.rb", "the test file did not move");

    let table = std::fs::read_to_string(emitted.join("runtime/roundhouse_source_paths.rb")).expect("source path table");
    assert!(
        table.contains(&format!("{file:?} => \"components/checkouts/test/models/stub_paths_test.rb\",")),
        "{table}"
    );
    assert!(
        table.contains("\"CheckoutPathsHelper\" => [\"components/checkouts/test/support/helpers/checkout_paths_helper.rb\", 1, true].freeze,"),
        "{table}"
    );
    let helper = std::fs::read_to_string(emitted.join("test/test_helper.rb")).unwrap();
    assert!(helper.contains("require_relative \"../runtime/source_locations\""));

    let results = emitted.join("results.jsonl");
    let out = emit_and_run::ruby()
        .args(["-Itest", "-e", "load ARGV.fetch(0)", &file])
        .current_dir(&*emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .output()
        .expect("emitted CRuby");
    let log = format!("stdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{log}");
    let rows = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    let row: serde_json::Value = serde_json::from_str(rows.lines().next().expect("one row")).unwrap();
    assert_eq!(row["status"], "pass", "{rows}\n{log}");
}

#[test]
fn an_app_whose_tests_stay_put_gets_no_table() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/models/widget.rb", WIDGET)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "test/models/widget_test.rb",
            "require \"test_helper\"\n\nclass WidgetTest < ActiveSupport::TestCase\n  test \"kind\" do\n    assert_equal :widget, Widget.kind\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(!emitted.join("runtime/roundhouse_source_paths.rb").exists());
    assert!(!std::fs::read_to_string(emitted.join("test/test_helper.rb")).unwrap().contains("source_locations"));
}

const HOOK: &str = "require \"minitest/test\"\n\nmodule StrictHook\n  def before_setup\n    @hooked = :before_setup\n    super\n  end\nend\n\nMinitest::Test.prepend(StrictHook)\n";

/// Core's support initializers prepend to Minitest::Test as they load
/// (`Minitest::Test.prepend(MochaStrictStubbing::TestHook)`); natively
/// ActiveSupport::TestCase descends from it, so the hook runs for every
/// test. The emitted TestBase is not a Minitest::Test.
#[test]
fn a_module_prepended_to_minitest_test_runs_for_emitted_tests() {
    let test = "require \"test_helper\"\nrequire_relative \"../support/strict_hook\"\n\nclass HookedTest < ActiveSupport::TestCase\n  test \"the hook ran\" do\n    assert_equal :before_setup, @hooked\n  end\nend\n";
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/support/strict_hook.rb", HOOK)
        .write("test/models/hooked_test.rb", test)
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let results = emitted.join("results.jsonl");
    let out = emit_and_run::ruby()
        .args(["-Itest", "-e", "load ARGV.fetch(0)", "test/models/hooked_test.rb"])
        .current_dir(&*emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .output()
        .expect("emitted CRuby");
    let log = format!("stdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let rows = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    let row: serde_json::Value = serde_json::from_str(rows.lines().next().expect("one row")).unwrap();
    assert_eq!(row["status"], "pass", "{rows}\n{log}");
}

const SOURCE_TEST: &str = r##"require "test_helper"
require "method_source"

class SourceTextTest < ActiveSupport::TestCase
  def preload_marker
    assert_no_resolve_queries_marker
  end

  test "method source reads the emitted text of a moved test" do
    assert_includes method(:preload_marker).source, "assert_no_resolve_queries_marker"
    assert_equal "components/checkouts/test/models/source_text_test.rb", method(:preload_marker).source_location.first.delete_prefix("#{Rails.root}/")
    assert_includes self.class.methods_matching(/^test_/).first, "method_source_reads"
  end
end
"##;

/// method_source's `Method#source` opens the file `source_location`
/// names, which is the authored one (absent from the tree, and its lines
/// are not the emitted ones): Core's GraphQLTestHelper raised "Could not
/// load source" from it for every moved test.
#[test]
fn method_source_reads_the_emitted_text_of_a_moved_test() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("components/checkouts/test/models/source_text_test.rb", SOURCE_TEST)
        .write("roundhouse.yml", "test_paths:\n  - components/checkouts/test/models\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).unwrap()).unwrap();
    let file = index["sources"][0]["classes"][0]["file"].as_str().unwrap().to_string();
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let results = emitted.join("results.jsonl");
    let out = emit_and_run::ruby()
        .args(["-Itest", &format!("-I{gem_home}/gems/method_source-1.1.0/lib"), "-e", "load ARGV.fetch(0)", &file])
        .current_dir(&*emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .output()
        .expect("emitted CRuby");
    let log = format!("stdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{log}");
    let rows = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    let row: serde_json::Value = serde_json::from_str(rows.lines().next().expect("one row")).unwrap();
    assert_eq!(row["status"], "pass", "{rows}\n{log}");
}
