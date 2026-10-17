//! Ruby-family assignment casts and the public `attributes` hash.
//!
//! Rails casts every value assigned to an attribute through the
//! attribute's type (attribute.rb's FromUser: `type.cast(value)`), so
//! `Shop.new(count: "7").count` is 7 for an integer column, `"0"` is
//! false for a boolean one, `"9.99"` is a BigDecimal for a decimal one
//! and `""` is nil for all of them. The shared lowering writes the slot
//! as given (every target types the writer to the column's slot), with
//! ad hoc `to_i`/`to_f` pre-casts in `update` and `[]=`. Here the Ruby
//! family, whose callers can pass any value, casts instead:
//!
//! - a column's writer `<col>=` casts through `type_for_attribute`
//!   (the runtime's `Base._cast_column`), unless the value is nil or
//!   already of the class the type casts to (a hydrated value, a typed
//!   assignment), which the type would hand back unchanged. A temporal
//!   writer casts before formatting its stored text.
//! - the writer of a type declared with `attribute` over no column
//!   casts through it too, without the pass-through test.
//! - `[]=` (Rails' `write_attribute`) casts the same way, and `[]`
//!   reads the cast value without a pre-cast.
//! - `initialize`, `update`, `update!` and row hydration hand the value
//!   to the writer as given.
//! - a decimal column hydrates the driver's value (`Db.column_decimal`),
//!   not a Float, and `reload` casts a value whose stored class differs
//!   from the type's (a decimal, a declared type over another column).
//!   The Hash-row path (`<Model>Row.from_raw`, an explicit `select`)
//!   keeps a decimal's value too, and stores a timestamp the driver
//!   decoded as its full text (`ActiveSupport.format_db_time`), the
//!   microseconds included.
//!
//! The cast type is the one the model's `schema_column_types` names
//! (`emit::ruby::attribute_api`): a column whose type the port does not
//! carry (`[:unported, ...]`, an enum, JSON), a retypable column
//! (`Model::retypable_attributes`, which casts through its declared type
//! already) and a JSON `serialize`d one (its coder's type, not the
//! column's, casts: `lower::serialize`) keep the shared lowering's
//! writer. A single integer or text primary key's writer casts too, but
//! no other access to its slot changes: the slot keeps the unsaved
//! sentinel.
//!
//! A new record's `id` reader answers nil, as Rails' does, and its
//! `to_param` nil with it; the slot keeps the unsaved sentinel.
//!
//! `attributes` is Rails' `@attributes.to_hash`: every column in schema
//! order, the id included (nil on a new record, where the slot holds
//! the unsaved sentinel), each as its type reads it (a temporal column's
//! Date or Time, not its stored text). The lowering's String-keyed
//! snapshot without the id stays as `_attribute_snapshot`, which the
//! runtime's Dirty tracking and keyset order compare.

use crate::App;
use crate::dialect::{LibraryClass, MethodDef, MethodReceiver, Model};
use crate::expr::{Arm, BoolOpKind, BoolOpSurface, Expr, ExprNode, LValue, Literal};
use crate::ident::Symbol;
use crate::lower::model_to_library::schema::{col_storage_name, is_retypable, temporal_seam};
use crate::schema::{Column, ColumnType, Table};
use crate::span::Span;
use crate::ty::Ty;

/// What a column's cast type casts to, for the writer's pass-through
/// test.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Integer,
    Float,
    Boolean,
    Text,
    /// Cast every non-nil value: a decimal applies its scale, a uuid
    /// checks and formats its text.
    Always,
    Date,
    Time,
}

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else {
            continue;
        };
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else {
            continue;
        };
        let specs = spec_kinds(lc);
        let serialized: Vec<Symbol> =
            crate::lower::serialize::serialize_decls(model).into_iter().map(|d| d.column).collect();
        let cols: Vec<(&Column, Kind)> = table
            .columns
            .iter()
            .filter_map(|c| {
                if c.primary_key || is_retypable(model, c) || model.enums.contains_key(&c.name) || serialized.contains(&c.name) {
                    return None;
                }
                let kind = specs.iter().find(|(n, _)| n == c.name.as_str())?.1?;
                // The stored form follows the column: a temporal column
                // keeps text, any other its value.
                (matches!(kind, Kind::Date | Kind::Time) == is_temporal(c)).then_some((c, kind))
            })
            .collect();
        if !cols.is_empty() {
            cast_writes(lc, &cols);
        }
        cast_key_writer(lc, model, table, &specs);
        cast_declared_writers(lc, model, table);
        public_attributes(lc, model, table);
        nil_unsaved_id(lc, model, table);
        let row_class = format!("{}Row", model.name.0.as_str());
        if let Some(row_lc) = lcs.iter_mut().find(|lc| lc.name.0.as_str() == row_class) {
            raw_row_values(row_lc, &cols);
        }
    }
    for lc in lcs.iter_mut().filter(|lc| app.models.iter().any(|m| m.name == lc.name)) {
        nil_unsaved_param(lc);
    }
}

/// The single primary key's writer (`id=`, and `<key>=` for a key not
/// named `id`) casts as a column's does: Rails' `Owner.new(id: "6").id`
/// is 6, and `assign_attributes` hands the key to the writer as given.
/// Only the writer: the key's slot keeps the unsaved sentinel every
/// other synthesized read and write tests.
fn cast_key_writer(lc: &mut LibraryClass, model: &Model, table: &Table, specs: &[(String, Option<Kind>)]) {
    let mut keys = table.columns.iter().filter(|c| c.primary_key);
    let (Some(col), None) = (keys.next(), keys.next()) else { return };
    if is_retypable(model, col) {
        return;
    }
    let Some(kind) = specs.iter().find(|(n, _)| n == col.name.as_str()).and_then(|(_, k)| *k) else { return };
    if !matches!(kind, Kind::Integer | Kind::Text) || is_temporal(col) {
        return;
    }
    let own = format!("{}=", col.name.as_str());
    for m in lc.methods.iter_mut().filter(|m| m.name_span.is_synthetic() && m.receiver == MethodReceiver::Instance) {
        if m.name.as_str() == "id=" || m.name.as_str() == own {
            cast_writer(m, col, kind);
        }
    }
}

/// A type declared with `attribute :name, :<type>` over no column: the
/// shared lowering's writer assigns `@name = value` as given, where
/// Rails casts every assignment through the declared type (with its
/// TimeZoneConverter where time zone aware), so Core's
/// `attribute :publish_on, :time` reads a TimeWithZone, not the String
/// assigned. The writer casts through `type_for_attribute`, for a type
/// the port carries (`schema_column_types` names it); `:boolean` keeps
/// the lowering's own cast.
fn cast_declared_writers(lc: &mut LibraryClass, model: &Model, table: &Table) {
    let kinds = spec_symbols(lc);
    for (name, ty) in crate::lower::model_to_library::attribute_api_decls(&model.body) {
        if ty.as_str() == "boolean" || table.columns.iter().any(|c| c.name == name) {
            continue;
        }
        if !kinds.iter().any(|(n, k)| n == name.as_str() && k != "unported" && k != "registered") {
            continue;
        }
        let setter = format!("{}=", name.as_str());
        for m in lc.methods.iter_mut() {
            if !m.name_span.is_synthetic() || m.receiver != MethodReceiver::Instance || m.name.as_str() != setter {
                continue;
            }
            let Some(param) = m.params.first().map(|p| p.name.clone()) else { continue };
            if let ExprNode::Assign { target: LValue::Ivar { name: slot }, value } = &mut *m.body.node {
                if *slot == name && is_var(value, &param) {
                    *value = cast_named(name.as_str(), var(&param));
                }
            }
        }
    }
}

/// The name => spec kind symbol of the model's `schema_column_types`.
fn spec_symbols(lc: &LibraryClass) -> Vec<(String, String)> {
    let Some(m) = lc.methods.iter().find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "schema_column_types")
    else {
        return Vec::new();
    };
    let ExprNode::Hash { entries, .. } = &*m.body.node else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(k, v)| {
            let ExprNode::Lit { value: Literal::Str { value: name } } = &*k.node else { return None };
            let ExprNode::Array { elements, .. } = &*v.node else { return None };
            let ExprNode::Lit { value: Literal::Sym { value } } = &*elements.first()?.node else { return None };
            Some((name.clone(), value.as_str().to_string()))
        })
        .collect()
}

/// Rails' new record has no id (`Owner.new.id` is nil): the `id`
/// reader answers nil while the slot holds the unsaved sentinel, so
/// equality (`!id.nil? && other.id == id`), `hash`, and the key a
/// belongs_to writer takes from an unsaved target (`pet.owner =
/// Owner.new`) read nil as Rails'. The slot keeps the sentinel the
/// shared lowering and every target's guards test. A new record given
/// id 0 reads nil, where Rails reads 0.
fn nil_unsaved_id(lc: &mut LibraryClass, model: &Model, table: &Table) {
    let Some(col) = table.columns.iter().find(|c| c.primary_key && c.name.as_str() == "id") else {
        return;
    };
    if is_retypable(model, col) {
        return;
    }
    let slot = col_storage_name(col);
    for m in lc.methods.iter_mut() {
        if m.receiver != MethodReceiver::Instance || m.name.as_str() != "id" {
            continue;
        }
        if !matches!(&*m.body.node, ExprNode::Ivar { name } if *name == slot) {
            continue;
        }
        let nilable = |t: Ty| Ty::Union { variants: vec![t, Ty::Nil].into() };
        let ty = m.body.ty.clone().map(nilable);
        m.body = id_value(col);
        m.body.ty = ty;
        if let Some(Ty::Fn { ret, .. }) = &mut m.signature {
            *ret = std::sync::Arc::new(nilable((**ret).clone()));
        }
    }
}

/// Rails' `to_param` is the key joined, nil for a new record (`to_key`
/// is nil without an id); the lowering's reads the slot (`@id.to_s`,
/// the sentinel's "0"). It reads the `id` reader instead.
fn nil_unsaved_param(lc: &mut LibraryClass) {
    for m in lc.methods.iter_mut() {
        if m.receiver != MethodReceiver::Instance || m.name.as_str() != "to_param" {
            continue;
        }
        let reads_slot = matches!(&*m.body.node, ExprNode::Send { recv: Some(r), method, args, .. }
            if method.as_str() == "to_s" && args.is_empty() && matches!(&*r.node, ExprNode::Ivar { name } if name.as_str() == "id"));
        if !reads_slot {
            continue;
        }
        let id = || send(self_ref(), "id", vec![]);
        let ty = m.body.ty.clone().map(|t| Ty::Union { variants: vec![t, Ty::Nil].into() });
        m.body = sp(ExprNode::If {
            cond: send(id(), "nil?", vec![]),
            then_branch: lit(Literal::Nil),
            else_branch: send(id(), "to_s", vec![]),
        });
        m.body.ty = ty;
    }
}

/// The column name => cast kind of the model's `schema_column_types`.
fn spec_kinds(lc: &LibraryClass) -> Vec<(String, Option<Kind>)> {
    let Some(m) = lc.methods.iter().find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "schema_column_types")
    else {
        return Vec::new();
    };
    let ExprNode::Hash { entries, .. } = &*m.body.node else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(k, v)| {
            let ExprNode::Lit { value: Literal::Str { value: name } } = &*k.node else { return None };
            let ExprNode::Array { elements, .. } = &*v.node else { return None };
            let kind = match elements.first().map(|e| &*e.node) {
                Some(ExprNode::Lit { value: Literal::Sym { value } }) => match value.as_str() {
                    "integer" | "big_integer" => Some(Kind::Integer),
                    "float" => Some(Kind::Float),
                    "boolean" => Some(Kind::Boolean),
                    "string" | "text" | "immutable_string" => Some(Kind::Text),
                    "decimal" | "uuid" => Some(Kind::Always),
                    "date" => Some(Kind::Date),
                    "datetime" | "timestamp" | "time" => Some(Kind::Time),
                    _ => None,
                },
                _ => None,
            };
            Some((name.clone(), kind))
        })
        .collect()
}

fn is_temporal(col: &Column) -> bool {
    matches!(col.col_type, ColumnType::Date | ColumnType::DateTime | ColumnType::Time)
}

/// The kind the column's own slot holds when hydrated: a declared type
/// over another column (`attribute :legacy, :integer` on a varchar)
/// differs from it.
fn stored_kind(col: &Column) -> Option<Kind> {
    match col.col_type {
        ColumnType::Integer | ColumnType::BigInt | ColumnType::Reference { .. } => Some(Kind::Integer),
        ColumnType::Float => Some(Kind::Float),
        ColumnType::Boolean => Some(Kind::Boolean),
        ColumnType::String { .. } | ColumnType::Text => Some(Kind::Text),
        _ => None,
    }
}

fn sp(node: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), node)
}

fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    let parenthesized = !args.is_empty();
    sp(ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args, block: None, parenthesized })
}

fn self_ref() -> Expr {
    sp(ExprNode::SelfRef)
}

fn var(name: &Symbol) -> Expr {
    sp(ExprNode::Var { id: crate::ident::VarId(0), name: name.clone() })
}

fn konst(name: &str) -> Expr {
    sp(ExprNode::Const { path: vec![Symbol::from(name)] })
}

fn lit(value: Literal) -> Expr {
    sp(ExprNode::Lit { value })
}

fn or(left: Expr, right: Expr) -> Expr {
    let mut e = sp(ExprNode::BoolOp { op: BoolOpKind::Or, surface: BoolOpSurface::Symbol, left, right });
    e.ty = Some(Ty::Bool);
    e
}

/// `self.class._cast_column("<col>", value)`.
fn cast_call(col: &Column, value: Expr) -> Expr {
    cast_named(col.name.as_str(), value)
}

fn cast_named(name: &str, value: Expr) -> Expr {
    let mut e = send(
        send(self_ref(), "class", vec![]),
        "_cast_column",
        vec![lit(Literal::Str { value: name.to_string() }), value],
    );
    e.ty = Some(Ty::Untyped);
    e
}

/// `value.nil? || <passes> ? value : self.class._cast_column("<col>", value)`.
fn guarded_cast(col: &Column, kind: Kind, name: &Symbol) -> Expr {
    let is_a = |class: &str| send(var(name), "is_a?", vec![konst(class)]);
    let nil_q = send(var(name), "nil?", vec![]);
    let passes = match kind {
        Kind::Integer => Some(is_a("Integer")),
        Kind::Float => Some(is_a("Float")),
        Kind::Text => Some(is_a("String")),
        Kind::Date => Some(is_a("Date")),
        Kind::Time => Some(is_a("Time")),
        Kind::Boolean => Some(or(
            send(var(name), "==", vec![lit(Literal::Bool { value: true })]),
            send(var(name), "==", vec![lit(Literal::Bool { value: false })]),
        )),
        Kind::Always => None,
    };
    let mut cast = cast_call(col, var(name));
    let cond = match passes {
        // Active Model's String type casts a String through `::String.new(value)`
        // (type/string.rb `cast_value`): the attribute owns a mutable copy, so
        // `record.name.downcase!` neither raises on a frozen literal nor warns
        // on a chilled one, and never mutates the caller's string.
        Some(p) if kind == Kind::Text => {
            let mut copy = sp(ExprNode::If { cond: p, then_branch: send(var(name), "dup", vec![]), else_branch: cast });
            copy.ty = Some(Ty::Untyped);
            cast = copy;
            nil_q
        }
        Some(p) => or(nil_q, p),
        None => nil_q,
    };
    let mut e = sp(ExprNode::If { cond, then_branch: var(name), else_branch: cast });
    e.ty = Some(Ty::Untyped);
    e
}

fn is_var(e: &Expr, name: &Symbol) -> bool {
    matches!(&*e.node, ExprNode::Var { name: n, .. } if n == name)
}

fn setter_col<'a>(method: &str, cols: &[(&'a Column, Kind)]) -> Option<(&'a Column, Kind)> {
    let name = method.strip_suffix('=')?;
    cols.iter().find(|(c, _)| col_storage_name(c).as_str() == name).copied()
}

fn cast_writes(lc: &mut LibraryClass, cols: &[(&Column, Kind)]) {
    let time_reads: Vec<(Symbol, Expr)> = cols
        .iter()
        .filter(|(_, k)| *k == Kind::Time)
        .map(|(c, _)| (col_storage_name(c), temporal_value(lc, c)))
        .collect();
    for m in lc.methods.iter_mut().filter(|m| m.name_span.is_synthetic()) {
        match (m.receiver, m.name.as_str()) {
            (MethodReceiver::Instance, "[]=") => cast_index_write(m, cols),
            (MethodReceiver::Instance, "[]") => read_index_uncast(m, cols, &time_reads),
            (MethodReceiver::Instance, "initialize" | "update" | "update!") => hand_to_writers(&mut m.body, cols),
            (MethodReceiver::Instance, "_adapter_reload") => cast_reload(&mut m.body, cols),
            (MethodReceiver::Class, "from_row" | "from_stmt") => {
                hand_to_writers(&mut m.body, cols);
                hydrate_decimals(&mut m.body, cols);
            }
            (MethodReceiver::Instance, name) => {
                let Some(n) = name.strip_suffix('=') else { continue };
                if let Some(&(col, kind)) = cols.iter().find(|(c, _)| c.name.as_str() == n) {
                    cast_writer(m, col, kind);
                }
            }
            _ => {}
        }
    }
}

/// The public writer: `@<col> = value` (or a `normalizes` call over
/// it) casts `value`; a temporal one casts what it formats. A foreign
/// key's writer does so after its association bookkeeping.
fn cast_writer(m: &mut MethodDef, col: &Column, kind: Kind) {
    let Some(param) = m.params.first().map(|p| p.name.clone()) else { return };
    cast_writer_body(&mut m.body, col, kind, &param);
}

fn cast_writer_body(e: &mut Expr, col: &Column, kind: Kind, param: &Symbol) {
    match &mut *e.node {
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| cast_writer_body(x, col, kind, param)),
        ExprNode::Assign { target: LValue::Ivar { name }, value } if *name == col_storage_name(col) => {
            if is_var(value, param) {
                *value = guarded_cast(col, kind, param);
            } else if let ExprNode::Send { args, .. } = &mut *value.node {
                for a in args.iter_mut().filter(|a| is_var(a, param)) {
                    *a = guarded_cast(col, kind, param);
                }
            }
        }
        ExprNode::Assign { target: LValue::Attr { name, .. }, value } if *name == col_storage_name(col) => {
            cast_formatted(value, col, Some((kind, param)));
        }
        _ => {}
    }
}

/// `ActiveSupport.format_db_<date|time>(x)` → the same over the cast of `x`.
fn cast_formatted(value: &mut Expr, col: &Column, guard: Option<(Kind, &Symbol)>) {
    let format = temporal_seam(col).2;
    let ExprNode::Send { method, args, .. } = &mut *value.node else { return };
    if method.as_str() != format || args.len() != 1 {
        return;
    }
    let arg = &mut args[0];
    if matches!(&*arg.node, ExprNode::Send { method, .. } if method.as_str() == "_cast_column")
        || matches!(&*arg.node, ExprNode::If { .. })
    {
        return;
    }
    *arg = match guard {
        Some((kind, param)) if is_var(arg, param) => guarded_cast(col, kind, param),
        _ => cast_call(col, arg.clone()),
    };
}

/// `self.<col> = <cast of x>` → `self.<col> = x`, the writer casting; a
/// temporal column's `self.<col>_raw = format(x)` formats the cast of x.
fn hand_to_writers(e: &mut Expr, cols: &[(&Column, Kind)]) {
    match &mut *e.node {
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| hand_to_writers(x, cols)),
        ExprNode::If { cond, then_branch, else_branch } => {
            hand_to_writers(cond, cols);
            hand_to_writers(then_branch, cols);
            hand_to_writers(else_branch, cols);
        }
        ExprNode::Send { recv: Some(_), method, args, .. } if args.len() == 1 => {
            let Some((col, _)) = setter_col(method.as_str(), cols) else { return };
            if is_temporal(col) {
                cast_formatted(&mut args[0], col, None);
            } else if let ExprNode::Cast { value, .. } = &*args[0].node {
                args[0] = value.clone();
            }
        }
        ExprNode::Assign { target: LValue::Attr { name, .. }, value } => {
            let Some((col, _)) = setter_col(&format!("{}=", name.as_str()), cols) else { return };
            if is_temporal(col) {
                cast_formatted(value, col, None);
            } else if let ExprNode::Cast { value: inner, .. } = &*value.node {
                *value = inner.clone();
            }
        }
        _ => {}
    }
}

/// `from_raw`'s slot writes from the adapter row: a decimal column's
/// `row["c"]` as the driver decoded it (a BigDecimal, which the model's
/// writer casts), not `to_f`; a timestamp's as its stored text,
/// `ActiveSupport.format_db_time(row["c"])`, not `Time#to_s`, which
/// drops the microseconds.
fn raw_row_values(lc: &mut LibraryClass, cols: &[(&Column, Kind)]) {
    for m in lc.methods.iter_mut().filter(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "from_raw") {
        let Some(row) = m.params.first().map(|p| p.name.clone()) else { continue };
        let ExprNode::Seq { exprs } = &mut *m.body.node else { continue };
        for stmt in exprs.iter_mut() {
            let (setter, value) = match &mut *stmt.node {
                ExprNode::Send { method, args, .. } if args.len() == 1 => (method.as_str().to_string(), &mut args[0]),
                ExprNode::Assign { target: LValue::Attr { name, .. }, value } => (format!("{}=", name.as_str()), value),
                _ => continue,
            };
            let Some(&(col, kind)) = cols.iter().find(|(c, _)| setter.strip_suffix('=') == Some(c.name.as_str())) else {
                continue;
            };
            let lookup = sp(ExprNode::Send {
                recv: Some(var(&row)),
                method: Symbol::from("[]"),
                args: vec![lit(Literal::Str { value: col.name.as_str().to_string() })],
                block: None,
                parenthesized: false,
            });
            match (&col.col_type, kind) {
                (ColumnType::Decimal { .. }, _) => *value = lookup,
                (_, Kind::Time) => {
                    let mut text = send(konst("ActiveSupport"), "format_db_time", vec![lookup]);
                    text.ty = Some(Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() });
                    *value = text;
                }
                _ => {}
            }
        }
    }
}

/// `Db.column_float[_opt](h, i)` for a decimal column → `Db.column_decimal[_opt](h, i)`.
fn hydrate_decimals(e: &mut Expr, cols: &[(&Column, Kind)]) {
    let decimal = |name: &str| {
        cols.iter().any(|(c, _)| c.name.as_str() == name && matches!(c.col_type, ColumnType::Decimal { .. }))
    };
    match &mut *e.node {
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| hydrate_decimals(x, cols)),
        ExprNode::Send { recv: Some(_), method, args, .. } if args.len() == 1 => {
            if method.as_str().strip_suffix('=').is_some_and(decimal) {
                decimal_read(&mut args[0]);
            }
        }
        ExprNode::Assign { target: LValue::Ivar { name }, value } if decimal(name.as_str()) => decimal_read(value),
        _ => {}
    }
}

fn decimal_read(e: &mut Expr) {
    if let ExprNode::Send { method, .. } = &mut *e.node {
        match method.as_str() {
            "column_float" => *method = Symbol::from("column_decimal"),
            "column_float_opt" => *method = Symbol::from("column_decimal_opt"),
            _ => {}
        }
    }
}

/// `reload`'s slot writes: a decimal reads the driver's value, and a
/// value whose stored class is not the type's is cast.
fn cast_reload(e: &mut Expr, cols: &[(&Column, Kind)]) {
    match &mut *e.node {
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| cast_reload(x, cols)),
        ExprNode::If { then_branch, else_branch, .. } => {
            cast_reload(then_branch, cols);
            cast_reload(else_branch, cols);
        }
        ExprNode::BeginRescue { body, ensure, .. } => {
            cast_reload(body, cols);
            if let Some(ensure) = ensure {
                cast_reload(ensure, cols);
            }
        }
        ExprNode::Assign { target: LValue::Ivar { name }, value } => {
            let Some((col, kind)) = cols.iter().find(|(c, _)| !is_temporal(c) && c.name == *name).copied() else {
                return;
            };
            if matches!(col.col_type, ColumnType::Decimal { .. }) {
                decimal_read(value);
            }
            if kind == Kind::Always || stored_kind(col) != Some(kind) {
                *value = cast_call(col, value.clone());
            }
        }
        _ => {}
    }
}

/// `[]=`'s arms: `@<col> = <pre-cast of value>` casts `value` as the
/// writer does; a temporal arm stores the formatted cast through the
/// raw writer, which also drops the parsed value it memoized.
fn cast_index_write(m: &mut MethodDef, cols: &[(&Column, Kind)]) {
    let Some(param) = m.params.get(1).map(|p| p.name.clone()) else { return };
    for_each_name_arm(&mut m.body, &mut |arm| {
        let ExprNode::Assign { target: LValue::Ivar { name }, .. } = &*arm.body.node else { return };
        let Some((col, kind)) = cols.iter().find(|(c, _)| col_storage_name(c) == *name).copied() else { return };
        if is_temporal(col) {
            let format = send(konst("ActiveSupport"), temporal_seam(col).2, vec![guarded_cast(col, kind, &param)]);
            arm.body = sp(ExprNode::Assign {
                target: LValue::Attr { recv: self_ref(), name: col_storage_name(col) },
                value: format,
            });
        } else if let ExprNode::Assign { value, .. } = &mut *arm.body.node {
            *value = guarded_cast(col, kind, &param);
        }
    });
}

/// `[]`'s arms: the slot holds the cast value, so a pre-cast of it
/// (`@price&.to_f`, `@legacy&.to_s`) reads it as it is. A boolean keeps
/// its arm, which also reads an SQLite 0/1 that `reload` stored. A
/// timestamp's arm, its stored text (`@<col>_raw&.to_s`), reads the
/// Time its generated reader parses, as Rails' `read_attribute` does.
fn read_index_uncast(m: &mut MethodDef, cols: &[(&Column, Kind)], time_reads: &[(Symbol, Expr)]) {
    for_each_name_arm(&mut m.body, &mut |arm| {
        let ExprNode::Cast { value, .. } = &*arm.body.node else { return };
        let ExprNode::Ivar { name } = &*value.node else { return };
        if let Some((_, read)) = time_reads.iter().find(|(raw, _)| raw == name) {
            arm.body = read.clone();
        } else if cols.iter().any(|(c, k)| !is_temporal(c) && *k != Kind::Boolean && c.name == *name) {
            arm.body = value.clone();
        }
    });
}

fn for_each_name_arm(e: &mut Expr, f: &mut dyn FnMut(&mut Arm)) {
    match &mut *e.node {
        ExprNode::Case { arms, .. } => arms.iter_mut().for_each(|a| f(a)),
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| for_each_name_arm(x, f)),
        _ => {}
    }
}

/// Renames the lowering's snapshot to `_attribute_snapshot` and adds
/// Rails' `attributes` beside it. A model that defines `attributes`
/// itself keeps its own (the runtime's snapshot then reads it).
fn public_attributes(lc: &mut LibraryClass, model: &Model, table: &Table) {
    let Some(at) = lc
        .methods
        .iter()
        .position(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == "attributes" && m.name_span.is_synthetic())
    else {
        return;
    };
    let ExprNode::Hash { entries, .. } = &*lc.methods[at].body.node else {
        return;
    };
    let snapshot = entries.clone();
    let mut public: Vec<(Expr, Expr)> = Vec::new();
    for col in &table.columns {
        let key = col.name.as_str();
        let value = if col.primary_key && !is_retypable(model, col) {
            id_value(col)
        } else if is_temporal(col) {
            temporal_value(lc, col)
        } else {
            snapshot
                .iter()
                .find(|(k, _)| matches!(&*k.node, ExprNode::Lit { value: Literal::Str { value } } if value == key))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| sp(ExprNode::Ivar { name: col_storage_name(col) }))
        };
        public.push((lit(Literal::Str { value: key.to_string() }), value));
    }
    let mut method = lc.methods[at].clone();
    lc.methods[at].name = Symbol::from("_attribute_snapshot");
    // The baseline dirty tracking compares: stored text that is not JSON
    // stays that text (Rails decodes lazily, so only the reader raises).
    if let ExprNode::Hash { entries, .. } = &mut *lc.methods[at].body.node {
        for (_, value) in entries.iter_mut() {
            if let ExprNode::Send { recv: Some(recv), method, .. } = &mut *value.node
                && method.as_str() == "load"
                && matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "RoundhouseJsonColumn")
            {
                *method = Symbol::from("snapshot");
            }
        }
    }
    let ty = method.body.ty.clone();
    method.body = sp(ExprNode::Hash { entries: public, kwargs: false });
    method.body.ty = ty;
    lc.methods.insert(at + 1, method);
}

/// The key slot, nil while it holds the unsaved sentinel of a new
/// record (`0`, or `""` for a String key; base.rb's `initialize`).
fn id_value(col: &Column) -> Expr {
    let slot = || sp(ExprNode::Ivar { name: col_storage_name(col) });
    let sentinel = match crate::lower::model_to_library::ty_of_column_slot(col) {
        Ty::Int => lit(Literal::Int { value: 0 }),
        Ty::Str => lit(Literal::Str { value: String::new() }),
        _ => return slot(),
    };
    let unsaved = sp(ExprNode::BoolOp {
        op: BoolOpKind::And,
        surface: BoolOpSurface::Symbol,
        left: send(slot(), "==", vec![sentinel]),
        right: send(self_ref(), "new_record?", vec![]),
    });
    sp(ExprNode::If { cond: unsaved, then_branch: lit(Literal::Nil), else_branch: slot() })
}

/// A temporal column's Date or Time: its compiled reader's body where
/// the model has one, else the parse of its stored text.
fn temporal_value(lc: &LibraryClass, col: &Column) -> Expr {
    lc.methods
        .iter()
        .find(|m| {
            m.receiver == MethodReceiver::Instance && m.name == col.name && m.name_span.is_synthetic() && m.params.is_empty()
        })
        .map(|m| m.body.clone())
        .unwrap_or_else(|| {
            send(konst("ActiveSupport"), temporal_seam(col).1, vec![sp(ExprNode::Ivar { name: col_storage_name(col) })])
        })
}
