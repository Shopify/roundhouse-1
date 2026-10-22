//! A class method an app writes to define methods, expanded at each
//! class-body call into the methods it defines.
//!
//! forem's `Settings::Base.setting` (adapted from rails-settings-cached)
//! is the shape: a subclass writes `setting :community_name, type:
//! :string, default: …`, the inherited class method hands its arguments
//! to a private `define_setting`, and that one calls
//! `define_singleton_method(key) { … }` for a reader and writers whose
//! bodies read the captured arguments. Nothing in the source declares
//! `Settings::Community.community_name`; Ruby makes it when the class
//! body runs.
//!
//! The macro's body runs here, partially: its arguments are bound, the
//! statements that only compute from them (`key = key.to_s`, `type ||
//! :string`, a guard that does not fire) are evaluated, a call to
//! another of the class's methods is followed into, and each
//! `define_method` / `define_singleton_method` with a name that comes
//! out literal becomes an ordinary `def` in the calling class. The
//! defined bodies read the captured values as Ruby's closures do: a
//! Symbol, number, `true`, `false` or `nil` as that literal, anything
//! else (a String, `Time.zone.today.year`) through a constant the
//! expansion assigns where the call is, so it is evaluated once, then,
//! and shared by every method that captures it. A statement on the
//! class's own state (`@defined_settings << {…}`) runs in the calling
//! class's body, as it does when the macro runs there.
//!
//! What cannot be evaluated this way — a guard on a value known only at
//! run time, a name computed from one, a captured local the defined body
//! reassigns, a call the expansion cannot follow — leaves the call as it
//! was and reports it, rather than dropping the methods it would have
//! defined.

use std::collections::{HashMap, HashSet};

use crate::App;
use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility, ModelBodyItem, Param};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::{SourceFile, Span};

use crate::ingest::prism_api::PrismLocation;
const MAX_DEPTH: usize = 8;

pub(crate) fn expand_class_macros(app: &mut App, sources: &[SourceFile]) {
    let parents: HashMap<ClassId, ClassId> = app
        .models
        .iter()
        .filter_map(|m| Some((m.name.clone(), m.parent.clone()?)))
        .chain(app.library_classes.iter().filter(|lc| !lc.is_module).filter_map(|lc| Some((lc.name.clone(), lc.parent.clone()?))))
        .collect();
    let hooks = hook_macro_table(app);
    let frozen = frozen_readers(app, sources);
    let known: HashSet<String> = app
        .library_classes
        .iter()
        .map(|c| c.name.0.as_str().to_string())
        .chain(app.models.iter().map(|m| m.name.0.as_str().to_string()))
        .collect();
    for model_index in 0..app.models.len() {
        let ancestors = ancestors(app, &parents, &app.models[model_index].name);
        let mut table = class_method_table(app, &ancestors);
        let own_names: HashSet<Symbol> = table.keys().cloned().collect();
        for (name, def) in &hooks {
            table.entry(name.clone()).or_insert_with(|| def.clone());
        }
        if table.is_empty() {
            continue;
        }
        let macros: HashSet<Symbol> = table
            .keys()
            .filter(|name| (hooks.contains_key(*name) && !own_names.contains(*name)) || reaches_definition(name, &table, 0))
            .cloned()
            .collect();
        if macros.is_empty() {
            continue;
        }
        let constants = literal_constants(app, &ancestors);
        let model = &app.models[model_index];
        // Not one this can follow: a class that redefines the definers, or a macro also called where the class body does not always run it.
        let redefined = table.contains_key(&Symbol::from("define_method")) || table.contains_key(&Symbol::from("define_singleton_method"));
        let mut nested: HashSet<Symbol> = HashSet::new();
        for item in &model.body {
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let root = match &*expr.node {
                ExprNode::Send { method, .. } => Some(method),
                _ => None,
            };
            let mut first = true;
            visit(expr, &mut |e| {
                if let ExprNode::Send { recv, method, .. } = &*e.node
                    && macros.contains(method)
                    && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                    && !(first && root == Some(method))
                {
                    nested.insert(method.clone());
                }
                first = false;
            });
        }
        let own: HashSet<(Symbol, MethodReceiver)> = model.methods().map(|m| (m.name.clone(), m.receiver)).collect();
        let prefix = slot_prefix(model.name.0.as_str());
        let mut counter = 0usize;
        let mut defined: HashSet<(Symbol, MethodReceiver)> = HashSet::new();
        let mut reserved: Option<HashSet<Symbol>> = None;
        let mut expansions: HashMap<usize, Vec<ModelBodyItem>> = HashMap::new();
        let mut defaults: indexmap::IndexMap<Symbol, Expr> = indexmap::IndexMap::new();
        for (index, item) in model.body.iter().enumerate() {
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv, method, args, block, .. } = &*expr.node else { continue };
            if !macros.contains(method) || recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef)) {
                continue;
            }
            let mut run = Run { table: &table, constants: &constants, frozen: &frozen, known: &known, scope: None, prefix: &prefix, counter, out: Vec::new(), replayed: HashSet::new(), class_attrs: HashMap::new(), unset_cattrs: HashMap::new(), methods: Vec::new(), aliases: Vec::new(), strings: HashMap::new(), owner: model.name.0.clone(), sources };
            let result = if block.is_some() {
                Err("it is passed a block".to_string())
            } else if redefined {
                Err("the class redefines `define_method`".to_string())
            } else if nested.contains(method) {
                Err("it is also called where the class body may not run it".to_string())
            } else if !call_syntax_kept(expr, sources) {
                Err("its arguments use a splat or a block argument".to_string())
            } else {
                run.call(method, args, &HashMap::new(), 0)
            }
            .and_then(|()| {
                for m in &run.methods {
                    let key = (m.name.clone(), m.receiver);
                    if own.contains(&key) || !defined.insert(key) {
                        return Err(format!("`{}` is defined twice", m.name.as_str()));
                    }
                    if m.receiver == MethodReceiver::Instance && reserved.get_or_insert_with(|| synthesized_names(app, model_index)).contains(&m.name) {
                        return Err(format!("`{}` is also a method the model's associations or declarations define", m.name.as_str()));
                    }
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    counter = run.counter;
                    let mut items: Vec<ModelBodyItem> = Vec::new();
                    for (index, expr) in run.out.into_iter().enumerate() {
                        if run.replayed.contains(&index) {
                            items.extend(replay(&expr, &model.name, &mut defaults));
                        } else {
                            items.push(ModelBodyItem::Unknown { expr, leading_comments: Vec::new(), leading_blank_line: false });
                        }
                    }
                    items.extend(run.methods.into_iter().map(|method| ModelBodyItem::Method { method, leading_comments: Vec::new(), leading_blank_line: false }));
                    items.extend(run.aliases.into_iter().map(|(new, old, span)| ModelBodyItem::Alias { new, old, leading_comments: Vec::new(), leading_blank_line: false, span }));
                    expansions.insert(index, items);
                }
                // A macro that reaches model DSL is refused by name where the model is lowered
                // (`lower::model_to_library::macro_replay`), which says what it reaches; one report.
                Err(why)
                    if why
                        .strip_prefix("it calls `")
                        .and_then(|rest| rest.strip_suffix('`'))
                        .is_some_and(crate::lower::model_to_library::macro_replay::is_model_dsl) => {}
                // A macro whose body declares through model DSL (`has_many(name, scope, **options)`) is replayed where the model is lowered, whatever its parameters.
                Err(_) if reaches_model_dsl(method, &table, 0) => {}
                // Parameters this expansion does not bind (`*args, **kwargs`): the model lowering's replay takes the call, or refuses it by name.
                Err(why) if why.ends_with("parameters are not ones it binds") || why == "its parameters include a rest or a block" => {}
                Err(why) => {
                    super::prism::push(crate::diagnostic::Diagnostic::unsupported(
                        expr.span,
                        None,
                        "class macro",
                        format!("`{}` defines methods this call does not expand ({why}), so they are missing", method.as_str()),
                    ));
                }
            }
        }
        if expansions.is_empty() {
            continue;
        }
        let model = &mut app.models[model_index];
        model.class_attr_defaults.extend(defaults);
        let mut body = Vec::with_capacity(model.body.len());
        for (index, item) in std::mem::take(&mut model.body).into_iter().enumerate() {
            match expansions.remove(&index) {
                Some(items) => body.extend(items),
                None => body.push(item),
            }
        }
        model.body = body;
    }
}

/// The macros a gem's `ActiveSupport.on_load(:active_record)` gives
/// every model: an included module's class methods, an extended
/// module's instance methods, each as a class method of the model.
fn hook_macro_table(app: &App) -> HashMap<Symbol, MethodDef> {
    let mut table = HashMap::new();
    let sources = app
        .load_hook_class_macros
        .iter()
        .map(|id| (id, MethodReceiver::Class))
        .chain(app.load_hook_extended_macros.iter().map(|id| (id, MethodReceiver::Instance)));
    for (id, receiver) in sources {
        for class in app.library_classes.iter().filter(|c| &c.name == id) {
            for m in class.methods.iter().filter(|m| m.receiver == receiver) {
                let mut def = m.clone();
                def.receiver = MethodReceiver::Class;
                table.entry(def.name.clone()).or_insert(def);
            }
        }
    }
    table
}

/// `Owner.reader` whose body reads a class variable the class body
/// initializes to a literal, and which nothing in the program writes
/// again: ancestry's `Ancestry.default_ancestry_format`. Its setter
/// exists, so a call to it anywhere (or a `class_variable_set` naming
/// the variable) keeps the reader a value known only when it runs.
fn frozen_readers(app: &App, sources: &[SourceFile]) -> HashMap<(String, Symbol), Expr> {
    let mut out = HashMap::new();
    for class in &app.library_classes {
        let initial: HashMap<&str, &Expr> = class
            .class_ivar_initializers
            .iter()
            .filter_map(|e| match &*e.node {
                ExprNode::Assign { target: LValue::Var { name, .. }, value }
                    if name.as_str().starts_with("@@") && matches!(&*value.node, ExprNode::Lit { .. }) =>
                {
                    Some((name.as_str(), value))
                }
                _ => None,
            })
            .collect();
        if initial.is_empty() {
            continue;
        }
        for reader in class.methods.iter().filter(|m| m.receiver == MethodReceiver::Class && m.params.is_empty()) {
            let body = match &*reader.body.node {
                ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
                _ => &reader.body,
            };
            let ExprNode::Var { name: cvar, .. } = &*body.node else { continue };
            let Some(value) = initial.get(cvar.as_str()) else { continue };
            let writers: Vec<&MethodDef> = class.methods.iter().filter(|m| assigns(&m.body, cvar)).collect();
            if writers.iter().any(|w| !w.name.as_str().ends_with('=')) {
                continue;
            }
            let setters: Vec<&str> = writers.iter().map(|w| w.name.as_str().trim_end_matches('=')).collect();
            if sources.iter().any(|source| writes_through(&source.text, &setters, cvar.as_str())) {
                continue;
            }
            out.insert((class.name.0.as_str().to_string(), reader.name.clone()), (*value).clone());
        }
    }
    out
}

/// Whether `text` calls one of `setters` (`Owner.name = v`, `send(:name=, v)`)
/// or sets the variable reflectively. Over-matching only keeps a value unfolded.
fn writes_through(text: &str, setters: &[&str], cvar: &str) -> bool {
    for setter in setters {
        for (at, _) in text.match_indices(setter) {
            let before = &text[..at];
            let after = &text[at + setter.len()..];
            let assigns = after.trim_start().starts_with('=') && !after.trim_start().starts_with("==");
            // `send(:name=, v)` and `send("name=", v)` name the setter as a Symbol or a String
            let named = before.ends_with(':') || before.ends_with('"') || before.ends_with('\'');
            if (assigns && before.ends_with('.') && !before.ends_with("def self.")) || (named && after.starts_with('=')) {
                return true;
            }
        }
    }
    text.contains("class_variable_set") && text.contains(&cvar[2..])
}

/// A class-body declaration the macro ran, as the model's own: written
/// back as Ruby and read the way the model's body is.
fn replay(statement: &Expr, owner: &ClassId, defaults: &mut indexmap::IndexMap<Symbol, Expr>) -> Vec<ModelBodyItem> {
    let unknown = || vec![ModelBodyItem::Unknown { expr: statement.clone(), leading_comments: Vec::new(), leading_blank_line: false }];
    let source = crate::emit::ruby::expr::emit_expr(statement);
    let parsed = super::prism::parse_silent(source.as_bytes());
    if parsed.errors().next().is_some() {
        return unknown();
    }
    let Some(program) = parsed.node().as_program_node() else { return unknown() };
    let mut items = Vec::new();
    for stmt in program.statements().body().iter() {
        match super::model::ingest_model_body_items(&stmt, owner, "<class macro>", Vec::new(), Some(&mut *defaults), &super::model::ClassConsts::default()) {
            Ok(mut ingested) => {
                for item in &mut ingested {
                    mark_constants(item);
                }
                items.extend(ingested);
            }
            Err(_) => return unknown(),
        }
    }
    items
}

// A replayed statement's span points into a fragment the constant resolver never indexed, so its constants are read as written
fn mark_constants(item: &mut ModelBodyItem) {
    fn mark(e: &mut Expr) {
        if matches!(&*e.node, ExprNode::Const { .. }) {
            e.decisions |= crate::expr::GENERATED_CONST_REF;
        }
        e.node.for_each_child_mut(&mut mark);
    }
    if let ModelBodyItem::Unknown { expr, .. } = item {
        mark(expr);
    }
}

/// The instance methods the model lowering synthesizes for this model
/// (associations, declarations), which a generated `def` of the same
/// name would collide with.
fn synthesized_names(app: &App, model_index: usize) -> HashSet<Symbol> {
    let model = &app.models[model_index];
    let mut specs = crate::lower::controller_to_library::params::collect_specs(&app.controllers);
    specs.mark_file_fields(&app.models);
    let synthesized = crate::lower::model_to_library::build_methods(model, &app.models, &app.schema, &specs);
    let mut out: HashSet<Symbol> = crate::lower::model_to_library::build_class_info(model, &synthesized, app.schema.tables.get(&model.table.0))
        .instance_methods
        .into_keys()
        .collect();
    out.extend(synthesized.iter().filter(|m| m.receiver == MethodReceiver::Instance).map(|m| m.name.clone()));
    // Not a column's accessor: a model's own one wins over it (`push_user_methods`), as a generated one does in Ruby.
    if let Some(table) = app.schema.tables.get(&model.table.0) {
        for c in &table.columns {
            out.remove(&c.name);
            out.remove(&Symbol::from(format!("{}=", c.name.as_str())));
        }
    }
    out
}

fn ancestors(app: &App, parents: &HashMap<ClassId, ClassId>, model: &ClassId) -> Vec<ClassId> {
    let mut out = Vec::new();
    let mut cur = parents.get(model);
    while let Some(p) = cur {
        if out.contains(p) || !(app.models.iter().any(|m| &m.name == p) || app.library_classes.iter().any(|lc| &lc.name == p)) {
            break;
        }
        out.push(p.clone());
        cur = parents.get(p);
    }
    out
}

/// The class methods the ancestors define, the nearest one's winning.
fn class_method_table(app: &App, ancestors: &[ClassId]) -> HashMap<Symbol, MethodDef> {
    let mut table = HashMap::new();
    for id in ancestors.iter().rev() {
        let methods: Vec<&MethodDef> = match app.models.iter().find(|m| &m.name == id) {
            Some(model) => model.methods().collect(),
            None => app.library_classes.iter().filter(|lc| &lc.name == id).flat_map(|lc| lc.methods.iter()).collect(),
        };
        for m in methods.into_iter().filter(|m| m.receiver == MethodReceiver::Class) {
            table.insert(m.name.clone(), m.clone());
        }
    }
    table
}

fn reaches_definition(name: &Symbol, table: &HashMap<Symbol, MethodDef>, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let Some(def) = table.get(name) else { return false };
    let mut found = false;
    visit(&def.body, &mut |e| {
        if let ExprNode::Send { recv, method, .. } = &*e.node
            && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        {
            found |= matches!(method.as_str(), "define_method" | "define_singleton_method")
                || (method != name && table.contains_key(method) && reaches_definition(method, table, depth + 1));
        }
    });
    found
}

/// Whether the macro, or a class method it calls, calls model DSL.
fn reaches_model_dsl(name: &Symbol, table: &HashMap<Symbol, MethodDef>, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let Some(def) = table.get(name) else { return false };
    let mut found = false;
    visit(&def.body, &mut |e| {
        if let ExprNode::Send { recv, method, .. } = &*e.node
            && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        {
            // A class method of the same name (`store`) is the class's own, not the DSL.
            found |= if table.contains_key(method) {
                method != name && reaches_model_dsl(method, table, depth + 1)
            } else {
                crate::lower::model_to_library::macro_replay::is_model_dsl(method.as_str())
            };
        }
    });
    found
}

fn visit(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    e.node.for_each_child(&mut |c| visit(c, f));
}

/// Constants the ancestors assign a list of Strings or Symbols
/// (`PROTECTED_KEYS = %w[var value].freeze`), for a guard that reads one.
fn literal_constants(app: &App, ancestors: &[ClassId]) -> HashMap<Symbol, Expr> {
    let mut out = HashMap::new();
    for id in ancestors.iter().rev() {
        let assigned: Vec<(Symbol, Expr)> = match app.models.iter().find(|m| &m.name == id) {
            Some(model) => model
                .body
                .iter()
                .filter_map(|item| match item {
                    ModelBodyItem::Unknown { expr, .. } => match &*expr.node {
                        ExprNode::Assign { target: LValue::Const { path }, value } if path.len() == 1 => Some((path[0].clone(), value.clone())),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
            None => app.library_classes.iter().filter(|lc| &lc.name == id).flat_map(|lc| lc.constants.iter().cloned()).collect(),
        };
        for (name, value) in &assigned {
            let value = match &*value.node {
                ExprNode::Send { recv: Some(r), method, args, .. } if method.as_str() == "freeze" && args.is_empty() => r,
                _ => value,
            };
            if let ExprNode::Array { elements, .. } = &*value.node
                && elements.iter().all(|e| matches!(&*e.node, ExprNode::Lit { value: Literal::Str { .. } | Literal::Sym { .. } }))
            {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    out
}

fn slot_prefix(class: &str) -> String {
    let base: String = class.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    format!("ROUNDHOUSE_MACRO_{base}")
}

/// A value the macro computes from its arguments: a literal, or a
/// constant the expansion assigned (`truthy` when its shape says so).
#[derive(Clone, Debug)]
enum Val {
    Lit(Expr),
    Slot { name: Symbol, truthy: Option<bool> },
}

struct Run<'a> {
    table: &'a HashMap<Symbol, MethodDef>,
    constants: &'a HashMap<Symbol, Expr>,
    frozen: &'a HashMap<(String, Symbol), Expr>,
    /// Every class and module this program declares, by full name.
    known: &'a HashSet<String>,
    /// The module the running macro is defined in.
    scope: Option<Symbol>,
    prefix: &'a str,
    counter: usize,
    /// Class-body statements, in the order the macro runs them.
    out: Vec<Expr>,
    /// Indexes into `out` of the declarations to read as the model's
    /// body (`validate`, `cattr_accessor`, a mixin) rather than keep as
    /// a statement.
    replayed: HashSet<usize>,
    /// What `self.x = …` set, for a later read of `x`.
    class_attrs: HashMap<Symbol, Val>,
    /// A `cattr_*`/`mattr_*` declared in this run and not yet written,
    /// by name: the index of its declaration in `out`.
    unset_cattrs: HashMap<Symbol, usize>,
    methods: Vec<MethodDef>,
    aliases: Vec<(Symbol, Symbol, Span)>,
    /// One constant per captured String, so every method shares it.
    strings: HashMap<(String, usize), Symbol>,
    owner: Symbol,
    sources: &'a [SourceFile],
}

type Env = HashMap<Symbol, Val>;

impl Run<'_> {
    fn call(&mut self, name: &Symbol, args: &[Expr], env: &Env, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("its calls nest too deep".into());
        }
        let def = self.table.get(name).ok_or_else(|| format!("`{}` is not one of the class's methods", name.as_str()))?;
        if !super::model_macros::supported_source_signature(def, self.sources, true) {
            return Err(format!("`{}`'s parameters are not ones it binds", name.as_str()));
        }
        let mut bound = self.bind(def, args, env)?;
        let statements: Vec<Expr> = match &*def.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![def.body.clone()],
        };
        let inner = def.enclosing_class.clone().or_else(|| self.scope.clone());
        let outer = std::mem::replace(&mut self.scope, inner);
        let ran = statements.iter().try_for_each(|st| match self.exec(st, &mut bound, depth) {
            Ok(true) => Err(None),
            Ok(false) => Ok(()),
            Err(why) => Err(Some(why)),
        });
        self.scope = outer;
        match ran {
            Err(Some(why)) => Err(why),
            _ => Ok(()),
        }
    }

    fn bind(&mut self, def: &MethodDef, args: &[Expr], env: &Env) -> Result<Env, String> {
        if def.block_param.is_some() || def.params.iter().any(|p| p.rest || p.from_kwrest) {
            return Err("its parameters include a rest or a block".into());
        }
        let (positional, keywords): (&[Expr], Vec<(Symbol, Expr)>) = match args.last().map(|a| &*a.node) {
            Some(ExprNode::Hash { entries, kwargs: true }) => {
                let mut kw = Vec::new();
                for (k, v) in entries {
                    let ExprNode::Lit { value: Literal::Sym { value } } = &*k.node else {
                        return Err("a keyword is not a Symbol".into());
                    };
                    kw.push((value.clone(), v.clone()));
                }
                (&args[..args.len() - 1], kw)
            }
            _ => (args, Vec::new()),
        };
        let mut keywords: HashMap<Symbol, Expr> = keywords.into_iter().collect();
        let mut positional = positional.iter();
        let mut bound = Env::new();
        for p in &def.params {
            let given = if p.keyword || p.from_keyword { keywords.remove(&p.name) } else { positional.next().cloned() };
            let val = match (given, &p.default) {
                (Some(e), _) => self.eval(&e, env)?,
                (None, Some(default)) => self.eval(default, &bound)?,
                (None, None) => return Err(format!("`{}` is not given", p.name.as_str())),
            };
            bound.insert(p.name.clone(), val);
        }
        if positional.next().is_some() || !keywords.is_empty() {
            return Err("it is given an argument it does not take".into());
        }
        Ok(bound)
    }

    /// Runs one statement; `true` when it returned from the macro.
    fn exec(&mut self, st: &Expr, env: &mut Env, depth: usize) -> Result<bool, String> {
        match &*st.node {
            ExprNode::Seq { exprs } => {
                for e in exprs {
                    if self.exec(e, env, depth)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            // The macro's value is not read: a class-body call discards it.
            ExprNode::Return { .. } => Ok(true),
            ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
                let v = self.eval(value, env)?;
                env.insert(name.clone(), v);
                Ok(false)
            }
            ExprNode::OpAssign { target: LValue::Var { name, .. }, op: crate::expr::OpAssignOp::OrOr, value } => {
                let current = env.get(name).cloned().ok_or_else(|| format!("`{}` is not bound", name.as_str()))?;
                match self.truthy(&current) {
                    Some(true) => {}
                    Some(false) => {
                        let v = self.eval(value, env)?;
                        env.insert(name.clone(), v);
                    }
                    None => return Err("a `||=` depends on a value known only when it runs".into()),
                }
                Ok(false)
            }
            ExprNode::If { cond, then_branch, else_branch } => match {
                let v = self.eval(cond, env)?;
                self.truthy(&v)
            } {
                Some(true) => self.exec(then_branch, env, depth),
                Some(false) => self.exec(else_branch, env, depth),
                None => Err("a condition depends on a value known only when it runs".into()),
            },
            ExprNode::Lit { value: Literal::Nil } => Ok(false),
            ExprNode::Send { recv, method, args, block, .. } if recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) => {
                match method.as_str() {
                    "define_method" | "define_singleton_method" => {
                        let receiver = if method.as_str() == "define_method" { MethodReceiver::Instance } else { MethodReceiver::Class };
                        let [name] = args.as_slice() else { return Err("a definition is given other than one name".into()) };
                        let Some(block) = block else { return Err("a definition has no block".into()) };
                        if !definition_syntax_kept(st, self.sources) {
                            return Err("a definition uses syntax the expansion would erase".into());
                        }
                        self.define(receiver, name, block, env).map(|()| false)
                    }
                    "alias_method" => {
                        let [new_name, old_name] = args.as_slice() else { return Err("an alias is not given two names".into()) };
                        let new_name = self.literal_name(new_name, env)?;
                        let old_name = self.literal_name(old_name, env)?;
                        self.aliases.push((new_name, old_name, st.span));
                        Ok(false)
                    }
                    "raise" | "fail" => Err("it raises".into()),
                    "include" | "extend"
                        if block.is_none() && !args.is_empty() && args.iter().all(|a| matches!(&*a.node, ExprNode::Const { .. })) =>
                    {
                        for arg in args {
                            let ExprNode::Const { path } = &*arg.node else { continue };
                            if !self.names_alike(path) {
                                return Err(format!(
                                    "its `{}` names a different module from the model than from the macro's own module",
                                    path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::")
                                ));
                            }
                        }
                        self.declare(st.clone());
                        Ok(false)
                    }
                    name if name.ends_with('=')
                        && !matches!(name, "==" | "!=" | "<=" | ">=" | "===")
                        && recv.is_some()
                        && block.is_none()
                        && args.len() == 1
                        && !self.table.contains_key(method) =>
                    {
                        let v = self.eval(&args[0], env)?;
                        let read = self.read(&v, args[0].span);
                        let attr = Symbol::from(name.trim_end_matches('='));
                        self.class_attrs.insert(attr.clone(), v);
                        // The first write after the declaration is its default, which the model's cattr reads as one
                        if let Some(index) = self.unset_cattrs.remove(&attr)
                            && let ExprNode::Send { args, .. } = &mut *self.out[index].node
                        {
                            let key = lit(read.span, Literal::Sym { value: Symbol::from("default") });
                            args.push(Expr::new(read.span, ExprNode::Hash { entries: vec![(key, read)], kwargs: true }));
                            return Ok(false);
                        }
                        let mut assigned = st.clone();
                        if let ExprNode::Send { args, .. } = &mut *assigned.node {
                            args[0] = read;
                        }
                        self.declare(assigned);
                        Ok(false)
                    }
                    name if CLASS_BODY_DECLARATIONS.contains(&name) && block.is_none() && !self.table.contains_key(method) => {
                        let declared = self.substitute(st, env, &HashSet::new())?;
                        if matches!(name, "cattr_accessor" | "cattr_reader" | "mattr_accessor" | "mattr_reader")
                            && let ExprNode::Send { args, .. } = &*declared.node
                            && let [arg] = args.as_slice()
                            && let ExprNode::Lit { value: Literal::Sym { value } } = &*arg.node
                        {
                            self.unset_cattrs.insert(value.clone(), self.out.len());
                        }
                        self.declare(declared);
                        Ok(false)
                    }
                    _ if self.table.contains_key(method) && block.is_none() => {
                        let args: Vec<Expr> = args.clone();
                        self.call(method, &args, env, depth + 1).map(|()| false)
                    }
                    _ => Err(format!("it calls `{}`", method.as_str())),
                }
            }
            ExprNode::Assign { target: LValue::Attr { recv, name }, value } if matches!(&*recv.node, ExprNode::SelfRef) => {
                let v = self.eval(value, env)?;
                let read = self.read(&v, value.span);
                self.class_attrs.insert(name.clone(), v);
                let mut assigned = st.clone();
                if let ExprNode::Assign { value, .. } = &mut *assigned.node {
                    *value = read;
                }
                self.declare(assigned);
                Ok(false)
            }
            // A loop over what the macro was given: each element runs the block, as Ruby does.
            ExprNode::Send { recv: Some(r), method, args, block: Some(b), .. }
                if matches!(method.as_str(), "each" | "each_pair") && args.is_empty() =>
            {
                let Some(Val::Lit(over)) = self.fold(r, env) else { return Err("it loops over a value known only when it runs".into()) };
                let ExprNode::Lambda { params, rest_param: None, body, .. } = &*b.node else { return Err("a loop's block takes other than plain parameters".into()) };
                let rows: Vec<Vec<Expr>> = match &*over.node {
                    ExprNode::Hash { entries, .. } => entries.iter().map(|(k, v)| vec![k.clone(), v.clone()]).collect(),
                    ExprNode::Array { elements, .. } => elements.iter().map(|e| vec![e.clone()]).collect(),
                    _ => return Err("it loops over a value that is not a literal Hash or Array".into()),
                };
                // Not a key: Ruby hands a one-parameter block over a Hash the `[key, value]` pair
                if matches!(&*over.node, ExprNode::Hash { .. }) && params.len() != 2 {
                    return Err("a loop over a Hash binds other than its key and value".into());
                }
                for row in rows {
                    if params.len() > row.len() {
                        return Err("a loop's block takes more parameters than each element has".into());
                    }
                    let mut inner = env.clone();
                    for (param, value) in params.iter().zip(row.iter()) {
                        inner.insert(param.clone(), Val::Lit(value.clone()));
                    }
                    if self.exec(body, &mut inner, depth)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            // The class's own state: run where the macro runs, in the calling class's body.
            _ if touches_class_state(st) => {
                let substituted = self.substitute(st, env, &HashSet::new())?;
                self.out.push(substituted);
                Ok(false)
            }
            _ => Err(format!("a statement is not one it can run: {}", describe(st))),
        }
    }

    /// The statement is replayed in the model, so its constant must name
    /// there what it names in the macro's module: a known module with no
    /// nearer one of that name in the macro's nesting.
    fn names_alike(&self, path: &[Symbol]) -> bool {
        let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
        let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
        if !self.known.contains(&written) {
            return false;
        }
        if rooted {
            return true;
        }
        let mut scope = self.scope.as_ref().map(|s| s.as_str().to_string());
        while let Some(outer) = scope {
            if self.known.contains(&format!("{outer}::{written}")) {
                return false;
            }
            scope = outer.rsplit_once("::").map(|(o, _)| o.to_string());
        }
        true
    }

    fn declare(&mut self, statement: Expr) {
        self.replayed.insert(self.out.len());
        self.out.push(statement);
    }

    fn literal_name(&mut self, name: &Expr, env: &Env) -> Result<Symbol, String> {
        match self.fold(name, env) {
            Some(Val::Lit(e)) => match &*e.node {
                ExprNode::Lit { value: Literal::Sym { value } } => Ok(value.clone()),
                ExprNode::Lit { value: Literal::Str { value } } => Ok(Symbol::from(value.as_str())),
                _ => Err("a defined name is not a name".into()),
            },
            _ => Err("a defined name is known only when it runs".into()),
        }
    }

    fn define(&mut self, receiver: MethodReceiver, name: &Expr, block: &Expr, env: &Env) -> Result<(), String> {
        let name = self.literal_name(name, env)?;
        if !valid_def_name(&name) {
            return Err(format!("`{}` is not a method name", name.as_str()));
        }
        let ExprNode::Lambda { params, rest_param, extra_params, block_param: None, body, .. } = &*block.node else {
            return Err("a definition's block takes a block".into());
        };
        let mut shadow: HashSet<Symbol> = params.iter().cloned().chain(rest_param.iter().cloned()).collect();
        shadow.extend(extra_params.iter().map(|p| p.name.clone()));
        collect_locals(body, &mut shadow);
        if let Some(captured) = env.keys().find(|k| shadow.contains(*k) && assigns(body, k)) {
            return Err(format!("the defined `{}` reassigns its captured `{}`", name.as_str(), captured.as_str()));
        }
        let mut method_params: Vec<Param> = params.iter().map(|p| plain_param(p.clone())).collect();
        if let Some(rest) = rest_param {
            let mut p = plain_param(rest.clone());
            p.rest = true;
            method_params.push(p);
        }
        for p in extra_params {
            let mut p = p.clone();
            if let Some(d) = &p.default {
                p.default = Some(self.substitute(d, env, &HashSet::new())?);
            }
            method_params.push(p);
        }
        let body = self.substitute(body, env, &shadow)?;
        let template = self.table.values().next().expect("a macro was found").clone();
        // Ruby makes these private whatever defines them.
        let visibility = if receiver == MethodReceiver::Instance && matches!(name.as_str(), "initialize" | "initialize_copy" | "initialize_dup" | "initialize_clone") {
            MethodVisibility::Private
        } else {
            MethodVisibility::Public
        };
        self.methods.push(MethodDef {
            name,
            receiver,
            visibility,
            params: method_params,
            unsupported_formals: None,
            has_anonymous_block: false,
            block_param: None,
            name_span: block.span,
            body,
            signature: None,
            effects: Default::default(),
            enclosing_class: Some(self.owner.clone()),
            ..template
        });
        Ok(())
    }

    /// What the macro computes from `e`, folded where its inputs are
    /// literal, otherwise assigned to a constant at this point.
    fn eval(&mut self, e: &Expr, env: &Env) -> Result<Val, String> {
        if let Some(v) = self.fold(e, env) {
            return Ok(v);
        }
        let value = self.substitute(e, env, &HashSet::new())?;
        let truthy = matches!(&*value.node, ExprNode::Hash { .. } | ExprNode::Array { .. } | ExprNode::StringInterp { .. }).then_some(true);
        Ok(Val::Slot { name: self.slot(value), truthy })
    }

    fn fold(&mut self, e: &Expr, env: &Env) -> Option<Val> {
        match &*e.node {
            ExprNode::Lit { .. } | ExprNode::SelfRef => Some(Val::Lit(e.clone())),
            ExprNode::Var { name, .. } => env.get(name).cloned(),
            ExprNode::Send { recv: Some(r), method, args, block: None, .. }
                if args.is_empty()
                    && let ExprNode::Const { path } = &*r.node
                    && let Some(value) = self.frozen.get(&(
                        path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::"),
                        method.clone(),
                    )) =>
            {
                Some(Val::Lit(Expr { span: e.span, ..value.clone() }))
            }
            ExprNode::Hash { entries, kwargs } => {
                let mut folded = Vec::new();
                for (k, v) in entries {
                    let (Val::Lit(k), Val::Lit(v)) = (self.fold(k, env)?, self.fold(v, env)?) else { return None };
                    folded.push((k, v));
                }
                Some(Val::Lit(Expr::new(e.span, ExprNode::Hash { entries: folded, kwargs: *kwargs })))
            }
            ExprNode::Array { elements, style } => {
                let mut folded = Vec::new();
                for el in elements {
                    let Val::Lit(el) = self.fold(el, env)? else { return None };
                    folded.push(el);
                }
                Some(Val::Lit(Expr::new(e.span, ExprNode::Array { elements: folded, style: style.clone() })))
            }
            ExprNode::Send { recv, method, args, block: None, .. }
                if args.is_empty() && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) && self.class_attrs.contains_key(method) =>
            {
                self.class_attrs.get(method).cloned()
            }
            ExprNode::Send { recv: Some(r), method, args, block: None, .. } if self.fold_query(r, method, args, env).is_some() => {
                self.fold_query(r, method, args, env).map(|value| Val::Lit(lit(e.span, value)))
            }
            ExprNode::Send { recv: Some(r), method, args, block: None, .. } if method.as_str() == "[]" || method.as_str() == "presence" => {
                let Val::Lit(recv) = self.fold(r, env)? else { return None };
                match (&*recv.node, method.as_str(), args.as_slice()) {
                    (ExprNode::Hash { entries, .. }, "[]", [key]) => {
                        let Val::Lit(key) = self.fold(key, env)? else { return None };
                        let ExprNode::Lit { value: key } = &*key.node else { return None };
                        for (k, v) in entries {
                            let ExprNode::Lit { value: k } = &*k.node else { return None };
                            if literal_eq(k, key)? {
                                return Some(Val::Lit(v.clone()));
                            }
                        }
                        Some(Val::Lit(lit(e.span, Literal::Nil)))
                    }
                    (_, "presence", []) => Some(Val::Lit(if blank(&recv)? { lit(e.span, Literal::Nil) } else { recv.clone() })),
                    _ => None,
                }
            }
            ExprNode::StringInterp { .. } => {
                let mut text = String::new();
                let mut ok = true;
                let ExprNode::StringInterp { parts } = &*e.node else { unreachable!() };
                for part in parts {
                    match part {
                        crate::expr::InterpPart::Text { value } => text.push_str(value),
                        crate::expr::InterpPart::Expr { expr } => match self.fold(expr, env).and_then(|v| lit_text(&v)) {
                            Some(t) => text.push_str(&t),
                            None => ok = false,
                        },
                    }
                }
                ok.then(|| Val::Lit(lit(e.span, Literal::Str { value: text })))
            }
            ExprNode::BoolOp { op, left, right, .. } => {
                let l = self.fold(left, env)?;
                let lt = self.truthy(&l)?;
                let or = matches!(op, crate::expr::BoolOpKind::Or);
                if lt == or { Some(l) } else { self.fold(right, env) }
            }
            ExprNode::Send { recv: Some(r), method, args, block: None, .. } => {
                let recv = self.fold(r, env)?;
                let Val::Lit(recv) = recv else { return None };
                let ExprNode::Lit { value } = &*recv.node else { return None };
                match (method.as_str(), value, args.as_slice()) {
                    ("to_s", _, []) => lit_text(&Val::Lit(recv.clone())).map(|t| Val::Lit(lit(e.span, Literal::Str { value: t }))),
                    ("to_sym", _, []) => lit_text(&Val::Lit(recv.clone())).map(|t| Val::Lit(lit(e.span, Literal::Sym { value: Symbol::from(t.as_str()) }))),
                    ("nil?", v, []) => Some(Val::Lit(lit(e.span, Literal::Bool { value: matches!(v, Literal::Nil) }))),
                    ("==" | "!=", _, [other]) => {
                        let Val::Lit(other) = self.fold(other, env)? else { return None };
                        let ExprNode::Lit { value: o } = &*other.node else { return None };
                        let same = literal_eq(value, o)?;
                        Some(Val::Lit(lit(e.span, Literal::Bool { value: same == (method.as_str() == "==") })))
                    }
                    ("in?", _, [list]) => {
                        let ExprNode::Const { path } = &*list.node else { return None };
                        let [name] = path.as_slice() else { return None };
                        let ExprNode::Array { elements, .. } = &*self.constants.get(name)?.node else { return None };
                        let mut found = false;
                        for el in elements {
                            let ExprNode::Lit { value: o } = &*el.node else { return None };
                            found |= literal_eq(value, o)?;
                        }
                        Some(Val::Lit(lit(e.span, Literal::Bool { value: found })))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// A question with a literal answer about a literal: `is_a?`,
    /// `present?`, `blank?`, `include?`, `!`.
    fn fold_query(&mut self, r: &Expr, method: &Symbol, args: &[Expr], env: &Env) -> Option<Literal> {
        if !matches!(method.as_str(), "is_a?" | "kind_of?" | "present?" | "blank?" | "empty?" | "include?" | "!") {
            return None;
        }
        let Val::Lit(recv) = self.fold(r, env)? else { return None };
        let answer = match (method.as_str(), args) {
            ("is_a?" | "kind_of?", [class]) => {
                let ExprNode::Const { path } = &*class.node else { return None };
                let written: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
                let kind = match &*recv.node {
                    ExprNode::Hash { .. } => "Hash",
                    ExprNode::Array { .. } => "Array",
                    ExprNode::Lit { value: Literal::Str { .. } } => "String",
                    ExprNode::Lit { value: Literal::Sym { .. } } => "Symbol",
                    ExprNode::Lit { value: Literal::Int { .. } } => "Integer",
                    ExprNode::Lit { value: Literal::Nil } => "NilClass",
                    _ => return None,
                };
                match written.as_slice() {
                    [name] if *name == kind => true,
                    [name] if matches!(*name, "Hash" | "Array" | "String" | "Symbol" | "Integer" | "NilClass") => false,
                    _ => return None,
                }
            }
            ("present?", []) => !blank(&recv)?,
            ("blank?", []) => blank(&recv)?,
            ("empty?", []) => match &*recv.node {
                ExprNode::Hash { entries, .. } => entries.is_empty(),
                ExprNode::Array { elements, .. } => elements.is_empty(),
                ExprNode::Lit { value: Literal::Str { value } } => value.is_empty(),
                _ => return None,
            },
            ("include?", [needle]) => {
                let ExprNode::Array { elements, .. } = &*recv.node else { return None };
                let Val::Lit(needle) = self.fold(needle, env)? else { return None };
                let ExprNode::Lit { value: needle } = &*needle.node else { return None };
                let mut found = false;
                for el in elements {
                    let ExprNode::Lit { value } = &*el.node else { return None };
                    found |= literal_eq(value, needle)?;
                }
                found
            }
            ("!", []) => !self.truthy(&Val::Lit(recv))?,
            _ => return None,
        };
        Some(Literal::Bool { value: answer })
    }

    fn truthy(&self, v: &Val) -> Option<bool> {
        match v {
            Val::Lit(e) => match &*e.node {
                ExprNode::Lit { value: Literal::Nil } => Some(false),
                ExprNode::Lit { value: Literal::Bool { value } } => Some(*value),
                _ => Some(true),
            },
            Val::Slot { truthy, .. } => *truthy,
        }
    }

    fn slot(&mut self, value: Expr) -> Symbol {
        self.counter += 1;
        let name = Symbol::from(format!("{}_{}", self.prefix, self.counter));
        let span = value.span;
        self.out.push(Expr::new(span, ExprNode::Assign { target: LValue::Const { path: vec![name.clone()] }, value }));
        name
    }

    /// `e` with each captured name replaced by what it holds, and a
    /// literal-named `send` / `__send__` on `self` called directly.
    fn substitute(&mut self, e: &Expr, env: &Env, shadow: &HashSet<Symbol>) -> Result<Expr, String> {
        let mut out = e.clone();
        let mut failed = None;
        self.substitute_in(&mut out, env, shadow, &mut failed);
        match failed {
            Some(why) => Err(why),
            None => Ok(out),
        }
    }

    fn substitute_in(&mut self, e: &mut Expr, env: &Env, shadow: &HashSet<Symbol>, failed: &mut Option<String>) {
        if let ExprNode::Var { name, .. } = &*e.node
            && !shadow.contains(name)
            && let Some(v) = env.get(name).cloned()
        {
            *e = self.read(&v, e.span);
            return;
        }
        if matches!(&*e.node, ExprNode::Lambda { .. }) {
            let mut inner = shadow.clone();
            if let ExprNode::Lambda { params, rest_param, extra_params, .. } = &*e.node {
                inner.extend(params.iter().cloned().chain(rest_param.iter().cloned()).chain(extra_params.iter().map(|p| p.name.clone())));
            }
            e.node.for_each_child_mut(&mut |c| self.substitute_in(c, env, &inner, failed));
            return;
        }
        e.node.for_each_child_mut(&mut |c| self.substitute_in(c, env, shadow, failed));
        // Only a name being spelled (`:"#{name}="`): other folds read the macro owner's constants, not this body's.
        if (matches!(&*e.node, ExprNode::StringInterp { .. }) || matches!(&*e.node, ExprNode::Send { method, .. } if method.as_str() == "to_sym"))
            && let Some(Val::Lit(folded)) = self.fold(e, &Env::new())
        {
            *e = folded;
        }
        collapse_literal_send(e);
    }

    fn read(&mut self, v: &Val, span: Span) -> Expr {
        match v {
            // Not inlined: a captured String is one object every method shares, where a literal allocates a fresh one per call.
            Val::Lit(e) if matches!(&*e.node, ExprNode::Lit { value: Literal::Str { .. } }) => {
                let text = lit_text(v).unwrap_or_default();
                let key = (text, 0);
                let name = match self.strings.get(&key) {
                    Some(n) => n.clone(),
                    None => {
                        let n = self.slot(e.clone());
                        self.strings.insert(key, n.clone());
                        n
                    }
                };
                const_read(span, name)
            }
            Val::Lit(e) => Expr { span, ..e.clone() },
            Val::Slot { name, .. } => const_read(span, name.clone()),
        }
    }
}

fn source_text<'s>(span: Span, sources: &'s [SourceFile]) -> Option<&'s [u8]> {
    let source = span.file.0.checked_sub(1).and_then(|i| sources.get(i as usize))?;
    source.text.as_bytes().get(span.start as usize..span.end as usize)
}

/// The call as written has no splat, double splat or block argument,
/// which the IR would have flattened into ordinary arguments.
fn call_syntax_kept(call: &Expr, sources: &[SourceFile]) -> bool {
    let Some(text) = source_text(call.span, sources) else { return false };
    let parsed = super::prism::parse_silent(text);
    if parsed.errors().next().is_some() {
        return false;
    }
    struct Erased(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Erased {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.0 |= node.as_splat_node().is_some() || node.as_assoc_splat_node().is_some() || node.as_block_argument_node().is_some();
        }
    }
    let mut erased = Erased(false);
    ruby_prism::Visit::visit(&mut erased, &parsed.node());
    !erased.0
}

/// A `define_method` as written has no splat, nested `def`, `defined?`,
/// and its block takes plain parameters: what the IR would have erased
/// into a simpler, different definition. A splat in a rescue clause's
/// class list (`rescue Timeout::Error, *ERRORS => e`) is not an argument
/// splat: the IR keeps it as written, a `Splat` among the clause's
/// classes, so it erases nothing.
fn definition_syntax_kept(statement: &Expr, sources: &[SourceFile]) -> bool {
    let Some(text) = source_text(statement.span, sources) else { return false };
    let parsed = super::prism::parse_silent(text);
    if parsed.errors().next().is_some() {
        return false;
    }
    struct Erased(bool, std::collections::HashSet<usize>);
    impl<'pr> ruby_prism::Visit<'pr> for Erased {
        fn visit_rescue_node(&mut self, node: &ruby_prism::RescueNode<'pr>) {
            for e in node.exceptions().iter() {
                if e.as_splat_node().is_some() {
                    self.1.insert(e.location().start_offset());
                }
            }
            ruby_prism::visit_rescue_node(self, node);
        }

        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            if node.as_splat_node().is_some() && self.1.contains(&node.location().start_offset()) {
                return;
            }
            self.0 |= node.as_splat_node().is_some()
                || node.as_assoc_splat_node().is_some()
                || node.as_def_node().is_some()
                || node.as_defined_node().is_some()
                || node.as_multi_target_node().is_some()
                || node.as_keyword_rest_parameter_node().is_some()
                || node.as_rest_parameter_node().is_some()
                || node.as_block_parameter_node().is_some();
        }
    }
    let mut erased = Erased(false, Default::default());
    ruby_prism::Visit::visit(&mut erased, &parsed.node());
    !erased.0
}

/// A synthetic span, not the capture's: Rubydex indexed the source, which has no such constant, and the class's own constants answer it.
fn const_read(_span: Span, name: Symbol) -> Expr {
    let mut e = Expr::new(Span::synthetic(), ExprNode::Const { path: vec![name] });
    e.decisions |= crate::expr::GENERATED_CONST_REF;
    e
}

fn touches_class_state(st: &Expr) -> bool {
    match &*st.node {
        ExprNode::Assign { target: LValue::Ivar { .. }, .. } | ExprNode::OpAssign { target: LValue::Ivar { .. }, .. } => true,
        ExprNode::Send { recv: Some(r), .. } => matches!(&*r.node, ExprNode::Ivar { .. }),
        _ => false,
    }
}

/// A short name for a statement, for a report.
fn describe(st: &Expr) -> String {
    match &*st.node {
        ExprNode::Send { method, recv, .. } => match recv {
            Some(_) => format!("a call to `.{}`", method.as_str()),
            None => format!("a call to `{}`", method.as_str()),
        },
        ExprNode::Assign { target: LValue::Index { .. }, .. } => "an index write".to_string(),
        ExprNode::Assign { target: LValue::Attr { name, .. }, .. } => format!("a write of `{}`", name.as_str()),
        other => format!("a `{}`", other.kind_str()),
    }
}

/// ActiveSupport's `blank?` on a literal.
fn blank(e: &Expr) -> Option<bool> {
    Some(match &*e.node {
        ExprNode::Lit { value: Literal::Nil } => true,
        ExprNode::Lit { value: Literal::Bool { value } } => !value,
        ExprNode::Lit { value: Literal::Str { value } } => value.trim().is_empty(),
        ExprNode::Lit { .. } => false,
        ExprNode::Hash { entries, .. } => entries.is_empty(),
        ExprNode::Array { elements, .. } => elements.is_empty(),
        _ => return None,
    })
}

/// What a macro may declare on its caller, replayed in the caller's body.
const CLASS_BODY_DECLARATIONS: &[&str] = &[
    "cattr_accessor", "cattr_reader", "cattr_writer", "mattr_accessor", "mattr_reader", "mattr_writer", "class_attribute",
    "attribute", "validates", "validate", "validates_presence_of", "validates_numericality_of", "validates_uniqueness_of",
    "before_validation", "after_validation", "before_save", "after_save", "around_save", "before_create", "after_create",
    "before_update", "after_update", "before_destroy", "after_destroy", "after_commit", "after_touch",
    "scope", "has_many", "has_one", "belongs_to", "delegate",
];

fn lit(span: Span, value: Literal) -> Expr {
    Expr::new(span, ExprNode::Lit { value })
}

fn lit_text(v: &Val) -> Option<String> {
    let Val::Lit(e) = v else { return None };
    match &*e.node {
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        ExprNode::Lit { value: Literal::Int { value } } => Some(value.to_string()),
        _ => None,
    }
}

fn literal_eq(a: &Literal, b: &Literal) -> Option<bool> {
    Some(match (a, b) {
        (Literal::Str { value: x }, Literal::Str { value: y }) => x == y,
        (Literal::Sym { value: x }, Literal::Sym { value: y }) => x == y,
        (Literal::Int { value: x }, Literal::Int { value: y }) => x == y,
        (Literal::Nil, Literal::Nil) => true,
        (Literal::Bool { value: x }, Literal::Bool { value: y }) => x == y,
        (Literal::Str { .. } | Literal::Sym { .. } | Literal::Int { .. } | Literal::Nil | Literal::Bool { .. }, _) => false,
        _ => return None,
    })
}

fn plain_param(name: Symbol) -> Param {
    Param { name, default: None, keyword: false, rest: false, forwarding: false, from_keyword: false, from_kwrest: false }
}

fn collect_locals(e: &Expr, out: &mut HashSet<Symbol>) {
    visit(e, &mut |x| match &*x.node {
        ExprNode::Assign { target: LValue::Var { name, .. }, .. } | ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => {
            out.insert(name.clone());
        }
        _ => {}
    });
}

fn assigns(e: &Expr, name: &Symbol) -> bool {
    let mut found = false;
    visit(e, &mut |x| {
        found |= matches!(&*x.node,
            ExprNode::Assign { target: LValue::Var { name: n, .. }, .. } | ExprNode::OpAssign { target: LValue::Var { name: n, .. }, .. } if n == name);
    });
    found
}

/// `__send__(:value_of, key)` / `send(:x)` on `self` with a literal
/// name is the direct call, which a private method also answers.
fn collapse_literal_send(e: &mut Expr) {
    let ExprNode::Send { recv, method, args, block, parenthesized } = &*e.node else { return };
    if !matches!(method.as_str(), "send" | "__send__") || recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef)) {
        return;
    }
    let Some(first) = args.first() else { return };
    let name = match &*first.node {
        ExprNode::Lit { value: Literal::Sym { value } } => value.clone(),
        _ => return,
    };
    if !valid_def_name(&name) {
        return;
    }
    *e.node = ExprNode::Send { recv: None, method: name, args: args[1..].to_vec(), block: block.clone(), parenthesized: *parenthesized };
}

fn valid_def_name(name: &Symbol) -> bool {
    let header = format!("def {name}\nend\n");
    let parsed = super::prism::parse_silent(header.as_bytes());
    let node = parsed.node();
    let Some(program) = node.as_program_node() else { return false };
    let nodes: Vec<_> = program.statements().body().iter().collect();
    let [node] = nodes.as_slice() else { return false };
    let Some(def) = node.as_def_node() else { return false };
    parsed.errors().next().is_none() && def.receiver().is_none() && def.name().as_slice() == name.as_str().as_bytes()
}
