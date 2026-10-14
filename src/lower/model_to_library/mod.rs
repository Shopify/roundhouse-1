//! Lower a Rails-shape `Model` (with associations, validations, callbacks,
//! Schema-derived columns) into a post-lowering `LibraryClass` whose body
//! is a flat sequence of `MethodDef`s — the universal IR shape every
//! emitter consumes (see `project_universal_post_lowering_ir.md`).
//!
//! The output target is the emitted `app/models/<model>.rb` in the
//! spinel-shape tree: explicit method bodies (`def title; @title;
//! end`, `def comments; Comment.where(article_id: @id); end`, `def
//! validate; validates_presence_of(:title) { @title }; end`), no
//! Rails DSL.
//!
//! This module is pure: input is one `Model` plus the app `Schema`, output
//! is one `LibraryClass`. No side-effects, no per-target choices. Per-Rails-
//! idiom lowering is a separate function so each can be tested in
//! isolation (skeleton, schema columns, has_many, belongs_to, validates,
//! callbacks, …).
//!
//! Strangler-fig direction: this lives alongside the existing per-target
//! emit paths. Callers that consume the post-lowering shape opt in
//! explicitly; the rich `Model` dialect remains the input for emitters
//! that haven't migrated.

mod adapter_emit;
mod assign_attributes;
pub(crate) mod accessor_surface;
pub(crate) mod schema;
pub use schema::col_storage_name;
pub use schema::shakeable_synthesized_names;
pub(crate) use schema::generated_attribute_method_names;
pub(crate) mod validations;
pub(crate) mod associations;
pub(crate) use associations::polymorphic_type_switch;
mod attribute_alias;
mod composite_key;
pub(crate) mod broadcasts;
pub(crate) mod markers;
pub(crate) mod macro_replay;
pub(crate) mod macro_associations;
pub(crate) mod transaction_callbacks;
pub(crate) mod private_constant;
pub use macro_replay::replay_model_macros;
pub mod row;

use crate::det::{HashMap, HashSet};

use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver, Model, Param};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::schema::{Column, ColumnType, Schema, Table};
use crate::span::Span;
use crate::ty::{Row, Ty};

use self::associations::{push_association_methods, push_dependent_destroy};
pub(crate) use self::associations::model_defines_instance_method;
pub(crate) use self::markers::attribute_api_decls;
pub(crate) use self::markers::symbol_after_initialize;
pub(crate) use self::markers::block_callback_shape;

/// Push a synthesized instance method unless the model body defines the
/// name (custom methods win — `push_user_methods` runs after the
/// synthesizers and drops collisions, so a synthesized duplicate would
/// shadow the user's) or an earlier synthesizer already claimed it.
/// Shared by the DSL-marker synthesizers (typed_store,
/// secure_password) that run late in `build_methods`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_synth_instance_method(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    name: Symbol,
    params: Vec<Param>,
    body: Expr,
    signature: Option<Ty>,
    kind: AccessorKind,
    mutates_self: bool,
) {
    if model_defines_instance_method(model, &name)
        || methods
            .iter()
            .any(|m| m.receiver == MethodReceiver::Instance && m.name == name)
    {
        return;
    }
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name,
        receiver: MethodReceiver::Instance,
        params,
        body,
        signature,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind,
        is_async: false,
        mutates_self,
        block_param: None,
    });
}
use self::broadcasts::push_broadcasts_methods;
use self::markers::{
    push_attr_accessor_methods, push_callback_methods, push_dom_prefix_method,
    push_unknown_marker_methods,
};
use self::adapter_emit::push_adapter_methods;
use self::schema::push_schema_methods;
use self::validations::push_validate_method;

/// The Ruby-family connection runtime accepts raw request keys and owns their
/// shared normalization. Strict runtimes keep their existing scalar finder
/// contract; they do not ship the union-valued connection/Relation surface.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinderInputs {
    Scalar,
    Request,
}

/// The Ruby-family runtimes create `errors`/`error_details` on first read,
/// as Rails does. Strict targets keep the field read: the Rust emitter
/// turns an `@errors ||= []` reader into a method that does not compile.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorsReaders {
    FieldRead,
    CreatedOnFirstRead,
}

/// Probe bodies expose framework ownership hidden by source overrides,
/// but register the ordinary production definitions for faithful typing.
/// Selection bounds retained bodies, never registry or demand inputs.
pub(crate) enum Materialization<'a> {
    Emit,
    AccessorProbe(&'a HashSet<ClassId>),
}

impl Materialization<'_> {
    fn retains(&self, id: &ClassId) -> bool {
        match self {
            Self::Emit => true,
            Self::AccessorProbe(selected) => selected.contains(id),
        }
    }
}

/// Bulk entry point: lower every model in `models` against `schema`,
/// sharing one class registry so cross-model dispatch (`Article` calling
/// `Comment.where(...)`) types correctly. Use this for whole-app emit;
/// the single-model entry point below is for tests/probes.
///
/// `extra_class_infos` lets callers register additional ClassInfo
/// entries (e.g. lowered view modules so `Views::Articles.article(...)`
/// dispatches type) — passed as flat `(ClassId, ClassInfo)` pairs;
/// callers that want both the full path and a last-segment alias
/// should insert both.
/// Same as [`lower_models_to_library_classes`] but also returns the
/// shared class registry the body-typer used. Callers (e.g. the
/// controller lowerer) can extend that registry with their own
/// entries to keep cross-class dispatch resolving consistently.
pub fn lower_models_with_registry(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    let (lcs, classes) = lower_models_inner(
        models,
        schema,
        extra_class_infos,
        &Default::default(),
        &Default::default(),
        Materialization::Emit,
        FinderInputs::Scalar,
    );
    (lcs, classes)
}

/// Variant that also takes (resource → permitted fields) tuples
/// collected from controllers. When a model's resource (e.g. `:article`
/// for `Article`) is in `params_specs`, the model gets a typed
/// `from_params(p: <Resource>Params)` factory whose body assigns each
/// permitted field through the column setter. Models without a
/// matching spec skip the factory (no controller permits them).
pub fn lower_models_with_registry_and_params(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    lower_models_inner(models, schema, extra_class_infos, params_specs, &Default::default(), Materialization::Emit, FinderInputs::Scalar)
}

/// Lower models with scalar finder inputs when the caller needs classes only;
/// Ruby-family request consumers select their separate normalization entry.
pub fn lower_models_to_library_classes(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
) -> Vec<LibraryClass> {
    lower_models_inner(
        models,
        schema,
        extra_class_infos,
        &Default::default(),
        &Default::default(),
        Materialization::Emit,
        FinderInputs::Scalar,
    )
    .0
}

/// Add controller-permitted factories while retaining the shared scalar
/// finder contract and returning only the materialized classes.
pub fn lower_models_to_library_classes_with_params(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
) -> Vec<LibraryClass> {
    lower_models_inner(models, schema, extra_class_infos, params_specs, &Default::default(), Materialization::Emit, FinderInputs::Scalar).0
}

/// As above, plus the class methods whose bodies must NOT be arel-folded
/// — the query-shaped half of `scope_chain::AssocClassMethods`, which a
/// Ruby-family emit is about to re-root on a threaded relation
/// (`scope_chain::assoc_query_method_names` names them). Folding
/// `Message.count` there to an inline whole-table `SELECT COUNT(*)`
/// would bake in the one reading the method must not have.
///
/// Ruby-family only. Every other target passes the empty set through
/// the entries above and keeps the fold, which is right for an emit
/// with no relation to thread.
pub fn lower_models_to_library_classes_unfolding(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
    unfolded: &crate::det::HashSet<(ClassId, Symbol)>,
) -> Vec<LibraryClass> {
    lower_models_inner(models, schema, extra_class_infos, params_specs, unfolded, Materialization::Emit, FinderInputs::Request).0
}

/// Register raw-input finders only for Ruby-family controller/test consumers.
pub(crate) fn lower_models_with_request_finders(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    lower_models_inner(
        models, schema, extra_class_infos, &Default::default(), &Default::default(),
        Materialization::Emit, FinderInputs::Request,
    )
}

/// Build one registry for the complete model graph, then type the selected
/// bodies with the caller's finder-input contract. Materialization limits
/// retained bodies without removing definitions needed by cross-model calls.
pub(crate) fn lower_models_inner(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
    unfolded: &crate::det::HashSet<(ClassId, Symbol)>,
    materialization: Materialization<'_>,
    finder_inputs: FinderInputs,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    lower_models_inner_with_ruby_values(
        models, schema, extra_class_infos, params_specs, unfolded, materialization,
        finder_inputs, false, ErrorsReaders::FieldRead,
    )
}

pub(crate) fn lower_models_inner_with_ruby_values(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
    unfolded: &crate::det::HashSet<(ClassId, Symbol)>,
    materialization: Materialization<'_>,
    finder_inputs: FinderInputs,
    ruby_read_values: bool,
    errors_readers: ErrorsReaders,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    lower_models_inner_for_dialect(models, schema, extra_class_infos, params_specs,
        unfolded, materialization, finder_inputs, ruby_read_values, errors_readers,
        crate::lower::arel::SqlDialect::Sqlite)
}

/// Database-aware model lowering. Existing target entry points retain SQLite
/// until their runtime adapter supports the requested database.
pub(crate) fn lower_models_inner_for_dialect(
    models: &[Model],
    schema: &Schema,
    extra_class_infos: Vec<(ClassId, crate::analyze::ClassInfo)>,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
    unfolded: &crate::det::HashSet<(ClassId, Symbol)>,
    materialization: Materialization<'_>,
    finder_inputs: FinderInputs,
    ruby_read_values: bool,
    errors_readers: ErrorsReaders,
    dialect: crate::lower::arel::SqlDialect,
) -> (Vec<LibraryClass>, HashMap<ClassId, crate::analyze::ClassInfo>) {
    let mut all_methods: Vec<(Vec<MethodDef>, ClassId, Option<&Table>, &Model)> = Vec::new();
    let mut classes: HashMap<ClassId, crate::analyze::ClassInfo> = HashMap::default();
    let _body_names = schema::share_model_body_names(models, &Default::default());
    for model in models {
        let methods = build_methods_for_dialect(
            model, models, schema, params_specs, finder_inputs, errors_readers, dialect,
        );
        let table = schema.tables.get(&model.table.0);
        // Register actual production definitions, even for unselected
        // models and when source overrides hide framework ownership.
        classes.insert(model.name.clone(), build_class_info_with_finder_inputs(model, &methods, table, finder_inputs));
        if !materialization.retains(&model.name) {
            continue;
        }
        let methods = match materialization {
            Materialization::Emit => methods,
            Materialization::AccessorProbe(_) => {
                let mut definitions = model.clone();
                definitions.body.retain(|item| match item {
                    crate::dialect::ModelBodyItem::Method { method, .. } => method.name_span.is_synthetic(),
                    crate::dialect::ModelBodyItem::Unknown { expr, .. } => !matches!(&*expr.node,
                        ExprNode::Send { recv: None, method, .. }
                            if matches!(method.as_str(), "attr_accessor" | "attr_reader" | "attr_writer")),
                    _ => true,
                });
                let mut methods = build_methods_for_dialect(
                    &definitions, models, schema, params_specs, finder_inputs, errors_readers, dialect,
                );
                // Preserve original source inputs for late derivations
                // (e.g. raw helpers) without treating them as framework
                // claims. Both kinds traverse the canonical Arel/typer.
                methods.extend(model.methods().filter(|m| !m.name_span.is_synthetic()).cloned().map(|mut method| {
                    // Match build_methods' provenance backfill even when a source
                    // method was retained beside the generated probe surface.
                    method.body.inherit_span(model.span);
                    method
                }));
                methods
            }
        };
        all_methods.push((methods, model.name.clone(), table, model));
    }

    // Register framework runtime stubs (Sqlite primitive surface, etc.)
    // so model bodies that call into them — `Sqlite.prepare/step?/...` in
    // the lowerer-emitted `_adapter_*` primitives — type cleanly.
    crate::lower::view_to_library::insert_framework_stubs(&mut classes);
    if dialect == crate::lower::arel::SqlDialect::Postgres {
        classes.get_mut(&ClassId(Symbol::from("Db"))).expect("Db primitive registry")
            .class_methods.insert(Symbol::from("insert_returning_int"),
                fn_sig(vec![(Symbol::from("sql"), Ty::Str)], Ty::Int));
    }
    if finder_inputs == FinderInputs::Request {
        insert_integer_key_cast_info(&mut classes);
    }
    // Register synthesized Row classes so dispatch on `Article.from_row(r)`
    // / `ArticleRow.from_raw(h)` resolves through the body-typer.
    // Stream unselected rows: their registry metadata is needed, but
    // retaining every row's bodies would defeat bounded observation.
    let mut row_classes = Vec::new();
    for model in models {
        for row_lc in self::row::synthesize_row_classes(std::slice::from_ref(model), schema) {
            classes.insert(row_lc.name.clone(), self::row::row_class_info(&row_lc));
            if materialization.retains(&model.name) {
                row_classes.push(row_lc);
            }
        }
    }
    // Register synthesized Params classes (info-only — the actual class
    // is emitted by the controller lowerer). Needed so the model's
    // `from_params` body's `p.<field>` Send dispatches to the
    // `<Resource>Params` attr_reader signature.
    for spec in params_specs.iter() {
        let class_id = spec.class_id.clone();
        let mut info = crate::analyze::ClassInfo::default();
        // MUST match what `build_params_class` actually synthesizes. A
        // registry that disagrees is worse than none: the coercion pass
        // reads these signatures to decide whether a value needs
        // wrapping for a nullable column, so claiming `String | nil`
        // where the class returns `String` silently drops the `Some(…)`
        // rust needs.
        let mut register = |name: Symbol, ty: Ty| {
            info.instance_methods.insert(name.clone(), fn_sig(vec![], ty.clone()));
            info.instance_method_kinds
                .insert(name.clone(), crate::dialect::AccessorKind::AttributeReader);
            let setter_name = Symbol::from(format!("{}=", name.as_str()));
            info.instance_methods.insert(
                setter_name.clone(),
                fn_sig(vec![(Symbol::from("value"), ty.clone())], ty),
            );
            info.instance_method_kinds
                .insert(setter_name, crate::dialect::AccessorKind::AttributeWriter);
        };
        for field in &spec.fields {
            register(field.clone(), Ty::Str);
            register(
                crate::lower::controller_to_library::params::provided_field(field),
                Ty::Bool,
            );
        }
        info.class_methods.insert(
            Symbol::from("from_raw"),
            fn_sig(
                vec![(
                    Symbol::from("params"),
                    Ty::Hash {
                        key: std::sync::Arc::new(Ty::Sym),
                        value: std::sync::Arc::new(Ty::Untyped),
                    },
                )],
                Ty::Class { id: class_id.clone(), args: vec![].into() },
            ),
        );
        info.class_method_kinds
            .insert(Symbol::from("from_raw"), crate::dialect::AccessorKind::Method);
        classes.insert(class_id, info);
    }
    // Framework runtime stubs — referenced from broadcasts_to expansions
    // but not part of any model. Mirrors runtime/ruby/broadcasts.rb's
    // public surface (each takes a kwargs hash and returns Nil).
    classes.insert(ClassId(Symbol::from("Broadcasts")), broadcasts_class_info());
    // Caller-supplied entries (typically the lowered view modules,
    // registered under both their full ClassId and a last-segment
    // alias for the typer's last-segment Const lookup).
    for (id, info) in extra_class_infos {
        classes.insert(id, info);
    }

    let mut out = Vec::new();
    for (mut methods, _, table, model) in all_methods {
        for method in &mut methods {
            // Arel pass — rewrite recognized AR call sites
            // (`Comment.where(article_id: @id)`, `Article.find_by(id:
            // @id)`, `Comment.all`, `Comment.count`, `Comment.exists?`)
            // into inline SELECT/hydrate expansions over the `Db.*`
            // primitive surface. Sends that don't match a static
            // pattern are left as-is for the body-typer + emitter to
            // handle (Phase 2 will route them to a runtime Arel
            // module instead). See project_arel_compile_time_first.md.
            //
            // Held back for a class method whose body is about to be
            // re-rooted on a threaded relation: its `Message.count` is
            // an implicit-self read that ingest spelled with the
            // constant, and an inline whole-table count would bake in
            // the reading Rails does not have. Only the Ruby-family
            // entry ever names one.
            let unfold = method.receiver == crate::dialect::MethodReceiver::Class
                && unfolded.contains(&(model.name.clone(), method.name.clone()));
            if !unfold {
                crate::lower::arel::rewrite_arel_in_method_with_ruby_values(
                    &mut method.body, schema, &classes, &[], ruby_read_values, dialect,
                    &HashSet::default(),
                    (method.receiver == crate::dialect::MethodReceiver::Class).then_some(&model.name),
                );
            }
            type_method_body(method, &classes, table, Some(model));
        }
        out.push(model_class(model, models, methods, table));
    }
    // Type-check Row class method bodies too so the strict typing residual
    // check doesn't blow up. The Row class shares its column shape with
    // the model's table (schema columns map 1:1 to attr_accessor pairs),
    // so we look up the corresponding table by stripping the `Row` suffix
    // off the class name. The typer's `seed ivar_bindings from columns`
    // path then resolves `@id` / `@title` / etc. inside attr_reader bodies.
    let mut row_classes = row_classes;
    for row_lc in &mut row_classes {
        let model_name = row_lc.name.0.as_str().trim_end_matches("Row");
        let table = models
            .iter()
            .find(|m| m.name.0.as_str() == model_name)
            .and_then(|m| schema.tables.get(&m.table.0));
        for method in &mut row_lc.methods {
            // Row classes hold scalar schema fields only — no
            // associations, so no cache ivars to seed.
            type_method_body(method, &classes, table, None);
        }
    }
    // Append synthesized Row classes after the model classes. Per-target
    // emit walks `out` linearly and emits one file per LibraryClass; the
    // Row classes get their own files (`app/models/article_row.rb`,
    // `article_row.ts`, etc.).
    out.extend(row_classes);
    (out, classes)
}

/// Build a `ClassInfo` for a lowered LibraryClass — used to feed
/// view modules / runtime-class lowerings into the model lowerer's
/// shared registry. Each `MethodDef.signature` becomes an entry in
/// `class_methods` (for `MethodReceiver::Class`) or `instance_methods`
/// (for `MethodReceiver::Instance`).
pub fn class_info_from_library_class(lc: &LibraryClass) -> crate::analyze::ClassInfo {
    use crate::dialect::AccessorKind;
    use crate::expr::{ExprNode, LValue};

    // Collect ivar names assigned in any instance method body. A
    // method whose name matches one of these ivars (e.g. Validations'
    // `def errors` paired with `@errors = []`) shadows the field
    // declaration in TS emit; the body analyzer's force-parens rule
    // shouldn't add `()` to such calls since the field is read as
    // a property. Reclassify those Method entries as AttributeReader
    // so the typer treats them like field accesses.
    let mut ivar_names: crate::det::HashSet<String> =
        crate::det::HashSet::default();
    fn collect_ivars(e: &crate::expr::Expr, out: &mut crate::det::HashSet<String>) {
        match &*e.node {
            ExprNode::Assign { target: LValue::Ivar { name }, value } => {
                out.insert(name.as_str().to_string());
                collect_ivars(value, out);
            }
            ExprNode::Assign { target, value } => {
                if let LValue::Attr { recv, .. } | LValue::Index { recv, .. } = target {
                    collect_ivars(recv, out);
                }
                collect_ivars(value, out);
            }
            ExprNode::Send { recv, args, block, .. } => {
                if let Some(r) = recv {
                    collect_ivars(r, out);
                }
                for a in args {
                    collect_ivars(a, out);
                }
                if let Some(b) = block {
                    collect_ivars(b, out);
                }
            }
            ExprNode::Seq { exprs } => {
                for x in exprs {
                    collect_ivars(x, out);
                }
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                collect_ivars(cond, out);
                collect_ivars(then_branch, out);
                collect_ivars(else_branch, out);
            }
            ExprNode::Lambda { body, .. } => collect_ivars(body, out),
            _ => {}
        }
    }
    for m in &lc.methods {
        if matches!(m.receiver, MethodReceiver::Instance) {
            collect_ivars(&m.body, &mut ivar_names);
        }
    }

    let mut info = crate::analyze::ClassInfo::default();
    info.parent = lc.parent.clone();
    for m in &lc.methods {
        if let Some(sig) = &m.signature {
            // If a Method-kind instance method's name matches an ivar
            // in the class body, treat as AttributeReader for the
            // typer. The TS emit already drops such methods in favor
            // of the ivar-derived field declaration; the kind change
            // keeps force-parens from firing on call sites.
            let kind = if matches!(m.receiver, MethodReceiver::Instance)
                && matches!(m.kind, AccessorKind::Method)
                && ivar_names.contains(m.name.as_str())
            {
                AccessorKind::AttributeReader
            } else {
                m.kind
            };
            match m.receiver {
                MethodReceiver::Instance => {
                    info.instance_methods.insert(m.name.clone(), sig.clone());
                    info.instance_method_kinds.insert(m.name.clone(), kind);
                }
                MethodReceiver::Class => {
                    info.class_methods.insert(m.name.clone(), sig.clone());
                    info.class_method_kinds.insert(m.name.clone(), kind);
                }
            }
        }
    }
    info
}

/// Single-model entry point: lower one `Model` (Rails-shape, with DSL
/// items in `body`) into a post-lowering `LibraryClass`. Builds a
/// class registry containing only this model — for whole-app emit
/// where cross-model dispatch needs typing, prefer
/// `lower_models_to_library_classes`.
///
/// `schema` supplies the column list for the model's table — needed for
/// the per-column accessors / `attributes` / `[]` / `[]=` / `update` /
/// `initialize` lowerings. Models whose table isn't in the schema (rare;
/// abstract or virtual) get only the non-schema-driven methods.
pub fn lower_model_to_library_class(model: &Model, schema: &Schema) -> LibraryClass {
    let mut methods =
        build_methods(model, std::slice::from_ref(model), schema, &Default::default());
    let table = schema.tables.get(&model.table.0);
    let class_info = build_class_info(model, &methods, table);
    let mut classes: HashMap<ClassId, crate::analyze::ClassInfo> = HashMap::default();
    classes.insert(model.name.clone(), class_info);
    insert_integer_key_cast_info(&mut classes);
    // Register Row classes so `<Model>.from_row(r)` / `<Model>Row.from_raw(h)`
    // calls inside the model body type correctly. The synthesized Row
    // class itself is not returned by this entry point (single-class
    // shape) — callers that need both should use the bulk entry point.
    let row_lcs = self::row::synthesize_row_classes(std::slice::from_ref(model), schema);
    for row_lc in &row_lcs {
        classes.insert(row_lc.name.clone(), self::row::row_class_info(row_lc));
    }
    for method in &mut methods {
        type_method_body(method, &classes, table, Some(model));
    }
    model_class(model, std::slice::from_ref(model), methods, table)
}

/// Canonical class envelope for both production lowering and ownership
/// observation. Typing and method selection remain the caller's job.
fn model_class(model: &Model, models: &[Model], methods: Vec<MethodDef>, table: Option<&Table>) -> LibraryClass {
    // ActiveRecord::Base includes ActiveSupport::Callbacks; the runtime
    // base does not, so a model that runs the module's class side
    // (`callback_dsl_calls`) includes the port itself, ahead of its own
    // mixins as the base's include is, and keeps those calls in body
    // order beside its prepends. A gem port's class macro calls
    // (`gem_port_macro_calls`) are class-body statements.
    let callbacks = callback_dsl_calls(model, models);
    let port_macros = gem_port_macro_calls(model, models);
    let mut includes = crate::analyze::model_includes(model);
    let module = ClassId(Symbol::from("ActiveSupport::Callbacks"));
    if !callbacks.is_empty() && !includes.contains(&module) {
        includes.insert(0, module);
    }
    let unknown_calls = model
        .body
        .iter()
        .filter_map(|item| match item {
            crate::dialect::ModelBodyItem::Unknown { expr, .. } => Some(expr),
            _ => None,
        })
        .filter(|expr| is_constant_prepend(expr) || callbacks.iter().any(|c| std::ptr::eq(*c, *expr)))
        .cloned()
        .collect();
    LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: model.name.clone(),
        is_module: false,
        parent: model.parent.clone(),
        parent_span: Default::default(),
        // Mixins and constants must survive in every model projection.
        includes,
        methods,
        nullable_columns: nullable_column_names(table),
        origin: None,
        constants: collect_model_constants(model),
        unknown_calls,
        class_ivar_initializers: collect_class_attr_initializers(model, &port_macros),
        visibility_overrides: model.visibility_overrides.clone(),
    }
}

/// A class body's `prepend M, ...` statements naming constants go to the
/// lowered class's `unknown_calls`: the Ruby family replays one whose
/// modules the app defines (`emit::ruby::library::app_prepend_calls`)
/// and reports the rest; a target outside it refuses each
/// (`project::report_class_body_prepends`). Dropped here, the module's
/// wrappers silently never ran around the class's methods.
///
/// `prepend A, B` (bare or `self.`) whose every argument is a constant.
pub(crate) fn is_constant_prepend(expr: &Expr) -> bool {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return false };
    method.as_str() == "prepend"
        && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        && !args.is_empty()
        && args.iter().all(|a| matches!(&*a.node, ExprNode::Const { .. }))
}

/// `Model::class_attr_defaults` → class ivar writes emitted once on the
/// model class object, then the body's `ModelBodyItem::ClassBody`
/// statements in source order (class_attribute and `cattr_*`/`mattr_*`
/// seeds, class-side writes, carried `included do` statements), with
/// the gem port's class macro calls (`gem_port_macro_calls`) among them
/// where the body has them.
///
/// Active Record answers `table_name`, `primary_key`, `ignored_columns`
/// and the rest of the model's schema from its first class-body
/// statement on; the lowered model answers them from the class methods
/// it synthesizes from the compiled table (`table_name`, `_table_sql`,
/// `_compiled_ignored_columns`, ...), and the runtime base's
/// `ignored_columns=` and `query_constraints` check against those. So
/// each statement here that calls a class method (a macro, a
/// class-side write through a writer: the Ruby family writes these, and
/// every class-body statement after the first of them, below the
/// class's methods) is a call of that method in the IR:
///
/// - `self.ignored_columns += [..]` is Ruby's `self.ignored_columns=(
///   self.ignored_columns + [..])`, the same call `self.ignored_columns
///   = [..]` is ([`class_side_write_as_call`]). Core's 271 models with
///   the `+=` form wrote it above `def self._compiled_ignored_columns`,
///   and the write raised at class load ("the compiled table leaves out
///   only []").
/// - Core's 235 `database_documentation do |dd| ... end` port macro calls
///   read `table_name`. Kept beside the model's mixins they were written
///   above an eager constant, and so above `def self.table_name`.
fn collect_class_attr_initializers(model: &Model, port_macros: &[&Expr]) -> Vec<Expr> {
    model
        .class_attr_defaults
        .iter()
        .map(|(name, value)| {
            Expr::new(
                Span::synthetic(),
                ExprNode::Assign {
                    target: crate::expr::LValue::Ivar { name: name.clone() },
                    value: value.clone(),
                },
            )
        })
        .chain(model.body.iter().filter_map(|item| match item {
            crate::dialect::ModelBodyItem::ClassBody { expr, .. } => Some(class_side_write_as_call(expr)),
            crate::dialect::ModelBodyItem::Unknown { expr, .. } if port_macros.iter().any(|c| std::ptr::eq(*c, expr)) => {
                Some(expr.clone())
            }
            _ => None,
        }))
        .collect()
}

/// `self.x op= v` for an arithmetic or bitwise `op` (`+=`, `-=`, `|=`,
/// ...) as the writer call Ruby makes of it, `self.x=(self.x op v)`:
/// the shape `self.x = v` already has. Anything else as it is.
/// (`||=` and `&&=` call the writer only on one branch.)
pub(crate) fn class_side_write_as_call(expr: &Expr) -> Expr {
    use crate::expr::{LValue, OpAssignOp};
    let ExprNode::OpAssign { target: LValue::Attr { recv, name }, op, value } = &*expr.node else {
        return expr.clone();
    };
    if !matches!(&*recv.node, ExprNode::SelfRef) || matches!(op, OpAssignOp::OrOr | OpAssignOp::AndAnd) {
        return expr.clone();
    }
    let send = |recv: Option<Expr>, method: Symbol, args: Vec<Expr>| {
        Expr::new(expr.span, ExprNode::Send { recv, method, args, block: None, parenthesized: false })
    };
    let read = send(Some(recv.clone()), name.clone(), Vec::new());
    let operator = op.as_ruby().trim_end_matches('=');
    let combined = send(Some(read), Symbol::from(operator), vec![value.clone()]);
    send(Some(recv.clone()), Symbol::from(format!("{}=", name.as_str())), vec![combined])
}

/// Class-level `NAME = <expr>` constants declared in a model body (e.g.
/// `User::NEW_USER_DAYS = 70`), so references resolve at emit. Mirrors the
/// controller path's `collect_class_constants`. They reach the body as
/// `ModelBodyItem::Unknown` Const-assigns (the DSL classifier doesn't claim
/// them); single-segment paths only — qualified writes are something else.
pub(crate) fn collect_model_constants(model: &Model) -> Vec<(Symbol, Expr)> {
    let mut out = Vec::new();
    for item in &model.body {
        let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
        if let ExprNode::Assign {
            target: crate::expr::LValue::Const { path },
            value,
        } = &*expr.node
        {
            if let [name] = path.as_slice() {
                out.push((name.clone(), value.clone()));
            }
        }
    }
    out
}

/// A short spelling of a receiver for a diagnostic: `Registry`,
/// `relation_delegate_class(…)`, `<expression>`.
pub(crate) fn receiver_label(recv: &Expr) -> String {
    match &*recv.node {
        ExprNode::Const { path } => path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::"),
        ExprNode::Send { recv: None, method, args, .. } if args.is_empty() => method.as_str().to_string(),
        ExprNode::Send { recv: None, method, .. } => format!("{}(…)", method.as_str()),
        ExprNode::Send { recv: Some(inner), method, .. } => format!("{}.{}", receiver_label(inner), method.as_str()),
        _ => "<expression>".to_string(),
    }
}

/// The name a dropped class-body statement is reported under: the
/// method it calls (`include`, `Registry.register`), or the writer an
/// attribute assignment calls (`x=`, `Registry.count=`; a `Foo.x ||= v`
/// ingests as `Foo.x || Foo.x = v`, named by its write).
pub(crate) fn dropped_statement_name(call: &Expr) -> String {
    use crate::expr::LValue;
    match &*call.node {
        ExprNode::Send { recv, method, .. } => match recv.as_ref() {
            None => method.as_str().to_string(),
            Some(r) if matches!(&*r.node, ExprNode::SelfRef) => method.as_str().to_string(),
            Some(r) => format!("{}.{}", receiver_label(r), method.as_str()),
        },
        ExprNode::Assign { target: LValue::Attr { recv, name }, .. }
        | ExprNode::OpAssign { target: LValue::Attr { recv, name }, .. } => {
            let written = format!("{}=", name.as_str().trim_end_matches('='));
            if matches!(&*recv.node, ExprNode::SelfRef) {
                written
            } else {
                format!("{}.{written}", receiver_label(recv))
            }
        }
        ExprNode::BoolOp { right, .. } if matches!(&*right.node, ExprNode::Assign { .. }) => {
            dropped_statement_name(right)
        }
        other => format!("<{}>", other.kind_str()),
    }
}

/// Every model's [`unclaimed_unknowns`], for `analyze::diagnose`: `check`
/// and the emit gate report the same refusals.
pub fn diagnose_unclaimed_model_dsl(app: &crate::App) -> Vec<crate::diagnostic::Diagnostic> {
    let mut all: Vec<_> =
        app.models.iter().flat_map(|model| unclaimed_unknowns(model, app)).collect();
    all.extend(unclaimed_sti_subclass_calls(app));
    all.extend(app.models.iter().filter_map(|model| user_validate_conflict(model, &app.models)));
    all.extend(private_constant::scoped_reads(app));
    // A concern's statement is spliced into every includer: one row per
    // statement, naming the first includer and counting the rest.
    let mut first: std::collections::BTreeMap<(u32, u32, u32), (usize, usize)> = Default::default();
    let mut out: Vec<crate::diagnostic::Diagnostic> = Vec::new();
    for d in all {
        let key = (d.span.file.0, d.span.start, d.span.end);
        match first.get_mut(&key) {
            Some((_, others)) if !d.span.is_synthetic() => *others += 1,
            _ => {
                first.insert(key, (out.len(), 0));
                out.push(d);
            }
        }
    }
    for (index, others) in first.into_values() {
        if others > 0 {
            let more = if others == 1 { "1 more includer".to_string() } else { format!("{others} more includers") };
            out[index].message = out[index].message.replacen("` not lowered;", &format!("` (and {more}) not lowered;"), 1);
        }
    }
    out
}

/// A model's own instance `def validate` that the lowered validation
/// path would drop or call.
///
/// ActiveModel's instance `validate` is an alias of `valid?`, so in Rails
/// a model's `def validate` replaces the alias and is a plain method:
/// `valid?` and `save` run the `validates` / `validate :m` rules and never
/// call it (pinned natively in tests/user_validate_method.rs). The
/// lowered runtime runs the rules through a method of that name (`valid?`
/// calls `validate`, in runtime/ruby/active_record/connection.rb, base.rb
/// and the ActiveModel surface `validations.rs` synthesizes). So a model
/// with rules lost its method to the synthesized one (`push_user_methods`
/// keeps the first), and a model under a `valid?` that calls `validate`
/// had it run on every `valid?` and `save`. Refused at the `def` until
/// `valid?` runs the rules through a hook of its own. A class outside any
/// hierarchy that declares no rules keeps its method: nothing synthesized
/// calls it.
fn user_validate_conflict(model: &Model, models: &[Model]) -> Option<crate::diagnostic::Diagnostic> {
    let method = model.body.iter().find_map(|item| match item {
        crate::dialect::ModelBodyItem::Method { method, .. }
            if method.name.as_str() == "validate"
                && method.receiver == MethodReceiver::Instance
                && matches!(method.kind, crate::dialect::AccessorKind::Method) =>
        {
            Some(method)
        }
        _ => None,
    })?;
    // The rules the lowering would put under `validate`.
    let mut synthesized = Vec::new();
    push_validate_method(&mut synthesized, model, models, ErrorsReaders::FieldRead, false);
    let has_rules = synthesized.iter().any(|m| m.name.as_str() == "validate");
    // A parent whose `valid?` calls `validate`: ActiveRecord's, or an app
    // model's (the AR hierarchy, or a tableless class's synthesized one).
    let under_valid = model.parent.as_ref().is_some_and(|parent| {
        parent.0.as_str() == "ActiveRecord::Base" || models.iter().any(|m| &m.name == parent)
    });
    if !has_rules && !under_valid {
        return None;
    }
    let why = if has_rules {
        "the lowered `valid?` runs its rules through a method named `validate`, which would replace this one"
    } else {
        "the inherited lowered `valid?` calls `validate` to run the rules, so it would run this method on every `valid?` and `save`"
    };
    let span = if method.name_span.is_synthetic() { model.span } else { method.name_span };
    Some(crate::diagnostic::Diagnostic::unsupported(
        span,
        None,
        "`def validate`",
        format!(
            "on `{}`: in Rails it replaces ActiveModel's `validate` alias of `valid?` and is a plain method that validation never calls; {why}",
            model.name.0.as_str()
        ),
    ))
}

/// The model-body statements no lowering pass claims, each an
/// `unsupported` error at its declaration.
///
/// `ModelBodyItem::Unknown` is a holding pen, not a verdict: broadcasts
/// (`broadcasts_to`), markers (`primary_abstract_class`), block-form
/// callbacks, and class-scope constants are all fished out of it by
/// later recognizers. Whatever nobody claims never reaches the lowered
/// output: `has_one_attached :audio` vanished and the user's first signal
/// was a dispatch failure at some call site three files away, and Core's
/// `define_callbacks :revocation` vanished with `check` clean, so the
/// emitted `run_callbacks` ran no chain. Each such drop removes behavior
/// Rails runs at class load, so it is an error (a coverage gap), not a
/// warning. What carries no runtime behavior is skipped by name (sorbet's
/// annotations, visibility markers, `include` of a spliced concern). The
/// skip-list below must stay in sync with the claiming passes — each
/// entry names the pass that consumes the shape.
pub(crate) fn unclaimed_unknowns(model: &Model, app: &crate::App) -> Vec<crate::diagnostic::Diagnostic> {
    use crate::diagnostic::{Diagnostic, Severity};
    use crate::expr::LValue;

    let (models, schema) = (&app.models, &app.schema);
    let callbacks = callback_dsl_calls(model, models);
    let port_macros = gem_port_macro_calls(model, models);
    let macros = macro_replay::model_macro_verdicts(model, app);
    let mut out = Vec::new();
    for item in &model.body {
        let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
        // A statement another path decides on: the model's database
        // (`connects_to`, `establish_connection`: ingest::database_ownership,
        // which replays it at boot or refuses it as a database refusal), a
        // reproducible `normalizes` (lower::normalizes), a call ingest
        // lowered and claimed on the model.
        if claimed_in_model(model, expr) {
            continue;
        }
        // A call of an app class macro: replayed in the class body
        // (`replay_model_macros`), or refused with the reason.
        match macros.iter().find(|(m, _)| std::ptr::eq(*m, expr)) {
            // `belongs_to_required_by_default` settings are applied to the
            // `belongs_to` declarations they decide.
            Some((_, macro_replay::MacroVerdict::Replay | macro_replay::MacroVerdict::BelongsToRequiredByDefault(_))) => continue,
            Some((_, macro_replay::MacroVerdict::Refuse(reason))) => {
                // The association the call declares
                // (`ingest::macro_associations`) is compiled even so.
                let declared = model.body.iter().find_map(|i| match i {
                    crate::dialect::ModelBodyItem::Association { assoc, span, .. } if *span == expr.span => Some(assoc),
                    _ => None,
                });
                let declared = declared.map_or_else(String::new, |a| {
                    format!(
                        "; its `{}` association is compiled from the call, and the rest of the macro does not run",
                        a.name().as_str()
                    )
                });
                out.push(Diagnostic::unsupported(
                    expr.span,
                    None,
                    dropped_statement_name(expr).as_str(),
                    format!("model macro call on `{}` not lowered; {reason}{declared}", model.name.0.as_str()),
                ));
                continue;
            }
            None => {}
        }
        // ActiveSupport::Callbacks' class side (`define_callbacks`,
        // `set_callback`, a macro that calls it) — carried into the
        // lowered class by `model_class` and replayed through the
        // runtime port (`emit::ruby::library::model_callback_calls`).
        if callbacks.iter().any(|c| std::ptr::eq(*c, expr)) {
            continue;
        }
        // `private_constant :NAME` of a constant the body defines: not
        // run (the lowered program reads constants through the class);
        // each scoped read Ruby would refuse is an error instead
        // (`private_constant::scoped_reads`). One naming a constant the
        // body lacks raises NameError at load.
        if let Some(names) = private_constant::marked_names(expr) {
            let missing = private_constant::undefined(model, app, expr, &names);
            if missing.is_empty() {
                continue;
            }
            out.push(Diagnostic::unsupported(
                expr.span,
                None,
                "private_constant",
                format!(
                    "on `{}`: {} not defined in the class body, so Ruby raises NameError when the class loads",
                    model.name.0.as_str(),
                    missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                ),
            ));
            continue;
        }
        // A class macro a gem runtime port's mixin gives the model
        // (`database_documentation do ... end`): carried by `model_class`
        // and replayed through the port.
        if port_macros.iter().any(|c| std::ptr::eq(*c, expr)) {
            continue;
        }
        // A `delegate` ingest left because its names are called with
        // arguments: the Ruby emit writes its `(...)` forwarders
        // (`ingest::delegate`, `App::ruby_declined_delegate_forwarders`).
        if declined_delegate_forwarded(model, expr, app) {
            continue;
        }
        // Class-scope constants — claimed by analyze::extract_const_assignments.
        if matches!(&*expr.node, ExprNode::Assign { target: LValue::Const { .. }, .. }) {
            continue;
        }
        // `self.x += v`, `Registry.count ||= 0`, `Registry.x = v`: a
        // class-body attribute write no lowering claimed (a
        // `class_attribute` one is a `ClassBody` item).
        let attr_write = match &*expr.node {
            ExprNode::OpAssign { target: LValue::Attr { .. }, .. } => true,
            ExprNode::Assign { target: LValue::Attr { recv, .. }, .. } => !matches!(&*recv.node, ExprNode::SelfRef),
            ExprNode::BoolOp { right, .. } => matches!(&*right.node, ExprNode::Assign { target: LValue::Attr { .. }, .. }),
            _ => false,
        };
        if attr_write {
            let written = dropped_statement_name(expr);
            let d = Diagnostic::unsupported(
                expr.span,
                None,
                written.as_str(),
                format!("model class-body write on `{}` not lowered; it is dropped", model.name.0.as_str()),
            );
            out.push(d);
            continue;
        }
        let ExprNode::Send { recv, method, args, .. } = &*expr.node else {
            continue;
        };
        let name = method.as_str();
        if let Some(recv) = recv {
            // A call on another object (`relation_delegate_class(
            // ActiveRecord::Relation).include(M)`, `Registry.add(self)`):
            // the model's emitted body replays none of these, so each is
            // named where it is dropped.
            if !matches!(&*recv.node, ExprNode::SelfRef) {
                let construct = format!("{}.{name}", receiver_label(recv));
                let detail = if receiver_label(recv) == "relation_delegate_class(…)" {
                    format!(
                        "`relation_delegate_class` on `{}` not lowered; the runtime has no per-model relation \
                         class to extend, so the statement is dropped",
                        model.name.0.as_str()
                    )
                } else {
                    format!("model class-body call on `{}` not lowered; it is dropped", model.name.0.as_str())
                };
                let d = Diagnostic::unsupported(expr.span, None, construct.as_str(), detail);
                out.push(d);
                continue;
            }
            // Class-body writes are sends too, and dropping an unclaimed
            // setting must report just like dropping a receiver-less DSL.
            // A `self.` call of anything else reports as the receiverless
            // call it is.
            // Literal table settings are consumed by ingest::model's
            // explicit_class_setting; dynamic values remain unsupported.
            if matches!(name, "table_name=" | "table_name_prefix=")
                && args.len() == 1
                && matches!(&*args[0].node, ExprNode::Lit { value: Literal::Str { .. } | Literal::Sym { .. } })
            {
                continue;
            }
        }
        // `self.abstract_class = true|false` — read by
        // markers::is_abstract_class for synthesis and replayed into the
        // Ruby-family class body by emit::ruby::attribute_api.
        if self::markers::literal_abstract_class_write(expr).is_some() {
            continue;
        }
        // A bare visibility keyword is a marker for the `def`s after
        // it (the method walk reads it as one); it is not a DSL call.
        if matches!(name, "private" | "protected" | "public") {
            continue;
        }
        // `attr_accessor`/`attr_reader`/`attr_writer` — claimed by
        // markers::push_attribute_api_methods, which synthesizes the
        // virtual attributes the permit filter counts as writers.
        if matches!(name, "attr_accessor" | "attr_reader" | "attr_writer") {
            continue;
        }
        // `has_one_attached` / `has_many_attached :name` (with or
        // without a variants block) — claimed by lower::attached on
        // exactly that shape. Campfire's three (`avatar`, `attachment`,
        // `logo`) were reported as not lowered on every emit while the
        // attachment lowering ran on each.
        if name == "has_one_attached" || name == "has_many_attached" {
            if let ExprNode::Send { args, .. } = &*expr.node {
                if !args.is_empty() {
                    continue;
                }
            }
        }
        // `broadcasts_to` — claimed by lower::broadcasts.
        if name == "broadcasts_to" {
            continue;
        }
        // `typed_store` — claimed by lower::typed_store's shared
        // method synthesis (all targets).
        if name == "typed_store" {
            continue;
        }
        // `has_secure_password` — claimed by lower::secure_password's
        // shared method synthesis (all targets).
        if name == "has_secure_password" {
            continue;
        }
        // `has_secure_token` — claimed by lower::secure_token, which
        // expands the bare/`length:`/`on:` forms it can honor. Asked
        // by span rather than re-derived, same as has_json/
        // has_rich_text below: an option it declines to expand keeps
        // warning.
        if name == "has_secure_token"
            && crate::lower::secure_token::secure_token_decls(&model.body)
                .iter()
                .any(|d| d.span == expr.span)
        {
            continue;
        }
        // `extend FriendlyId` / `friendly_id :col, use: [...]` — claimed
        // by lower::friendly_id on the configurations it honors. The
        // slug-generation gap that remains is reported once, below the
        // claim, as its own warning.
        if (name == "friendly_id" || name == "extend")
            && crate::lower::friendly_id::claims(model, expr.span)
        {
            if name == "friendly_id" {
                if let Some((span, message)) = crate::lower::friendly_id::write_gap(model) {
                    let kind = crate::diagnostic::DiagnosticKind::LowerResidue {
                        pass: crate::ident::Symbol::from("friendly_id"),
                        construct: crate::ident::Symbol::from("friendly_id"),
                        reason: crate::ident::Symbol::from("slug generation on save"),
                    };
                    crate::emit::diagnostics::push(Diagnostic {
                        span,
                        severity: Severity::Warning,
                        kind,
                        message,
                    });
                }
            }
            continue;
        }
        // `generates_token_for :purpose, expires_in: D do … end` —
        // claimed by lower::generates_token_for for the forms it
        // expands; asked by span for the same reason.
        if name == "generates_token_for" && crate::lower::generates_token_for::claims(model, expr.span) {
            continue;
        }
        // `has_json :col, key: <literal>, …` — claimed by
        // lower::has_json's shared method synthesis. A declaration
        // carrying a schema entry that pass cannot expand (a
        // symbol-declared type, whose Rails default is nil) is NOT
        // claimed and keeps warning: half an expansion is worse than
        // none. `has_json_decls` is the one place that decides which
        // declarations are claimed; this test asks it rather than
        // re-deriving the shape (same dance as `has_rich_text`).
        if name == "has_json"
            && crate::lower::has_json::has_json_decls(&model.body)
                .iter()
                .any(|d| d.span == expr.span)
        {
            continue;
        }
        // `serialize :col, coder: JSON` (and legacy positional `JSON`) —
        // claimed by lower::serialize; accessors go through RoundhouseJsonColumn.
        if name == "serialize"
            && crate::lower::serialize::serialize_decls(model)
                .iter()
                .any(|d| d.span == expr.span)
        {
            continue;
        }
        // Bare `has_rich_text :body` — claimed by lower::rich_text.
        // The option-carrying forms (`encrypted:`, `store_if_blank:`)
        // are NOT claimed: each changes the expansion and none is
        // implemented, so they keep warning. `rich_text_attrs` is the
        // one place that decides which shape is claimed; this test
        // asks it rather than re-deriving the arity.
        if name == "has_rich_text"
            && crate::lower::rich_text::rich_text_attrs(model)
                .iter()
                .any(|(span, _)| *span == expr.span)
        {
            continue;
        }
        // Bare `has_markdown :body` — claimed by lower::plain_text_attr
        // (named plain-text association+storage) only when the
        // `action_text_markdowns` table is present. Option-carrying
        // forms and missing-table apps stay unclaimed.
        if name == "has_markdown"
            && crate::lower::plain_text_attr::record_table_present(schema)
            && crate::lower::plain_text_attr::plain_text_attrs(model)
                .iter()
                .any(|(span, _)| *span == expr.span)
        {
            continue;
        }
        // 2-arg `attribute :name, :type` — claimed by
        // markers::push_attribute_api_methods (typed virtual
        // attributes). Other arities (default:-carrying) stay
        // unclaimed and warn.
        if name == "attribute" {
            if let ExprNode::Send { args, .. } = &*expr.node {
                if args.len() == 2 {
                    continue;
                }
            }
        }
        // `primary_abstract_class` — claimed by markers.rs.
        if name == "primary_abstract_class" {
            continue;
        }
        // `include Notifications` — a model concern, spliced by the
        // analyzer (`model_includes` + `concern_model_items`) into the
        // includer's registry and lowered with its methods; nothing is
        // left for an emitter to lower. Framework marker includes are
        // dropped by `model_includes` for the same reason.
        if name == "include" {
            continue;
        }
        // `prepend Guard` — carried to the lowered class
        // (`model_class`); the emit replays or refuses it.
        if is_constant_prepend(expr) {
            continue;
        }
        // Block-form and lambda-argument lifecycle hooks
        // (`before_create -> { … }`) — both claimed by
        // markers::push_block_callback, on the shape it reads: a block,
        // or a parameterless lambda as the first argument.
        // The transaction callbacks' block forms, on the shapes
        // `transaction_callbacks` lowers.
        if self::transaction_callbacks::is_transaction_hook(name) {
            if self::transaction_callbacks::claims(model, expr) {
                continue;
            }
        } else if self::markers::BLOCK_CALLBACK_HOOKS.contains(&name) {
            // Exactly the shapes `markers::push_block_callback` lowers
            // (`block_callback_shape`); a declined one reports below.
            if self::markers::block_callback_shape(expr).is_some() {
                continue;
            }
            // `after_initialize :set_defaults, if: :new_record?` —
            // folded by markers::push_callback_methods.
            if self::markers::symbol_after_initialize(expr).is_some()
                && !self::markers::defines_after_initialize(model)
            {
                continue;
            }
        }
        // sorbet's pure ANNOTATIONS. The library-class walk drops
        // these at ingest; a model never saw them until an abstract
        // base outside `app/models` started being classified as one,
        // and then every such base reported its `abstract!` as
        // unlowered DSL. They carry types and nothing else, and the
        // emitted tree has no sorbet-runtime to read them.
        if matches!(
            name,
            "abstract!"
                | "interface!"
                | "final!"
                | "sealed!"
                | "sig"
                | "type_parameters"
                | "requires_ancestor"
                | "has_attached_class!"
        ) {
            continue;
        }
        // The mixin spelling of the same thing: `extend T::Helpers`.
        // (`include` is already skipped above, whatever its argument.)
        if name == "extend" {
            if let ExprNode::Send { args, .. } = &*expr.node {
                if let Some(ExprNode::Const { path }) = args.first().map(|a| &*a.node) {
                    let written =
                        path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                    if matches!(written.as_str(), "T::Sig" | "T::Helpers" | "T::Generic") {
                        continue;
                    }
                }
            }
        }
        // An app class macro reaching `attribute` or `query_constraints`
        // whose literal columns the model's compiled methods answer
        // (`analyze::retypable_attributes::claimed_body_calls`).
        if model.claimed_body_calls.contains(&expr.span) {
            continue;
        }
        let d = Diagnostic::unsupported(
            expr.span,
            None,
            name,
            format!("model DSL call on `{}` not lowered; {}", model.name.0.as_str(), unlowered_dsl_reason(name)),
        );
        out.push(d);
    }
    out
}

/// Why the lowered class cannot run a framework DSL call nothing claims,
/// where the call's Rails behavior is known to reach past the class body
/// (tests/model_dsl_refusal_reasons.rs pins it natively); the generic
/// reason otherwise.
fn unlowered_dsl_reason(name: &str) -> &'static str {
    match name {
        // Rails merges the scope into every relation of the class
        // (`all`, `find`, `count`, associations) and seeds new records
        // with its `where` values; `unscoped` drops it. The compiled
        // finders and the runtime relation build their SQL without it,
        // so every read would return rows Rails filters out.
        "default_scope" => {
            "Rails applies a default_scope to every query of the class and to new records' \
             attributes, and the lowered finders and relations do not apply it"
        }
        // Rails defines the attribute methods under the new name
        // (`title`, `title=`, `title_changed?`, ...) and translates the
        // name to the column in `new`, `where`, `order`, `pluck`. The
        // lowered model compiles that for literal names over a column
        // (`analyze::retypable_attributes::alias_attribute_decl`); any
        // other alias (of an association, another alias, a computed
        // name) gets neither the methods nor the key translation.
        "alias_attribute" => {
            "Rails answers the alias's attribute methods and translates the alias to its target in \
             `new`, `where`, `order` and `pluck`, and the lowered model compiles that only for a literal \
             new name over a literal column"
        }
        _ => "the class does not run it",
    }
}

/// The passes other than model lowering that consume a model-body
/// statement, each asked through its own predicate rather than
/// re-derived here. A claimed statement is neither reported as dropped
/// DSL nor replayed or refused as a macro call (`macro_replay`). A pass
/// that lowers a model macro (one file per macro) adds its `claims` here.
pub(crate) const MODEL_STATEMENT_CLAIMS: &[fn(&Expr) -> bool] = &[
    // `connects_to`, `establish_connection`, ...: the database manifest.
    crate::ingest::database_ownership::claims_model_statement,
    // A reproducible `normalizes`.
    crate::lower::normalizes::claims,
    // A reproducible `default_scope` (applied by the runtime relation).
    crate::lower::default_scope::claims,
    // A literal `self.inheritance_column = ...` (the STI column).
    crate::lower::sti_scope::claims_inheritance_column,
];

/// `delegate :a, :b, to: :t` on `model` whose every forwarder the Ruby
/// emit writes from `App::ruby_declined_delegate_forwarders`.
fn declined_delegate_forwarded(model: &Model, expr: &Expr, app: &crate::App) -> bool {
    let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { return false };
    if method.as_str() != "delegate" {
        return false;
    }
    let Some(forwarders) = app.ruby_declined_delegate_forwarders.get(&model.name) else { return false };
    let mut names = args.iter().filter_map(|a| match &*a.node {
        ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => Some(value),
        _ => None,
    });
    let prefix = args.iter().any(|a| matches!(&*a.node, ExprNode::Hash { entries, .. } if entries.iter().any(|(k, _)|
        matches!(&*k.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } } if value.as_str() == "prefix"))));
    !prefix && names.all(|n| forwarders.iter().any(|f| &f.name == n))
}

/// Whether some path in [`MODEL_STATEMENT_CLAIMS`] consumes `expr`.
pub(crate) fn claimed_elsewhere(expr: &Expr) -> bool {
    MODEL_STATEMENT_CLAIMS.iter().any(|claims| claims(expr))
}

/// Whether a pass consumes `model`'s body statement `expr`: the one
/// check both the unclaimed-DSL report and the macro verdicts (hence the
/// replay) consult, so a claimed statement is neither reported, refused
/// nor replayed. Claims recorded on the model itself by ingest (the
/// spans of calls lowered there) belong here too.
pub(crate) fn claimed_in_model(model: &Model, expr: &Expr) -> bool {
    claimed_elsewhere(expr) || crate::lower::default_scope::claims_in(model, expr)
}

/// ActiveSupport::Callbacks' class-side surface as the Ruby-family runtime
/// port declares it (`define_callbacks`, `set_callback`, `skip_callback`,
/// `reset_callbacks`, …).
fn callbacks_port_methods() -> &'static [String] {
    crate::analyze::runtime_reads::mixed_in_class_methods("ActiveSupport::Callbacks").unwrap_or(&[])
}

/// The body calls (bare or `self.`) of a class method that a module a
/// gem runtime port declares gives the model through its `ClassMethods`
/// (`runtime_reads::mixed_in_class_methods`), where the model or an app
/// model ancestor includes that module: Core's 230 `database_documentation
/// do |dd| ... end` under `include
/// DatabaseDocumentation::ModelMetadata::Metadata`. `model_class` carries
/// them into the lowered class as class-body statements in body order
/// (`collect_class_attr_initializers`), which the Ruby family writes
/// below the synthesized `table_name` they read; a tree without the
/// port file refuses the include. The state_machines gem's
/// `state_machine`, which the gem gives every class, is one too
/// (`lower::state_machines`). A class method of the model's own, or an
/// ancestor's, of the same name answers instead, so its call is not one.
pub(crate) fn gem_port_macro_calls<'a>(model: &'a Model, models: &[Model]) -> Vec<&'a Expr> {
    let mut chain: Vec<&Model> = vec![model];
    while let Some(parent) = chain.last().and_then(|m| m.parent.as_ref()) {
        match models.iter().find(|m| &m.name == parent) {
            Some(next) if !chain.iter().any(|seen| seen.name == next.name) => chain.push(next),
            _ => break,
        }
    }
    let app_module = |name: &ClassId| models.iter().any(|m| &m.name == name);
    let provided: Vec<&str> = chain
        .iter()
        .flat_map(|m| crate::analyze::model_includes(m))
        .filter(|module| !app_module(module) && crate::project::gem_port_declaring(module.0.as_str()).is_some())
        .filter_map(|module| crate::analyze::runtime_reads::mixed_in_class_methods(module.0.as_str()))
        .flatten()
        .map(String::as_str)
        // The state_machines gem's macro, which it defines on every class
        // (`lower::state_machines`).
        .chain(crate::lower::state_machines::state_machine_calls(model).next().map(|_| crate::lower::state_machines::MACRO))
        .collect();
    if provided.is_empty() {
        return Vec::new();
    }
    let own_class_method = |name: &str| {
        chain.iter().any(|m| m.methods().any(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == name))
    };
    model
        .body
        .iter()
        .filter_map(|item| match item {
            crate::dialect::ModelBodyItem::Unknown { expr, .. } => Some(expr),
            _ => None,
        })
        .filter(|expr| {
            let ExprNode::Send { recv, method, .. } = &*expr.node else { return false };
            recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                && provided.contains(&method.as_str())
                && !own_class_method(method.as_str())
        })
        .collect()
}

/// One model per class: every other declaration of a model's class (a
/// file that reopens it) is merged into the declaration in the
/// constant's own file, after that one's body, as Ruby runs a reopen
/// when it loads.
///
/// Core writes a nested class's file around its owner restated in full,
/// superclass included: `class Event < GlobalDB::ShopRecord` around
/// `class OrderEvent` in event/order_event.rb (21 such files for Event,
/// 29 for PlaybookTimelineEvent), with at most a repeated
/// `extend(FixMeMyModelHasImplicitAssociationValidation)`. Ingest
/// classifies each such file as a model of that name, so the app held
/// several `Event` models, each lowered to app/models/event.rb:
/// - the emitted file was whichever came last (the tree keeps the last
///   of one path), so Core's Event lost every association and macro of
///   its body to a reopen;
/// - a lookup by name found whichever came first:
///   shipping_line/applied_discount.rb (`class ShippingLine <
///   GlobalDB::ShopRecord` around `class AppliedDiscount`) sorts before
///   shipping_line.rb, so the emitted ShippingLine answered
///   no reflection (`emit::ruby::library::apply_association_reflections`
///   read the reopen's empty body) and its `accelerated_readonly_belongs_to
///   :shop` raised NotImplementedError at class load (also GiftCard and
///   PlaybookTimelineEvent).
///
/// The declaration kept is the one in `<underscore(name)>.rb`, the file
/// Zeitwerk loads for the constant; else the first with a body; else the
/// first. The reopens follow it in ingest order. What a reopen gives the
/// class joins the declaration's: its body items, enums, class-attribute
/// seeds, visibility changes and the facts stamped per model (STI
/// subclasses, subclass declarations, retypable and aliased attributes),
/// and the superclass, keys and query constraints where the declaration
/// names none.
pub(crate) fn merge_model_reopens(app: &mut crate::App) {
    let mut groups: HashMap<ClassId, Vec<usize>> = HashMap::default();
    let mut order: Vec<ClassId> = Vec::new();
    for (i, model) in app.models.iter().enumerate() {
        let members = groups.entry(model.name.clone()).or_default();
        if members.is_empty() {
            order.push(model.name.clone());
        }
        members.push(i);
    }
    if groups.values().all(|members| members.len() < 2) {
        return;
    }
    let path_of = |model: &Model| {
        (model.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)).map(|s| s.path.as_str())
    };
    let mut merges: Vec<(usize, usize)> = Vec::new();
    for name in &order {
        let members = &groups[name];
        if members.len() < 2 {
            continue;
        }
        let own_file = format!("{}.rb", crate::naming::underscore(name.0.as_str()));
        let in_own_file = |i: &usize| {
            path_of(&app.models[*i]).is_some_and(|p| p == own_file || p.ends_with(&format!("/{own_file}")))
        };
        let kept = members
            .iter()
            .copied()
            .find(in_own_file)
            .or_else(|| members.iter().copied().find(|&i| !app.models[i].body.is_empty()))
            .unwrap_or(members[0]);
        merges.extend(members.iter().copied().filter(|&i| i != kept).map(|i| (kept, i)));
    }
    let mut slots: Vec<Option<Model>> = std::mem::take(&mut app.models).into_iter().map(Some).collect();
    for (kept, reopen) in merges {
        let reopen = slots[reopen].take().expect("each reopen merges once");
        absorb_model_reopen(slots[kept].as_mut().expect("the kept declaration stays"), reopen);
    }
    app.models = slots.into_iter().flatten().collect();
}

fn absorb_model_reopen(target: &mut Model, reopen: Model) {
    fn extend_new<T: PartialEq>(into: &mut Vec<T>, from: Vec<T>) {
        for item in from {
            if !into.contains(&item) {
                into.push(item);
            }
        }
    }
    if target.parent.is_none() {
        target.parent = reopen.parent;
        target.parent_span = reopen.parent_span;
    }
    if target.primary_key.is_none() {
        target.primary_key = reopen.primary_key;
    }
    if target.composite_primary_key.is_empty() {
        target.composite_primary_key = reopen.composite_primary_key;
    }
    if target.query_constraints.is_empty() {
        target.query_constraints = reopen.query_constraints;
    }
    if target.ignored_columns.is_empty() {
        target.ignored_columns = reopen.ignored_columns;
    }
    target.body.extend(reopen.body);
    for (name, values) in reopen.enums {
        target.enums.entry(name).or_insert(values);
    }
    for (name, value) in reopen.enum_defaults {
        target.enum_defaults.entry(name).or_insert(value);
    }
    // A later class-attribute write wins, as it runs later.
    target.class_attr_defaults.extend(reopen.class_attr_defaults);
    target.visibility_overrides.extend(reopen.visibility_overrides);
    extend_new(&mut target.sti_subclass_names, reopen.sti_subclass_names);
    target.sti_subclass_names.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for (subclass, items) in reopen.subclass_declarations {
        if !target.subclass_declarations.iter().any(|(s, _)| *s == subclass) {
            target.subclass_declarations.push((subclass, items));
        }
    }
    target.subclass_default_scope |= reopen.subclass_default_scope;
    target.lexical_json_shadow |= reopen.lexical_json_shadow;
    extend_new(&mut target.retypable_attributes, reopen.retypable_attributes);
    extend_new(&mut target.defaulted_attributes, reopen.defaulted_attributes);
    extend_new(&mut target.attribute_aliases, reopen.attribute_aliases);
    extend_new(&mut target.claimed_body_calls, reopen.claimed_body_calls);
}

/// An STI subclass (a library class whose superclass is a model) that
/// runs ActiveSupport::Callbacks' class side includes the port itself
/// when no model ancestor's lowered class does (`model_class`), as each
/// inherits ActiveRecord::Base's include natively.
pub fn include_callbacks_in_sti_subclasses(app: &mut crate::App) {
    let module = ClassId(Symbol::from("ActiveSupport::Callbacks"));
    let models = &app.models;
    fn ancestor_includes<'a>(mut parent: Option<&'a ClassId>, models: &'a [Model], module: &ClassId) -> bool {
        let mut seen: Vec<&ClassId> = Vec::new();
        while let Some(model) = parent.and_then(|p| models.iter().find(|m| &m.name == p)) {
            if seen.contains(&&model.name) {
                break;
            }
            seen.push(&model.name);
            if crate::analyze::model_includes(model).contains(module) || !callback_dsl_calls(model, models).is_empty() {
                return true;
            }
            parent = model.parent.as_ref();
        }
        false
    }
    let mut adds: Vec<usize> = Vec::new();
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.includes.contains(&module) || !lc.parent.as_ref().is_some_and(|p| models.iter().any(|m| &m.name == p)) {
            continue;
        }
        let own: Vec<&MethodDef> = lc.methods.iter().filter(|m| m.receiver == MethodReceiver::Class).collect();
        if !callback_calls(lc.unknown_calls.iter(), &own, lc.parent.as_ref(), models).is_empty()
            && !ancestor_includes(lc.parent.as_ref(), models, &module)
        {
            adds.push(i);
        }
    }
    for i in adds {
        app.library_classes[i].includes.insert(0, module.clone());
    }
}

/// The class-body calls an STI subclass's Ruby-family emit drops
/// (`emit::ruby::library::modelled_class_body_calls`), each refused like
/// a model's [`unclaimed_unknowns`]. An STI subclass is a model to Rails
/// and a library class to ingest (`lower::sti_subclass_callbacks`); the
/// lifecycle declarations that pass folds into hooks are claimed.
fn unclaimed_sti_subclass_calls(app: &crate::App) -> Vec<crate::diagnostic::Diagnostic> {
    let mut out = Vec::new();
    // A model subclass's own default scope is emitted on it, STI or not
    // (`lower::sti_scope`, `lower::default_scope::push_subclass_methods`).
    for lc in &app.library_classes {
        if !lc.parent.as_ref().is_some_and(|p| app.models.iter().any(|m| &m.name == p)) {
            continue;
        }
        let kept = crate::emit::ruby::modelled_class_body_calls(lc, app);
        let macros = macro_replay::sti_macro_verdicts(lc, app);
        for call in &lc.unknown_calls {
            if kept.iter().any(|k| std::ptr::eq(*k, call))
                || crate::analyze::is_include_refusal(call)
                || crate::lower::sti_subclass_callbacks::folds(call)
                || crate::lower::default_scope::claims(call)
                || crate::lower::sti_subclass_attributes::claims(app, lc, call)
                || crate::lower::subclass_macros::lowers(app, &lc.name, call)
                || app.models.iter().any(|m| lc.parent.as_ref() == Some(&m.name) && m.claimed_body_calls.contains(&call.span))
            {
                continue;
            }
            let detail = match macros.iter().find(|(m, _)| std::ptr::eq(*m, call)) {
                Some((_, macro_replay::MacroVerdict::Refuse(reason))) => {
                    format!("model macro call on `{}` not lowered; {reason}", lc.name.0.as_str())
                }
                Some((_, macro_replay::MacroVerdict::BelongsToRequiredByDefault(_))) => continue,
                _ => format!(
                    "STI subclass body call on `{}` not lowered; {}",
                    lc.name.0.as_str(),
                    unlowered_dsl_reason(dropped_statement_name(call).as_str())
                ),
            };
            out.push(crate::diagnostic::Diagnostic::unsupported(call.span, None, dropped_statement_name(call).as_str(), detail));
        }
    }
    out
}

/// The model's class-body calls that run on ActiveSupport::Callbacks,
/// which every ActiveRecord::Base includes: a bare or `self.` call of the
/// module's class side (`define_callbacks :revocation`, `set_callback
/// :revocation, :after, :notify`, `skip_callback`), or of a class method the
/// model or an app model ancestor defines whose body reaches one (Core's
/// Payments::Vault concerns' `after_revocation :notify` and
/// `around_redaction :capture`, `class_methods` macros over
/// `set_callback`). A concern's `included do` statements are spliced
/// into the includer's body where the include is, so they count as its
/// own. In body order.
///
/// A call that addresses one of ActiveRecord's own chains
/// ([`ACTIVE_RECORD_CALLBACK_CHAINS`], `skip_callback :save, :before,
/// :x`) is not one: the lowered model runs its lifecycle callbacks
/// without the port, so replaying it there would not reach them.
///
/// `model_class` carries these into the lowered class with the module
/// included; the Ruby family replays them through the runtime port
/// (`emit::ruby::library::model_callback_calls`) and a tree without the
/// port refuses them (`runtime_reads::ruby_family_runtime_includes`).
pub(crate) fn callback_dsl_calls<'a>(model: &'a Model, models: &[Model]) -> Vec<&'a Expr> {
    let own: Vec<&MethodDef> = model.methods().filter(|d| d.receiver == MethodReceiver::Class).collect();
    let calls = model.body.iter().filter_map(|item| match item {
        crate::dialect::ModelBodyItem::Unknown { expr, .. } => Some(expr),
        _ => None,
    });
    callback_calls(calls, &own, model.parent.as_ref(), models)
}

/// [`callback_dsl_calls`] over any class body: `calls` the body's
/// statements, `own` its class methods, and `parent` its superclass,
/// whose model ancestors' class methods it answers too (an STI subclass,
/// which ingest keeps as a library class, passes its own).
pub(crate) fn callback_calls<'a>(
    calls: impl Iterator<Item = &'a Expr>,
    own: &[&MethodDef],
    parent: Option<&ClassId>,
    models: &[Model],
) -> Vec<&'a Expr> {
    let port = callbacks_port_methods();
    if port.is_empty() {
        return Vec::new();
    }
    let port: &[String] = port;
    // The class methods the class answers from its own body or an app
    // model ancestor's, nearest first.
    let mut chain: Vec<&Model> = Vec::new();
    let mut next = parent;
    while let Some(name) = next {
        match models.iter().find(|m| &m.name == name) {
            Some(model) if !chain.iter().any(|seen| seen.name == model.name) => {
                chain.push(model);
                next = model.parent.as_ref();
            }
            _ => break,
        }
    }
    let class_method = |name: &str| -> Option<&MethodDef> {
        own.iter().rev().find(|d| d.name.as_str() == name).copied().or_else(|| {
            chain.iter().find_map(|m| {
                m.methods().filter(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == name).last()
            })
        })
    };
    fn on_self(recv: &Option<Expr>) -> bool {
        recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
    }
    // Does a bare or `self.` call in `expr` reach the port, directly or
    // through another such class method?
    fn reaches<'m>(
        expr: &Expr,
        port: &[String],
        class_method: &dyn Fn(&str) -> Option<&'m MethodDef>,
        seen: &mut Vec<String>,
    ) -> bool {
        if let ExprNode::Send { recv, method, .. } = &*expr.node {
            if on_self(recv) {
                let name = method.as_str();
                if port.iter().any(|p| p == name) {
                    return !addresses_model_chain(expr);
                }
                if !seen.iter().any(|s| s == name) {
                    if let Some(def) = class_method(name) {
                        seen.push(name.to_string());
                        if reaches(&def.body, port, class_method, seen) {
                            return true;
                        }
                    }
                }
            }
        }
        let mut found = false;
        expr.node.for_each_child(&mut |child| {
            if !found {
                found = reaches(child, port, class_method, seen);
            }
        });
        found
    }
    calls
        .filter(|expr| {
            let ExprNode::Send { recv, method, .. } = &*expr.node else { return false };
            if !on_self(recv) {
                return false;
            }
            let name = method.as_str();
            if port.iter().any(|p| p == name) {
                return !addresses_model_chain(expr);
            }
            class_method(name).is_some_and(|def| {
                reaches(&def.body, port, &class_method, &mut vec![name.to_string()])
            })
        })
        .collect()
}

/// The callback chains ActiveRecord::Base defines on itself (and
/// ActiveModel::Validations'), which a lowered model runs through its
/// own lifecycle rather than ActiveSupport::Callbacks.
const ACTIVE_RECORD_CALLBACK_CHAINS: &[&str] = &[
    "before_commit",
    "commit",
    "create",
    "destroy",
    "find",
    "initialize",
    "rollback",
    "save",
    "touch",
    "update",
    "validate",
    "validation",
];

/// Does a port call name one of [`ACTIVE_RECORD_CALLBACK_CHAINS`] as a
/// literal chain (`set_callback :save, ...`, `define_callbacks :create`)?
fn addresses_model_chain(call: &Expr) -> bool {
    let ExprNode::Send { method, args, .. } = &*call.node else { return false };
    let chain_args = if method.as_str() == "define_callbacks" { &args[..] } else { &args[..args.len().min(1)] };
    chain_args.iter().any(|arg| {
        let name = match &*arg.node {
            ExprNode::Lit { value: Literal::Sym { value } } => value.as_str(),
            ExprNode::Lit { value: Literal::Str { value } } => value.as_str(),
            _ => return false,
        };
        ACTIVE_RECORD_CALLBACK_CHAINS.contains(&name)
    })
}

/// Filter a controller's permitted-fields list down to the names this
/// model can actually assign — the union of every writer source the
/// synthesizers and the model body jointly provide: table columns,
/// `belongs_to` writers, `attr_accessor`/`attr_writer` virtuals,
/// `typed_store` attrs, `has_secure_password`'s plaintext pair, and
/// user-defined `def <field>=`.
///
/// Rails' permit contract is wider than its assign contract: permitting
/// a name is harmless until assignment meets a field with no writer
/// (`ActiveModel::UnknownAttributeError`). Lobsters permits lookup keys
/// (`tag[tag_name]`, `category[category_name]`) that no writer backs —
/// synthesizing `self.tag_name = p.tag_name` into `update` turned that
/// latent contract gap into a hard undefined-method error under spinel
/// AOT. Dropping the assignment diverges from Rails only for a request
/// that actually submits the writerless key (Rails raises, we ignore);
/// each dropped name is ledgered as a `lower_residue` warning.
fn writable_permit_fields(
    model: &Model,
    table: &crate::schema::Table,
    fields: &[crate::ident::Symbol],
) -> Vec<crate::ident::Symbol> {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    use crate::ident::Symbol;

    let writable = writable_field_set(model, table);

    fields
        .iter()
        .filter(|field| {
            if writable.contains(*field) || model_defines_writer(model, field) {
                return true;
            }
            let kind = DiagnosticKind::LowerResidue {
                pass: Symbol::from("permit_writer_filter"),
                construct: Symbol::from("permit"),
                reason: Symbol::from("no writer"),
            };
            let d = Diagnostic {
                span: model.span,
                severity: Diagnostic::default_severity(&kind),
                kind,
                message: format!(
                    "permitted field `{field}` has no writer on `{model_name}` (not a \
                     column, attr_writer, belongs_to, typed_store, has_secure_password, \
                     or `def {field}=`) — dropped from the synthesized update/from_params; \
                     Rails would raise UnknownAttributeError if a request submitted it",
                    field = field.as_str(),
                    model_name = model.name.0.as_str(),
                ),
            };
            crate::emit::diagnostics::push(d);
            false
        })
        .cloned()
        .collect()
}

/// Every name this model can be assigned through, EXCEPT the per-field
/// `def <field>=` check (which needs the field name — see
/// `model_defines_writer`). Split out of `writable_permit_fields` so
/// other passes can ask the same question without its ledger side
/// effect: `params_merge` has to know whether a merged key has a writer
/// before it synthesizes a setter, and a second copy of this scan would
/// drift.
pub fn writable_field_set(
    model: &Model,
    table: &crate::schema::Table,
) -> std::collections::BTreeSet<crate::ident::Symbol> {
    use crate::dialect::{Association, ModelBodyItem};
    use crate::expr::{ExprNode, Literal};
    use crate::ident::Symbol;

    let mut writable: std::collections::BTreeSet<Symbol> = std::collections::BTreeSet::new();
    for col in &table.columns {
        writable.insert(col.name.clone());
    }
    for (_span, assoc) in model.spanned_associations() {
        if let Association::BelongsTo { name, .. } = assoc {
            writable.insert(name.clone());
        }
    }
    // `attr_accessor` / `attr_writer` virtuals (mirrors the scan in
    // markers::push_attr_accessor_methods).
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
            continue;
        };
        if !matches!(method.as_str(), "attr_accessor" | "attr_writer") {
            continue;
        }
        for arg in args {
            if let ExprNode::Lit { value: Literal::Sym { value } } = &*arg.node {
                writable.insert(value.clone());
            }
        }
    }
    for (_store, attrs) in crate::lower::typed_store::typed_store_decls(&model.body) {
        for attr in attrs {
            writable.insert(attr.name);
        }
    }
    if let Some(attr) = crate::lower::secure_password::secure_password_attr(&model.body) {
        writable.insert(Symbol::from(format!("{}_confirmation", attr.as_str())));
        writable.insert(attr);
    }
    for (name, _ty) in self::markers::attribute_api_decls(&model.body) {
        writable.insert(name);
    }
    // `has_rich_text :body` synthesizes `body=`, so `permit(:body)` is
    // assignable even though `body` is not a column on this table.
    // Without this the permit filter drops it and campfire's composer
    // posts a message with no content.
    for (_span, attr) in crate::lower::rich_text::rich_text_attrs(model) {
        writable.insert(attr);
    }
    // Callers without schema cannot prove the backing table; those
    // paths go through `build_methods` which gates expansion. Here the
    // attrs are still shape-claimed — `push_plain_text_methods` is the
    // hard gate. Keep permit surface aligned when the declaration is
    // present (Writebook / emit_and_run always ship the table).
    for (_span, attr) in crate::lower::plain_text_attr::plain_text_attrs(model) {
        writable.insert(attr);
    }
    // `has_one_attached :avatar` synthesizes `avatar=` the same way, so
    // a permitted `:avatar` (campfire's signup, profile, bot and account
    // forms all permit one) reaches the record instead of being dropped
    // at the permit filter — which is what made every avatar picker
    // post a file nothing read.
    for (_span, attr) in crate::lower::attached::attached_attrs(model) {
        writable.insert(attr);
    }
    // `has_many_attached` is claimed for the proxy reader / `.attach` /
    // `.attachments` surface only. There is no `attr=` / after_save
    // writer yet (Many appends via the proxy, not mass-assign), so the
    // attrs stay OUT of the writable set — putting them in would make
    // `update(uploads: …)` / permit emit a missing writer (inv. 6).
    writable
}

/// The per-field half of the writable question: a hand-written
/// `def <field>=` in the model body.
pub(crate) fn model_defines_writer(model: &Model, field: &crate::ident::Symbol) -> bool {
    let writer = crate::ident::Symbol::from(format!("{}=", field.as_str()));
    model_defines_instance_method(model, &writer)
}

/// The untyped model synthesis emission uses, built per model the first
/// time a forwarding contract question needs it, and the two questions.
///
/// The forwarding analysis asks whether synthesis keeps a source
/// contract: a model's own forwarding method, or a declaration (own or
/// inherited) a source packet selects that a synthesized override could
/// shadow. Building every model that could be asked about cost seconds
/// per forwarding pass on Core, where the passes ask about few
/// declarations; so the analysis finds the models a span concerns and asks
/// only about those. Each answer is the one the whole-
/// model check gave for that source.
///
/// Source lookup stays with the caller: inherited library contracts can
/// also be forwarding destinations, even when neither they nor the model
/// declare `...`.
pub(crate) struct ModelSynthesis<'a> {
    app: &'a crate::App,
    specs: std::cell::OnceCell<crate::lower::controller_to_library::params::ParamsSpecs>,
    /// Per model position, its synthesized methods.
    built: std::cell::RefCell<HashMap<usize, std::rc::Rc<Vec<MethodDef>>>>,
    /// Time spent synthesizing, for `ROUNDHOUSE_TIMINGS`.
    spent: std::cell::Cell<std::time::Duration>,
    /// `app.models`' body names, shared by every model's synthesis.
    body_names: schema::ModelBodyNames,
}

impl<'a> ModelSynthesis<'a> {
    pub(crate) fn new(app: &'a crate::App) -> Self {
        ModelSynthesis {
            app,
            specs: std::cell::OnceCell::new(),
            built: std::cell::RefCell::new(HashMap::default()),
            spent: std::cell::Cell::new(std::time::Duration::ZERO),
            body_names: Default::default(),
        }
    }

    /// Time spent synthesizing so far.
    pub(crate) fn time_spent(&self) -> std::time::Duration {
        self.spent.get()
    }

    /// How many models have been synthesized so far.
    pub(crate) fn models_built(&self) -> usize {
        self.built.borrow().len()
    }

    fn built(&self, position: usize) -> std::rc::Rc<Vec<MethodDef>> {
        if let Some(built) = self.built.borrow().get(&position) {
            return built.clone();
        }
        let app = self.app;
        let started = std::time::Instant::now();
        // Synthesis can report incidental emit warnings. A survey must neither
        // publish those nor consume an enclosing transpile's diagnostic buffer.
        let built = crate::emit::diagnostics::scope(|| {
            let specs = self.specs.get_or_init(|| {
                let mut specs =
                    crate::lower::controller_to_library::params::collect_specs(&app.controllers);
                specs.mark_file_fields(&app.models);
                specs
            });
            let _body_names = schema::share_model_body_names(&app.models, &self.body_names);
            build_methods(&app.models[position], &app.models, &app.schema, specs)
        })
        .0;
        self.spent.set(self.spent.get() + started.elapsed());
        let built = std::rc::Rc::new(built);
        self.built.borrow_mut().insert(position, built.clone());
        built
    }

    fn preserved(source: &MethodDef, built: &MethodDef) -> bool {
        built.name_span == source.name_span
            && built.params == source.params
            && built.block_param == source.block_param
    }

    /// Does the model at `position` lose its own forwarding method
    /// `source`: is another of its definitions the effective one, or does
    /// the synthesized class not keep this one's contract?
    pub(crate) fn drops_own(&self, position: usize, source: &MethodDef) -> bool {
        let model = &self.app.models[position];
        let matches = |m: &&MethodDef| m.name == source.name && m.receiver == source.receiver;
        let effective = model.methods().filter(matches).last().unwrap();
        effective.name_span != source.name_span
            || self
                .built(position)
                .iter()
                .rev()
                .find(matches)
                .is_none_or(|m| !Self::preserved(source, m))
    }

    /// Does a synthesized method of the model at `position` override
    /// `source`, a declaration its lookup selects for a selector that
    /// receives source argument packets (its own or an inherited one),
    /// without its contract? No synthesized same-name method means the
    /// source declaration survives.
    pub(crate) fn drops_selected(&self, position: usize, source: &MethodDef) -> bool {
        self.built(position)
            .iter()
            .rev()
            .find(|m| m.name == source.name && m.receiver == source.receiver)
            .is_some_and(|m| !Self::preserved(source, m))
    }
}

/// Build the common model method set without Ruby-family request widening;
/// strict-target callers keep the schema's scalar finder signatures.
pub(crate) fn build_methods(
    model: &Model,
    models: &[Model],
    schema: &Schema,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
) -> Vec<MethodDef> {
    build_methods_for_dialect(
        model, models, schema, params_specs, FinderInputs::Scalar, ErrorsReaders::FieldRead,
        crate::lower::arel::SqlDialect::Sqlite,
    )
}

/// Return the instance method surface synthesized for a model, for ingest
/// passes that need collision checks without depending on `MethodDef` details.
pub(crate) fn method_names(
    model: &Model,
    models: &[Model],
    schema: &Schema,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
) -> HashSet<String> {
    build_methods(model, models, schema, params_specs)
        .into_iter()
        .filter(|method| method.receiver == MethodReceiver::Instance)
        .map(|method| method.name.as_str().to_string())
        .collect()
}

/// Add the schema dispatch only when its shared Ruby-family owner is shipped,
/// and render SQL in the owning database's dialect.
fn build_methods_for_dialect(
    model: &Model,
    models: &[Model],
    schema: &Schema,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
    finder_inputs: FinderInputs,
    errors_readers: ErrorsReaders,
    dialect: crate::lower::arel::SqlDialect,
) -> Vec<MethodDef> {
    let mut methods: Vec<MethodDef> = Vec::new();

    if let Some(table) = schema.tables.get(&model.table.0) {
        let resource = crate::ident::Symbol::from(crate::naming::snake_case(model.name.0.as_str()));
        // The canonical permit list — the one holding the unqualified
        // `<Resource>Params` name — sizes the plain `update` / `update!`
        // / `from_params`. Other lists for the same resource get their
        // own named methods below, since their params class is an
        // unrelated type on every strict target.
        let canonical = params_specs.canonical(&resource);
        let permitted_fields = canonical.map(|s| s.fields.as_slice());
        // A controller's permit list is wider than the model's writer
        // surface (lobsters permits lookup keys like `tag[tag_name]`
        // that no writer backs) — filter to assignable names before
        // sizing `from_params`. The `<Resource>Params` class itself keeps
        // every permitted field (registration above uses the unfiltered
        // spec); only the assignment synthesis narrows.
        let writable_fields =
            permitted_fields.map(|fields| writable_permit_fields(model, table, fields));
        let permitted_fields = writable_fields.as_deref();
        push_schema_methods(&mut methods, model, models, table);
        // The PostgreSQL CRuby tree ships Rails' mass assignment
        // (runtime/spinel/cruby/active_record_core.rb); a PostgreSQL
        // app's ruby family is that tree.
        if dialect == crate::lower::arel::SqlDialect::Postgres && crate::emit::ruby::native_keywords() {
            assign_attributes::route_through_assign_attributes(&mut methods);
        }
        if dialect == crate::lower::arel::SqlDialect::Postgres {
            if let Some(method) = methods.iter_mut().find(|method| method.name.as_str() == "_table_sql") {
                method.body = crate::lower::typing::lit_str(dialect.ident(table.name.as_str()));
            }
        }
        // Per-model Level-3 adapter primitives (`_adapter_find_by_id`, etc.)
        // — typed methods that go directly from SQL composition to typed
        // model instances over the `Sqlite` primitive surface. See
        // project_level_3_adapter_emit.md.
        if finder_inputs == FinderInputs::Request {
            methods.push(adapter_emit::synth_find_primary_key_input(model, table));
            methods.push(adapter_emit::synth_exists_primary_key_input(model, table));
        }
        push_adapter_methods(&mut methods, model, table, schema, dialect);
        // `from_params(p: <Resource>Params)` — typed factory matching the
        // (resource, fields) tuple a controller's `permit(...)` declared.
        // Skipped silently when the model isn't permitted by any
        // controller.
        if let (Some(fields), Some(spec)) = (permitted_fields, canonical) {
            self::schema::push_from_params_method(
                &mut methods, model, fields, table, &spec.class_id,
                crate::lower::controller_to_library::params::model_from_params_name(spec),
            );
        }
        // One typed update pair per permit list of this resource —
        // INCLUDING the canonical one, which no longer claims the plain
        // `update` / `update!` names (those are Rails-shaped now; see
        // `synth_update_hash`). Two lists for one resource are unrelated
        // types on every strict target, so each needs its own method
        // name and `rewrite_update_to_typed_variant` retargets the
        // controller call site to it. The typed factory (`from_params`)
        // still keeps the plain name for the canonical list, since
        // nothing else competes for it.
        for spec in params_specs.for_resource(&resource) {
            if spec.is_canonical {
                let writable = writable_permit_fields(model, table, &spec.fields);
                self::schema::push_update_typed_variants(
                    &mut methods, model, &writable, table, spec,
                );
                continue;
            }
            let writable = writable_permit_fields(model, table, &spec.fields);
            self::schema::push_from_params_method(
                &mut methods, model, &writable, table, &spec.class_id,
                crate::lower::controller_to_library::params::model_from_params_name(spec),
            );
            self::schema::push_update_typed_variants(
                &mut methods, model, &writable, table, spec,
            );
        }
        // `create_from_params(p)` / `create_from_params!(p)` — the typed
        // factory plus a save, one per permit list that a call site
        // actually creates through. Demand-gated on the spec (like
        // `wants_except`) so a model nobody creates this way keeps the
        // surface it had.
        for spec in params_specs.for_resource(&resource) {
            use crate::lower::controller_to_library::params::{
                model_create_from_params_name, model_from_params_name,
            };
            for bang in [false, true] {
                let wanted = if bang { spec.wants_create_bang } else { spec.wants_create };
                if !wanted {
                    continue;
                }
                self::schema::push_create_from_params_method(
                    &mut methods,
                    &model.name,
                    &spec.class_id,
                    model_from_params_name(spec),
                    model_create_from_params_name(spec, bang),
                    bang,
                );
            }
        }
        // A composite primary key re-keys what the passes above
        // synthesized: `id` is the tuple, the `id` column `id_value`.
        // Before the model's own methods, which win over the tuple's.
        self::composite_key::rekey(&mut methods, model, table);
    }

    // Every tree carries each error's detail as `error_details`; the CRuby
    // and JRuby trees record it in the runtime's `ErrorAttributes` too.
    push_validate_method(&mut methods, model, models, errors_readers, true);
    push_association_methods(&mut methods, model, models);
    self::macro_associations::push_reflection_methods(&mut methods, model, models);
    push_dependent_destroy(&mut methods, model);
    push_unknown_marker_methods(&mut methods, model);
    push_attr_accessor_methods(&mut methods, model);
    // AFTER the accessors: the constructor assigns exactly what they
    // declare, and it checks `methods` for an existing `initialize`.
    self::validations::push_active_model_constructor(&mut methods, model);
    // `attribute :name, :type` (Rails Attributes API) — typed virtual
    // readers + cast writers; same before-user-methods ordering.
    self::markers::push_attribute_api_methods(&mut methods, model);
    // typed_store virtual attributes — reader/predicate/writer per
    // declared attr, routing through the `TypedStore` runtime (the
    // YAML seam). Before `push_user_methods` so a custom method in the
    // model body wins via the synthesizer's own model-body check.
    crate::lower::typed_store::push_typed_store_methods(&mut methods, model, &self::schema::demanded_names(models));
    // `has_json` schema keys — the JSON twin of the above: one typed
    // accessor triple per declared key, over the serialized column.
    // Same ordering rationale.
    crate::lower::has_json::push_has_json_methods(&mut methods, model);
    // has_secure_password — authenticate + plaintext accessors,
    // against the bcrypt gem's own surface (`BCrypt::Password`). Same
    // ordering rationale.
    crate::lower::secure_password::push_secure_password_methods(&mut methods, model);
    // Action Text — `has_rich_text`'s expansion on a declaring model,
    // and the `body`-as-Content coder on the synthesized
    // `ActionText::RichText`. AFTER `push_schema_methods` because the
    // coder REPLACES the column accessor pair that pass produced;
    // before `push_user_methods` for the usual reason (a hand-written
    // method in the model body wins).
    crate::lower::rich_text::push_rich_text_methods(&mut methods, model);
    // Named plain-text association (`has_markdown`) — same slot; the
    // record's `content` column needs no coder override.
    crate::lower::plain_text_attr::push_plain_text_methods(&mut methods, model, schema);
    // `has_one_attached` / `has_many_attached` — the attachment
    // readers, over the synthesized `ActiveStorage::Attachment` row.
    // Same ordering rationale as the macros above (a hand-written
    // method wins). The Attachment MODEL itself gets its blob helpers
    // here too (`url` / `filename` / `content_type`).
    crate::lower::attachment_model::push_attachment_record_methods(&mut methods, model);
    crate::lower::attached::push_attached_methods(&mut methods, model);
    // The ruby family writes column accessors into the model's
    // GeneratedAttributeMethods module (`emit::ruby::library`), so a
    // model's own `def name=` overrides them and can `super` into them,
    // as in Rails; it rides beside the synthesized accessor here.
    let generated_attribute_overrides = match schema.tables.get(&model.table.0) {
        Some(table) if crate::emit::ruby::native_keywords() => {
            generated_attribute_method_names(model, models, table)
        }
        _ => Default::default(),
    };
    // Snapshot BEFORE push_user_methods: an association reader or
    // `attr_accessor` pair's PRISTINE synthesized body, for
    // `resolve_model_aliases` below. `push_user_methods` mutates
    // `methods` in place when a later hand-written `def` overrides
    // one of these (last-def-wins) — exactly the collapse snapshot
    // semantics needs to see THROUGH, not after.
    let pre_user_methods = methods.clone();
    push_user_methods(&mut methods, model, &generated_attribute_overrides);
    // friendly_id — `friendly`, its configuration and the slug
    // `to_param`. After the user methods (their own wins), before the
    // `to_param` default below.
    crate::lower::friendly_id::push_friendly_methods(&mut methods, model);
    // AFTER push_user_methods and every other synthesizer above it
    // (friendly_id included): resolving `alias new old` needs to see
    // the full method surface (columns, associations, attr_accessor,
    // attribute, hand-written defs, enum methods, friendly_id's
    // `friendly`) to tell a real gap from a false one. See
    // `resolve_model_aliases`'s own doc for the snapshot semantics and
    // the Rails-grounded per-source matrix.
    resolve_model_aliases(model, &mut methods, &pre_user_methods, schema);
    push_dom_prefix_method(&mut methods, model, models);
    // AFTER push_user_methods, unlike the macros above: its skip guard
    // reads the accumulated list, so a model's own `to_param` (already
    // pushed) wins over the synthesized `@id.to_s`.
    self::markers::push_to_param_method(&mut methods, model);
    // Also after push_user_methods: its BODY branches on whether the
    // model carries its own `to_key` (campfire's Message does).
    self::markers::push_dom_record_key_method(&mut methods, model);
    // After push_user_methods too: a model that writes its own
    // `cache_key` keeps it.
    self::markers::push_cache_key_methods(&mut methods, model, schema);
    // The identity methods above read the key slot; a composite key's
    // read the tuple.
    self::composite_key::rekey_identity(&mut methods, model);
    push_broadcasts_methods(&mut methods, model);
    // `has_secure_token` — the token default folds into the
    // `before_create` hook, so it runs BEFORE `push_callback_methods`:
    // the fold appends, and the macro's assignment belongs ahead of a
    // callback the model declares below it (Rails' declaration order).
    crate::lower::secure_token::push_secure_token_methods(&mut methods, model);
    // `generates_token_for` — the token methods, over the TokenFor
    // runtime. After the user methods, so a model's own
    // `generate_token_for` wins.
    crate::lower::generates_token_for::push_token_for_methods(&mut methods, model);
    let mut lifted_callbacks = Vec::new();
    push_callback_methods(&mut methods, &mut lifted_callbacks, model);
    methods.extend(lifted_callbacks);
    self::transaction_callbacks::push_transaction_callback_methods(&mut methods, model, models);

    // File-grain catch-all (mirrors view_to_library's
    // build_library_class): whatever the synthesizers left span-less —
    // schema accessors, adapter primitives, `from_params`,
    // `dom_prefix`, Seq wrappers — attributes to the model's class
    // declaration. Nodes the per-declaration stamps above already
    // covered keep their finer spans.
    for m in &mut methods {
        m.body.inherit_span(model.span);
    }

    methods
}

/// Emit each `scope :name, ->(args){ body }` as a class method
/// `def self.name(args, __rel = ActiveRecord::Relation.new(self))` whose
/// body is rewritten (scope-chain normalization) to thread `__rel`: bare
/// query calls (`where`/`order`/…) target `__rel`, and nested scope calls
/// become `Model.scope(args, recv)`. The trailing relation parameter is
/// what lets `Story.base(u).positive_ranked` chain without metaprogramming.
/// Generate scope class methods for a model. Lives here (next to the other
/// model synthesizers) but is invoked only from the Ruby emit seam — the
/// scope methods call `ActiveRecord::Relation`, which only the CRuby/JRuby
/// runtime provides, so other targets must NOT receive them.
/// User-defined `def` methods on the model (`def can_be_seen_by_user?(u)`,
/// `def as_json`, …). These were dropped — `build_methods` synthesized
/// schema/association/scope methods but never carried the model's own
/// method bodies through. Emit them now; their bodies ride the same
/// arel-rewrite + body-typer the synthesized methods do.
///
/// Name collisions:
///   * A synthesized **attr_accessor / attr_reader / attr_writer**
///     half yields to a later real `def` of that name — Ruby's
///     last-definition-wins. Campfire's `Opengraph::Location` declares
///     `attr_accessor :parsed_url` and then memoizes
///     `def parsed_url; … URI.parse …; end`; keeping the bare
///     `@parsed_url` reader left the ivar untyped/unread and Spinel's
///     strict emit failed with `error[ivar_unresolved]`.
///   * Column / association / scope synthesizers still win over a
///     duplicate body name (the corpus does not redefine those). The
///     replace predicate matches unsigned bare-ivar attr_* halves only
///     (`signature: None`); schema column readers stamp a signature and
///     are never replaced. The ruby family keeps both for a generated
///     attribute method (`generated_attribute_overrides`): the
///     synthesized one is written into GeneratedAttributeMethods.
///     A later source-level `delegate` may replace an earlier real
///     method, but only when that method was already selected from the
///     model body; generated framework methods are not reordered by
///     source spans.
fn push_user_methods(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    generated_attribute_overrides: &crate::det::HashSet<Symbol>,
) {
    use crate::dialect::{AccessorKind, ModelBodyItem};
    use crate::expr::{ExprNode, LValue};
    let mut selected_source_methods = HashSet::default();
    for item in &model.body {
        let ModelBodyItem::Method { method, .. } = item else { continue };
        if let Some(idx) = methods
            .iter()
            .position(|m| m.name == method.name && m.receiver == method.receiver)
        {
            let existing = &methods[idx];
            let incoming_is_real = matches!(method.kind, AccessorKind::Method);
            // Only bare-ivar attr_* halves (attr_accessor/reader/writer
            // synth, which carry no signature) yield to a later real
            // `def`. Schema column AttributeReaders are also bare
            // `@col` reads for scalar columns, but they stamp a
            // signature — keep those winning (documented above).
            let existing_is_attr_half = existing.signature.is_none()
                && match existing.kind {
                    AccessorKind::AttributeReader => {
                        // `@name`, or the `@@name` of a `cattr_*`.
                        match &*existing.body.node {
                            ExprNode::Ivar { name } => name == &existing.name,
                            ExprNode::Var { name, .. } => name.as_str().strip_prefix("@@") == Some(existing.name.as_str()),
                            _ => false,
                        }
                    }
                    AccessorKind::AttributeWriter => {
                        let base = existing
                            .name
                            .as_str()
                            .strip_suffix('=')
                            .unwrap_or(existing.name.as_str());
                        match &*existing.body.node {
                            ExprNode::Assign { target: LValue::Ivar { name }, .. } => name.as_str() == base,
                            ExprNode::Assign { target: LValue::Var { name, .. }, .. } => {
                                name.as_str().strip_prefix("@@") == Some(base)
                            }
                            _ => false,
                        }
                    }
                    AccessorKind::Method => false,
                };
            let incoming_is_later_delegate =
                method.receiver == crate::dialect::MethodReceiver::Instance
                    && !method.name_span.is_synthetic()
                    && method.name_span.start == method.name_span.end
                    && selected_source_methods.contains(&existing.name_span)
                    && existing.name_span.file == method.name_span.file
                    && method.name_span.start > existing.name_span.start;
            if incoming_is_real && (existing_is_attr_half || incoming_is_later_delegate) {
                selected_source_methods.remove(&existing.name_span);
                methods[idx] = method.clone();
                selected_source_methods.insert(method.name_span);
            } else if incoming_is_real
                && method.receiver == MethodReceiver::Instance
                && existing.name_span.is_synthetic()
                && generated_attribute_overrides.contains(&method.name)
            {
                // Ruby family only: the synthesized accessor moves to
                // GeneratedAttributeMethods and this def overrides it.
                methods.push(method.clone());
            }
            continue;
        }
        methods.push(method.clone());
        selected_source_methods.insert(method.name_span);
    }
}

/// A model subclass's view model's synthesized methods
/// (`lower::subclass_macros`): its associations, the `dependent:
/// :destroy` cascade, its lifecycle hooks, its required `belongs_to`
/// checks and its transaction-callback chains (`lineage` the classes
/// above it, `subclass_macros::lineage`). A hook starts with `super`:
/// Rails' subclass chain is a copy of its model's with the subclass's
/// callbacks appended. A method the subclass defines wins.
pub(crate) fn push_subclass_model_methods(methods: &mut Vec<MethodDef>, view: &Model, lineage: &[Model], models: &[Model]) {
    let mut synthesized = Vec::new();
    associations::push_association_methods(&mut synthesized, view, models);
    let mut hooks = Vec::new();
    let mut lifted_callbacks = Vec::new();
    push_dependent_destroy(&mut hooks, view);
    push_callback_methods(&mut hooks, &mut lifted_callbacks, view);
    for hook in &mut hooks {
        if hook.receiver != MethodReceiver::Instance || !hook.params.is_empty() {
            continue;
        }
        let own = match &*hook.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![hook.body.clone()],
        };
        let mut stmts = vec![Expr::new(Span::synthetic(), ExprNode::Super { args: Some(Vec::new()), block: None })];
        stmts.extend(own);
        hook.body = seq(stmts);
    }
    synthesized.extend(hooks);
    synthesized.extend(lifted_callbacks);
    for m in synthesized {
        if !methods.iter().any(|o| o.name == m.name && o.receiver == m.receiver) {
            methods.push(m);
        }
    }
    validations::push_subclass_belongs_to_checks(methods, view, models);
    if !lineage.is_empty() {
        self::transaction_callbacks::push_transaction_callback_methods(methods, view, lineage);
    }
}

/// Resolve this model's `alias new old` body items (`ModelBodyItem::Alias`)
/// against the fully synthesized method surface, in source order, and
/// splice each resolved copy into `methods`. Unresolved aliases are
/// refused with an `Error`-severity diagnostic (survey mode demotes it
/// to a gap — see `attribute_analysis_gaps`'s `alias_target_unresolved`
/// branch), exactly like today's ingest-time refusal did.
///
/// **Snapshot semantics are mandatory**: `old` is read as defined AT
/// THE ALIAS LINE, not as finally defined. A hand-written `def` or an
/// enum-generated method (both land in `model.body` as
/// `ModelBodyItem::Method`, in source order) is tracked precisely: a
/// later redefinition of `old` doesn't change what the alias copied,
/// and `old` declared only after the alias doesn't resolve at all. A
/// column / association / `attr_accessor` / `attribute` name is
/// resolved against its one synthesized definition regardless of
/// position — Rails apps declare each of those exactly once per
/// model, and this corpus has no counterexample, so order tracking
/// for them isn't implemented.
///
/// The same reasoning applies on the way OUT, to `new`: a hand-written
/// `def new` declared AFTER this alias in the body is the last
/// definition and wins over the alias copy, exactly as it would win
/// over a `def new` written before the alias. Splicing the resolved
/// copy into `methods` unconditionally — instead of checking for a
/// later redefinition — silently lost that later `def` (#839).
///

/// **Per-source Rails ground truth** (checked against Rails 8.1 at
/// class-body time, in-memory sqlite ActiveRecord — see the PR
/// description for the transcript):
///   - hand-written `def` / enum method: resolves (already true
///     before this function existed).
///   - `belongs_to` / `has_one` reader: resolves — Rails defines the
///     reader when the macro runs, same as a `def`.
///   - `attr_accessor` / `attr_reader` / `attr_writer`: resolves.
///   - ActiveModel::Attributes `attribute`: resolves in Rails, but
///     roundhouse doesn't synthesize a method for it yet (separate,
///     undone work) — so there's nothing in `methods` to find, and it
///     stays a gap, unchanged from today.
///   - a plain AR column reader: Rails itself raises `NameError`
///     here — ActiveRecord generates attribute methods lazily, which
///     is why `alias_attribute` exists. Refused with a gap that names
///     `alias_attribute` instead.
///   - an ActiveRecord::Base-inherited method (`destroy!`, `save!`,
///     …): resolves as a synthesized one-line forwarder, but ONLY
///     when the model's own body doesn't redefine `old` (concern
///     method bodies aren't visible to this lowering, so a concern
///     override can't be checked — best-effort, documented).
///   - a method from an included concern: resolves in Rails, but NOT
///     here — only a concern's `included do` DSL reaches `model.body`
///     (`App::concern_model_items`); its plain `def`s stay on the
///     concern's own `LibraryClass`, which this function never sees.
///     Stays a gap, unchanged from today. Doesn't appear in the
///     corpus this was grounded against.
fn resolve_model_aliases(
    model: &Model,
    methods: &mut Vec<MethodDef>,
    pre_user_methods: &[MethodDef],
    schema: &Schema,
) {
    use crate::dialect::ModelBodyItem;

    if !model.body.iter().any(|item| matches!(item, ModelBodyItem::Alias { .. })) {
        return;
    }

    let table = schema.tables.get(&model.table.0);
    // Every name declared SOMEWHERE in the body via a category this
    // function can see — a hand-written `def` / enum method, a
    // `belongs_to`/`has_one` reader, or an `attr_accessor`/`attr_reader`/
    // `attr_writer` — regardless of position. A name in this set is
    // "pending" once `live` (below) doesn't have it yet: it WILL be
    // defined, just not at this point in the body, so an alias here
    // must refuse rather than fall through to the inherited-method
    // forwarder (which would be synthesizing a forwarder for a name
    // that is, in fact, about to become a plain local method).
    let mut declared_anywhere: HashSet<Symbol> = HashSet::default();
    for item in &model.body {
        match item {
            ModelBodyItem::Method { method, .. } if method.receiver == MethodReceiver::Instance => {
                declared_anywhere.insert(method.name.clone());
            }
            ModelBodyItem::Association { assoc, .. } => {
                declared_anywhere.insert(assoc.name().clone());
            }
            ModelBodyItem::Unknown { expr, .. } => {
                declared_anywhere.extend(attr_accessor_arg_names(expr));
            }
            _ => {}
        }
    }
    // Body-order snapshot: `live[name]` is what `name` resolves to AT
    // THE CURRENT POSITION, updated as each defining item is reached.
    // A hand-written `def`/enum method carries its own body; an
    // association reader or `attr_accessor` pair is looked up in
    // `pre_user_methods` — the PRISTINE synthesized copy, from before
    // any hand-written override collapsed it in the real `methods`
    // (Rails apps declare each of those exactly once per model, so
    // that one synthesized copy is what's live from here on, until a
    // later hand-written redefinition — tracked by the arm below —
    // changes it again).
    let mut live: HashMap<Symbol, MethodDef> = HashMap::default();

    for (idx, item) in model.body.iter().enumerate() {
        match item {
            ModelBodyItem::Method { method, .. } if method.receiver == MethodReceiver::Instance => {
                live.insert(method.name.clone(), method.clone());
            }
            ModelBodyItem::Association { assoc, .. } => {
                let name = assoc.name();
                if let Some(m) =
                    pre_user_methods.iter().find(|m| m.name == *name && m.receiver == MethodReceiver::Instance)
                {
                    live.insert(name.clone(), m.clone());
                }
            }
            ModelBodyItem::Unknown { expr, .. } => {
                for name in attr_accessor_arg_names(expr) {
                    if let Some(m) = pre_user_methods
                        .iter()
                        .find(|m| m.name == name && m.receiver == MethodReceiver::Instance)
                    {
                        live.insert(name, m.clone());
                    }
                }
            }
            ModelBodyItem::Alias { new, old, span, .. } => {
                let resolved = if let Some(m) = live.get(old) {
                    Some(m.clone())
                } else if declared_anywhere.contains(old) {
                    // Declared later in the body — not live yet, and
                    // resolving against the eventual definition would
                    // break snapshot semantics, so this is a refusal
                    // (the `None` arm below), not a fallback.
                    None
                } else if table.is_some_and(|t| t.columns.iter().any(|c| c.name == *old)) {
                    // A plain AR column reader — Rails itself raises
                    // NameError here (attribute methods are generated
                    // lazily), so this never resolves; the `None` arm
                    // below names `alias_attribute` instead.
                    None
                } else {
                    inherited_forwarder(model, old)
                };
                match resolved {
                    Some(mut copy) => {
                        copy.name = new.clone();
                        copy.name_span = Span::synthetic();
                        // Snapshot semantics cut both ways: `live`
                        // above tracks what `old` meant AT THE ALIAS
                        // LINE, and the same body-order reasoning
                        // applies to `new` on the way out. Ruby's
                        // class body runs top to bottom, so a
                        // hand-written `def new` AFTER this alias
                        // statement is the last definition and wins —
                        // `push_user_methods` (which ran before this
                        // function) already put that later def in
                        // `methods`, so leave it there untouched. Only
                        // a `def new` BEFORE the alias (or no `def
                        // new` at all) lets the alias copy win.
                        let redefined_later = model.body[idx + 1..].iter().any(|later| matches!(
                            later,
                            ModelBodyItem::Method { method, .. }
                                if method.receiver == MethodReceiver::Instance
                                    && method.name == *new
                        ));
                        if !redefined_later {
                            if let Some(midx) = methods
                                .iter()
                                .position(|m| m.name == *new && m.receiver == MethodReceiver::Instance)
                            {
                                methods[midx] = copy.clone();
                            } else {
                                methods.push(copy.clone());
                            }
                        }
                        live.insert(new.clone(), copy);
                    }
                    None => {
                        push_alias_target_unresolved_gap(*span, model, old, new, table);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The symbol names an `attr_accessor` / `attr_reader` / `attr_writer`
/// call declares, when `expr` is one (direct literal-Symbol arguments
/// only — the splat/constant forms `push_attr_accessor_methods`
/// handles separately aren't needed here: missing one of those just
/// means `resolve_model_aliases` falls through to a refusal instead
/// of a resolution, never to a WRONG one).
fn attr_accessor_arg_names(expr: &Expr) -> Vec<Symbol> {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
        return Vec::new();
    };
    if !matches!(method.as_str(), "attr_accessor" | "attr_reader" | "attr_writer") {
        return Vec::new();
    }
    args.iter()
        .filter_map(|a| match &*a.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// A forwarder for `old` when it's a known ActiveRecord::Base instance
/// method (`destroy!`, `save!`, `touch`, …) the model's own body
/// doesn't redefine. See `resolve_model_aliases`'s doc for why concern
/// redefinitions can't be checked here.
fn inherited_forwarder(model: &Model, old: &Symbol) -> Option<MethodDef> {
    crate::catalog::lookup(old.as_str(), crate::catalog::ReceiverContext::Instance)?;
    if model_defines_instance_method(model, old) {
        return None;
    }
    Some(MethodDef {
        name: old.clone(), // the caller renames to `new`
        receiver: MethodReceiver::Instance,
        visibility: crate::dialect::MethodVisibility::Public,
        params: Vec::new(),
        unsupported_formals: None,
        has_anonymous_block: false,
        block_param: None,
        name_span: Span::synthetic(),
        body: Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: None,
                method: old.clone(),
                args: Vec::new(),
                block: None,
                parenthesized: false,
            },
        ),
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: true,
    })
}

/// Refuse an alias roundhouse can't resolve — same shape as any other
/// unsupported construct (`Error` severity; survey mode's
/// `attribute_analysis_gaps` demotes it to a gap). `construct` is
/// `alias_target_unresolved` so that demotion can find it by name.
fn push_alias_target_unresolved_gap(
    span: Span,
    model: &Model,
    old: &Symbol,
    new: &Symbol,
    table: Option<&Table>,
) {
    let is_column = table.is_some_and(|t| t.columns.iter().any(|c| c.name == *old));
    let detail = if is_column {
        format!(
            "alias {new} {old} names a column — Rails raises NameError here too \
             (attribute methods are generated lazily); use `alias_attribute :{new}, :{old}` instead"
        )
    } else {
        format!(
            "alias {new} {old} on {}: {old} is not defined (not a hand-written method, \
             enum method, association, attr_accessor, or inherited ActiveRecord method)",
            model.name.0
        )
    };
    crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
        span,
        None,
        "alias_target_unresolved",
        detail,
    ));
}

/// How many times `body` reads the local `name`.
fn count_var_reads(body: &mut Expr, name: &Symbol) -> usize {
    let mut n = 0;
    if let ExprNode::Var { name: v, .. } = &*body.node {
        if v == name {
            return 1;
        }
    }
    body.node.for_each_child_mut(&mut |child| n += count_var_reads(child, name));
    n
}

/// Replace every read of the scope's threaded relation `name` that roots
/// a chain with `name.spawn()`, so each chain in the body starts from its
/// own copy. A read passed straight to a call as an argument (the
/// relation threaded into another scope, `Model.other(__rel)`) is left
/// bare: that callee spawns on entry, and with every chain root spawned
/// nothing has mutated `__rel` by the time it is read.
fn spawn_each_var_read(body: &mut Expr, name: &Symbol, model: &ClassId) {
    if let ExprNode::Send { recv, args, block, .. } = &mut *body.node {
        if let Some(r) = recv {
            spawn_each_var_read(r, name, model);
        }
        for a in args.iter_mut() {
            if !matches!(&*a.node, ExprNode::Var { name: v, .. } if v == name) {
                spawn_each_var_read(a, name, model);
            }
        }
        if let Some(b) = block {
            spawn_each_var_read(b, name, model);
        }
        return;
    }
    if let ExprNode::Var { name: v, .. } = &*body.node {
        if v == name {
            let rel_ty = Ty::Relation { of: model.clone() };
            let mut var = body.clone();
            var.ty = Some(rel_ty.clone());
            let mut spawn = Expr::new(
                body.span,
                ExprNode::Send {
                    recv: Some(var),
                    method: Symbol::from("spawn"),
                    args: vec![],
                    block: None,
                    parenthesized: true,
                },
            );
            spawn.ty = Some(rel_ty);
            *body = spawn;
            return;
        }
    }
    body.node.for_each_child_mut(&mut |child| spawn_each_var_read(child, name, model));
}

pub(crate) fn push_scope_methods(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    scopes: &crate::lower::scope_chain::ScopeRegistry,
    models_set: &crate::det::HashSet<ClassId>,
    assocs: &crate::lower::scope_chain::AssocRegistry,
) {
    use crate::dialect::{AccessorKind, ModelBodyItem, Param};
    use crate::expr::{Expr, ExprNode, LValue};
    use crate::ty::Ty;
    let rel_param = Symbol::from("__rel");
    for item in &model.body {
        let ModelBodyItem::Scope { scope, .. } = item else { continue };
        let mut params = scope.params.clone();
        // `__rel` is an optional POSITIONAL param — it must precede any
        // keyword params in the def (`def recent(user = nil, __rel = …,
        // unmerged: true)`), or the signature is a syntax error.
        //
        // A REST param (`*tags`) is the same exception `insert_rel_param`
        // (the emit-side twin of this function, for user-written
        // relation-taking class methods) has: an optional positional
        // can't follow a splat, so `__rel` goes in as an optional
        // KEYWORD instead, ahead of any `**opts`. `parse_scope` drops a
        // scope lambda's splat today, so `scope.params` never actually
        // carries a rest param yet — this branch is dead until that
        // changes (a separate, later fix) — but kept here so the two
        // insertion sites stay consistent rather than silently diverging
        // the day it does.
        if params.iter().any(|p| p.rest && !p.keyword) {
            let at = params.iter().position(|p| p.rest && p.keyword).unwrap_or(params.len());
            params.insert(at, Param::keyword(rel_param.clone(), Some(relation_new_self())));
        } else {
            let insert_at = params
                .iter()
                .position(|p| p.keyword)
                .unwrap_or(params.len());
            params.insert(
                insert_at,
                Param::with_default(rel_param.clone(), relation_new_self()),
            );
        }

        let mut body = scope.body.clone();
        crate::lower::scope_chain::rewrite_scope_body(
            &mut body,
            &model.name,
            &rel_param,
            scopes,
            models_set,
            assocs,
        );

        // Rails relations are immutable, so every implicit-self root in
        // a scope body starts from the scope as it was on entry:
        // `where(a).or(where(b))` is `a OR b`. Ours mutate in place, and
        // the rewrite roots each of those on the one threaded `__rel`,
        // so the receiver's `where(a)` landed on the argument too and
        // the query became `(a AND b) OR (a AND b)`. When the body reads
        // the relation more than once, each read takes its own copy.
        // (A single read keeps the bare `__rel`; the entry spawn below
        // already protects the caller's relation.)
        if count_var_reads(&mut body, &rel_param) > 1 {
            spawn_each_var_read(&mut body, &rel_param, &model.name);
        }

        // Rails scopes spawn on entry: `rel.visible.with_direct_rooms`
        // must not leave joins/orders on `rel.visible` for a sibling
        // `rel.visible.with_ordered_room`. Our chain methods mutate in
        // place, so the scope method itself takes a copy first.
        let span = body.span;
        let rel_ty = Ty::Relation {
            of: model.name.clone(),
        };
        let mut rel_var = Expr::new(
            span,
            ExprNode::Var {
                id: crate::ident::VarId(0),
                name: rel_param.clone(),
            },
        );
        rel_var.ty = Some(rel_ty.clone());
        let mut spawn_send = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(rel_var),
                method: Symbol::from("spawn"),
                args: vec![],
                block: None,
                parenthesized: true,
            },
        );
        // `Relation#spawn` returns a Relation of the same model.
        spawn_send.ty = Some(rel_ty);
        let spawn_assign = Expr::new(
            span,
            ExprNode::Assign {
                target: LValue::Var {
                    id: crate::ident::VarId(0),
                    name: rel_param.clone(),
                },
                value: spawn_send,
            },
        );
        body = Expr::new(
            span,
            ExprNode::Seq {
                exprs: vec![spawn_assign, body],
            },
        );

        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: scope.name.clone(),
            receiver: MethodReceiver::Class,
            params,
            body,
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// Fixed-full-arity relation entry points for every registered scope /
/// relation-taking class method on a model:
///
///   def self.__scope_<name>__<k>(__rel, p0..p<k-1>)          # k supplied positionals
///   def self.__scope_<name>__<k>__kw_<names>(__rel, …, kw:)  # + a keyword subset
///
/// Each body forwards to the primary scope method with the omitted
/// optional positionals padded from the primary's OWN default
/// expressions — cloned here, inside the model class, where those
/// expressions still mean what the author wrote (`saved`'s
/// `exclude_tags = []`, constant reads, …). The generated
/// `ActiveRecord::Relation` delegates (`emit_relation_scope_delegates`)
/// dispatch mid-chain calls to these instead of the primary because the
/// primary's `__rel` sits AFTER the optional positionals (unreachable
/// without supplying them) and because spinel's class-value dispatch
/// requires each call to supply a method's exact full positional arity:
/// it neither defaults omitted optionals through the dispatch nor binds
/// kwargs correctly past an omitted optional. A fixed-arity entry per
/// supplied shape sidesteps all of it. Ruby-emit seam only, same as the
/// scope methods themselves; invoked AFTER all scope-chain rewriting so
/// these bodies are never re-threaded.
pub(crate) fn push_scope_variants(
    methods: &mut Vec<MethodDef>,
    model_name: &ClassId,
    registered: &crate::det::HashMap<Symbol, Vec<crate::dialect::Param>>,
) {
    use crate::dialect::{AccessorKind, Param};
    use crate::lower::scope_chain::{delegable_name, scope_variant_name, DelegableShape};
    let rel = Symbol::from("__rel");
    let mut names: Vec<&Symbol> = registered.keys().collect();
    names.sort_by_key(|n| n.as_str());
    for name in names {
        if !delegable_name(name) {
            continue;
        }
        let Some(shape) = DelegableShape::of(&registered[name]) else { continue };
        for k in shape.min_required..=shape.positionals.len() {
            for subset in shape.keyword_subsets() {
                let vname = scope_variant_name(name, k, &subset);
                if methods
                    .iter()
                    .any(|m| m.name == vname && m.receiver == MethodReceiver::Class)
                {
                    continue;
                }
                let mut params = vec![Param::positional(rel.clone())];
                for p in &shape.positionals[..k] {
                    params.push(Param::positional(p.name.clone()));
                }
                for p in &subset {
                    params.push(Param::keyword(p.name.clone(), None));
                }
                let mut args: Vec<Expr> = shape.positionals[..k]
                    .iter()
                    .map(|p| var_ref(p.name.clone()))
                    .collect();
                for p in &shape.positionals[k..] {
                    args.push(p.default.clone().expect("optional tail past min_required"));
                }
                args.push(var_ref(rel.clone()));
                if !subset.is_empty() {
                    let entries = subset
                        .iter()
                        .map(|p| {
                            (
                                Expr::new(
                                    Span::synthetic(),
                                    ExprNode::Lit {
                                        value: crate::expr::Literal::Sym { value: p.name.clone() },
                                    },
                                ),
                                var_ref(p.name.clone()),
                            )
                        })
                        .collect();
                    args.push(Expr::new(
                        Span::synthetic(),
                        ExprNode::Hash { entries, kwargs: true },
                    ));
                }
                let body = Expr::new(
                    Span::synthetic(),
                    ExprNode::Send {
                        recv: Some(class_const(model_name)),
                        method: name.clone(),
                        args,
                        block: None,
                        parenthesized: true,
                    },
                );
                // The signature types `__rel` as the runtime Relation
                // class — it IS one by construction, and this is
                // load-bearing under spinel's --rbs seed: an `untyped
                // __rel` here widens the primaries' `__rel` and every
                // `__rel.order(…)` receiver inside scope bodies to
                // untyped, and spinel's untyped-receiver path drops a
                // kwargs hash into `*rest` (the #3503 shape — /newest
                // lost its ORDER BY and the AOT lane lost parity).
                // User params and the return stay honest untyped.
                let mut sig_params = vec![crate::ty::Param {
                    name: rel.clone(),
                    ty: Ty::Class {
                        id: ClassId(Symbol::from("ActiveRecord::Relation")),
                        args: vec![].into(),
                    }.into(),
                    kind: crate::ty::ParamKind::Required,
                }];
                for p in &shape.positionals[..k] {
                    sig_params.push(crate::ty::Param {
                        name: p.name.clone(),
                        ty: Ty::Untyped.into(),
                        kind: crate::ty::ParamKind::Required,
                    });
                }
                for p in &subset {
                    sig_params.push(crate::ty::Param {
                        name: p.name.clone(),
                        ty: Ty::Untyped.into(),
                        kind: crate::ty::ParamKind::Keyword { required: true },
                    });
                }
                methods.push(MethodDef {
                    visibility: crate::dialect::MethodVisibility::Public,
                    unsupported_formals: None,
                    has_anonymous_block: false,
                    name_span: crate::span::Span::synthetic(),
                    name: vname,
                    receiver: MethodReceiver::Class,
                    params,
                    body,
                    signature: Some(Ty::Fn {
                        params: sig_params.into(),
                        block: None,
                        ret: std::sync::Arc::new(Ty::Untyped),
                        effects: crate::effect::EffectSet::default(),
                    }),
                    effects: crate::effect::EffectSet::default(),
                    enclosing_class: Some(model_name.0.clone()),
                    kind: AccessorKind::Method,
                    is_async: false,
                    mutates_self: false,
                    block_param: None,
                });
            }
        }
    }
}

/// `ActiveRecord::Relation.new(self)` — the default relation a scope class
/// method starts from when called on the class rather than chained.
pub(crate) fn relation_new_self() -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const {
                    path: vec![Symbol::from("ActiveRecord"), Symbol::from("Relation")],
                },
            )),
            method: Symbol::from("new"),
            args: vec![Expr::new(Span::synthetic(), ExprNode::SelfRef)],
            block: None,
            parenthesized: true,
        },
    )
}

/// Construct the `ClassInfo` for a lowered model: schema-derived
/// attribute row, plus instance/class method tables built from the
/// synthesized `MethodDef.signature`s and an ApplicationRecord
/// baseline (save / destroy / persisted? / errors / find / all /
/// where / count / exists? / find_by / destroy_all).
pub(crate) fn build_class_info(
    model: &Model,
    methods: &[MethodDef],
    table: Option<&Table>,
) -> crate::analyze::ClassInfo {
    build_class_info_with_finder_inputs(model, methods, table, FinderInputs::Scalar)
}

/// Match the public finder contract to the runtime selected by its caller.
fn build_class_info_with_finder_inputs(
    model: &Model,
    methods: &[MethodDef],
    table: Option<&Table>,
    finder_inputs: FinderInputs,
) -> crate::analyze::ClassInfo {
    let mut info = crate::analyze::ClassInfo::default();
    info.table = Some(model.table.clone());
    info.parent = model.parent.clone();
    info.default_scoped = crate::lower::default_scope::declares(model);

    // Attributes row from schema columns.
    if let Some(t) = table {
        let mut row = Row::closed();
        for col in &t.columns {
            // The SLOT type — a nullable column's ivar genuinely holds
            // nil until something sets it, and the strict targets need
            // that in the attributes row: Crystal's auto-`.not_nil!`
            // ivar bridge, for one, keys off this type and would
            // otherwise assert non-nil on a column that is routinely
            // NULL. A retypable column is typed gradually (`schema::is_retypable`).
            let ty = if schema::is_retypable(model, col) { Ty::Untyped } else { ty_of_column_slot(col) };
            row.fields.insert(col.name.clone(), ty);
        }
        info.attributes = row;
    }

    // Explicit signatures are authoritative. Precise source-body returns
    // are filled below, after semantic scope/relation classification has
    // had first refusal for otherwise unsigned methods.
    for m in methods {
        if let Some(sig) = &m.signature {
            match m.receiver {
                MethodReceiver::Instance => {
                    info.instance_methods.insert(m.name.clone(), sig.clone());
                    info.instance_method_kinds.insert(m.name.clone(), m.kind);
                }
                MethodReceiver::Class => {
                    info.class_methods.insert(m.name.clone(), sig.clone());
                    info.class_method_kinds.insert(m.name.clone(), m.kind);
                }
            }
        }
    }

    // Scopes. `push_scope_methods` runs only at the ruby emit seam and
    // leaves `signature: None`, so the loop above cannot see them — and
    // this registry is what types TEST bodies. Without an entry the
    // chain dies at the scope hop: `users(:david).rooms.opens.last`
    // typed to nothing, the route-helper id projection declined, and
    // campfire's rooms tests asserted a redirect to
    // `/rooms/#<Room:0x000000010…>`. Uses the analyzer's own seed rule
    // rather than a second copy of it.
    {
        let scope_names: crate::det::HashSet<Symbol> =
            model.scopes().map(|s| s.name.clone()).collect();
        for scope in model.scopes() {
            info.class_methods.entry(scope.name.clone()).or_insert_with(|| {
                crate::analyze::scope_return_seed(&scope.body, &model.name, &scope_names)
            });
            info.class_method_kinds
                .entry(scope.name.clone())
                .or_insert(crate::dialect::AccessorKind::Method);
            info.relation_derived.insert(scope.name.clone());
        }
        // Hand-written class methods whose body IS a query over this
        // model. campfire's `class << self; def original;
        // order(:created_at).first; end; end` is the shape, reached as
        // `Current.user.rooms.original`. The synthesized-method loop
        // above cannot see these (they come from the model body, not
        // from `build_methods`), and the analyzer's registry — which
        // does harvest them — is not the one that types test bodies.
        for item in &model.body {
            let crate::dialect::ModelBodyItem::Method { method, .. } = item else { continue };
            if method.receiver != crate::dialect::MethodReceiver::Class {
                continue;
            }
            let seed = crate::analyze::scope_return_seed(&method.body, &model.name, &scope_names);
            // `scope_return_seed`'s fallback is `Array[Self]`, which for
            // a method that is NOT a query would be a fabrication. Only
            // register when the body actually classified.
            if !crate::analyze::body_is_relation_query(&method.body, &model.name, &scope_names) {
                continue;
            }
            info.class_methods.entry(method.name.clone()).or_insert(seed);
            info.class_method_kinds
                .entry(method.name.clone())
                .or_insert(crate::dialect::AccessorKind::Method);
            info.relation_derived.insert(method.name.clone());
        }
    }

    // Last-resort record returns from the source analyzer. Keep this
    // after semantic seeding: a body's stale annotation must not prevent
    // scope_return_seed from recording Relation (or a terminal result).
    // Preserve parent-helper record identity, not arbitrary container
    // annotations: retyping a raw Hash can repeat already-lowered key
    // coercions. A raw Fn would likewise be mistaken by unwrap_fn_ret
    // for this method's own signature instead of its returned callable.
    for m in methods {
        if m.signature.is_some() || contains_return(&m.body) {
            continue;
        }
        let Some(inferred) = m
            .body
            .ty
            .as_ref()
            .filter(|ty| match ty {
                Ty::Class { .. } => true,
                Ty::Union { variants } => variants.iter().any(|ty| matches!(ty, Ty::Class { .. }))
                    && variants.iter().all(|ty| matches!(ty, Ty::Class { .. } | Ty::Nil)),
                // An Array of scalars carries no key coercion or
                // record identity to repeat, and without it a test's
                // `assert_equal [2, 4], Model.evens` reads an untyped
                // call: the `.to_a` the assertion lowering adds stays
                // a dynamic send no typed target defines on its list.
                Ty::Array { elem } => matches!(
                    **elem,
                    Ty::Int | Ty::Float | Ty::Str | Ty::Sym | Ty::Bool
                ),
                _ => false,
            })
        else {
            continue;
        };
        let (method_map, kind_map) = match m.receiver {
            MethodReceiver::Instance => {
                (&mut info.instance_methods, &mut info.instance_method_kinds)
            }
            MethodReceiver::Class => (&mut info.class_methods, &mut info.class_method_kinds),
        };
        method_map
            .entry(m.name.clone())
            .or_insert_with(|| Ty::Fn {
                // Retain the calling convention too: a positional Hash
                // default must still normalize keyword syntax into a Hash.
                // Unsigned methods retain default types, not call-site seeds.
                params: m.params.iter().map(|p| crate::ty::Param {
                    name: p.name.clone(),
                    ty: p.default.as_ref().and_then(|d| d.ty.clone()).unwrap_or(Ty::Untyped).into(),
                    kind: p.ty_kind(),
                }).collect(),
                block: (m.block_param.is_some() || m.has_anonymous_block)
                    .then(|| std::sync::Arc::new(Ty::Untyped)),
                ret: std::sync::Arc::new(inferred.clone()),
                effects: m.effects.clone(),
            });
        kind_map.entry(m.name.clone()).or_insert(m.kind);
    }

    // ApplicationRecord baseline (subset of runtime/ruby/active_record/base.rb's
    // public API that synthesized model bodies actually call). Only insert
    // when not already overridden by the lowerer.
    let class_id = &model.name;
    let owner_ty = Ty::Class { id: class_id.clone(), args: vec![].into() };
    let owner_or_nil = Ty::Union {
        variants: vec![owner_ty.clone(), Ty::Nil].into(),
    };
    let array_owner = Ty::Array { elem: std::sync::Arc::new(owner_ty.clone()) };
    let any_hash = Ty::Hash { key: std::sync::Arc::new(Ty::Sym), value: std::sync::Arc::new(Ty::Untyped) };

    // ApplicationRecord declares `id` and `id=` (the schema synthesizers
    // skip the id column because it's inherited from the base class).
    // Their type is the primary key's: `Ty::Int` for Rails' default
    // bigint, `Ty::Str` for a `t.uuid` / `id: :string` key, and every
    // adapter primitive that takes or answers a key follows it. Public
    // `find` accepts raw inputs until shared casting; `thing.id` and the
    // adapter boundary remain schema-typed for uuid keys (#90). The RUNTIME write path still reads
    // `last_insert_rowid`; that half stays ledgered at ingest.
    let key_ty = primary_key_ty(model, table);
    insert_default(&mut info.instance_methods, "id", fn_sig(vec![], key_ty.clone()));
    insert_default(
        &mut info.instance_methods,
        "id=",
        fn_sig(vec![(Symbol::from("value"), key_ty.clone())], key_ty.clone()),
    );
    insert_default(&mut info.instance_methods, "save", fn_sig(vec![], Ty::Bool));
    // `save!` answers the RECORD, not a bool: `runtime/ruby/active_record/
    // base.rb` raises on failure and returns `self`, its sidecar declares
    // `save!: () -> Base`, and the catalog types it `ReturnKind::SelfType`.
    // Declaring Bool here made every method whose tail is `x.save!` carry a
    // `-> bool` signature its own body contradicts — invisible under CRuby,
    // a refused AOT build once spinel judged return seeds (matz/spinel#4005):
    // lobsters' `Domain#ban_by_user_for_reason!`, `#unban_by_user_for_reason!`
    // and `SavedStory.save_story_for_user` all ended in a `save!`.
    insert_default(&mut info.instance_methods, "save!", fn_sig(vec![], owner_ty.clone()));
    insert_default(&mut info.instance_methods, "destroy", fn_sig(vec![], owner_ty.clone()));
    insert_default(&mut info.instance_methods, "destroyed?", fn_sig(vec![], Ty::Bool));
    insert_default(&mut info.instance_methods, "persisted?", fn_sig(vec![], Ty::Bool));
    insert_default(
        &mut info.instance_methods,
        "mark_persisted!",
        fn_sig(vec![], Ty::Nil),
    );
    // `errors` returns `Array[String]` — matches both the framework
    // Ruby (`runtime/ruby/active_record/base.rb`, where `@errors` is
    // declared `Array[String]` in the `.rbs` sidecar) and every
    // validation-rule push site (every `errors << "..."` arm in
    // `validations.rs` shoves a String literal). The earlier
    // `Array[untyped]` predated the RBS sidecar; widening was load-
    // bearing for nothing. Tightening to `Str` lets rust's `<<` emit
    // pick the Vec<String>-shaped push coercion uniformly.
    insert_default(
        &mut info.instance_methods,
        "errors",
        fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
    );
    insert_default(
        &mut info.instance_methods,
        "error_details",
        fn_sig(vec![], crate::lower::errors_add::error_details_ty()),
    );
    insert_default(
        &mut info.instance_methods,
        "error_messages_for",
        fn_sig(vec![(Symbol::from("attribute"), Ty::Str)], Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
    );
    insert_default(&mut info.instance_methods, "valid?", fn_sig(vec![], Ty::Bool));
    insert_default(
        &mut info.instance_methods,
        "reload",
        fn_sig(vec![], owner_ty.clone()),
    );

    // Validations mixin — instance helpers expected on every record.
    insert_default(
        &mut info.instance_methods,
        "validates_presence_of",
        fn_sig(vec![(Symbol::from("attr"), Ty::Sym), (Symbol::from("value"), Ty::Untyped)], Ty::Nil),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_absence_of",
        fn_sig(vec![(Symbol::from("attr"), Ty::Sym), (Symbol::from("value"), Ty::Untyped)], Ty::Nil),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_length_of",
        fn_sig(
            vec![
                (Symbol::from("attr"), Ty::Sym),
                (Symbol::from("value"), Ty::Untyped),
                (Symbol::from("opts"), any_hash.clone()),
            ],
            Ty::Nil,
        ),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_format_of",
        fn_sig(
            vec![
                (Symbol::from("attr"), Ty::Sym),
                (Symbol::from("value"), Ty::Untyped),
                (Symbol::from("opts"), any_hash.clone()),
            ],
            Ty::Nil,
        ),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_numericality_of",
        fn_sig(
            vec![
                (Symbol::from("attr"), Ty::Sym),
                (Symbol::from("value"), Ty::Untyped),
                (Symbol::from("opts"), any_hash.clone()),
            ],
            Ty::Nil,
        ),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_inclusion_of",
        fn_sig(
            vec![
                (Symbol::from("attr"), Ty::Sym),
                (Symbol::from("value"), Ty::Untyped),
                (Symbol::from("opts"), any_hash.clone()),
            ],
            Ty::Nil,
        ),
    );
    insert_default(
        &mut info.instance_methods,
        "validates_belongs_to",
        fn_sig(
            vec![
                (Symbol::from("attr"), Ty::Sym),
                (Symbol::from("fk_value"), Ty::Int),
                (Symbol::from("target_class"), Ty::Untyped),
            ],
            Ty::Nil,
        ),
    );

    // Only the Ruby-family connection runtime owns raw request casting.
    // Other targets retain their schema scalar argument and call coercions.
    let find_input = if finder_inputs == FinderInputs::Request {
        finder_input_ty(&key_ty)
    } else {
        key_ty.clone()
    };
    insert_default(
        &mut info.class_methods,
        "find",
        fn_sig(vec![(Symbol::from("id"), find_input)], owner_ty.clone()),
    );
    insert_default(
        &mut info.class_methods,
        "find_by",
        fn_sig(vec![(Symbol::from("attrs"), any_hash.clone())], owner_or_nil),
    );
    insert_default(
        &mut info.class_methods,
        "all",
        fn_sig(vec![], array_owner.clone()),
    );
    insert_default(
        &mut info.class_methods,
        "where",
        fn_sig(vec![(Symbol::from("conditions"), any_hash.clone())], array_owner.clone()),
    );
    insert_default(
        &mut info.class_methods,
        "count",
        fn_sig(vec![], Ty::Int),
    );
    let exists_input = if finder_inputs == FinderInputs::Request {
        finder_input_ty(&key_ty)
    } else {
        key_ty.clone()
    };
    insert_default(
        &mut info.class_methods,
        "exists?",
        fn_sig(vec![(Symbol::from("id"), exists_input)], Ty::Bool),
    );
    insert_default(
        &mut info.class_methods,
        "destroy_all",
        fn_sig(vec![], Ty::Int),
    );
    insert_default(
        &mut info.class_methods,
        "first",
        fn_sig(vec![], Ty::Union { variants: vec![owner_ty.clone(), Ty::Nil].into() }),
    );
    insert_default(
        &mut info.class_methods,
        "last",
        fn_sig(vec![], Ty::Union { variants: vec![owner_ty.clone(), Ty::Nil].into() }),
    );
    insert_default(
        &mut info.class_methods,
        "take",
        fn_sig(vec![], Ty::Union { variants: vec![owner_ty.clone(), Ty::Nil].into() }),
    );
    insert_default(
        &mut info.class_methods,
        "new",
        fn_sig(vec![(Symbol::from("attrs"), any_hash.clone())], owner_ty.clone()),
    );
    // `<Model>.create(attrs)` → instance; `<Model>.create!(attrs)`
    // raises on validation failure but returns instance otherwise.
    // Both registered so test bodies (which use the bang form) and
    // seeds (which my has-many rewrite produces) type cleanly
    // through the registry.
    insert_default(
        &mut info.class_methods,
        "create",
        fn_sig(vec![(Symbol::from("attrs"), any_hash.clone())], owner_ty.clone()),
    );
    insert_default(
        &mut info.class_methods,
        "create!",
        fn_sig(vec![(Symbol::from("attrs"), any_hash)], owner_ty.clone()),
    );

    // Per-model Level-3 adapter primitives — the typed factories the
    // lowerer emits in `adapter_emit.rs`. Registered at the Base level so
    // public `Base#find/all/save/destroy/...` dispatch through `self.
    // _adapter_X` typed against the receiver class; per-model emitted
    // bodies override. Underscore-prefix signals framework-internal; not
    // part of the public AR API.
    insert_default(
        &mut info.class_methods,
        "_adapter_find_by_id",
        fn_sig(
            vec![(Symbol::from("id"), key_ty.clone())],
            Ty::Union { variants: vec![owner_ty.clone(), Ty::Nil].into() },
        ),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_all",
        fn_sig(vec![], Ty::Array { elem: std::sync::Arc::new(owner_ty.clone()) }),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_insert",
        fn_sig(vec![(Symbol::from("instance"), owner_ty.clone())], key_ty.clone()),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_update",
        fn_sig(
            vec![
                (Symbol::from("id"), key_ty.clone()),
                (Symbol::from("instance"), owner_ty.clone()),
            ],
            Ty::Nil,
        ),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_delete",
        fn_sig(vec![(Symbol::from("id"), key_ty.clone())], Ty::Nil),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_count",
        fn_sig(vec![], Ty::Int),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_any?",
        fn_sig(vec![], Ty::Bool),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_exists_by_id?",
        fn_sig(vec![(Symbol::from("id"), key_ty.clone())], Ty::Bool),
    );
    insert_default(
        &mut info.class_methods,
        "_adapter_truncate",
        fn_sig(vec![], Ty::Nil),
    );
    insert_default(
        &mut info.class_methods,
        "delete_all",
        fn_sig(vec![], Ty::Nil),
    );
    // The Relation load path (`Relation#to_a`): the model's qualified
    // column list, and a typed multi-hydrate over a caller-composed
    // SELECT that projects exactly that list. Base carries Hash-path
    // defaults; the per-model emit overrides both.
    insert_default(&mut info.class_methods, "_columns_sql", fn_sig(vec![], Ty::Str));
    insert_default(
        &mut info.class_methods,
        "_hydrate_all",
        fn_sig(
            vec![(Symbol::from("sql"), Ty::Str)],
            Ty::Array { elem: std::sync::Arc::new(owner_ty.clone()) },
        ),
    );

    // Typed factory taking the synthesized `<Model>Row` (one typed slot
    // per schema column). The body-typer needs this signature to resolve
    // `Article.from_row(row_value)` calls cross-class — `synth_from_row`
    // installs the body, but the registry entry has to exist before the
    // body of any caller is typed.
    let row_class_id = self::row::row_class_id(&model.name);
    insert_default(
        &mut info.class_methods,
        "from_row",
        fn_sig(
            vec![(Symbol::from("row"), Ty::Class { id: row_class_id, args: vec![].into() })],
            owner_ty.clone(),
        ),
    );

    // Positional twin of `from_row` (`synth_from_stmt`): hydrates from a
    // prepared-statement handle (`stmt : Int`). Registered so the Arel
    // visitor's `<Model>.from_stmt(stmt)` hydrate calls — same-class in
    // the adapter methods, cross-class in eager-load preloads — type
    // before their callers' bodies are walked.
    insert_default(
        &mut info.class_methods,
        "from_stmt",
        fn_sig(vec![(Symbol::from("stmt"), Ty::Int)], owner_ty),
    );

    // Tag every entry the baseline added as Method (defaults match —
    // ApplicationRecord's `save`, `find`, `where`, etc. are all real
    // method calls with parens). The earlier loop over synthesized
    // methods already populated `*_method_kinds` from the per-method
    // `kind` field; this fills in any baseline names that weren't
    // overridden.
    use crate::dialect::AccessorKind;
    for name in info.class_methods.keys().cloned().collect::<Vec<_>>() {
        info.class_method_kinds.entry(name).or_insert(AccessorKind::Method);
    }
    for name in info.instance_methods.keys().cloned().collect::<Vec<_>>() {
        info.instance_method_kinds.entry(name).or_insert(AccessorKind::Method);
    }
    // Baseline names that are field-like on the transpiled framework
    // Base/Validations (backed by `@<name>` ivars set in
    // `initialize`) — override the Method default so the body
    // analyzer's force-parens skips them. Callers reading
    // `record.errors` need the field, not a method call.
    for field_name in ["errors", "error_details", "id"] {
        info.instance_method_kinds
            .insert(Symbol::from(field_name), AccessorKind::AttributeReader);
    }
    info
}

/// The type of a model's key as the emitted adapter binds it: the
/// schema column marked `primary_key: true` (`id`, or the column
/// `create_table primary_key:` names), else Rails' default integer.
/// The SCHEMA decides rather than `Model::primary_key`, because the
/// synthesized `_adapter_*` bodies bind that column — lobsters'
/// `Keystore` (`self.primary_key = "key"` over a table that still has
/// an integer `id`) keeps its integer `id` here, which is the column
/// those bodies read; its `key` identity is served by `find_by` and
/// the upsert conflict target, not by `find`.
pub(super) fn primary_key_column(table: &Table) -> Option<&crate::schema::Column> {
    table.columns.iter().find(|c| c.primary_key)
}

fn primary_key_ty(_model: &Model, table: Option<&Table>) -> Ty {
    table
        .and_then(primary_key_column)
        .map(|c| ty_of_column(&c.col_type))
        .unwrap_or(Ty::Int)
}

fn insert_default(map: &mut HashMap<Symbol, Ty>, name: &str, sig: Ty) {
    map.entry(Symbol::from(name)).or_insert(sig);
}

/// Stub `ClassInfo` for the `Broadcasts` framework module. Each
/// helper takes a kwargs hash and returns Nil (per the Ruby
/// runtime). Only carrying signatures the model lowerer's
/// broadcasts expansion + block-form callbacks actually emit.
fn broadcasts_class_info() -> crate::analyze::ClassInfo {
    let mut info = crate::analyze::ClassInfo::default();
    // `Broadcasts.{append,prepend,replace,remove}` takes a kwargs bag —
    // declared as `**opts` in Ruby/Spinel (Hash-collected), as named
    // params in Crystal, as an options object in TS. Tag the signature
    // param as `KeywordRest` so the body-typer's
    // `normalize_trailing_kwargs` pass leaves the trailing `kwargs:
    // true` Hash alone (it renders as bare named-args at the call
    // site, which every target dispatches correctly: Ruby kwargs →
    // Hash, Crystal NamedTuple → named params, TS object literal).
    use crate::dialect::AccessorKind;
    let opts_ty = Ty::Hash { key: std::sync::Arc::new(Ty::Sym), value: std::sync::Arc::new(Ty::Untyped) };
    let sig = Ty::Fn {
        params: vec![crate::ty::Param {
            name: Symbol::from("opts"),
            ty: opts_ty.into(),
            kind: crate::ty::ParamKind::KeywordRest,
        }].into(),
        block: None,
        ret: std::sync::Arc::new(Ty::Nil),
        effects: crate::effect::EffectSet::pure(),
    };
    for name in ["prepend", "replace", "remove", "append"] {
        info.class_methods.insert(Symbol::from(name), sig.clone());
        info.class_method_kinds.insert(Symbol::from(name), AccessorKind::Method);
    }
    info
}

/// Broaden only keys handled by shared normalization; preserve other adapters'
/// existing input contracts rather than assigning them integer casting rules.
pub(super) fn finder_input_ty(key: &Ty) -> Ty {
    if matches!(key, Ty::Int | Ty::Str) {
        Ty::Union { variants: vec![Ty::Int, Ty::Float, Ty::Str, Ty::Nil].into() }
    } else {
        key.clone()
    }
}

/// The schema-selected wrapper calls the shared Ruby normalization result.
/// Keep this registry aligned with active_record/connection.rbs; no cast rules live here.
fn insert_integer_key_cast_info(classes: &mut HashMap<ClassId, crate::analyze::ClassInfo>) {
    let id = ClassId(Symbol::from("ActiveRecord::IntegerKeyCast"));
    let mut info = crate::analyze::ClassInfo::default();
    let input = Ty::Union { variants: vec![Ty::Int, Ty::Float, Ty::Str, Ty::Nil].into() };
    info.class_methods.insert(Symbol::from("parse"), fn_sig(
        vec![(Symbol::from("id"), input.clone())], Ty::Class { id: id.clone(), args: vec![].into() },
    ));
    info.class_methods.insert(Symbol::from("input_text"), fn_sig(
        vec![(Symbol::from("id"), input)], Ty::Str,
    ));
    info.instance_methods.insert(Symbol::from("valid"), fn_sig(vec![], Ty::Bool));
    info.instance_methods.insert(Symbol::from("value"), fn_sig(vec![], Ty::Int));
    classes.insert(id, info);
}

fn type_method_body(
    method: &mut MethodDef,
    classes: &HashMap<ClassId, crate::analyze::ClassInfo>,
    table: Option<&Table>,
    model: Option<&Model>,
) {
    let typer = crate::analyze::BodyTyper::new(classes);
    let mut ctx = crate::analyze::Ctx::default();
    ctx.enclosing_def = Some((
        method.name.clone(),
        matches!(method.receiver, crate::dialect::MethodReceiver::Class),
    ));
    if let Some(Ty::Fn { params, .. }) = &method.signature {
        for (param, sig) in method.params.iter().zip(params.iter()) {
            ctx.local_bindings.insert(param.name.clone(), (*sig.ty).clone());
        }
    }
    if let Some(enclosing) = &method.enclosing_class {
        ctx.self_ty = Some(Ty::Class {
            id: ClassId(enclosing.clone()),
            args: vec![].into(),
        });
    }
    // Seed ivar_bindings from schema columns so bare `@title` reads
    // resolve to the column type. Same source the synthesizers used
    // for the fields themselves. MODEL classes store temporal columns
    // under `@<col>_raw` (see `schema::col_storage_name`), so their
    // bodies are seeded with storage names; Row classes keep plain
    // column-named ivars (no storage/accessor split — they're the raw
    // transport) and are distinguished by `model: None`.
    if matches!(method.receiver, MethodReceiver::Instance) {
        if let Some(t) = table {
            for col in &t.columns {
                let ivar_name = if model.is_some() {
                    self::schema::col_storage_name(col)
                } else {
                    col.name.clone()
                };
                // Slot type: a nullable column's ivar holds nil until
                // something sets it. Model classes only — a Row is the
                // raw transport whose fields the adapter always fills.
                let ivar_ty = if model.is_some() {
                    ty_of_column_slot(col)
                } else {
                    ty_of_column(&col.col_type)
                };
                ctx.ivar_bindings.insert(ivar_name, ivar_ty);
            }
        }
        // has_many eager-load cache ivars (`@<assoc>_cache` /
        // `@<assoc>_loaded`) aren't schema columns, so seed them too —
        // otherwise the cache-aware reader's reads stay `Var(0)`
        // (issue #27). Row classes pass `None` (scalar fields only, no
        // associations).
        if let Some(m) = model {
            for (name, ty) in self::associations::assoc_cache_ivar_bindings(m) {
                ctx.ivar_bindings.insert(name, ty);
            }
        }
    }
    if let Some(m) = model {
        for name in crate::ingest::class_attribute::declared_names(&m.body) {
            ctx.ivar_bindings.entry(crate::ingest::class_attribute::storage_ivar(&name)).or_insert(Ty::Untyped);
            ctx.ivar_bindings.entry(name).or_insert(Ty::Untyped);
        }
    }
    // Opt-in to `recv: Some(SelfRef)` rewriting on bare Sends —
    // matches the pattern view_to_library uses.
    ctx.annotate_self_dispatch = true;
    let body_ty = typer.analyze_expr(&mut method.body, &ctx);
    backfill_scalar_signature(method, body_ty);
}

/// Backfill a signature for a user-defined method whose body types to a
/// concrete scalar. User methods carry `signature: None`, so the
/// emitted `.rbs` says `-> untyped` even when the body is trivially
/// typed — and under spinel AOT an untyped return turns every chained
/// call into a whole-program poly dispatch (lobsters:
/// `User.username_regex_s[1...-1]` became a 26-class `[]` switch that
/// doesn't C-compile). Scalars only, and only when the body has no
/// explicit `return` (an early return of another type would make the
/// trailing-expression type a lie — in Ruby a `return` inside a block
/// exits the method, so the walk counts those too). If/Case bodies
/// union their branches, so divergent shapes fail the scalar gate on
/// their own. Param types stay untyped — this pins only the return.
/// A scalar, or a scalar unioned with nil — `String?` and friends.
///
/// The nullable arm matters because a `rescue` contributes one: a body
/// of `uri.to_s … rescue nil` types `Str | Nil`, and refusing to
/// backfill that leaves the method `-> untyped`, i.e. a boxed return
/// where a flat nullable scalar was available. Spinel stores `String?`
/// as a NULL `char *` and `Integer?` as a sentinel, so nullability
/// costs nothing here — widening to `untyped` does.
fn scalar_or_nullable_scalar(ty: &Ty) -> bool {
    fn is_scalar(t: &Ty) -> bool {
        matches!(t, Ty::Str | Ty::Int | Ty::Float | Ty::Bool)
    }
    match ty {
        t if is_scalar(t) => true,
        Ty::Union { variants } => {
            variants.iter().any(is_scalar)
                && variants.iter().all(|v| is_scalar(v) || matches!(v, Ty::Nil))
                // One scalar kind only: `Str | Int | Nil` has no single
                // flat representation, and guessing one would be the
                // bag this whole rule exists to avoid.
                && variants.iter().filter(|v| is_scalar(v)).count() == 1
        }
        _ => false,
    }
}

fn backfill_scalar_signature(method: &mut MethodDef, body_ty: Ty) {
    if method.signature.is_some()
        || method.block_param.is_some()
        || !scalar_or_nullable_scalar(&body_ty)
        || contains_return(&method.body)
    {
        return;
    }
    let params = method
        .params
        .iter()
        .map(|p| crate::ty::Param {
            name: p.name.clone(),
            ty: Ty::Untyped.into(),
            kind: p.ty_kind(),
        })
        .collect();
    method.signature = Some(Ty::Fn {
        params,
        block: None,
        ret: std::sync::Arc::new(body_ty),
        effects: crate::effect::EffectSet::default(),
    });
}

fn contains_return(e: &Expr) -> bool {
    if matches!(&*e.node, ExprNode::Return { .. }) {
        return true;
    }
    let mut found = false;
    e.node.for_each_child(&mut |child| {
        if !found && contains_return(child) {
            found = true;
        }
    });
    found
}

// ---------------------------------------------------------------------------
// Small ExprNode constructors used throughout. Each takes a synthetic span;
// span attribution happens at the synthesis choke points instead (per-
// declaration `inherit_span` stamps in the pushers, then `build_methods`'
// file-grain catch-all) — see a704ad6's convention.
// ---------------------------------------------------------------------------

pub(super) fn lit_str(s: String) -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: s } }),
        Ty::Str,
    )
}

pub(super) fn lit_sym(name: Symbol) -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Sym { value: name } }),
        Ty::Sym,
    )
}

pub(super) fn lit_int(value: i64) -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Int { value } }),
        Ty::Int,
    )
}

pub(super) fn nil_lit() -> Expr {
    with_ty(
        Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        Ty::Nil,
    )
}

/// Attach a known type to an Expr. Lowerers use this when the type is
/// statically known by construction — avoiding a separate analyzer
/// pass to rediscover what we already knew.
pub(super) fn with_ty(mut e: Expr, ty: Ty) -> Expr {
    e.ty = Some(ty);
    e
}

/// Schema column type → roundhouse `Ty`. Mirrors `ingest::model::ty_of_column`
/// — duplicated here to avoid making that internal helper public for one
/// caller. Keep them in sync; the mapping is small and stable.
///
/// Date/DateTime/Time map to `Ty::Str` — the STORAGE type. The DB column
/// is ISO-8601 TEXT (`column_read_method` reads `column_text`), and the
/// ivar / Row field / hydration / writes / `fill_timestamps` / `?`
/// predicate all operate on that stored text, uniformly across every
/// target. The logical `Ty::Time` lives one level up: `synth_attr_reader`
/// synthesizes the column's reader to parse the stored text into a real
/// `Time` (return type `Ty::Time`), so `record.created_at` is a native
/// `Time` for callers / analyze — while storage stays portable text. See
/// `synth_attr_reader`'s temporal branch.
pub fn ty_of_column(t: &ColumnType) -> Ty {
    match t {
        ColumnType::Integer | ColumnType::BigInt => Ty::Int,
        ColumnType::Float | ColumnType::Decimal { .. } => Ty::Float,
        ColumnType::String { .. } | ColumnType::Text => Ty::Str,
        ColumnType::Boolean => Ty::Bool,
        ColumnType::Date | ColumnType::DateTime | ColumnType::Time => Ty::Str,
        ColumnType::Binary => Ty::Str,
        // A schema-less `json` or `jsonb` column is stored TEXT and nothing
        // parses it: the
        // Row field, hydration, `[]`, `attributes` and the adapter's
        // escape all move the serialized string. `Hash[String, String]`
        // was a declaration no synthesized path implemented. What gives
        // such a column STRUCTURE is a `has_json` declaration, and that
        // is modeled as typed per-key accessors over this text
        // (`lower::has_json`), not as a Hash the whole column decodes to.
        ColumnType::Json | ColumnType::Jsonb => Ty::Str,
        ColumnType::Uuid => Ty::Str,
        ColumnType::Reference { .. } => Ty::Int,
    }
}

/// The column's type AS STORED IN A RECORD — `ty_of_column` widened with
/// `Nil` when the schema says the column is nullable or the column is
/// database-generated. A generated attribute is unset on a new record
/// until the INSERT computes it, even when the database column is NOT NULL.
/// Rails' unset value
/// for such a column is NULL, not the type's zero: a nullable unique
/// column left unset must not collide row-to-row (lobsters'
/// `users.password_reset_token`), and `where(merged_story_id: nil)` has
/// to match the rows that never set it — storing `0` there makes
/// `scope :unmerged` match nothing.
///
/// The primary key is excluded: `id` is assigned by the INSERT and every
/// hydration path treats it as present, so widening it would nilify
/// every `record.id` read for no semantic gain.
///
/// NOT for the SQL seam — `column_read_method` and the Arel visitor pick
/// their reader from the underlying column type, which nullability
/// doesn't change.
pub fn ty_of_column_slot(col: &Column) -> Ty {
    let base = ty_of_column(&col.col_type);
    if (col.nullable || col.generated.is_some()) && !col.primary_key {
        Ty::Union { variants: vec![base, Ty::Nil].into() }
    } else {
        base
    }
}

/// Build a `Ty::Fn` signature from positional (name, type) pairs and a return type.
/// Effects default to pure — callers refine if needed (lifecycle hooks etc.).
pub(crate) fn fn_sig(params: Vec<(Symbol, Ty)>, ret: Ty) -> Ty {
    Ty::Fn {
        params: params
            .into_iter()
            .map(|(name, ty)| crate::ty::Param {
                name,
                ty: ty.into(),
                kind: crate::ty::ParamKind::Required,
            })
            .collect(),
        block: None,
        ret: std::sync::Arc::new(ret),
        effects: crate::effect::EffectSet::pure(),
    }
}

pub(super) fn var_ref(name: Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Var { id: VarId(0), name })
}

pub(super) fn class_const(id: &ClassId) -> Expr {
    let path: Vec<Symbol> = id.0.as_str().split("::").map(Symbol::from).collect();
    Expr::new(Span::synthetic(), ExprNode::Const { path })
}

pub(super) fn self_ref() -> Expr {
    Expr::new(Span::synthetic(), ExprNode::SelfRef)
}

pub(super) fn seq(exprs: Vec<Expr>) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Seq { exprs })
}

pub(super) fn is_id_column(name: &Symbol) -> bool {
    let s = name.as_str();
    s == "id" || s.ends_with("_id")
}

/// The schema columns a class stores that the DB declares nullable —
/// the `LibraryClass::nullable_columns` payload. Same rule as
/// `ty_of_column_slot`: nullable, primary key excluded.
pub(crate) fn nullable_column_names(table: Option<&Table>) -> Vec<Symbol> {
    let Some(t) = table else { return Vec::new() };
    t.columns
        .iter()
        .filter(|c| c.nullable && !c.primary_key)
        .map(|c| c.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::det::HashMap;
    use std::path::PathBuf;

    /// Framework normalization must not replace a same-named application class.
    #[test]
    fn integer_key_cast_registry_keeps_application_constants() {
        let app_id = ClassId(Symbol::from("IntegerKeyCast"));
        let marker = Symbol::from("application_marker");
        let mut application = crate::analyze::ClassInfo::default();
        application.class_methods.insert(marker.clone(), fn_sig(vec![], Ty::Str));
        let mut classes = HashMap::from_iter([(app_id.clone(), application)]);

        insert_integer_key_cast_info(&mut classes);

        assert_eq!(classes[&app_id].class_methods[&marker], fn_sig(vec![], Ty::Str));
        let framework = &classes[&ClassId(Symbol::from("ActiveRecord::IntegerKeyCast"))];
        assert!(framework.class_methods.contains_key(&Symbol::from("parse")));
        assert!(!classes[&app_id].class_methods.contains_key(&Symbol::from("parse")));
    }

    fn app(model_body: &str) -> crate::App {
        let files = [
            ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :articles do |t|\n    t.integer :book_id\n  end\n  create_table :books do |t|\n    t.string :title\n  end\nend\n".to_string()),
            ("app/models/article.rb", format!("class Article < ApplicationRecord\n{model_body}\nend\n")),
            ("app/models/book.rb", "class Book < ApplicationRecord\nend\n".to_string()),
        ];
        crate::ingest::ingest_app_from_tree(
            files
                .into_iter()
                .map(|(path, text)| (PathBuf::from(path), text.into_bytes()))
                .collect::<HashMap<_, _>>(),
        )
        .expect("ingest test app")
    }

    fn article_methods(app: &crate::App) -> Vec<MethodDef> {
        let model = app
            .models
            .iter()
            .find(|m| m.name.0.as_str() == "Article")
            .unwrap();
        build_methods(model, &app.models, &app.schema, &Default::default())
    }

    fn article_info(app: &crate::App, methods: &[MethodDef]) -> crate::analyze::ClassInfo {
        let model = app
            .models
            .iter()
            .find(|m| m.name.0.as_str() == "Article")
            .unwrap();
        build_class_info(model, methods, app.schema.tables.get(&model.table.0))
    }

    #[test]
    fn class_info_keeps_precise_parent_helper_return() {
        let app = app("  belongs_to :book\n  def positioning_parent\n    book\n  end");
        let mut methods = article_methods(&app);
        methods
            .iter_mut()
            .find(|m| m.name.as_str() == "positioning_parent")
            .unwrap()
            .body
            .ty = Some(Ty::Class {
            id: ClassId(Symbol::from("Book")),
            args: vec![].into(),
        });
        let info = article_info(&app, &methods);
        assert_eq!(
            info.instance_methods
                .get(&Symbol::from("positioning_parent")),
            Some(&fn_sig(vec![], Ty::Class {
                id: ClassId(Symbol::from("Book")),
                args: vec![].into()
            }))
        );
    }

    #[test]
    fn inferred_record_return_keeps_positional_hash_and_keyword_call_shapes() {
        for (formal, positional) in [("options = {}", true), ("options: {}", false)] {
            let app = app(&format!(
                "  belongs_to :book\n  def parent_for({formal})\n    book\n  end\n  def probe\n    parent_for(title: 'asymmetric')\n  end"
            ));
            let mut methods = article_methods(&app);
            let record = Ty::Class { id: ClassId(Symbol::from("Book")), args: vec![].into() };
            let parent = methods.iter_mut().find(|m| m.name.as_str() == "parent_for").unwrap();
            parent.body.ty = Some(record.clone());
            parent.params[0].default.as_mut().unwrap().ty = Some(Ty::Hash {
                key: std::sync::Arc::new(Ty::Sym), value: std::sync::Arc::new(Ty::Str),
            });
            let classes = HashMap::from_iter([
                (ClassId(Symbol::from("Article")), article_info(&app, &methods)),
            ]);
            let probe = methods.iter_mut().find(|m| m.name.as_str() == "probe").unwrap();
            probe.enclosing_class = Some(Symbol::from("Article"));
            type_method_body(probe, &classes, None, None);
            assert_eq!(probe.body.ty, Some(record), "{formal}");
            let ExprNode::Send { args, .. } = &*probe.body.node else { panic!("probe call") };
            assert!(matches!(&*args[0].node, ExprNode::Hash { kwargs, .. } if *kwargs != positional),
                "lost call convention for {formal}: {:?}", probe.body);
        }
    }

    #[test]
    fn semantic_relation_seed_precedes_raw_body_fallback() {
        let app = app("  def self.recent\n    where(book_id: 1)\n  end");
        let mut methods = article_methods(&app);
        let recent = methods
            .iter_mut()
            .find(|m| m.name.as_str() == "recent")
            .unwrap();
        recent.signature = None;
        recent.body.ty = Some(Ty::Class { id: ClassId(Symbol::from("Book")), args: vec![].into() });
        let info = article_info(&app, &methods);
        assert_eq!(
            info.class_methods.get(&Symbol::from("recent")),
            Some(&Ty::Relation {
                of: ClassId(Symbol::from("Article"))
            })
        );
    }

    #[test]
    fn raw_container_and_fn_body_types_are_excluded() {
        let app = app("  def callable\n    1\n  end");
        let mut methods = article_methods(&app);
        for ty in [fn_sig(vec![], Ty::Int), Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Int) }, Ty::Untyped] {
            let callable = methods.iter_mut().find(|m| m.name.as_str() == "callable").unwrap();
            callable.signature = None;
            callable.body.ty = Some(ty);
            assert!(!article_info(&app, &methods).instance_methods.contains_key(&Symbol::from("callable")));
        }
    }

    /// A test body types `Article.evens` from this registry; without the
    /// entry `assert_equal [2, 4], Article.evens` compared against an
    /// untyped call, and its `.to_a` reached typed targets as a dynamic
    /// send their lists do not define.
    #[test]
    fn scalar_array_body_type_is_registered_but_not_an_untyped_one() {
        let app = app("  def self.evens\n    [1, 2].map { |x| x * 2 }\n  end");
        let mut methods = article_methods(&app);
        let ints = Ty::Array { elem: std::sync::Arc::new(Ty::Int) };
        for (ty, kept) in [(ints.clone(), true), (Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }, false)] {
            let evens = methods.iter_mut().find(|m| m.name.as_str() == "evens").unwrap();
            evens.signature = None;
            evens.body.ty = Some(ty);
            let got = article_info(&app, &methods).class_methods.get(&Symbol::from("evens")).cloned();
            match got {
                Some(Ty::Fn { ret, .. }) if kept => assert_eq!(*ret, ints),
                None if !kept => {}
                other => panic!("unexpected registry entry: {other:?}"),
            }
        }
    }

    #[test]
    fn explicit_signature_precedes_raw_body_type() {
        let app = app("  def answer\n    'wrong'\n  end");
        let mut methods = article_methods(&app);
        let answer = methods
            .iter_mut()
            .find(|m| m.name.as_str() == "answer")
            .unwrap();
        let explicit = fn_sig(vec![], Ty::Int);
        answer.signature = Some(explicit.clone());
        answer.body.ty = Some(Ty::Class { id: ClassId(Symbol::from("Book")), args: vec![].into() });
        assert_eq!(
            article_info(&app, &methods)
                .instance_methods
                .get(&Symbol::from("answer")),
            Some(&explicit)
        );
    }
}
