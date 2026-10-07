//! Keyword splats and implicit `super`s the shared lowering cannot prove
//! for a target without keyword arguments, which the CRuby tree writes
//! as written.
//!
//! Core's `klass.new(**attrs)`, `I18n.t(key, **options)` and
//! `Struct.new(…, keyword_init: true).new(**h)` name a selector some app
//! `def initialize(...)` / `def t(...)` also defines, so the analysis
//! could not tell a full forwarder from an ordinary method and refused
//! the call on every target. Its many input and error classes write
//! `def initialize(obj, context:, …) = super` below an
//! `initialize(*)`, and `def initialize(*) super … end` itself; both
//! were refused too. The Ruby emitter keeps every `**value`, a method's
//! keywords and a zsuper as written, so the interpreter binds each call
//! as the source does. CRuby is the oracle; the other targets name each
//! shape.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const FWD: &str = r#"class Fwd
  def initialize(...)
    @n = 1
  end

  def t(...)
    :fwd_t
  end
end
"#;

const SCHEME: &str = r#"class Scheme
  attr_reader :a, :b

  def initialize(a:, b: 2)
    @a = a
    @b = b
  end

  def self.build(h)
    new(**h)
  end
end
"#;

const ASSET: &str = r#"class Asset
  attr_reader :attrs

  def initialize(**attrs)
    @attrs = attrs
  end
end

class BodyAsset < Asset
  def initialize(body:, image: nil) = super
end

class OptsAsset < Asset
  def initialize(x, **opts)
    super(x: x, **opts)
  end
end
"#;

const RUNNER: &str = r#"Point = Struct.new(:x, :y, keyword_init: true)
Pair = Data.define(:l, :r)

class Translator
  def self.t(key, **opts)
    [key, opts]
  end
end

class Runner
  def self.make(klass, h)
    klass.new(**h)
  end

  def self.go
    opts = { r: 2 }
    [Scheme.build({ a: 1 }).b, BodyAsset.new(body: "x").attrs, OptsAsset.new(1, y: 2).attrs,
     make(Scheme, { a: 7 }).a, Point.new(**{ x: 1, y: 2 }).y, Pair.new(l: 1, **opts).r,
     Translator.t("k", **opts)]
  end
end
"#;

const REST: &str = r#"class RestBase
  attr_reader :sum

  def initialize(a, b = 0)
    @sum = a + b
  end

  def m(*args)
    args
  end
end

class RestKid < RestBase
  attr_reader :kid

  def initialize(*)
    super
    @kid = true
  end

  def m(*)
    super
  end
end

class Opener
  attr_reader :opened

  def initialize(*)
    @opened = true
  end

  def m(*, **kw)
    kw
  end
end

class KeyOpener < Opener
  attr_reader :code

  def initialize(code:, size: 1)
    super
    @code = code
  end

  def m(a, k: 1)
    super
  end
end
"#;

fn native(sources: &[&str], script: &str) -> String {
    let out = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{}\n{script}", sources.join("\n")))
        .output()
        .expect("native Ruby control");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn keyword_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/fwd.rb", FWD)
        .write("app/models/scheme.rb", SCHEME)
        .write("app/models/asset.rb", ASSET)
        .write("app/models/runner.rb", RUNNER)
}

/// `**value` into `new` while an app `initialize(...)` exists, from a
/// receiver the analysis does not resolve (`klass.new`, a Struct or
/// Data class), beside the resolved sends and `super`s around them.
#[test]
fn keyword_splats_beside_a_full_forwarder_run_as_written_on_cruby() {
    let script = "p Runner.go\n";
    let expected = native(&[FWD, SCHEME, ASSET, RUNNER], script);
    assert_eq!(
        expected,
        "[2, {body: \"x\", image: nil}, {x: 1, y: 2}, 7, 2, 2, [\"k\", {r: 2}]]\n"
    );
    let run = keyword_app().run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// Off the Ruby family the unresolved sends are not shown to bind either
/// way; the resolved ones keep the shared lowering.
#[test]
fn keyword_splats_beside_a_full_forwarder_are_refused_off_the_ruby_family() {
    // Spinel carries Struct/Data factory classes; the other non-Ruby
    // targets refuse the factory class first.
    for target in [BuildTarget::Spinel] {
        let (_, errors) = keyword_app().emit(target);
        let refused = errors.iter().filter(|e| e.contains("keyword splat into full argument forwarding")).count();
        // `klass.new(**h)`, `Point.new(**…)` and `Pair.new(…, **opts)`.
        assert_eq!(refused, 3, "{target:?}: {errors:?}");
    }
}

/// An implicit `super` from a nameless rest, and from an override with
/// keywords into one, pass the arguments on as Ruby does.
#[test]
fn implicit_super_from_and_into_a_nameless_rest_runs_on_cruby() {
    let script = "k = KeyOpener.new(code: \"c\")\n\
                  p [RestKid.new(1, 2).sum, RestKid.new(3).kid, RestKid.new(3).m(4, 5), k.opened, k.code, k.m(1), k.m(1, k: 3)]\n";
    let expected = native(&[REST], script);
    assert_eq!(expected, "[3, true, [4, 5], true, \"c\", {k: 1}, {k: 3}]\n");
    let run = emit_and_run::empty_app().write("app/models/rest.rb", REST).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// No target but the CRuby tree keeps the rest, and elsewhere the
/// override's keywords reach it as positionals.
#[test]
fn implicit_super_into_a_nameless_rest_is_refused_off_cruby() {
    for target in [BuildTarget::Jruby, BuildTarget::Spinel, BuildTarget::Typescript] {
        let (_, errors) = emit_and_run::empty_app().write("app/models/rest.rb", REST).emit(target);
        let declarations = errors.iter().filter(|e| e.contains("anonymous positional rest declaration")).count();
        assert_eq!(declarations, 4, "{target:?}: {errors:?}");
        let keyword_supers = errors
            .iter()
            .filter(|e| e.contains("implicit super with keywords into an anonymous positional rest"))
            .count();
        assert_eq!(keyword_supers, 2, "{target:?}: {errors:?}");
    }
}

const BUILDER: &str = r#"module GraphApi
  class ConnectionDataBuilder
    attr_reader :opts

    def initialize(context:, query_class: nil, sort_key: nil)
      @opts = [context, query_class, sort_key]
    end
  end
end
"#;

const ITEMS_BUILDER: &str = r#"module GraphApi
  module Admin
    class ItemsBuilder < GraphApi::ConnectionDataBuilder
      def initialize(**kwargs)
        super(**{ query_class: :items, **kwargs })
      end
    end
  end
end
"#;

const NESTED: &str = r#"module GraphApi
  class ConnectionDataBuilder
    class CursorPrefetch
      def size = 1
    end
  end
end
"#;

const PREPENDED: &str = r#"module Hook
  def initialize(...)
    super(...)
  end
end

module GraphApi
  class ConnectionDataBuilder
    prepend Hook
  end
end
"#;

fn builder_app(reopen: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/fwd.rb", FWD)
        .write("app/models/graph_api/connection_data_builder.rb", BUILDER)
        .write("app/models/graph_api/admin/items_builder.rb", ITEMS_BUILDER)
        .write("app/models/graph_api/connection_data_builder/cursor_prefetch.rb", reopen)
}

/// Core's `GraphApi::ConnectionDataBuilder` is reopened in
/// `connection_data_builder/cursor_prefetch.rb` only to nest a class.
/// That reopen adds nothing to the class, so the `super(**…)` of its
/// subclasses still resolves to its `initialize`. A reopen that does add
/// something (`prepend Hook`, with a `...` forwarder) still leaves the
/// destination unverified.
#[test]
fn a_namespace_only_reopen_keeps_a_super_destination_resolved() {
    let script = "p GraphApi::Admin::ItemsBuilder.new(context: 1, sort_key: :id).opts, GraphApi::ConnectionDataBuilder::CursorPrefetch.new.size\n";
    let expected = native(&[FWD, BUILDER, ITEMS_BUILDER, NESTED], script);
    assert_eq!(expected, "[1, :items, :id]\n1\n");
    let run = builder_app(NESTED).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let (_, errors) = builder_app(PREPENDED).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("keyword producer's full forwarding destination cannot be verified")),
        "{errors:?}"
    );
}
