//! OpenStruct, as Shopify Core's test support reaches it at boot
//! (`lib/graphql_mocks/*.rb` builds mock payloads with
//! `OpenStruct.new({ accountOwner: OpenStruct.new(...) })`).
//!
//! ostruct is a bundled gem since Ruby 4.0, so the CRuby tree declares
//! and loads it (`project.rs::RUNTIME_GEMS`), its own RBI types it, and
//! every other tree refuses the constant as the gem's. An attribute read
//! is answered by the gem's `method_missing`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    ostruct (0.6.3)\n\nDEPENDENCIES\n  activesupport\n  ostruct\n";

/// The part of Tapioca's ostruct@0.6.3 RBI this app reaches.
const OSTRUCT_RBI: &str = r#"# typed: true
class OpenStruct
  def initialize(hash = T.unsafe(nil)); end
  def ==(other); end
  def [](name); end
  def []=(name, value); end
  def dig(name, *names); end
  def to_h(&block); end

  private

  def method_missing(mid, *args); end
end
"#;

fn ostruct_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/ostruct@0.6.3.rbi", OSTRUCT_RBI)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/payload.rb", r#"class Payload
  def account
    OpenStruct.new({
      owner: OpenStruct.new({ name: "Ada" }),
      balance: 12,
    })
  end
end
"#)
        .write("test/payload_test.rb", r#"require "test_helper"
require "payload"
class PayloadTest < ActiveSupport::TestCase
  test "an OpenStruct payload answers its attributes" do
    account = Payload.new.account
    assert_equal "Ada", account.owner.name
    assert_equal 12, account[:balance]
    assert_equal "Ada", account.dig(:owner, :name)
  end
end
"#)
}

#[test]
fn ostruct_runs_in_emitted_ruby() {
    ostruct_app().run_test("test/models/payload_test.rb").assert_passes();
}

#[test]
fn the_emitted_ruby_tree_declares_and_loads_ostruct() {
    let (dir, errors) = ostruct_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gemfile = std::fs::read_to_string(dir.join("Gemfile")).expect("Gemfile");
    assert!(gemfile.contains("gem \"ostruct\""), "{gemfile}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades");
    assert!(facades.contains("\"ostruct\""), "{facades}");
}

#[test]
fn ostruct_is_refused_as_the_gems_where_no_tree_loads_it() {
    let (_, errors) = ostruct_app().emit(BuildTarget::Rust);
    assert!(errors.iter().any(|e| e.contains("OpenStruct is declared by the ostruct gem")), "{errors:#?}");
}
