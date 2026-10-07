//! The whole-program fixpoint settles on slots that nest one container
//! deeper every round without ever repeating a value.
//!
//! Both are reduced from Shopify Core, where thousands of slots like
//! them kept the production and absorb loops moving at the 12-round cap
//! and each round typed larger trees than the last:
//!
//! - `NestedFieldSet#serialize!` (GraphApi) returns
//!   `{ key => sub_set.serialize! }` for each nested set: recursion
//!   through another instance of the same class, which the self-call
//!   recursion widening does not see. Its return gained an
//!   `Array[Hash[_, …]]` level (about 12 nodes) per round.
//! - `Risk::Service#recommendations_for_orders` has an instance method
//!   and a class method of the same name, which share one parameter
//!   key. The instance method wraps each element of its argument in a
//!   Hash and passes the array to the class method, so the shared slot
//!   received its own value one Hash deeper every round.
//!
//! Neither revisits a value, so cycle widening never applies. Depth
//! limiting does: after a slot sets a new nesting high three times in a
//! loop, the levels below the limit become `Untyped`.
use roundhouse::analyze::Analyzer;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::{App, ClassId, Symbol, Ty};
use std::path::PathBuf;

const SOURCES: &[(&str, &str)] = &[
    (
        "app/models/nested_field_set.rb",
        r#"class NestedFieldSet
  def initialize
    @symbols = Set.new
    @nested = {}
  end

  def merge(fields)
    fields.each do |field|
      case field
      when Symbol
        @symbols.add(field)
      when Hash
        field.each do |key, sub_fields|
          nested = (@nested[key] ||= NestedFieldSet.new)
          nested.merge(sub_fields)
        end
      end
    end
    self
  end

  def serialize!
    @serialized ||= begin
      result = @symbols.to_a
      @nested.each do |key, sub_set|
        result << { key => sub_set.serialize! }
      end
      result.freeze
    end
  end
end
"#,
    ),
    (
        "app/models/risk/service.rb",
        r#"module Risk
  class Service
    def self.recommendations_for_orders(order_and_shop_ids)
      order_and_shop_ids.each_with_object({}) do |pair, hash|
        hash[pair.fetch(:order_id)] = pair.fetch(:shop_id)
      end
    end

    def initialize(shop_id)
      @shop_id = shop_id
    end

    attr_reader :shop_id

    def recommendations_for_orders(order_ids)
      order_and_shop_ids = order_ids.map do |order_id|
        { order_id: order_id, shop_id: shop_id }
      end
      Service.recommendations_for_orders(order_and_shop_ids)
    end
  end
end
"#,
    ),
    (
        "app/models/report.rb",
        r#"class Report
  def fields
    NestedFieldSet.new.merge([:id, { title: [:text] }]).serialize!
  end

  def risks
    Risk::Service.new(1).recommendations_for_orders([10, 20])
  end
end
"#,
    ),
];

struct Analyzed {
    app: App,
    rounds: roundhouse::analyze::FixpointRounds,
    serialize_return: Option<Ty>,
}

fn analyzed() -> Analyzed {
    let tree = SOURCES
        .iter()
        .map(|(path, src)| (PathBuf::from(path), src.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("fixture ingests");
    let mut analyzer = Analyzer::new(&app);
    analyzer.analyze(&mut app);
    let rounds = analyzer.fixpoint_rounds();
    let serialize_return = analyzer
        .class_registry()
        .get(&ClassId(Symbol::from("NestedFieldSet")))
        .and_then(|c| c.instance_methods.get(&Symbol::from("serialize!")))
        .cloned();
    Analyzed { app, rounds, serialize_return }
}

/// Container nesting, as depth limiting counts it.
fn depth(ty: &Ty) -> usize {
    match ty {
        Ty::Array { elem } => 1 + depth(elem),
        Ty::Hash { key, value } => 1 + depth(key).max(depth(value)),
        Ty::Tuple { elems } => 1 + elems.iter().map(depth).max().unwrap_or(0),
        Ty::Record { row } => 1 + row.fields.values().map(depth).max().unwrap_or(0),
        Ty::Class { args, .. } if !args.is_empty() => 1 + args.iter().map(depth).max().unwrap_or(0),
        Ty::Union { variants } => variants.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn params<'a>(app: &'a App, class: &str, method: &str) -> &'a [Ty] {
    app.inferred_method_params
        .get(&(ClassId(Symbol::from(class)), Symbol::from(method)))
        .unwrap_or_else(|| panic!("{class}#{method} has inferred params"))
}

#[test]
fn every_fixpoint_loop_settles_before_the_cap() {
    let a = analyzed();
    for (name, n) in [("production", a.rounds.production), ("tests", a.rounds.tests), ("absorb", a.rounds.absorb)] {
        assert!(n.is_some_and(|n| n <= 8), "{name} loop did not settle: {:?}", a.rounds);
    }
}

#[test]
fn a_return_nesting_itself_through_another_instance_is_depth_limited() {
    let ret = analyzed().serialize_return.expect("NestedFieldSet#serialize! registered");
    // Unlimited, this nested one `Array[Hash[…]]` level deeper per round.
    assert!(depth(&ret) <= 4, "{ret:?}");
    // The levels above the limit keep their shape: an Array of
    // `{ key => … }` hashes whose values are the nested sets' Arrays.
    let Ty::Array { elem } = &ret else { panic!("array return: {ret:?}") };
    let Ty::Union { variants } = &**elem else { panic!("union element: {ret:?}") };
    assert!(
        variants.iter().any(|v| matches!(v, Ty::Hash { value, .. } if matches!(**value, Ty::Array { .. }))),
        "{ret:?}"
    );
}

#[test]
fn a_param_fed_its_own_value_one_hash_deeper_is_depth_limited() {
    let a = analyzed();
    let [param] = params(&a.app, "Risk::Service", "recommendations_for_orders") else {
        panic!("one param")
    };
    // Unlimited, this gained a `Hash[Sym, …]` level per round.
    assert!(depth(param) <= 4, "{param:?}");
    // The argument is still an Array whose elements include the
    // `{ order_id:, shop_id: }` hashes the instance method builds.
    let Ty::Array { elem } = param else { panic!("array param: {param:?}") };
    let has_hash = |t: &Ty| match t {
        Ty::Hash { .. } => true,
        Ty::Union { variants } => variants.iter().any(|v| matches!(v, Ty::Hash { .. })),
        _ => false,
    };
    assert!(has_hash(elem), "{param:?}");
}
