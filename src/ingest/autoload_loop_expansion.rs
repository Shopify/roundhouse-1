//! A Zeitwerk `on_load` loop over literal model paths, expanded into each
//! model's own body.
//!
//! ```ruby
//! # components/platform/essentials/config/initializers/ledger_sharding.rb
//! LedgerSharding::MODELS.each_key do |model_name|
//!   Rails.autoloaders.main.on_load(model_name) do |klass, _abspath|
//!     klass.include(ModelUuidColumns)
//!     uuid_attributes = LedgerSharding::MODEL_UUID_ATTRIBUTES[model_name]
//!     klass.model_uuid_attributes(*uuid_attributes) if uuid_attributes
//!   end
//! end
//! ```
//!
//! Run as the callbacks they are, the block's `include` of a plain module
//! does not extend its `ClassMethods` on a compiled model and its
//! `attribute(...)` is refused, so none of the models got the concern's
//! macros. Written in each model's class body the same statements are
//! what a Core model's own `include ModelUuidColumns` and
//! `model_uuid_attributes :id, :shop_id` are, and the model lowering
//! (concern splice, `macro_replay`) treats them alike.
//!
//! The pass rewrites each callback statement, per model, as the text the
//! model body would hold: the literal path replaces the loop variable,
//! the block's first parameter is the class (`klass.include(M)` is
//! `include M`, `klass` read as a value is `self`), and a local the block
//! reads out of a literal hash the initializer's file defines
//! (`TABLE[model_name]`, so `model_uuid_attributes(*uuid_attributes) if
//! uuid_attributes` is the call with literal columns, or nothing). The
//! text, blank before the statement as `with_options`' expansion is so
//! spans stay the file's, goes through the model body classifier
//! (`model::ingest_model_body_items`), so `validates`, a callback or an
//! association is the typed item the model's own would be. The items are
//! appended to the model's body, where Zeitwerk would have run them (the
//! end of the constant's load). The loop is then consumed:
//! `initializer_programs` carries one `on_load_lifted` check per model in
//! its place, as for a lifted single callback.
//!
//! A call of a singleton method the same file defines, with the loop's
//! values for arguments (`LedgerSharding.apply_yugabyte_query_constraints!(
//! model_name, klass)`), is inlined the same way: its parameters are the
//! arguments, and its body's statements go through this rewrite in turn.
//! `if local = TABLE[param]` decides on the entry.
//!
//! It expands only when the loop walks a literal list of paths
//! (`initializer_programs::literal_strings`), every one a model of the
//! app, and every statement of the loop body is an
//! `Rails.autoloaders.main.on_load(<loop variable>)` with a block whose
//! statements ingest. Anything else is left as it was, registered with the
//! ported loader at run time.

use std::path::PathBuf;

use ruby_prism::Node;

use crate::app::App;
use crate::dialect::ModelBodyItem;
use crate::ident::{ClassId, Symbol};
use crate::vfs::Vfs;

use super::initializer_programs::{literal_constants, literal_strings, LiteralConstants};
use super::load_hook_includes::rails_autoloader;
use super::util::{class_name_path, constant_id_str, constant_path_segments_strs, flatten_statements, module_name_path};

/// The deepest chain of inlined singleton method calls.
const INLINE_DEPTH: usize = 4;

/// What the file defines the callbacks read: its literal hashes, by full
/// constant path (the string keys with the source text of the values),
/// and its singleton methods, by `Module.method`.
struct Defs<'pr> {
    tables: crate::det::HashMap<String, Vec<(String, String)>>,
    methods: crate::det::HashMap<String, Method<'pr>>,
}

struct Method<'pr> {
    params: Vec<String>,
    body: Vec<Node<'pr>>,
    /// The modules and classes the definition is nested in.
    nesting: Vec<String>,
}

impl Defs<'_> {
    /// The table a constant path names, as Ruby resolves it from `nesting`.
    fn table(&self, written: &str, nesting: &[String]) -> Option<&Vec<(String, String)>> {
        let written = written.trim_start_matches("::");
        (0..=nesting.len())
            .rev()
            .map(|n| nesting[..n].iter().map(String::as_str).chain([written]).collect::<Vec<_>>().join("::"))
            .find_map(|full| self.tables.get(&full))
    }

    fn method(&self, receiver: &str, name: &str, nesting: &[String]) -> Option<&Method<'_>> {
        let receiver = receiver.trim_start_matches("::");
        (0..=nesting.len())
            .rev()
            .map(|n| nesting[..n].iter().map(String::as_str).chain([receiver]).collect::<Vec<_>>().join("::"))
            .find_map(|full| self.methods.get(&format!("{full}.{name}")))
    }
}

/// What the names a statement reads are, for one model.
#[derive(Clone, Default)]
struct Env {
    /// Locals holding the class (`self` in the class body).
    classes: Vec<String>,
    /// Locals holding a string: the loop variable, a parameter bound to it.
    strings: crate::det::HashMap<String, String>,
    /// Locals folded to the text of a table entry (`None`: no entry).
    folded: crate::det::HashMap<String, Option<String>>,
}

/// One `on_load(<loop variable>)` block of the loop: its first parameter,
/// the statements.
struct Callback<'pr> {
    klass: Option<String>,
    body: Vec<Node<'pr>>,
}

/// What one model's callbacks add to it.
#[derive(Default)]
struct Out {
    items: Vec<ModelBodyItem>,
    primary_key: Option<Symbol>,
    composite_primary_key: Option<Vec<Symbol>>,
}

struct Site<'a, 'pr> {
    defs: &'a Defs<'pr>,
    /// The method names the app's modules and classes define: what a
    /// receiverless call in a class body can be a macro of.
    macros: &'a crate::det::HashSet<String>,
    owner: &'a ClassId,
    bytes: &'a [u8],
    file: &'a str,
}

pub(super) fn expand_autoload_loops<V: Vfs + ?Sized>(app: &mut App, vfs: &V, boot_initializers: &[PathBuf]) {
    for entry in boot_initializers {
        let Ok(bytes) = vfs.read(entry) else { continue };
        if !bytes.windows(11).any(|w| w == b"autoloaders") || !bytes.windows(7).any(|w| w == b"on_load") {
            continue;
        }
        let file = entry.display().to_string();
        let result = ruby_prism::parse(&bytes);
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        let constants = literal_constants(program.statements().body().iter().collect());
        let macros: crate::det::HashSet<String> =
            app.library_classes.iter().flat_map(|lc| lc.methods.iter().map(|m| m.name.as_str().to_string())).collect();
        let mut defs: Option<Defs<'_>> = None;
        for stmt in program.statements().body().iter() {
            let Some((var, names, callbacks)) = loop_callbacks(&stmt, &constants) else { continue };
            let indexes: Option<Vec<usize>> =
                names.iter().map(|name| app.models.iter().position(|m| m.name.0.as_str() == name)).collect();
            let Some(indexes) = indexes else { continue };
            if names.iter().any(|name| name.contains(['"', '\\', '#'])) {
                continue;
            }
            let defs = defs.get_or_insert_with(|| file_defs(program.statements().body().iter().collect()));
            // Every model's items first: a statement that does not
            // classify leaves the whole loop to run as written.
            let mut per_model: Vec<Out> = Vec::new();
            for (name, index) in names.iter().zip(&indexes) {
                let owner = app.models[*index].name.clone();
                let site = Site { defs, macros: &macros, owner: &owner, bytes: &bytes, file: &file };
                let mut out = Out::default();
                let ok = callbacks.iter().all(|callback| {
                    let mut env = Env::default();
                    env.strings.insert(var.clone(), name.clone());
                    env.classes.extend(callback.klass.clone());
                    site.run(&callback.body, &mut env, &[], &mut out, 0).is_some()
                });
                if !ok {
                    break;
                }
                per_model.push(out);
            }
            if per_model.len() != names.len() {
                continue;
            }
            let start = stmt.location().start_offset() as u32;
            for ((name, index), out) in names.iter().zip(&indexes).zip(per_model) {
                let model = &mut app.models[*index];
                model.body.extend(out.items);
                if out.primary_key.is_some() {
                    model.primary_key = out.primary_key;
                }
                if let Some(keys) = out.composite_primary_key {
                    model.composite_primary_key = keys;
                }
                app.lifted_autoload_callbacks.push(crate::app::LiftedAutoloadCallback {
                    cpath: name.clone(),
                    file: file.clone(),
                    start,
                });
            }
        }
    }
}

/// `<list>.each do |var| Rails.autoloaders.main.on_load(var) do |klass, _| … end … end`:
/// the loop variable, the paths it takes and the callbacks.
fn loop_callbacks<'pr>(stmt: &Node<'pr>, constants: &LiteralConstants) -> Option<(String, Vec<String>, Vec<Callback<'pr>>)> {
    let call = stmt.as_call_node()?;
    let method = constant_id_str(&call.name());
    if !matches!(method, "each" | "each_key") || call.arguments().is_some() {
        return None;
    }
    let block = call.block()?.as_block_node()?;
    let params = block.parameters()?.as_block_parameters_node()?.parameters()?;
    if params.optionals().iter().next().is_some() || params.rest().is_some() || params.keywords().iter().next().is_some() {
        return None;
    }
    let mut required = params.requireds().iter();
    let var = constant_id_str(&required.next()?.as_required_parameter_node()?.name()).to_string();
    if required.next().is_some() {
        return None;
    }
    let names = literal_strings(&call.receiver()?, method == "each_key", constants)?;
    if names.is_empty() {
        return None;
    }
    let mut callbacks = Vec::new();
    for inner in block.body().map(flatten_statements)? {
        let hook = inner.as_call_node()?;
        if constant_id_str(&hook.name()) != "on_load" || rails_autoloader(&hook.receiver()?).is_none() {
            return None;
        }
        let args: Vec<Node<'pr>> = hook.arguments()?.arguments().iter().collect();
        let [arg] = args.as_slice() else { return None };
        if constant_id_str(&arg.as_local_variable_read_node()?.name()) != var {
            return None;
        }
        let hook_block = hook.block()?.as_block_node()?;
        let klass = match hook_block.parameters() {
            None => None,
            Some(p) => {
                let p = p.as_block_parameters_node()?.parameters()?;
                if p.optionals().iter().next().is_some() || p.rest().is_some() || p.keywords().iter().next().is_some() {
                    return None;
                }
                p.requireds().iter().next().and_then(|r| r.as_required_parameter_node()).map(|r| constant_id_str(&r.name()).to_string())
            }
        };
        let body = hook_block.body().map(flatten_statements).unwrap_or_default();
        if body.is_empty() {
            return None;
        }
        callbacks.push(Callback { klass, body });
    }
    (!callbacks.is_empty()).then_some((var, names, callbacks))
}

impl<'pr> Site<'_, 'pr> {
    /// The statements as the model body holds them, added to `out`;
    /// `None` when one does not read as a model body's.
    fn run(&self, nodes: &[Node<'pr>], env: &mut Env, nesting: &[String], out: &mut Out, depth: usize) -> Option<()> {
        for node in nodes {
            // `local = TABLE[path]` binds the entry's text instead of
            // being a statement.
            if self.bind_table_local(node, env, nesting) {
                continue;
            }
            // `stmt if local` / `unless local`, or `if local = TABLE[path]`.
            if let Some(branch) = self.folded_condition(node, env, nesting) {
                self.run(&branch, env, nesting, out, depth)?;
                continue;
            }
            if self.inline_call(node, env, nesting, out, depth)? {
                continue;
            }
            self.statement(node, env, out)?;
        }
        Some(())
    }

    /// A call of a singleton method the file defines whose arguments are
    /// the class, the path or literals: its body, with the parameters bound.
    /// `Some(false)` when the statement is not such a call.
    fn inline_call(&self, node: &Node<'pr>, env: &Env, nesting: &[String], out: &mut Out, depth: usize) -> Option<bool> {
        let Some(call) = node.as_call_node() else { return Some(false) };
        let Some(receiver) = call.receiver().and_then(|r| constant_path_segments_strs(&r)) else { return Some(false) };
        let Some(method) = self.defs.method(&receiver.join("::"), constant_id_str(&call.name()), nesting) else {
            return Some(false);
        };
        if depth >= INLINE_DEPTH || call.block().is_some() {
            return None;
        }
        let args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if args.len() != method.params.len() {
            return None;
        }
        let mut inner = Env::default();
        for (param, arg) in method.params.iter().zip(&args) {
            if let Some(read) = arg.as_local_variable_read_node() {
                let name = constant_id_str(&read.name());
                if env.classes.iter().any(|c| c == name) {
                    inner.classes.push(param.clone());
                } else if let Some(text) = env.strings.get(name) {
                    inner.strings.insert(param.clone(), text.clone());
                } else {
                    return None;
                }
            } else if let Some(text) = arg.as_string_node() {
                inner.strings.insert(param.clone(), String::from_utf8_lossy(text.unescaped()).into_owned());
            } else {
                return None;
            }
        }
        self.run(&method.body, &mut inner, &method.nesting, out, depth + 1)?;
        Some(true)
    }

    /// `local = CONST[key]` over a table of the file, `key` a string the
    /// statement knows: the local is the entry's text.
    fn bind_table_local(&self, node: &Node<'pr>, env: &mut Env, nesting: &[String]) -> bool {
        let Some(write) = node.as_local_variable_write_node() else { return false };
        let Some(entry) = self.table_entry(&write.value(), env, nesting) else { return false };
        env.folded.insert(constant_id_str(&write.name()).to_string(), entry);
        true
    }

    /// `CONST[key]`'s entry (`Some(None)` when the table has no entry for
    /// the string); `None` when it is not a lookup this can decide.
    fn table_entry(&self, lookup: &Node<'pr>, env: &Env, nesting: &[String]) -> Option<Option<String>> {
        let lookup = lookup.as_call_node()?;
        if constant_id_str(&lookup.name()) != "[]" || lookup.block().is_some() {
            return None;
        }
        let table = self.defs.table(&constant_path_segments_strs(&lookup.receiver()?)?.join("::"), nesting)?;
        let args: Vec<Node<'pr>> = lookup.arguments()?.arguments().iter().collect();
        let [key] = args.as_slice() else { return None };
        let key = env.strings.get(constant_id_str(&key.as_local_variable_read_node()?.name()))?;
        Some(table.iter().find(|(k, _)| k == key).map(|(_, text)| text.clone()))
    }

    /// The statements an `if` / `unless` on a folded local (or on an
    /// assignment from a table lookup) runs; `None` when `node` is not
    /// such a condition. A condition that is false runs nothing.
    fn folded_condition(&self, node: &Node<'pr>, env: &mut Env, nesting: &[String]) -> Option<Vec<Node<'pr>>> {
        let (predicate, statements, inverse) = if let Some(cond) = node.as_if_node() {
            if cond.subsequent().is_some() {
                return None;
            }
            (cond.predicate(), cond.statements(), false)
        } else if let Some(cond) = node.as_unless_node() {
            if cond.else_clause().is_some() {
                return None;
            }
            (cond.predicate(), cond.statements(), true)
        } else {
            return None;
        };
        let truthy = if let Some(read) = predicate.as_local_variable_read_node() {
            env.folded.get(constant_id_str(&read.name()))?.is_some()
        } else if let Some(write) = predicate.as_local_variable_write_node() {
            let entry = self.table_entry(&write.value(), env, nesting)?;
            let truthy = entry.is_some();
            env.folded.insert(constant_id_str(&write.name()).to_string(), entry);
            truthy
        } else {
            return None;
        };
        if truthy == inverse {
            return Some(Vec::new());
        }
        Some(statements.map(|s| s.body().iter().collect()).unwrap_or_default())
    }

    /// One statement as the model body reads it.
    fn statement(&self, node: &Node<'pr>, env: &Env, out: &mut Out) -> Option<()> {
        let source = rewritten_source(node, env, self.bytes);
        let parsed = super::prism::parse(&source, self.file);
        let root = parsed.node();
        let program = root.as_program_node()?;
        let statements: Vec<Node<'_>> = program.statements().body().iter().collect();
        let [stmt] = statements.as_slice() else { return None };
        // Recognized into the model's key, not kept as a statement.
        if let Some(key) = super::model::parse_primary_key_decl(stmt) {
            out.primary_key = Some(key);
        } else if let Some(keys) = super::model::parse_composite_primary_key_decl(stmt) {
            out.composite_primary_key = Some(keys);
        } else {
            let items = super::model::ingest_model_body_items(stmt, self.owner, self.file, Vec::new(), None).ok()?;
            // What runs in the class body must be what the model lowering
            // reads as a class macro, or it would be dropped there: a
            // callback that does other work keeps running at load.
            if !items.iter().all(|item| self.is_macro(item)) {
                return None;
            }
            out.items.extend(items);
        }
        Some(())
    }
}

impl Site<'_, '_> {
    /// Whether a body item is one the model lowering reads: a typed
    /// declaration, or a receiverless call of `include` / `extend` /
    /// `prepend` or a method some module of the app defines.
    fn is_macro(&self, item: &ModelBodyItem) -> bool {
        let ModelBodyItem::Unknown { expr, .. } = item else { return true };
        match &*expr.node {
            crate::expr::ExprNode::Send { recv: None, method, .. } => {
                matches!(method.as_str(), "include" | "extend" | "prepend") || self.macros.contains(method.as_str())
            }
            _ => false,
        }
    }
}

/// The statement's text for the model body: blank before it (newlines
/// kept, so the spans are the file's) and with `edits` applied.
fn rewritten_source(node: &Node<'_>, env: &Env, bytes: &[u8]) -> Vec<u8> {
    let (start, end) = (node.location().start_offset(), node.location().end_offset());
    let mut visit = Edits { env, edits: Vec::new(), skip: None };
    // `klass.macro(…)` is the class body's `macro(…)`; a setter or an
    // operator keeps its receiver, `self`.
    if let Some(call) = node.as_call_node() {
        if let (Some(recv), Some(message)) = (call.receiver(), call.message_loc()) {
            let name = constant_id_str(&call.name());
            let plain = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && !name.ends_with('=');
            let on_class = recv.as_local_variable_read_node().is_some_and(|r| env.classes.iter().any(|c| c == constant_id_str(&r.name())));
            if plain && on_class {
                let from = recv.location().start_offset();
                visit.edits.push((from, message.start_offset(), " ".repeat(message.start_offset() - from)));
                visit.skip = Some((from, message.start_offset()));
            }
        }
    }
    ruby_prism::Visit::visit(&mut visit, node);
    let mut edits = visit.edits;
    edits.sort_by_key(|(from, _, _)| *from);
    let mut source: Vec<u8> = bytes[..start].iter().map(|&b| if b == b'\n' { b'\n' } else { b' ' }).collect();
    let mut at = start;
    for (from, to, text) in edits {
        if from < at {
            continue;
        }
        source.extend_from_slice(&bytes[at..from]);
        source.extend_from_slice(text.as_bytes());
        at = to;
    }
    source.extend_from_slice(&bytes[at..end]);
    source.push(b'\n');
    source
}

/// What a callback statement's reads become in the model body.
struct Edits<'a> {
    env: &'a Env,
    edits: Vec<(usize, usize, String)>,
    skip: Option<(usize, usize)>,
}

impl<'pr> ruby_prism::Visit<'pr> for Edits<'_> {
    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        let loc = node.location();
        let (from, to) = (loc.start_offset(), loc.end_offset());
        if self.skip.is_some_and(|(a, b)| a <= from && to <= b) {
            return;
        }
        let name = constant_id_str(&node.name());
        if self.env.classes.iter().any(|c| c == name) {
            self.edits.push((from, to, "self".into()));
        } else if let Some(text) = self.env.strings.get(name) {
            self.edits.push((from, to, format!("\"{text}\"")));
        } else if let Some(entry) = self.env.folded.get(name) {
            self.edits.push((from, to, entry.clone().unwrap_or_else(|| "nil".into())));
        }
    }

    /// `*local` over a folded array literal is its elements.
    fn visit_splat_node(&mut self, node: &ruby_prism::SplatNode<'pr>) {
        let entry = node
            .expression()
            .and_then(|e| e.as_local_variable_read_node().map(|r| constant_id_str(&r.name()).to_string()))
            .and_then(|name| self.env.folded.get(&name).cloned().flatten());
        if let Some(text) = entry.as_deref().map(str::trim).filter(|t| t.starts_with('[') && t.ends_with(']')) {
            let loc = node.location();
            self.edits.push((loc.start_offset(), loc.end_offset(), text[1..text.len() - 1].trim().to_string()));
            return;
        }
        ruby_prism::visit_splat_node(self, node);
    }
}

/// The string-keyed literal hashes the file's modules and classes (nested
/// as written) and its top level assign to constants, through `.freeze`,
/// and the singleton methods they define.
fn file_defs<'pr>(top: Vec<Node<'pr>>) -> Defs<'pr> {
    fn unfrozen<'pr>(node: Node<'pr>) -> Node<'pr> {
        if let Some(call) = node.as_call_node() {
            if constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() {
                if let Some(recv) = call.receiver() {
                    return unfrozen(recv);
                }
            }
        }
        node
    }
    fn method<'pr>(def: &ruby_prism::DefNode<'pr>, singleton: bool, nesting: &[String], out: &mut Defs<'pr>) {
        if !singleton && !def.receiver().is_some_and(|r| r.as_self_node().is_some()) {
            return;
        }
        let Some(params) = def.parameters() else {
            return insert_method(def, Vec::new(), nesting, out);
        };
        if params.optionals().iter().next().is_some() || params.rest().is_some() || params.keywords().iter().next().is_some() {
            return;
        }
        let names: Option<Vec<String>> = params
            .requireds()
            .iter()
            .map(|p| p.as_required_parameter_node().map(|p| constant_id_str(&p.name()).to_string()))
            .collect();
        if let Some(names) = names {
            insert_method(def, names, nesting, out);
        }
    }
    fn insert_method<'pr>(def: &ruby_prism::DefNode<'pr>, params: Vec<String>, nesting: &[String], out: &mut Defs<'pr>) {
        let key = format!("{}.{}", nesting.join("::"), constant_id_str(&def.name()));
        let body = def.body().map(flatten_statements).unwrap_or_default();
        out.methods.insert(key, Method { params, body, nesting: nesting.to_vec() });
    }
    fn walk<'pr>(stmts: Vec<Node<'pr>>, nesting: &[String], out: &mut Defs<'pr>) {
        for stmt in stmts {
            if let Some(module) = stmt.as_module_node() {
                if let Some(path) = module_name_path(&module) {
                    let mut inner = nesting.to_vec();
                    inner.extend(path);
                    walk(module.body().map(flatten_statements).unwrap_or_default(), &inner, out);
                }
            } else if let Some(class) = stmt.as_class_node() {
                if let Some(path) = class_name_path(&class) {
                    let mut inner = nesting.to_vec();
                    inner.extend(path);
                    walk(class.body().map(flatten_statements).unwrap_or_default(), &inner, out);
                }
            } else if let Some(singleton) = stmt.as_singleton_class_node() {
                if singleton.expression().as_self_node().is_some() {
                    for inner in singleton.body().map(flatten_statements).unwrap_or_default() {
                        if let Some(def) = inner.as_def_node() {
                            method(&def, true, nesting, out);
                        }
                    }
                }
            } else if let Some(def) = stmt.as_def_node() {
                method(&def, false, nesting, out);
            } else if let Some(write) = stmt.as_constant_write_node() {
                let value = unfrozen(write.value());
                let Some(hash) = value.as_hash_node() else { continue };
                let entries: Option<Vec<(String, String)>> = hash
                    .elements()
                    .iter()
                    .map(|element| {
                        let assoc = element.as_assoc_node()?;
                        let key = String::from_utf8_lossy(assoc.key().as_string_node()?.unescaped()).into_owned();
                        let text = String::from_utf8_lossy(assoc.value().location().as_slice()).into_owned();
                        Some((key, text))
                    })
                    .collect();
                if let Some(entries) = entries {
                    let full = nesting.iter().cloned().chain([constant_id_str(&write.name()).to_string()]).collect::<Vec<_>>().join("::");
                    out.tables.insert(full, entries);
                }
            }
        }
    }
    let mut out = Defs { tables: Default::default(), methods: Default::default() };
    walk(top, &[], &mut out);
    out
}
