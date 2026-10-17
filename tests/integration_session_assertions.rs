//! A client built with `ActionDispatch::Integration::Session.new(app)`
//! (Core's `Checkouts::One::Web::Client`): Rails' session includes
//! `Minitest::Assertions` and its verbs answer the response status, so
//! `@session.assert_equal(200, @session.post(url, as: :json))` works. The
//! harness's session had neither (`undefined method 'assert_equal' for an
//! instance of ActionDispatch::Integration::Session`).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_session_asserts_and_its_verbs_answer_the_status() {
    emit_and_run::real_blog()
        .edit(
            "config/routes.rb",
            "  root \"articles#index\"\n",
            "  root \"articles#index\"\n  post \"/probe\", to: \"probes#create\"\n",
        )
        .write(
            "app/controllers/probes_controller.rb",
            r##"class ProbesController < ApplicationController
  skip_forgery_protection

  def create
    render plain: "json"
  end
end
"##,
        )
        .write(
            "test/controllers/probes_controller_test.rb",
            r#"require "test_helper"

class ProbesControllerTest < ActionDispatch::IntegrationTest
  test "a session asserts on its own and answers the status" do
    session = ActionDispatch::Integration::Session.new(nil)
    status = session.post("/probe", params: { a: 1 }, as: :json)
    session.assert_equal(200, status)
    assert_equal "json", session.response.body
    assert_equal 1, session.assertions
  end
end
"#,
        )
        .run_test("test/controllers/probes_controller_test.rb")
        .assert_passes();
}
