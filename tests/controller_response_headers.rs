//! Rack::Response::Helpers' header accessors on the controller, which is
//! its own response on the CRuby tree. Core's API deprecation concern
//! writes `response.set_header(name, reason)`; the action raised
//! `undefined method 'set_header' for an instance of Admin::OrderRisksController`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCRIPT: &str = r#"
controller = ArticlesController.new
response = controller.response
raise "absent header" unless response.get_header("X-Deprecated-Reason").nil? && !response.has_header?("X-Deprecated-Reason")
response.set_header("X-Deprecated-Reason", "gone")
raise "set_header lands in headers" unless controller.headers["X-Deprecated-Reason"] == "gone"
raise "get_header" unless response.get_header("X-Deprecated-Reason") == "gone"
raise "case-insensitive get" unless response.get_header("x-deprecated-reason") == "gone"
raise "has_header?" unless response.has_header?("X-Deprecated-Reason")
raise "delete_header answers the value" unless response.delete_header("X-Deprecated-Reason") == "gone"
raise "deleted" if response.has_header?("X-Deprecated-Reason")
puts "response headers passed"
"#;

#[test]
fn a_controller_response_sets_and_reads_headers() {
    emit_and_run::real_blog().run_ruby(SCRIPT).assert_passes();
}
