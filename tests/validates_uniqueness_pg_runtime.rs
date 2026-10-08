//! `validates ..., uniqueness:` and the validation context (`on:`,
//! `valid?(context)`, `save(context:)`, and `:create` / `:update` from
//! `new_record?`) on an ActiveRecord model, against native Rails 8.2 on
//! the same local PostgreSQL database.
//!
//! UniquenessValidator asks the model's table, unscoped, whether another
//! row holds the value: with `scope:` (a column, or a `belongs_to` read
//! as its foreign key), `case_sensitive: false` (`LOWER(col) = LOWER(?)`
//! on a string column, `IS NULL` for nil), a `conditions:` lambda (a
//! `where` on that relation), the record itself excluded once persisted,
//! and a `message:` interpolating `%{model}`. The emitted model writes
//! that query through the runtime's Relation.
//!
//! In STI, a uniqueness rule asks the class that declared it: the base's
//! asks the whole table, a subclass's keeps its type condition (the
//! subclass and every class below it), as `find_finder_class_for` and
//! `unscoped` do. A grandchild record (`PlatinumBadge < GoldBadge <
//! Badge`) is stored with its own type, meets the base's rule across
//! the whole table and its parent's within the parent's type set, new
//! and persisted.
//!
//! The same model files run natively (Core's pinned Rails, over
//! `establish_connection`) and on the emitted program, each on a freshly
//! created `rh_e2e_validates_uniq`, and every line `UniqProbe.lines`
//! prints must be equal, and equal to what Rails is known to answer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "shops", force: :cascade do |t|
    t.string "name"
  end
  create_table "handles", force: :cascade do |t|
    t.string "handle"
    t.string "code"
    t.bigint "shop_id"
    t.boolean "is_deleted", default: false, null: false
    t.integer "rank"
  end
  create_table "badges", force: :cascade do |t|
    t.string "name"
    t.string "code"
    t.string "type"
  end
end
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_validates_uniq
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
const SHOP: &str = "class Shop < ApplicationRecord\nend\n";

const HANDLE: &str = r##"class Handle < ApplicationRecord
  belongs_to :shop, optional: true

  validates :handle, uniqueness: { scope: :shop_id, case_sensitive: false }
  validates :code, uniqueness: { conditions: -> { where(is_deleted: false) } }, allow_nil: true
  validates :rank, uniqueness: { scope: [:shop], message: "is used in %{model}" }, on: :create
  validates :code, presence: true, on: :update
end
"##;

const BADGE: &str = "class Badge < ApplicationRecord\n  validates :code, uniqueness: true\nend\n";
const GOLD_BADGE: &str = "class GoldBadge < Badge\n  validates :name, uniqueness: { case_sensitive: false }\nend\n";
const PLATINUM_BADGE: &str = "class PlatinumBadge < GoldBadge\nend\n";
const SILVER_BADGE: &str = "class SilverBadge < Badge\nend\n";

const PROBE: &str = r##"class UniqProbe
  def self.observe(label, record)
    valid = record.valid?
    "#{label}: #{valid} #{record.errors.full_messages.inspect}"
  end

  def self.lines
    out = []
    shop = Shop.create!(name: "a")
    other = Shop.create!(name: "b")
    first = Handle.new(handle: "Foo", code: "c1", shop_id: shop.id, rank: 1)
    out << observe("first new", first)
    out << "first saved: #{first.save}"
    out << observe("first persisted", first)
    out << observe("handle other case", Handle.new(handle: "FOO", code: "c2", shop_id: shop.id, rank: 2))
    out << observe("handle other shop", Handle.new(handle: "foo", code: "c3", shop_id: other.id, rank: 3))
    out << observe("handle no shop", Handle.new(handle: "foo", code: "c4", rank: 4))
    out << observe("code taken", Handle.new(handle: "x", code: "c1", rank: 5))
    gone = Handle.new(handle: "gone", code: "c9", is_deleted: true, rank: 6)
    out << "gone saved: #{gone.save}"
    out << observe("code of a deleted row", Handle.new(handle: "y", code: "c9", rank: 7))
    out << observe("code nil", Handle.new(handle: "z", code: nil, rank: 8))
    out << observe("rank taken in shop", Handle.new(handle: "r", code: "r1", rank: 1, shop_id: shop.id))
    out << observe("rank in other shop", Handle.new(handle: "r", code: "r1", rank: 1, shop_id: other.id))
    out << observe("rank of the deleted row", Handle.new(handle: "r", code: "r1", rank: 6))
    nameless = Handle.new(handle: nil, code: "n1", rank: 9)
    out << "nameless saved: #{nameless.save}"
    out << observe("handle nil twice", Handle.new(handle: nil, code: "n2", rank: 10))
    first.code = nil
    out << observe("persisted, code cleared", first)
    out << "persisted save: #{first.save}"
    first.code = "c1"
    out << "persisted valid in create context: #{first.valid?(:create)} #{first.errors.full_messages.inspect}"
    second = Handle.new(handle: "Bar", code: "c2", rank: 11)
    out << "second saved: #{second.save}"
    second.rank = 6
    out << observe("second takes the deleted row's rank (update)", second)
    out << "second in create context: #{second.save(context: :create)} #{second.errors.full_messages.inspect}"
    second.code = nil
    out << "second update to nil code: #{second.save} #{second.errors.full_messages.inspect}"
    second.code = "c2"
    second.handle = "FOO"
    out << observe("second renamed onto first, other shop", second)
    second.shop_id = shop.id
    out << observe("second renamed onto first, same shop", second)
    out << "rows: #{Handle.count}"
    Badge.create!(name: "Star", code: "b1")
    GoldBadge.create!(name: "Sun", code: "g1")
    PlatinumBadge.create!(name: "Moon", code: "p1")
    out << observe("gold named like a plain badge", GoldBadge.new(name: "star", code: "g2"))
    out << observe("gold named like gold", GoldBadge.new(name: "SUN", code: "g3"))
    out << observe("gold named like platinum", GoldBadge.new(name: "moon", code: "g4"))
    out << observe("platinum named like gold", PlatinumBadge.new(name: "sun", code: "p2"))
    out << observe("silver named like gold", SilverBadge.new(name: "Sun", code: "s1"))
    out << observe("silver with gold's code", SilverBadge.new(name: "x", code: "g1"))
    out << observe("gold with a plain badge's code", GoldBadge.new(name: "y", code: "b1"))
    SilverBadge.create!(name: "Rain", code: "s0")
    out << observe("platinum with a plain badge's code", PlatinumBadge.new(name: "z", code: "b1"))
    out << observe("platinum with silver's code", PlatinumBadge.new(name: "z", code: "s0"))
    out << observe("plain badge with platinum's code", Badge.new(name: "z", code: "p1"))
    out << observe("silver with platinum's code", SilverBadge.new(name: "z", code: "p1"))
    out << observe("platinum named like platinum", PlatinumBadge.new(name: "MOON", code: "p4"))
    out << observe("platinum named like a plain badge", PlatinumBadge.new(name: "star", code: "p5"))
    out << observe("silver named like platinum", SilverBadge.new(name: "Moon", code: "s2"))
    platinum = PlatinumBadge.where(code: "p1").first
    out << "platinum row: #{platinum.class.name} #{platinum.type}"
    out << observe("platinum keeps its own name and code", platinum)
    platinum.code = "g1"
    out << observe("platinum takes gold's code", platinum)
    platinum.code = "p1"
    platinum.name = "sun"
    out << observe("platinum renamed onto gold", platinum)
    out << "platinum rows: #{PlatinumBadge.count} gold rows: #{GoldBadge.count} badge rows: #{Badge.count}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 44] = [
    "first new: true []",
    "first saved: true",
    "first persisted: true []",
    r#"handle other case: false ["Handle has already been taken"]"#,
    "handle other shop: true []",
    "handle no shop: true []",
    r#"code taken: false ["Code has already been taken"]"#,
    "gone saved: true",
    "code of a deleted row: true []",
    "code nil: true []",
    r#"rank taken in shop: false ["Rank is used in Handle"]"#,
    "rank in other shop: true []",
    r#"rank of the deleted row: false ["Rank is used in Handle"]"#,
    "nameless saved: true",
    r#"handle nil twice: false ["Handle has already been taken"]"#,
    r#"persisted, code cleared: false ["Code can't be blank"]"#,
    "persisted save: false",
    "persisted valid in create context: true []",
    "second saved: true",
    "second takes the deleted row's rank (update): true []",
    r#"second in create context: false ["Rank is used in Handle"]"#,
    r#"second update to nil code: false ["Code can't be blank"]"#,
    "second renamed onto first, other shop: true []",
    r#"second renamed onto first, same shop: false ["Handle has already been taken"]"#,
    "rows: 4",
    "gold named like a plain badge: true []",
    r#"gold named like gold: false ["Name has already been taken"]"#,
    r#"gold named like platinum: false ["Name has already been taken"]"#,
    r#"platinum named like gold: false ["Name has already been taken"]"#,
    "silver named like gold: true []",
    r#"silver with gold's code: false ["Code has already been taken"]"#,
    r#"gold with a plain badge's code: false ["Code has already been taken"]"#,
    r#"platinum with a plain badge's code: false ["Code has already been taken"]"#,
    r#"platinum with silver's code: false ["Code has already been taken"]"#,
    r#"plain badge with platinum's code: false ["Code has already been taken"]"#,
    r#"silver with platinum's code: false ["Code has already been taken"]"#,
    r#"platinum named like platinum: false ["Name has already been taken"]"#,
    "platinum named like a plain badge: true []",
    "silver named like platinum: true []",
    "platinum row: PlatinumBadge PlatinumBadge",
    "platinum keeps its own name and code: true []",
    r#"platinum takes gold's code: false ["Code has already been taken"]"#,
    r#"platinum renamed onto gold: false ["Name has already been taken"]"#,
    "platinum rows: 1 gold rows: 2 badge rows: 4",
];

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/schema.rb", SCHEMA),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/handle.rb", HANDLE),
    ("app/models/badge.rb", BADGE),
    ("app/models/gold_badge.rb", GOLD_BADGE),
    ("app/models/platinum_badge.rb", PLATINUM_BADGE),
    ("app/models/silver_badge.rb", SILVER_BADGE),
    ("app/models/uniq_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: no check error, and the emitted model asks the
/// relation.
#[test]
fn uniqueness_and_validation_context_compile() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let model = std::fs::read_to_string(emitted.join("app/models/handle.rb")).unwrap();
    for expected in [
        r#"ActiveRecord::Relation.new(Handle).where("LOWER(handle) = LOWER(?)", @handle)"#,
        "handle_relation = handle_relation.not({ id: [self.id] }) if self.persisted?",
        "code_relation = code_relation.where(is_deleted: false)",
        "rank_relation = rank_relation.where({ shop_id: @shop_id })",
        "if [:create].intersect?(Array(validation_context))",
    ] {
        assert!(model.contains(expected), "missing {expected:?} in:\n{model}");
    }
    let gold = std::fs::read_to_string(emitted.join("app/models/gold_badge.rb")).unwrap();
    let expected = r#"ActiveRecord::Relation.new(GoldBadge).where({ type: ["GoldBadge", "PlatinumBadge"] })"#;
    assert!(gold.contains(expected) && gold.contains("super()"), "missing {expected:?} in:\n{gold}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn uniqueness_and_validation_context_match_native_rails_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_root: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join("validates-uniq-native");
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    // The pg gem and Rails' native extensions come from Core's gem home,
    // as the oracle (tests/support/rails_oracle.rb) loads them.
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    let result = Command::new("ruby")
        .arg("tests/validates_uniqueness_pg_runtime.rb")
        .arg(&native_root)
        .arg(&*emitted)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .env("LD_PRELOAD", &jemalloc)
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(result.status.success(), "stdout={stdout}\nstderr={}", String::from_utf8_lossy(&result.stderr));
    let answers: serde_json::Value = serde_json::from_str(stdout.trim()).expect("driver prints JSON");
    let lines = |side: &str| -> Vec<String> {
        let run = &answers[side];
        assert_eq!(run["ok"], true, "{side} run failed:\n{}", run["stderr"].as_str().unwrap_or(""));
        run["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
    };
    let (native, emitted) = (lines("native"), lines("emitted"));
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
