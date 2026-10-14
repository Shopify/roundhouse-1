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
