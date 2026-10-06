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

use std::collections::HashMap;
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
/// A subclass of that app class still is one.
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
    let script = "p Conf::Money.new.show\n";
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{MONEY}\n{REST}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "[\"x=0\", 1]\n");

    let run = emit_and_run::real_blog()
        .write("app/lib/conf/money.rb", MONEY)
        .write("app/lib/rest.rb", REST)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "[\"x=0\", 1]\n");

    let app = analyzed(&[("app/lib/rest.rb", REST), ("app/lib/rest_sub.rb", SUB)]);
    let refused: Vec<_> = diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == Severity::Error && d.message.contains("implicit super into an anonymous positional rest"))
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
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
            "X = Data.define(:a) do |klass|\n  def twice = a * 2\nend\n",
            "block parameters on a Data factory body are not modeled",
        ),
        (
            "X = Struct.new(:a) do\n  LIMIT = 3\n  def capped = [a, LIMIT].min\nend\n",
            "constant or class variable assigned inside a Struct.new/Data.define body block",
        ),
    ] {
        survey::activate();
        let result = ingest_app_from_tree(HashMap::from([(
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
