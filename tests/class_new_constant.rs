//! `NAME = Class.new(Parent)` with no block is a class declaration:
//! the shape Shopify Core writes its exception hierarchies in
//! (`Error = Class.new(StandardError)`, `Sub = Class.new(Error)`).
//! Ingest turns it into the `class NAME < Parent; end` it stands for.
//! A block body or a non-constant parent is not that shape and stays
//! an error.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn errors_app(lib: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/foo.rb", lib)
}

#[test]
fn class_new_exception_hierarchy_runs_in_emitted_ruby() {
    errors_app(r#"Boom = Class.new(::RuntimeError)
module Foo
  Zed = Class.new(StandardError)
  Abc = Class.new(Zed)
  class Real < Abc; end
  def self.boom(kind)
    case kind
    when :abc then raise Abc, "abc"
    when :real then raise Real, "real"
    else raise Boom, "boom"
    end
  end
end
class Service
  Error = Class.new(ArgumentError)
  def call
    raise Error, "bad"
  end
end
"#)
    .write("test/foo_test.rb", r#"require "test_helper"
require "foo"
class FooTest < ActiveSupport::TestCase
  test "hierarchy" do
    e = assert_raises(Foo::Zed) { Foo.boom(:abc) }
    assert_equal "abc", e.message
    assert_kind_of Foo::Abc, e
    assert_kind_of StandardError, e
    assert_equal "Foo::Abc", e.class.name
    assert_equal Foo::Zed, Foo::Abc.superclass
    real = assert_raises(Foo::Abc) { Foo.boom(:real) }
    assert_equal "real", real.message
    boom = assert_raises(RuntimeError) { Foo.boom(:other) }
    assert_equal "Boom", boom.class.name
    err = assert_raises(Service::Error) { Service.new.call }
    assert_kind_of ArgumentError, err
    assert_equal "bad", err.message
  end
end
"#)
    .run_test("test/models/foo_test.rb")
    .assert_passes();
}

fn go_test() -> &'static str {
    "require \"test_helper\"\nrequire \"foo\"\nclass FooTest < ActiveSupport::TestCase\n  test \"x\" do\n    Foo.go\n  end\nend\n"
}

#[test]
fn class_new_with_a_dynamic_or_missing_parent_stays_an_error() {
    let (_, errors) = errors_app(r#"module Foo
  def self.base
    StandardError
  end
  Dynamic = Class.new(base)
  Bare = Class.new
  def self.go
    [Dynamic, Bare]
  end
end
"#)
    .write("test/foo_test.rb", go_test())
    .emit(BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("Dynamic")), "a computed parent is not admitted: {errors:#?}");
    assert!(errors.iter().any(|e| e.contains("Bare")), "a parentless Class.new is not admitted: {errors:#?}");
}

#[test]
fn class_new_with_a_block_body_stays_an_error() {
    // The block's `def` is refused before this shape is read, so the
    // error is at the module's first reader rather than at `Blocky`.
    let (_, errors) = errors_app(r#"module Foo
  Blocky = Class.new(StandardError) do
    def code
      1
    end
  end
  def self.go
    Blocky
  end
end
"#)
    .write("test/foo_test.rb", go_test())
    .emit(BuildTarget::Ruby);
    assert!(!errors.is_empty(), "a Class.new block body is not admitted");
}
