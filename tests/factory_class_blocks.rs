//! `X = Struct.new(...) do … end`, the `keyword_init:` form and
//! `X = Data.define(...) do … end` define a class whose methods are the
//! block's own defs, with the block's visibility markers and none of an
//! outer `private`. `included do … end` in an `ActiveSupport::Concern`
//! defines its defs on the includer, again with its own markers.
//!
//! CRuby is the oracle: the same visibility report
//! (`public_method_defined?` / `private_method_defined?` /
//! `protected_method_defined?`) runs on the source with native Ruby and on
//! the emitted app, and the two must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::analyze::{Analyzer, diagnose};
use roundhouse::diagnostic::Severity;
use roundhouse::dialect::{LibraryClassOrigin, MethodVisibility};
use roundhouse::ingest::{ingest_app_from_tree, survey};

const CONNECTIONS: &str = r##"module Conf
  class Connections
    def services
      Details.new("h", 1).to_s
    end

    private

    def helper
      Point.new(x: 2).doubled
    end

    Details = Struct.new(:host, :port) do
      def to_s = "#{host}:#{port}"

      private

      def secret = port
    end

    Options = Struct.new(:verbose, keyword_init: true) do
      def self.default_verbose = false

      def loud? = verbose == true
    end

    Point = Data.define(:x) do
      def doubled = x * 2

      def bigger?(other) = guarded > other.guarded

      protected

      def guarded = x
    end

    def after_private = 1
  end

  module Shapes
    Size = Data.define(:w, :h) do
      class << self
        def unit_area = new(w: 1, h: 1).area
      end

      def area = w * h
    end
  end
end

Pair = Struct.new(:left, :right) do
  def swap = Pair.new(right, left)
end
"##;

const GREETING: &str = r##"module Conf
  module Greeting
    extend ActiveSupport::Concern

    private

    included do
      def hello = "hi #{name} #{secret}"

      def self.kind = "greeter"

      private

      def secret = "s"
    end

    class_methods do
      def kind2 = "k2"
    end

    def name = "n"
  end
end
"##;

const HOST: &str = r##"module Conf
  class Host
    include Greeting

    def run = "#{hello} #{Host.kind} #{Host.kind2}"
  end
end
"##;

/// One line per class and method: where Ruby put it.
const REPORT: &str = r##"
def vis(k, m)
  if k.public_method_defined?(m) then "public"
  elsif k.protected_method_defined?(m) then "protected"
  elsif k.private_method_defined?(m) then "private"
  else "none"
  end
end
{
  Conf::Connections => %i[services helper after_private],
  Conf::Connections::Details => %i[host port host= to_s secret],
  Conf::Connections::Options => %i[verbose verbose= loud?],
  Conf::Connections::Point => %i[x doubled bigger? guarded],
  Conf::Shapes::Size => %i[w h area],
  Pair => %i[left right swap],
  Conf::Host => %i[hello secret name run],
}.each do |k, ms|
  puts "#{k.name} < #{k.superclass.name}: " + ms.map { |m| "#{m}=#{vis(k, m)}" }.join(" ")
end
puts "Options.default_verbose=#{vis(Conf::Connections::Options.singleton_class, :default_verbose)}"
puts "Host.kind=#{vis(Conf::Host.singleton_class, :kind)}"
puts "Size.unit_area=#{vis(Conf::Shapes::Size.singleton_class, :unit_area)} #{Conf::Shapes::Size.unit_area}"
puts "Point#bigger?=#{Conf::Connections::Point.new(x: 3).bigger?(Conf::Connections::Point.new(x: 2))}"
puts "Connections#services=#{Conf::Connections.new.services}"
puts "Connections#helper=#{Conf::Connections.new.send(:helper)}"
puts "Size#area=#{Conf::Shapes::Size.new(w: 2, h: 3).area}"
puts "Pair#swap=#{Pair.new(1, 2).swap.to_a.inspect}"
puts "Options#loud?=#{Conf::Connections::Options.new(verbose: true).loud?}"
puts "Host#run=#{Conf::Host.new.run}"
"##;

const EXPECTED: &str = "\
Conf::Connections < Object: services=public helper=private after_private=private
Conf::Connections::Details < Struct: host=public port=public host==public to_s=public secret=private
Conf::Connections::Options < Struct: verbose=public verbose==public loud?=public
Conf::Connections::Point < Data: x=public doubled=public bigger?=public guarded=protected
Conf::Shapes::Size < Data: w=public h=public area=public
Pair < Struct: left=public right=public swap=public
Conf::Host < Object: hello=public secret=private name=private run=public
Options.default_verbose=public
Host.kind=public
Size.unit_area=public 1
Point#bigger?=true
Connections#services=h:1
Connections#helper=4
Size#area=6
Pair#swap=[2, 1]
Options#loud?=true
Host#run=hi n s greeter k2
";

#[test]
fn native_ruby_places_factory_and_included_block_methods() {
    let output = Command::new("ruby")
        .args(["-ractive_support", "-ractive_support/concern", "-e"])
        .arg(format!("{CONNECTIONS}\n{GREETING}\n{HOST}\n{REPORT}"))
        .output()
        .expect("native Ruby control");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_factory_and_included_block_methods_match_native_visibility() {
    let run = emit_and_run::real_blog()
        .write("app/lib/conf/connections.rb", CONNECTIONS)
        .write("app/models/concerns/conf/greeting.rb", GREETING)
        .write("app/lib/conf/host.rb", HOST)
        .run_ruby(REPORT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}

fn analyzed(files: &[(&str, &str)]) -> roundhouse::App {
    let mut app = ingest_app_from_tree(
        files
            .iter()
            .map(|(path, source)| (PathBuf::from(path), source.as_bytes().to_vec()))
            .collect::<HashMap<_, _>>(),
    )
    .expect("ingest synthetic source");
    Analyzer::new(&app).analyze(&mut app);
    app
}

#[test]
fn factory_blocks_ingest_as_classes_with_the_blocks_visibility() {
    let app = analyzed(&[("app/lib/conf/connections.rb", CONNECTIONS)]);
    let class = |name: &str| {
        app.library_classes
            .iter()
            .find(|c| c.name.0.as_str() == name)
            .unwrap_or_else(|| panic!("no library class {name}"))
    };
    let visibility = |class: &str, method: &str| {
        let c = self::class_methods(&app, class, method);
        c.unwrap_or_else(|| panic!("no {class}#{method}"))
    };
    for (name, parent) in [
        ("Conf::Connections::Details", "Struct"),
        ("Conf::Connections::Options", "Struct"),
        ("Conf::Connections::Point", "Data"),
        ("Conf::Shapes::Size", "Data"),
        ("Pair", "Struct"),
    ] {
        let c = class(name);
        assert_eq!(c.parent.as_ref().map(|p| p.0.as_str()), Some(parent), "{name}");
        assert!(matches!(c.origin, Some(LibraryClassOrigin::FactoryClass { .. })), "{name}");
    }
    assert_eq!(visibility("Conf::Connections::Details", "to_s"), MethodVisibility::Public);
    assert_eq!(visibility("Conf::Connections::Details", "secret"), MethodVisibility::Private);
    assert_eq!(visibility("Conf::Connections::Point", "guarded"), MethodVisibility::Protected);
    assert_eq!(visibility("Conf::Connections", "services"), MethodVisibility::Public);
    assert_eq!(visibility("Conf::Connections", "helper"), MethodVisibility::Private);
    // The outer `private` continues past the constant writes.
    assert_eq!(visibility("Conf::Connections", "after_private"), MethodVisibility::Private);
    // The block's defs belong to the factory class, not the enclosing one.
    assert!(class_methods(&app, "Conf::Connections", "to_s").is_none());
    let errors: Vec<_> = diagnose(&app).into_iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

fn class_methods(app: &roundhouse::App, class: &str, method: &str) -> Option<MethodVisibility> {
    app.library_classes
        .iter()
        .find(|c| c.name.0.as_str() == class)?
        .methods
        .iter()
        .find(|m| m.name.as_str() == method)
        .map(|m| m.visibility)
}

/// `super` from a factory class's `initialize` reaches the runtime
/// `Data#initialize` / `Struct#initialize`, so an app `initialize(*)`
/// elsewhere does not make it an implicit super into a nameless rest.
/// A subclass of that app class still is one: the CRuby tree writes its
/// keywords and the zsuper as written, so Ruby runs it, and every other
/// target names it.
#[test]
fn implicit_super_in_a_factory_initialize_reaches_the_runtime_constructor() {
    const MONEY: &str = r##"module Conf
  class Money
    Balance = Data.define(:name, :amount) do
      #: (name: String, ?amount: Integer) -> void
      def initialize(name:, amount: 0)
        super
      end

      def label = "#{name}=#{amount}"
    end

    Entry = Struct.new(:code, :count, keyword_init: true) do
      #: (code: String, ?count: Integer) -> void
      def initialize(code:, count: 1)
        super
      end
    end

    def show
      [Balance.new(name: "x").label, Entry.new(code: "c").count]
    end
  end
end
"##;
    const REST: &str = "class Rest\n  def initialize(*)\n    @ok = true\n  end\nend\n";
    const SUB: &str = "class RestSub < Rest\n  def initialize(code:)\n    super\n  end\nend\n";
    let script = "p Conf::Money.new.show, RestSub.new(code: \"c\").instance_variable_get(:@ok)\n";
    let expected = "[\"x=0\", 1]\ntrue\n";
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{MONEY}\n{REST}\n{SUB}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/conf/money.rb", MONEY)
        .write("app/lib/rest.rb", REST)
        .write("app/lib/rest_sub.rb", SUB)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);

    let off_ruby = |files: &[(&str, &str)]| {
        let app = analyzed(files);
        assert!(!diagnose(&app).iter().any(|d| d.severity == Severity::Error), "{:?}", diagnose(&app));
        roundhouse::analyze::nameless_rest_refusals_off_ruby(&app)
            .into_iter()
            .filter(|d| d.message.contains("implicit super with keywords into an anonymous positional rest"))
            .count()
    };
    assert_eq!(off_ruby(&[("app/lib/conf/money.rb", MONEY), ("app/lib/rest.rb", REST)]), 0);
    assert_eq!(off_ruby(&[("app/lib/conf/money.rb", MONEY), ("app/lib/rest.rb", REST), ("app/lib/rest_sub.rb", SUB)]), 1);
}

/// A constant written in a factory block is the ENCLOSING scope's:
/// the block keeps its cref, so `HDR` below is `Conf::Patch::HDR` and
/// `TOP` is `Object::TOP` (Core's `CI::TestOnboarding::ChangedTestFile`
/// with its `HUNK_HEADER`). The class's methods read them lexically.
#[test]
fn constants_in_a_factory_block_belong_to_the_enclosing_scope() {
    const PATCH: &str = r##"module Conf
  module Patch
    Hunk = Data.define(:line) do
      HDR = /\A@@ -(\d+)/

      class << self
        #: (String text) -> Hunk?
        def parse(text)
          match = HDR.match(text)
          match ? new(line: match[1].to_i) : nil
        end
      end

      def next_line = line + STEP

      STEP = 1
    end
  end
end
"##;
    const TALLY: &str = r##"Tally = Struct.new(:count) do
  TOP = 10

  def room = TOP - count
end
"##;
    let script = "p Conf::Patch::HDR, Conf::Patch::Hunk.parse(\"@@ -7,2\").next_line, \
                  Conf::Patch::Hunk.const_defined?(:HDR, false), Tally.new(3).room, TOP\n";
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{PATCH}\n{TALLY}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "/\\A@@ -(\\d+)/\n8\nfalse\n7\n10\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/conf/patch.rb", PATCH)
        .write("app/lib/tally.rb", TALLY)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

/// Ruby passes the new class to the block; Core's
/// `Changes = Struct.new(...) do |_klass|` never reads it, so the block is
/// the plain class body.
#[test]
fn an_unread_class_parameter_on_a_factory_block_is_the_plain_body() {
    const CHANGES: &str = "module Billing\n  Changes = Struct.new(:removed, :created) do |_klass|\n    def empty? = removed.nil? && created.nil?\n  end\nend\n";
    let run = emit_and_run::real_blog()
        .write("app/lib/billing.rb", CHANGES)
        .run_ruby("p Billing::Changes.new(nil, nil).empty?, Billing::Changes.new(1, nil).empty?\n");
    run.assert_passes();
    assert_eq!(run.stdout, "true\nfalse\n");
}

/// Shapes the class model does not carry are named, not dropped.
#[test]
fn unmodeled_factory_block_shapes_are_named_refusals() {
    for (source, message) in [
        (
            "MEMBERS = %i[a b]\nX = Struct.new(*MEMBERS) do\n  def sum = a + b\nend\n",
            "Struct factory with a body block needs literal symbol members",
        ),
        (
            "X = Data.define(:a) do |klass|\n  def twice = a * 2\n  klass\nend\n",
            "block parameters on a Data factory body are not modeled",
        ),
        (
            "X = Struct.new(:a) do\n  @@limit = 3\n  def capped = [a, @@limit].min\nend\n",
            "constant or class variable assigned inside a Struct.new/Data.define body block",
        ),
        (
            "X = Struct.new(:a) do\n  Inner = Data.define(:b) do\n    def c = b\n  end\n  def d = a\nend\n",
            "Struct.new/Data.define body block nested in another one is not modeled",
        ),
        (
            "module X\n  P = Struct.new(:a) do\n    class << self\n      C = 1\n      def c = C\n    end\n  end\nend\n",
            "constant written in a `class << self` of a Struct factory body inside a class or module body",
        ),
    ] {
        survey::activate();
        let result = ingest_app_from_tree(HashMap::from_iter([(
            PathBuf::from("app/lib/x.rb"),
            source.as_bytes().to_vec(),
        )]));
        let gaps = survey::drain();
        let app = result.expect("survey retains the app");
        assert!(gaps.iter().any(|gap| gap.to_string().contains(message)), "{source}\n=> {gaps:?}");
        // Refused, not half-modeled: no class for `X` and no stray defs.
        assert!(!app.library_classes.iter().any(|c| c.name.0.as_str() == "X"), "{source}");
    }
}

/// `X = Struct.new(...) do … end` inside `class << self` (Core's
/// `Merchandising::FilterQueryBuilder::QUERY`) is a class like any other
/// factory write, and the constants after it read it. Ruby binds `X` on
/// the singleton class; the walks carry every `class << self` constant
/// on the declaration (`singleton_class`), and the factory class with
/// them, so the methods that read it by bare name run the same. In a
/// model's `class << self` too, where the model walk must not replay the
/// write a second time.
#[test]
fn a_factory_class_in_class_self_is_a_class_its_constants_read() {
    const BUILDER: &str = r##"module Merch
  module FilterBuilder
    MAPPING = { price: "variants.price" }.freeze

    class << self
      def build(field) = query(field)

      private

      QUERY = Struct.new(:index) do
        def multi_index? = false

        def unavailable_fields = []
      end
      PRODUCT_QUERY = QUERY.new(:products)

      def query(field)
        [PRODUCT_QUERY.multi_index?, PRODUCT_QUERY.index, QUERY.members, MAPPING[field]]
      end
    end
  end
end
"##;
    const ARTICLE_ROWS: &str = r##"class Article < ApplicationRecord
  class << self
    Row = Struct.new(:n) do
      def twice = n * 2
    end

    def doubled_row(n) = Row.new(n).twice
  end

"##;
    let script = "p Merch::FilterBuilder.build(:price), Merch::FilterBuilder.respond_to?(:query), \
                  Merch::FilterBuilder.private_methods.include?(:query)\n";
    let expected = "[false, :products, [:index], \"variants.price\"]\nfalse\ntrue\n";
    let native = Command::new("ruby").arg("-e").arg(format!("{BUILDER}\n{script}")).output().expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog().write("app/lib/merch/filter_builder.rb", BUILDER).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);

    // The model's singleton factory class, through the model walk.
    let model = emit_and_run::real_blog()
        .edit("app/models/article.rb", "class Article < ApplicationRecord\n", ARTICLE_ROWS)
        .run_ruby("p Article.doubled_row(3), Article::Row.new(4).twice\n");
    model.assert_passes();
    assert_eq!(model.stdout, "6\n8\n");
}

/// The other class-method carriers bind the constant somewhere no class
/// is emitted for, and stay named refusals.
#[test]
fn a_factory_class_in_other_class_method_carriers_is_refused() {
    let source = "module Conf\n  module Greeting\n    extend ActiveSupport::Concern\n\n    class_methods do\n      \
                  X = Struct.new(:a) do\n        def b = a\n      end\n    end\n\n    def hi = 1\n  end\nend\n";
    survey::activate();
    let result = ingest_app_from_tree(HashMap::from_iter([(
        PathBuf::from("app/models/concerns/conf/greeting.rb"),
        source.as_bytes().to_vec(),
    )]));
    let gaps = survey::drain();
    result.expect("survey retains the app");
    assert!(
        gaps.iter().any(|gap| gap.to_string().contains("in a singleton or class-method carrier is not modeled")),
        "{gaps:?}"
    );
}

/// A constant written in a `class << self` of a factory block is the new
/// class's singleton's (Core's `EmbeddedAppliedDiscount::
/// PERSISTED_ATTRIBUTES`), not the enclosing scope's: `class << self`
/// opens a cref of its own. It is carried on the class, as every
/// `class << self` constant is, and written through `self` in the
/// emitted block, whose bare writes would land on the scope around it.
/// Rubydex does not index that singleton at file level, so its reads
/// (the singleton's constant, a top-level class and its constant) are
/// typed by their full names.
#[test]
fn constants_in_a_factory_blocks_class_self_belong_to_the_class() {
    const DISCOUNT: &str = r##"Discount = Struct.new(:amount, :title, keyword_init: true) do
  class << self
    def new(amount:, title: nil)
      instance = allocate
      instance.send(:initialize, amount: amount, title: title)
      instance
    end

    PERSISTED = [:amount, :title].freeze
    private_constant :PERSISTED

    def persisted = PERSISTED

    def from_h(hash) = Discount.new(amount: hash.fetch("amount") * Rate::FACTOR)
  end

  def encode = self.class.persisted.map { |a| public_send(a) }
end
"##;
    const RATE: &str = "class Rate\n  FACTOR = 2\nend\n";
    let script = "p Discount.new(amount: 3).encode, Discount.from_h(\"amount\" => 4).amount, \
                  Object.const_defined?(:PERSISTED)\n";
    let expected = "[3, nil]\n8\nfalse\n";
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{RATE}\n{DISCOUNT}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog()
        .write("app/lib/discount.rb", DISCOUNT)
        .write("app/lib/rate.rb", RATE)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
