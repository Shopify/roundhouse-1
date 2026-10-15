//! `const_set(:Connection, Factory.for(Widget))` followed by `class
//! Connection` in the same body: the reopen adds to the class the factory
//! built, so it runs after the `const_set`. Core's
//! graph_api/storefront/product/connection.rb is written so, over a parent
//! (`Product < GraphApi::ObjectType`) that product.rb opens and whose base
//! a gem defines. The nested class was emitted ahead of the body's
//! statements: a plain `class Connection` before the `const_set`, which
//! then redefined the constant (Core's StrictWarning raises "already
//! initialized constant") and left the class without its ancestors.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = r#"require "logger"

class ObjectBase < Logger
  def self.field(name)
    (@fields ||= []) << name
  end

  def self.fields
    @fields ||= []
  end
end
"#;

const FACTORY: &str = r#"
module Factory
  #: (Class) -> Class
  def self.for(owner)
    connection = Class.new(ObjectBase)
    owner.const_set(:Connection, connection)
    connection
  end
end
"#;

const WIDGET: &str = r#"
class Widget < ObjectBase
  field :name
  LIMIT = 3

  def self.label = "widget"
end
"#;

const CONNECTION: &str = r#"
class Widget
  const_set(:Connection, Factory.for(Widget))

  class Connection
    field :filters

    def extra
      :extra
    end

    class << self
      def build(**kwargs)
        Value.new(**kwargs)
      end
    end

    class Value
      attr_accessor :filters

      def initialize(filters:)
        @filters = filters
      end
    end
  end
end
"#;

#[test]
fn a_class_reopened_after_const_set_reopens_the_class_const_set_built() {
    let run = emit_and_run::empty_app()
        .write("lib/object_base.rb", BASE)
        .write("lib/factory.rb", FACTORY)
        .write("app/models/widget.rb", WIDGET)
        .write("app/models/widget/connection.rb", CONNECTION)
        .run_ruby(
            "$stderr.sync = true\nconn = Widget::Connection\nputs conn.allocate.extra.inspect\nputs conn.fields.inspect\nputs conn.superclass\nputs Widget.fields.inspect\nputs conn.build(filters: 7).filters\n",
        );
    run.assert_passes();
    assert!(run.stdout.contains(":extra"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "[:filters]"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "ObjectBase"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "[:name]"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "7"), "{}", run.stdout);
    assert!(!run.stderr.contains("already initialized"), "{}", run.stderr);
}

const GRAPH_BASE: &str = r#"require "logger"

class GraphBase < Logger
  def self.field(name, **)
    (@fields ||= []) << name
  end

  def self.fields
    @fields ||= []
  end

  def self.post_hook(&block)
    @hook = block
  end

  class << self
    attr_accessor :cacheable
  end
end
"#;

const GRAPH_FACTORY: &str = r#"
module GraphFactory
  #: (Class) -> Class
  def self.for(owner)
    connection = Class.new(GraphBase)
    owner.const_set(:Connection, connection)
    connection
  end
end
"#;

/// A block calling the class's own def, then `self.cacheable = true`: the
/// body's tail effects, written after the methods. The `const_set` from the
/// other file lands among them, and the reopening follows it.
const GRAPH_PRODUCT: &str = r#"
module Graph
  module Storefront
    class Product < GraphBase
      post_hook do |product, ctx|
        contextually_published?(ctx, product)
      end

      self.cacheable = true

      field :name

      def name = "n"

      def self.contextually_published?(ctx, product) = ctx
    end
  end
end
"#;

const GRAPH_CONNECTION: &str = r#"
module Graph
  module Storefront
    class Product
      const_set(:Connection, GraphFactory.for(Product))

      class Connection
        field :filters
      end
    end
  end
end
"#;

/// Core's graph_api/storefront/product.rb: the `const_set` is one of the
/// parent's tail effects (a block that calls its own def is above it), so
/// the reopening was written above the methods, ahead of the call, and
/// `field` ran on a plain class ("undefined method 'field' for class
/// ...::Product::Connection"). The tail's `const_set` was also written
/// bare, setting the constant a second time.
#[test]
fn a_reopening_follows_a_const_set_written_among_the_parents_tail_effects() {
    let run = emit_and_run::empty_app()
        .write("lib/graph_base.rb", GRAPH_BASE)
        .write("lib/graph_factory.rb", GRAPH_FACTORY)
        .write("app/models/graph/storefront/product.rb", GRAPH_PRODUCT)
        .write("app/models/graph/storefront/product/connection.rb", GRAPH_CONNECTION)
        .run_ruby(
            "$stderr.sync = true\nconn = Graph::Storefront::Product::Connection\nputs conn.fields.inspect\nputs conn.superclass\nputs Graph::Storefront::Product.cacheable\n",
        );
    run.assert_passes();
    assert!(run.stdout.lines().any(|l| l == "[:filters]"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "GraphBase"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "true"), "{}", run.stdout);
    assert!(!run.stderr.contains("already initialized"), "{}", run.stderr);
}
