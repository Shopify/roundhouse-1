//! `tests SomeController` in an `ActionController::TestCase` names the
//! controller class (actionpack's `TestCase.tests`), as the class name's
//! `FooControllerApiTest` -> `FooControllerApi` default cannot. The emitted
//! test class dropped the macro, so `@controller` stayed nil and `process`
//! raised "@controller is nil: make sure you set it in your test's setup
//! method" (e2e v2-r39: Admin::OrderRisksControllerApiTest, 3 ids).
//!
//! `request.uuid` / `request_id` (ActionDispatch::Request, set by the
//! RequestId middleware) was also missing on the test request, so Core's
//! `add_request_id_to_log_context` raised NoMethodError (8 ids).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const APPLICATION: &str = "class ApplicationController < ActionController::Base\nend\n";

const THINGS: &str = "module Admin\n  class ThingsController < ApplicationController\n    def index\n      head :ok\n    end\n  end\nend\n";

const ROUTES: &str = "Rails.application.routes.draw do\n  namespace :admin do\n    resources :things, only: [:index]\n  end\nend\n";

const SCHEMA: &str = "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\nend\n";

const API_TEST: &str = r#"require "test_helper"

module Admin
  class ThingsControllerApiTest < ActionController::TestCase
    tests Admin::ThingsController

    test "index" do
      get :index
      assert_response :ok
    end
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/controllers/application_controller.rb", APPLICATION)
        .write("app/controllers/admin/things_controller.rb", THINGS)
        .write("config/routes.rb", ROUTES)
        .write("db/schema.rb", SCHEMA)
}

#[test]
fn tests_macro_is_replayed_in_the_emitted_controller_test_class() {
    let (emitted, errors) = app().write("test/controllers/admin/things_controller_api_test.rb", API_TEST).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let test = std::fs::read_to_string(emitted.join("test/models/admin_things_controller_api_test.rb")).unwrap();
    assert_eq!(test.matches("tests(Admin::ThingsController)").count() + test.matches("tests Admin::ThingsController").count(), 1, "{test}");
}

const REQUEST_TEST: &str = r#"require "test_helper"

class RequestUuidTest < ActiveSupport::TestCase
  test "the test request has a request id like ActionDispatch::RequestId's" do
    request = ActionController::TestRequest.create(Object)
    id = request.uuid
    assert_match(/\A\h{8}-\h{4}-\h{4}-\h{4}-\h{12}\z/, id)
    assert_equal id, request.request_id
    assert_equal id, request.get_header("action_dispatch.request_id")
  end

  test "ip skips trusted proxies like Rack::Request#ip" do
    request = ActionController::TestRequest.create(Object)
    assert_equal "0.0.0.0", request.ip
    request.set_header("REMOTE_ADDR", "203.0.113.9")
    assert_equal "203.0.113.9", request.ip
    request.set_header("REMOTE_ADDR", "127.0.0.1")
    request.set_header("HTTP_X_FORWARDED_FOR", "198.51.100.7, 10.0.0.2")
    assert_equal "198.51.100.7", request.ip
  end

  test "request_id= sets the id the readers answer" do
    request = ActionController::TestRequest.create(Object)
    request.request_id = "fixed-id"
    assert_equal "fixed-id", request.uuid
    assert_equal "fixed-id", request.request_id
  end

  test "X-Request-Id is honoured, sanitized" do
    request = ActionController::TestRequest.create(Object)
    request.set_header("HTTP_X_REQUEST_ID", "abc-123 !!")
    assert_equal "abc-123", request.uuid
    assert_equal "abc-123", request.request_id
  end
end
"#;

#[test]
fn test_request_answers_uuid_and_request_id() {
    app().write("test/models/request_uuid_test.rb", REQUEST_TEST).run_test("test/models/request_uuid_test.rb").assert_passes();
}
