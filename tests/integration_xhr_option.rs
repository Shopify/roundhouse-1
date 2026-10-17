//! `xhr: true` on an integration verb (`get "/x", xhr: true`): Rails'
//! `ActionDispatch::Integration::Session#process` marks the request an
//! XMLHttpRequest (`HTTP_X_REQUESTED_WITH`) and, unless the test names an
//! `Accept`, sends the browser's XHR one. The harness's verbs took no
//! `xhr:` keyword (`ArgumentError: unknown keyword: :xhr` out of
//! `RequestDispatch#get`, Core's `LoginTest`).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn an_integration_verb_takes_xhr() {
    emit_and_run::real_blog()
        .edit(
            "config/routes.rb",
            "  root \"articles#index\"\n",
            "  root \"articles#index\"\n  get \"/probe\", to: \"probes#show\"\n",
        )
        .write(
            "app/controllers/probes_controller.rb",
            r##"class ProbesController < ApplicationController
  def show
    response.headers["X-Xhr"] = request.xhr?.to_s
    response.headers["X-Accept"] = request.headers["HTTP_ACCEPT"].to_s
    render plain: "ok"
  end
end
"##,
        )
        .write(
            "test/controllers/probes_controller_test.rb",
            r#"require "test_helper"

class ProbesControllerTest < ActionDispatch::IntegrationTest
  test "xhr: true marks the request" do
    get "/probe", xhr: true
    assert_response :success
    assert_equal "true", response.headers["X-Xhr"]
    assert_equal "text/javascript, text/html, application/xml, text/xml, */*", response.headers["X-Accept"]
  end

  test "an Accept the test names is kept" do
    get "/probe", xhr: true, headers: { "HTTP_ACCEPT" => "application/json" }
    assert_equal "true", response.headers["X-Xhr"]
    assert_equal "application/json", response.headers["X-Accept"]
  end

  test "without xhr the request is a plain one" do
    get "/probe"
    assert_equal "false", response.headers["X-Xhr"]
  end
end
"#,
        )
        .run_test("test/controllers/probes_controller_test.rb")
        .assert_passes();
}
