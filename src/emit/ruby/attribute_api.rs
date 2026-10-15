//! Ruby-family attribute API surface on lowered models: the parts of
//! ActiveRecord 8.2.0.alpha's `[]`, `abstract_class` and
//! `type_for_attribute` that need the app's schema or class body.
//!
//! - `record["shop_id"]` / `record["shop_id"] = v`: Rails' `[]` is
//!   `read_attribute`, which takes `attr_name.to_s`, so a String and a
//!   Symbol name read the same attribute. The shared model lowering
//!   synthesizes `[]`/`[]=` as a `case` over Symbols (every target can
//!   type that), and a String fell through to nil. Here the Ruby family,
//!   whose callers can pass either, turns a String name into the Symbol
//!   first. Any other name still reads nil, as an unknown attribute does
//!   in Rails.
//! - `self.abstract_class = true` (and `primary_abstract_class`, which
//!   sets it) is replayed into the class body, so the runtime's
//!   `abstract_class` / `abstract_class?` (inheritance.rb's
//!   `attr_accessor :abstract_class`, a per-class ivar) answer for the
//!   class that declared it and not for its subclasses.
//! - `schema_column_types`: per model, the column name to the cast type
//!   the database adapter's type map gives that column, as data. The
//!   runtime's `type_for_attribute` / `attribute_types` build the type
//!   objects from it (`runtime/ruby/active_record/connection.rb`). An
//!   entry the type port does not carry is `[:unported, "<class>"]`, and
//!   asking for that attribute's type raises NotImplementedError naming
//!   the missing class rather than answering a different type.
//!
//! A class-body call to an app class macro is replayed by the shared
//! lowering (`model_to_library::replay_model_macros`, a `ClassBody`
//! statement) when everything it does at load is something the lowered
//! class answers, and refused otherwise; it is no longer replayed here
//! unguarded, which crashed the class at load when the macro reached
//! model DSL (`attribute`, `query_constraints`, `belongs_to`).
//!
//! Ruby-family only: the strict targets keep the Symbol-typed `[]`, have
//! no runtime for `abstract_class` or `type_for_attribute`, and cannot
//! run a class-body call.

use crate::App;
use crate::database::DatabaseAdapter;
use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver, MethodVisibility, Model};
use crate::effect::EffectSet;
use crate::expr::{ArrayStyle, Expr, ExprNode, LValue, Literal};
use crate::ident::{Symbol, VarId};
use crate::schema::{Column, ColumnType, Table};
use crate::span::Span;
use crate::ty::Ty;

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    let adapter = app.primary_database_adapter();
    for model in &app.models {
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else {
            continue;
        };
        // The lowering wrote the mattr seeds, then the body's ClassBody
        // statements (a replayed class macro among them,
        // `model_to_library::replay_model_macros`); the abstract-class
        // write runs between the two.
        if let Some(value) = crate::lower::model_to_library::markers::abstract_class_setting(model) {
            let seeds = model.class_attr_defaults.len().min(lc.class_ivar_initializers.len());
            lc.class_ivar_initializers.insert(seeds, abstract_class_write(value));
        }
        let Some(table) = app.schema.tables.get(&model.table.0) else {
            continue;
        };
        for m in &mut lc.methods {
            if m.receiver == MethodReceiver::Instance
                && m.name_span.is_synthetic()
                && matches!(m.name.as_str(), "[]" | "[]=")
            {
                accept_string_name(m);
            }
        }
        if !lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "schema_column_types") {
            lc.methods.push(schema_column_types(model, table, adapter));
        }
    }
}


fn sp(node: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), node)
}

fn typed(node: ExprNode, ty: Ty) -> Expr {
    let mut e = sp(node);
    e.ty = Some(ty);
    e
}

fn abstract_class_write(value: bool) -> Expr {
    sp(ExprNode::Send {
        recv: Some(sp(ExprNode::SelfRef)),
        method: Symbol::from("abstract_class="),
        args: vec![typed(ExprNode::Lit { value: Literal::Bool { value } }, Ty::Bool)],
        block: None,
        parenthesized: false,
    })
}

/// `name = name.to_sym if name.is_a?(::String)` ahead of the `case`.
fn accept_string_name(m: &mut MethodDef) {
    // A column-free table's reader and writer are a bare `nil` (no
    // `case` to normalize the name for).
    if !matches!(&*m.body.node, ExprNode::Case { .. }) {
        return;
    }
    let Some(param) = m.params.first() else { return };
    let name = param.name.clone();
    let var = || sp(ExprNode::Var { id: VarId(0), name: name.clone() });
    let is_string = typed(
        ExprNode::Send {
            recv: Some(var()),
            method: Symbol::from("is_a?"),
            args: vec![sp(ExprNode::Const { path: vec![Symbol::from("String")] })],
            block: None,
            parenthesized: true,
        },
        Ty::Bool,
    );
    let to_sym = sp(ExprNode::If {
        cond: is_string,
        then_branch: sp(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: name.clone() },
            value: typed(
                ExprNode::Send { recv: Some(var()), method: Symbol::from("to_sym"), args: vec![], block: None, parenthesized: false },
                Ty::Sym,
            ),
        }),
        else_branch: sp(ExprNode::Lit { value: Literal::Nil }),
    });
    let body = std::mem::replace(&mut m.body, sp(ExprNode::Lit { value: Literal::Nil }));
    m.body = sp(ExprNode::Seq { exprs: vec![to_sym, body] });
    if let Some(Ty::Fn { params, .. }) = m.signature.as_mut() {
        if let Some(first) = params.first_mut() {
            first.ty = std::sync::Arc::new(Ty::Union { variants: vec![Ty::Sym, Ty::Str].into() });
        }
    }
}

/// `def self.schema_column_types; { "id" => [:integer, 8], … }; end`.
fn schema_column_types(model: &Model, table: &Table, adapter: Option<&DatabaseAdapter>) -> MethodDef {
    let mut declared: Vec<(Symbol, Symbol)> = crate::lower::model_to_library::markers::attribute_api_decls(&model.body);
    declared.extend(
        crate::lower::model_to_library::markers::virtual_default_attribute_decls(model).into_iter().map(|(n, ty, _)| (n, ty)),
    );
    let mut entries: Vec<(Expr, Expr)> = Vec::new();
    for col in &table.columns {
        // `attribute :col, :type` replaces the column's type (Rails'
        // attribute API decorates the schema type map by name).
        let spec = match declared.iter().position(|(n, _)| *n == col.name) {
            Some(at) => declared_spec(&declared.remove(at).1),
            None if model.enums.contains_key(&col.name) => enum_spec(&model.enums[&col.name], column_spec(col, adapter)),
            None => column_spec(col, adapter),
        };
        entries.push((str_lit(col.name.as_str()), spec));
    }
    for (name, ty) in declared {
        entries.push((str_lit(name.as_str()), declared_spec(&ty)));
    }
    let ret = Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }) };
    MethodDef {
        visibility: MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from("schema_column_types"),
        receiver: MethodReceiver::Class,
        params: Vec::new(),
        body: typed(ExprNode::Hash { entries, kwargs: false }, ret.clone()),
        signature: Some(crate::lower::model_to_library::fn_sig(vec![], ret)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

fn str_lit(s: &str) -> Expr {
    typed(ExprNode::Lit { value: Literal::Str { value: s.to_string() } }, Ty::Str)
}

fn sym(s: &str) -> Expr {
    typed(ExprNode::Lit { value: Literal::Sym { value: Symbol::from(s) } }, Ty::Sym)
}

fn spec(kind: &str, limit: Option<i64>) -> Expr {
    let limit = match limit {
        Some(n) => typed(ExprNode::Lit { value: Literal::Int { value: n } }, Ty::Int),
        None => typed(ExprNode::Lit { value: Literal::Nil }, Ty::Nil),
    };
    sp(ExprNode::Array { elements: vec![sym(kind), limit], style: ArrayStyle::Brackets })
}

/// `[:decimal, precision, scale]`, PostgreSQL's OID::Decimal over them.
fn decimal_spec(precision: Option<u8>, scale: Option<u8>) -> Expr {
    let int = |n: Option<u8>| match n {
        Some(n) => typed(ExprNode::Lit { value: Literal::Int { value: i64::from(n) } }, Ty::Int),
        None => typed(ExprNode::Lit { value: Literal::Nil }, Ty::Nil),
    };
    sp(ExprNode::Array { elements: vec![sym("decimal"), int(precision), int(scale)], style: ArrayStyle::Brackets })
}

/// `[:enum, [[label, stored], …], <column spec>]`: ActiveRecord::Enum::EnumType
/// over the column's type, with the label → stored value mapping the `enum`
/// macro declared. A mapping whose stored values are not all integers, all
/// strings or all symbols is a value only known at run time and stays unported.
fn enum_spec(mapping: &[(String, Literal)], subtype: Expr) -> Expr {
    let ty = match mapping.first().map(|(_, v)| v) {
        Some(Literal::Int { .. }) => Ty::Int,
        Some(Literal::Str { .. }) => Ty::Str,
        Some(Literal::Sym { .. }) => Ty::Sym,
        _ => return unported("ActiveRecord::Enum::EnumType"),
    };
    let same_kind = |v: &Literal| {
        matches!((v, &ty), (Literal::Int { .. }, Ty::Int) | (Literal::Str { .. }, Ty::Str) | (Literal::Sym { .. }, Ty::Sym))
    };
    if !mapping.iter().all(|(_, v)| same_kind(v)) {
        return unported("ActiveRecord::Enum::EnumType");
    }
    let pairs = mapping
        .iter()
        .map(|(label, stored)| {
            sp(ExprNode::Array {
                elements: vec![str_lit(label), typed(ExprNode::Lit { value: stored.clone() }, ty.clone())],
                style: ArrayStyle::Brackets,
            })
        })
        .collect();
    sp(ExprNode::Array {
        elements: vec![sym("enum"), sp(ExprNode::Array { elements: pairs, style: ArrayStyle::Brackets }), subtype],
        style: ArrayStyle::Brackets,
    })
}

fn unported(class: &str) -> Expr {
    sp(ExprNode::Array { elements: vec![sym("unported"), str_lit(class)], style: ArrayStyle::Brackets })
}

/// The `ActiveRecord::Type.lookup(<sym>)` an `attribute :x, :<sym>`
/// declaration resolves to (activerecord/lib/active_record/type.rb's
/// registrations), where the port carries that class.
fn declared_spec(ty: &Symbol) -> Expr {
    match ty.as_str() {
        "integer" => spec("integer", None),
        "big_integer" => spec("big_integer", None),
        "string" => spec("string", None),
        "text" => spec("text", None),
        "boolean" => spec("boolean", None),
        "float" => spec("float", None),
        "binary" => spec("binary", None),
        "immutable_string" => spec("immutable_string", None),
        "value" => spec("value", None),
        "time" => spec("time", None),
        "json" => spec("json", None),
        other => unported(&format!("the `:{other}` attribute type")),
    }
}

/// The adapter type map's answer for a schema column (8.2.0.alpha's
/// `PostgreSQLAdapter.initialize_type_map` and `AbstractAdapter`'s).
/// The schema keeps the Rails type, not the SQL one, so a `t.string`
/// standing for `inet`/`cidr`/`macaddr` and a `t.text` for `citext` read
/// as String/Text where PostgreSQL would answer its OID type.
fn column_spec(col: &Column, adapter: Option<&DatabaseAdapter>) -> Expr {
    let pg = matches!(adapter, Some(DatabaseAdapter::Postgresql));
    let sqlite = matches!(adapter, Some(DatabaseAdapter::Sqlite3) | None);
    if !pg && !sqlite {
        return unported("this database adapter's type map");
    }
    match &col.col_type {
        ColumnType::Integer if pg => spec("integer", Some(4)),
        ColumnType::BigInt | ColumnType::Reference { .. } if pg => spec("integer", Some(8)),
        ColumnType::Integer | ColumnType::BigInt | ColumnType::Reference { .. } => {
            unported("ActiveRecord::ConnectionAdapters::SQLite3Adapter::SQLite3Integer")
        }
        ColumnType::String { limit } => spec("string", limit.map(i64::from)),
        ColumnType::Text => spec("text", None),
        ColumnType::Boolean => spec("boolean", None),
        ColumnType::Binary if pg => spec("bytea", None),
        ColumnType::Binary => spec("binary", None),
        ColumnType::Float => spec("float", None),
        ColumnType::Decimal { precision, scale } if pg => decimal_spec(*precision, *scale),
        ColumnType::Decimal { .. } => unported("ActiveModel::Type::Decimal"),
        ColumnType::Date if pg => spec("date", None),
        ColumnType::Date => unported("ActiveModel::Type::Date"),
        ColumnType::DateTime if pg => spec("timestamp", None),
        ColumnType::DateTime => unported("ActiveModel::Type::DateTime with the time zone converter"),
        // `register_class_with_precision m, "time", Type::Time`: an unsized
        // column (fmod -1) is precision 6 on PostgreSQL; SQLite reads it
        // from the declared type, which the schema does not keep.
        ColumnType::Time if pg => spec("time", Some(6)),
        ColumnType::Time => spec("time", None),
        ColumnType::Jsonb if pg => spec("jsonb", None),
        ColumnType::Json | ColumnType::Jsonb => spec("json", None),
        ColumnType::Uuid if pg => spec("uuid", None),
        ColumnType::Uuid => unported("a uuid column type"),
    }
}
