//! Lower YAML fixtures into one `<Plural>Fixtures` LibraryClass per
//! file. Each labeled record becomes `def self.<label>` returning a
//! typed model instance built via the model's `.new({field: value,
//! ...})` constructor.
//!
//! Companion rewrite: `articles(:one)` calls in test bodies get
//! rewritten to `ArticlesFixtures.one()`. Self-describing — the call
//! site lands at concrete dispatch (no runtime fixture-lookup helper
//! needed) and types through the registry like any other class call.
//!
//! IDs: assigned 1-indexed within each fixture file, mirroring
//! Rails's AUTOINCREMENT-on-load behavior. Predictable so test
//! setups can `Article.find(1)` if they need to.

use crate::det::{HashMap, HashSet};

use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::lower::fixtures::{
    LoweredFixture, LoweredFixtureRecord, LoweredFixtureSet, LoweredFixtureValue,
};
use crate::lower::controller_to_library::util::map_expr;
use crate::lower::model_to_library::schema::{enum_storage_writer, schema_storage_default};
use crate::lower::typing::{fn_sig, lit_int, lit_str, with_ty};
use crate::naming::camelize;
use crate::schema::Column;
use crate::span::Span;
use crate::ty::Ty;
use crate::App;

/// Bulk entry. Lower every fixture file into a `<Plural>Fixtures`
/// LibraryClass. Returns an empty Vec when the app has no fixtures
/// (apps without test fixtures skip the artifact).
pub fn lower_fixtures_to_library_classes(app: &App) -> Vec<LibraryClass> {
    let lowered = crate::lower::lower_fixtures(app);
    // `by_label` is DEMAND-GATED — emitted only for a fixture some test
    // body reaches with a variable label. Unconditionally is wrong, and
    // strict targets are what say so: its `label` param is Untyped, and
    // rust renders that `serde_json::Value`, which answers no `to_s`.
    // The blog writes `articles(:one)` everywhere and never needs the
    // table, so emitting it there was four E0599s of dead code.
    let dynamic = fixtures_reached_by_variable(app);
    load_order(&lowered)
        .into_iter()
        .map(|f| {
            let defaults: Vec<&Column> = app.models.iter()
                .find(|m| m.name == f.class)
                .and_then(|m| app.schema.tables.get(&m.table.0)
                    .map(|t| t.columns.iter()
                        .filter(|c| m.enum_defaults.contains_key(&c.name)).collect()))
                .unwrap_or_default();
            build_fixture_class(f, &lowered, dynamic.contains(&f.name), &defaults)
        })
        .collect()
}

/// The fixtures in PARENT-BEFORE-CHILD order, so a loader that walks
/// this list never inserts a row whose `belongs_to` target does not
/// exist yet.
///
/// The order used to be the file order (alphabetical), with a comment
/// admitting it merely "approximates" the dependency order for the
/// blog's Articles → Comments shape and deferring the real thing until
/// a fixture set broke it. campfire is that fixture set, and it broke
/// it completely rather than partially: `users` sorts LAST, so rooms,
/// messages, memberships, sessions and boosts each referenced a user
/// that did not exist yet. Every one of those rows failed its
/// `belongs_to` presence validation and the generated loader calls
/// `save`, not `save!` — so five whole tables loaded as zero rows,
/// silently, and every test that named a room died on `RecordNotFound`
/// a long way from the cause.
///
/// The edges are already in the lowered IR: an `FkLookup` field names
/// the fixture it points at. Kahn's algorithm over those, ties broken
/// by the incoming order so the result stays deterministic and the
/// blog's list does not move. A cycle (mutually-referencing fixtures,
/// which Rails permits because it assigns ids up front) keeps its
/// members in incoming order at the end — no ordering can satisfy it,
/// and dropping them from the list would be worse than emitting them
/// in the order that exists today.
fn load_order(lowered: &LoweredFixtureSet) -> Vec<&LoweredFixture> {
    let mut deps: HashMap<&str, HashSet<&str>> = HashMap::default();
    for f in &lowered.fixtures {
        let entry = deps.entry(f.name.as_str()).or_default();
        for rec in &f.records {
            for field in &rec.fields {
                if let LoweredFixtureValue::FkLookup { target_fixture, .. } = &field.value {
                    // A self-reference (a `parent_id` on the same
                    // fixture) is satisfied within one loader call, so
                    // it is not an edge.
                    if target_fixture.as_str() != f.name.as_str() {
                        entry.insert(target_fixture.as_str());
                    }
                }
            }
        }
    }
    let mut emitted: HashSet<&str> = HashSet::default();
    let mut out: Vec<&LoweredFixture> = Vec::new();
    // At most one pass per fixture: each round emits every fixture whose
    // dependencies are already out, so a chain of depth N settles in N
    // rounds and a cycle stops making progress.
    while out.len() < lowered.fixtures.len() {
        let before = out.len();
        for f in &lowered.fixtures {
            let name = f.name.as_str();
            if emitted.contains(name) {
                continue;
            }
            let ready = deps
                .get(name)
                .map(|d| d.iter().all(|t| emitted.contains(t) || !deps.contains_key(t)))
                .unwrap_or(true);
            if ready {
                emitted.insert(name);
                out.push(f);
            }
        }
        if out.len() == before {
            // Cycle: emit what is left in incoming order and stop.
            for f in &lowered.fixtures {
                if !emitted.contains(f.name.as_str()) {
                    out.push(f);
                }
            }
            break;
        }
    }
    out
}

/// Fixture names called with something other than a symbol literal —
/// `users(user)` rather than `users(:david)`. campfire's spliced
/// `SessionTestHelper#sign_in` is the only shape in the corpus that
/// does this.
fn fixtures_reached_by_variable(app: &App) -> crate::det::HashSet<Symbol> {
    let names: Vec<Symbol> = app.fixtures.iter().map(|f| f.name.clone()).collect();
    let mut out = crate::det::HashSet::default();
    for tm in &app.test_modules {
        let bodies = tm
            .tests
            .iter()
            .map(|t| &t.body)
            .chain(tm.helpers.iter().map(|h| &h.body))
            .chain(tm.setup.iter());
        for body in bodies {
            collect_variable_fixture_calls(body, &names, &mut out);
        }
    }
    out
}

fn collect_variable_fixture_calls(
    e: &Expr,
    names: &[Symbol],
    out: &mut crate::det::HashSet<Symbol>,
) {
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node {
        if args.len() == 1
            && names.iter().any(|n| n == method)
            && !matches!(&*args[0].node, ExprNode::Lit { value: Literal::Sym { .. } })
        {
            out.insert(method.clone());
        }
    }
    e.node.for_each_child(&mut |c| collect_variable_fixture_calls(c, names, out));
}

fn build_fixture_class(
    f: &LoweredFixture,
    all: &LoweredFixtureSet,
    wants_by_label: bool,
    defaults: &[&Column],
) -> LibraryClass {
    let owner_name = format!("{}Fixtures", camelize(f.name.as_str()));
    let owner_id = ClassId(Symbol::from(owner_name.clone()));
    let class_ty = Ty::Class { id: f.class.clone(), args: vec![] };

    // Each label method (`one`, `two`, …) returns the persisted record
    // looked up by id. The actual insert happens in `_fixtures_load!`,
    // which the test_helper's FixtureLoader invokes after each
    // SchemaSetup.reset!.
    let mut methods: Vec<MethodDef> = f
        .records
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let id = (idx + 1) as i64;
            let body = build_find_call(&f.class, id);
            MethodDef {
                visibility: crate::dialect::MethodVisibility::Public,
                unsupported_formals: None,
                has_anonymous_block: false,
                name_span: crate::span::Span::synthetic(),
                name: r.label.clone(),
                receiver: MethodReceiver::Class,
                params: Vec::new(),
                body,
                signature: Some(fn_sig(vec![], class_ty.clone())),
                effects: EffectSet::default(),
                enclosing_class: Some(owner_id.0.clone()),
                kind: AccessorKind::Method,
                is_async: false,
            mutates_self: false,
            block_param: None,
            }
        })
        .collect();

    // `by_label(name)` — the DYNAMIC accessor. `users(:david)` is
    // rewritten at the call site to `UsersFixtures.david()`, but Rails'
    // fixture accessor also takes a variable, and campfire's
    // `SessionTestHelper#sign_in` is exactly that shape:
    //
    //     def sign_in(user)
    //       user = users(user) unless user.is_a? User
    //
    // A label→record table has to exist somewhere for that to resolve.
    // Built as an if/elsif chain over the KNOWN labels rather than a
    // runtime `send`: the label set is a compile-time fact, and a chain
    // of string compares needs nothing from a target's dynamic dispatch
    // (spinel's AOT model has no constant table to `send` through).
    if wants_by_label {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("by_label"),
        receiver: MethodReceiver::Class,
        params: vec![crate::dialect::Param {
            name: Symbol::from("label"),
            default: None,
            keyword: false,
            rest: false,
            forwarding: false,
            from_keyword: false,
            from_kwrest: false,
        }],
        body: build_by_label_body(&f.class, &f.records),
        signature: Some(fn_sig(
            vec![(Symbol::from("label"), Ty::Untyped)],
            class_ty.clone(),
        )),
        effects: EffectSet::default(),
        enclosing_class: Some(owner_id.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    });
    }

    // `_fixtures_load!` — class method that inserts every record into
    // the DB by `<Class>.new({...attrs...}).save`. Inserts happen in
    // 1-indexed file order so the autoincrement column matches the
    // ids the label methods look up. Body is a Seq of Sends.
    let load_body = build_load_method_body(&f.class, &f.records, &f.preamble, &f.file_blobs, all, defaults);
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_fixtures_load!"),
        receiver: MethodReceiver::Class,
        params: Vec::new(),
        body: load_body,
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner_id.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    });

    LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: owner_id,
        is_module: true,
        parent: None,
        parent_span: Default::default(),
        includes: Vec::new(),
        methods,
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: Vec::new(),
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
    }
}

/// Body of `by_label(label)`: an if/elsif chain comparing
/// `label.to_s` against each known label, answering that label's
/// record, and raising on a name no fixture defines.
///
/// `.to_s` on the way in so a Symbol and a String both hit — Rails'
/// accessor takes `users(:david)` and `users("david")` alike, and the
/// variable that reaches here has already lost which one it was.
fn build_by_label_body(cls: &ClassId, records: &[LoweredFixtureRecord]) -> Expr {
    let label_var = with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from("label") },
        ),
        Ty::Untyped,
    );
    let key = with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(label_var),
                method: Symbol::from("to_s"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        ),
        Ty::Str,
    );

    // Innermost else: no fixture by that name. Raising beats answering
    // a wrong record — `find(0)` would surface as a confusing
    // RecordNotFound three frames away from the typo.
    let mut chain = Expr::new(
        Span::synthetic(),
        ExprNode::Raise {
            value: lit_str(format!("no {} fixture named", cls.0.as_str())),
        },
    );

    for (idx, r) in records.iter().enumerate().rev() {
        let cond = with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(key.clone()),
                    method: Symbol::from("=="),
                    args: vec![lit_str(r.label.as_str().to_string())],
                    block: None,
                    parenthesized: false,
                },
            ),
            Ty::Bool,
        );
        chain = Expr::new(
            Span::synthetic(),
            ExprNode::If {
                cond,
                then_branch: build_find_call(cls, (idx + 1) as i64),
                else_branch: chain,
            },
        );
    }
    chain
}

/// `<Class>.find(<id>)` — used by each label method to return the
/// persisted record corresponding to that fixture row.
fn build_find_call(cls: &ClassId, id: i64) -> Expr {
    let class_const = with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Const { path: vec![cls.0.clone()] },
        ),
        Ty::Class { id: cls.clone(), args: vec![] },
    );
    with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(class_const),
                method: Symbol::from("find"),
                args: vec![lit_int(id)],
                block: None,
                parenthesized: true,
            },
        ),
        Ty::Class { id: cls.clone(), args: vec![] },
    )
}

/// Body of `_fixtures_load!`: a Seq of per-record blocks. Each
/// record lowers to
///   instance = <Class>.new
///   instance.id = <id>
///   instance.<field> = <value>
///   ...
///   instance.save
/// mirroring the shape `synth_from_params` uses. The per-field
/// assignment path avoids the `<Class>.new({attrs_hash})`
/// constructor pattern — strict targets (Crystal) can't reconcile
/// a `Hash[Symbol, Untyped]` arg's lookups (`attrs[:id]? || 0` is
/// `String | Int64`) against typed setters. Each typed setter
/// accepts its column type directly, so the value-type union never
/// surfaces.
fn build_load_method_body(
    cls: &ClassId,
    records: &[LoweredFixtureRecord],
    preamble: &[Expr],
    file_blobs: &indexmap::IndexMap<Symbol, crate::dialect::FixtureFileBlob>,
    all: &LoweredFixtureSet,
    defaults: &[&Column],
) -> Expr {
    let class_ty = Ty::Class { id: cls.clone(), args: vec![] };
    let instance_sym = Symbol::from("instance");

    let mut exprs: Vec<Expr> = Vec::new();

    // ERB statement tags first, in source order — they bind the locals
    // the value tags read (`password_digest`), so they have to run in
    // this body and ahead of every insert.
    exprs.extend(preamble.iter().cloned());

    for (idx, r) in records.iter().enumerate() {
        let id = (idx + 1) as i64;
        if let Some(blob) = file_blobs.get(&r.label) {
            exprs.push(load_file_blob(id, blob));
            continue;
        }
        // instance = <Class>.new
        let class_const = with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![cls.0.clone()] },
            ),
            class_ty.clone(),
        );
        let new_call = with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(class_const),
                    method: Symbol::from("new"),
                    args: vec![],
                    block: None,
                    parenthesized: true,
                },
            ),
            class_ty.clone(),
        );
        exprs.push(Expr::new(
            Span::synthetic(),
            ExprNode::Assign {
                target: crate::expr::LValue::Var {
                    id: crate::ident::VarId(0),
                    name: instance_sym.clone(),
                },
                value: new_call,
            },
        ));

        // instance.id = <id>
        let instance_var = with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Var {
                    id: crate::ident::VarId(0),
                    name: instance_sym.clone(),
                },
            ),
            class_ty.clone(),
        );
        // Omitted enum fields are schema row data, not the DSL defaults
        // ordinary `new` applies. Populate them without assignment validation.
        // Preserve the existing constructor-before-fixture-fields lifecycle:
        // after_initialize still sees model defaults, and fixture data (implicit
        // or explicit) wins over its writes. The insert below runs no callbacks.
        for col in defaults.iter().filter(|c| !r.fields.iter().any(|f| f.column == c.name)) {
            exprs.push(Expr::new(Span::synthetic(), ExprNode::Send {
                recv: Some(instance_var.clone()),
                method: enum_storage_writer(col),
                args: vec![schema_storage_default(col)],
                block: None,
                parenthesized: false,
            }));
        }
        exprs.push(Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(instance_var.clone()),
                method: Symbol::from("id="),
                args: vec![lit_int(id)],
                block: None,
                parenthesized: false,
            },
        ));

        // instance.<field> = <value>
        for field in &r.fields {
            let value_expr = match &field.value {
                LoweredFixtureValue::Literal { ty, raw } => literal_value_to_expr(ty, raw),
                LoweredFixtureValue::FkLookup { target_fixture, target_label } => {
                    resolve_fk_id(target_fixture, target_label, all)
                }
                LoweredFixtureValue::Ruby(expr) => expr.clone(),
            };
            let recv = with_ty(
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Var {
                        id: crate::ident::VarId(0),
                        name: instance_sym.clone(),
                    },
                ),
                class_ty.clone(),
            );
            exprs.push(Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(recv),
                    method: Symbol::from(format!("{}=", field.column.as_str())),
                    args: vec![value_expr],
                    block: None,
                    parenthesized: false,
                },
            ));
        }

        // instance.save
        let save_recv = with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Var {
                    id: crate::ident::VarId(0),
                    name: instance_sym.clone(),
                },
            ),
            class_ty.clone(),
        );
        exprs.push(with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(save_recv),
                    // NOT `save`: Rails loads fixtures with raw SQL
                    // (`insert_fixtures_set`), so a fixture row runs NO
                    // validations and NO callbacks. Saving a model ran
                    // both: validations silently dropped every row whose
                    // model checked something the fixture did not
                    // satisfy (`Room`'s creator took all seven room
                    // fixtures with it, see `lower::fixtures`) and walked
                    // campfire's push-subscription fixtures into a live
                    // DNS lookup; callbacks did what the app does on a
                    // real create — Message's `after_create_commit`
                    // marked memberships unread, so the fixture users
                    // started with unread rooms.
                    //
                    // `_insert_row` (runtime/ruby/active_record/
                    // base.rb) fills omitted timestamps and inserts.
                    // Every target's AR base is transpiled from that
                    // file, so every target has it.
                    //
                    // Safe because a fixture assigns only scalar
                    // columns — never an association object — so there
                    // is no autosave to miss.
                    method: Symbol::from("_insert_row"),
                    args: vec![],
                    block: None,
                    // Explicit parens so per-target emit doesn't drop
                    // them (the lowerer doesn't re-run the body-typer
                    // on this synth, so the auto-parens rule for Method
                    // accessors doesn't fire).
                    parenthesized: true,
                },
            ),
            Ty::Bool,
        ));
    }
    Expr::new(Span::synthetic(), ExprNode::Seq { exprs })
}

/// `ActiveStorage::FixtureSet.load_blob!(<id>, "reading.webp", "test", "")`
/// for a row Rails renders with `ActiveStorage::FixtureSet.blob`. The
/// runtime half (the ruby family's disk service) reads the file under
/// `test/fixtures/files`, uploads it and inserts the row under `id`;
/// an empty string is an argument the call did not pass.
fn load_file_blob(id: i64, blob: &crate::dialect::FixtureFileBlob) -> Expr {
    let recv = with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Const { path: vec![Symbol::from("ActiveStorage"), Symbol::from("FixtureSet")] },
        ),
        Ty::Class { id: ClassId(Symbol::from("ActiveStorage::FixtureSet")), args: vec![] },
    );
    with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(recv),
                method: Symbol::from("load_blob!"),
                args: vec![
                    lit_int(id),
                    lit_str(blob.filename.clone()),
                    lit_str(blob.service_name.clone().unwrap_or_default()),
                    lit_str(blob.content_type.clone().unwrap_or_default()),
                ],
                block: None,
                parenthesized: true,
            },
        ),
        Ty::Nil,
    )
}

/// YAML-string values come through as raw strings; cast to the
/// column's typed literal. Number-shaped raws to Int/Float; "true"/
/// "false" to Bool; everything else to Str.
fn literal_value_to_expr(ty: &Ty, raw: &str) -> Expr {
    match ty {
        Ty::Int => raw
            .parse::<i64>()
            .map(lit_int)
            .unwrap_or_else(|_| lit_str(raw.to_string())),
        Ty::Float => raw
            .parse::<f64>()
            .map(|v| with_ty(
                Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Float { value: v } }),
                Ty::Float,
            ))
            .unwrap_or_else(|_| lit_str(raw.to_string())),
        Ty::Bool => match raw {
            "true" => with_ty(
                Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Bool { value: true } }),
                Ty::Bool,
            ),
            "false" => with_ty(
                Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Bool { value: false } }),
                Ty::Bool,
            ),
            _ => lit_str(raw.to_string()),
        },
        // Str / Sym / Time / everything else: render as String literal
        // (Time columns get ISO strings from YAML; the model's accessor
        // typing presents them as String anyway).
        _ => lit_str(raw.to_string()),
    }
}

/// FK resolution: find the target fixture's record by label,
/// substitute its 1-indexed position as the literal Int id.
fn resolve_fk_id(
    target_fixture: &Symbol,
    target_label: &Symbol,
    all: &LoweredFixtureSet,
) -> Expr {
    if let Some(target) = all.fixtures.iter().find(|f| &f.name == target_fixture) {
        if let Some((idx, _)) = target
            .records
            .iter()
            .enumerate()
            .find(|(_, r)| &r.label == target_label)
        {
            return lit_int((idx + 1) as i64);
        }
    }
    // Fallback: missing reference — emit 0 so the IR is still typed.
    // The runtime will likely fail when looking up id=0, surfacing
    // the broken FK at test time rather than emit time.
    lit_int(0)
}

/// Walk each test method body and rewrite `<fixture_name>(:label)`
/// bare-Sends to `<Plural>Fixtures.<label>()` Const-Sends. Lets the
/// body-typer dispatch through the fixture class registry without a
/// runtime fixture-lookup helper.
///
/// Called by `test_module_to_library` on each method body before
/// typing. Takes the App's fixtures slice (uses fixture names as
/// the key set) plus an optional model lookup for the class name.
///
/// Walks with the shared `map_expr`, not a copy: this file carried its
/// own walker, which never learned `MultiAssign`, so `message, signature
/// = rooms(:pets).attachable_sgid.split("--")` kept its bare fixture
/// call — `undefined method 'rooms'` on the ruby lane, a compile error
/// on spinel — while the plain assignment beside it was rewritten.
///
/// The rewritten reader keeps the argument SLOT's keyword marks
/// (`lower::keyword_marks`). A test class's own keyword helper — one it
/// defines, or one spliced in from an included helper module — has its
/// calls respelled to marked positionals by `helper_kwargs` before this
/// runs, so `build_cart_transforms(shop: @shop, api_client:
/// api_clients(:translation_app))` reaches here with the fixture call
/// marked `api_client`. A fresh `Expr` drops that mark, and the ruby
/// family wrote `build_cart_transforms(shop: @shop,
/// ApiClientsFixtures.translation_app)`, which does not parse (Core's
/// `cart_transforms_test.rb`, unit-1000).
pub fn rewrite_fixture_calls(body: &Expr, fixture_names: &[Symbol]) -> Expr {
    map_expr(body, &|e| {
        let mut rewritten = rewrite_fixture_call(e, fixture_names)?;
        crate::lower::keyword_marks::SlotMarks::of(e).restore(&mut rewritten);
        Some(rewritten)
    })
}

fn rewrite_fixture_call(e: &Expr, fixture_names: &[Symbol]) -> Option<Expr> {
    let ExprNode::Send {
        recv: None,
        method,
        args,
        block: None,
        ..
    } = &*e.node
    else {
        return None;
    };
    if !fixture_names.iter().any(|f| f == method) {
        return None;
    }
    if args.is_empty() {
        return None;
    }
    let owner = format!("{}Fixtures", camelize(method.as_str()));
    let owner_id = ClassId(Symbol::from(owner.clone()));
    let class_const = || {
        with_ty(
            Expr::new(
                e.span,
                ExprNode::Const { path: vec![Symbol::from(owner.as_str())] },
            ),
            Ty::Class { id: owner_id.clone(), args: vec![] },
        )
    };
    // One argument → one record.
    //
    // `users(:david)` — the label is known here, so bind straight to
    // the generated reader. Concrete dispatch, no table lookup.
    // `users(name)` — the label is a value. Rails allows it and
    // campfire's `SessionTestHelper#sign_in` writes it, so route it
    // through the generated `by_label` table rather than leaving a
    // bare `users(...)` that resolves to nothing.
    let one = |arg: &Expr| -> Expr {
        if let ExprNode::Lit { value: Literal::Sym { value: label } } = &*arg.node {
            return Expr::new(
                e.span,
                ExprNode::Send {
                    recv: Some(class_const()),
                    method: label.clone(),
                    args: vec![],
                    block: None,
                    parenthesized: true,
                },
            );
        }
        Expr::new(
            e.span,
            ExprNode::Send {
                recv: Some(class_const()),
                method: Symbol::from("by_label"),
                args: vec![arg.clone()],
                block: None,
                parenthesized: true,
            },
        )
    };
    if args.len() == 1 {
        return Some(one(&args[0]));
    }
    // Rails' accessor is `def users(*names)`, and with more than one
    // name it answers the ARRAY of those records — campfire's
    // account page test reads `users(:david, :jason).map(&:name)`.
    // Left unrewritten this stayed a bare `users(:david, :jason)`
    // that resolved to nothing, which is the same silent hole the
    // value-label case above was opened to close, one arity over.
    //
    // An Array LITERAL of the per-label readers, not a call to a
    // variadic helper: each element is the same concrete dispatch
    // the single-argument form emits, so the array's element type
    // follows from its members and no target needs a splat.
    Some(Expr::new(
        e.span,
        ExprNode::Array {
            elements: args.iter().map(one).collect(),
            style: crate::expr::ArrayStyle::Brackets,
        },
    ))
}

// ---------------------------------------------------------------------------
// Raw-row fixture modules: the CRuby PostgreSQL tree.

/// The CRuby PostgreSQL tree's fixture modules. Each loads its set the
/// way Rails does, as table rows: `_fixtures_load!` hands the set's
/// column → value rows to `ActiveRecord::TestFixtures.insert_fixture_rows`,
/// one multi-row INSERT cast per column, so no model is instantiated and
/// no writer, callback or validation runs (`lower::fixtures::lower_fixture_rows`
/// resolves the rows). Each label accessor finds its inserted row (`TestFixtures.fixture_find`),
/// memoized until the next load, as Rails' fixture cache does. The other
/// targets keep `lower_fixtures_to_library_classes`.
pub fn lower_fixtures_to_raw_row_library_classes(app: &App) -> Vec<LibraryClass> {
    let dynamic = fixtures_reached_by_variable(app);
    crate::lower::fixtures::lower_fixture_rows(app)
        .iter()
        .map(|set| build_raw_row_fixture_class(set, dynamic.contains(&set.name)))
        .collect()
}

fn class_method(owner: &ClassId, name: Symbol, params: Vec<crate::dialect::Param>, body: Expr, signature: Ty) -> MethodDef {
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name,
        receiver: MethodReceiver::Class,
        params,
        body,
        signature: Some(signature),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

fn build_raw_row_fixture_class(set: &crate::lower::fixtures::RawFixtureSet, wants_by_label: bool) -> LibraryClass {
    let owner_name = format!("{}Fixtures", camelize(set.name.as_str()));
    let owner_id = ClassId(Symbol::from(owner_name));
    let class_ty = Ty::Class { id: set.class.clone(), args: vec![] };

    // Rails' `Fixture#find` finds by the model's key columns of the row
    // the set inserted (`fixture.slice(*primary_key)`), so each accessor
    // reads back its row as `_fixtures_load!` inserted it: an ERB key, a
    // generated id and a normalized uuid are the values the INSERT used.
    let finds: Vec<Expr> = (0..set.rows.len())
        .map(|index| {
            let rows = with_ty(
                Expr::new(Span::synthetic(), ExprNode::Ivar { name: Symbol::from(FIXTURE_ROWS_IVAR) }),
                Ty::Array { elem: Box::new(Ty::Untyped) },
            );
            send(
                Some(test_fixtures_const()),
                "fixture_find",
                vec![class_const(&set.class), rows, lit_int(index as i64)],
                class_ty.clone(),
            )
        })
        .collect();

    let mut methods: Vec<MethodDef> = set
        .rows
        .iter()
        .zip(&finds)
        .map(|(row, find)| {
            class_method(
                &owner_id,
                row.label.clone(),
                Vec::new(),
                memoized_lookup(&row.label, find),
                fn_sig(vec![], class_ty.clone()),
            )
        })
        .collect();

    if wants_by_label {
        let label_var = with_ty(
            Expr::new(Span::synthetic(), ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from("label") }),
            Ty::Untyped,
        );
        let key = send(Some(label_var), "to_s", vec![], Ty::Str);
        let mut chain = Expr::new(
            Span::synthetic(),
            ExprNode::Raise { value: lit_str(format!("no {} fixture named", set.class.0.as_str())) },
        );
        for (row, find) in set.rows.iter().zip(&finds).rev() {
            let cond = send(Some(key.clone()), "==", vec![lit_str(row.label.as_str().to_string())], Ty::Bool);
            chain = Expr::new(
                Span::synthetic(),
                ExprNode::If { cond, then_branch: find.clone(), else_branch: chain },
            );
        }
        methods.push(class_method(
            &owner_id,
            Symbol::from("by_label"),
            vec![crate::dialect::Param {
                name: Symbol::from("label"),
                default: None,
                keyword: false,
                rest: false,
                forwarding: false,
                from_keyword: false,
                from_kwrest: false,
            }],
            chain,
            fn_sig(vec![(Symbol::from("label"), Ty::Untyped)], class_ty.clone()),
        ));
    }

    methods.push(class_method(
        &owner_id,
        Symbol::from("_fixtures_load!"),
        Vec::new(),
        build_raw_row_load_body(set),
        fn_sig(vec![], Ty::Nil),
    ));

    LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: owner_id,
        is_module: true,
        parent: None,
        parent_span: Default::default(),
        includes: Vec::new(),
        methods,
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: Vec::new(),
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
    }
}

/// The module's inserted rows, which its accessors find by. Not an
/// accessor cache slot (`__fx_` / `__fxe_`): it lives until the next load.
const FIXTURE_ROWS_IVAR: &str = "__fixture_rows";

/// `_fixtures_load!`: clear the accessors' cache, run the file's ERB
/// statements, then insert the rows, keeping them as inserted (or raise
/// the set's refusal).
fn build_raw_row_load_body(set: &crate::lower::fixtures::RawFixtureSet) -> Expr {
    let mut exprs: Vec<Expr> = set
        .rows
        .iter()
        .map(|row| {
            Expr::new(
                Span::synthetic(),
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: fixture_cache_ivar(&row.label) },
                    value: crate::lower::typing::nil_lit(),
                },
            )
        })
        .collect();
    exprs.extend(set.preamble.iter().cloned());
    match (&set.columns, set.errors.first()) {
        (Some(columns), None) => {
            let columns_hash = hash_lit(
                columns
                    .iter()
                    .map(|(name, ty)| (lit_str(name.as_str().to_string()), lit_str((*ty).to_string())))
                    .collect(),
            );
            let timestamps = with_ty(
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Array {
                        elements: set.timestamps.iter().map(|c| lit_str(c.as_str().to_string())).collect(),
                        style: crate::expr::ArrayStyle::Brackets,
                    },
                ),
                Ty::Array { elem: Box::new(Ty::Str) },
            );
            let rows = with_ty(
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Array {
                        elements: set
                            .rows
                            .iter()
                            .map(|row| {
                                hash_lit(
                                    row.values
                                        .iter()
                                        .map(|(k, v)| (lit_str(k.as_str().to_string()), raw_value_expr(v)))
                                        .collect(),
                                )
                            })
                            .collect(),
                        style: crate::expr::ArrayStyle::Brackets,
                    },
                ),
                Ty::Array { elem: Box::new(Ty::Untyped) },
            );
            let mut args = vec![lit_str(set.table.as_str().to_string()), columns_hash, timestamps, rows];
            if set.rules == crate::lower::fixtures::FixtureRules::FasterFixtures {
                // `model:` is the set's model class name, which
                // FasterFixtures' `normalize_fixture_value` hook takes for
                // an Integer in a uuid column.
                args.push(Expr::new(
                    Span::synthetic(),
                    ExprNode::Hash {
                        entries: vec![
                            (
                                crate::lower::typing::lit_sym(Symbol::from("faster_fixtures")),
                                crate::lower::typing::lit_bool(true),
                            ),
                            (
                                crate::lower::typing::lit_sym(Symbol::from("model")),
                                lit_str(set.class.0.as_str().trim_start_matches("::").to_string()),
                            ),
                        ],
                        kwargs: true,
                    },
                ));
            }
            exprs.push(Expr::new(
                Span::synthetic(),
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: Symbol::from(FIXTURE_ROWS_IVAR) },
                    value: send(
                        Some(test_fixtures_const()),
                        "insert_fixture_rows",
                        args,
                        Ty::Array { elem: Box::new(Ty::Untyped) },
                    ),
                },
            ));
        }
        (_, error) => {
            let message = error.cloned().unwrap_or_else(|| {
                format!("roundhouse: fixture set `{}`: table `{}` is not in this tree's schema", set.path, set.table)
            });
            exprs.push(Expr::new(Span::synthetic(), ExprNode::Raise { value: lit_str(message) }));
        }
    }
    exprs.push(crate::lower::typing::nil_lit());
    Expr::new(Span::synthetic(), ExprNode::Seq { exprs })
}

fn test_fixtures_const() -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Const { path: vec![Symbol::from("ActiveRecord"), Symbol::from("TestFixtures")] },
    )
}

fn class_const(cls: &ClassId) -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Const { path: vec![cls.0.clone()] }),
        Ty::Class { id: cls.clone(), args: vec![] },
    )
}

fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>, ty: Ty) -> Expr {
    with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized: true },
        ),
        ty,
    )
}

fn hash_lit(entries: Vec<(Expr, Expr)>) -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Hash { entries, kwargs: false }),
        Ty::Hash { key: Box::new(Ty::Str), value: Box::new(Ty::Untyped) },
    )
}

/// A row value as Ruby: the literal Psych loaded, or the ERB expression.
fn raw_value_expr(v: &crate::lower::fixtures::RawFixtureValue) -> Expr {
    use crate::lower::fixtures::RawFixtureValue as V;
    match v {
        V::Nil => crate::lower::typing::nil_lit(),
        V::Bool(b) => crate::lower::typing::lit_bool(*b),
        V::Int(i) => lit_int(*i),
        V::BigInt(digits) => with_ty(
            Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::BigInt { value: digits.clone() } }),
            Ty::Int,
        ),
        V::Float(f) => crate::lower::typing::lit_float(*f),
        V::Str(s) => lit_str(s.clone()),
        V::Structured(value) => yaml_value_expr(value),
        V::Ruby(expr) => expr.clone(),
        V::RubyReference { value, uuid, integer_passes, identifier } => {
            let mut args = vec![
                value.clone(),
                lit_str(if *uuid { "uuid" } else { "integer" }.to_string()),
                crate::lower::typing::lit_bool(*integer_passes),
            ];
            if let Some((model, column)) = identifier {
                args.push(lit_str(model.0.as_str().trim_start_matches("::").to_string()));
                args.push(lit_str(column.as_str().to_string()));
            }
            send(Some(test_fixtures_const()), "fixture_reference", args, Ty::Untyped)
        }
        V::Identifier { model, column, label } => send(
            Some(test_fixtures_const()),
            "fixture_identifier",
            vec![
                lit_str(model.0.as_str().trim_start_matches("::").to_string()),
                lit_str(column.as_str().to_string()),
                lit_str(label.clone()),
            ],
            Ty::Untyped,
        ),
    }
}

fn yaml_value_expr(v: &serde_yaml_ng::Value) -> Expr {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::Null => crate::lower::typing::nil_lit(),
        Y::Bool(b) => crate::lower::typing::lit_bool(*b),
        Y::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => lit_int(i),
            (None, Some(f)) if n.is_f64() && f.is_finite() => crate::lower::typing::lit_float(f),
            _ => with_ty(
                Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::BigInt { value: n.to_string() } }),
                Ty::Int,
            ),
        },
        Y::String(s) => lit_str(s.clone()),
        Y::Sequence(items) => with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Array {
                    elements: items.iter().map(yaml_value_expr).collect(),
                    style: crate::expr::ArrayStyle::Brackets,
                },
            ),
            Ty::Array { elem: Box::new(Ty::Untyped) },
        ),
        Y::Mapping(map) => hash_lit(map.iter().map(|(k, v)| (yaml_value_expr(k), yaml_value_expr(v))).collect()),
        Y::Tagged(t) => yaml_value_expr(&t.value),
    }
}

/// `@__fx_<label> = <find> if @__fx_<label>.nil?; @__fx_<label>`: the
/// shape `emit::ruby::library::apply_fixture_memoization` gives an
/// integer-keyed accessor, for every key (a uuid, a composite key).
fn memoized_lookup(label: &Symbol, lookup: &Expr) -> Expr {
    let slot = fixture_cache_ivar(label);
    let read = || {
        let mut e = Expr::new(Span::synthetic(), ExprNode::Ivar { name: slot.clone() });
        e.ty = lookup.ty.clone();
        e
    };
    let fill = Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: send(Some(read()), "nil?", vec![], Ty::Bool),
            then_branch: Expr::new(
                Span::synthetic(),
                ExprNode::Assign { target: crate::expr::LValue::Ivar { name: slot.clone() }, value: lookup.clone() },
            ),
            else_branch: crate::lower::typing::nil_lit(),
        },
    );
    let mut seq = Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![fill, read()] });
    seq.ty = lookup.ty.clone();
    seq
}

/// The accessor's cache slot, the same name
/// `emit::ruby::library::fixture_cache_ivar` gives it (the runtime's
/// `reset_fixture_accessors` clears both prefixes): `__fx_<label>`, or
/// for a label that is not all `[A-Za-z0-9_]`, `__fxe_` and an escape.
fn fixture_cache_ivar(label: &Symbol) -> Symbol {
    let label = label.as_str();
    if label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Symbol::from(format!("__fx_{label}"));
    }
    let mut slot = String::from("__fxe_");
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            slot.push(c);
        } else if c == '_' {
            slot.push_str("__");
        } else {
            slot.push_str(&format!("_{:x}_", c as u32));
        }
    }
    Symbol::from(slot)
}
