//! dRuby, as Shopify Core's test parallelization reaches it at boot
//! (`components/platform/essentials/lib/active_support/testing/
//! parallelization_event_instrumentation.rb`): `DRb.stop_service`,
//! `DRbObject.new_with_uri(url)`, `DRb::DRbRemoteError.new(e)`.
//!
//! drb is a bundled gem since Ruby 3.4, so the stdlib-gem fate that
//! keeps a stdlib RBI out of the analysis does not apply: the CRuby tree
//! loads the gem (`project.rs::RUNTIME_GEMS`), its own RBI types it, and
//! every other tree refuses the constant as the gem's.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    armor (1.0)\n    drb (2.2.3)\n\nDEPENDENCIES\n  activesupport\n  armor\n  drb\n";

/// The part of Tapioca's drb@2.2.3 RBI this app reaches, in its shape:
/// the top-level `DRbObject` alias and the error class beside it.
const DRB_RBI: &str = r#"# typed: true
module DRb
  private

  def current_server; end
  def start_service(uri = T.unsafe(nil), front = T.unsafe(nil), config = T.unsafe(nil)); end
  def stop_service; end
  def uri; end

  class << self
    def current_server; end
    def start_service(uri = T.unsafe(nil), front = T.unsafe(nil), config = T.unsafe(nil)); end
    def stop_service; end
    def uri; end
  end
end

class DRb::DRbObject
  def initialize(obj, uri = T.unsafe(nil)); end
  def method_missing(msg_id, *a, **_arg2, &b); end

  class << self
    def new_with_uri(uri); end
  end
end

class DRb::DRbRemoteError < ::DRb::DRbError
  def initialize(error); end
  def reason; end
end

DRbObject = DRb::DRbObject
"#;

/// A gem nothing loads that reopens drb's class, and sorts before it.
const ARMOR_RBI: &str = "# typed: true\nclass DRb::DRbObject\n  def armored; end\nend\n";

fn drb_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/drb@2.2.3.rbi", DRB_RBI)
        .write("sorbet/rbi/gems/armor@1.0.rbi", ARMOR_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/echo.rb", "class Echo\n  def shout(word)\n    word.upcase\n  end\nend\n")
        .write("lib/relay.rb", r#"class Relay
  def initialize(url)
    @url = url
  end

  def serve(front)
    DRb.start_service(@url, front)
  end

  def call(word)
    DRbObject.new_with_uri(@url).shout(word)
  end

  def remote_error(error)
    DRb::DRbRemoteError.new(error)
  end

  def stop
    DRb.stop_service
  end
end
"#)
        .write("test/relay_test.rb", r#"require "test_helper"
require "relay"
class RelayTest < ActiveSupport::TestCase
  test "a dRuby round trip over a unix socket" do
    relay = Relay.new("drbunix:#{File.join(ENV.fetch("TMPDIR", "/tmp"), "rh_drb_#{Process.pid}.sock")}")
    relay.serve(Echo.new)
    assert_equal "HI", relay.call("hi")
    assert_equal "RuntimeError", relay.remote_error(RuntimeError.new("boom")).reason
  ensure
    relay&.stop
  end
end
"#)
}

/// Opens a unix socket under $TMPDIR; a sandbox that forbids
/// `socket(2)` fails it with "Operation not permitted".
#[test]
fn drb_runs_in_emitted_ruby() {
    drb_app().run_test("test/models/relay_test.rb").assert_passes();
}

#[test]
fn the_emitted_ruby_tree_declares_and_loads_drb() {
    let (dir, errors) = drb_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(gemfile.contains("gem \"drb\""), "{gemfile}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("\"drb\""), "{facades}");
}

/// Named as drb's, not as the unloaded gem's that also declares it.
#[test]
fn drb_is_refused_as_the_gems_where_no_tree_loads_it() {
    let (_, errors) = drb_app().emit(BuildTarget::Rust);
    for konst in ["DRb::DRbRemoteError", "DRbObject", "DRb"] {
        let needle = format!("{konst} is declared by the drb gem");
        assert!(errors.iter().any(|e| e.contains(&needle)), "{konst}: {errors:#?}");
    }
    assert!(!errors.iter().any(|e| e.contains("armor gem")), "{errors:#?}");
}
