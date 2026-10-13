//! Rails' composite primary key (`Model::composite_primary_key`): a model
//! on a table whose primary key spans several columns, declaring no
//! `self.primary_key`, keys its records on all of them
//! (attribute_methods/composite_primary_key.rb, persistence.rb).
//!
//! - `Model.primary_key` is the column list and
//!   `composite_primary_key?` is true.
//! - `record.id` is the tuple of the key columns' values and `id=` takes
//!   one (a non-Enumerable raises TypeError); the `id` column itself is
//!   `id_value` / `id_value=`, and `record[:id]`, `attributes["id"]` and
//!   `where(id:)` keep naming the column.
//! - update, destroy, reload and `first`/`last` key on every column:
//!   the list is the model's `query_constraints` (ingest), so the
//!   compiled adapter methods carry the composite WHERE and ORDER BY.
//! - `find([shop_id, id, island])` and `find([[..], [..]])` and `ids`
//!   are the runtime's (`_find_composite_primary_key`, `Relation#ids`).
//!
//! [`rekey`] runs on the synthesized methods before the model's own;
//! [`rekey_identity`] on the identity methods synthesized after them.

use crate::dialect::{MethodDef, MethodReceiver, Model};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::Symbol;
use crate::schema::{Column, Table};
use crate::span::Span;
use crate::ty::Ty;

use super::fn_sig;
use super::schema::col_storage_name;

/// The `id` column's accessors become `id_value`/`id_value=`, and the
/// synthesized writes through `id=` (hydration, the constructor) follow
/// them; `id`/`id=` become the tuple; the class answers the column list.
pub(super) fn rekey(methods: &mut Vec<MethodDef>, model: &Model, table: &Table) {
    let columns: Vec<&Column> =
        model.composite_primary_key.iter().filter_map(|n| table.columns.iter().find(|c| &c.name == n)).collect();
    if columns.is_empty() || columns.len() != model.composite_primary_key.len() {
        return;
    }
    let has_id_column = table.columns.iter().any(|c| c.name.as_str() == "id");
    if has_id_column {
        for m in methods.iter_mut() {
            if m.receiver == MethodReceiver::Instance {
                match m.name.as_str() {
                    "id" => m.name = Symbol::from("id_value"),
                    "id=" => m.name = Symbol::from("id_value="),
                    _ => {}
                }
            }
            rename_sends(&mut m.body, "id=", "id_value=");
            if m.name.as_str() == "initialize" && m.receiver == MethodReceiver::Instance {
                // `new(id_value: 10)` sets the column; `new(id: [..])`
                // goes through the tuple writer once the columns are set.
                rename_symbols(&mut m.body, "id", "id_value");
            }
        }
    }
    if let Some(init) =
        methods.iter_mut().find(|m| m.name.as_str() == "initialize" && m.receiver == MethodReceiver::Instance)
    {
        let given = parse_body("if attrs.key?(:id)\n  self.id = attrs[:id]\nend\n");
        if let ExprNode::Seq { exprs } = &mut *init.body.node {
            // Before an `after_initialize` tail, which observes the assigned attributes.
            let at = exprs.iter().rposition(|e| !fires_after_initialize(e)).map_or(exprs.len(), |i| i + 1);
            exprs.insert(at, given);
        }
    }
    // The insert answers the tuple, which `save` assigns through `id=`.
    if let Some(insert) =
        methods.iter_mut().find(|m| m.name.as_str() == "_adapter_insert" && m.receiver == MethodReceiver::Instance)
    {
        let key = key_storage(&columns);
        match &mut *insert.body.node {
            // A supplied key: the INSERT writes it and answers the ivar.
            ExprNode::Seq { exprs } if matches!(exprs.last().map(|e| &*e.node), Some(ExprNode::Ivar { name }) if *name == key) => {
                *exprs.last_mut().unwrap() = parse_body("id\n");
            }
            // A generated one comes back from the INSERT.
            _ => {
                let old = std::mem::replace(&mut insert.body, Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }));
                let assign = Expr::new(Span::synthetic(), ExprNode::Assign { target: LValue::Ivar { name: key }, value: old });
                insert.body = Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![assign, parse_body("id\n")] });
            }
        }
        insert.signature = Some(fn_sig(vec![], Ty::Untyped));
    }
    // Rails' `attributes` includes the `id` column.
    if has_id_column {
        if let Some(attrs) =
            methods.iter_mut().find(|m| m.name.as_str() == "attributes" && m.receiver == MethodReceiver::Instance)
        {
            if let ExprNode::Hash { entries, .. } = &mut *attrs.body.node {
                let before: Vec<&str> =
                    table.columns.iter().take_while(|c| c.name.as_str() != "id").map(|c| c.name.as_str()).collect();
                let at = entries
                    .iter()
                    .rposition(|(k, _)| matches!(&*k.node, ExprNode::Lit { value: Literal::Str { value } } if before.contains(&value.as_str())))
                    .map_or(0, |i| i + 1);
                let id = table.columns.iter().find(|c| c.name.as_str() == "id").unwrap();
                let value = super::with_ty(
                    Expr::new(Span::synthetic(), ExprNode::Ivar { name: col_storage_name(id) }),
                    super::ty_of_column_slot(id),
                );
                entries.insert(at, (super::lit_str("id".to_string()), value));
            }
        }
    }

    let names = columns.iter().map(|c| format!("{:?}", c.name.as_str())).collect::<Vec<_>>().join(", ");
    // The key columns' public accessors; the `id` column's is `id_value`.
    let accessor = |c: &Column| if c.name.as_str() == "id" { "id_value".to_string() } else { c.name.as_str().to_string() };
    let readers = columns.iter().map(|c| accessor(c)).collect::<Vec<_>>().join(", ");
    let writes = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("  self.{} = values[{i}]\n", accessor(c)))
        .collect::<String>();
    let source = format!(
        "def id\n  [{readers}]\nend\n\
         def id=(value)\n  raise TypeError, \"Expected value matching #{{self.class.primary_key.inspect}}, got #{{value.inspect}}.\" unless value.is_a?(Enumerable)\n  values = value.to_a\n{writes}  value\nend\n\
         def self.primary_key\n  [{names}]\nend\n\
         def self.composite_primary_key?\n  true\nend\n"
    );
    let strs = Ty::Array { elem: std::sync::Arc::new(Ty::Str) };
    for mut m in crate::runtime_src::parse_methods(&source).expect("composite key methods parse") {
        m.enclosing_class = Some(model.name.0.clone());
        let value = Symbol::from("value");
        m.signature = Some(match (m.receiver, m.name.as_str()) {
            (MethodReceiver::Instance, "id") => fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }),
            (MethodReceiver::Instance, _) => fn_sig(vec![(value, Ty::Untyped)], Ty::Untyped),
            (MethodReceiver::Class, "composite_primary_key?") => fn_sig(vec![], Ty::Bool),
            (MethodReceiver::Class, _) => fn_sig(vec![], strs.clone()),
        });
        methods.push(m);
    }
}

/// `to_param`, `dom_record_key` and `cache_key` read the key slot
/// (`@id`); Rails' read the tuple: `to_param` and the dom key join it
/// with `_` (`Array(id).join("_")`, `to_key.join("_")`), and `cache_key`
/// interpolates it.
pub(super) fn rekey_identity(methods: &mut [MethodDef], model: &Model) {
    if model.composite_primary_key.is_empty() {
        return;
    }
    for m in methods.iter_mut() {
        if m.receiver != MethodReceiver::Instance || m.name_span != Span::synthetic() {
            continue;
        }
        match m.name.as_str() {
            "to_param" | "dom_record_key" if reads_id_to_s(&m.body) => {
                m.body = super::with_ty(parse_body("id.join(\"_\")\n"), Ty::Str);
            }
            "cache_key" | "cache_key_with_version" => replace_id_ivar(&mut m.body),
            _ => {}
        }
    }
}

/// The storage ivar the insert's key value lands in: the `id` column
/// when it is part of the key, else the key's first column.
fn key_storage(columns: &[&Column]) -> Symbol {
    let key = columns.iter().find(|c| c.name.as_str() == "id").unwrap_or(&columns[0]);
    col_storage_name(key)
}

fn parse_body(source: &str) -> Expr {
    let wrapped = format!("def __composite_key\n{source}end\n");
    let mut methods = crate::runtime_src::parse_methods(&wrapped).expect("composite key body parses");
    methods.remove(0).body
}

fn fires_after_initialize(expr: &Expr) -> bool {
    let mut found = false;
    visit(expr, &mut |e| {
        if let ExprNode::Send { method, .. } = &*e.node {
            found |= method.as_str() == "after_initialize";
        }
    });
    found
}

fn reads_id_to_s(body: &Expr) -> bool {
    matches!(&*body.node, ExprNode::Send { recv: Some(r), method, .. }
        if method.as_str() == "to_s" && matches!(&*r.node, ExprNode::Ivar { name } if name.as_str() == "id"))
}

fn visit(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    expr.node.for_each_child(&mut |c| visit(c, f));
}

fn rename_sends(expr: &mut Expr, from: &str, to: &str) {
    if let ExprNode::Send { method, .. } = &mut *expr.node {
        if method.as_str() == from {
            *method = Symbol::from(to);
        }
    }
    expr.node.for_each_child_mut(&mut |c| rename_sends(c, from, to));
}

fn rename_symbols(expr: &mut Expr, from: &str, to: &str) {
    if let ExprNode::Lit { value: Literal::Sym { value } } = &mut *expr.node {
        if value.as_str() == from {
            *value = Symbol::from(to);
        }
    }
    expr.node.for_each_child_mut(&mut |c| rename_symbols(c, from, to));
}

fn replace_id_ivar(expr: &mut Expr) {
    if matches!(&*expr.node, ExprNode::Ivar { name } if name.as_str() == "id") {
        *expr = parse_body("id\n");
        return;
    }
    expr.node.for_each_child_mut(&mut |c| replace_id_ivar(c));
}
