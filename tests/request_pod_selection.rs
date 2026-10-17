//! The emitted controller-test harness runs a request in the pod Core's
//! test environment would route it to. Natively `ProcessWithNoPodSelected`
//! unselects every pod around `process` and the mock sorting-hat router
//! selects the shop's again, so `raise_if_pod_not_selected` never raises
//! `ShopMissingError` for a request that names a shop. The emitted
//! harness called `process_action` directly, so no pod was ever selected
//! and every controller test of a pod-routed controller raised it.
//!
//! `RequestDispatch.with_request_pod` is run from the runtime source, with
//! a `ShopRoute` table and a selector that record what they are given.

const TEST_HELPER: &str = include_str!("../runtime/spinel/test/test_helper.rb");
const TEST_CASE: &str = include_str!("../runtime/spinel/test/action_controller_test_case.rb");

#[test]
fn both_dispatch_paths_select_the_request_pod() {
    assert_eq!(TEST_HELPER.matches("RequestDispatch.with_request_pod(merged, controller.request) {").count(), 2);
    assert!(TEST_CASE.contains("::RequestDispatch.with_request_pod(merged, request) { controller.process_action(action.to_sym) }"));
}

fn helper_source() -> String {
    let start = TEST_HELPER.find("  def self.with_request_pod").expect("with_request_pod");
    let end = TEST_HELPER[start..].find("\n  end\n").expect("end of with_request_pod") + start + "\n  end\n".len();
    format!("module RequestDispatch\n{}end\n", &TEST_HELPER[start..end])
}

fn run(script_body: &str) -> String {
    let script = format!(
        r#"{helper}
class ShopRoute
  ROWS = [Struct.new(:shop_id, :host, :pod_id).new(7, "shop.example.com", 3)]
  def self.find_by(shop_id: nil, host: nil)
    ROWS.find {{ |r| (shop_id && r.shop_id == shop_id) || (host && r.host == host) }}
  end
end
module Podding; module SortingHat
  class SelectorMiddleware
    SECTION_HEADER = "section"; POD_ID_HEADER = "pod"; SHOP_ID_HEADER = "shop"
    class << self; attr_accessor :selected; end
    def initialize(app) = @app = app
    def call(env)
      self.class.selected = [env["pod"], env["shop"]]
      @app.call(env)
    ensure
      self.class.selected = nil
    end
  end
end; end
Request = Struct.new(:host, :env)
sel = Podding::SortingHat::SelectorMiddleware
{body}
"#,
        helper = helper_source(),
        body = script_body
    );
    let output = std::process::Command::new("ruby").args(["-e", &script]).output().unwrap();
    assert!(output.status.success(), "{}\n{script}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_shop_parameter_or_host_selects_the_pod_around_the_block() {
    let out = run(r#"
req = Request.new("other.example.com", {})
p RequestDispatch.with_request_pod({ "shop_id" => "7" }, req) { sel.selected }
p RequestDispatch.with_request_pod({ "id" => 7 }, req) { sel.selected }
p RequestDispatch.with_request_pod({ "shop" => "7" }, req) { sel.selected }
p RequestDispatch.with_request_pod({}, Request.new("SHOP.example.com", {})) { sel.selected }
p sel.selected
"#);
    assert_eq!(out, "[3, 7]\n[3, 7]\n[3, 7]\n[3, 7]\nnil");
}

#[test]
fn a_request_that_names_no_shop_runs_as_it_came() {
    let out = run(r#"
req = Request.new("none.example.com", {})
p RequestDispatch.with_request_pod({ "id" => "x7" }, req) { [sel.selected, :ran] }
p RequestDispatch.with_request_pod({ "shop_id" => "99" }, req) { [sel.selected, :ran] }
p RequestDispatch.with_request_pod({ "shop_id" => "07" }, Request.new(nil, {})) { [sel.selected, :ran] }
"#);
    assert_eq!(out, "[nil, :ran]\n[nil, :ran]\n[nil, :ran]");
}

#[test]
fn a_tree_without_the_sorting_hat_runs_the_block_as_is() {
    let script = format!("{}\np RequestDispatch.with_request_pod({{}}, nil) {{ :ran }}\n", helper_source());
    let output = std::process::Command::new("ruby").args(["-e", &script]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), ":ran");
}
