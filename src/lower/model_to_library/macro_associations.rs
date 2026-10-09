//! The class side of an association a class macro declares (Core's
//! `accelerated_belongs_to :shop`, which runs `belongs_to(record, *args,
//! **kwargs)`; `ingest::macro_associations` lowers the association from
//! the macro call). The call still replays at class load
//! (`macro_replay`), and the `belongs_to` and `reflect_on_association`
//! it makes there are answered from the compiled association: the class
//! gets `self.belongs_to`, which answers the reflection of an
//! association it compiled, with the options the macro passes, and
//! raises for any other, and `self.reflect_on_association`, which
//! answers a `ActiveRecord::Reflection::BelongsToReflection` for each
//! such association (and defers to the superclass for the rest). On the
//! PostgreSQL CRuby tree that method answers every association the model
//! compiled instead (`emit::ruby::library::apply_association_reflections`).
//! The
//! association's reader goes in GeneratedAttributeMethods
//! (`emit::ruby::library::generated_attribute_methods`), as Rails defines
//! it in a generated module, so the reader a macro's `class_eval` writes
//! supers into it.

use crate::dialect::{AccessorKind, Association, MethodDef, MethodReceiver, MethodVisibility, Model, ModelBodyItem, Param};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{Symbol, VarId};
use crate::span::Span;

/// The body calls whose `belongs_to` the model compiled: an Association
/// item shares the macro call's span.
pub(crate) fn declaring_calls(model: &Model) -> Vec<Span> {
    declared(model).into_iter().map(|(span, _)| span).collect()
}

/// The `belongs_to` associations a class macro declares, with the call's
/// span. The call is an `Unknown` statement until `replay_model_macros`
/// makes it a `ClassBody` one.
fn declared(model: &Model) -> Vec<(Span, &Association)> {
    let calls: Vec<Span> = model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => Some(expr.span),
            _ => None,
        })
        .collect();
    model
        .body
        .iter()
        .filter_map(|item| match item {
            ModelBodyItem::Association { assoc: assoc @ Association::BelongsTo { .. }, span, .. }
                if !span.is_synthetic() && calls.contains(span) =>
            {
                Some((*span, assoc))
            }
            _ => None,
        })
        .collect()
}

/// The readers of those associations, which GeneratedAttributeMethods
/// carries.
pub(crate) fn declared_readers(model: &Model) -> Vec<Symbol> {
    declared(model).into_iter().map(|(_, assoc)| assoc.name().clone()).collect()
}

/// `self.reflect_on_association(name)` and `self.belongs_to(name, *args,
/// **options)` for a model whose class macros declare associations.
pub(super) fn push_reflection_methods(methods: &mut Vec<MethodDef>, model: &Model, models: &[Model]) {
    let declared = declared(model);
    if declared.is_empty() {
        return;
    }
    let name = Symbol::from("name");
    let name_ref = || Expr::new(Span::synthetic(), ExprNode::Var { id: VarId(0), name: name.clone() });
    // An ancestor's reflections, for the names this class does not declare.
    let inherited = ancestors(model, models).any(|m| !self::declared(m).is_empty());
    let mut answer = if inherited {
        Expr::new(Span::synthetic(), ExprNode::Super { args: None, block: None })
    } else {
        lit(Literal::Nil)
    };
    for (_, assoc) in declared.iter().rev() {
        let Association::BelongsTo { name: assoc_name, target, foreign_key, foreign_type, primary_key, polymorphic, .. } = assoc
        else {
            continue;
        };
        let foreign_type = match (polymorphic, foreign_type) {
            (true, Some(t)) => str_lit(t.as_str()),
            (true, None) => str_lit(&format!("{}_type", assoc_name.as_str())),
            (false, _) => lit(Literal::Nil),
        };
        let reflection = send(
            Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const {
                    path: ["ActiveRecord", "Reflection", "BelongsToReflection"].into_iter().map(Symbol::from).collect(),
                },
            )),
            "new",
            vec![
                lit(Literal::Sym { value: assoc_name.clone() }),
                str_lit(foreign_key.as_str()),
                if *polymorphic { lit(Literal::Nil) } else { str_lit(target.0.as_str()) },
                foreign_type,
                primary_key.as_ref().map_or_else(|| lit(Literal::Nil), |k| str_lit(k.as_str())),
            ],
        );
        let cond = send(
            Some(send(Some(name_ref()), "to_s", Vec::new())),
            "==",
            vec![str_lit(assoc_name.as_str())],
        );
        answer = Expr::new(Span::synthetic(), ExprNode::If { cond, then_branch: reflection, else_branch: answer });
    }
    let param = Param::positional(name.clone());
    if !methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "reflect_on_association") {
        methods.push(class_method(model, "reflect_on_association", vec![param.clone()], answer));
    }
    // `belongs_to :shop, ...` at class-body run time: the compiled
    // association's reflection, which takes the options the macro
    // passes (the macro may add its own: Core's
    // `accelerated_readonly_belongs_to` merges `autosave: false`), or an
    // error naming one never compiled.
    if !methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "belongs_to") {
        // `runtime_src` reads required positionals only: the params are
        // written plain and given their kinds below.
        let source = "def self.belongs_to(name, _args, options)\n  \
             reflection = reflect_on_association(name)\n  \
             if reflection.nil?\n    \
             raise NotImplementedError, \"#{self.name}.belongs_to :#{name} at class-body run time: the compiler lowers \
             only the associations a class macro declares from its call\"\n  \
             end\n  \
             reflection._roundhouse_declared(options)\n\
             end\n";
        let parsed = crate::runtime_src::parse_methods(source)
            .unwrap_or_else(|e| panic!("macro_associations: generated `belongs_to` failed to parse: {e}\n{source}"));
        for mut m in parsed {
            m.enclosing_class = Some(model.name.0.clone());
            m.name_span = Span::synthetic();
            if let [_, args, options] = m.params.as_mut_slice() {
                args.rest = true; // *_args
                options.keyword = true; // **options
                options.rest = true;
            }
            methods.push(m);
        }
    }
}

/// The model's app model ancestors, nearest first.
fn ancestors<'a>(model: &'a Model, models: &'a [Model]) -> impl Iterator<Item = &'a Model> {
    let mut seen = vec![model.name.clone()];
    let mut next = model.parent.clone();
    std::iter::from_fn(move || {
        let parent = models.iter().find(|m| Some(&m.name) == next.as_ref())?;
        if seen.contains(&parent.name) {
            return None;
        }
        seen.push(parent.name.clone());
        next = parent.parent.clone();
        Some(parent)
    })
}

fn lit(value: Literal) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Lit { value })
}

fn str_lit(value: &str) -> Expr {
    lit(Literal::Str { value: value.to_string() })
}

fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized: false },
    )
}

fn class_method(model: &Model, name: &str, params: Vec<Param>, body: Expr) -> MethodDef {
    MethodDef {
        visibility: MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Class,
        params,
        body,
        signature: None,
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}
