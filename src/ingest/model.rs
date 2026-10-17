//! ActiveRecord model ingestion — parses one `app/models/*.rb` into
//! a `Model`, including validations, associations, callbacks, scopes,
//! and methods.

use indexmap::IndexMap;
use ruby_prism::Node;

use crate::dialect::{Comment, Model, ModelBodyItem};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, Literal};
use crate::naming::{camelize, singularize_camelize, snake_case};
use crate::schema::{ColumnType, Schema, Table};
use crate::span::Span;
use crate::ty::{Row, Ty};
use crate::{ClassId, Symbol, TableRef};

use super::composite_keys;
use super::expr::ingest_expr;
use super::visibility::{self, Visibility};
use super::util::{
    class_name_path, collect_comments, constant_id_str, constant_path_of, drain_comments_before,
    find_first_class, flatten_statements, source_has_blank_line, string_value, symbol_or_string_value,
    symbol_value,
};
use super::{IngestError, IngestResult};

/// Namespace module → the `table_name_prefix` it declares. Rails walks a
/// model's `module_parents` looking for this and prepends what it finds,
/// which is the ONLY thing that puts a namespace into a table name (the
/// name itself is demodulized — see `naming::rails_table_name`).
///
/// campfire's `app/models/push.rb` is four lines and exists solely for
/// this: without it `Push::Subscription` reads `subscriptions`, and its
/// table is `push_subscriptions`.
pub type TablePrefixes = crate::det::HashMap<String, String>;

mod enum_constants;
pub(in crate::ingest) use enum_constants::EnumConstants;

/// Scan one file for `module <Ns>; def self.table_name_prefix; "<p>"; end`,
/// or the same def inside the module's `class << self`.
/// Deliberately narrow: only a module-level singleton def whose body is a
/// single string literal. A computed prefix would have to run to be known,
/// and nothing in the corpus writes one.
pub fn ingest_table_name_prefixes(source: &[u8], file: &str) -> TablePrefixes {
    fn literal_prefix(def: &ruby_prism::DefNode<'_>) -> Option<String> {
        if constant_id_str(&def.name()) != "table_name_prefix" {
            return None;
        }
        let stmts = flatten_statements(def.body()?);
        if stmts.len() != 1 {
            return None;
        }
        string_value(&stmts[0])
    }
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut out = TablePrefixes::default();
    for (scope, module) in super::util::find_all_modules_with_scope(&root) {
        let Some(name_path) = super::util::module_name_path(&module) else {
            continue;
        };
        let mut full = scope;
        full.extend(name_path);
        let Some(body) = module.body() else { continue };
        let stmts = flatten_statements(body);
        // `extend self` makes every instance method a singleton one too, so a
        // nested module can override an outer `ledger_` with a plain
        // `def table_name_prefix = ""`.
        let extends_self = stmts.iter().any(|s| {
            s.as_call_node().is_some_and(|c| {
                c.receiver().is_none()
                    && constant_id_str(&c.name()) == "extend"
                    && c.arguments().is_some_and(|a| {
                        let args: Vec<_> = a.arguments().iter().collect();
                        args.len() == 1 && args[0].as_self_node().is_some()
                    })
            })
        });
        for stmt in stmts {
            if let Some(def) = stmt.as_def_node() {
                let singleton = match def.receiver() {
                    Some(r) => r.as_self_node().is_some(),
                    None => extends_self,
                };
                if !singleton {
                    continue;
                }
                if let Some(prefix) = literal_prefix(&def) {
                    out.insert(full.join("::"), prefix);
                }
            } else if let Some(sclass) = stmt.as_singleton_class_node() {
                if sclass.expression().as_self_node().is_none() {
                    continue;
                }
                let Some(sbody) = sclass.body() else { continue };
                for inner in flatten_statements(sbody) {
                    let Some(def) = inner.as_def_node() else { continue };
                    if def.receiver().is_some() {
                        continue;
                    }
                    if let Some(prefix) = literal_prefix(&def) {
                        out.insert(full.join("::"), prefix);
                    }
                }
            }
        }
    }
    out
}

/// `isolate_namespace Billing` in an engine body: Rails defines
/// `Billing.table_name_prefix` as the engine name and `_`
/// (`billing_`), unless the module already declares one. A
/// `Billing::Rate` with no `table_name` read `rates` without the
/// prefix, a table no schema has, and every column read on it was
/// `no known method`. The written module is the enclosing one it names
/// (`module Billing; class Engine`), else the name as written.
pub fn ingest_isolated_namespace_prefixes(source: &[u8], file: &str) -> TablePrefixes {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut out = TablePrefixes::default();
    for (scope, class) in super::util::find_all_classes_with_scope(&root) {
        let Some(body) = class.body() else { continue };
        let mut nesting = scope;
        nesting.extend(class_name_path(&class).unwrap_or_default());
        for stmt in flatten_statements(body) {
            let Some(call) = stmt.as_call_node() else { continue };
            if call.receiver().is_some() || constant_id_str(&call.name()) != "isolate_namespace" {
                continue;
            }
            let Some(arg) = call.arguments().and_then(|args| args.arguments().iter().next()) else { continue };
            let Some(written) = constant_path_of(&arg) else { continue };
            let rooted = arg.as_constant_path_node().is_some_and(|p| super::util::constant_path_is_rooted(&p));
            let enclosing = (!rooted)
                .then(|| nesting[..nesting.len().saturating_sub(1)].iter().rposition(|s| *s == written[0]))
                .flatten();
            let full = match enclosing {
                Some(at) => nesting[..at].iter().chain(&written).cloned().collect::<Vec<_>>().join("::"),
                None => written.join("::"),
            };
            let prefix = format!("{}_", crate::naming::underscore(&full).replace('/', "_"));
            out.insert(full, prefix);
        }
    }
    out
}

/// Each class one file declares with its own `self.table_name_prefix =
/// "…"`, by full name. Rails keeps the prefix in a class attribute, so a
/// model inherits it from its abstract base: an abstract
/// `PrefixedRecord` that sets `legacy_` gives its
/// models (`Line`, …) that name no table `legacy_lines`.
pub fn ingest_class_table_prefixes(source: &[u8], file: &str) -> TablePrefixes {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut out = TablePrefixes::default();
    for (mut scope, class) in super::util::find_all_classes_with_scope(&root) {
        let Some(prefix) = class.body().and_then(explicit_table_prefix) else { continue };
        let Some(path) = class_name_path(&class) else { continue };
        scope.extend(path);
        out.insert(scope.join("::"), prefix);
    }
    out
}

/// Rails' `compute_table_name`: a base class nested in a concrete model
/// prefixes its table with the parent's singular table name, after the
/// module prefix. Core's `DraftOrder::AdditionalFee` reads
/// `draft_order_additional_fees` and `Script::Version` `script_versions`;
/// both read `additional_fees`/`versions`, tables no schema has, and
/// every column was `no known method`. Runs after every model is
/// ingested, outermost first, so a chain sees its parent's final table.
/// A model that writes `self.table_name` keeps it; an STI subclass (its
/// superclass a concrete model) keeps the base's table.
pub(super) fn nest_tables_in_model_parents(
    models: &mut [Model],
    schema: &Schema,
    model_bases: &super::library_class::ModelBases,
) {
    let index: crate::det::HashMap<String, usize> =
        models.iter().enumerate().map(|(i, m)| (m.name.0.as_str().to_string(), i)).collect();
    let concrete = |name: &str| !model_bases.contains(name) && index.contains_key(name);
    let mut order: Vec<usize> = (0..models.len()).collect();
    order.sort_by_key(|&i| models[i].name.0.as_str().matches("::").count());
    for i in order {
        let name = models[i].name.0.as_str().to_string();
        let Some((parent, _)) = name.rsplit_once("::") else { continue };
        if !concrete(parent)
            || models[i].parent.as_ref().is_some_and(|p| concrete(p.0.as_str()))
            || writes_table_name(&models[i])
        {
            continue;
        }
        let parent_table = models[index[parent]].table.0.to_string();
        let undecorated = crate::naming::rails_table_name(&name);
        let Some(prefix) = models[i].table.0.as_str().strip_suffix(undecorated.as_str()) else { continue };
        let table = format!("{prefix}{}_{undecorated}", crate::naming::singularize(&parent_table));
        models[i].attributes = match schema.tables.get(&Symbol::from(table.as_str())) {
            Some(t) => row_from_table(t),
            None => Row::closed(),
        };
        models[i].table = TableRef(Symbol::from(table));
    }
}

/// Does the model's own class body write `self.table_name`? Unreadable
/// counts as written, so the model keeps the table ingest gave it.
fn writes_table_name(model: &Model) -> bool {
    let Some(text) = super::sources::text_of_file(model.span.file) else { return true };
    let result = super::prism::parse(text.as_bytes(), "");
    let root = result.node();
    super::util::find_all_classes_with_scope(&root)
        .into_iter()
        .find(|(_, class)| class.location().start_offset() == model.span.start as usize)
        .is_none_or(|(_, class)| {
            !matches!(class.body().map(|b| parse_table_name_decl(b, "")).transpose(), Ok(None | Some(None)))
        })
}

/// The prefix each abstract base passes down: its own, else its parent's.
/// `pairs` are the pre-pass's `(abstract class, parent)` pairs; a bare
/// parent is found the way Ruby finds it, the child's enclosing scopes first.
pub fn inherited_table_prefixes(own: &TablePrefixes, pairs: &[(String, String)]) -> TablePrefixes {
    let parents: crate::det::HashMap<&str, &str> = pairs.iter().map(|(c, p)| (c.as_str(), p.as_str())).collect();
    let known = |name: &str| own.contains_key(name) || parents.contains_key(name);
    let resolve = |child: &str, parent: &str| -> String {
        let mut segments: Vec<&str> = child.split("::").collect();
        segments.pop();
        while !segments.is_empty() {
            let candidate = format!("{}::{parent}", segments.join("::"));
            if known(&candidate) {
                return candidate;
            }
            segments.pop();
        }
        parent.to_string()
    };
    let mut out = TablePrefixes::default();
    for (child, _) in pairs {
        let mut at = child.clone();
        // A cycle guard: the walk follows `parent` links, so a cyclic
        // hierarchy must end somewhere.
        for _ in 0..16 {
            if let Some(prefix) = own.get(&at) {
                out.insert(child.clone(), prefix.clone());
                break;
            }
            let Some(parent) = parents.get(at.as_str()) else { break };
            at = resolve(&at, parent);
        }
    }
    out
}

/// `self.table_name_prefix = "three_d_secure_"` in a model body: the
/// class's own prefix, which wins over any namespace module's.
fn explicit_table_prefix(body: ruby_prism::Node<'_>) -> Option<String> {
    explicit_class_setting(body, "table_name_prefix=")
}

fn explicit_class_setting(body: ruby_prism::Node<'_>, setter: &str) -> Option<String> {
    for stmt in flatten_statements(body) {
        let Some(call) = stmt.as_call_node() else { continue };
        if constant_id_str(&call.name()) != setter {
            continue;
        }
        if call.receiver().and_then(|r| r.as_self_node()).is_none() {
            continue;
        }
        let args = call.arguments()?;
        let first = args.arguments().iter().next()?;
        if let Some(name) = string_value(&first).or_else(|| symbol_value(&first)) {
            return Some(name);
        }
    }
    None
}

/// Parse a single model file. The first class definition is treated as the
/// model; any schema-derived attributes are filled in from `schema`.
pub fn ingest_model(
    source: &[u8],
    file: &str,
    schema: &Schema,
    prefixes: &TablePrefixes,
) -> IngestResult<Option<Model>> {
    let mut constants = EnumConstants::default();
    constants.record(source, file);
    constants.finish();
    let bases = super::library_class::ModelBases::new();
    ingest_model_with_enum_constants(source, file, schema, prefixes, &TablePrefixes::default(), &constants, &bases)
}

/// `base_prefixes`: each abstract base's inherited `table_name_prefix`
/// ([`inherited_table_prefixes`]).
pub(super) fn ingest_model_with_enum_constants(
    source: &[u8],
    file: &str,
    schema: &Schema,
    prefixes: &TablePrefixes,
    base_prefixes: &TablePrefixes,
    enum_constants: &EnumConstants,
    model_bases: &super::library_class::ModelBases,
) -> IngestResult<Option<Model>> {
    ingest_model_class_at(source, file, schema, prefixes, base_prefixes, enum_constants, model_bases, None)
}

/// The byte offsets of the classes `source` declares that descend from an
/// ActiveRecord base, in source order: a file that declares several
/// models (a test-support helper's dummy models) is ingested once per
/// offset ([`ingest_model_class_at`]).
pub(super) fn active_record_class_offsets(source: &[u8], model_bases: &super::library_class::ModelBases) -> Vec<usize> {
    let result = super::prism::parse(source, "");
    let root = result.node();
    super::util::find_all_classes_with_scope(&root)
        .into_iter()
        .filter(|(scope, class)| {
            class
                .superclass()
                .and_then(|n| constant_path_of(&n))
                .is_some_and(|path| model_bases.contains(&model_bases.resolve_superclass(scope, &path)))
        })
        .map(|(_, class)| class.location().start_offset())
        .collect()
}

/// [`ingest_model_with_enum_constants`] for the class starting at byte
/// `class_at`, not the file's primary class.
#[allow(clippy::too_many_arguments)]
pub(super) fn ingest_model_class_at(
    source: &[u8],
    file: &str,
    schema: &Schema,
    prefixes: &TablePrefixes,
    base_prefixes: &TablePrefixes,
    enum_constants: &EnumConstants,
    model_bases: &super::library_class::ModelBases,
    class_at: Option<usize>,
) -> IngestResult<Option<Model>> {
    super::sources::register(file, &String::from_utf8_lossy(source));
    let result = super::prism::parse(source, file);
    let root = result.node();
    // Scope-aware: a model declared as `module Admin; class Report`
    // must ingest as `Admin::Report` (the compound `class Admin::Report`
    // spelling already carries its path). Falls back to the scopeless
    // finder for shapes the scoped walk doesn't cover.
    let (scope, class) = match class_at {
        Some(at) => match super::util::find_all_classes_with_scope(&root)
            .into_iter()
            .find(|(_, c)| c.location().start_offset() == at)
        {
            Some((s, c)) => (s, Some(c)),
            None => (Vec::new(), None),
        },
        None => match super::util::primary_class_with_scope(&root) {
            Some((s, c)) => (s, Some(c)),
            None => (Vec::new(), find_first_class(&root)),
        },
    };
    let Some(class) = class else {
        return Ok(None);
    };

    // Syntactic nesting, not every prefix of the class name: `module
    // Admin::Nested` does not put `Admin` in Ruby's lexical search path.
    let enum_owners = enum_constants.nesting
        .get(&(file.to_string(), class.location().start_offset()))
        .cloned().unwrap_or_default();
    let mut name_path = scope.clone();
    name_path.extend(class_name_path(&class).ok_or_else(|| IngestError::Unsupported {
        file: file.into(),
        message: "model class name must be a simple constant or path".into(),
    })?);
    let class_name = Symbol::from(name_path.join("::"));
    let owner = ClassId(class_name.clone());
    // Rails: `full_table_name_prefix + undecorated_table_name`. The
    // prefix comes from the nearest module parent that declares one,
    // searched innermost-out the way `module_parents` walks.
    let table_decl = class.body().map(|body| parse_table_name_decl(body, file)).transpose()?.flatten();
    let table_name = if let Some((name, _)) = &table_decl {
        name.clone()
    } else {
        let mut segments: Vec<&str> = class_name.as_str().split("::").collect();
        segments.pop();
        let mut prefix = class.body().and_then(explicit_table_prefix).unwrap_or_default();
        let mut from_module = false;
        while prefix.is_empty() && !segments.is_empty() {
            if let Some(p) = prefixes.get(&segments.join("::")) {
                prefix = p.clone();
                from_module = true;
                break;
            }
            segments.pop();
        }
        // No module parent declares one (an empty one still counts): the
        // class attribute its abstract base set.
        if prefix.is_empty()
            && !from_module
            && let Some(parent) = class.superclass().and_then(|n| constant_path_of(&n))
            && let Some(inherited) = base_prefixes.get(&model_bases.resolve_superclass(&scope, &parent))
        {
            prefix = inherited.clone();
        }
        format!("{prefix}{}", crate::naming::rails_table_name(class_name.as_str()))
    };

    // A superclass-less class is a tableless ActiveModel includer
    // (`classify_class_file`): the table its name would infer is not its
    // own. Core's `Taxes::TaxLine` (`include ActiveModel::Model` and
    // `attr_accessor`s) shares `tax_lines` with the `TaxLine` record, and
    // taking that table's columns made it a row-hydrating class whose
    // writers cast through a `_cast_column` it does not have.
    let table_name = if table_decl.is_none()
        && class.superclass().is_none()
        && schema.tables.contains_key(&Symbol::from(table_name.as_str()))
    {
        String::new()
    } else {
        table_name
    };
    let attributes = if let Some(table) = schema.tables.get(&Symbol::from(table_name.as_str())) {
        row_from_table(table)
    } else {
        Row::closed()
    };

    let mut comments = collect_comments(&result);
    // Discard comments that precede the `class` keyword — file-level
    // magic pragmas, doc blocks. We'll attach those to `Model` itself
    // when a fixture forces it. Comments inside the class body (after
    // `class Foo` but before its first statement) are preserved and
    // naturally attach to the first body item below.
    drain_comments_before(&mut comments, class.location().start_offset());
    let mut body: Vec<ModelBodyItem> = Vec::new();
    let mut enums: IndexMap<Symbol, Vec<(String, Literal)>> = IndexMap::new();
    let mut enum_defaults: IndexMap<Symbol, Literal> = IndexMap::new();
    let mut class_attr_defaults: IndexMap<Symbol, Expr> = IndexMap::new();
    let mut primary_key: Option<Symbol> = None;
    let mut composite_primary_key: Vec<Symbol> = Vec::new();
    let visibility = Visibility::resolve(class.body().as_ref(), file, None)?;
    let mut visibility_overrides = visibility.overrides();
    if let Some(class_body) = class.body() {
        let mut prev_end: Option<usize> = None;
        // Constants the class body assigns, for `enum :x, CONST` and for
        // labels spelled with a constant (`NAME => NAME`).
        let stmts = flatten_statements(class_body);
        let class_consts = ClassConsts::collect(&stmts, file);
        let resolve_constant = |node: &Node<'_>| enum_constants.resolve(node, &enum_owners);
        for statement in stmts {
            // `private attr_reader :x` declares like `attr_reader :x`; the
            // visibility pass already gave the generated methods the marker's.
            let definition = visibility::definition(&statement)
                .map(|d| d.as_node())
                .or_else(|| visibility::wrapped_declaration(&statement).map(|c| c.as_node()));
            let stmt = definition.as_ref().unwrap_or(&statement);
            if definition.is_none() && statement.as_call_node().is_some_and(|c| visibility::marker(&c)) {
                prev_end = Some(statement.location().end_offset());
                continue;
            }
            // Explicit names override convention before schema binding.
            // Like primary_key, the setter is consumed: lowering already
            // synthesizes table_name from Model::table for every target.
            if table_decl.as_ref().is_some_and(|(_, offset)| *offset == stmt.location().start_offset()) {
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `X = Struct.new(...) do … end` is the class `Model::X`,
            // which the library ingest of this file builds
            // (`factory_class`). As a body item its block's defs would
            // be lost.
            if super::factory_class::is_factory_class_write(stmt) {
                // Its block's constant writes are this model's
                // (`factory_class::block_constant_writes`), which a
                // model body does not carry.
                if !super::factory_class::block_constant_writes(stmt).is_empty() {
                    return Err(IngestError::Unsupported {
                        file: file.into(),
                        message: "constant assigned inside a Struct.new/Data.define/Class.new body block in a model \
                                  (it is the model's constant) is not modeled"
                            .into(),
                    });
                }
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `self.primary_key = "key"` is recognized into
            // `Model::primary_key` instead of being kept as a body item:
            // the lowering synthesizes a reader from it, and re-emitting
            // the assignment verbatim would call a writer no target's
            // runtime defines. Comments above it fall through to the
            // next statement.
            if let Some(pk) = parse_primary_key_decl(&stmt) {
                primary_key = Some(pk);
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `self.primary_key = [:shop_id, :id]`: a composite key, in
            // the order written (`Model::composite_primary_key`).
            if let Some(keys) = parse_composite_primary_key_decl(&stmt) {
                composite_primary_key = keys;
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            let stmt_start = stmt.location().start_offset();
            let leading_area_start =
                comments.first().map(|(off, _)| *off).filter(|off| *off < stmt_start)
                    .unwrap_or(stmt_start);
            let leading = drain_comments_before(&mut comments, stmt_start);
            let leading_blank = prev_end
                .map(|pe| source_has_blank_line(source, pe, leading_area_start))
                .unwrap_or(false);
            // `concerning :Topic do … end` (`util::ConcerningCall`): the
            // block's constants are the model's, written where the call
            // stands, then `include Topic`, the module the library-class
            // pass over this file builds from the block.
            if let Some(concerning) = super::util::concerning_call(&stmt) {
                let mut items = Vec::new();
                concerning_model_items(&concerning, &owner, file, true, &mut items)?;
                let (mut leading, mut blank) = (leading, leading_blank);
                for mut item in items {
                    *item.leading_comments_mut() = std::mem::take(&mut leading);
                    item.set_leading_blank_line(std::mem::take(&mut blank));
                    body.push(item);
                }
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `enum :status, %i[…]` — one statement standing for a
            // scope + predicate + bang writer per label, so it expands
            // in the walk loop for the same reason `class << self` does.
            if let Some(call) = stmt.as_call_node() {
                match expand_class_body_dsl(&call, file, &leading, &class_consts, &resolve_constant) {
                    Ok(Some(ClassBodyExpansion::DelegatedType(expanded))) => {
                        let mut blank = leading_blank;
                        for mut item in expanded {
                            item.set_leading_blank_line(std::mem::take(&mut blank));
                            body.push(item);
                        }
                        prev_end = Some(stmt.location().end_offset());
                        continue;
                    }
                    Ok(Some(ClassBodyExpansion::Enum(expanded))) => {
                        if let Some(d) = expanded.default {
                            enum_defaults.insert(expanded.column.clone(), d);
                        }
                        if let Some(mapping) = expanded.mapping {
                            enums.insert(expanded.column, mapping);
                        }
                        let mut blank = leading_blank;
                        for mut item in expanded.items {
                            item.set_leading_blank_line(std::mem::take(&mut blank));
                            body.push(item);
                        }
                        prev_end = Some(stmt.location().end_offset());
                        continue;
                    }
                    Ok(None) => {}
                    Err(err) if super::survey::is_active() => {
                        super::survey::record(&err);
                        prev_end = Some(stmt.location().end_offset());
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
            // `class << self … end` — the singleton block's defs are
            // class methods of the model (`Room.create_for`). A model's
            // IR body is one item per statement, so expand the block in
            // place; `ingest_model_body_item` returns a single item and
            // can't. Library classes get the same treatment one level
            // down, in `walk_decl_body`.
            //
            // `alias_method :to, :from` of an instance method this body
            // already defined copies that def, as the library walk does
            // (`library_class::alias_source`), and the copy takes the
            // visibility Ruby gives the alias: the original's
            // (`Visibility`'s walk). Not one whose body calls `super`:
            // Ruby's alias reaches the original name's super method and a
            // renamed copy would reach its own. Anything else stays an
            // unknown item.
            if let Some(mut copy) = stmt.as_call_node().and_then(|call| model_alias_method_copy(&call, &body)) {
                visibility.apply(&statement, &mut copy);
                body.push(ModelBodyItem::Method {
                    method: copy,
                    leading_comments: leading,
                    leading_blank_line: leading_blank,
                });
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `alias new old` (keyword form). Columns, associations,
            // `attr_accessor` and `attribute` are synthesized later, in
            // `lower::model_to_library` — this walk only sees
            // hand-written `def`s and enum-generated methods, so
            // resolving here would wrongly refuse an alias naming any
            // of those (#481: an enum alias failed even though the
            // model itself emitted fine). Record it as a typed body
            // item, in order, and let `lower_model_to_library_class`
            // resolve it after synthesis, with snapshot semantics.
            if let Some(alias) = stmt.as_alias_method_node() {
                let to = super::library_class::alias_keyword_name(&alias.new_name());
                let from = super::library_class::alias_keyword_name(&alias.old_name());
                if let Some((to, from)) = to.zip(from) {
                    let span = Span {
                        file: super::sources::file_id(file),
                        start: stmt.location().start_offset() as u32,
                        end: stmt.location().end_offset() as u32,
                    };
                    body.push(ModelBodyItem::Alias {
                        new: crate::ident::Symbol::from(to.as_str()),
                        old: crate::ident::Symbol::from(from.as_str()),
                        leading_comments: leading,
                        leading_blank_line: leading_blank,
                        span,
                    });
                    prev_end = Some(stmt.location().end_offset());
                    continue;
                }
                // Neither side is a plain name (e.g. a global variable
                // alias, `alias $a $b`) — not a method alias at all.
                return Err(IngestError::Unsupported {
                    file: file.into(),
                    message: "alias names a method this body has not defined".into(),
                });
            }
            if let Some(sc) = stmt.as_singleton_class_node() {
                match ingest_singleton_class_body(&sc, &owner, file, &visibility) {
                    Ok(singleton) => {
                        let mut leading = leading;
                        let mut blank = leading_blank;
                        let items = singleton
                            .methods
                            .into_iter()
                            .map(|method| ModelBodyItem::Method {
                                method,
                                leading_comments: Vec::new(),
                                leading_blank_line: false,
                            })
                            .chain(singleton.class_body.into_iter().map(|expr| {
                                ModelBodyItem::Unknown {
                                    expr,
                                    leading_comments: Vec::new(),
                                    leading_blank_line: false,
                                }
                            }));
                        for mut item in items {
                            *item.leading_comments_mut() = std::mem::take(&mut leading);
                            item.set_leading_blank_line(std::mem::take(&mut blank));
                            body.push(item);
                        }
                        prev_end = Some(stmt.location().end_offset());
                        continue;
                    }
                    Err(err) if super::survey::is_active() => {
                        super::survey::record(&err);
                        prev_end = Some(stmt.location().end_offset());
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
            // Survey mode: an unsupported *item* (an exotic scope form, a
            // DSL shape the classifier rejects) costs itself, not the
            // whole class — record the gap and keep walking, mirroring
            // the expr-level recovery inside `ingest_expr`. Before this
            // gate, one such item silently dropped the entire model
            // (Mastodon lost `Status` to a single spelled-out scope
            // lambda). Strict mode still aborts.
            // A nested class or module is a class of its own, not a body
            // item: `class Row < T::Struct` inside a model or controller
            // is the same declaration it would be in `app/services`, and
            // the library-class pass over this very file registers it
            // under its qualified name. Reaching the expression ingester
            // with it aborted the whole file.
            //
            // Exception: a nested `class JSON` / `module JSON` shadows
            // bare `JSON` for `serialize …, coder: JSON` — leave a
            // synthetic Const-assign marker so lower::serialize can see
            // the shadow without treating the namespace as a body item.
            if let Some(path) = stmt
                .as_class_node()
                .and_then(|c| constant_path_of(&c.constant_path()))
                .or_else(|| {
                    stmt.as_module_node()
                        .and_then(|m| constant_path_of(&m.constant_path()))
                })
            {
                let path_syms: Vec<Symbol> = path.iter().map(|s| Symbol::from(s.as_str())).collect();
                if crate::lower::serialize::const_path_shadows_bare_json(
                    &path_syms,
                    owner.0.as_str(),
                ) {
                    body.push(ModelBodyItem::Unknown {
                        expr: Expr::new(
                            Span::synthetic(),
                            ExprNode::Assign {
                                target: crate::expr::LValue::Const {
                                    path: vec![Symbol::from("JSON")],
                                },
                                value: Expr::new(
                                    Span::synthetic(),
                                    ExprNode::Lit {
                                        value: crate::expr::Literal::Nil,
                                    },
                                ),
                            },
                        ),
                        leading_comments: Vec::new(),
                        leading_blank_line: false,
                    });
                }
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            // `@x = {}` in the class body writes the class object's own
            // ivar where it stands, as a library class's does
            // (`library_class::with_class_body_ivar_writes`). Core's
            // `Event` seeds `@admin_timeline_subject_types = {}` for its
            // `class << self` accessor; left Unknown, it was dropped.
            if stmt.as_instance_variable_write_node().is_some()
                || stmt.as_instance_variable_or_write_node().is_some()
                || stmt.as_instance_variable_and_write_node().is_some()
                || stmt.as_instance_variable_operator_write_node().is_some()
            {
                match ingest_expr(&stmt, file) {
                    Ok(expr) => body.push(ModelBodyItem::ClassBody {
                        expr,
                        leading_comments: leading,
                        leading_blank_line: leading_blank,
                    }),
                    Err(err) if super::survey::is_active() => super::survey::record(&err),
                    Err(err) => return Err(err),
                }
                prev_end = Some(stmt.location().end_offset());
                continue;
            }
            let items = match in_model_class_body(|| match constant_writes_in_list(&stmt) {
                // `[A = "a", B = "b"].freeze`: Ruby assigns each constant as
                // the list is built, and the list itself is not kept.
                Some(writes) => {
                    let mut items = Vec::new();
                    let mut leading = Some(leading);
                    for write in &writes {
                        let own = leading.take().unwrap_or_default();
                        items.extend(ingest_model_body_items(write, &owner, file, own, Some(&mut class_attr_defaults))?);
                    }
                    Ok(items)
                }
                None => ingest_model_body_items(&stmt, &owner, file, leading, Some(&mut class_attr_defaults)),
            }) {
                Ok(items) => items,
                Err(err) if super::survey::is_active() => {
                    super::survey::record(&err);
                    prev_end = Some(stmt.location().end_offset());
                    continue;
                }
                Err(err) => return Err(err),
            };
            // A multi-attribute `validates` expands to one item per
            // attribute; only the first carries the blank line that
            // separated the declaration from what came before it.
            let mut unknown_seen = false;
            let mut unknown_refused = false;
            for (i, mut item) in items.into_iter().enumerate() {
                if let ModelBodyItem::Method { method, .. } = &mut item {
                    visibility.apply(&statement, method);
                } else if let ModelBodyItem::Unknown { .. } = &item {
                    // The statement's unknown items share one statement, so
                    // its overrides are read once; a refused one (survey
                    // mode) drops every unknown item it produced.
                    if unknown_refused {
                        continue;
                    }
                    if !std::mem::replace(&mut unknown_seen, true) {
                        match visibility.model_item_overrides(&statement, file) {
                            Ok(overrides) => visibility_overrides.extend(overrides),
                            Err(err) => {
                                super::survey::continue_or_fail(err)?;
                                unknown_refused = true;
                                continue;
                            }
                        }
                    }
                }
                item.set_leading_blank_line(leading_blank && i == 0);
                body.push(item);
            }
            prev_end = Some(stmt.location().end_offset());
        }
    }

    let parent_span = class
        .superclass()
        .map(|n| super::util::node_span(&n, file))
        .unwrap_or_default();
    let parent = class.superclass().and_then(|n| {
        constant_path_of(&n).map(|p| {
            let resolved = model_bases.resolve_superclass(&scope, &p);
            ClassId(Symbol::from(model_bases.emit_superclass(&resolved)))
        })
    });

    let class_loc = class.location();
    Ok(Some(Model {
        name: owner,
        parent,
        parent_span,
        sti_subclass_names: Vec::new(),
        retypable_attributes: Vec::new(),
        subclass_default_scope: false,
        subclass_declarations: Vec::new(),
        defaulted_attributes: Vec::new(),
        attribute_aliases: Vec::new(),
        query_constraints: Vec::new(),
        claimed_body_calls: Vec::new(),
        after_transaction_callbacks_in_order: false,
        ignored_columns: Vec::new(),
        composite_primary_key,
        i18n: Default::default(),
        table: TableRef(Symbol::from(table_name)),
        primary_key,
        attributes,
        body,
        enums,
        enum_defaults,
        class_attr_defaults,
        lexical_json_shadow: enum_constants.shadows_bare_json(&enum_owners),
        span: Span {
            file: super::sources::file_id(file),
            start: class_loc.start_offset() as u32,
            end: class_loc.end_offset() as u32,
        },
        visibility_overrides,
    }))
}

/// `alias_method :to, :from` (literal names) of the last instance def of
/// `from` already in `body`, renamed `to`. None when the def calls
/// `super`.
fn model_alias_method_copy(call: &ruby_prism::CallNode<'_>, body: &[ModelBodyItem]) -> Option<crate::dialect::MethodDef> {
    fn calls_super(expr: &Expr) -> bool {
        let mut found = matches!(&*expr.node, ExprNode::Super { .. });
        expr.node.for_each_child(&mut |child| found = found || calls_super(child));
        found
    }
    if call.receiver().is_some() || call.block().is_some() || constant_id_str(&call.name()) != "alias_method" {
        return None;
    }
    let args: Vec<_> = call.arguments()?.arguments().iter().collect();
    let [to, from] = args.as_slice() else { return None };
    let (to, from) = (symbol_value(to)?, symbol_value(from)?);
    let source = body.iter().rev().find_map(|item| match item {
        ModelBodyItem::Method { method, .. }
            if method.name.as_str() == from && method.receiver == crate::dialect::MethodReceiver::Instance =>
        {
            Some(method)
        }
        _ => None,
    })?;
    if calls_super(&source.body) {
        return None;
    }
    let mut copy = source.clone();
    copy.name = Symbol::from(to.as_str());
    Some(copy)
}

/// Classify one class-body statement into its `ModelBodyItem` variant.
/// One statement of a model body, as one or MORE items.
///
/// `validates :title, :url, presence: true` declares one Validation per
/// ATTRIBUTE, and `ingest_model_body_item` can only answer with one —
/// its own comment said the tail was dropped and that "no real fixture
/// triggers this yet". campfire's `Opengraph::Metadata` triggers it:
/// `validates_presence_of :title, :url, :description` must fault all
/// three, and the test reads `errors.full_messages` for two of them.
///
/// The `validates_*_of` family is the same declaration in Rails' older
/// spelling — `validates_presence_of :a` IS `validates :a, presence:
/// true` — so both are read by `ingest::validates`, which answers an
/// explicit `ValidationRule::Unsupported` for any validator or option it
/// does not model rather than dropping it.
pub(super) fn ingest_model_body_items(
    stmt: &Node<'_>,
    owner: &ClassId,
    file: &str,
    leading_comments: Vec<Comment>,
    class_attr_defaults: Option<&mut IndexMap<Symbol, Expr>>,
) -> IngestResult<Vec<ModelBodyItem>> {
    let span = Span {
        file: super::sources::file_id(file),
        start: stmt.location().start_offset() as u32,
        end: stmt.location().end_offset() as u32,
    };
    if let Some(call) = stmt.as_call_node() {
        // `class_attribute :x, …` — the defs it makes, then the seed
        // it runs where it stands (`ingest::class_attribute`). Also
        // `self.class_attribute …` and `T.unsafe(self).class_attribute …`.
        if let Some(decl) = super::class_attribute::parse(&call, file) {
            let mut out: Vec<ModelBodyItem> = super::class_attribute::methods(owner, &decl)
                .into_iter()
                .map(|method| ModelBodyItem::Method { method, leading_comments: Vec::new(), leading_blank_line: false })
                .collect();
            out.extend(super::class_attribute::seeds(&decl).into_iter().map(|expr| ModelBodyItem::ClassBody {
                expr,
                leading_comments: Vec::new(),
                leading_blank_line: false,
            }));
            if let Some(first) = out.first_mut() {
                *first.leading_comments_mut() = leading_comments;
            }
            return Ok(out);
        }
        if call.receiver().is_none() {
            let name = constant_id_str(&call.name()).to_string();
            if name == "with_options" && call.block().is_some_and(|b| b.as_block_node().is_some()) {
                let mut defaults = class_attr_defaults;
                if let Some(sources) = with_options_calls(&call, file) {
                    let mut out = Vec::new();
                    let mut comments = Some(leading_comments.clone());
                    let mut complete = true;
                    for (source, start) in &sources {
                        let result = super::prism::parse(source, file);
                        let root = result.node();
                        let Some(inner) = root
                            .as_program_node()
                            .into_iter()
                            .flat_map(|p| p.statements().body().iter().collect::<Vec<_>>())
                            .find(|s| s.location().start_offset() == *start)
                        else {
                            complete = false;
                            break;
                        };
                        let leading = comments.take().unwrap_or_default();
                        let items = ingest_model_body_items(&inner, owner, file, leading, defaults.as_deref_mut())?;
                        // A class macro the statement calls stays an
                        // Unknown item, read as an association later
                        // from its call; that read needs the merged text.
                        if items.iter().any(|item| matches!(item, ModelBodyItem::Unknown { .. })) {
                            MERGED_CALLS.with_borrow_mut(|m| m.insert((super::sources::file_id(file).0, *start as u32), source.clone()));
                        }
                        out.extend(items);
                    }
                    if complete {
                        return Ok(out);
                    }
                }
                super::survey::record(&IngestError::Unsupported {
                    file: file.to_string(),
                    message: format!(
                        "with_options on line {} not expanded: only literal symbol-keyed options over plain calls are merged, so its declarations do not run",
                        super::sources::line_at(file, call.location().start_offset()).unwrap_or(0)
                    ),
                });
                return Ok(vec![ingest_model_body_item(stmt, owner, file, leading_comments)?]);
            }
            if let Some(parsed) = super::validates::parse_validation_call(&call, file)? {
                return Ok(parsed
                    .into_iter()
                    .enumerate()
                    .map(|(i, validation)| ModelBodyItem::Validation {
                        validation,
                        // The declaration's comments belong to the
                        // first of the items it expands to.
                        leading_comments: if i == 0 {
                            leading_comments.clone()
                        } else {
                            Vec::new()
                        },
                        leading_blank_line: false,
                        span,
                    })
                    .collect());
            }
            // `cattr_*` / `mattr_*` — same class-attr expansion library
            // ingest applies. Plain `attr_*` stays Unknown: concern
            // `included` blocks share this walker, and expanding those
            // into Method items breaks `included_has_accessor` /
            // visibility gating for `private;` inside `included`.
            if matches!(
                name.as_str(),
                "cattr_reader"
                    | "cattr_writer"
                    | "cattr_accessor"
                    | "mattr_reader"
                    | "mattr_writer"
                    | "mattr_accessor"
            ) {
                match expand_mattr_cattr(&call, owner, file, span, leading_comments.clone())? {
                    Some(out) => return Ok(out),
                    None => {} // unsupported options — fall through Unknown
                }
            }
        }
    }
    Ok(vec![ingest_model_body_item(stmt, owner, file, leading_comments)?])
}

thread_local! {
    /// The merged copy of a file a `with_options` call's inner statement was
    /// read from, by file and the statement's start offset, for the ones
    /// that stay Unknown items.
    static MERGED_CALLS: std::cell::RefCell<crate::det::HashMap<(u32, u32), Vec<u8>>> = Default::default();
}

/// The text a `with_options` inner call was read from, options merged;
/// spans in it are the file's.
pub(super) fn merged_call_source(file: crate::span::FileId, start: u32) -> Option<Vec<u8>> {
    MERGED_CALLS.with_borrow(|m| m.get(&(file.0, start)).cloned())
}

/// Drop the merged copies once the Unknown items are read.
pub(super) fn forget_merged_calls() {
    MERGED_CALLS.with_borrow_mut(|m| m.clear());
}

/// Whether `stmt` is a `with_options` block the model walk expands
/// ([`with_options_calls`]): its inner calls come back with the outer
/// options merged.
pub(super) fn expands_with_options(stmt: &Node<'_>, file: &str) -> bool {
    let Some(call) = stmt.as_call_node() else { return false };
    call.receiver().is_none()
        && constant_id_str(&call.name()) == "with_options"
        && call.block().is_some_and(|b| b.as_block_node().is_some())
        && with_options_calls(&call, file).is_some()
}

/// `with_options inverse_of: :account, foreign_key: :account_id do … end`
/// (or `do |o| o.has_many … end`): Rails' `OptionMerger` deep-merges the
/// options into every call in the block, the call's own options winning.
/// Core's `ShopifyPayments::Account` declares `capabilities` and ten other
/// associations that way; the block was one Unknown item, so they were
/// never registered (every read `no known method`) and, like the
/// `with_options presence: true do validates …` rules in 35 Core models,
/// did not run in the emitted app.
///
/// Each inner call is re-read from a copy of the file whose bytes before
/// it are blanked (newlines kept) and whose option hashes get the missing
/// outer pairs appended, so its own spans and lines are the file's. The
/// copies come back as `(source, start offset)`. `None` declines: options
/// that are not literal symbol-keyed pairs, a statement that is not a
/// plain call, a bare call beside a block parameter (Rails sends it to
/// `self` unmerged), a call whose only argument is a proc (Rails wraps
/// it), or a nested `with_options`.
fn with_options_calls(call: &ruby_prism::CallNode<'_>, file: &str) -> Option<Vec<(Vec<u8>, usize)>> {
    fn symbol_key(pair: &Node<'_>) -> Option<Vec<u8>> {
        let assoc = pair.as_assoc_node()?;
        assoc.key().as_symbol_node().map(|s| s.unescaped().to_vec())
    }
    fn pairs<'pr>(hash: &Node<'pr>) -> Option<Vec<Node<'pr>>> {
        let elements = match (hash.as_keyword_hash_node(), hash.as_hash_node()) {
            (Some(h), _) => h.elements(),
            (_, Some(h)) => h.elements(),
            _ => return None,
        };
        let out: Vec<Node<'pr>> = elements.iter().collect();
        out.iter().all(|p| symbol_key(p).is_some()).then_some(out)
    }
    // Insertions `(offset, text)` that deep-merge `outer` into `inner`.
    fn merge(outer: &[Node<'_>], inner: &Node<'_>, edits: &mut Vec<(usize, String)>) -> Option<()> {
        let own = pairs(inner)?;
        let mut missing = Vec::new();
        for pair in outer {
            let key = symbol_key(pair)?;
            match own.iter().find(|p| symbol_key(p).as_deref() == Some(key.as_slice())) {
                None => missing.push(String::from_utf8_lossy(pair.location().as_slice()).into_owned()),
                Some(mine) => {
                    let (theirs, mine) = (pair.as_assoc_node()?.value(), mine.as_assoc_node()?.value());
                    if theirs.as_hash_node().is_some() && mine.as_hash_node().is_some() {
                        merge(&pairs(&theirs)?, &mine, edits)?;
                    }
                }
            }
        }
        if missing.is_empty() {
            return Some(());
        }
        let text = missing.join(", ");
        match inner.as_hash_node() {
            Some(h) => {
                let sep = if own.is_empty() { " " } else { ", " };
                edits.push((h.closing_loc().start_offset(), format!("{sep}{text} ")));
            }
            None => edits.push((inner.location().end_offset(), format!(", {text}"))),
        }
        Some(())
    }

    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    let [options] = args.as_slice() else { return None };
    let outer = pairs(options)?;
    let block = call.block()?.as_block_node()?;
    let param = match block.parameters() {
        None => None,
        Some(params) => {
            let params = params.as_block_parameters_node()?.parameters()?;
            let required: Vec<Node<'_>> = params.requireds().iter().collect();
            let [only] = required.as_slice() else { return None };
            if params.optionals().iter().next().is_some() || params.rest().is_some() || params.keywords().iter().next().is_some() {
                return None;
            }
            Some(only.as_required_parameter_node()?.name().as_slice().to_vec())
        }
    };
    let text = super::sources::text_of_file(super::sources::file_id(file))?;
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for stmt in flatten_statements(block.body()?) {
        let inner = stmt.as_call_node()?;
        if constant_id_str(&inner.name()) == "with_options" {
            return None;
        }
        let (start, end) = (stmt.location().start_offset(), stmt.location().end_offset());
        if bytes.get(start..end) != Some(stmt.location().as_slice()) {
            return None;
        }
        let mut blank: Vec<(usize, usize)> = Vec::new();
        // Where the re-read call begins: past a blanked `o.`.
        let mut call_start = start;
        match (inner.receiver(), &param) {
            (None, None) => {}
            (Some(recv), Some(name)) => {
                let local = recv.as_local_variable_read_node()?;
                if local.name().as_slice() != name.as_slice() {
                    return None;
                }
                call_start = inner.message_loc()?.start_offset();
                blank.push((recv.location().start_offset(), call_start));
            }
            _ => return None,
        }
        let mut edits: Vec<(usize, String)> = Vec::new();
        let own: Vec<Node<'_>> = inner.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let last = own.last()?;
        if last.as_keyword_hash_node().is_some() || last.as_hash_node().is_some() {
            merge(&outer, last, &mut edits)?;
        } else if own.len() == 1 && last.as_lambda_node().is_some() {
            return None;
        } else {
            let text: Vec<String> =
                outer.iter().map(|p| String::from_utf8_lossy(p.location().as_slice()).into_owned()).collect();
            edits.push((last.location().end_offset(), format!(", {}", text.join(", "))));
        }
        edits.sort_by_key(|(at, _)| *at);
        let mut source: Vec<u8> = bytes[..start].iter().map(|&b| if b == b'\n' { b'\n' } else { b' ' }).collect();
        let mut at = start;
        for (offset, insert) in edits {
            source.extend(bytes[at..offset].iter().enumerate().map(|(i, &b)| {
                let pos = at + i;
                if blank.iter().any(|(s, e)| (*s..*e).contains(&pos)) { b' ' } else { b }
            }));
            source.extend_from_slice(insert.as_bytes());
            at = offset;
        }
        source.extend(bytes[at..end].iter().enumerate().map(|(i, &b)| {
            let pos = at + i;
            if blank.iter().any(|(s, e)| (*s..*e).contains(&pos)) { b' ' } else { b }
        }));
        source.push(b'\n');
        out.push((source, call_start));
    }
    Some(out)
}

/// `mattr_*` / `cattr_*`: the accessors and the seed
/// (`ingest::cattr`), one `@@name` shared by the class, its subclasses
/// and their instances. The seed is a class-body statement where the
/// declaration stands, so a concern's `included do` carries it into each
/// includer with the accessors. A declaration `cattr::declaration` does
/// not model stays unexpanded.
fn expand_mattr_cattr(
    call: &ruby_prism::CallNode<'_>,
    owner: &ClassId,
    file: &str,
    span: Span,
    leading_comments: Vec<Comment>,
) -> IngestResult<Option<Vec<ModelBodyItem>>> {
    let kw = constant_id_str(&call.name());
    let Ok(declaration) = super::cattr::declaration(call, &kw, file) else { return Ok(None) };
    let mut out: Vec<ModelBodyItem> = declaration
        .methods(owner)
        .into_iter()
        .map(|method| ModelBodyItem::Method { method, leading_comments: Vec::new(), leading_blank_line: false })
        .collect();
    out.extend(declaration.seeds(span).into_iter().map(|expr| ModelBodyItem::ClassBody {
        expr,
        leading_comments: Vec::new(),
        leading_blank_line: false,
    }));
    if let Some(first) = out.first_mut() {
        *first.leading_comments_mut() = leading_comments;
    }
    Ok(Some(out))
}

/// `leading_comments` is attached regardless of variant so every item
/// keeps its inline docs.
thread_local! {
    /// Set while a model's own class-body statement is classified, as
    /// opposed to a concern's `included do` or a `class_eval` body.
    static IN_MODEL_CLASS_BODY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn in_model_class_body<T>(f: impl FnOnce() -> T) -> T {
    let outer = IN_MODEL_CLASS_BODY.replace(true);
    let out = f();
    IN_MODEL_CLASS_BODY.set(outer);
    out
}

pub(super) fn ingest_model_body_item(
    stmt: &Node<'_>,
    owner: &ClassId,
    file: &str,
    leading_comments: Vec<Comment>,
) -> IngestResult<ModelBodyItem> {
    // Span of the whole statement — the typed variants (Association /
    // Validation / Callback) drop the source Expr during recognition,
    // so the declaration's location rides the ModelBodyItem wrapper.
    let span = Span {
        file: super::sources::file_id(file),
        start: stmt.location().start_offset() as u32,
        end: stmt.location().end_offset() as u32,
    };
    if let Some(call) = stmt.as_call_node() {
        if call.receiver().is_some() {
            return Ok(ModelBodyItem::Unknown {
                expr: ingest_expr(stmt, file)?,
                leading_comments,
                leading_blank_line: false,
            });
        }
        let method = constant_id_str(&call.name()).to_string();
        if let Some(assoc) = parse_association(&call, owner, &method, file) {
            return Ok(ModelBodyItem::Association { assoc, leading_blank_line: false, leading_comments, span });
        }
        // `validate :m, on:/if:/unless:`, `validate do ... end` and
        // `validates_with K, opts` (`ingest::validates`).
        if let Some(validation) = super::validates::parse_validate_call(&call, file)? {
            return Ok(ModelBodyItem::Validation { validation, leading_comments, leading_blank_line: false, span });
        }
        if let Some(parsed) = super::validates::parse_validation_call(&call, file)? {
            // The caller expects ONE item; `ingest_model_body_items`
            // answers every one of them and is the model walk's entry.
            if let Some(first) = parsed.into_iter().next() {
                return Ok(ModelBodyItem::Validation {
                    validation: first,
                    leading_comments,
                    leading_blank_line: false,
                    span,
                });
            }
        }
        if method == "scope" {
            if let Some(scope) = parse_scope(&call, file)? {
                return Ok(ModelBodyItem::Scope { scope, leading_blank_line: false, leading_comments });
            }
        }
        if let Some(callback) = parse_callback(&call, &method, file) {
            return Ok(ModelBodyItem::Callback { callback, leading_blank_line: false, leading_comments, span });
        }
        // The same classifier sees model declarations and a concern's
        // `included do`. Record before the latter drops the Unknown,
        // and before ingesting a block that may contain further gaps.
        // Name the DSL shape, not unverified gem ownership.
        // A model's own `state_machine` is carried into the lowered class
        // body and runs against the loaded state_machines gem
        // (`lower::state_machines`); a concern's is not spliced.
        if (method == "aasm" || (method == "state_machine" && !IN_MODEL_CLASS_BODY.get()))
            && call.block().and_then(|b| b.as_block_node()).is_some()
        {
            super::survey::record(&IngestError::Unsupported {
                file: file.into(),
                message: format!("state-machine DSL block `{method}` is not modeled"),
            });
        }
        return Ok(ModelBodyItem::Unknown {
            expr: ingest_expr(stmt, file)?,
            leading_comments,
            leading_blank_line: false,
        });
    }
    if let Some(def) = stmt.as_def_node() {
        return Ok(ModelBodyItem::Method {
            method: ingest_method(&def, file)?,
            leading_comments,
            leading_blank_line: false,
        });
    }
    Ok(ModelBodyItem::Unknown {
        expr: ingest_expr(stmt, file)?,
        leading_comments,
        leading_blank_line: false,
    })
}

/// A model body's `concerning` block (`util::ConcerningCall`) as body
/// items: its constants, which bind in the model (the block's cref), a
/// nested block's included; then, when `mixes_in` (the model's own
/// block), the `include` / `prepend` of `Model::Topic`. The module's
/// methods and hooks are the library-class pass's (`concerning_modules`),
/// and its class-side carriers the concern splice's. An `included` or
/// `prepended` block holding DSL is refused by name: the model concern
/// splice reads authored concern modules, so the DSL would be lost.
fn concerning_model_items(
    concerning: &super::util::ConcerningCall<'_>,
    owner: &ClassId,
    file: &str,
    mixes_in: bool,
    out: &mut Vec<ModelBodyItem>,
) -> IngestResult<()> {
    let topic = ClassId(Symbol::from(format!("{}::{}", owner.0.as_str(), concerning.topic)));
    let stmts = concerning.block.body().map(flatten_statements).unwrap_or_default();
    for stmt in &stmts {
        if stmt.as_constant_write_node().is_some() || stmt.as_constant_path_write_node().is_some() {
            out.push(ModelBodyItem::Unknown {
                expr: ingest_expr(stmt, file)?,
                leading_comments: Vec::new(),
                leading_blank_line: false,
            });
            continue;
        }
        if let Some(inner) = super::util::concerning_call(stmt) {
            concerning_model_items(&inner, &topic, file, false, out)?;
            continue;
        }
        let Some(call) = stmt.as_call_node() else { continue };
        let hook = constant_id_str(&call.name());
        if call.receiver().is_none() && matches!(hook, "included" | "prepended") {
            let dsl = call
                .block()
                .and_then(|b| b.as_block_node())
                .and_then(|b| b.body())
                .map(flatten_statements)
                .unwrap_or_default()
                .iter()
                .any(|inner| visibility::definition(inner).is_none());
            if dsl {
                return Err(super::error_site::at(
                    IngestError::Unsupported {
                        file: file.into(),
                        message: format!("an `{hook}` block with class-body statements in a model's `concerning` block is not modeled"),
                    },
                    stmt,
                ));
            }
        }
    }
    if mixes_in && concerning.mixes_in {
        let span = super::util::node_span(&concerning.call.as_node(), file);
        let path = topic.0.as_str().split("::").map(Symbol::from).collect();
        out.push(ModelBodyItem::Unknown {
            expr: Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from(if concerning.prepend { "prepend" } else { "include" }),
                    args: vec![Expr::new(span, ExprNode::Const { path })],
                    block: None,
                    parenthesized: false,
                },
            ),
            leading_comments: Vec::new(),
            leading_blank_line: false,
        });
    }
    Ok(())
}

/// Expand `enum :status, %i[active deactivated banned]` into the DSL it
/// stands for: one scope, one predicate and one bang writer per label.
///
/// Returns `None` when the statement isn't an `enum` call, so callers
/// can fall through to the normal classifier.
///
/// Desugaring into `Scope` + `Method` items — rather than adding a
/// `ModelBodyItem::Enum` and teaching thirteen emitters to expand it —
/// buys the whole existing pipeline for free: scopes already lower to
/// relation-returning class methods (with `__scope_` delegates), and
/// predicate bodies are ordinary column comparisons.
///
/// **Stored values, not labels.** The generated bodies compare and
/// query against what the column holds (`status == 0`), which is what
/// makes them correct without an enum type at runtime. The divergence
/// from Rails is the attribute reader: `user.status` yields `0` here
/// and `"active"` there. Rails' own `enum` maps at every boundary; that
/// mapping (for hand-written `where(role: :bot)` and `update!(status:
/// :deactivated)` sites) is a separate, type-aware pass.
pub(super) struct EnumExpansion {
    pub column: Symbol,
    /// Label → stored value, in declaration order. `None` when some
    /// stored value is only known at runtime (`Status::ARCHIVED` from
    /// another file): the scopes and predicates still carry it as an
    /// expression, but the label → literal table other passes rewrite
    /// hand-written sites with (`where(status: :archived)`) must be
    /// complete or absent, so a partial one is never recorded.
    pub mapping: Option<Vec<(String, Literal)>>,
    pub default: Option<Literal>,
    pub items: Vec<ModelBodyItem>,
}

/// Table payload of [`EnumExpansion`] without the generated items.
/// Captured from a concern `included do` so the splice can fold the
/// mapping into each includer's `enums` / `enum_defaults`.
#[derive(Clone, Debug)]
pub struct ConcernEnumDecl {
    pub column: Symbol,
    pub mapping: Vec<(String, Literal)>,
    pub default: Option<Literal>,
}

/// One class-body DSL expansion — `delegated_type` then `enum`. Both
/// ingest walks call [`expand_class_body_dsl`] instead of growing a
/// third parallel `match`.
pub(super) enum ClassBodyExpansion {
    DelegatedType(Vec<ModelBodyItem>),
    Enum(EnumExpansion),
}

pub(super) fn expand_class_body_dsl(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    leading_comments: &[Comment],
    class_consts: &ClassConsts,
    resolve_constant: &impl Fn(&Node<'_>) -> Option<Vec<(String, Literal)>>,
) -> IngestResult<Option<ClassBodyExpansion>> {
    match super::delegated_type::expand_delegated_type_decl(
        call,
        file,
        leading_comments,
        resolve_constant,
    )? {
        Some(items) => return Ok(Some(ClassBodyExpansion::DelegatedType(items))),
        None => {}
    }
    match expand_enum_decl(call, file, leading_comments, class_consts, resolve_constant)? {
        Some(exp) => Ok(Some(ClassBodyExpansion::Enum(exp))),
        None => Ok(None),
    }
}

/// The same syntax contract serves expansion and post-ingest validation.
/// Validation reads declarations only, never reconstructs model IR.
struct EnumDeclaration<'pr> {
    column: String,
    mapping: Node<'pr>,
    prefix: String,
    suffix: String,
    /// The label `default:` names, which seeds a new record over the
    /// column default. Computed here because only this parse sees the
    /// options hash.
    default_label: Option<String>,
    /// Rails `scopes:` / `instance_methods:` (default true). Named
    /// fields so a new option cannot vanish the way `default:` did.
    scopes: bool,
    instance_methods: bool,
}

fn enum_declaration<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<EnumDeclaration<'pr>> {
    if call.receiver().is_some() || constant_id_str(&call.name()) != "enum" {
        return None;
    }
    let args = call.arguments()?;
    let all_args = args.arguments();
    let mut iter = all_args.iter();
    let first = iter.next()?;

    // Two spellings: `enum :status, <mapping>, **opts` (Rails 7) and the
    // older `enum status: <mapping>, **opts`, where the column and its
    // mapping are the first pair of one keyword hash.
    let (column, mapping_node, prefix, suffix, default_label, scopes, instance_methods) =
        match symbol_value(&first) {
            Some(col) => {
                let column: String = col;
                let mapping = iter.next();
                let opts = iter.next();
                match opts.as_ref().and_then(|o| o.as_keyword_hash_node()) {
                    Some(kh) => {
                        let (p, s) = enum_affixes(&kh.elements(), &column);
                        (
                            column,
                            mapping,
                            p,
                            s,
                            enum_default_label(&kh.elements()),
                            enum_bool_option(&kh.elements(), "scopes", true),
                            enum_bool_option(&kh.elements(), "instance_methods", true),
                        )
                    }
                    None => (column, mapping, String::new(), String::new(), None, true, true),
                }
            }
            None => {
                let kh = first.as_keyword_hash_node()?;
                let elements = kh.elements();
                let pair = elements.iter().next()?.as_assoc_node()?;
                let column = symbol_value(&pair.key())?;
                let (prefix, suffix) = enum_affixes(&elements, &column);
                (
                    column,
                    Some(pair.value()),
                    prefix,
                    suffix,
                    enum_default_label(&elements),
                    enum_bool_option(&elements, "scopes", true),
                    enum_bool_option(&elements, "instance_methods", true),
                )
            }
        };
    Some(EnumDeclaration {
        column,
        mapping: mapping_node?,
        prefix,
        suffix,
        default_label,
        scopes,
        instance_methods,
    })
}

fn enum_mapping_error(file: &str, column: &str) -> IngestError {
    IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "enum :{} mapping must be an array or hash literal (or `%w[…].index_by(&:itself)`)",
            column
        ),
    }
}

fn validate_sorbet_enum_mappings(
    model: &Model,
    source: &crate::span::SourceFile,
    constants: &EnumConstants,
) -> IngestResult<()> {
    // This source was already parsed by ingest. Do not duplicate parse or
    // unrelated model diagnostics, or rebuild the model to check one mapping.
    let result = ruby_prism::parse(source.text.as_bytes());
    let root = result.node();
    let Some((_, class)) = super::util::find_all_classes_with_scope(&root).into_iter()
        .find(|(_, class)| class.location().start_offset() == model.span.start as usize)
        else { return Ok(()) };
    let Some(body) = class.body() else { return Ok(()) };
    let owners = constants.nesting.get(&(source.path.clone(), model.span.start as usize))
        .cloned().unwrap_or_default();
    for statement in flatten_statements(body) {
        let Some(declaration) = statement.as_call_node().and_then(|call| enum_declaration(&call))
            else { continue };
        if let Some(receiver) = serialized_enum_receiver(&declaration.mapping) {
            if constants.resolve(&receiver, &owners).is_none() {
                super::survey::unwrap_or_record::<()>(Err(enum_mapping_error(
                    &source.path, &declaration.column,
                )))?;
            }
        }
    }
    Ok(())
}

pub(super) fn expand_enum_decl(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    leading_comments: &[crate::dialect::Comment],
    class_consts: &ClassConsts,
    resolve_constant: &impl Fn(&Node<'_>) -> Option<Vec<(String, Literal)>>,
) -> IngestResult<Option<EnumExpansion>> {
    use crate::dialect::{MethodDef, MethodReceiver, Scope};
    use crate::effect::EffectSet;

    let Some(EnumDeclaration {
        column,
        mapping: mapping_node,
        prefix,
        suffix,
        default_label,
        scopes,
        instance_methods,
    }) = enum_declaration(call)
    else {
        return Ok(None)
    };
    // `enum :status, STATUSES` — the mapping named by a constant the class
    // body assigned above (`STATUSES = %i[…].freeze`).
    let labels = if let Some(receiver) = serialized_enum_receiver(&mapping_node) {
        resolve_constant(&receiver).map(|mapping| mapping.into_iter().map(|(label, value)| (label, EnumStored::Lit(value))).collect())
    } else { match mapping_node.as_constant_read_node() {
        Some(cr) => class_consts.mappings.get(constant_id_str(&cr.name())).cloned(),
        None => enum_label_values(&mapping_node, file, class_consts, resolve_constant)?,
    }}
    .ok_or_else(|| IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "enum :{} mapping must be an array or hash literal (or `%w[…].index_by(&:itself)`)",
            column
        ),
    })?;
    let labels = dedup_mapping_labels(labels);
    let all_labels = labels.clone();
    let default = default_label.and_then(|d| labels.iter().find(|(l, _)| *l == d).and_then(|(_, v)| {
        match v { EnumStored::Lit(value) => Some(value.clone()), EnumStored::Expr(_) => None }
    }));
    let literal_mapping = all_labels.iter().map(|(label, stored)| match stored {
        EnumStored::Lit(value) => Some((label.clone(), value.clone())),
        EnumStored::Expr(_) => None,
    }).collect::<Option<Vec<_>>>();
    let reads_label = literal_mapping.as_ref().is_some_and(|m| crate::dialect::enum_mapping_reads_label(m));
    // A label that is not a Ruby identifier (`32bits`, `64bits`) has no
    // predicate, scope or bang writer Ruby could name: Rails reaches them
    // through `send`, which the emit has no equivalent of. Skipped.
    let labels: Vec<(String, EnumStored)> = labels
        .into_iter()
        .filter(|(l, _)| {
            l.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .collect();

    let span = Span::synthetic();
    let sym = |s: &str| Expr::new(span, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(s) } });
    let column_read = || {
        Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: Symbol::from(column.as_str()),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        )
    };
    let mut items = Vec::new();
    for (label, stored) in labels.iter().cloned() {
        let base = format!("{prefix}{label}{suffix}");
        let stored_expr = || match &stored {
            EnumStored::Lit(value) => Expr::new(span, ExprNode::Lit { value: value.clone() }),
            EnumStored::Expr(e) => e.clone(),
        };
        let pair = Expr::new(
            span,
            ExprNode::Hash {
                entries: vec![(sym(&column), stored_expr())],
                kwargs: true,
            },
        );
        let call_with_pair = |method: &str| {
            Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from(method),
                    args: vec![pair.clone()],
                    block: None,
                    parenthesized: true,
                },
            )
        };
        let method_def = |name: String, body: Expr, comments: Vec<crate::dialect::Comment>| {
            ModelBodyItem::Method {
                method: MethodDef {
                    name_span: crate::span::Span::synthetic(),
                    name: Symbol::from(name),
                    receiver: MethodReceiver::Instance,
                    visibility: crate::dialect::MethodVisibility::Public,
                    params: Vec::new(),
                    unsupported_formals: None,
                    has_anonymous_block: false,
                    block_param: None,
                    body,
                    signature: None,
                    effects: EffectSet::pure(),
                    enclosing_class: None,
                    kind: crate::dialect::AccessorKind::Method,
                    is_async: false,
                    mutates_self: false,
                },
                leading_comments: comments,
                leading_blank_line: false,
            }
        };

        if scopes {
            items.push(ModelBodyItem::Scope {
                scope: Scope {
                    name: Symbol::from(base.as_str()),
                    params: Vec::new(),
                    body: call_with_pair("where"),
                },
                // The declaration's own comments ride the first item it
                // expands to, so a documented `enum` keeps its docs.
                leading_comments: if items.is_empty() {
                    leading_comments.to_vec()
                } else {
                    Vec::new()
                },
                leading_blank_line: false,
            });
            // `not_published`: Rails generates the negative scope beside each positive one.
            let where_not = Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(Expr::new(
                        span,
                        ExprNode::Send { recv: None, method: Symbol::from("where"), args: vec![], block: None, parenthesized: false },
                    )),
                    method: Symbol::from("not"),
                    args: vec![pair.clone()],
                    block: None,
                    parenthesized: true,
                },
            );
            items.push(ModelBodyItem::Scope {
                scope: Scope { name: Symbol::from(format!("not_{base}")), params: Vec::new(), body: where_not },
                leading_comments: Vec::new(),
                leading_blank_line: false,
            });
        }
        if instance_methods {
            // Aliases share their stored value and therefore their
            // canonical reader label. Every alias predicate is true
            // for that value, even though the reader names only the first.
            let predicate_label = literal_mapping.as_ref().and_then(|mapping| {
                let EnumStored::Lit(value) = &stored else { return None };
                mapping.iter().find(|(_, v)| v == value).map(|(label, _)| label.clone())
            }).unwrap_or_else(|| label.clone());
            items.push(method_def(
                format!("{base}?"),
                Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(column_read()),
                        method: Symbol::from("=="),
                        args: vec![if reads_label {
                            Expr::new(span, ExprNode::Lit { value: Literal::Str { value: predicate_label } })
                        } else { stored_expr() }],
                        block: None,
                        parenthesized: false,
                    },
                ),
                if items.is_empty() {
                    leading_comments.to_vec()
                } else {
                    Vec::new()
                },
            ));
            items.push(method_def(
                format!("{base}!"),
                call_with_pair("update!"),
                Vec::new(),
            ));
        }
    }
    // `Model.statuses`: every label, identifier or not, keyed by String as Rails' mapping is.
    let mapping_hash = Expr::new(
        span,
        ExprNode::Hash {
            entries: all_labels
                .iter()
                .map(|(label, value)| {
                    (
                        Expr::new(span, ExprNode::Lit { value: Literal::Str { value: label.clone() } }),
                        match value { EnumStored::Lit(value) => Expr::new(span, ExprNode::Lit { value: value.clone() }), EnumStored::Expr(expr) => expr.clone() },
                    )
                })
                .collect(),
            kwargs: false,
        },
    );
    let mapping_comments = if items.is_empty() {
        leading_comments.to_vec()
    } else {
        Vec::new()
    };
    items.push(ModelBodyItem::Method {
        method: MethodDef {
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from(crate::naming::pluralize_snake(&column)),
            receiver: MethodReceiver::Class,
            visibility: crate::dialect::MethodVisibility::Public,
            params: Vec::new(),
            unsupported_formals: None,
            has_anonymous_block: false,
            block_param: None,
            body: mapping_hash,
            signature: None,
            effects: EffectSet::pure(),
            enclosing_class: None,
            kind: crate::dialect::AccessorKind::Method,
            is_async: false,
            mutates_self: false,
        },
        leading_comments: mapping_comments,
        leading_blank_line: false,
    });
    let mapping = all_labels
        .iter()
        .map(|(l, v)| match v {
            EnumStored::Lit(lit) => Some((l.clone(), lit.clone())),
            EnumStored::Expr(_) => None,
        })
        .collect::<Option<Vec<_>>>();
    let writes = mapping_constant_writes(&mapping_node, file)?;
    // The declaration's comments ride the first item, so with constant
    // writes ahead of the generated items they move to the first write.
    let mut doc_comments = if writes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(items[0].leading_comments_mut())
    };
    items.splice(
        0..0,
        writes.into_iter().map(|expr| ModelBodyItem::Unknown {
            expr,
            leading_comments: std::mem::take(&mut doc_comments),
            leading_blank_line: false,
        }),
    );
    Ok(Some(EnumExpansion { column: Symbol::from(column.as_str()), mapping, default, items }))
}

/// The stored value of one enum label. A literal is what Rails' own
/// table holds and what other passes rewrite hand-written sites with; an
/// expression (`Status::ARCHIVED`, `Eligibility::Paused.serialize`)
/// is a value only the running program knows. The generated scope and
/// predicate use it either way — they compare against the column, not
/// against a constant folded at ingest.
#[derive(Clone)]
enum EnumStored {
    Lit(Literal),
    Expr(Expr),
}

/// Constants a class body assigns that an `enum` declaration below them
/// may name: the mapping itself (`enum :status, STATUSES`), a label
/// (`NAME => NAME`, `"#{PENDING}": 1`) or a stored value
/// (`active: ACTIVE`). Collected in source order so an alias
/// (`ROLE_B = ROLE_A`) resolves through the constant it names.
#[derive(Default)]
pub(super) struct ClassConsts {
    /// `NAME = "x"`, `NAME = :x`, `NAME = 3`, `NAME = OTHER`.
    scalars: crate::det::HashMap<String, Literal>,
    /// `NAME = %i[…]` / `{ … }`, as the mapping an enum would build.
    mappings: crate::det::HashMap<String, Vec<(String, EnumStored)>>,
    /// `NAME = %i[…]` / `%w[…]` — the bare labels, for the idioms that
    /// derive a mapping from them (`NAME.index_by(&:to_s)`).
    labels: crate::det::HashMap<String, Vec<String>>,
}

impl ClassConsts {
    pub(super) fn collect(stmts: &[Node<'_>], file: &str) -> Self {
        let mut out = ClassConsts::default();
        for stmt in stmts {
            let Some(cw) = stmt.as_constant_write_node() else { continue };
            let name = constant_id_str(&cw.name()).to_string();
            let value = cw.value();
            if let Some(lit) = out.scalar(&value) {
                out.scalars.insert(name, lit);
                continue;
            }
            if let Some(labels) = label_list(&value) {
                out.labels.insert(name.clone(), labels);
            }
            // A failure to ingest a value is not this pass's to report:
            // the constant is simply not usable as a mapping, and the
            // enum that names it says so.
            if let Ok(Some(mapping)) = enum_label_values(&value, file, &out, &|_| None) {
                out.mappings.insert(name, mapping);
            }
        }
        out
    }

    /// A scalar the class body can fold: a string, a symbol (stored as
    /// its string, which is what a string column holds), an integer,
    /// or a constant already folded. `.freeze` is transparent.
    fn scalar(&self, node: &Node<'_>) -> Option<Literal> {
        if let Some(call) = node.as_call_node() {
            if constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() {
                return self.scalar(&call.receiver()?);
            }
            return None;
        }
        if let Some(s) = string_value(node).or_else(|| symbol_value(node)) {
            return Some(Literal::Str { value: s });
        }
        if let Some(i) = node.as_integer_node() {
            return Some(Literal::Int { value: super::util::integer_i64(&i.value())? });
        }
        if let Some(cr) = node.as_constant_read_node() {
            return self.scalars.get(constant_id_str(&cr.name())).cloned();
        }
        None
    }

    /// A label spelled as a symbol, a string, a folded constant, or an
    /// interpolation of folded constants (`"#{UNSET_STATUS}": 0`).
    fn label(
        &self,
        node: &Node<'_>,
        resolve_constant: &impl Fn(&Node<'_>) -> Option<Vec<(String, Literal)>>,
    ) -> Option<String> {
        if let Some(Literal::Str { value }) = self.scalar(node) {
            return Some(value);
        }
        let parts: Vec<Node<'_>> = if let Some(d) = node.as_interpolated_symbol_node() {
            d.parts().iter().collect()
        } else if let Some(d) = node.as_interpolated_string_node() {
            d.parts().iter().collect()
        } else {
            return None;
        };
        let mut out = String::new();
        for part in parts {
            if let Some(s) = string_value(&part) {
                out.push_str(&s);
            } else if let Some(e) = part.as_embedded_statements_node() {
                let body: Vec<Node<'_>> = e.statements()?.body().iter().collect();
                let [only] = body.as_slice() else { return None };
                // `Status::Approved.serialize`: a nested T::Enum member's
                // serialized value, which resolves as a one-pair mapping.
                if only.as_call_node().is_some() {
                    let pairs = resolve_constant(only)?;
                    let [(serialized, _)] = pairs.as_slice() else { return None };
                    out.push_str(serialized);
                    continue;
                }
                match self.scalar(only)? {
                    Literal::Str { value } => out.push_str(&value),
                    Literal::Int { value } => out.push_str(&value.to_string()),
                    _ => return None,
                }
            } else {
                return None;
            }
        }
        Some(out)
    }
}

/// The bare labels of a `%i[…]` / `%w[…]` array (optionally `.freeze`d).
fn label_list(node: &Node<'_>) -> Option<Vec<String>> {
    if let Some(call) = node.as_call_node() {
        if constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() {
            return label_list(&call.receiver()?);
        }
    }
    node.as_array_node()?
        .elements()
        .iter()
        .map(|el| symbol_value(&el).or_else(|| string_value(&el)))
        .collect()
}

/// Label → stored value for an `enum` mapping. An array literal maps by
/// index the way Rails does (`%i[active deactivated]` → 0, 1); a hash
/// literal carries its own values; `%w[…].index_by(&:itself)` — the
/// idiom for a string-backed column — maps each label to itself.
///
/// A hash label may be a symbol, a string, or a class-body constant
/// folded through `consts`; a stored value that is not a foldable scalar
/// is kept as an expression. `Ok(None)` for anything whose LABELS are not
/// known at ingest (a computed hash, a constant from another file),
/// which the caller reports as a gap rather than guessing at names.
fn enum_label_values(
    node: &Node<'_>,
    file: &str,
    consts: &ClassConsts,
    resolve_constant: &impl Fn(&Node<'_>) -> Option<Vec<(String, Literal)>>,
) -> IngestResult<Option<Vec<(String, EnumStored)>>> {
    if node.as_constant_path_node().is_some() {
        return Ok(resolve_constant(node).map(|pairs| pairs.into_iter().map(|(label, value)| (label, EnumStored::Lit(value))).collect()));
    }

    // `%i[…].freeze` / `{ … }.freeze` — the literal is the receiver.
    if let Some(call) = node.as_call_node() {
        if constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() && call.block().is_none() {
            if let Some(recv) = call.receiver() {
                return enum_label_values(&recv, file, consts, resolve_constant);
            }
        }
    }
    if let Some(arr) = node.as_array_node() {
        let Some(labels) = label_list(node) else { return Ok(None) };
        let _ = arr;
        return Ok(Some(
            labels
                .into_iter()
                .enumerate()
                .map(|(i, label)| (label, EnumStored::Lit(Literal::Int { value: i as i64 })))
                .collect(),
        ));
    }
    if let Some(hash) = node.as_hash_node() {
        return enum_label_pairs(hash.elements().iter(), file, consts, resolve_constant);
    }
    // `enum :status, processing: 'processing', ready: 'ready'` — Rails 7's
    // dominant spelling. A trailing bare-hash argument (no braces) parses
    // as a `KeywordHashNode`, not a `HashNode`; its elements are the same
    // `AssocNode`s either way, so the extraction logic is shared.
    if let Some(kwhash) = node.as_keyword_hash_node() {
        return enum_label_pairs(kwhash.elements().iter(), file, consts, resolve_constant);
    }
    let Some(call) = node.as_call_node() else { return Ok(None) };
    let Some(recv) = call.receiver() else { return Ok(None) };
    // The labels a derived mapping starts from: a literal array, or a
    // class-body constant naming one.
    let labels = || match recv.as_constant_read_node() {
        Some(cr) => consts.labels.get(constant_id_str(&cr.name())).cloned(),
        None => label_list(&recv).or_else(|| resolve_constant(&recv).map(|pairs| pairs.into_iter().map(|(label, _)| label).collect())),
    };
    let identity = |labels: Vec<String>| {
        labels
            .into_iter()
            .map(|l| (l.clone(), EnumStored::Lit(Literal::Str { value: l })))
            .collect::<Vec<_>>()
    };
    match constant_id_str(&call.name()) {
        // `%w[ invisible nothing ].index_by(&:itself)`,
        // `STATUSES.index_with(&:itself)` — the labels ARE the stored
        // strings. (`&:to_s`/`&:to_sym` on symbols or strings name the
        // same label.)
        "index_by" | "index_with" if is_identity_proc(&call) => {
            Ok(labels().map(identity))
        }
        // `STATUSES.to_h { |status| [status, status.to_s] }`
        "to_h" if is_identity_pair_block(&call) => Ok(labels().map(identity)),
        // The same mapping written as `STATUSES.map { |s| [s, s.to_s] }.to_h`.
        "to_h" if call.arguments().is_none() && call.block().is_none() => {
            let Some(map) = recv.as_call_node() else { return Ok(None) };
            if constant_id_str(&map.name()) != "map" || !is_identity_pair_block(&map) {
                return Ok(None);
            }
            let Some(source) = map.receiver() else { return Ok(None) };
            let labels = match source.as_constant_read_node() {
                Some(cr) => consts.labels.get(constant_id_str(&cr.name())).cloned(),
                None => label_list(&source).or_else(|| resolve_constant(&source).map(|pairs| pairs.into_iter().map(|(label, _)| label).collect())),
            };
            Ok(labels.map(identity))
        }
        _ => Ok(None),
    }
}

/// `&:itself` / `&:to_s` / `&:to_sym` as the only argument: the block maps
/// a label to itself. (`["draft", "active"].index_by(&:to_sym)` keys by
/// symbol and stores the string; the label text and the stored value are
/// the same either way.)
fn is_identity_proc(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(block) = call.block().and_then(|b| b.as_block_argument_node()) else {
        return false;
    };
    block
        .expression()
        .and_then(|e| symbol_value(&e))
        .is_some_and(|s| s == "itself" || s == "to_s" || s == "to_sym")
}

/// `{ |x| [x, x.to_s] }` / `{ |x| [x, x] }`: a `to_h` block pairing each
/// label with its own string.
fn is_identity_pair_block(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(block) = call.block().and_then(|b| b.as_block_node()) else { return false };
    let Some(params) = block.parameters().and_then(|p| p.as_block_parameters_node()) else {
        return false;
    };
    let Some(pn) = params.parameters() else { return false };
    let reqs: Vec<_> = pn.requireds().iter().collect();
    let [only] = reqs.as_slice() else { return false };
    let Some(rp) = only.as_required_parameter_node() else { return false };
    let param = constant_id_str(&rp.name()).to_string();
    let Some(body) = block.body() else { return false };
    let stmts: Vec<Node<'_>> = flatten_statements(body);
    let [stmt] = stmts.as_slice() else { return false };
    let Some(arr) = stmt.as_array_node() else { return false };
    let els: Vec<Node<'_>> = arr.elements().iter().collect();
    let [k, v] = els.as_slice() else { return false };
    let is_param = |n: &Node<'_>| {
        n.as_local_variable_read_node().is_some_and(|l| constant_id_str(&l.name()) == param)
    };
    is_param(k)
        && (is_param(v)
            || v.as_call_node().is_some_and(|c| {
                constant_id_str(&c.name()) == "to_s"
                    && c.arguments().is_none()
                    && c.receiver().is_some_and(|r| is_param(&r))
            }))
}

/// The one admitted consumer of enum-valued reads. Shared with the source
/// guard so a mutating block cannot hide behind a method name like `to_h`.
fn serialized_enum_receiver<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    let call = node.as_call_node()?;
    if constant_id_str(&call.name()) == "freeze"
        && call.arguments().is_none() && call.block().is_none() {
        return serialized_enum_receiver(&call.receiver()?);
    }
    if constant_id_str(&call.name()) != "to_h" || call.arguments().is_some() {
        return None;
    }
    let block = call.block()?.as_block_node()?;
    let parameters = block.parameters()?.as_block_parameters_node()?;
    let params = parameters.parameters()?;
    if parameters.locals().iter().next().is_some() || params.optionals().iter().next().is_some()
        || params.rest().is_some() || params.posts().iter().next().is_some()
        || params.keywords().iter().next().is_some() || params.keyword_rest().is_some()
        || params.block().is_some() {
        return None;
    }
    let requireds: Vec<_> = params.requireds().iter().collect();
    let [parameter] = requireds.as_slice() else { return None };
    let name = parameter.as_required_parameter_node()?.name();
    let statements = flatten_statements(block.body()?);
    let [statement] = statements.as_slice() else { return None };
    let pair = statement.as_array_node()?;
    let elements: Vec<_> = pair.elements().iter().collect();
    let [key, value] = elements.as_slice() else { return None };
    // `values.map(&:serialize).to_h { |s| [s.to_sym, s] }`: the block
    // receives the serialized strings, so the parameter itself is the
    // label. Otherwise it receives members and reads `s.serialize`.
    let receiver = call.receiver()?;
    let (values, serialized_param) = match receiver.as_call_node() {
        Some(map) if constant_id_str(&map.name()) == "map" => {
            let argument = map.block()?.as_block_argument_node()?;
            let symbol = argument.expression()?.as_symbol_node()?;
            if map.arguments().is_some() || symbol_value(&symbol.as_node()).as_deref() != Some("serialize") {
                return None;
            }
            (map.receiver()?, true)
        }
        _ => (receiver, false),
    };
    let reads_serialized = |node: &Node<'pr>| -> bool {
        if serialized_param {
            return node.as_local_variable_read_node()
                .is_some_and(|read| constant_id_str(&read.name()) == constant_id_str(&name));
        }
        let Some(call) = node.as_call_node() else { return false };
        constant_id_str(&call.name()) == "serialize" && call.arguments().is_none()
            && call.block().is_none() && call.receiver().and_then(|r| r.as_local_variable_read_node())
                .is_some_and(|read| constant_id_str(&read.name()) == constant_id_str(&name))
    };
    // A symbol key is the same label: `[s.serialize.to_sym, s.serialize]`.
    let unsymbolized = key.as_call_node().filter(|call| {
        constant_id_str(&call.name()) == "to_sym" && call.arguments().is_none() && call.block().is_none()
    }).map(|call| call.receiver());
    let key_reads = match &unsymbolized {
        Some(inner) => inner.as_ref().is_some_and(|inner| reads_serialized(inner)),
        None => reads_serialized(key),
    };
    if !key_reads || !reads_serialized(value) {
        return None;
    }
    let values_call = values.as_call_node()?;
    (constant_id_str(&values_call.name()) == "values"
        && values_call.arguments().is_none() && values_call.block().is_none()).then_some(values)
}

/// A mapping whose labels fold to the same string (`ROLE_B =>
/// ROLE_B, ROLE_A => ROLE_A` with `ROLE_B = ROLE_A`) is one
/// entry in the Hash Ruby builds: the first position, the last value.
/// Kept as written, the generated `self.<plural>` literal would repeat a
/// key, which Ruby warns about at parse time.
fn dedup_mapping_labels(labels: Vec<(String, EnumStored)>) -> Vec<(String, EnumStored)> {
    let mut out: Vec<(String, EnumStored)> = Vec::with_capacity(labels.len());
    for (label, stored) in labels {
        match out.iter_mut().find(|(l, _)| *l == label) {
            Some(slot) => slot.1 = stored,
            None => out.push((label, stored)),
        }
    }
    out
}

/// Shared `label => value` extraction for both a braced `HashNode` and a
/// bare trailing `KeywordHashNode` — same element shape (`AssocNode`s),
/// different Prism wrapper type depending on whether the source wrote
/// `{ … }` or left the braces off.
fn enum_label_pairs<'a>(
    elements: impl Iterator<Item = Node<'a>>,
    file: &str,
    consts: &ClassConsts,
    resolve_constant: &impl Fn(&Node<'_>) -> Option<Vec<(String, Literal)>>,
) -> IngestResult<Option<Vec<(String, EnumStored)>>> {
    let mut out = Vec::new();
    for el in elements {
        let Some(assoc) = el.as_assoc_node() else { return Ok(None) };
        let Some(label) = consts.label(&assoc.key(), resolve_constant) else { return Ok(None) };
        let value = assoc.value();
        // `pending: PENDING = "pending"` stores the written value and
        // defines the constant, which `mapping_constant_writes` hoists
        // into the class body. A literal value is stored as that literal;
        // only a non-literal one makes the generated methods read the
        // constant.
        let stored = match value.as_constant_write_node() {
            Some(cw) => match consts.scalar(&cw.value()) {
                Some(lit) => EnumStored::Lit(lit),
                None => EnumStored::Expr(Expr::new(
                    Span::synthetic(),
                    ExprNode::Const { path: vec![Symbol::from(constant_id_str(&cw.name()))] },
                )),
            },
            None => match consts.scalar(&value) {
                Some(lit) => EnumStored::Lit(lit),
                // `unknown: nil` names a NULL column: a literal too.
                None if value.as_nil_node().is_some() => EnumStored::Lit(Literal::Nil),
                None => EnumStored::Expr(ingest_expr(&value, file)?),
            },
        };
        out.push((label, stored));
    }
    Ok(Some(out))
}

/// The constants an enum's hash mapping assigns as it is built
/// (`pending: PENDING = "pending"`), as class-body writes. Ruby runs
/// them when the class body evaluates the `enum` call's arguments; left
/// inside the mapping they would be copied into the generated methods,
/// where a constant assignment does not parse.
fn mapping_constant_writes(node: &Node<'_>, file: &str) -> IngestResult<Vec<Expr>> {
    if let Some(call) = node.as_call_node() {
        if constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() && call.block().is_none() {
            if let Some(recv) = call.receiver() {
                return mapping_constant_writes(&recv, file);
            }
        }
        return Ok(Vec::new());
    }
    let elements: Vec<Node<'_>> = if let Some(hash) = node.as_hash_node() {
        hash.elements().iter().collect()
    } else if let Some(kwhash) = node.as_keyword_hash_node() {
        kwhash.elements().iter().collect()
    } else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for el in elements {
        let Some(assoc) = el.as_assoc_node() else { continue };
        let value = assoc.value();
        if value.as_constant_write_node().is_some() {
            out.push(ingest_expr(&value, file)?);
        }
    }
    Ok(out)
}

/// Rails `scopes:` / `instance_methods:` — only the unprefixed
/// spellings; `_scopes` / `_instance_methods` are rejected by Rails 7+.
fn enum_bool_option(elements: &ruby_prism::NodeList<'_>, name: &str, default: bool) -> bool {
    for el in elements.iter() {
        let Some(assoc) = el.as_assoc_node() else { continue };
        let Some(key) = symbol_value(&assoc.key()) else { continue };
        // `_scopes:` / `_instance_methods:` are the pre-Rails-7 spellings.
        if key.trim_start_matches('_') != name {
            continue;
        }
        let value = assoc.value();
        if value.as_false_node().is_some() {
            return false;
        }
        if value.as_true_node().is_some() {
            return true;
        }
    }
    default
}

// `_default:` is the pre-Rails-7 spelling, as `_prefix:` is.
fn enum_default_label(elements: &ruby_prism::NodeList<'_>) -> Option<String> {
    elements.iter().find_map(|el| {
        let assoc = el.as_assoc_node()?;
        let key = symbol_value(&assoc.key())?;
        if key.trim_start_matches('_') != "default" {
            return None;
        }
        symbol_value(&assoc.value()).or_else(|| string_value(&assoc.value()))
    })
}

/// `prefix:`/`suffix:` from an `enum`'s option hash. `true` means "use
/// the column name" (Rails' own convention); a symbol or string names
/// the affix directly. Returns the strings to splice around each label,
/// already carrying their separating underscore.
fn enum_affixes(elements: &ruby_prism::NodeList<'_>, column: &str) -> (String, String) {
    let mut prefix = String::new();
    let mut suffix = String::new();
    for el in elements.iter() {
        let Some(assoc) = el.as_assoc_node() else { continue };
        let Some(key) = symbol_value(&assoc.key()) else { continue };
        // `_prefix`/`_suffix` are the pre-Rails-7 spellings.
        let which = key.trim_start_matches('_');
        if which != "prefix" && which != "suffix" {
            continue;
        }
        let value = assoc.value();
        let affix = if value.as_true_node().is_some() {
            column.to_string()
        } else if let Some(s) = symbol_value(&value).or_else(|| string_value(&value)) {
            s
        } else {
            continue;
        };
        if which == "prefix" {
            prefix = format!("{affix}_");
        } else {
            suffix = format!("_{affix}");
        }
    }
    (prefix, suffix)
}

/// Expand the singleton body while retaining the enclosing visibility decisions.
fn ingest_singleton_class_body(
    sc: &ruby_prism::SingletonClassNode<'_>,
    owner: &ClassId,
    file: &str,
    visibility: &Visibility,
) -> IngestResult<super::singleton_class::SingletonBody> {
    super::singleton_class::ingest_singleton_body(sc, owner, file, &|def| {
        let mut method = ingest_method(def, file)?;
        visibility.apply(&def.as_node(), &mut method);
        Ok(method)
    })
}

pub(super) fn ingest_method(
    def: &ruby_prism::DefNode<'_>,
    file: &str,
) -> IngestResult<crate::dialect::MethodDef> {
    use crate::dialect::{MethodDef, MethodReceiver};

    let formals = super::forwarding::parse(def);
    let name = Symbol::from(constant_id_str(&def.name()));
    // `def self.foo` / `def Post.foo` have explicit receivers; plain `def foo`
    // is an instance method.
    let receiver = if def.receiver().is_some() {
        MethodReceiver::Class
    } else {
        MethodReceiver::Instance
    };

    // Collect required positional params, then optional-with-default
    // params (`def avatar_path(size = 100)`) carrying their default expr so
    // the emitted method reproduces the arity — dropping the optional left
    // `def avatar_path` with a body still reading `size`, an ArgumentError
    // at every call site that passes one. Every parameter kind the
    // library-class path records is recorded here too, in Ruby's
    // declaration order, so the `def` keeps the source arity.
    let mut params: Vec<crate::dialect::Param> = Vec::new();
    if let Some(pn) = def.parameters() {
        for req in pn.requireds().iter() {
            if let Some(rp) = req.as_required_parameter_node() {
                params.push(crate::dialect::Param::positional(Symbol::from(
                    constant_id_str(&rp.name()),
                )));
            }
        }
        for opt in pn.optionals().iter() {
            if let Some(op) = opt.as_optional_parameter_node() {
                let default = ingest_expr(&op.value(), file)?;
                params.push(crate::dialect::Param::with_default(
                    Symbol::from(constant_id_str(&op.name())),
                    default,
                ));
            }
        }
        // `*rest`, and the required params Ruby allows after it
        // (`def pair(first, *rest)`, `def f(*rest, last)`). Dropping
        // the splat left `def tagged` with a body still reading
        // `labels` — an ArgumentError at every call site that passes
        // one, while the same method on a plain class kept it. An
        // anonymous `*` has no name to bind and is skipped; the
        // declaration stays refused (`unsupported_formals`).
        if let Some(rest) = pn.rest() {
            if let Some(loc) = rest.as_rest_parameter_node().and_then(|rp| rp.name()) {
                params.push(crate::dialect::Param::rest(Symbol::from(constant_id_str(&loc))));
            } else if let Some(name) = &formals.anonymous_rest_name {
                // An unforwarded `*`: see `forwarding`'s module doc.
                params.push(crate::dialect::Param::rest(name.clone()));
            }
        }
        for post in pn.posts().iter() {
            if let Some(pp) = post.as_required_parameter_node() {
                params.push(crate::dialect::Param::positional(Symbol::from(
                    constant_id_str(&pp.name()),
                )));
            }
        }
        // Keyword params (`def recent_threads(amount, for_user: nil)`),
        // required (`k:`) and optional (`k: default`) alike. Dropping
        // them left `def recent_threads(amount)` with a body reading
        // `for_user` — an ArgumentError at every kwarg call site.
        for kw in pn.keywords().iter() {
            if let Some(okw) = kw.as_optional_keyword_parameter_node() {
                let default = ingest_expr(&okw.value(), file)?;
                params.push(crate::dialect::Param::keyword(
                    Symbol::from(constant_id_str(&okw.name())),
                    Some(default),
                ));
            } else if let Some(rkw) = kw.as_required_keyword_parameter_node() {
                params.push(crate::dialect::Param::keyword(
                    Symbol::from(constant_id_str(&rkw.name())),
                    None,
                ));
            }
        }
        // `**params`: the same trailing positional-defaulting-to-`{}`
        // that `ingest::library_class` records, MARKED `from_kwrest`
        // for `lower::kwrest_forward`. Dropping it left campfire's
        // `Push::Subscription#notification(**params)` as `def
        // notification` with a body still reading `params` — every
        // caller an ArgumentError, and the forward into
        // `WebPush::Notification.new(**params, …)` a bare name.
        //
        // Beside a positional `*rest` the flattening does not parse
        // (`def both(*args, options = {})`), so there the slot stays a
        // real `**kwrest`, as the library-class path keeps it.
        //
        // Beside an earlier keyword (`def notification(badge: …,
        // **params)` — campfire preview) flattening to `params = {}`
        // also does not parse: optional positionals cannot follow
        // keywords. Keep a real keyword-rest there too (same rule
        // `library_class` applies when `keeps_keywords`).
        if let Some(krest) = pn.keyword_rest() {
            if let Some(krp) = krest.as_keyword_rest_parameter_node() {
                if let Some(loc) = krp.name() {
                    let name = Symbol::from(constant_id_str(&loc));
                    let has_keywords = params.iter().any(|p| p.keyword || p.from_keyword);
                    // Match `library_class`: `from_kwrest` marks the
                    // *flattened* `params = {}` form only. A real
                    // keyword-rest kept beside keywords / `*rest` must
                    // not carry the marker — forwarding analysis treats
                    // `from_kwrest` as "flattened keyword ABI" and would
                    // reject valid `**` / full-arg forwards into it.
                    let p = if params.iter().any(|p| p.rest) || has_keywords {
                        let mut p = crate::dialect::Param::keyword(name, None);
                        p.rest = true;
                        p
                    } else {
                        let mut p = crate::dialect::Param::with_default(
                            name,
                            Expr::new(
                                Span::synthetic(),
                                ExprNode::Hash { entries: vec![], kwargs: false },
                            ),
                        );
                        p.from_kwrest = true;
                        p
                    };
                    params.push(p);
                }
            }
        }
    }

    // `&blk` rides in `MethodDef.block_param`, not the flat list, as
    // the library-class path records it: it fills the call-site
    // `block:` slot, and the emitter closes the `def` with `&blk` so a
    // body that passes it on (`each(&blk)`) still binds the name.
    // Ruby 3.4's anonymous `&` gets the same synthesized name the
    // library-class path gives it, so bare-`&` forwarding binds.
    let block_param = def.parameters().and_then(|pn| pn.block()).map(|block| {
        let name = block
            .name()
            .and_then(|loc| std::str::from_utf8(loc.as_slice()).ok())
            .unwrap_or("__blk");
        crate::dialect::Param::positional(Symbol::from(name))
    });

    // Only full `...` or nameless `**` enters this canonical seam.
    // Named rest/keyword-rest above and the separate block slot stay
    // source-owned; no forwarding packet is expanded into local names.
    params.extend(formals.anonymous.map(super::forwarding::AnonymousFormal::into_param));

    let body = match def.body() {
        Some(b) => ingest_expr(&b, file)?,
        None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
    };

    Ok(MethodDef {
        name_span: super::util::def_name_span(def, file),
        name,
        receiver,
        visibility: crate::dialect::MethodVisibility::Public,
        params,
        unsupported_formals: formals.unsupported,
        has_anonymous_block: formals.has_anonymous_block,
        body,
        signature: None,
        effects: EffectSet::pure(),
        // Rails model methods carry their owner on the surrounding
        // Model struct (model.name); no need to duplicate here.
        enclosing_class: None,
        // Source-defined `def` in a Rails model — Method by default.
        kind: crate::dialect::AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param,
    })
}

fn parse_callback(
    call: &ruby_prism::CallNode<'_>,
    method: &str,
    file: &str,
) -> Option<crate::dialect::Callback> {
    use crate::dialect::{Callback, CallbackHook, CallbackOn};

    if let Some(callback) = transaction_callback(call, method, file) {
        return callback;
    }
    let hook = match method {
        "before_validation" => CallbackHook::BeforeValidation,
        "after_validation" => CallbackHook::AfterValidation,
        "before_save" => CallbackHook::BeforeSave,
        "after_save" => CallbackHook::AfterSave,
        "before_create" => CallbackHook::BeforeCreate,
        "after_create" => CallbackHook::AfterCreate,
        "before_update" => CallbackHook::BeforeUpdate,
        "after_update" => CallbackHook::AfterUpdate,
        "before_destroy" => CallbackHook::BeforeDestroy,
        "after_destroy" => CallbackHook::AfterDestroy,
        _ => return None,
    };
    let args = call.arguments()?;
    let mut targets: Vec<Symbol> = Vec::new();
    let mut object: Option<Expr> = None;
    let mut prepend = false;
    let mut on: Option<CallbackOn> = None;
    let mut on_contexts: Vec<Symbol> = Vec::new();
    let validation_hook = matches!(hook, CallbackHook::BeforeValidation | CallbackHook::AfterValidation);
    let mut if_cond: Option<Expr> = None;
    let mut unless_cond: Option<Expr> = None;
    for arg in args.arguments().iter() {
        if let Some(sym) = symbol_value(&arg) {
            targets.push(Symbol::from(sym.as_str()));
        } else if let Some(kh) = arg.as_keyword_hash_node() {
            for el in kh.elements().iter() {
                let assoc = el.as_assoc_node()?;
                let key = symbol_value(&assoc.key())?;
                match key.as_str() {
                    "on" => {
                        let value = assoc.value();
                        let single = symbol_value(&value).and_then(|s| match s.as_str() {
                            "create" => Some(CallbackOn::Create),
                            "update" => Some(CallbackOn::Update),
                            "destroy" => Some(CallbackOn::Destroy),
                            _ => None,
                        });
                        if let Some(single) = single {
                            on = Some(single);
                        } else if validation_hook {
                            // `on: [:create, :fulfillment]` or a custom
                            // context: a validation hook runs when the
                            // record's validation context is any of them.
                            let values: Vec<ruby_prism::Node<'_>> = match value.as_array_node() {
                                Some(array) => array.elements().iter().collect(),
                                None => vec![value],
                            };
                            on_contexts.clear();
                            for v in values {
                                on_contexts.push(Symbol::from(symbol_value(&v)?.as_str()));
                            }
                        } else {
                            // Array form and unknown values on a hook
                            // without validation contexts: not modeled.
                            return None;
                        }
                    }
                    // `if:` / `unless:` — a zero-arity lambda/proc body
                    // (Rails `instance_exec`s it with `self` the record)
                    // or a Symbol naming a predicate method. Lowered to a
                    // guard around the callback body, so the callback runs
                    // in exactly Rails' circumstances. Dropping the
                    // declaration instead would run it in no circumstance
                    // at all, which is the wrong answer too.
                    "if" => if_cond = Some(callback_condition(&assoc.value(), file)?),
                    "unless" => unless_cond = Some(callback_condition(&assoc.value(), file)?),
                    // `prepend: true` puts a before callback at the front of
                    // its chain (an after callback already is: ActiveModel
                    // sets `prepend` on every after callback it defines).
                    "prepend" => {
                        let value = assoc.value();
                        prepend = if value.as_true_node().is_some() {
                            true
                        } else if value.as_false_node().is_some() {
                            false
                        } else {
                            return None;
                        };
                    }
                    // Anything else: not modeled — reject rather than run
                    // the callback in the wrong place.
                    _ => return None,
                }
            }
        } else if (arg.as_constant_read_node().is_some() || arg.as_constant_path_node().is_some()) && object.is_none() {
            // A callback OBJECT naming a constant (`after_create Methods`):
            // Rails sends it `<hook>(record)`.
            object = Some(ingest_expr(&arg, file).ok()?);
        } else {
            // Lambda / block-pass target (`after_create -> { … }`):
            // stays an Unknown item (block-form bodies are handled by
            // `push_callback_methods`; lambda args are still a gap).
            return None;
        }
    }
    if targets.is_empty() == object.is_none() {
        return None;
    }
    if prepend && !matches!(hook, CallbackHook::BeforeValidation | CallbackHook::BeforeSave | CallbackHook::BeforeCreate | CallbackHook::BeforeUpdate | CallbackHook::BeforeDestroy) {
        prepend = false;
    }
    // `on:` is only expressible where the runtime hook surface can
    // carry it — the validation hooks (new_record? guard). Rails
    // doesn't accept `on:` for the save/create/update/destroy hooks,
    // so anything else here is either source that doesn't run in
    // Rails or a shape we can't lower faithfully. (The transaction
    // hooks take theirs in `transaction_callback`.)
    if on.is_some() && !validation_hook {
        return None;
    }

    let condition = match (if_cond, unless_cond) {
        (None, None) => None,
        (Some(c), None) => Some(c),
        (None, Some(c)) => Some(negate_condition(c)),
        (Some(a), Some(b)) => Some(and_condition(a, negate_condition(b))),
    };

    Some(Callback { hook, targets, on, on_contexts, condition, on_any: Vec::new(), object, prepend })
}

/// The transaction callbacks (activerecord transactions.rb, the Rails
/// native Core runs): `after_commit` and its `after_{create,update,
/// destroy,save}_commit` sugar, `after_rollback`, `before_commit`.
/// `None` for any other name; `Some(None)` for a declaration of one this
/// does not model, which stays an Unknown item (and is reported).
///
/// What Rails' `set_options_for_callbacks!` and `set_callback` accept:
/// - symbol targets (`after_commit :a, :b`), or one callback object (a
///   constant, `Foo.new(...)`, any non-symbol expression that is not a
///   lambda), which Rails calls as `<hook>(record)`. A lambda or block
///   target is the block form (`markers::push_block_callback`);
/// - `on:` a symbol or an array of `:create`/`:update`/`:destroy`; the
///   sugar spellings enforce theirs over any written one, as
///   `options.merge!(enforced_options)` does;
/// - `if:`/`unless:` a symbol, a lambda (no parameter, or one: Rails
///   `instance_exec`s it with the record as `self` and as the argument),
///   or an array of those, all of which must hold;
/// - `prepend:` a literal boolean;
/// - any other key, which ActiveSupport::Callbacks ignores: Core writes
///   `only: :create` six times, and native Rails runs those callbacks on
///   every action (checked on the pinned Rails, tests/transaction_callbacks_pg_runtime.rs).
fn transaction_callback(
    call: &ruby_prism::CallNode<'_>,
    method: &str,
    file: &str,
) -> Option<Option<crate::dialect::Callback>> {
    use crate::dialect::{Callback, CallbackHook, CallbackOn};

    let (hook, enforced): (CallbackHook, &[CallbackOn]) = match method {
        "after_commit" => (CallbackHook::AfterCommit, &[]),
        "after_create_commit" => (CallbackHook::AfterCommit, &[CallbackOn::Create]),
        "after_update_commit" => (CallbackHook::AfterCommit, &[CallbackOn::Update]),
        "after_destroy_commit" => (CallbackHook::AfterCommit, &[CallbackOn::Destroy]),
        "after_save_commit" => (CallbackHook::AfterCommit, &[CallbackOn::Create, CallbackOn::Update]),
        "after_rollback" => (CallbackHook::AfterRollback, &[]),
        "before_commit" => (CallbackHook::BeforeCommit, &[]),
        _ => return None,
    };
    let parsed = (|| {
        if call.block().is_some() {
            return None;
        }
        let args = call.arguments()?;
        let mut targets: Vec<Symbol> = Vec::new();
        let mut object: Option<Expr> = None;
        let mut on_any: Vec<CallbackOn> = Vec::new();
        // ActiveSupport::Callbacks evaluates every `if:` and then every
        // `unless:`, whichever key is written first.
        let mut conditions: Vec<Expr> = Vec::new();
        let mut unless_conditions: Vec<Expr> = Vec::new();
        let mut prepend = false;
        for arg in args.arguments().iter() {
            if let Some(sym) = symbol_value(&arg) {
                targets.push(Symbol::from(sym.as_str()));
            } else if let Some(kh) = arg.as_keyword_hash_node() {
                for el in kh.elements().iter() {
                    let assoc = el.as_assoc_node()?;
                    let key = symbol_value(&assoc.key())?;
                    let value = assoc.value();
                    match key.as_str() {
                        "on" => {
                            on_any.clear();
                            let values: Vec<ruby_prism::Node<'_>> = match value.as_array_node() {
                                Some(array) => array.elements().iter().collect(),
                                None => vec![value],
                            };
                            for v in values {
                                let action = match symbol_value(&v)?.as_str() {
                                    "create" => CallbackOn::Create,
                                    "update" => CallbackOn::Update,
                                    "destroy" => CallbackOn::Destroy,
                                    // Rails raises ArgumentError at class load.
                                    _ => return None,
                                };
                                on_any.push(action);
                            }
                        }
                        "if" | "unless" => {
                            let values: Vec<ruby_prism::Node<'_>> = match value.as_array_node() {
                                Some(array) => array.elements().iter().collect(),
                                None => vec![value],
                            };
                            for v in values {
                                let cond = transaction_callback_condition(&v, file)?;
                                if key == "if" {
                                    conditions.push(cond);
                                } else {
                                    unless_conditions.push(negate_condition(cond));
                                }
                            }
                        }
                        "prepend" => {
                            prepend = if value.as_true_node().is_some() {
                                true
                            } else if value.as_false_node().is_some() {
                                false
                            } else {
                                return None;
                            };
                        }
                        _ => {}
                    }
                }
            } else if arg.as_lambda_node().is_some()
                || arg.as_block_argument_node().is_some()
                || spelled_lambda_parts(&arg).is_some()
                || object.is_some()
            {
                return None;
            } else {
                object = Some(ingest_expr(&arg, file).ok()?);
            }
        }
        if targets.is_empty() == object.is_none() {
            return None;
        }
        if !enforced.is_empty() {
            on_any = enforced.to_vec();
        }
        let condition = conditions.into_iter().chain(unless_conditions).reduce(and_condition);
        Some(Callback { hook, targets, on: None, on_contexts: Vec::new(), condition, on_any, object, prepend })
    })();
    Some(parsed)
}

/// One `if:`/`unless:` condition of a transaction callback: a symbol (the
/// predicate method), or a lambda/proc whose one expression is the guard
/// (`simple_condition_body`). A one-parameter lambda is
/// `instance_exec(record, &block)` in ActiveSupport::Callbacks, so its
/// parameter is the record, as `self` is: its reads become `self`.
fn transaction_callback_condition(value: &ruby_prism::Node<'_>, file: &str) -> Option<Expr> {
    let (params, body) = if let Some(lambda) = value.as_lambda_node() {
        (lambda.parameters(), lambda.body())
    } else if let Some(parts) = spelled_lambda_parts(value) {
        parts
    } else {
        return callback_condition(value, file);
    };
    let record = params.as_ref().and_then(single_required_param);
    let params = if record.is_some() { None } else { params };
    let body = simple_condition_body(params, body)?;
    let mut cond = ingest_expr(&body, file).ok()?;
    if let Some(name) = record {
        let mut bound = false;
        rebind_local_to_self(&mut cond, &name, &mut bound);
    }
    Some(cond)
}

/// The name of a block's single required parameter (`|r|`, `(r)`), or
/// None for any other parameter list.
fn single_required_param(params: &ruby_prism::Node<'_>) -> Option<String> {
    let bp = params.as_block_parameters_node()?;
    if bp.locals().iter().next().is_some() {
        return None;
    }
    let p = bp.parameters()?;
    if p.optionals().iter().next().is_some()
        || p.rest().is_some()
        || p.posts().iter().next().is_some()
        || p.keywords().iter().next().is_some()
        || p.keyword_rest().is_some()
        || p.block().is_some()
    {
        return None;
    }
    let mut reqs = p.requireds().iter();
    let only = reqs.next()?.as_required_parameter_node()?;
    if reqs.next().is_some() {
        return None;
    }
    Some(constant_id_str(&only.name()).to_string())
}

/// Every read of the local `name` in `expr` becomes `self`.
fn rebind_local_to_self(expr: &mut Expr, name: &str, bound: &mut bool) {
    if matches!(&*expr.node, ExprNode::Var { name: n, .. } if n.as_str() == name) {
        *expr = Expr::new(expr.span, ExprNode::SelfRef);
        *bound = true;
        return;
    }
    expr.node.for_each_child_mut(&mut |child| rebind_local_to_self(child, name, bound));
}

/// A callback's `if:`/`unless:` value → the condition expression that
/// guards the callback body. A zero-arity lambda/proc contributes its
/// body (Rails `instance_exec`s it with `self` the record, which is what
/// a spliced body sees). A Symbol is the predicate method, called on the
/// record. Anything else (a lambda with parameters, an array of
/// conditions) is unmodeled and declines.
fn callback_condition(value: &ruby_prism::Node<'_>, file: &str) -> Option<Expr> {
    if let Some(lambda) = value.as_lambda_node() {
        let body = simple_condition_body(lambda.parameters(), lambda.body())?;
        return ingest_expr(&body, file).ok();
    }
    if let Some(call) = value.as_call_node() {
        if call.receiver().is_none() {
            let name = constant_id_str(&call.name());
            if name == "proc" || name == "lambda" {
                let block = call.block()?.as_block_node()?;
                let body = simple_condition_body(block.parameters(), block.body())?;
                return ingest_expr(&body, file).ok();
            }
        }
    }
    let sym = symbol_value(value)?;
    Some(Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: None,
            method: Symbol::from(sym.as_str()),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    ))
}

/// The one expression a lambda/proc callback condition may splice into the
/// hook as its guard. The guard runs inline in the callback, with `self`
/// as the record and no frame of its own, so only a body that is the same
/// expression there is accepted:
///
/// * no parameters (`-> {}`, `->() {}`, `proc { }`, `proc { || }`) — a
///   `->(r) { r.title… }` would splice `r` unbound (numbered params and
///   `it` are parameters too);
/// * exactly one statement, with no `return`/`next`/`break`/`redo`/
///   `retry` — inside the hook a `return` exits the whole callback chain;
/// * no local writes — they would leak into the hook's scope. That
///   includes a local bound through a target: a multi-write
///   `(a, b = …)`, a `=> t` match-write, a `rescue => e`; and any
///   multi-write declines, its targets being locals or not.
///
/// Anything else declines (`None`), and the callback falls back to the
/// unsupported-DSL warning, as it did before conditions were modelled.
fn simple_condition_body<'pr>(
    params: Option<ruby_prism::Node<'pr>>,
    body: Option<ruby_prism::Node<'pr>>,
) -> Option<ruby_prism::Node<'pr>> {
    if let Some(p) = params {
        let bp = p.as_block_parameters_node()?;
        if bp.parameters().is_some() || bp.locals().iter().next().is_some() {
            return None;
        }
    }
    let stmts = body?.as_statements_node()?;
    let mut it = stmts.body().iter();
    let only = it.next()?;
    if it.next().is_some() {
        return None;
    }

    struct Escapes(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Escapes {
        fn visit_return_node(&mut self, _: &ruby_prism::ReturnNode<'pr>) { self.0 = true; }
        fn visit_next_node(&mut self, _: &ruby_prism::NextNode<'pr>) { self.0 = true; }
        fn visit_break_node(&mut self, _: &ruby_prism::BreakNode<'pr>) { self.0 = true; }
        fn visit_redo_node(&mut self, _: &ruby_prism::RedoNode<'pr>) { self.0 = true; }
        fn visit_retry_node(&mut self, _: &ruby_prism::RetryNode<'pr>) { self.0 = true; }
        fn visit_local_variable_write_node(&mut self, _: &ruby_prism::LocalVariableWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_local_variable_operator_write_node(
            &mut self,
            _: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
        ) {
            self.0 = true;
        }
        fn visit_local_variable_or_write_node(&mut self, _: &ruby_prism::LocalVariableOrWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_local_variable_and_write_node(&mut self, _: &ruby_prism::LocalVariableAndWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_local_variable_target_node(&mut self, _: &ruby_prism::LocalVariableTargetNode<'pr>) {
            self.0 = true;
        }
        fn visit_multi_write_node(&mut self, _: &ruby_prism::MultiWriteNode<'pr>) {
            self.0 = true;
        }
    }
    let mut v = Escapes(false);
    ruby_prism::Visit::visit(&mut v, &only);
    if v.0 { None } else { Some(only) }
}

fn negate_condition(cond: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(cond),
            method: Symbol::from("!"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    )
}

fn and_condition(left: Expr, right: Expr) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::And,
            surface: crate::expr::BoolOpSurface::Symbol,
            left,
            right,
        },
    )
}

fn parse_scope(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
) -> IngestResult<Option<crate::dialect::Scope>> {
    use crate::dialect::Scope;

    let Some(args) = call.arguments() else { return Ok(None) };
    let all_args = args.arguments();
    let mut iter = all_args.iter();

    let Some(name_node) = iter.next() else { return Ok(None) };
    let Some(name_str) = symbol_value(&name_node) else { return Ok(None) };
    let name = Symbol::from(name_str.as_str());

    let Some(body_node) = iter.next() else { return Ok(None) };
    // `scope :for_tools, (lambda do |tools| … end)` — Procore wraps the
    // spelled-out form in its own parens (`reports/app/models/
    // report.rb`'s `for_tools`, `for_data_sets`, `shared`). The parens
    // are surface-only (same treatment `ingest_expr` gives them
    // generally); unwrapping to the single inner statement is what
    // lets the `lambda`/`proc`/`->` checks below see the call or
    // lambda node they expect instead of a `ParenthesesNode`.
    let body_node = unwrap_parenthesized_single_statement(body_node);
    // A scope body is a lambda in one of two spellings: the arrow form
    // `->(x) { ... }` (a LambdaNode) or the spelled-out `lambda { |x| … }`
    // / `proc { |x| … }` (a receiverless CallNode whose block carries the
    // same parameters + body — Mastodon's multi-line scopes use this).
    // `proc { … }` is the one spelling whose jumps are a proc's.
    let form = match body_node.as_call_node() {
        Some(call) if constant_id_str(&call.name()) == "proc" => crate::lower::lifted_block::Form::Block,
        _ => crate::lower::lifted_block::Form::Lambda,
    };
    let (param_node, lambda_body) = if let Some(lambda) = body_node.as_lambda_node() {
        (lambda.parameters(), lambda.body())
    } else if let Some((params, body)) = spelled_lambda_parts(&body_node) {
        (params, body)
    } else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "scope :{name} body must be a lambda (`-> {{ ... }}` or `lambda {{ ... }}`)"
            ),
        });
    };

    // Lambda parameters: required (`->(user)`), optional-with-default
    // (`->(user = nil)`), and keywords (`->(user, unmerged: true)` —
    // lobsters' base/recent scopes). Defaults are carried so the
    // lowered class method reproduces the signature; the lowerer
    // inserts the trailing relation parameter before any keywords.
    // Block and anonymous-splat scope params still fall through unrecorded.
    let mut params: Vec<crate::dialect::Param> = Vec::new();
    if let Some(pn) = param_node
        .and_then(|p| p.as_block_parameters_node().and_then(|bpn| bpn.parameters()))
    {
        for req in pn.requireds().iter() {
            if let Some(rp) = req.as_required_parameter_node() {
                params.push(crate::dialect::Param::positional(Symbol::from(
                    constant_id_str(&rp.name()),
                )));
            }
        }
        for opt in pn.optionals().iter() {
            if let Some(op) = opt.as_optional_parameter_node() {
                let default = ingest_expr(&op.value(), file)?;
                params.push(crate::dialect::Param::with_default(
                    Symbol::from(constant_id_str(&op.name())),
                    default,
                ));
            }
        }
        // `->(key, *rest)`: the splat. The lowered class method carries
        // it, with the relation as a keyword after it (`push_scope_methods`).
        if let Some(rest) = pn.rest().and_then(|r| r.as_rest_parameter_node())
            && let Some(name) = rest.name()
        {
            params.push(crate::dialect::Param::rest(Symbol::from(constant_id_str(&name))));
        }
        for kw in pn.keywords().iter() {
            if let Some(okw) = kw.as_optional_keyword_parameter_node() {
                let default = ingest_expr(&okw.value(), file)?;
                params.push(crate::dialect::Param::keyword(
                    Symbol::from(constant_id_str(&okw.name())),
                    Some(default),
                ));
            } else if let Some(rkw) = kw.as_required_keyword_parameter_node() {
                params.push(crate::dialect::Param::keyword(
                    Symbol::from(constant_id_str(&rkw.name())),
                    None,
                ));
            }
        }
    }

    let mut body = match lambda_body {
        Some(b) => ingest_expr(&b, file)?,
        None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
    };
    // The body becomes a class method, where the lambda's own `next v`
    // (its early return) is `return v`; `next` in a def does not parse.
    // Rails runs the body as `instance_exec(*args, &body) || self`
    // (`ActiveRecord::Relation#_exec_scope`), so a falsy `next` answers
    // the relation: a bare `next`, `next nil` or `next false` returns
    // `all`, and any other value `v` returns `v || all` unless it is
    // `all` or `none` itself. A longer chain can still be falsy
    // (`where(x).presence`, `find_by(x)`, a helper returning nil).
    crate::lower::lifted_block::to_method_body_with(&mut body, form, &scope_next_value);

    Ok(Some(Scope { name, params, body }))
}

/// The value a scope body's `next` returns from the class method it
/// becomes; see `parse_scope`.
fn scope_next_value(value: Option<Expr>, span: Span) -> Expr {
    let all = || {
        Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: Symbol::from("all"),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        )
    };
    let Some(value) = value else { return all() };
    if matches!(
        &*value.node,
        ExprNode::Lit { value: crate::expr::Literal::Nil | crate::expr::Literal::Bool { value: false } }
    ) {
        return all();
    }
    if is_all_or_none(&value) {
        return value;
    }
    Expr::new(
        span,
        ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: crate::expr::BoolOpSurface::Symbol,
            left: value,
            right: all(),
        },
    )
}

/// A receiverless `all` or `none`: a relation, so `|| all` after it
/// is dead.
fn is_all_or_none(e: &Expr) -> bool {
    matches!(
        &*e.node,
        ExprNode::Send { recv: None, method, args, block: None, .. }
            if args.is_empty() && matches!(method.as_str(), "all" | "none")
    )
}

/// `(expr)` around a single statement — surface-only parens, same as
/// the ones `ingest_expr`'s own `ParenthesesNode` arm strips. Only
/// unwraps when there's exactly one statement inside: `(a; b)`'s two
/// void statements aren't a lambda in disguise, so those are left
/// alone and fail the caller's lambda check same as before.
fn unwrap_parenthesized_single_statement(node: Node<'_>) -> Node<'_> {
    let Some(paren) = node.as_parentheses_node() else { return node };
    let Some(inner) = paren.body() else { return node };
    let mut stmts = flatten_statements(inner);
    if stmts.len() == 1 {
        stmts.pop().unwrap()
    } else {
        node
    }
}

/// `lambda { |x| … }` / `proc { |x| … }` — a receiverless call whose
/// braces-block carries the parameters and body. Returns the same
/// `(parameters, body)` pair a `LambdaNode` exposes so `parse_scope`
/// treats both spellings identically. `None` for anything else
/// (including block-pass `lambda(&blk)`, which has no BlockNode).
fn spelled_lambda_parts<'a>(
    node: &Node<'a>,
) -> Option<(Option<Node<'a>>, Option<Node<'a>>)> {
    let call = node.as_call_node()?;
    if call.receiver().is_some() {
        return None;
    }
    if !matches!(constant_id_str(&call.name()), "lambda" | "proc") {
        return None;
    }
    let block = call.block()?.as_block_node()?;
    Some((block.parameters(), block.body()))
}

pub(super) fn parse_association(
    call: &ruby_prism::CallNode<'_>,
    owner: &ClassId,
    method: &str,
    file: &str,
) -> Option<crate::dialect::Association> {
    use super::util::{bool_value, string_value};
    use crate::dialect::{Association, Dependent};

    let args = call.arguments()?;
    let all_args = args.arguments();
    let mut iter = all_args.iter();
    let first = iter.next()?;
    let name_str = symbol_value(&first)?;
    let name = Symbol::from(name_str.as_str());

    let mut class_name: Option<String> = None;
    let mut foreign_key: Option<String> = None;
    let mut primary_key: Option<String> = None;
    // `belongs_to :x, polymorphic: true, foreign_type: "object_type"` — the
    // type column, where Rails otherwise reads `x_type`.
    let mut foreign_type: Option<String> = None;
    let mut through: Option<String> = None;
    let mut source: Option<String> = None;
    let mut source_type: Option<String> = None;
    let mut dependent: Option<Dependent> = None;
    let mut optional: Option<bool> = None;
    // `required: x` is ActiveRecord's older spelling of `optional: !x`, and wins.
    let mut required: Option<bool> = None;
    let mut join_table: Option<String> = None;
    let mut scope: Option<crate::expr::Expr> = None;
    // The lambda's single required parameter (the owner), when it has one.
    let mut scope_owner: Option<Symbol> = None;
    let mut polymorphic: Option<bool> = None;
    let mut as_interface: Option<String> = None;
    let mut belongs_to_default: Option<crate::expr::Expr> = None;
    let mut touch: Option<crate::dialect::Touch> = None;
    let mut autosave: Option<bool> = None;
    let mut inverse_of: Option<String> = None;
    let mut validate: Option<bool> = None;
    // The composite-key spellings of `foreign_key:` / `primary_key:` /
    // `query_constraints:`, plain and under Core's `yugabyte_overrides:`.
    let mut plain_keys = composite_keys::KeyOptions::default();
    let mut override_keys: Option<composite_keys::KeyOptions> = None;

    for arg in iter {
        // Positional lambda between name and kwargs — the association
        // scope (`has_many :x, -> { where(...) }, through: :y`).
        // Recorded as its raw body Expr; the reader synthesis grafts it
        // onto the relation seed. A lambda taking one parameter is an
        // owner-dependent scope (Rails `instance_exec`s it with the
        // record); the parameter name is kept so each consumer can bind
        // it to the owner, or decline it.
        if let Some(lambda) = arg.as_lambda_node() {
            if scope.is_none() {
                let params = lambda
                    .parameters()
                    .and_then(|p| p.as_block_parameters_node().and_then(|b| b.parameters()));
                let required: Vec<String> = params
                    .as_ref()
                    .map(|pn| {
                        pn.requireds()
                            .iter()
                            .filter_map(|r| r.as_required_parameter_node())
                            .map(|r| constant_id_str(&r.name()).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let only_required = params.as_ref().is_none_or(|pn| {
                    pn.requireds().iter().count() == required.len()
                        && pn.optionals().iter().next().is_none()
                        && pn.rest().is_none()
                        && pn.posts().iter().next().is_none()
                        && pn.keywords().iter().next().is_none()
                        && pn.keyword_rest().is_none()
                        && pn.block().is_none()
                });
                if only_required && required.len() <= 1 {
                    scope = lambda.body().and_then(|b| ingest_expr(&b, file).ok());
                    if scope.is_some() {
                        scope_owner = required.first().map(|n| Symbol::from(n.as_str()));
                    }
                }
            }
            continue;
        }
        let Some(kh) = arg.as_keyword_hash_node() else { continue };
        for el in kh.elements().iter() {
            let Some(assoc) = el.as_assoc_node() else { continue };
            let Some(key) = symbol_value(&assoc.key()) else { continue };
            let value = assoc.value();
            match key.as_str() {
                // `"::Portal"` names the top-level class `Portal`. Kept as
                // written: app assembly's `qualify_association_targets`
                // resolves a relative name in the owner's namespace and
                // only unroots a rooted one.
                "class_name" => class_name = string_value(&value),
                "foreign_key" => {
                    foreign_key = string_value(&value).or_else(|| symbol_value(&value));
                    plain_keys.foreign_key = key_option(&value);
                }
                "primary_key" => {
                    primary_key = string_value(&value).or_else(|| symbol_value(&value));
                    plain_keys.primary_key = key_option(&value);
                }
                "query_constraints" => plain_keys.query_constraints = key_option(&value),
                "yugabyte_overrides" => override_keys = value.as_hash_node().map(|h| key_options_of(&h)),
                "foreign_type" => {
                    foreign_type = string_value(&value).or_else(|| symbol_value(&value))
                }
                "through" => through = symbol_value(&value),
                "source" => source = symbol_value(&value),
                "source_type" => source_type = string_value(&value).map(|s| s.trim_start_matches("::").to_string()),
                "dependent" => {
                    dependent = symbol_value(&value).and_then(|s| dependent_from_sym(&s))
                }
                "optional" => optional = bool_value(&value),
                "required" => required = bool_value(&value),
                // `touch: true` / `touch: :last_message_at`. A `false`
                // is Rails' explicit opt-out and reads as no touch,
                // which is what `None` already means.
                "touch" => {
                    touch = match bool_value(&value) {
                        Some(true) => Some(crate::dialect::Touch::UpdatedAt),
                        Some(false) => None,
                        None => symbol_value(&value)
                            .map(|s| crate::dialect::Touch::Column(Symbol::from(s.as_str()))),
                    }
                }
                "join_table" => join_table = string_value(&value),
                "polymorphic" => polymorphic = bool_value(&value),
                "as" => as_interface = super::util::symbol_or_string_value(&value),
                "autosave" => autosave = bool_value(&value),
                "validate" => validate = bool_value(&value),
                "inverse_of" => inverse_of = symbol_value(&value),
                // `default: -> { Current.user }` — the lambda BODY, not
                // the lambda. Rails calls it with `instance_exec`, so
                // the body is already written against the record; a
                // param-taking form would need the record threaded and
                // nothing in the corpus writes one.
                "default" => {
                    belongs_to_default = value
                        .as_lambda_node()
                        .filter(|l| {
                            l.parameters()
                                .and_then(|p| p.as_block_parameters_node().and_then(|b| b.parameters()))
                                .map(|pn| pn.requireds().iter().next().is_none())
                                .unwrap_or(true)
                        })
                        .and_then(|l| l.body())
                        .and_then(|b| ingest_expr(&b, file).ok());
                }
                _ => {}
            }
        }
    }

    // A composite key: the primary pair fills the single-column fields and
    // the rest rides in `extra_keys`. Core's `yugabyte_overrides` replace
    // the plain options when written.
    let mut extra_keys: Vec<(Symbol, Symbol)> = Vec::new();
    if matches!(method, "has_many" | "has_one" | "belongs_to") {
        if let Some(resolved) = composite_keys::resolve(&plain_keys, override_keys.as_ref()) {
            foreign_key = Some(resolved.foreign_key);
            primary_key = resolved.primary_key;
            extra_keys = resolved
                .extra_keys
                .into_iter()
                .map(|(a, b)| (Symbol::from(a.as_str()), Symbol::from(b.as_str())))
                .collect();
        } else if let Some(over) = override_keys.as_ref().and_then(|o| o.foreign_key.as_ref()) {
            // Scalar overrides replace the plain scalar key too.
            foreign_key = over.columns.first().cloned();
            primary_key = override_keys
                .as_ref()
                .and_then(|o| o.primary_key.as_ref())
                .and_then(|p| p.columns.first().cloned())
                .filter(|p| p != "id");
        }
    }
    let foreign_key_explicit = foreign_key.is_some();
    let optional = required.map(|r| !r).or(optional);

    // Rails `foreign_key` demodulizes: `Billing::Invoice` → `invoice_id`.
    let owner_snake = snake_case(crate::naming::demodulize(owner.0.as_str()));

    // Association-extension block: `has_many :memberships do def
    // grant_to(users) … end end`. Only `def`s are collected — a block
    // body that does anything else is not an extension module and is
    // left where it is rather than half-read.
    let extension: Vec<crate::dialect::MethodDef> = call
        .block()
        .and_then(|b| b.as_block_node())
        .and_then(|b| b.body())
        .and_then(|b| b.as_statements_node())
        .map(|stmts| {
            stmts
                .body()
                .iter()
                .filter_map(|s| s.as_def_node())
                .filter_map(|d| ingest_method(&d, file).ok())
                .collect()
        })
        .unwrap_or_default();

    match method {
        "has_many" => Some(Association::HasMany {
            name: name.clone(),
            extension,
            // `source:` names the association on the `through:` model
            // that supplies the rows (`has_many :upvoted_stories,
            // through: :votes, source: :story` → Story, not the
            // assoc-name-derived "UpvotedStory" phantom). class_name
            // still wins when both are given, per Rails.
            //
            // SINGULARIZED, because the source association can be a
            // has_many and then its name is PLURAL: campfire's
            // `has_many :reachable_messages, through: :rooms, source:
            // :messages` means Room#messages, so the class is Message.
            // Camelizing alone produced a `Messages` phantom — and
            // campfire has a `Messages::` controller MODULE by that
            // name, so the reader resolved to it and failed at the
            // `where` instead of at the missing constant. Singular
            // sources are unaffected (Rails' inflector answers
            // "story" for "story"), which is every other corpus use.
            //
            // `source_type:` disambiguates a *polymorphic* source
            // reflection, naming the concrete class directly
            // (`has_many :comment_references, through: :mod_mail_references,
            // source: :reference, source_type: "Comment"` → Comment). It
            // takes precedence over the camelized `source` name, which
            // would otherwise be the polymorphic association name
            // ("Reference") — a phantom class. Already CamelCase, so no
            // transform.
            target: class_name
                .map(|s| ClassId(Symbol::from(s.as_str())))
                .or_else(|| source_type.map(|s| ClassId(Symbol::from(s.as_str()))))
                .or_else(|| source.map(|s| ClassId(Symbol::from(singularize_camelize(s.as_str())))))
                .unwrap_or_else(|| ClassId(Symbol::from(singularize_camelize(name_str.as_str())))),
            // `as: :notifiable` — the rows point back through the
            // interface columns, not an owner-named key.
            foreign_key: foreign_key
                .map(|s| Symbol::from(s.as_str()))
                .unwrap_or_else(|| match &as_interface {
                    Some(intf) => Symbol::from(format!("{intf}_id")),
                    None => Symbol::from(format!("{owner_snake}_id")),
                }),
            foreign_key_explicit,
            inverse_of: inverse_of.as_deref().map(Symbol::from),
            through: through.map(|s| Symbol::from(s.as_str())),
            dependent: dependent.unwrap_or_default(),
            as_interface: as_interface.as_deref().map(Symbol::from),
            scope,
            scope_owner,
            skip_validation: validate == Some(false),
            autosave: autosave == Some(true),
            no_autosave: autosave == Some(false),
            primary_key: primary_key.as_deref().map(Symbol::from),
            extra_keys,
        }),
        "has_one" => Some(Association::HasOne {
            name: name.clone(),
            target: class_name
                .map(|s| ClassId(Symbol::from(s.as_str())))
                .or_else(|| source_type.map(|s| ClassId(Symbol::from(s.as_str()))))
                .or_else(|| source.clone().map(|s| ClassId(Symbol::from(singularize_camelize(s.as_str())))))
                .unwrap_or_else(|| ClassId(Symbol::from(camelize(name_str.as_str())))),
            through: through.map(|s| Symbol::from(s.as_str())),
            source: source.map(|s| Symbol::from(s.as_str())),
            foreign_key: foreign_key
                .map(|s| Symbol::from(s.as_str()))
                .unwrap_or_else(|| match &as_interface {
                    Some(intf) => Symbol::from(format!("{intf}_id")),
                    None => Symbol::from(format!("{owner_snake}_id")),
                }),
            foreign_key_explicit,
            inverse_of: inverse_of.as_deref().map(Symbol::from),
            dependent: dependent.unwrap_or_default(),
            as_interface: as_interface.as_deref().map(Symbol::from),
            scope,
            scope_owner,
            autosave: autosave.unwrap_or(false),
            primary_key: primary_key.as_deref().map(Symbol::from),
            extra_keys,
        }),
        "belongs_to" => Some(Association::BelongsTo {
            name: name.clone(),
            target: class_name
                .map(|s| ClassId(Symbol::from(s.as_str())))
                .unwrap_or_else(|| ClassId(Symbol::from(camelize(name_str.as_str())))),
            foreign_key: foreign_key
                .map(|s| Symbol::from(s.as_str()))
                .unwrap_or_else(|| Symbol::from(format!("{name_str}_id"))),
            optional: optional.unwrap_or(false),
            declared_optional: optional,
            polymorphic: polymorphic.unwrap_or(false),
            // Filled by `resolve_polymorphic_targets` once every
            // model's inverse `as:` declarations are ingested.
            polymorphic_targets: Vec::new(),
            default: belongs_to_default,
            touch,
            foreign_type: foreign_type.as_deref().map(Symbol::from),
            primary_key: primary_key.as_deref().map(Symbol::from),
            autosave,
            extra_keys,
            constraint_keys_preserved: override_keys.as_ref().is_some_and(composite_keys::constraint_keys_preserved),
        }),
        "has_and_belongs_to_many" => Some(Association::HasAndBelongsToMany {
            name: name.clone(),
            target: class_name
                .map(|s| ClassId(Symbol::from(s.as_str())))
                .unwrap_or_else(|| ClassId(Symbol::from(singularize_camelize(name_str.as_str())))),
            join_table: join_table
                .map(|s| Symbol::from(s.as_str()))
                .unwrap_or_else(|| Symbol::from(default_habtm_table(owner, name_str.as_str()))),
        }),
        _ => None,
    }
}

/// A key option's columns: `:shop_id`, `"shop_id"` or `[:shop_id, :id]`.
fn key_option(node: &Node<'_>) -> Option<composite_keys::KeyOption> {
    if let Some(one) = string_value(node).or_else(|| symbol_value(node)) {
        return Some(composite_keys::KeyOption { columns: vec![one], array: false });
    }
    let columns = node
        .as_array_node()?
        .elements()
        .iter()
        .map(|e| string_value(&e).or_else(|| symbol_value(&e)))
        .collect::<Option<Vec<_>>>()?;
    Some(composite_keys::KeyOption { columns, array: true })
}

/// The key options inside a `yugabyte_overrides: { .. }` hash literal.
fn key_options_of(hash: &ruby_prism::HashNode<'_>) -> composite_keys::KeyOptions {
    let mut out = composite_keys::KeyOptions::default();
    for el in hash.elements().iter() {
        let Some(assoc) = el.as_assoc_node() else { continue };
        let Some(key) = symbol_value(&assoc.key()) else { continue };
        let value = key_option(&assoc.value());
        match key.as_str() {
            "foreign_key" => out.foreign_key = value,
            "primary_key" => out.primary_key = value,
            "query_constraints" => out.query_constraints = value,
            _ => {}
        }
    }
    out
}

/// `self.primary_key = "key"` / `self.primary_key = :key` — Rails'
/// per-model override of the `id` default. Prism parses it as a call to
/// `primary_key=` on an explicit `self` receiver, which would otherwise
/// land in `ModelBodyItem::Unknown` and be dropped.
pub(super) fn parse_primary_key_decl(stmt: &Node<'_>) -> Option<Symbol> {
    let call = stmt.as_call_node()?;
    call.receiver()?.as_self_node()?;
    if constant_id_str(&call.name()) != "primary_key=" {
        return None;
    }
    let args = call.arguments()?;
    let first = args.arguments().iter().next()?;
    let name = string_value(&first).or_else(|| symbol_value(&first))?;
    Some(Symbol::from(name.as_str()))
}

/// `self.primary_key = [:shop_id, :id]` — Rails' composite primary key,
/// a literal list of two or more string/symbol column names.
pub(super) fn parse_composite_primary_key_decl(stmt: &Node<'_>) -> Option<Vec<Symbol>> {
    let call = stmt.as_call_node()?;
    call.receiver()?.as_self_node()?;
    if constant_id_str(&call.name()) != "primary_key=" {
        return None;
    }
    let args = call.arguments()?;
    if args.arguments().len() != 1 {
        return None;
    }
    let array = args.arguments().iter().next()?.as_array_node()?;
    let keys = array
        .elements()
        .iter()
        .map(|e| string_value(&e).or_else(|| symbol_value(&e)).map(|n| Symbol::from(n.as_str())))
        .collect::<Option<Vec<_>>>()?;
    (keys.len() > 1).then_some(keys)
}

/// Bind one direct literal string/symbol `self.table_name` before reading the schema.
/// Scan the selected class's executable body first: a conditional, compound
/// or subsequent write must not leave a plausible but incorrect row bound.
/// Method and nested namespace bodies are separate scopes; their headers
/// still execute in the enclosing scope and must not hide table writes.
fn parse_table_name_decl(body: Node<'_>, file: &str) -> IngestResult<Option<(String, usize)>> {
    struct Collector {
        direct: Vec<(usize, usize)>,
        writes: Vec<Option<(String, usize)>>,
    }
    impl Collector {
        fn record(&mut self, node: &Node<'_>) {
            if let Some(call) = node.as_call_node() {
                if constant_id_str(&call.name()) != "table_name=" { return; }
                let offset = node.location().start_offset();
                let valid = self.direct.contains(&(offset, node.location().end_offset()))
                    && call.receiver().is_some_and(|r| r.as_self_node().is_some())
                    && !call.is_safe_navigation() && call.block().is_none();
                let name = valid.then_some(())
                    .and_then(|_| call.arguments())
                    .filter(|args| args.arguments().len() == 1)
                    .and_then(|args| symbol_or_string_value(&args.arguments().iter().next()?))
                    .filter(|name| bare_table_identifier(name));
                self.writes.push(name.map(|name| (name, offset)));
            } else {
                let name = node.as_call_and_write_node().map(|w| w.write_name())
                    .or_else(|| node.as_call_or_write_node().map(|w| w.write_name()))
                    .or_else(|| node.as_call_operator_write_node().map(|w| w.write_name()))
                    .or_else(|| node.as_call_target_node().map(|w| w.name()));
                if name.is_some_and(|name| constant_id_str(&name) == "table_name=") {
                    self.writes.push(None);
                }
            }
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Collector {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) { self.record(&node); }
        fn visit_leaf_node_enter(&mut self, node: Node<'pr>) { self.record(&node); }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if let Some(receiver) = node.receiver() { self.visit(&receiver); }
        }
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.visit(&node.constant_path());
            if let Some(superclass) = node.superclass() { self.visit(&superclass); }
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.visit(&node.constant_path());
        }
    }
    let mut collector = Collector {
        direct: body.as_statements_node()
            .map(|stmts| stmts.body().iter().filter_map(|s| s.as_call_node())
                .map(|s| (s.location().start_offset(), s.location().end_offset())).collect())
            .unwrap_or_else(|| vec![(body.location().start_offset(), body.location().end_offset())]),
        writes: Vec::new(),
    };
    ruby_prism::Visit::visit(&mut collector, &body);
    if collector.writes.is_empty() {
        Ok(table_name_reader_override(&body))
    } else if collector.writes.len() == 1 && collector.writes[0].is_some() {
        Ok(collector.writes.pop().unwrap())
    } else {
        Err(IngestError::Unsupported {
            file: file.into(),
            message: "table_name binding requires one direct self.table_name assignment to a literal string or symbol naming a safe bare SQL identifier".into(),
        })
    }
}

/// Shared DDL/DML currently emits bare table names. Refuse names
/// needing qualification or SQL quoting.
fn bare_table_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !crate::naming::is_sqlite_keyword(name)
}

/// The other way to name the table: a class-side reader answering one
/// literal, `def self.table_name = "x"` or `class << self; def
/// table_name; "x"; end; end` directly in the body (a
/// `Catalog::Item::Detail`). Rails reads the table through that
/// method, so it binds the schema row. The direct `def self.` form is
/// consumed like the setter (lowering writes `def self.table_name` from
/// `Model::table`); the `class << self` form stays, saying the same name.
/// Any other shape, or more than one reader, keeps the convention.
fn table_name_reader_override(body: &Node<'_>) -> Option<(String, usize)> {
    fn literal_reader(def: &ruby_prism::DefNode<'_>) -> Option<String> {
        if constant_id_str(&def.name()) != "table_name" || def.parameters().is_some() {
            return None;
        }
        let body = def.body()?;
        let value = match body.as_statements_node() {
            Some(stmts) if stmts.body().len() == 1 => stmts.body().iter().next()?,
            Some(_) => return None,
            None => body,
        };
        symbol_or_string_value(&value).filter(|name| bare_table_identifier(name))
    }
    fn statements<'pr>(node: &Node<'pr>) -> Vec<Node<'pr>> {
        match node.as_statements_node() {
            Some(stmts) => stmts.body().iter().collect(),
            None => vec![],
        }
    }
    let mut found = Vec::new();
    for stmt in statements(body) {
        if let Some(def) = stmt.as_def_node() {
            if def.receiver().is_some_and(|r| r.as_self_node().is_some()) {
                if let Some(name) = literal_reader(&def) {
                    found.push((name, stmt.location().start_offset()));
                } else if constant_id_str(&def.name()) == "table_name" {
                    return None;
                }
            }
        } else if let Some(sclass) = stmt.as_singleton_class_node() {
            if sclass.expression().as_self_node().is_none() {
                continue;
            }
            for inner in sclass.body().map(|b| statements(&b)).unwrap_or_default() {
                let Some(def) = inner.as_def_node() else { continue };
                if def.receiver().is_some() {
                    continue;
                }
                if let Some(name) = literal_reader(&def) {
                    // Not a body statement: nothing is consumed.
                    found.push((name, usize::MAX));
                } else if constant_id_str(&def.name()) == "table_name" {
                    return None;
                }
            }
        }
    }
    (found.len() == 1).then(|| found.pop()).flatten()
}

pub(crate) fn dependent_from_sym(s: &str) -> Option<crate::dialect::Dependent> {
    use crate::dialect::Dependent;
    Some(match s {
        "destroy" => Dependent::Destroy,
        "destroy_async" => Dependent::DestroyAsync,
        "delete" => Dependent::Delete,
        "delete_all" => Dependent::DeleteAll,
        "nullify" => Dependent::Nullify,
        "restrict_with_exception" | "restrict_with_error" => Dependent::Restrict,
        _ => return None,
    })
}

fn default_habtm_table(owner: &ClassId, target_plural_sym: &str) -> String {
    crate::naming::habtm_join_table(owner.0.as_str(), target_plural_sym)
}

pub(crate) fn row_from_table(table: &Table) -> Row {
    let mut fields = IndexMap::new();
    for col in &table.columns {
        fields.insert(col.name.clone(), ty_of_column_slot(col));
    }
    Row { fields: fields.into(), rest: None }
}

/// The attributes-row type for a column: `ty_of_column` widened with
/// `Nil` where the schema says the column is nullable. Rails stores
/// NULL there until something sets it, so a read genuinely can be nil
/// — validations and analysis both need to see that. The primary key
/// is excluded (the INSERT assigns it and every hydration path treats
/// it as present). Twin of `lower::model_to_library::ty_of_column_slot`;
/// keep them in sync, same as the `ty_of_column` pair below.
fn ty_of_column_slot(col: &crate::schema::Column) -> Ty {
    let base = ty_of_column(&col.col_type);
    if col.nullable && !col.primary_key {
        Ty::Union { variants: vec![base, Ty::Nil].into() }
    } else {
        base
    }
}

fn ty_of_column(t: &ColumnType) -> Ty {
    // A datetime column is a `Time` at the Ruby source level (every use
    // in practice is `created_at.strftime` / `.to_i` / `.after?` /
    // `Time.current` assignment / passed to a Time helper — never a
    // String), so the analyzer sees the first-class `Ty::Time` for those
    // calls to resolve. The runtime *stores* them as ISO-8601 strings,
    // which is the target's column-seam concern (hydration/serialization),
    // not the type. The EMIT-side `lower::model_to_library::ty_of_column`
    // agrees on `Ty::Time` — a target with no native datetime type wired
    // yet surfaces the honest not-supported gap there.
    match t {
        ColumnType::Integer | ColumnType::BigInt => Ty::Int,
        ColumnType::Float | ColumnType::Decimal { .. } => Ty::Float,
        ColumnType::String { .. } | ColumnType::Text => Ty::Str,
        ColumnType::Boolean => Ty::Bool,
        ColumnType::Date => Ty::Date,
        ColumnType::DateTime | ColumnType::Time => Ty::Time,
        ColumnType::Binary => Ty::Str,
        // Rails exposes a schema-less JSON value here: it may be an
        // Array, Hash, scalar, or nil, so neither String nor one fixed
        // container type is honest. The emitted model keeps serialized
        // text in its DB slot and decodes/encodes at the public accessor
        // boundary (`RoundhouseJsonColumn`); analysis uses the deliberate gradual
        // type. A `has_json` declaration adds its stronger per-key schema
        // separately in `lower::has_json`.
        ColumnType::Json | ColumnType::Jsonb => Ty::Untyped,
        ColumnType::Uuid => Ty::Str,
        ColumnType::Reference { .. } => Ty::Int,
    }
}

#[cfg(test)]
mod singleton_visibility_tests {
    use super::*;

    fn ingest(src: &str) -> IngestResult<Option<crate::dialect::Model>> {
        ingest_model(
            src.as_bytes(),
            "app/models/thing.rb",
            &Schema::default(),
            &Default::default(),
        )
    }

    /// A bare `private` inside `class << self` used to abort the whole
    /// file's ingest ("unsupported statement inside `class << self`"),
    /// which drops the model rather than the marker.
    #[test]
    fn a_visibility_marker_in_a_singleton_block_is_resolved_not_refused() {
        let model = ingest(
            "class Thing < ApplicationRecord\n  \
             class << self\n    \
             def visible\n      1\n    end\n\n    \
             private\n      def hidden\n        2\n      end\n  \
             end\nend\n",
        )
        .expect("ingest must not fail on a visibility marker")
        .expect("model");
        let names: Vec<&str> = model
            .body
            .iter()
            .filter_map(|item| match item {
                crate::dialect::ModelBodyItem::Method { method, .. } => Some(method.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["visible", "hidden"], "both singleton methods survive");
        let visibility: Vec<_> = model.body.iter().filter_map(|item| match item {
            crate::dialect::ModelBodyItem::Method { method, .. } => Some(method.visibility),
            _ => None,
        }).collect();
        assert_eq!(visibility, vec![crate::dialect::MethodVisibility::Public, crate::dialect::MethodVisibility::Private]);
    }

    /// A statement the walk genuinely cannot read still refuses — the
    /// skip is for the three markers, not for anything call-shaped.
    #[test]
    fn a_real_statement_in_a_singleton_block_still_refuses() {
        let err = ingest(
            "class Thing < ApplicationRecord\n  \
             class << self\n    memoize_everything :cache\n  end\nend\n",
        );
        assert!(err.is_err(), "an unmodeled singleton statement is still an error: {err:?}");
    }
}

/// The constant writes of a body statement that is a literal list of
/// them, `[A = "a", B = "b"]`, with or without a trailing `.freeze`
/// (Core's `Delivery::Setting`). The list itself is unused, so only
/// the writes it makes carry on; `None` for any other statement,
/// including a list with an element that is not a constant write.
fn constant_writes_in_list<'pr>(stmt: &Node<'pr>) -> Option<Vec<Node<'pr>>> {
    let list = match stmt.as_call_node() {
        Some(call) if call.name().as_slice() == b"freeze" && call.arguments().is_none() => call.receiver()?.as_array_node()?,
        Some(_) => return None,
        None => stmt.as_array_node()?,
    };
    let writes: Vec<Node<'pr>> = list.elements().iter().collect();
    let all = !writes.is_empty() && writes.iter().all(|e| e.as_constant_write_node().is_some());
    all.then_some(writes)
}
