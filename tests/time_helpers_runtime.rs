//! ActiveSupport::Testing::TimeHelpers (runtime/spinel/
//! active_support_testing_time_helpers.rb) against activesupport 8.1.4:
//! tests/time_helpers_runtime.rb runs the same observations natively and
//! on the runtime port and requires them equal. The emitted-tree tests
//! hold the wiring: a CRuby tree whose tests freeze time carries the port,
//! its TestBase includes it in place of the offset stand-in, and the
//! runner's after_teardown travels back between tests.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;
use std::process::Command;

#[test]
fn time_helpers_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/time_helpers_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the emitted TimeHelpers agree"),
        "TimeHelpers diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(stdout.lines().count() > 45, "too few observations:\n{stdout}");
}

const CLOCK_TEST: &str = r#"require "test_helper"

class ClockTest < ActiveSupport::TestCase
  def test_freeze_time_holds_the_clock
    freeze_time
    first = Time.now
    sleep 0.01
    assert_equal first, Time.now
    assert_equal 0, first.usec
  end

  def test_travel_to_a_block_and_back
    travel_to Time.utc(2001, 2, 3, 4, 5, 6) do
      assert_equal 981173106, Time.now.to_i
      assert_equal Date.new(2001, 2, 3), Date.today
    end
    assert Time.now.year > 2001
  end

  def test_travel_moves_from_the_frozen_instant
    travel_to Time.utc(2001, 2, 3, 4, 5, 6)
    travel 60
    assert_equal 981173166, Time.now.to_i
    travel_back
    assert Time.now.year > 2001
  end

  # The runner runs these in source order: the stub this one leaves is
  # gone by the next test, as Minitest's after_teardown leaves it.
  def test_travels_without_coming_back
    travel_to Time.utc(2001, 2, 3, 4, 5, 6)
    assert_equal 2001, Time.now.year
  end

  def test_the_clock_is_real_again
    assert Time.now.year > 2001
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :probes do |t|\n    t.string :name\n  end\nend\n")
        .write("test/models/clock_test.rb", CLOCK_TEST)
}

#[test]
fn a_cruby_tree_whose_tests_freeze_time_runs_them_on_the_port() {
    let run = app().run_test("test/models/clock_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("ClockTest: 5 tests passed"), "{}", run.stdout);
}

#[test]
fn the_port_replaces_the_stand_in_only_where_carried() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let helper = std::fs::read_to_string(emitted.join("test/test_helper.rb")).unwrap();
    assert!(helper.contains("include ActiveSupport::Testing::TimeHelpers"), "{helper}");
    assert!(!helper.contains("ActiveSupport.travel(target.to_i - Time.now.to_i)"), "{helper}");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"active_support_testing_time_helpers\""), "{facades}");
    assert!(emitted.join("runtime/active_support_testing_time_helpers.rb").exists());

    let plain = "require \"test_helper\"\n\nclass PlainTest < ActiveSupport::TestCase\n  def test_one\n    travel_to Time.utc(2001, 2, 3)\n    assert true\n  end\nend\n";
    let (emitted, errors) = app().write("test/models/clock_test.rb", plain).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let helper = std::fs::read_to_string(emitted.join("test/test_helper.rb")).unwrap();
    assert!(!helper.contains("include ActiveSupport::Testing::TimeHelpers"), "{helper}");
    assert!(!emitted.join("runtime/active_support_testing_time_helpers.rb").exists());
}

// An app class that includes the module, as Core's
// PanamaClient::TestFakeClient does: the include resolves to the carried
// port on CRuby, and a tree without the file refuses it by name.
const FAKE_CLOCK: &str = r#"class FakeClock
  include ActiveSupport::Testing::TimeHelpers

  def stopped_at(time)
    travel_to(time) { Time.now.to_i }
  end
end
"#;

const FAKE_CLOCK_TEST: &str = r#"require "test_helper"

class FakeClockTest < ActiveSupport::TestCase
  def test_an_app_class_travels_with_the_port
    clock = FakeClock.new
    assert_equal 981173106, clock.stopped_at(Time.utc(2001, 2, 3, 4, 5, 6))
    assert Time.now.year > 2001
  end
end
"#;

fn fake_clock_app() -> emit_and_run::Overlay {
    app()
        .remove("test/models/clock_test.rb")
        .write("app/models/fake_clock.rb", FAKE_CLOCK)
        .write("test/models/fake_clock_test.rb", FAKE_CLOCK_TEST)
}

#[test]
fn an_app_class_including_the_module_runs_on_cruby() {
    let run = fake_clock_app().run_test("test/models/fake_clock_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("FakeClockTest: 1 tests passed"), "{}", run.stdout);
    let model = std::fs::read_to_string(run.emitted.join("app/models/fake_clock.rb")).unwrap();
    assert!(!model.contains("include not supported"), "{model}");
}

#[test]
fn a_tree_without_the_port_refuses_the_include_by_name() {
    let (_, errors) = fake_clock_app().emit(BuildTarget::Spinel);
    assert!(
        errors.iter().any(|e| e.contains("FakeClock includes ActiveSupport::Testing::TimeHelpers")
            && e.contains("spinel")),
        "{errors:#?}"
    );
}
