//! Target-neutral fixture-loading lowering.
//!
//! Resolves every YAML fixture record into a structured plan:
//! which class it becomes, which columns receive literals, and which
//! columns are foreign-key references to another fixture's eventual
//! AUTOINCREMENT rowid. Per-target emitters consume the plan to
//! render `_load_all()` bodies + labeled getters in their own syntax.
//!
//! The lowering does NOT embed any persistence or runtime surface —
//! it only describes *what* goes into each INSERT, not *how* the
//! target wraps it. The Rust emitter wraps it in `article.save()`
//! plus a thread-local id map; Python might use sqlite3 directly;
//! Crystal might emit DB.exec. That's per-target.

use crate::dialect::{Association, Fixture, FixtureValue, Model};
use crate::schema::ColumnType;
use indexmap::IndexMap;
use crate::expr::Expr;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;
use crate::App;

/// Every fixture in an app, in declaration order. Emitters render
/// this once as a flat loader that runs at test setup; cross-fixture
/// FK references resolve through runtime lookup keyed on
/// `(target_fixture, target_label)`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoweredFixtureSet {
    pub fixtures: Vec<LoweredFixture>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoweredFixture {
    /// The YAML filename stem: `articles.yml` → `"articles"`.
    pub name: Symbol,
    /// The model class these records hydrate into: `Article`.
    pub class: ClassId,
    /// Records in declaration order.
    pub records: Vec<LoweredFixtureRecord>,
    /// Statements from the file's `<% … %>` ERB tags, to run once
    /// ahead of the inserts and in the same scope as the values.
    /// Empty for every fixture without ERB.
    pub preamble: Vec<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoweredFixtureRecord {
    pub label: Symbol,
    pub fields: Vec<LoweredFixtureField>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoweredFixtureField {
    pub column: Symbol,
    pub value: LoweredFixtureValue,
}

/// How to source a column's value at test-setup time.
#[derive(Clone, Debug, PartialEq)]
pub enum LoweredFixtureValue {
    /// Literal scalar from the YAML, typed by the column's schema. The
    /// `raw` field is the YAML string form; emitters apply
    /// target-specific literal syntax (Rust `"foo".to_string()`,
    /// Python `"foo"`, etc.).
    Literal { ty: Ty, raw: String },
    /// `article: one` in `comments.yml` — a reference to another
    /// fixture's label. Resolves to that fixture's AUTOINCREMENT id
    /// at runtime, because id assignment only happens after the
    /// referenced record INSERTs.
    FkLookup {
        target_fixture: Symbol,
        target_label: Symbol,
    },
    /// A `<%= … %>` tag's Ruby, as an expression. Unlike `Literal`
    /// there is no `raw` to re-render per target: the value only
    /// exists once the expression runs, so a target can carry it only
    /// if it can execute Ruby-shaped IR. Ruby-family emitters splice
    /// the expression into the loader body; the others skip the field
    /// and say so in the generated source.
    Ruby(Expr),
}

pub fn lower_fixtures(app: &App) -> LoweredFixtureSet {
    let fixtures = app
        .fixtures
        .iter()
        .map(|f| lower_fixture(f, app))
        .collect();
    LoweredFixtureSet { fixtures }
}

fn lower_fixture(fixture: &Fixture, app: &App) -> LoweredFixture {
    // From the PATH, not the flattened name: a fixture in a
    // subdirectory is namespaced (`push/subscriptions` ->
    // `Push::Subscription`), and `singularize_camelize` over the
    // flattened `push_subscriptions` names `PushSubscription` — a class
    // no model registers, so `model` below comes back `None` and every
    // field is dropped as "not a known column".
    let class = fixture.class_id();
    let model = app
        .models
        .iter()
        .find(|m| m.name == class);

    let records = fixture
        .records
        .iter()
        .map(|(label, raw_fields)| LoweredFixtureRecord {
            label: label.clone(),
            fields: raw_fields
                .iter()
                .flat_map(|(k, v)| resolve_field(k, v, model, app))
                .collect(),
        })
        .collect();

    LoweredFixture {
        name: fixture.name.clone(),
        class,
        records,
        preamble: fixture.preamble.clone(),
    }
}

/// Resolve one raw (key, value) entry into the lowered field(s) it
/// names. EMPTY when the key doesn't match a known column or
/// association — caller silently drops such entries today, mirroring
/// railcar's tolerance for scaffolding-only columns.
///
/// A Vec rather than an Option because one key can name TWO columns: a
/// POLYMORPHIC reference (`record: first (Message)`) writes both the id
/// and the type, and writing only one of them is worse than writing
/// neither — a row keyed to the right id under no type belongs to
/// every model at once.
fn resolve_field(
    key: &Symbol,
    value: &FixtureValue,
    model: Option<&Model>,
    app: &App,
) -> Vec<LoweredFixtureField> {
    resolve_field_inner(key, value, model, app).unwrap_or_default()
}

fn resolve_field_inner(
    key: &Symbol,
    value: &FixtureValue,
    model: Option<&Model>,
    app: &App,
) -> Option<Vec<LoweredFixtureField>> {
    let model = model?;

    if let Some(ty) = model.attributes.fields.get(key) {
        return Some(vec![LoweredFixtureField {
            column: key.clone(),
            value: match value {
                FixtureValue::Scalar(raw) => {
                    // An `enum` column's fixture value is the LABEL —
                    // campfire's `users.yml` says `role: administrator`
                    // where the column stores `1`. Rails maps it when it
                    // loads the fixture; the mapping is on the model, so
                    // map it here rather than writing the label text into
                    // an integer column. Without this David loaded with
                    // `role = "administrator"` and `david.administrator?`
                    // — which compares against `1` — answered false, so
                    // every test that signs in as an admin failed its
                    // first assertion.
                    let (ty, raw) = enum_stored_value(model, key, raw)
                        .unwrap_or_else(|| (ty.clone(), raw.clone()));
                    LoweredFixtureValue::Literal { ty, raw }
                }
                FixtureValue::Ruby(expr) => {
                    LoweredFixtureValue::Ruby(erb_value_for_column(expr, ty))
                }
            },
        }]);
    }

    // Only a scalar can name another fixture's label — `creator: <%=
    // … %>` would be an id-producing expression, and that lands on the
    // column branch above (or nowhere) rather than here.
    let Some(value) = value.as_scalar() else {
        return None;
    };
    // A label may be written as a YAML SYMBOL — campfire's rooms.yml
    // says `creator: :david` where its messages.yml says `creator:
    // david`. Both name the same fixture; Rails resolves the reference
    // by name and a Symbol scalar stringifies to it. Strip the sigil
    // for the lookup ONLY — the column branch above keeps its raw
    // scalar, so a genuine String column whose value starts with `:`
    // is untouched.
    //
    // Silently dropping the field is what makes this expensive: `Room`
    // validates its `creator` present, the generated loader calls
    // `save` and not `save!`, so all seven rooms vanished without a
    // word — and every messages row referencing one went with them.
    let value = value.strip_prefix(':').unwrap_or(value);

    for assoc in model.associations() {
        if let Association::BelongsTo {
            name,
            target,
            foreign_key,
            polymorphic,
            ..
        } = assoc
        {
            // A POLYMORPHIC reference names its class inline, which is
            // how Rails' fixtures spell the two columns as one key:
            //
            //   record: first (Message)   ->  record_id, record_type
            //
            // The declared `target` is meaningless here (the synthesizer
            // parks a placeholder), so the class comes from the value.
            if *polymorphic && name == key {
                let Some((label, class)) = split_polymorphic_reference(value) else {
                    continue;
                };
                let target_fixture = Symbol::from(
                    crate::naming::pluralize_snake(class.as_str()).as_str(),
                );
                let known = app
                    .fixtures
                    .iter()
                    .find(|f| f.name.as_str() == target_fixture.as_str())
                    .map(|f| f.records.keys().any(|l| l.as_str() == label))
                    .unwrap_or(false);
                if !known {
                    continue;
                }
                return Some(vec![
                    LoweredFixtureField {
                        column: foreign_key.clone(),
                        value: LoweredFixtureValue::FkLookup {
                            target_fixture,
                            target_label: Symbol::from(label.as_str()),
                        },
                    },
                    LoweredFixtureField {
                        column: Symbol::from(format!("{}_type", name.as_str())),
                        value: LoweredFixtureValue::Literal {
                            ty: Ty::Str,
                            raw: class,
                        },
                    },
                ]);
            }
            if name == key {
                let target_fixture = Symbol::from(
                    crate::naming::pluralize_snake(target.0.as_str()).as_str(),
                );
                let target_label = Symbol::from(value);
                let referenced = app
                    .fixtures
                    .iter()
                    .find(|f| f.name.as_str() == target_fixture.as_str());
                if referenced
                    .map(|f| f.records.keys().any(|l| l.as_str() == value))
                    .unwrap_or(false)
                {
                    return Some(vec![LoweredFixtureField {
                        column: foreign_key.clone(),
                        value: LoweredFixtureValue::FkLookup {
                            target_fixture,
                            target_label,
                        },
                    }]);
                }
            }
        }
    }
    None
}

/// `first (Message)` -> `("first", "Message")`. Rails' own spelling for
/// a polymorphic fixture reference, and the only one it accepts —
/// anything else (a bare label, a Ruby expression) is not a reference
/// this can resolve, because the type column has nowhere to come from.
fn split_polymorphic_reference(scalar: &str) -> Option<(String, String)> {
    let (label, rest) = scalar.split_once(" (")?;
    let class = rest.strip_suffix(')')?;
    if label.is_empty() || class.is_empty() {
        return None;
    }
    Some((label.trim().to_string(), class.trim().to_string()))
}

/// The value an enum column actually stores for `raw`, when `raw` names
/// one of that column's labels. `None` for a non-enum column or a value
/// that is not a label (a column whose enum maps `"active" => "active"`
/// stores the label itself, and this answers that too — harmlessly).
///
/// Reads the same `Model::enums` table `lower::enum_symbols` uses for
/// hand-written `where(role: :bot)`; the two are the query side and the
/// fixture side of one fact.
/// Whether a column stores text. A NULLable column arrives as
/// `Union { variants: [Str, Nil] }`, not `Str` — and campfire's
/// `password_digest` is exactly that, so a bare `matches!(ty, Ty::Str)`
/// matched none of the four fields this rule exists for.
fn is_string_column(ty: &Ty) -> bool {
    match ty {
        Ty::Str => true,
        Ty::Union { variants } => {
            let mut saw_str = false;
            for v in variants {
                match v {
                    Ty::Str => saw_str = true,
                    Ty::Nil => {}
                    _ => return false,
                }
            }
            saw_str
        }
        _ => false,
    }
}

/// A `<%= … %>` fixture value, made faithful to what Rails actually
/// loads.
///
/// RAILS RENDERS THE FIXTURE FILE TO TEXT FIRST. `password_digest:
/// <%= password_digest %>` is ERB over a YAML document: the tag's value
/// goes through `to_s` on its way into the document, and YAML then
/// reads that text back as a scalar. We bind the expression straight
/// into `instance.<column> = <expr>` instead, which skips the
/// stringification — so campfire's `users.yml`, whose preamble binds
/// `BCrypt::Password.create("secret123456")`, assigned the Password
/// OBJECT to a string column.
///
/// Under CRuby that is invisible: the object is stored as-is and
/// duck-types through every later read. Under spinel the object lands
/// in the String-typed slot as `"\xDD"` — one byte, no diagnostic —
/// and the next `BCrypt::Password.new` raises `invalid hash` out of
/// `_fixtures_load!`, which every `setup` runs. That was 60 of the
/// spinel suite lane's 288 tests, in whole controller files at a time.
///
/// Only a STRING column needs the wrap. For Int/Float/Bool the text
/// round-trip is `5` -> `"5"` -> `5`, which is what splicing the value
/// already does; wrapping those would be strictly wrong.
///
/// `String#to_s` is identity, so this costs nothing on the common case
/// where the tag already produced a String.
///
/// NOT the whole of `ActiveModel::Type::String#cast`: that maps `true`
/// to `"t"` and `false` to `"f"`, where `to_s` gives `"true"`/`"false"`.
/// A boolean interpolated into a string column is the one shape this
/// still gets wrong, and it is named here rather than silently rounded
/// off — the cast is a runtime branch on the value, not a wrap the
/// lowering can apply statically.
fn erb_value_for_column(expr: &Expr, ty: &Ty) -> Expr {
    if !is_string_column(ty) {
        return expr.clone();
    }
    crate::lower::typing::with_ty(
        Expr::new(
            expr.span,
            crate::expr::ExprNode::Send {
                recv: Some(expr.clone()),
                method: Symbol::from("to_s"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        ),
        Ty::Str,
    )
}

fn enum_stored_value(
    model: &Model,
    column: &Symbol,
    raw: &str,
) -> Option<(Ty, String)> {
    let labels = model.enums.get(column)?;
    let (_, stored) = labels.iter().find(|(label, _)| label == raw)?;
    match stored {
        crate::expr::Literal::Int { value } => Some((Ty::Int, value.to_string())),
        crate::expr::Literal::Str { value } => Some((Ty::Str, value.clone())),
        _ => None,
    }
}

/// Every fixture entry `lower_fixtures` would load as nothing: a set with
/// no model to load it, or a field that names neither a column nor a
/// belongs_to whose target label exists. Rails loads each (or raises on an
/// unknown column), so a dropped entry is different data, not a no-op.
pub(crate) fn unloaded_fixture_entries(app: &App) -> Vec<String> {
    let mut out = Vec::new();
    for fixture in &app.fixtures {
        let class = fixture.class_id();
        let Some(model) = app.models.iter().find(|m| m.name == class) else {
            out.push(format!("fixture set `{}` loads {}, which is not a model of this app", fixture.path, class.0));
            continue;
        };
        for (label, fields) in &fixture.records {
            for (key, value) in fields {
                // `parent: ~` on a belongs_to: Rails skips a nil reference
                // and leaves the foreign key unset, as the loader does.
                let nil_reference = value.as_scalar() == Some("")
                    && model.associations().any(|a| matches!(a, Association::BelongsTo { name, .. } if name == key));
                if !nil_reference && resolve_field(key, value, Some(model), app).is_empty() {
                    out.push(format!("fixture `{}`.{label}.{key} names neither a {} column nor a belongs_to with that fixture label", fixture.path, class.0));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Raw fixture rows: the CRuby PostgreSQL tree.
//
// Rails loads a fixture set as table rows: `ActiveRecord::FixtureSet`
// turns each YAML record into a column → value Hash, and
// `insert_fixtures_set` writes them with one multi-row INSERT per table,
// casting each value through its column's type. No model is
// instantiated, so no writer, callback or validation runs. The plan
// below is that row, resolved at compile time where the YAML decides it
// (ids, references, enum labels), and left to the runtime's column cast
// where it does not (ERB values, `now`). The CRuby runtime inserts it
// (`ActiveRecord::TestFixtures.insert_fixture_rows`).

/// Which loader's rules decide a set's table, ids and references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureRules {
    /// Rails' `ActiveRecord::FixtureSet` (TableRow): the model's table,
    /// enum labels mapped, `belongs_to` names resolved through the model.
    Rails,
    /// Shopify Core's `FasterFixtures::FixtureSet`, which Core's tests
    /// load fixtures with: the table is `_fixture: table_name` or the
    /// set name, `island` is stamped, a reference resolves through the
    /// table's `<name>_id` column first, and enum labels are not mapped.
    FasterFixtures,
}

/// The app's fixture loader. Core carries `FasterFixtures::FixtureSet`
/// (components/platform/essentials/lib/platform/testing/faster_fixtures),
/// and its test boot loads every set through it; every other app loads
/// through Rails.
pub fn fixture_rules(app: &App) -> FixtureRules {
    let faster = app
        .library_classes
        .iter()
        .any(|lc| lc.name.0.as_str().trim_start_matches("::") == "FasterFixtures::FixtureSet");
    if faster { FixtureRules::FasterFixtures } else { FixtureRules::Rails }
}

/// One value of a raw fixture row.
#[derive(Clone, Debug, PartialEq)]
pub enum RawFixtureValue {
    Nil,
    Bool(bool),
    Int(i64),
    /// An integer wider than `i64`, as its decimal digits.
    BigInt(String),
    Float(f64),
    Str(String),
    /// A YAML mapping or sequence, in source order: the column cast
    /// decides (JSON for a json column, a YAML dump for a text one).
    Structured(serde_yaml_ng::Value),
    /// A `<%= … %>` value, cast by the runtime.
    Ruby(Expr),
    /// A reference whose label an ERB tag produces: the runtime turns it
    /// into the referenced row's id (`identify`, by the column's type).
    /// `integer_passes` is FasterFixtures' rule that an Integer is the id.
    RubyReference { value: Expr, uuid: bool, integer_passes: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawFixtureRow {
    pub label: Symbol,
    /// Column → value, in YAML order with generated keys after.
    pub values: IndexMap<Symbol, RawFixtureValue>,
    /// What the label's accessor passes to `<Model>.find`: one value, or
    /// (for a composite-key model) the key's values in order.
    pub find_key: Vec<RawFixtureValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawFixtureSet {
    /// The flattened set name (`fulfillments_batches`): the module name.
    pub name: Symbol,
    /// Rails' set name (`fulfillments/batches`).
    pub path: Symbol,
    pub class: ClassId,
    pub table: Symbol,
    /// The table's writable columns with their cast type
    /// (`column_cast_type`), in schema order. `None` when the schema has
    /// no such table.
    pub columns: Option<Vec<(Symbol, &'static str)>>,
    /// The timestamp columns the loader fills with `now` when a row
    /// omits them.
    pub timestamps: Vec<Symbol>,
    /// `<Model>.find` takes the key as an Array.
    pub composite_find: bool,
    pub rows: Vec<RawFixtureRow>,
    pub preamble: Vec<Expr>,
    /// Refusals the set raises, by name, when it loads (the first one).
    pub errors: Vec<String>,
    /// The loader whose rules resolved the rows. Under FasterFixtures a
    /// key the compiled schema has no column for may still name a live
    /// column, or its `<key>_id`, which the runtime resolves.
    pub rules: FixtureRules,
}

/// Rails' `ActiveRecord::FixtureSet::MAX_ID`.
const FIXTURE_MAX_ID: u32 = (1 << 30) - 1;

/// `ActiveRecord::FixtureSet.identify(label)`: `Zlib.crc32(label) % MAX_ID`.
pub fn fixture_identify(label: &str) -> i64 {
    i64::from(crc32(label.as_bytes()) % FIXTURE_MAX_ID)
}

/// `ActiveRecord::FixtureSet.identify(label, :uuid)`:
/// `Digest::UUID.uuid_v5(Digest::UUID::OID_NAMESPACE, label)`.
pub fn fixture_identify_uuid(label: &str) -> String {
    const OID_NAMESPACE: [u8; 16] = [
        0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
    ];
    let mut input = OID_NAMESPACE.to_vec();
    input.extend_from_slice(label.as_bytes());
    let mut d = sha1(&input);
    d[6] = (d[6] & 0x0f) | 0x50;
    d[8] = (d[8] & 0x3f) | 0x80;
    let hex: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn identify_value(label: &str, uuid: bool) -> RawFixtureValue {
    if uuid {
        RawFixtureValue::Str(fixture_identify_uuid(label))
    } else {
        RawFixtureValue::Int(fixture_identify(label))
    }
}

/// zlib's CRC-32 (IEEE 802.3, reflected).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// SHA-1 (FIPS 180-4), for `uuid_v5`.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut data = message.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in data.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The cast the runtime applies to a column's fixture value, by the
/// ActiveModel type Rails' PostgreSQL adapter looks up for it.
pub fn column_cast_type(col: &ColumnType) -> &'static str {
    match col {
        ColumnType::Integer | ColumnType::BigInt | ColumnType::Reference { .. } => "integer",
        ColumnType::Float => "float",
        ColumnType::Decimal { .. } => "decimal",
        ColumnType::String { .. } | ColumnType::Text => "string",
        ColumnType::Boolean => "boolean",
        ColumnType::Date => "date",
        ColumnType::DateTime => "datetime",
        ColumnType::Time => "time",
        ColumnType::Binary => "binary",
        ColumnType::Json => "json",
        ColumnType::Jsonb => "jsonb",
        ColumnType::Uuid => "uuid",
    }
}

/// The app's models by name, and each library class's parent: an STI
/// subclass is a library class, not a Model, and loads through its base.
struct ModelIndex<'a> {
    models: crate::det::HashMap<&'a ClassId, &'a Model>,
    parents: crate::det::HashMap<&'a ClassId, &'a ClassId>,
}

impl<'a> ModelIndex<'a> {
    fn new(app: &'a App) -> Self {
        let mut models = crate::det::HashMap::default();
        for m in &app.models {
            models.entry(&m.name).or_insert(m);
        }
        let mut parents = crate::det::HashMap::default();
        for lc in &app.library_classes {
            if let Some(parent) = &lc.parent {
                parents.entry(&lc.name).or_insert(parent);
            }
        }
        ModelIndex { models, parents }
    }

    /// The model `class` is, or the model it descends from.
    fn model_for(&self, class: &ClassId) -> Option<&'a Model> {
        if let Some(m) = self.models.get(class) {
            return Some(m);
        }
        let mut cursor = class;
        for _ in 0..16 {
            let parent = self.parents.get(cursor)?;
            if let Some(m) = self.models.get(parent) {
                return Some(m);
            }
            cursor = parent;
        }
        None
    }
}

/// The set's model: `_fixture: model_class`, else the path's class (an
/// STI subclass answers its base model, whose table and associations it
/// shares).
fn fixture_model<'a>(fixture: &Fixture, index: &ModelIndex<'a>) -> Option<&'a Model> {
    index.model_for(&fixture.class_id())
}

/// The model's table, as Rails' `table_name` answers it: an STI
/// subclass's rows live in its base class's table.
fn model_table(model: &Model, app: &App) -> Symbol {
    let mut current = model;
    for _ in 0..64 {
        if app.schema.tables.contains_key(&current.table.0) {
            return current.table.0.clone();
        }
        let Some(parent) = current
            .parent
            .as_ref()
            .and_then(|p| app.models.iter().find(|m| &m.name == p))
        else {
            break;
        };
        current = parent;
    }
    model.table.0.clone()
}

/// The table a set's rows insert into. `_fixture: table_name` first (a
/// FasterFixtures key); then, under Rails, the model's table (an STI
/// subclass's base table); else Rails' `default_fixture_table_name`,
/// the set name with `/` as `_`, which is what FasterFixtures uses.
fn fixture_table_name(fixture: &Fixture, app: &App, index: &ModelIndex<'_>, rules: FixtureRules) -> Symbol {
    if let Some(table) = &fixture.table_name {
        return table.clone();
    }
    if rules == FixtureRules::Rails {
        if let Some(model) = fixture_model(fixture, index) {
            return model_table(model, app);
        }
    }
    fixture.name.clone()
}

/// Each set's table (`fixture_table_name`), by set name.
pub fn fixture_table_names(app: &App) -> crate::det::HashMap<Symbol, Symbol> {
    let rules = fixture_rules(app);
    let index = ModelIndex::new(app);
    app.fixtures
        .iter()
        .map(|f| (f.name.clone(), fixture_table_name(f, app, &index, rules)))
        .collect()
}

/// Every set's raw rows, in declaration order.
pub fn lower_fixture_rows(app: &App) -> Vec<RawFixtureSet> {
    let rules = fixture_rules(app);
    let index = ModelIndex::new(app);
    app.fixtures.iter().map(|f| lower_raw_fixture_set(f, app, &index, rules)).collect()
}

const TIMESTAMP_COLUMNS: [&str; 4] = ["created_at", "created_on", "updated_at", "updated_on"];

fn lower_raw_fixture_set(fixture: &Fixture, app: &App, index: &ModelIndex<'_>, rules: FixtureRules) -> RawFixtureSet {
    let class = fixture.class_id();
    let model = fixture_model(fixture, index);
    let table_name = fixture_table_name(fixture, app, index, rules);
    let table = app.schema.tables.get(&table_name);
    let mut errors = Vec::new();
    // Rails' PostgreSQL adapter supports virtual columns, so a generated
    // column is not one the INSERT can name.
    let columns: Option<Vec<(Symbol, &'static str)>> = table.map(|t| {
        t.columns
            .iter()
            .filter(|c| c.generated.is_none())
            .map(|c| (c.name.clone(), column_cast_type(&c.col_type)))
            .collect()
    });
    if table.is_none() {
        errors.push(format!(
            "roundhouse: fixture set `{}`: table `{}` is not in this tree's schema",
            fixture.path, table_name
        ));
    }
    let col_type = |name: &str| -> Option<&'static str> {
        columns.as_ref()?.iter().find(|(c, _)| c.as_str() == name).map(|(_, t)| *t)
    };
    let has_col = |name: &str| col_type(name).is_some();
    let defaulted: Vec<Symbol> = table
        .map(|t| t.columns.iter().filter(|c| c.default.is_some()).map(|c| c.name.clone()).collect())
        .unwrap_or_default();

    // Rails fills timestamps for a model that records them; FasterFixtures
    // for every table that has the columns.
    let timestamps: Vec<Symbol> = if rules == FixtureRules::FasterFixtures || model.is_some() {
        TIMESTAMP_COLUMNS.iter().filter(|c| has_col(c)).map(|c| Symbol::from(*c)).collect()
    } else {
        Vec::new()
    };

    // The table's key, as the schema cache answers `primary_keys`.
    let table_pk: Vec<Symbol> = table
        .map(|t| {
            if !t.primary_key_columns.is_empty() {
                t.primary_key_columns.clone()
            } else {
                t.columns.iter().filter(|c| c.primary_key).map(|c| c.name.clone()).collect()
            }
        })
        .unwrap_or_default();
    // The model's key, which `find` takes.
    let model_pk: Vec<Symbol> = match model {
        Some(m) if !m.composite_primary_key.is_empty() => m.composite_primary_key.clone(),
        Some(m) => vec![m.primary_key.clone().unwrap_or_else(|| Symbol::from("id"))],
        None if table_pk.len() > 1 => table_pk.clone(),
        None => vec![table_pk.first().cloned().unwrap_or_else(|| Symbol::from("id"))],
    };
    let composite_find = model_pk.len() > 1;

    let mut rows = Vec::new();
    for (label, fields) in &fixture.records {
        let mut row: IndexMap<Symbol, RawFixtureValue> = IndexMap::new();
        for (key, value) in fields {
            let v = match value {
                FixtureValue::Scalar(raw) => scalar_fixture_value(fixture, label, key, raw),
                FixtureValue::Ruby(expr) => RawFixtureValue::Ruby(expr.clone()),
            };
            row.insert(key.clone(), v);
        }
        match rules {
            FixtureRules::FasterFixtures => faster_fixtures_row(
                fixture, label, &mut row, model, &table_pk, &defaulted, &col_type, &mut errors,
            ),
            FixtureRules::Rails => rails_row(label, &mut row, model, &model_pk, &col_type),
        }
        let find_key = model_pk
            .iter()
            .map(|k| match row.get(k) {
                Some(RawFixtureValue::Ruby(_) | RawFixtureValue::RubyReference { .. }) | None => {
                    RawFixtureValue::Nil
                }
                Some(v) => v.clone(),
            })
            .collect();
        rows.push(RawFixtureRow { label: label.clone(), values: row, find_key });
    }

    RawFixtureSet {
        name: fixture.name.clone(),
        path: fixture.path.clone(),
        class,
        table: table_name,
        columns,
        timestamps,
        composite_find,
        rows,
        preamble: fixture.preamble.clone(),
        errors,
        rules,
    }
}

/// A YAML scalar as the value Psych loaded: `records` keeps the text,
/// `scalar_kinds` the type. Rails interpolates `$LABEL` into strings.
fn scalar_fixture_value(fixture: &Fixture, label: &Symbol, key: &Symbol, raw: &str) -> RawFixtureValue {
    use crate::dialect::FixtureScalarKind as K;
    match fixture.scalar_kind(label, key) {
        Some(K::Null) => RawFixtureValue::Nil,
        Some(K::Bool) => RawFixtureValue::Bool(raw == "true"),
        Some(K::Int) => raw
            .parse::<i64>()
            .map(RawFixtureValue::Int)
            .unwrap_or_else(|_| RawFixtureValue::BigInt(raw.to_string())),
        Some(K::Float) => match raw.parse::<f64>() {
            Ok(f) if f.is_finite() => RawFixtureValue::Float(f),
            _ => RawFixtureValue::Str(raw.to_string()),
        },
        // `records` keeps it as JSON text, which YAML reads back with
        // its keys in source order.
        Some(K::Mapping | K::Sequence) => serde_yaml_ng::from_str::<serde_yaml_ng::Value>(raw)
            .map(RawFixtureValue::Structured)
            .unwrap_or_else(|_| RawFixtureValue::Str(raw.to_string())),
        None => RawFixtureValue::Str(raw.replace("$LABEL", label.as_str())),
    }
}

/// `FasterFixtures::FixtureSet#read_fixture_files` and `#table_rows`.
#[allow(clippy::too_many_arguments)]
fn faster_fixtures_row(
    fixture: &Fixture,
    label: &Symbol,
    row: &mut IndexMap<Symbol, RawFixtureValue>,
    model: Option<&Model>,
    table_pk: &[Symbol],
    defaulted: &[Symbol],
    col_type: &dyn Fn(&str) -> Option<&'static str>,
    errors: &mut Vec<String>,
) {
    let has_col = |name: &str| col_type(name).is_some();
    let is_uuid = |name: &str| col_type(name) == Some("uuid");
    let absent = |row: &IndexMap<Symbol, RawFixtureValue>, k: &str| {
        matches!(row.get(&Symbol::from(k)), None | Some(RawFixtureValue::Nil))
    };
    // stamp_default_shop_island: ISLAND_COLUMN "island", DEFAULT_ISLAND 0.
    if has_col("island") && absent(row, "island") {
        row.insert(Symbol::from("island"), RawFixtureValue::Int(0));
    }
    // default_identifier_column: the single key, or "id" for a composite one.
    let pk_name: Option<Symbol> = match table_pk {
        [] => None,
        [one] => Some(one.clone()),
        _ => Some(Symbol::from("id")),
    };
    match &pk_name {
        Some(pk) if has_col(pk.as_str()) => {
            if absent(row, pk.as_str()) {
                row.insert(pk.clone(), identify_value(label.as_str(), is_uuid(pk.as_str())));
            }
        }
        _ if table_pk.len() > 1 => {
            // A composite key without an `id` column: every key column
            // comes from the YAML (an association key, or a column default).
            let missing: Vec<&str> = table_pk
                .iter()
                .filter(|c| {
                    !row.contains_key(*c)
                        && !(c.as_str() == "shop_id" && row.contains_key(&Symbol::from("shop")))
                        && !defaulted.contains(c)
                })
                .map(|c| c.as_str())
                .collect();
            if !missing.is_empty() {
                errors.push(format!(
                    "Fixture \"{label}\" in {} is missing primary key column(s): {}. Tables with a composite primary key and no `id` column require all PK columns to be provided in the fixture YAML, either as columns or via association keys (columns with database defaults are exempt).",
                    fixture.path,
                    missing.join(", ")
                ));
            }
        }
        _ => {}
    }
    if pk_name.as_ref().map(|p| p.as_str()) != Some("id") && has_col("id") && !row.contains_key(&Symbol::from("id")) {
        row.insert(Symbol::from("id"), identify_value(label.as_str(), is_uuid("id")));
    }

    // table_rows: a key that names no column is a reference — through the
    // `<key>_id` column, else the association's one writable foreign key.
    let keys: Vec<Symbol> = row.keys().filter(|k| !has_col(k.as_str())).cloned().collect();
    for key in keys {
        let mut fk_id = format!("{key}_id");
        let mut fk_type = format!("{key}_type");
        if !has_col(&fk_id) {
            let assoc = model.and_then(|m| {
                m.associations().find_map(|a| match a {
                    Association::BelongsTo { name, foreign_key, .. } if name == &key => Some(foreign_key.clone()),
                    _ => None,
                })
            });
            match assoc {
                Some(fk) if has_col(fk.as_str()) => {
                    fk_id = fk.as_str().to_string();
                    fk_type = format!("{key}_type");
                }
                _ => continue,
            }
        }
        let Some(value) = row.shift_remove(&key) else { continue };
        let uuid = is_uuid(&fk_id);
        let resolved = match value {
            RawFixtureValue::Nil | RawFixtureValue::Bool(false) => continue,
            RawFixtureValue::Int(i) => RawFixtureValue::Int(i),
            RawFixtureValue::BigInt(s) => RawFixtureValue::BigInt(s),
            RawFixtureValue::Str(s) => {
                if let Some((target, class)) = polymorphic_fixture_value(&s) {
                    row.insert(Symbol::from(fk_type.as_str()), RawFixtureValue::Str(class));
                    identify_value(&target, uuid)
                } else {
                    identify_value(&s, uuid)
                }
            }
            RawFixtureValue::Ruby(expr) => {
                RawFixtureValue::RubyReference { value: expr, uuid, integer_passes: true }
            }
            other => {
                let text = match &other {
                    RawFixtureValue::Bool(b) => b.to_string(),
                    RawFixtureValue::Float(f) => f.to_string(),
                    RawFixtureValue::Structured(v) => serde_json::to_string(v).unwrap_or_default(),
                    _ => String::new(),
                };
                identify_value(&text, uuid)
            }
        };
        row.insert(Symbol::from(fk_id.as_str()), resolved);
    }
}

/// FasterFixtures' POLYMORPHIC_VALUE, `/^(\S+)\s*\(([^\)]*)\)\s*$/`.
fn polymorphic_fixture_value(s: &str) -> Option<(String, String)> {
    let trimmed = s.trim_end();
    let inner = trimmed.strip_suffix(')')?;
    let open = inner.find('(')?;
    let (head, class) = (&inner[..open], &inner[open + 1..]);
    if class.contains(')') {
        return None;
    }
    let target = head.trim_end();
    if target.is_empty() || target.chars().any(char::is_whitespace) {
        return None;
    }
    Some((target.to_string(), class.to_string()))
}

/// Rails' `TableRow#fill_row_model_attributes` (key, enums, belongs_to).
fn rails_row(
    label: &Symbol,
    row: &mut IndexMap<Symbol, RawFixtureValue>,
    model: Option<&Model>,
    model_pk: &[Symbol],
    col_type: &dyn Fn(&str) -> Option<&'static str>,
) {
    let Some(model) = model else { return };
    let has_col = |name: &str| col_type(name).is_some();
    let is_uuid = |name: &str| col_type(name) == Some("uuid");
    // generate_primary_key / generate_composite_primary_key.
    if model_pk.len() > 1 {
        let id = fixture_identify(label.as_str());
        for (index, column) in model_pk.iter().enumerate() {
            if has_col(column.as_str()) && !row.contains_key(column) {
                let value = ((i128::from(id) << index) % i128::from(FIXTURE_MAX_ID)) as i64;
                row.insert(column.clone(), RawFixtureValue::Int(value));
            }
        }
    } else if let Some(pk) = model_pk.first() {
        if has_col(pk.as_str()) && !row.contains_key(pk) {
            row.insert(pk.clone(), identify_value(label.as_str(), is_uuid(pk.as_str())));
        }
    }
    // resolve_enums: a label becomes its stored value.
    for (column, mapping) in &model.enums {
        if let Some(RawFixtureValue::Str(raw)) = row.get(column) {
            if let Some((_, stored)) = mapping.iter().find(|(l, _)| l == raw) {
                let stored = match stored {
                    crate::expr::Literal::Int { value } => RawFixtureValue::Int(*value),
                    crate::expr::Literal::Str { value } => RawFixtureValue::Str(value.clone()),
                    _ => continue,
                };
                row.insert(column.clone(), stored);
            }
        }
    }
    // resolve_sti_reflections: a belongs_to name is its foreign key.
    for assoc in model.associations() {
        let Association::BelongsTo { name, foreign_key, polymorphic, .. } = assoc else { continue };
        if name == foreign_key || !row.contains_key(name) {
            continue;
        }
        let value = row.shift_remove(name).expect("present");
        let uuid = is_uuid(foreign_key.as_str());
        let resolved = match value {
            RawFixtureValue::Nil | RawFixtureValue::Bool(false) => continue,
            RawFixtureValue::Str(mut s) => {
                if *polymorphic {
                    if let Some(open) = s.rfind('(') {
                        let tail = s[open + 1..].trim_end();
                        if let Some(class) = tail.strip_suffix(')') {
                            if !class.contains(')') {
                                row.insert(Symbol::from(format!("{name}_type")), RawFixtureValue::Str(class.to_string()));
                                s = s[..open].trim_end().to_string();
                            }
                        }
                    }
                }
                identify_value(&s, uuid)
            }
            RawFixtureValue::Int(i) => identify_value(&i.to_string(), uuid),
            RawFixtureValue::Ruby(expr) => {
                RawFixtureValue::RubyReference { value: expr, uuid, integer_passes: false }
            }
            other => {
                let text = match &other {
                    RawFixtureValue::Bool(b) => b.to_string(),
                    RawFixtureValue::Float(f) => f.to_string(),
                    RawFixtureValue::BigInt(s) => s.clone(),
                    RawFixtureValue::Structured(v) => serde_json::to_string(v).unwrap_or_default(),
                    _ => String::new(),
                };
                identify_value(&text, uuid)
            }
        };
        row.insert(foreign_key.clone(), resolved);
    }
}
