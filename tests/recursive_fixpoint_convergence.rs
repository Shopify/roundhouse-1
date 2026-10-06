//! The whole-program fixpoint settles on recursive data and on
//! same-named defs that share a parameter key.
//!
//! Each case is a shape from Shopify Core that kept the inference
//! signature moving every round until the cap, with types growing each
//! round (doubling, for a method recursive through both a hash key and
//! its value):
//!
//! - `TreePrinter#fix` is `normalize_cache_args` / `fix_bad_encodings`:
//!   a structural recursion over `case value when Hash … when Array …`.
//!   Its return nested one container deeper every round, and without
//!   `when` narrowing its param alternated between two halves of the
//!   union.
//! - `ImageAttachment` has a concern `ClassMethods#build_target` that
//!   forwards to the module's own `class << self` `build_target`, which
//!   takes one more keyword. The colliding shapes left the kwargs Hash in
//!   the `filename` slot, and that slot then held a Hash of itself.
//! - `PaymentGateway#without_killswitched_wallets` is Core's wallet
//!   killswitch filter. Its return reaches its own `new:` argument
//!   through `supported_wallet_brands` and `disabled_wallet_brands=`,
//!   and a site that is `Untyped` one round is no evidence, so the param,
//!   the return and three callers cycled with period 4. The slots are
//!   widened to the join of the cycle.
//! - `Gateway#limit?` is an instance method and a class method with
//!   different keywords (`CollectionSource#over_..._limit?`), and
//!   `Gateway#to_entity` is a keyword-only instance method plus a
//!   class method that takes a positional (`RolloutTreatmentRecord`).
use roundhouse::analyze::Analyzer;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::{App, ClassId, Symbol, Ty};
use std::path::PathBuf;

const SOURCES: &[(&str, &str)] = &[
    (
        "app/models/application_record.rb",
        "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
    ),
    (
        "db/schema.rb",
        r#"ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "photos", force: :cascade do |t|
    t.string "name"
  end
  create_table "payment_gateways", force: :cascade do |t|
    t.string "name"
    t.text "disabled_wallet_brands"
  end
end
"#,
    ),
    (
        "app/models/photo.rb",
        r#"class Photo < ApplicationRecord
  include ImageAttachment

  def self.upload(shop)
    build_target(shop, filename: "a.png", mime_type: "image/png")
  end
end
"#,
    ),
    (
        "app/models/concerns/image_attachment.rb",
        r#"module ImageAttachment
  extend ActiveSupport::Concern

  module ClassMethods
    def build_target(shop, filename:, mime_type:)
      ImageAttachment.build_target(shop, filename: filename, mime_type: mime_type, bucket: "b")
    end
  end

  class << self
    def build_target(shop, filename:, mime_type:, bucket:)
      File.extname(filename)
    end
  end
end
"#,
    ),
    (
        "app/models/tree_printer.rb",
        r#"class TreePrinter
  def fix(value)
    case value
    when Hash
      out = {}
      value.each { |k, v| out[fix(k)] = fix(v) }
      out
    when Array
      value.map { |v| fix(v) }
    else
      value
    end
  end

  def run
    fix({ a: [1, "x"] })
  end
end
"#,
    ),
    (
        "app/models/gateway.rb",
        r#"class Gateway
  def self.limit?(shop_id:, ids:, extra: 0)
    ids.size + extra > shop_id
  end

  def limit?(ids:, extra: 0)
    self.class.limit?(shop_id: 1, ids: ids, extra: extra)
  end

  def self.to_entity(record, lookup: nil, other: nil)
    [record, lookup, other]
  end

  def to_entity(lookup: nil, other: nil, load: true)
    return Gateway.to_entity(self, lookup: lookup, other: other) if load
    [lookup, other]
  end

  def run
    limit?(ids: [1, 2])
    to_entity(lookup: { 1 => "a" }, load: false)
  end
end
"#,
    ),
    (
        "app/models/payment_gateway.rb",
        r#"class PaymentGateway < ApplicationRecord
  def without_killswitched_wallets(supported_wallet_brands)
    supported_wallet_brands -= [:facebook_pay]

    return supported_wallet_brands if Verdict::Flag.enabled?(handle: "bypass")

    supported_wallet_brands.reject do |supported_wallet_brand|
      Verdict::Flag.enabled?(handle: "wallet_killswitch_#{supported_wallet_brand}")
    end
  end

  def without_changes_to_killswitched_wallets(current:, new:)
    killswitched_but_previously_present = current.difference(without_killswitched_wallets(current))
    killswitched_but_previously_present
      .union(without_killswitched_wallets(new))
      .uniq
  end
end
"#,
    ),
    (
        "app/models/direct_payment_gateway.rb",
        r#"class DirectPaymentGateway < PaymentGateway
  def self.supported_wallets_by_provider(name)
    Providers.lookup(name)
  end

  def supported_wallet_brands
    wallets = self.class.supported_wallets_by_provider(name)
    wallets.delete(:amazon_pay) unless name
    without_killswitched_wallets(wallets)
  end

  def wallet_enabled?(brand)
    enabled_wallet_brands.include?(brand)
  end

  def disabled_wallet_brands
    brands = self[:disabled_wallet_brands]
    brands += [:apple_pay] if name
    brands.uniq
  end

  def enabled_wallet_brands
    @enabled_wallet_brands ||= (supported_wallet_brands - disabled_wallet_brands).freeze
  end

  def enabled_wallet_brands=(brands)
    @enabled_wallet_brands = nil
    self.disabled_wallet_brands = (supported_wallet_brands - brands.map(&:to_sym))
  end

  def disabled_wallet_brands=(brands)
    @enabled_wallet_brands = nil
    supported_wallets = self.class.supported_wallets_by_provider(name)
    disabled = (brands.map(&:to_sym) & supported_wallets)
    self[:disabled_wallet_brands] = without_changes_to_killswitched_wallets(
      current: self[:disabled_wallet_brands],
      new: disabled,
    )
  end

  def apple_pay?
    wallet_enabled?(:apple_pay)
  end
end
"#,
    ),
];

struct Analyzed {
    app: App,
    rounds: roundhouse::analyze::FixpointRounds,
    /// `TreePrinter#fix`'s return as the registry holds it.
    fix_return: Option<Ty>,
}

fn analyzed_full() -> Analyzed {
    let tree = SOURCES
        .iter()
        .map(|(path, src)| (PathBuf::from(path), src.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("fixture ingests");
    let mut analyzer = Analyzer::new(&app);
    analyzer.analyze(&mut app);
    let rounds = analyzer.fixpoint_rounds();
    let fix_return = analyzer
        .class_registry()
        .get(&ClassId(Symbol::from("TreePrinter")))
        .and_then(|c| c.instance_methods.get(&Symbol::from("fix")))
        .cloned();
    Analyzed { app, rounds, fix_return }
}

fn analyzed() -> (App, roundhouse::analyze::FixpointRounds) {
    let a = analyzed_full();
    (a.app, a.rounds)
}

fn size(ty: &Ty) -> usize {
    1 + match ty {
        Ty::Array { elem } => size(elem),
        Ty::Hash { key, value } => size(key) + size(value),
        Ty::Tuple { elems } => elems.iter().map(size).sum(),
        Ty::Union { variants } => variants.iter().map(size).sum(),
        Ty::Record { row } => row.fields.values().map(size).sum(),
        Ty::Class { args, .. } => args.iter().map(size).sum(),
        _ => 0,
    }
}

fn params<'a>(app: &'a App, class: &str, method: &str) -> &'a [Ty] {
    app.inferred_method_params
        .get(&(ClassId(Symbol::from(class)), Symbol::from(method)))
        .unwrap_or_else(|| panic!("{class}#{method} has inferred params"))
}

fn contains_hash(ty: &Ty) -> bool {
    match ty {
        Ty::Hash { .. } => true,
        Ty::Union { variants } => variants.iter().any(contains_hash),
        _ => false,
    }
}

#[test]
fn every_fixpoint_loop_settles_well_under_the_cap() {
    let (_, rounds) = analyzed();
    // The wallet cycle has period 4: it shows itself on the fifth
    // round, and its dependents settle in the next few.
    for (name, n) in [("production", rounds.production), ("tests", rounds.tests), ("absorb", rounds.absorb)] {
        assert!(n.is_some_and(|n| n <= 8), "{name} loop did not settle: {rounds:?}");
    }
}

#[test]
fn a_recursive_return_is_widened_at_the_recursion_point() {
    let ret = analyzed_full().fix_return.expect("TreePrinter#fix registered");
    // One unfolding stays precise; the recursion point is `untyped`.
    // Unwidened, this was 481 nodes at the 12-round cap.
    assert!(size(&ret) <= 12, "{ret:?}");
    let Ty::Union { variants } = &ret else { panic!("union return: {ret:?}") };
    assert!(variants.iter().any(|v| matches!(v, Ty::Array { .. })), "{ret:?}");
    assert!(variants.iter().any(|v| matches!(v, Ty::Hash { .. })), "{ret:?}");
}

#[test]
fn when_class_arms_narrow_so_the_recursive_param_collects_every_arm() {
    let (app, _) = analyzed();
    let [param] = params(&app, "TreePrinter", "fix") else { panic!("one param") };
    let Ty::Union { variants } = param else { panic!("{param:?}") };
    // Root: Hash; Hash arm: its keys (Sym) and values (Array); Array arm:
    // its elements (Int, String). Every round sees all of them.
    for want in [Ty::Int, Ty::Str, Ty::Sym] {
        assert!(variants.contains(&want), "{want:?} in {param:?}");
    }
    assert!(variants.iter().any(|v| matches!(v, Ty::Array { .. })), "{param:?}");
    assert!(variants.iter().any(|v| matches!(v, Ty::Hash { .. })), "{param:?}");
    assert!(size(param) <= 16, "{param:?}");
}

#[test]
fn a_keyword_only_one_def_declares_picks_that_def() {
    let (app, _) = analyzed();
    // `bucket:` exists only on the singleton: shop, filename, mime_type,
    // bucket, each typed by its keyword, never by the whole kwargs Hash.
    let p = params(&app, "ImageAttachment", "build_target");
    assert_eq!(p.len(), 4, "{p:?}");
    assert_eq!(&p[1..], &[Ty::Str, Ty::Str, Ty::Str], "{p:?}");
    let ext = params(&app, "File", "extname");
    assert!(!ext.iter().any(contains_hash), "{ext:?}");
}

#[test]
fn instance_and_class_defs_of_one_name_never_feed_a_slot_its_own_hash() {
    let (app, _) = analyzed();
    for method in ["limit?", "to_entity"] {
        let p = params(&app, "Gateway", method);
        let total: usize = p.iter().map(size).sum();
        assert!(total <= 12, "Gateway#{method}: {p:?}");
        // `limit?` passes only keywords; no slot sees the kwargs group.
        if method == "limit?" {
            assert!(!p.iter().any(contains_hash), "Gateway#{method}: {p:?}");
        }
    }
}

#[test]
fn a_return_that_feeds_its_own_argument_settles_on_the_join_of_its_cycle() {
    let a = analyzed_full();
    assert!(a.rounds.production.is_some_and(|n| n <= 8), "{:?}", a.rounds);
    let [param] = params(&a.app, "PaymentGateway", "without_killswitched_wallets") else {
        panic!("one param")
    };
    // The settled type admits every state of the cycle: the precise
    // `Array[Sym]` and the `Untyped` from the unresolved provider list.
    let syms = Ty::Array { elem: Box::new(Ty::Sym) };
    let admits = |want: &Ty| match param {
        Ty::Untyped => true,
        Ty::Union { variants } => variants.contains(want) || variants.contains(&Ty::Untyped),
        t => t == want,
    };
    assert!(admits(&syms) && admits(&Ty::Untyped), "{param:?}");
}
