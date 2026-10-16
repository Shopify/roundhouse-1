//! Per-model adapter primitives — Level-3 emit, built atop Arel IR.
//!
//! For each model with a known schema, synthesize per-model class methods
//! that go directly from SQL composition to typed model instances. Each
//! method's body is built by:
//!
//!   1. Constructing an `ArelOp` describing the operation.
//!   2. Calling the owning database dialect's visitor to get the
//!      target-runtime `Expr` over the `Db` primitive surface.
//!   3. Wrapping in a `MethodDef` with the appropriate name + signature.
//!
//! The `Db` primitive surface (configure / prepare / step? / column_int /
//! column_text / finalize / exec / escape_*) is the runtime contract these
//! primitives sit on top of; the public AR API in
//! `runtime/ruby/active_record/base.rb` delegates to these primitives.
//! `Db` is backend-agnostic — sibling shims (cruby/sqlite-gem,
//! spinel-FFI/sqlite, postgres/etc.) implement the same module name;
//! per-database SQL dialect differences live in the visitor's per-backend
//! impl.
//!
//! Underscore-prefix on emitted names signals "framework-internal, not
//! user-facing API." See project_level_3_adapter_emit.md and
//! project_arel_compile_time_first.md.
//!
//! Methods emitted (uniform per-model shape — no app-scan needed):
//!   `_adapter_find_by_id(id)` — find by primary key, returns `<Owner>?`
//!   `_adapter_all` — full table scan, returns `Array[<Owner>]`
//!   `_adapter_insert(instance)` — INSERT, returns last_insert_rowid
//!   `_adapter_update(id, instance)` — UPDATE WHERE id, returns void
//!   `_adapter_delete(id)` — DELETE WHERE id, returns void
//!   `_adapter_count` — SELECT COUNT(*), returns Integer
//!   `_adapter_any?` — SELECT 1 LIMIT 1 (table non-empty), returns Bool
//!   `_adapter_exists_by_id?(id)` — SELECT 1 LIMIT 1, returns Bool
//!   `_adapter_truncate` — DELETE FROM table (test setup)
//!   `_columns_sql` — the schema columns as a qualified SELECT list
//!   `_hydrate_all(sql)` — run a SELECT over `_columns_sql`, typed rows

use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, Model, Param};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol, TableRef, VarId};
use crate::lower::arel::{
    ArelOp, ArelVisitor, Assignment, ColRef, ColumnSpec, Delete, Direction, Insert, LimitSpec,
    Order, Predicate, Select, SqlDialect, Update, Value, ValueType,
};
use crate::schema::{Schema, Table};
use crate::span::Span;
use crate::ty::Ty;

use super::{fn_sig, ty_of_column};

mod exists;
use exists::{synth_adapter_any, synth_adapter_exists_by_id};

pub(super) fn push_adapter_methods(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    table: &Table,
    schema: &Schema,
    dialect: SqlDialect,
) {
    let owner = &model.name;
    methods.push(synth_adapter_find_by_id(model, table, schema, dialect));
    methods.push(synth_adapter_all(owner, table, schema, dialect));
    methods.push(synth_adapter_last(model, table, schema, dialect));
    methods.push(synth_adapter_insert(model, table, schema, dialect));
    methods.push(synth_adapter_update(model, table, schema, dialect));
    methods.push(synth_adapter_touch(model, table, schema, dialect));
    methods.push(synth_adapter_delete(model, table, schema, dialect));
    methods.push(synth_adapter_count(owner, table, schema, dialect));
    methods.push(synth_adapter_any(owner, table, schema, dialect));
    methods.push(synth_adapter_exists_by_id(model, table, schema, dialect));
    methods.push(synth_adapter_truncate(owner, table, schema, dialect));
    methods.push(synth_delete_all(owner, table, dialect));
    methods.push(synth_adapter_reload(model, table, dialect));
    methods.push(synth_columns_sql(owner, table, dialect));
    methods.push(synth_hydrate_all(owner));
    if !model.query_constraints.is_empty() {
        methods.push(synth_compiled_query_constraints(model));
    }
    if !model.ignored_columns.is_empty() {
        methods.push(synth_compiled_ignored_columns(model));
    }
}

// ---------------------------------------------------------------------------
// Each synth function builds an ArelOp for its shape, calls the visitor,
// and wraps in a MethodDef. The visitor produces the same Expr today's
// hand-written synth functions produced. See arel/visitor.rs for the
// per-shape emit (single hydrate / multi hydrate / count / exists /
// insert / update / delete).
// ---------------------------------------------------------------------------

/// Select shared normalization using the schema before the scalar adapter.
/// This is dispatch only: all prefix/range rules stay in IntegerKeyCast.
/// A runtime branch would compile both incompatible scalar adapter calls.
pub(super) fn synth_find_primary_key_input(model: &Model, table: &Table) -> MethodDef {
    let owner = &model.name;
    synth_primary_key_input(
        model,
        table,
        "_find_primary_key_input",
        "_adapter_find_by_id",
        Ty::Union {
            variants: vec![
                Ty::Class { id: owner.clone(), args: vec![].into() },
                Ty::Nil,
            ].into(),
        },
        super::nil_lit(),
    )
}

/// Same schema-selected dispatch as find, but invalid keys answer false
/// (Rails `exists?`) instead of nil / RecordNotFound.
pub(super) fn synth_exists_primary_key_input(model: &Model, table: &Table) -> MethodDef {
    let false_lit = Expr::new(
        Span::synthetic(),
        ExprNode::Lit { value: Literal::Bool { value: false } },
    );
    synth_primary_key_input(
        model,
        table,
        "_exists_primary_key_input",
        "_adapter_exists_by_id?",
        Ty::Bool,
        false_lit,
    )
}

fn synth_primary_key_input(
    model: &Model,
    table: &Table,
    name: &str,
    adapter: &str,
    ret: Ty,
    invalid: Expr,
) -> MethodDef {
    let owner = &model.name;
    let id = Symbol::from("id");
    let key = key_ty(model, table);
    let caster = ClassId(Symbol::from("ActiveRecord::IntegerKeyCast"));
    let adapter_call = |value| Expr::new(Span::synthetic(), ExprNode::Send {
        recv: None, method: Symbol::from(adapter),
        args: vec![value], block: None, parenthesized: true,
    });
    let caster_call = |method: &str| Expr::new(Span::synthetic(), ExprNode::Send {
        recv: Some(super::class_const(&caster)), method: Symbol::from(method),
        args: vec![var_ref(&id)], block: None, parenthesized: true,
    });
    let body = if key == Ty::Str {
        adapter_call(caster_call("input_text"))
    } else if key != Ty::Int {
        // Other schema keys retain their existing adapter conversion. For
        // example, a Float key must not normalize "1.5" into integer row 1.
        adapter_call(var_ref(&id))
    } else {
        let cast = Symbol::from("cast");
        let read = |method: &str| Expr::new(Span::synthetic(), ExprNode::Send {
            recv: Some(var_ref(&cast)), method: Symbol::from(method),
            args: vec![], block: None, parenthesized: true,
        });
        super::seq(vec![
            arel_assign(&cast, caster_call("parse")),
            Expr::new(Span::synthetic(), ExprNode::If {
                cond: read("valid"), then_branch: adapter_call(read("value")),
                else_branch: invalid,
            }),
        ])
    };
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Class,
        params: vec![Param::positional(id.clone())],
        body,
        signature: Some(fn_sig(
            vec![(id, super::finder_input_ty(&key))],
            ret,
        )),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

fn synth_adapter_find_by_id(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    let id = Symbol::from("id");
    let key_ty = key_ty(model, table);
    let owner_ty = Ty::Class { id: owner.clone(), args: vec![].into() };
    let nilable_owner = Ty::Union { variants: vec![owner_ty, Ty::Nil].into() };

    let op = ArelOp::Select(Select {
        single_record: true, // _adapter_find_by_id — one record or nil
        table: TableRef(table.name.clone()),
        columns: ColumnSpec::All,
        conditions: Some(eq_id_param(model, table, &id)),
        orders: vec![],
        limit: Some(LimitSpec(1)),
        joins: vec![],
            preloads: vec![],
    });

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_find_by_id"),
        receiver: MethodReceiver::Class,
        params: vec![Param::positional(id.clone())],
        body: dialect.visit(&op, schema, owner),
        signature: Some(fn_sig(vec![(id, key_ty)], nilable_owner)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

fn synth_adapter_all(owner: &ClassId, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner_ty = Ty::Class { id: owner.clone(), args: vec![].into() };

    let op = ArelOp::Select(Select {
        single_record: false, // _adapter_all — a collection
        table: TableRef(table.name.clone()),
        columns: ColumnSpec::All,
        conditions: None,
        orders: vec![],
        limit: None,
        joins: vec![],
            preloads: vec![],
    });

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_all"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body: dialect.visit(&op, schema, owner),
        signature: Some(fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(owner_ty) })),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def self._adapter_last` — `SELECT <cols> FROM <table> ORDER BY
/// <table>.id DESC LIMIT 1` (each query constraint column DESC once
/// `query_constraints` ran), a single nilable hydrate. Mirrors Rails'
/// `Model.last` (one row) instead of the old `_adapter_all().last()`
/// full-table scan; goes through the same `Db.prepare`/`from_stmt` path
/// as `_adapter_find_by_id`, so it works for every app (the raw
/// `ActiveRecord.adapter` is not wired under the Level-3 architecture).
fn synth_adapter_last(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    let owner_ty = Ty::Class { id: owner.clone(), args: vec![].into() };
    let nilable_owner = Ty::Union { variants: vec![owner_ty, Ty::Nil].into() };

    let last = |columns: Vec<Symbol>| {
        let op = ArelOp::Select(Select {
            single_record: true, // _adapter_last — one record or nil
            table: TableRef(table.name.clone()),
            columns: ColumnSpec::All,
            conditions: None,
            orders: columns
                .into_iter()
                .map(|column| Order { column: ColRef { table: TableRef(table.name.clone()), column }, direction: Direction::Desc })
                .collect(),
            limit: Some(LimitSpec(1)),
            joins: vec![],
            preloads: vec![],
        });
        dialect.visit(&op, schema, owner)
    };
    let single = last(vec![key_column_name(model, table)]);
    // Rails orders `last` by `query_constraints_list` once the call ran.
    let body = match query_constraint_columns(model, table) {
        Some(cols) => when_query_constraints_on(false, last(cols.iter().map(|c| c.name.clone()).collect()), single),
        None => single,
    };

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_last"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body: body,
        signature: Some(fn_sig(vec![], nilable_owner)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// `def _adapter_insert` — instance method; reads ivars to compose
/// the INSERT, returns last_insert_rowid. Instance-method (not
/// class-method) so save() reaches it via implicit-self dispatch
/// (`@id = _adapter_insert`) and the TS emitter places the libsql
/// `await` on the call result rather than the receiver. See the
/// reload comment for the underlying emit issue with
/// `self.class.<async_method>`.
fn synth_adapter_insert(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    use crate::schema::ColumnType;

    // An integer key is the database's to assign: it stays out of the
    // INSERT and comes back as `last_insert_rowid`. A non-integer key
    // (`t.uuid`, `id: :string`) is a value the record already holds —
    // set by the app, or minted just below — so it is written like any
    // other column and answered from the ivar; the rowid says nothing
    // about it (#90).
    //
    // A key the table does not flag, one column of a composite primary
    // key that `self.primary_key` names, has no generated identity to
    // return when it has no default: Rails writes the value the record
    // holds (Core's GenerateOid assigns it before create), and so does
    // this INSERT. A nil one fails the NOT NULL constraint, as in Rails.
    let key = key_column(model, table);
    let unflagged = super::primary_key_column(table).is_none();
    let supplied_key = key.filter(|c| {
        !matches!(c.col_type, ColumnType::Integer | ColumnType::BigInt) || (unflagged && c.default.is_none())
    });

    // SQL column names are the PUBLIC names; the value reads the STORAGE
    // ivar (`@col_raw` for temporal — stored ISO-8601 text goes to disk).
    let assignments: Vec<Assignment> = table
        .columns
        .iter()
        .filter(|c| c.generated.is_none() && (!c.primary_key || supplied_key.is_some()))
        .map(|c| Assignment {
            column: c.name.clone(),
            value: Value::Runtime {
                expr: column_bind_expr(model, c),
                ty: bind_type(model, c),
            },
        })
        .collect();

    let insert = Insert {
        table: TableRef(table.name.clone()),
        assignments,
        returns_rowid: supplied_key.is_none(),
    };
    // An integer key the record already holds is written, as Rails writes
    // an explicit `id:` (`Model.insert_all([{ id: 7, … }])`, `create(id: 7)`):
    // the database allocates only a key the record leaves nil. The
    // statement still answers the key (`RETURNING` / `last_insert_rowid`).
    let keyed = key.filter(|c| supplied_key.is_none() && c.primary_key && c.generated.is_none()).map(|k| {
        let assignments = table
            .columns
            .iter()
            .filter(|c| c.generated.is_none())
            .map(|c| Assignment {
                column: c.name.clone(),
                value: Value::Runtime { expr: column_bind_expr(model, c), ty: bind_type(model, c) },
            })
            .collect();
        (k, Insert { table: TableRef(table.name.clone()), assignments, returns_rowid: true })
    });
    let generated: Vec<&crate::schema::Column> =
        table.columns.iter().filter(|c| c.generated.is_some()).collect();
    let ret_ty = if supplied_key.is_some() { key_ty(model, table) } else { Ty::Int };
    // A uuid key left blank is minted here, as the `gen_random_uuid()`
    // default the Postgres schema declares would have on insert — SQLite
    // has no such default. A string key (`id: :string`) is the app's to
    // supply, as in Rails, where a NULL one fails the NOT NULL constraint.
    // A retypable key holds the declared type's value, which the app
    // assigns (Core's ids come from GenerateOid before create).
    let uuid_mint = supplied_key
        .filter(|k| matches!(k.col_type, ColumnType::Uuid) && !super::schema::is_retypable(model, k))
        .map(uuid_key_mint_expr);
    let build_insert = |insert: &Insert| -> Expr {
        if generated.is_empty() {
            // Keep ordinary inserts on their established exec/last-rowid path.
            let mut exprs = Vec::new();
            exprs.push(dialect.visit(&ArelOp::Insert(insert.clone()), schema, owner));
            if let Some(k) = supplied_key {
                exprs.push(ivar_ref(&k.name));
            }
            if exprs.len() == 1 {
                exprs.pop().expect("one ordinary insert expression")
            } else {
                Expr::new(Span::synthetic(), ExprNode::Seq { exprs })
            }
        } else {
            let returning_key = key_column(model, table)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| Symbol::from("rowid"));
            let returning_columns: Vec<Symbol> =
                generated.iter().map(|c| c.name.clone()).collect();
            let insert = dialect.visit_insert_returning(
                insert,
                schema,
                &returning_key,
                key_is_integer(model, table),
                &returning_columns,
            );
            hydrate_generated_after_insert(model, table, &generated, insert, &ret_ty)
        }
    };
    let mut body = match &keyed {
        Some((k, keyed_insert)) => {
            // A new record's key slot is nil or the runtime's `0`.
            let send = |method: &str, args: Vec<Expr>| {
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Send {
                        recv: Some(ivar_ref(&k.name)),
                        method: Symbol::from(method),
                        args,
                        block: None,
                        parenthesized: false,
                    },
                )
            };
            let unset = Expr::new(
                Span::synthetic(),
                ExprNode::BoolOp {
                    op: crate::expr::BoolOpKind::Or,
                    surface: Default::default(),
                    left: send("nil?", vec![]),
                    right: send("==", vec![arel_lit_int(0)]),
                },
            );
            Expr::new(
                Span::synthetic(),
                ExprNode::If { cond: unset, then_branch: build_insert(&insert), else_branch: build_insert(keyed_insert) },
            )
        }
        None => build_insert(&insert),
    };
    if let Some(mint) = uuid_mint {
        body = if generated.is_empty() {
            let mut exprs = vec![mint];
            match *body.node {
                ExprNode::Seq { exprs: rest } => exprs.extend(rest),
                _ => exprs.push(body),
            }
            Expr::new(Span::synthetic(), ExprNode::Seq { exprs })
        } else {
            Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![mint, body] })
        };
    }

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_insert"),
        receiver: MethodReceiver::Instance,
        params: vec![],
        body,
        signature: Some(fn_sig(vec![], ret_ty)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// Mint a UUID primary key before insertion when its slot is blank. SQLite
/// has no `gen_random_uuid()` default, so this mirrors the runtime's existing
/// client-side UUID behavior for both ordinary and generated-column inserts.
fn uuid_key_mint_expr(key: &crate::schema::Column) -> Expr {
    use crate::expr::LValue;

    let blank = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(ivar_ref(&key.name)),
            method: Symbol::from("=="),
            args: vec![arel_lit_str(String::new())],
            block: None,
            parenthesized: false,
        },
    );
    let mint = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![Symbol::from("SecureRandom")] },
            )),
            method: Symbol::from("uuid"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: blank,
            then_branch: Expr::new(
                Span::synthetic(),
                ExprNode::Assign { target: LValue::Ivar { name: key.name.clone() }, value: mint },
            ),
            else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        },
    )
}

/// After the INSERT has produced its key, read the database-owned generated
/// values from that statement and fill only storage slots still holding nil.
/// Reading the key and generated values from one `RETURNING` result avoids a
/// second database operation before save records the new identity. Explicit
/// non-nil assignments remain visible through create callbacks; explicit nil
/// is treated as unset, matching Rails' generated-attribute behavior.
fn hydrate_generated_after_insert(
    model: &Model,
    table: &Table,
    generated: &[&crate::schema::Column],
    insert_returning: Expr,
    key_ty: &Ty,
) -> Expr {
    let db = ClassId(Symbol::from("Db"));
    let inserted_key = Symbol::from("__rh_inserted_key");
    let stmt = Symbol::from("__rh_generated_stmt");
    let staged_key = Symbol::from("__rh_returning_key");
    let staged_values: Vec<Symbol> = generated
        .iter()
        .enumerate()
        .map(|(index, _)| Symbol::from(format!("__rh_generated_value_{index}")))
        .collect();

    let key_read = if key_is_integer(model, table) {
        let mut parsed = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(arel_db_call(
                    &db,
                    "column_text",
                    vec![var_ref(&stmt), arel_lit_int(0)],
                )),
                method: Symbol::from("to_i"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        );
        parsed.ty = Some(Ty::Int);
        parsed
    } else {
        let key_read_method = key_column(model, table)
            .map(super::schema::column_read_method_for)
            .unwrap_or("column_int");
        arel_db_call(&db, key_read_method, vec![var_ref(&stmt), arel_lit_int(0)])
    };
    let mut reads = vec![arel_assign(
        &staged_key,
        crate::lower::typing::with_ty(key_read, key_ty.clone()),
    )];
    for (index, (col, staged)) in generated.iter().zip(&staged_values).enumerate() {
        let storage = super::schema::col_storage_name(col);
        let mut is_nil = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(ivar_ref(&storage)),
                method: Symbol::from("nil?"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        );
        is_nil.ty = Some(Ty::Bool);
        let value = crate::lower::typing::with_ty(
            arel_db_call(
                &db,
                super::schema::column_read_method_for(col),
                vec![var_ref(&stmt), arel_lit_int((index + 1) as i64)],
            ),
            super::ty_of_column_slot(col),
        );
        // Read values into locals first. No generated slot is changed until
        // every required read and statement finalization has succeeded.
        reads.push(Expr::new(
            Span::synthetic(),
            ExprNode::If {
                cond: is_nil,
                then_branch: arel_assign(staged, value),
                else_branch: arel_assign(staged, ivar_ref(&storage)),
            },
        ));
    }
    reads.push(crate::lower::typing::with_ty(var_ref(&staged_key), key_ty.clone()));

    let mut step = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![Symbol::from("Db")] },
            )),
            method: Symbol::from("step?"),
            args: vec![var_ref(&stmt)],
            block: None,
            parenthesized: true,
        },
    );
    step.ty = Some(Ty::Bool);
    let mut missing_row = Expr::new(
        Span::synthetic(),
        ExprNode::Raise { value: arel_lit_str(format!(
            "INSERT ... RETURNING produced no row for `{}`",
            table.name.as_str(),
        )) },
    );
    missing_row.ty = Some(Ty::Bottom);
    let mut read_row = Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: step,
            then_branch: Expr::new(Span::synthetic(), ExprNode::Seq { exprs: reads }),
            else_branch: missing_row,
        },
    );
    read_row.ty = Some(key_ty.clone());
    let mut finalized_read = Expr::new(
        Span::synthetic(),
        ExprNode::BeginRescue {
            body: read_row,
            rescues: vec![],
            else_branch: None,
            ensure: Some(arel_db_call(&db, "finalize", vec![var_ref(&stmt)])),
            implicit: false,
        },
    );
    finalized_read.ty = Some(key_ty.clone());

    let mut stmts = vec![
        arel_assign(&stmt, insert_returning),
        arel_assign(&inserted_key, finalized_read),
    ];
    for (col, staged) in generated.iter().zip(&staged_values) {
        let storage = super::schema::col_storage_name(col);
        let mut is_nil = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(ivar_ref(&storage)),
                method: Symbol::from("nil?"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        );
        is_nil.ty = Some(Ty::Bool);
        let assign = Expr::new(
            Span::synthetic(),
            ExprNode::Assign {
                target: crate::expr::LValue::Ivar { name: storage },
                value: var_ref(staged),
            },
        );
        stmts.push(Expr::new(
            Span::synthetic(),
            ExprNode::If {
                cond: is_nil,
                then_branch: assign,
                else_branch: super::nil_lit(),
            },
        ));
    }
    stmts.push(crate::lower::typing::with_ty(var_ref(&inserted_key), key_ty.clone()));
    Expr::new(Span::synthetic(), ExprNode::Seq { exprs: stmts })
}

/// `def _adapter_update` — instance method; reads ivars + @id.
/// See `synth_adapter_insert` for the receiver-rationale.
fn synth_adapter_update(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    // Same storage-ivar convention as `synth_adapter_insert`.
    let assignments: Vec<Assignment> = table
        .columns
        .iter()
        .filter(|c| !c.primary_key && c.generated.is_none())
        .map(|c| Assignment {
            column: c.name.clone(),
            value: Value::Runtime {
                expr: column_bind_expr(model, c),
                ty: bind_type(model, c),
            },
        })
        .collect();
    let has_assignments = !assignments.is_empty();

    let update = |conditions: Predicate| {
        ArelOp::Update(Update { table: TableRef(table.name.clone()), assignments: assignments.clone(), conditions: Some(conditions) })
    };
    let mut body = dialect.visit(&update(eq_id_ivar(model, table)), schema, owner);
    if let Some(cols) = query_constraint_columns(model, table) {
        let composite = dialect.visit(&update(eq_query_constraints(model, table, &cols)), schema, owner);
        body = when_query_constraints(composite, body);
    }

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_update"),
        receiver: MethodReceiver::Instance,
        params: vec![],
        // A model whose only non-key attributes are generated columns has
        // nothing to write on update. Do not render `UPDATE ... SET WHERE`;
        // generated values are database-owned and a no-op save stays a no-op.
        body: if has_assignments { body } else { super::nil_lit() },
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def _adapter_touch` — `touch`'s write: `updated_at` alone, as
/// Rails' `touch` issues it. A full-row `_adapter_update` writes back
/// every column the record loaded, so a column the database maintains
/// behind the record — campfire's trigger-kept `rooms.messages_count`,
/// which a message's `belongs_to :room, touch: true` reaches — is
/// overwritten with the stale in-memory value. A table without
/// `updated_at` has nothing to write.
fn synth_adapter_touch(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    let assignments: Vec<Assignment> = table
        .columns
        .iter()
        .filter(|c| c.name.as_str() == "updated_at" && c.generated.is_none())
        .map(|c| Assignment {
            column: c.name.clone(),
            value: Value::Runtime {
                expr: column_bind_expr(model, c),
                ty: bind_type(model, c),
            },
        })
        .collect();
    let has_assignments = !assignments.is_empty();
    let update = |conditions: Predicate| {
        ArelOp::Update(Update { table: TableRef(table.name.clone()), assignments: assignments.clone(), conditions: Some(conditions) })
    };
    let mut body = dialect.visit(&update(eq_id_ivar(model, table)), schema, owner);
    if let Some(cols) = query_constraint_columns(model, table) {
        let composite = dialect.visit(&update(eq_query_constraints(model, table, &cols)), schema, owner);
        body = when_query_constraints(composite, body);
    }
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_touch"),
        receiver: MethodReceiver::Instance,
        params: vec![],
        body: if has_assignments { body } else { super::nil_lit() },
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def _adapter_delete` — instance method; reads @id.
/// See `synth_adapter_insert` for the receiver-rationale.
fn synth_adapter_delete(model: &Model, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let owner = &model.name;
    let delete = |conditions: Predicate| ArelOp::Delete(Delete { table: TableRef(table.name.clone()), conditions: Some(conditions) });
    let mut body = dialect.visit(&delete(eq_id_ivar(model, table)), schema, owner);
    if let Some(cols) = query_constraint_columns(model, table) {
        let composite = dialect.visit(&delete(eq_query_constraints(model, table, &cols)), schema, owner);
        body = when_query_constraints(composite, body);
    }

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_delete"),
        receiver: MethodReceiver::Instance,
        params: vec![],
        body,
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

fn synth_adapter_count(owner: &ClassId, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let op = ArelOp::Select(Select {
        single_record: false, // _adapter_count — a scalar
        table: TableRef(table.name.clone()),
        columns: ColumnSpec::Count,
        conditions: None,
        orders: vec![],
        limit: None,
        joins: vec![],
            preloads: vec![],
    });

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_count"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body: dialect.visit(&op, schema, owner),
        signature: Some(fn_sig(vec![], Ty::Int)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def self.delete_all` — bulk DELETE with ActiveRecord semantics:
/// rows go, the affected-row count comes back, the autoincrement
/// counter stays (`_adapter_truncate` is
/// the sequence-resetting sibling, for test setup). A PUBLIC name —
/// this per-model override shadows `Base.delete_all`'s
/// adapter-routing default, so strict targets go Db-direct like every
/// other CRUD primitive (and targets with no adapter module at all,
/// e.g. Elixir, never see the routing body).
fn synth_delete_all(owner: &ClassId, table: &Table, dialect: SqlDialect) -> MethodDef {
    use crate::expr::Literal;

    let exec = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![Symbol::from("Db")] },
            )),
            method: Symbol::from("exec"),
            args: vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit {
                    value: Literal::Str {
                        value: format!("DELETE FROM {}", dialect.ident(table.name.as_str())),
                    },
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    // Rails returns the affected-row count; `Db.changes` reads it off
    // the `exec` just issued.
    let changes = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![Symbol::from("Db")] },
            )),
            method: Symbol::from("changes"),
            args: vec![],
            block: None,
            parenthesized: true,
        },
    );
    let body = Expr::new(
        Span::synthetic(),
        ExprNode::Seq { exprs: vec![exec, changes] },
    );

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("delete_all"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body,
        signature: Some(fn_sig(vec![], Ty::Int)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

fn synth_adapter_truncate(owner: &ClassId, table: &Table, schema: &Schema, dialect: SqlDialect) -> MethodDef {
    let op = ArelOp::Delete(Delete {
        table: TableRef(table.name.clone()),
        conditions: None,
    });

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_truncate"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body: dialect.visit(&op, schema, owner),
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def _adapter_reload` — SELECT-and-assign-into-self variant of
/// `_adapter_find_by_id`. Re-reads the row by `@id` and writes columns
/// back into `self` (preserving identity); returns `self` — reloaded
/// when the row is present, unchanged when it has been deleted. Backs
/// framework Ruby's `Base#reload` ("silently no-ops when the row no
/// longer exists"). Returns `self` rather than a `result`/nil
/// accumulator so the functional (immutable) lowering threads the
/// reloaded record cleanly — `record = if step? do …reloaded… else
/// record end` — instead of leaving a dead local.
///
/// Modelled as an INSTANCE method (not a class method) so callers
/// reach it via implicit-self dispatch (`_adapter_reload`) rather
/// than `self.class._adapter_reload(self)`. The class-method form
/// trips an emit issue under the libsql async profile where the
/// emitter mishandles `self.class.<async_method>` — it lifts the
/// await to the receiver Send (`(await this.constructor).…`) and
/// drops the Promise from the actual call.
///
/// Built inline (not through the visitor) because the visitor's
/// single-hydrate shape always constructs a fresh `Owner.new`;
/// reload needs to write into self. Generalizing the visitor with
/// a "hydrate target = bare ivar / passed-in symbol" option is the
/// right cleanup once a second use surfaces.
fn synth_adapter_reload(model: &Model, table: &Table, dialect: SqlDialect) -> MethodDef {
    use crate::expr::{ExprNode, LValue, Literal};
    use crate::span::Span;

    let owner = &model.name;
    let stmt = Symbol::from("stmt");
    let db = ClassId(Symbol::from("Db"));
    let owner_ty = Ty::Class { id: owner.clone(), args: vec![].into() };

    // Rails' `reload` clears the association cache — owned by
    // associations (same kinds as `assoc_cache_ivar_bindings`).
    let clear_assoc = super::associations::clear_loaded_flag_assigns(model);

    // SQL: "SELECT <cols> FROM <table> WHERE id = " + Db.escape_int(@id) + " LIMIT 1"
    let cols_csv: String = table
        .columns
        .iter()
        .map(|c| dialect.ident(c.name.as_str()))
        .collect::<Vec<_>>()
        .join(", ");

    // Placeholder-bind gate (roundhouse#12). Reload is a `Db.prepare`
    // read — it hits the prepared-statement cache with a per-id key when
    // inlined, so parameterize its `@id` predicate too, keeping it in
    // step with the visitor-emitted find/exists paths. Gate off ⇒
    // inline-escape, byte-identical to before.
    let param = dialect == SqlDialect::Postgres || crate::lower::arel::visitor::param_binds_enabled();
    // The key column and its ivar, bound with the key's type — the same
    // column the visitor-emitted find/exists paths compare.
    let key = key_column_name(model, table);
    let id_ivar = match key_column(model, table) {
        Some(c) => column_bind_expr(model, c),
        None => Expr::new(Span::synthetic(), ExprNode::Ivar { name: key.clone() }),
    };
    let select = format!("SELECT {} FROM {} WHERE ", cols_csv, dialect.ident(table.name.as_str()));
    let (sql_concat, binds) =
        reload_lookup(&select, &[(key, id_ivar, key_value_type(model, table))], param, dialect, &db, &stmt);

    // if Db.step?(stmt) ; @<col> = Db.column_<int|text>(stmt, i) ; ... ; mark_persisted! ; end
    let mut if_body: Vec<Expr> = Vec::new();
    for (i, col) in table.columns.iter().enumerate() {
        // A nullable column reads through the `_opt` primitive so NULL
        // arrives as nil instead of the type's zero — `""` for a
        // nullable UNIQUE column would collide row-to-row, and 0 in a
        // nullable fk would make `where(fk: nil)` miss every row that
        // never set it.
        let read_method = super::schema::column_read_method_for(col);
        let read_call = arel_db_call(
            &db,
            read_method,
            vec![var_ref(&stmt), arel_lit_int(i as i64)],
        );
        // A retypable column rehydrates through its deserializing writer.
        if super::schema::is_retypable(model, col) {
            if_body.push(Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
                    method: super::schema::retypable_storage_writer(col),
                    args: vec![read_call],
                    block: None,
                    parenthesized: true,
                },
            ));
            continue;
        }
        if_body.push(Expr::new(
            Span::synthetic(),
            ExprNode::Assign {
                target: LValue::Ivar { name: super::schema::col_storage_name(col) },
                value: read_call,
            },
        ));

    }
    if_body.push(Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
            method: Symbol::from("mark_persisted!"),
            args: vec![],
            block: None,
            parenthesized: true,
        },
    ));

    let if_expr = Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: arel_db_call(&db, "step?", vec![var_ref(&stmt)]),
            then_branch: Expr::new(Span::synthetic(), ExprNode::Seq { exprs: if_body }),
            else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        },
    );

    // clear assoc loaded ; stmt = prepare ; [bind_int(stmt, 1, @id)] ; if step? {…} ; finalize ; self
    let assemble = |sql: Expr, binds: Vec<Expr>| {
        let mut body_exprs = clear_assoc.clone();
        body_exprs.push(arel_assign(&stmt, arel_db_call(&db, "prepare", vec![sql])));
        body_exprs.extend(binds);
        body_exprs.push(if_expr.clone());
        body_exprs.push(arel_db_call(&db, "finalize", vec![var_ref(&stmt)]));
        body_exprs.push(Expr::new(Span::synthetic(), ExprNode::SelfRef));
        Expr::new(Span::synthetic(), ExprNode::Seq { exprs: body_exprs })
    };
    let mut body = assemble(sql_concat, binds);
    // Rails' reload finds the row by `_in_memory_query_constraints_hash`.
    if let Some(cols) = query_constraint_columns(model, table) {
        let keys: Vec<_> = cols.iter().map(|c| (c.name.clone(), column_bind_expr(model, c), bind_type(model, c))).collect();
        let (sql, binds) = reload_lookup(&select, &keys, param, dialect, &db, &stmt);
        body = when_query_constraints(assemble(sql, binds), body);
    }

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_adapter_reload"),
        receiver: MethodReceiver::Instance,
        params: vec![],
        body,
        signature: Some(fn_sig(vec![], owner_ty)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// The WHERE of `_adapter_reload`'s SELECT on `keys` (column, bound
/// value, bind type): each `<col> = <value>`, joined by AND, then
/// ` LIMIT 1`. Returns the SQL and, when the values are placeholders, the
/// `Db.bind_*` calls; inline, the values are escaped into the SQL.
fn reload_lookup(
    select: &str,
    keys: &[(Symbol, Expr, ValueType)],
    param: bool,
    dialect: SqlDialect,
    db: &ClassId,
    stmt: &Symbol,
) -> (Expr, Vec<Expr>) {
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    for (i, (column, value, ty)) in keys.iter().enumerate() {
        let lead = if i == 0 { select } else { " AND " };
        parts.push(arel_lit_str(format!("{lead}{} = ", dialect.ident(column.as_str()))));
        let (bind_method, escape_method) = match ty.for_dialect(dialect) {
            ValueType::Str => ("bind_text", "escape_string"),
            ValueType::Binary => ("bind_binary", "escape_binary"),
            ValueType::BinaryOpt => ("bind_binary_opt", "escape_binary_opt"),
            ValueType::StrOpt => ("bind_text_opt", "escape_string_opt"),
            ValueType::IntOpt => ("bind_int_opt", "escape_int_opt"),
            _ => ("bind_int", "escape_int"),
        };
        if param {
            parts.push(arel_lit_str(dialect.placeholder(i + 1)));
            binds.push(arel_db_call(db, bind_method, vec![var_ref(stmt), arel_lit_int(i as i64 + 1), value.clone()]));
        } else {
            parts.push(arel_db_call(db, escape_method, vec![value.clone()]));
        }
    }
    parts.push(arel_lit_str(" LIMIT 1".to_string()));
    (arel_concat(parts), binds)
}

/// `def self._compiled_query_constraints; ["shop_id", "id"]; end` — the
/// list the compiled update, destroy and reload key on
/// (`Model::query_constraints`). The runtime's `query_constraints`
/// accepts this list and raises for any other.
fn synth_compiled_query_constraints(model: &Model) -> MethodDef {
    let names = model.query_constraints.iter().map(|c| c.as_str().to_string()).collect();
    synth_class_string_list(model, "_compiled_query_constraints", names)
}

/// `def self._compiled_ignored_columns; ["legacy_flag"]; end` — the
/// `ignored_columns` the compiled table already leaves out
/// (`Model::ignored_columns`). The runtime's `ignored_columns=` accepts a
/// list within it and raises for any other.
fn synth_compiled_ignored_columns(model: &Model) -> MethodDef {
    synth_class_string_list(model, "_compiled_ignored_columns", model.ignored_columns.clone())
}

/// `def self.<name>; [<names>]; end`.
fn synth_class_string_list(model: &Model, name: &str, names: Vec<String>) -> MethodDef {
    let owner = &model.name;
    let elements = names.into_iter().map(arel_lit_str).collect();
    let mut body = Expr::new(Span::synthetic(), ExprNode::Array { elements, style: crate::expr::ArrayStyle::Brackets });
    body.ty = Some(Ty::Array { elem: std::sync::Arc::new(Ty::Str) });
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Class,
        params: vec![],
        body,
        signature: Some(fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(Ty::Str) })),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// The columns `query_constraints` names (`Model::query_constraints`),
/// when the class body names any.
fn query_constraint_columns<'a>(model: &Model, table: &'a Table) -> Option<Vec<&'a crate::schema::Column>> {
    if model.query_constraints.is_empty() {
        return None;
    }
    model.query_constraints.iter().map(|n| table.columns.iter().find(|c| &c.name == n)).collect()
}

/// `<a> = @a AND <b> = @b`, in the order `query_constraints` names them:
/// Rails' `_query_constraints_hash`.
fn eq_query_constraints(model: &Model, table: &Table, cols: &[&crate::schema::Column]) -> Predicate {
    cols.iter()
        .map(|c| {
            Predicate::Eq(
                ColRef { table: TableRef(table.name.clone()), column: c.name.clone() },
                Value::Runtime { expr: in_database_bind_expr(model, table, c), ty: bind_type(model, c) },
            )
        })
        .reduce(|a, b| Predicate::And(Box::new(a), Box::new(b)))
        .expect("query_constraints names at least one column")
}

/// [`column_bind_expr`] over the value the row holds (`attribute_in_database`),
/// not the pending one: Rails' `_query_constraints_hash` addresses the row by
/// its stored key, so `address.update!(shop_id: other)` finds the row under
/// the old `shop_id` instead of matching nothing under the new one. The
/// key column keeps its ivar (the dirty snapshot does not carry it), and so
/// do the columns of a composite primary key: a key is not reassigned.
fn in_database_bind_expr(model: &Model, table: &Table, col: &crate::schema::Column) -> Expr {
    if col.primary_key
        || key_column(model, table).is_some_and(|k| k.name == col.name)
        || model.composite_primary_key.iter().any(|k| k.as_str() == col.name.as_str())
    {
        return column_bind_expr(model, col);
    }
    let stored = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
            method: Symbol::from("attribute_in_database"),
            args: vec![arel_lit_str(col.name.as_str().to_string())],
            block: None,
            parenthesized: true,
        },
    );
    if super::schema::is_retypable(model, col) {
        let self_class = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
                method: Symbol::from("class"),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        );
        declared_type_send(self_class, "_attribute_serialize", col, stored)
    } else {
        stored
    }
}

/// `if self.class.query_constraints_list then <composite> else <single>
/// end`: the composite WHERE once the class body's `query_constraints`
/// call has run (Core's runs it only under its Yugabyte configuration).
fn when_query_constraints(composite: Expr, single: Expr) -> Expr {
    when_query_constraints_on(true, composite, single)
}

/// [`when_query_constraints`] in an instance method (`self.class`) or a
/// class method (`self`).
fn when_query_constraints_on(instance: bool, composite: Expr, single: Expr) -> Expr {
    let send = |recv: Expr, method: &str| {
        Expr::new(
            Span::synthetic(),
            ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args: vec![], block: None, parenthesized: false },
        )
    };
    let receiver = Expr::new(Span::synthetic(), ExprNode::SelfRef);
    let receiver = if instance { send(receiver, "class") } else { receiver };
    let cond = send(receiver, "query_constraints_list");
    Expr::new(Span::synthetic(), ExprNode::If { cond, then_branch: composite, else_branch: single })
}

/// An identifier quoted the way Rails' SQLite adapter always quotes one
/// (`"messages"."id"`). Apps' tests pick statements out by that text:
/// campfire's query-plan tests filter on `start_with?(%(SELECT
/// "messages"))` (basecamp/once-campfire#312).
fn rails_quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `def self._columns_sql; "\"<table>\".\"<col>\" AS <col>, …"; end`
///
/// The schema columns, table-qualified, in the order `from_stmt` reads
/// them. `Relation#to_a` projects this instead of `<table>.*` so a
/// run-time composed SELECT (joins and all) still yields rows whose
/// positions the emit fixed at compile time — the precondition for
/// hydrating through `from_stmt` rather than a String-keyed Hash. The
/// qualification is what keeps `id` unambiguous under a JOIN; the
/// alias is what keeps `ORDER BY created_at` unambiguous under one:
/// sqlite resolves an ORDER BY name against the output columns' ALIASES
/// before the FROM tables, and `<table>.*` names its outputs, while a
/// bare `<table>.<col>` does not (campfire's `user.rooms.order(:created_at)`
/// joins memberships, which has its own).
fn synth_columns_sql(owner: &ClassId, table: &Table, dialect: SqlDialect) -> MethodDef {
    let cols_csv: String = table
        .columns
        .iter()
        .map(|c| {
            format!(
                "{t}.{q} AS {alias}",
                t = rails_quoted(table.name.as_str()),
                q = rails_quoted(c.name.as_str()),
                alias = dialect.ident(c.name.as_str())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_columns_sql"),
        receiver: MethodReceiver::Class,
        params: vec![],
        body: arel_lit_str(cols_csv),
        signature: Some(fn_sig(vec![], Ty::Str)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

/// `def self._hydrate_all(sql); stmt = Db.prepare(sql); results = [];
/// while Db.step?(stmt); results << from_stmt(stmt); end;
/// Db.finalize(stmt); results; end`
///
/// The `_adapter_all` loop with the SELECT supplied by the caller.
/// The contract is that `sql` projects exactly `_columns_sql` — which
/// is what `Relation#to_a` composes when the app never said
/// `select(...)`. One typed record per row and no Hash in between:
/// no per-cell key hashing on the write or the read, no boxing, and
/// nothing for the collector to finalise but the record itself.
fn synth_hydrate_all(owner: &ClassId) -> MethodDef {
    let sql = Symbol::from("sql");
    let stmt = Symbol::from("stmt");
    let results = Symbol::from("results");
    let db = ClassId(Symbol::from("Db"));
    let owner_ty = Ty::Class { id: owner.clone(), args: vec![].into() };
    let owner_array_ty = Ty::Array { elem: std::sync::Arc::new(owner_ty) };

    let stmt_assign = arel_assign(&stmt, arel_db_call(&db, "prepare", vec![var_ref(&sql)]));
    // Typed empty literal — same reason as the visitor's multi hydrate
    // (Crystal's `[] of Owner`).
    let results_init = arel_assign(
        &results,
        crate::lower::typing::with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Array { elements: vec![], style: crate::expr::ArrayStyle::Brackets },
            ),
            owner_array_ty.clone(),
        ),
    );
    let from_stmt = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![owner.0.clone()] },
            )),
            method: Symbol::from("from_stmt"),
            args: vec![var_ref(&stmt)],
            block: None,
            parenthesized: true,
        },
    );
    let push = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(var_ref(&results)),
            method: Symbol::from("<<"),
            args: vec![from_stmt],
            block: None,
            parenthesized: false,
        },
    );
    let while_loop = Expr::new(
        Span::synthetic(),
        ExprNode::While {
            cond: arel_db_call(&db, "step?", vec![var_ref(&stmt)]),
            body: Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![push] }),
            until_form: false,
        },
    );
    let finalize = arel_db_call(&db, "finalize", vec![var_ref(&stmt)]);
    let body = Expr::new(
        Span::synthetic(),
        ExprNode::Seq {
            exprs: vec![stmt_assign, results_init, while_loop, finalize, var_ref(&results)],
        },
    );

    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("_hydrate_all"),
        receiver: MethodReceiver::Class,
        params: vec![Param::positional(sql.clone())],
        body,
        signature: Some(fn_sig(vec![(sql, Ty::Str)], owner_array_ty)),
        effects: EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    }
}

// Inline expr helpers used by synth_adapter_reload — the ones in
// `super` are pub(super) but require helper visibility we don't want
// to widen for one synth function. Naming-prefixed to avoid shadowing.
fn arel_lit_str(s: String) -> Expr {
    use crate::expr::{ExprNode, Literal};
    use crate::span::Span;
    let mut e = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: s } });
    e.ty = Some(Ty::Str);
    e
}

fn arel_lit_int(value: i64) -> Expr {
    use crate::expr::{ExprNode, Literal};
    use crate::span::Span;
    let mut e = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Int { value } });
    e.ty = Some(Ty::Int);
    e
}

fn arel_assign(name: &Symbol, value: Expr) -> Expr {
    use crate::expr::{ExprNode, LValue};
    use crate::ident::VarId;
    use crate::span::Span;
    Expr::new(
        Span::synthetic(),
        ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: name.clone() },
            value,
        },
    )
}

fn arel_db_call(db: &ClassId, method: &str, args: Vec<Expr>) -> Expr {
    use crate::expr::ExprNode;
    use crate::span::Span;
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(Span::synthetic(), ExprNode::Const {
                path: db.0.as_str().split("::").map(Symbol::from).collect(),
            })),
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized: true,
        },
    )
}

// Fold through the shared arel helper so adapter SQL gets the same
// adjacent-literal merge as query emission.
use crate::lower::arel::visitor::concat_chain as arel_concat;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The table's key column: the one marked `primary_key: true`, which is
/// `id` unless `create_table primary_key:` named another. Every
/// key-taking primitive binds THIS column with THIS type — a uuid key
/// is `WHERE id = <str>`, not an integer compare against text (#90).
/// A table with no key column (a join table) keeps `id`/Int, the
/// shape the primitives always assumed.
/// The key the adapter binds: the schema column flagged primary key,
/// else the column `self.primary_key =` names when the schema flags
/// none (a composite key, `Table.primary_key_columns`, as Core's
/// `PRIMARY KEY((shop_id) HASH, id ASC, island ASC)` with
/// `self.primary_key = :id`) — the column Rails' find / update /
/// destroy use. A model on a composite key that declares none
/// (`Model::composite_primary_key`) has no single key column: its `id` column
/// when that is part of the key, else its first column, is the one the
/// INSERT answers (`composite_key::rekey` builds the tuple around it).
fn key_column<'a>(model: &Model, table: &'a Table) -> Option<&'a crate::schema::Column> {
    super::primary_key_column(table)
        .or_else(|| {
            let named = model.primary_key.as_ref()?;
            table.columns.iter().find(|c| &c.name == named)
        })
        .or_else(|| {
            let keys = &model.composite_primary_key;
            let named = keys.iter().find(|k| k.as_str() == "id").or(keys.first())?;
            table.columns.iter().find(|c| &c.name == named)
        })
}

/// The implicit SQLite rowid is integer-valued, as are the integer key
/// types handled by `last_insert_rowid` on the ordinary insert path.
fn key_is_integer(model: &Model, table: &Table) -> bool {
    use crate::schema::ColumnType;
    key_column(model, table)
        .map(|column| matches!(column.col_type, ColumnType::Integer | ColumnType::BigInt))
        .unwrap_or(true)
}

fn key_column_name(model: &Model, table: &Table) -> Symbol {
    key_column(model, table).map(|c| c.name.clone()).unwrap_or_else(|| Symbol::from("id"))
}

/// A retypable key (`Model::retypable_attributes`) holds the declared
/// type's cast value, so it is typed gradually.
pub(super) fn key_ty(model: &Model, table: &Table) -> Ty {
    match key_column(model, table) {
        Some(c) if super::schema::is_retypable(model, c) => Ty::Untyped,
        Some(c) => ty_of_column(&c.col_type),
        None => Ty::Int,
    }
}

fn key_value_type(model: &Model, table: &Table) -> ValueType {
    key_column(model, table).map(|c| bind_type(model, c)).unwrap_or(ValueType::Int)
}

/// `Eq(<table>.<key>, Runtime(<id-param>, <key type>))` — find_by_id /
/// exists_by_id? shape (id arrives as a method param). A retypable key
/// binds what its declared type makes of the argument (cast, then
/// serialized), as a where predicate does.
pub(super) fn eq_id_param(model: &Model, table: &Table, id_param: &Symbol) -> Predicate {
    let id = match key_column(model, table) {
        Some(c) if super::schema::is_retypable(model, c) => declared_type_send(
            Expr::new(Span::synthetic(), ExprNode::SelfRef),
            "_attribute_for_database",
            c,
            var_ref(id_param),
        ),
        _ => var_ref(id_param),
    };
    Predicate::Eq(
        ColRef { table: TableRef(table.name.clone()), column: key_column_name(model, table) },
        Value::Runtime { expr: id, ty: key_value_type(model, table) },
    )
}

/// `Eq(<table>.<key>, Runtime(@<key>, <key type>))` — instance-method
/// update / delete shape (the key is read from the instance ivar).
/// Used so save / destroy can dispatch to `_adapter_update` /
/// `_adapter_delete` via implicit-self (`_adapter_update`) instead of
/// the `self.class._adapter_update(@id, self)` chain that the TS
/// emitter mishandles under the libsql async profile.
fn eq_id_ivar(model: &Model, table: &Table) -> Predicate {
    let key = key_column_name(model, table);
    let expr = match key_column(model, table) {
        Some(c) => column_bind_expr(model, c),
        None => ivar_ref(&key),
    };
    Predicate::Eq(
        ColRef { table: TableRef(table.name.clone()), column: key },
        Value::Runtime { expr, ty: key_value_type(model, table) },
    )
}

/// The value an instance method binds for a column: its storage ivar,
/// or for a retypable column what the declared type serializes it to.
fn column_bind_expr(model: &Model, col: &crate::schema::Column) -> Expr {
    let ivar = ivar_ref(&super::schema::col_storage_name(col));
    if super::schema::is_retypable(model, col) {
        let self_class = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(Expr::new(Span::synthetic(), ExprNode::SelfRef)),
                method: Symbol::from("class"),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        );
        declared_type_send(self_class, "_attribute_serialize", col, ivar)
    } else {
        ivar
    }
}

/// `<recv>.<method>("<col>", value)` — a runtime Base declared-type
/// conversion.
fn declared_type_send(recv: Expr, method: &str, col: &crate::schema::Column, value: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args: vec![arel_lit_str(col.name.as_str().to_string()), value],
            block: None,
            parenthesized: true,
        },
    )
}

/// The escape/bind primitive for a column. A serialized value can be
/// nil whatever the column's nullability (an uncastable input), so a
/// retypable column binds through the `_opt` primitive and NULL reaches
/// the database, as in Rails.
fn bind_type(model: &Model, col: &crate::schema::Column) -> ValueType {
    let ty = value_type_for_column(col);
    if !super::schema::is_retypable(model, col) {
        return ty;
    }
    match ty {
        ValueType::Int => ValueType::IntOpt,
        ValueType::Str => ValueType::StrOpt,
        ValueType::Bool => ValueType::BoolOpt,
        ValueType::Binary => ValueType::BinaryOpt,
        other => other,
    }
}

fn ivar_ref(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Ivar { name: name.clone() })
}

fn var_ref(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Var { id: VarId(0), name: name.clone() })
}

/// Which `Db.escape_*` primitive writes this column. A nullable column
/// picks the `_opt` variant: nil has to reach the DB as the SQL keyword
/// NULL, not as `''` / `0` — otherwise a nullable UNIQUE column
/// collides on its second unset row and `where(fk: nil)` matches
/// nothing. A NOT NULL string / integer with no default holds nil until
/// set (`ty_of_column_slot`), so it picks the `_opt` variant too and the
/// database refuses the row, as in Rails, rather than being handed `''`
/// / `0`. Temporal columns store ISO-8601 text, so they escape as
/// strings like any other text column.
fn value_type_for_column(col: &crate::schema::Column) -> ValueType {
    let unset = !col.nullable
        && col.generated.is_none()
        && matches!(super::ty_of_column_slot(col), Ty::Union { .. });
    let nullable = (col.nullable || unset) && !col.primary_key;
    match ty_of_column(&col.col_type) {
        Ty::Int => {
            if nullable {
                ValueType::IntOpt
            } else {
                ValueType::Int
            }
        }
        Ty::Bool => {
            if nullable {
                ValueType::BoolOpt
            } else {
                ValueType::Bool
            }
        }
        Ty::Float => {
            if nullable {
                ValueType::FloatOpt
            } else {
                ValueType::Str
            }
        }
        _ if matches!(col.col_type, crate::schema::ColumnType::Binary) => {
            if nullable {
                ValueType::BinaryOpt
            } else {
                ValueType::Binary
            }
        }
        _ => {
            if nullable {
                ValueType::StrOpt
            } else {
                ValueType::Str
            }
        }
    }
}
