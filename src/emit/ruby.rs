//! Ruby emitter: App → spinel-shape Ruby source files.
//!
//! Mirrors the Crystal emitter's structure: lowered IR (LibraryClass) is
//! the single input, and emission is uniform across Rails components
//! (models, controllers, views, routes, schema all flow through
//! `library::emit_library_class_decl`). No parsed-AST emit path —
//! per the convergence decision, source-equivalence round-trip is no
//! longer a goal; compile-equivalence via Spinel is. Cross-cutting
//! helpers live in `shared`; expression emission lives in `expr`.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::EmittedFile;
use crate::App;
use crate::dialect::{LibraryClass, MethodDef, MethodReceiver};
use crate::lower::controller::body::FormatBreadth;

/// Canonical spinel test bootstrap. Single source of truth for what
/// the emitted spinel project's `test/test_helper.rb` should contain.
const SPINEL_TEST_HELPER: &str =
    include_str!("../../runtime/spinel/test/test_helper.rb");

mod attribute_api;
mod attribute_cast;
mod collection_autosave;
pub(crate) mod expr;
mod library;
pub(crate) use library::{body_statement_classes, carried_body_statement_spans, carries_body_statement, dropped_class_body_calls, includes_active_model_validations, install_lookup_indexes, modelled_class_body_calls, replays_class_body, singleton_scoped};
mod rbs;
pub mod shake;
mod shared;
pub mod source_map;
pub mod source_markers;

/// Render a `Ty` to its RBS string form (`String`, `Array[Comment]`,
/// `Article`, `Integer?`). Re-exported for non-emit consumers — e.g. the
/// browser playground's inferred-type hovers (`wasm/src/lib.rs`).
pub use rbs::ty_to_rbs;

// External API: the historical surface kept for `tests/` and `bin/`.
pub use expr::{emit_expr, with_error_attributes, with_native_keyword_splats, with_singleton_scoped};
pub(crate) use expr::native_keywords;

/// The `app/models/<stem>` file a library class is emitted into (a
/// nested class goes in its outermost owner's file), as `<stem>`.
pub(crate) fn library_file_stem(name: &str, app: &App) -> String {
    crate::naming::underscore(&library::file_owner(name, app))
}

/// Tarjan's strongly connected components (`library::strongly_connected`).
pub(crate) fn strongly_connected(edges: &[Vec<usize>]) -> Vec<usize> {
    library::strongly_connected(edges)
}

/// The app/models.rb anchor of a support file's file program.
pub(crate) fn file_program_anchor(app: &App, program: &crate::app::FileProgram) -> String {
    library::file_program_anchor(app, program)
}

/// Each app source with no file program => the `models/<stem>` files of
/// the classes and modules it declares, in the order requiring it
/// defines them.
pub(crate) fn declarations_by_source(app: &App) -> std::collections::BTreeMap<String, Vec<String>> {
    library::declarations_by_source(app)
}

/// Each support file's file program (`App::file_programs`) as
/// `(app/models/roundhouse_files/<source>.rb, content)`. `emitted` is
/// every app/models.rb anchor (`models/<stem>`) the tree has.
pub(crate) fn file_program_files(app: &App, emitted: &std::collections::BTreeSet<String>) -> Vec<(String, String)> {
    app.file_programs
        .iter()
        .map(|program| {
            let path = format!("app/{}.rb", library::file_program_anchor(app, program));
            let content = library::emit_file_program(program, app, std::path::Path::new(&path), emitted);
            (path, content)
        })
        .collect()
}

pub(crate) fn with_external_gem_namespaces<R>(
    namespaces: &std::collections::BTreeSet<String>,
    includes: std::collections::BTreeMap<String, Vec<String>>,
    f: impl FnOnce() -> R,
) -> R {
    expr::with_external_gem_namespaces(namespaces, includes, f)
}

/// Emit a single `MethodDef` as Ruby source (trailing newline included).
/// The signature and effects are not emitted — they belong to the RBS
/// sidecar, not to Ruby itself. Used by the runtime-extraction pipeline
/// to round-trip a typed standalone function back to Ruby source.
pub fn emit_method(m: &MethodDef) -> String {
    let prefix = match m.receiver {
        MethodReceiver::Instance => "",
        MethodReceiver::Class => "self.",
    };
    // A native-keyword tree writes ingest's flattened keywords back
    // (`name: default`, `**rest`): Ruby 3 would bind a caller's keyword
    // Hash to the first optional positional. Only a trailing run can be
    // keywords; anything after it keeps the whole method positional.
    let respelled = crate::dialect::Param::respelled_keywords_from(&m.params);
    let first_kw = respelled.unwrap_or(m.params.len());
    let respell = expr::native_keywords() && first_kw < m.params.len();
    let mut ps: Vec<String> = m
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (respell && i >= first_kw, p))
        .map(|(kw, p)| match (&p.default, p.keyword) {
            _ if p.forwarding => "...".to_string(),
            _ if kw && p.from_kwrest => format!("**{}", p.name.as_str()),
            (Some(default), false) if kw => format!("{}: {}", p.name.as_str(), emit_default(default)),
            (None, false) if kw => format!("{}:", p.name.as_str()),
            (Some(default), false) => {
                format!("{} = {}", p.name.as_str(), emit_default(default))
            }
            (Some(default), true) => {
                format!("{}: {}", p.name.as_str(), emit_default(default))
            }
            (None, true) if p.rest => format!("**{}", p.name.as_str()),
            (None, true) => format!("{}:", p.name.as_str()),
            (None, false) if p.rest => format!("*{}", p.name.as_str()),
            (None, false) => p.name.as_str().to_string(),
        })
        .collect();
    // The captured block param (`&block`) closes the list — methods that
    // pass their block on (`fetch(&block)`) need it named at the def site.
    if let Some(bp) = &m.block_param {
        ps.push(format!("&{}", bp.name.as_str()));
    }
    let params = if ps.is_empty() {
        String::new()
    } else {
        format!("({})", ps.join(", "))
    };
    let mut out = String::new();
    // The body's first statement: a Seq writes markers only between
    // its statements, so the first is named here.
    let first = match &*m.body.node {
        crate::expr::ExprNode::Seq { exprs } => exprs.first().unwrap_or(&m.body),
        _ => &m.body,
    };
    let body_marker = source_markers::marker_for(&first.span);
    // A marker ABOVE the def, so the def line never reports the previous
    // method's held position: Spinel names a --debug backtrace frame by
    // its def line (spinel#7658). A synthesized method has no name span;
    // its body's first statement stands in.
    if let Some(mk) = source_markers::marker_for(&m.name_span).or_else(|| body_marker.clone()) {
        writeln!(out, "{mk}").unwrap();
    }
    // A name no `def` can spell (`define_method("tag:unit") do … end`,
    // `class_body_define_method`) is defined the way the source did:
    // the block is the body, with the same formals and lambda arity.
    let spelled = def_spellable(m.name.as_str());
    if spelled {
        writeln!(out, "def {prefix}{}{}", m.name, params).unwrap();
    } else {
        let definer = match m.receiver {
            MethodReceiver::Instance => "define_method",
            MethodReceiver::Class => "define_singleton_method",
        };
        let block_params = if ps.is_empty() { String::new() } else { format!(" |{}|", ps.join(", ")) };
        writeln!(out, "{definer}({}) do{block_params}", expr::ruby_sym_literal(m.name.as_str())).unwrap();
    }
    if let Some(mk) = body_marker {
        writeln!(out, "{mk}").unwrap();
    }
    let body_text = emit_expr(&m.body);
    fn emit_default(default: &crate::expr::Expr) -> String {
        let src = expr::emit_expr(default);
        // A command-call default (`raise "…"`, the dead_default
        // grounding) needs parens to close unambiguously inside the
        // parameter list.
        if matches!(&*default.node, crate::expr::ExprNode::Raise { .. }) {
            format!("({src})")
        } else {
            src
        }
    }
    for line in body_text.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            writeln!(out, "  {line}").unwrap();
        }
    }
    out.push_str("end\n");
    out
}

/// What `audit_block_signatures` found.
#[derive(Debug, Default)]
pub struct BlockSignatureAudit {
    /// Lambdas the ingest built with a block signature.
    pub ingested: usize,
    /// Lambdas the Ruby emitter wrote whose body is one of those.
    pub written: usize,
    /// Each written one whose signature names other parameters than the
    /// ingest's: a lowering rebuilt the Lambda and dropped (or changed)
    /// what Lambda IR carries in `signature` (most `Lambda { .. }`
    /// constructions outside ingest do not bind it).
    pub losses: Vec<String>,
    /// The ingested ones the emit did not write as a Lambda (a lowering
    /// made a method of it, or dropped it), with their signatures.
    pub unwritten: Vec<String>,
}

/// Run `run` (ingest, lower and write a Ruby-family tree), recording
/// every block signature the ingest builds and every Lambda the Ruby
/// emitter writes, matched by the Lambda body's source span. A Lambda a
/// lowering dropped or turned into something else is not written and
/// not counted; one written with fewer or other optional, keyword or
/// keyword-rest parameters than the source had is a loss. Defaults are
/// not compared (a lowering may qualify a constant in one).
pub fn audit_block_signatures<T>(run: impl FnOnce() -> T) -> (T, BlockSignatureAudit) {
    use crate::expr::BlockSignature;
    crate::ingest::expr::INGESTED_SIGNATURES.with(|r| *r.borrow_mut() = Some(Vec::new()));
    expr::EMITTED_SIGNATURES.with(|r| *r.borrow_mut() = Some(Vec::new()));
    let out = run();
    let ingested = crate::ingest::expr::INGESTED_SIGNATURES.with(|r| r.borrow_mut().take()).unwrap_or_default();
    let emitted = expr::EMITTED_SIGNATURES.with(|r| r.borrow_mut().take()).unwrap_or_default();
    let shape = |s: &BlockSignature| -> String {
        s.optionals
            .iter()
            .map(|(n, _)| format!("{n} = …"))
            .chain(s.keywords.iter().map(|(n, d)| if d.is_some() { format!("{n}: …") } else { format!("{n}:") }))
            .chain(s.kwrest.iter().map(|n| format!("**{n}")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let by_body: crate::det::HashMap<crate::span::Span, &BlockSignature> =
        ingested.iter().map(|(span, s)| (*span, s)).collect();
    let mut audit = BlockSignatureAudit { ingested: by_body.len(), ..Default::default() };
    for (span, wrote) in &emitted {
        let Some(had) = by_body.get(span) else { continue };
        audit.written += 1;
        if shape(had) != shape(wrote) {
            audit.losses.push(format!(
                "{span:?}: the source block takes `{}`, the Ruby emit wrote `{}`",
                shape(had),
                shape(wrote)
            ));
        }
    }
    let mut unwritten: Vec<(crate::span::Span, String)> = by_body
        .iter()
        .filter(|(span, _)| !emitted.iter().any(|(e, _)| e == *span))
        .map(|(span, s)| (*span, format!("{span:?}: `{}`", shape(s))))
        .collect();
    unwritten.sort_by_key(|(span, _)| (span.file.0, span.start));
    audit.unwritten = unwritten.into_iter().map(|(_, s)| s).collect();
    (out, audit)
}

/// Whether `def <name>` parses as a method named `name`: an identifier
/// (a trailing `?`, `!` or `=` allowed) or an operator method. A Symbol
/// literal that needs quotes (`:"tag:unit"`) names one only
/// `define_method` can define.
pub(crate) fn def_spellable(name: &str) -> bool {
    !name.starts_with(['@', '$']) && !expr::ruby_sym_literal(name).starts_with(":\"")
}

/// Emit library-shape Ruby — for transpiled-shape input where class
/// bodies contain explicit methods rather than Rails DSL calls.
/// Complementary to `emit`; skips Rails-app artifacts (controllers,
/// routes, views, fixtures, importmap, schema) and emits only one
/// `.rb` file per `LibraryClass`. Mirrors `typescript::emit_library`.
pub fn emit_library(app: &App) -> Vec<EmittedFile> {
    library::emit_library_class_decls(app)
}

/// Swap façade-fated extras emits (Sponge) for their raising façades —
/// scaffold-base step; see `library::EXTRAS_FACADES`.
pub fn apply_extras_facades(files: &mut [(String, String)], app: &crate::App) {
    library::apply_extras_facades(files, app)
}

/// Restore the verbatim source-shape emit over the extras façades
/// (CRuby tree — the real stdlib is available there).
pub fn restore_extras_facades(files: &mut [(String, String)], app: &App) {
    library::restore_extras_facades(files, app)
}

/// Per-app `Rails::Application` reopen from the source
/// config/application.rb (see `App.rails_application`) — emitted to
/// `config/application.rb` so `Rails.application.read_only?` etc.
/// dispatch to the app's real config methods. The scaffold main.rb
/// requires it conditionally (same idiom as config/importmap.rb).
pub fn emit_rails_application(app: &App) -> Option<EmittedFile> {
    app.rails_application.as_ref().map(|lc| {
        let mut lc = lc.clone();
        // Ground `Rails.application.routes.url_helpers.<x>_url(host:,
        // protocol:)` (lobsters' root_url) — the routing internals
        // behind url_helpers aren't modeled, but the absolute URL is
        // just protocol + "://" + host + RouteHelpers.<x>_path.
        for m in &mut lc.methods {
            library::rewrite_url_helpers_absolute(&mut m.body);
        }
        let mut file = library::emit_library_class_decl(&lc, app, PathBuf::from("config/application.rb"));
        file.content.push_str(&application_namespace_alias(app.application_namespace.as_deref()));
        file
    })
}

/// `module Sample; Application = ::Rails::Application; end`: the reopen
/// above is the app's `class Sample::Application < Rails::Application`
/// (its body is replayed on `Rails::Application`), so the constant app
/// code reads (`Sample::Application.config`) names it. Empty without a
/// namespace, or one that is not a constant path.
fn application_namespace_alias(namespace: Option<&str>) -> String {
    let Some(namespace) = namespace else { return String::new() };
    let segments: Vec<&str> = namespace.split("::").collect();
    let constant_path = segments.iter().all(|s| {
        s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    if !constant_path {
        return String::new();
    }
    let mut out = String::from("\n");
    for (depth, segment) in segments.iter().enumerate() {
        out.push_str(&format!("{}module {segment}\n", "  ".repeat(depth)));
    }
    out.push_str(&format!("{}Application = ::Rails::Application\n", "  ".repeat(segments.len())));
    for depth in (0..segments.len()).rev() {
        out.push_str(&format!("{}end\n", "  ".repeat(depth)));
    }
    out
}

/// Lower each `app.models` entry through `model_to_library` and emit
/// the resulting `LibraryClass` as a Ruby source file. The output is
/// the universal post-lowering shape — explicit per-attr accessors,
/// explicit `validate` / `before_destroy` bodies, no Rails DSL.
///
/// Spinel is the natural validation target for the lowering pipeline:
/// the lowered IR shape *is* spinel-blog shape (per
/// `project_universal_post_lowering_ir.md`), so a Ruby render is the
/// shortest path from lowerer output to a runnable artifact. Use this
/// while accumulating lowerers; the per-target collapse decisions for
/// TS / Rust / etc. are deferred until enough lowerers exist for
/// natural groupings to surface.
/// Retype a model's synthesized `from_stmt(stmt)` class-method param to
/// `Untyped`. See the call site in `emit_lowered_models` for why this is
/// a ruby/spinel-only adapter (the stmt handle is a `void *` here, an
/// integer cursor on the strict targets).
fn relax_from_stmt_handle(lc: &mut LibraryClass) {
    for m in &mut lc.methods {
        if m.name.as_str() == "from_stmt" && m.receiver == MethodReceiver::Class {
            if let Some(crate::ty::Ty::Fn { params, .. }) = m.signature.as_mut() {
                if let Some(p) = params.first_mut() {
                    p.ty = crate::ty::Ty::Untyped.into();
                }
            }
        }
    }
}

/// Model IR before Ruby adaptation. Admission uses this same registry,
/// demand survey and body typer without rendering files or RBS.
pub(crate) fn materialize_models(
    app: &App,
    materialization: crate::lower::model_to_library::Materialization<'_>,
) -> (Vec<LibraryClass>, crate::lower::controller_to_library::params::ParamsSpecs) {
    // Collect controller `permit(...)` declarations so the model lowerer
    // can synthesize `from_params(p: <Resource>Params)` factories sized
    // to the permitted-fields list. See `controller_to_library/params.rs`.
    let mut params_specs =
        crate::lower::controller_to_library::params::collect_specs(&app.controllers);
    // A permitted `has_one_attached` field is a FILE on the params
    // class; the model's `from_params` reads it as one.
    params_specs.mark_file_fields(&app.models);

    // Bulk lower so per-resource synthesized siblings (`<Model>Row`)
    // ride alongside the model class. Each returned `LibraryClass`
    // becomes one `app/models/<stem>.rb` file. *Params classes are
    // synthesized by the controller lowerer (separate emit path —
    // `emit_lowered_controllers`); we register them here as
    // synthesized siblings so model files that reference them
    // (`Article.from_params(...)` calls) get explicit requires.
    // Class methods a call site reaches through an association and whose
    // body QUERIES — `@room.messages.paged?`. `apply_scope_lowering`
    // below re-roots those on a threaded `__rel`, so the arel fold has
    // to be held back on them here or there is nothing left to re-root.
    // Same survey both passes read, for the same reason the survey
    // covers the App rather than a slice.
    let mut assoc_scopes = crate::lower::scope_chain::assoc_query_method_names(
        &crate::lower::scope_chain::survey_assoc_class_methods(
            app,
            &crate::lower::scope_chain::build_assoc_registry(&app.models),
            &crate::lower::scope_chain::build_scope_registry(&app.models),
        )
        .0,
    );
    // …and a class method of a model with subclasses reading its
    // implicit `all`, which `apply_scope_lowering` seeds on `self`.
    assoc_scopes.extend(crate::lower::sti_scope::subclassed_self_reads(app));
    let _library_demand = crate::lower::model_to_library::schema::share_library_demand(&app.library_classes);
    let lcs = crate::lower::model_to_library::lower_models_inner_for_dialect(
        &app.models,
        &app.schema,
        Vec::new(),
        &params_specs,
        &assoc_scopes,
        materialization,
        crate::lower::model_to_library::FinderInputs::Request,
        true,
        crate::lower::model_to_library::ErrorsReaders::CreatedOnFirstRead,
        if app.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql) {
            crate::lower::arel::SqlDialect::Postgres
        } else {
            crate::lower::arel::SqlDialect::Sqlite
        },
    ).0;
    (lcs, params_specs)
}

pub fn emit_lowered_models(app: &App) -> Vec<EmittedFile> {
    let (lcs, params_specs) = lowered_model_lcs(app);
    emit_model_classes(&lcs, app, &params_specs)
}

/// The model classes `emit_lowered_models` writes, before rendering.
/// Shared with `emit::futamura::routes`, which walks what is emitted.
pub(crate) fn lowered_model_lcs(
    app: &App,
) -> (Vec<LibraryClass>, crate::lower::controller_to_library::params::ParamsSpecs) {
    let (mut lcs, params_specs) = materialize_models(
        app,
        crate::lower::model_to_library::Materialization::Emit,
    );
    library::preserve_record_receiver_returns(&mut lcs, app);
    // The sqlite statement handle `Db.prepare` returns is a per-target
    // `Db` primitive: an integer cursor on most adapters (the shared
    // model lowerer's `Ty::Int` default), but an opaque FFI `void *` on
    // the spinel shim (`runtime/spinel/db.rb`). Relax the synthesized
    // `from_stmt(stmt)` param to `untyped` so the emitted `.rbs` doesn't
    // pin it to `Integer` — spinel infers the pointer from the
    // `Db.column_*(stmt, …)` FFI calls, and CRuby ignores the sig
    // entirely. Confined here, the only emitter whose `Db` hands back a
    // raw pointer; the strict targets keep `Ty::Int` (correct for their
    // integer-handle `Db`). See the toolchain-spinel `from_stmt` seam.
    for lc in &mut lcs {
        relax_from_stmt_handle(lc);
    }
    apply_model_lowering(&mut lcs, app);
    (lcs, params_specs)
}

/// The existing ordered Ruby IR producers, shared with accessor admission.
/// Keep their implementations in place: sharing does not require moving
/// unrelated controller, view and library transformations.
pub(crate) fn apply_model_lowering(mut lcs: &mut [LibraryClass], app: &App) {
    // A model's lifecycle hook runs the one an ancestor defines first.
    library::chain_inherited_lifecycle_hooks(lcs, app);
    // Ruby-family scope lowering: synthesize model scope methods +
    // normalize scope chains before rendering (no-op for scope-free apps).
    library::apply_scope_lowering(&mut lcs, app);
    // has_many :through readers: rebuild the shared direct-fk reader as a
    // Relation join through the intermediate table (no-op when no
    // through-assoc resolves).
    library::apply_through_assoc_lowering(&mut lcs, app);
    // belongs_to autosave: an unsaved target assigned through the
    // writer is saved at before_validation and its id taken (no-op for
    // models with no belongs_to). Ruby-family only — see the pass.
    library::apply_belongs_to_autosave(&mut lcs, app);
    // An STI row hydrates as its subclass (no-op for apps with no STI).
    library::apply_sti_hydration(&mut lcs, app);
    // A plain (non-STI) model subclass loads as itself.
    crate::lower::sti_scope::apply_plain_subclass_hydration(&mut lcs, app);
    // belongs_to writers are synthesized by the shared model lowering
    // (lower::model_to_library::associations), alongside the readers.
    // App-helper resolution: bare `avatar_img(...)` → `ApplicationHelper.
    // avatar_img(...)` + helper modules become module-functions (no-op when
    // the app ships no non-empty helpers).
    library::apply_helper_lowering(&mut lcs, app);
    // A bare `<x>_path` that `apply_helper_lowering` did not claim and
    // the route table DOES answer is a route helper called somewhere
    // nothing qualifies — `rails_blob_path` in a `data:` hash, or in a
    // helper module that `delegate`s it. It gets the same
    // `RouteHelpers.` receiver every other call site gets.
    crate::lower::route_helper_receiver::qualify_lcs(&mut lcs, app);
    // A record handed to a path helper becomes its slug here, not
    // inside the helper (no-op unless a model overrides `to_param`).
    library::apply_route_param_lowering(&mut lcs, app);
    // A helper handed `raw(x)` retargets to a `_raw` clone in which
    // that argument's escaping comes off (no-op without such a site).
    library::apply_raw_helper_monomorphization(&mut lcs, app);
    // Dynamic `send(k)` → static `case` dispatch moved to the shared
    // post-analyze hook (`lower::apply_send_static_dispatch`) — hook
    // bodies arrive here already grounded, with duration-unit arms in
    // plural form for the duration lowering below.
    // `record.update!(k: v)` inlining moved to the shared post-analyze
    // hook (`lower::apply_update_kwargs_inline`) — hook bodies arrive
    // here already in writer-assign + save form.
    // `Time.current` and ActiveSupport durations (`70.days` →
    // `Duration.days(70)`) are grounded app-wide by the shared
    // post-analyze hook (`lower::apply_time_current_lowering`,
    // `lower::apply_duration_lowering`); only the view pipeline still
    // applies the emit-time copies.
    // Date/DateTime/Time column accessors coerce to/from real `Time`
    // (no-op for apps with no temporal columns).
    library::apply_datetime_lowering(&mut lcs, app);
    // has_secure_password (authenticate + plaintext accessors, bcrypt
    // contract) and typed_store virtual-attribute accessors are
    // synthesized by the shared model lowering
    // (`lower::secure_password`, `lower::typed_store`) — they arrive
    // here already present.
    // Boolean-column readers/predicates cast SQLite's 0/1 Integers
    // (0 is truthy in Ruby).
    library::apply_boolean_lowering(&mut lcs, app);
    // NULL fidelity: nullable columns hydrate to nil (not 0/"") and fk
    // 0-sentinel guards widen to accept nil (no-op for schemas with no
    // nullable columns); synthesized `.empty?` predicate forms in
    // model bodies tolerate the nil.
    library::apply_hydration_nil_lowering(&mut lcs, app);
    // CRuby/JRuby: a nil written to a belongs_to stores nil (NULL), not
    // the strict targets' 0/"" sentinel.
    let no_views = crate::det::HashMap::default();
    library::apply_belongs_to_nil_writes(&mut lcs, app, &no_views);
    // CRuby/JRuby: broadcasts_to renders for the record located again,
    // as Rails' later job does.
    library::apply_broadcast_located_record(&mut lcs, app);
    library::apply_nilsafe_empty_lowering(&mut lcs);
    library::apply_time_format_lowering(&mut lcs, app);
    library::apply_dynamic_render_options_lowering(&mut lcs);
    // Runtime-Relation eager loading: per-model `preload_associations`
    // machinery + belongs_to reader cache guards, so `includes(...)`
    // on a runtime Relation executes as batched IN-loads instead of
    // N+1 lazy reads. Runs last so nothing reprocesses the synthesized
    // bodies (no-op unless the app mentions includes).
    library::apply_preload_lowering(&mut lcs, app, &no_views);
    // A belongs_to reader memoizes, and writing the foreign key or the
    // association invalidates it (no-op for models with no belongs_to). AFTER the
    // preload pass, which is what puts the `return @<name>_cache if
    // @<name>_loaded` guard on the reader — this pass memoizes into
    // that same cache and would have nothing to prepend itself to if it
    // ran first.
    library::apply_belongs_to_memoization(&mut lcs, app);
    // …and a has_many/has_one reader keeps the records it loads where the
    // app reads `loaded?` (same guard, same reason to run after preload).
    library::apply_collection_memoization(&mut lcs, app, &no_views);
    // `record.reload` drops the targets those caches hold, as Rails'.
    library::apply_reload_resets_associations(&mut lcs, app, &no_views);
    // A has_many validates its new (or, in a custom context, loaded)
    // records with the owner, from the caches the memoization keeps.
    collection_autosave::apply(&mut lcs, app, &no_views);
    // Every association reader waits on its record's pending preload
    // (`lower::deferred_preload`): a Relation's includes run when a record
    // first reads an association, not when the rows arrive. After the two
    // passes above, which put the `@<name>_loaded` guard it looks for.
    crate::lower::deferred_preload::apply(&mut lcs, app);
    // `record.association(:name)` answers the association object, a view
    // over the `@<name>_cache`/`@<name>_loaded` pair the passes above
    // settle (no-op unless the app calls `association`). Before
    // `lazy_model_state`, which renames the cache reads it finds.
    library::apply_association_reader(&mut lcs, app);
    // `build_<name>` / `create_<name>` / `reload_<name>` / `reset_<name>` of
    // the singular associations the app calls, run on the association object.
    library::apply_singular_association_builders(&mut lcs, app);
    // A has_many cache is made on first read rather than at construction,
    // and the constructor's `attrs = {}` default is one shared frozen Hash
    // (`lower::lazy_model_state`) — nine Arrays and a Hash per hydrated
    // User on campfire's room page, for records that only answer a name.
    // LAST, after every pass that reads or writes `@<assoc>_cache` by name.
    crate::lower::lazy_model_state::apply(&mut lcs, app);
    // String names on `[]`/`[]=`, the class body's `abstract_class`
    // write and the per-model `schema_column_types` table the runtime's
    // `type_for_attribute` reads (no-op for a model with no table).
    attribute_api::apply(&mut lcs, app);
    // Assignment casts through each column's type, and Rails'
    // `attributes` (every column, the id too) beside the Dirty snapshot.
    // After `attribute_api`, whose `schema_column_types` names the types.
    attribute_cast::apply(&mut lcs, app);
}

fn emit_model_classes(
    lcs: &[LibraryClass],
    app: &App,
    params_specs: &crate::lower::controller_to_library::params::ParamsSpecs,
) -> Vec<EmittedFile> {
    // Synthesized siblings need explicit `require_relative` even when
    // they live in the same directory as their referencer — nothing else
    // in the require chain loads them. Build a (name, anchor) map from
    // every LC carrying an `origin` tag, plus the *Params classes that
    // controllers will synthesize separately.
    let mut synthesized: Vec<(String, String)> = lcs
        .iter()
        .filter(|lc| lc.origin.is_some())
        .map(|lc| {
            let name = lc.name.0.as_str().to_string();
            let stem = crate::naming::underscore(&name);
            (name, format!("app/models/{stem}"))
        })
        .collect();
    for spec in params_specs.iter() {
        let name = spec.class_id.0.as_str().to_string();
        let stem = crate::naming::underscore(&name);
        synthesized.push((name, format!("app/models/{stem}")));
    }

    lcs.iter()
        .flat_map(|lc| {
            let stem = crate::naming::underscore(lc.name.0.as_str());
            let out_path = PathBuf::from(format!("app/models/{stem}.rb"));
            library::emit_library_class_pair_with_synthesized(
                lc,
                app,
                out_path,
                &synthesized,
            )
        })
        .collect()
}

/// Emit `config/schema.rb` in spinel-blog shape — a `Schema` module
/// with `def self.statements` returning the DDL list. Per-statement
/// (rather than one joined string) so adapters that don't support
/// multi-statement execution work too. Consumes the universal
/// `lower_schema_module_functions` output, sharing shape across
/// every target. An app without tables still gets the module, with an
/// empty statement list.
pub fn emit_lowered_schema(app: &App) -> EmittedFile {
    let funcs = crate::lower::lower_schema_module_functions(&app.schema);
    library::emit_module_file(&funcs, app, PathBuf::from("config/schema.rb"))
}

/// Pair variant of `emit_lowered_schema` — produces both `.rb` and
/// `.rbs`. Replaces `emit_lowered_schema` at call sites that want
/// the typed sidecar emitted alongside.
pub fn emit_lowered_schema_pair(app: &App) -> Vec<EmittedFile> {
    let funcs = crate::lower::lower_schema_module_functions(&app.schema);
    library::emit_module_file_pair(&funcs, app, PathBuf::from("config/schema.rb"))
}

/// Fallible schema emission for a selected database dialect. Packaging must
/// propagate the error rather than silently emit an empty or SQLite schema.
pub fn emit_lowered_schema_pair_for(
    app: &App,
    dialect: crate::emit::shared::schema_sql::Dialect,
) -> Result<Vec<EmittedFile>, String> {
    let funcs = crate::lower::schema_to_library::lower_schema_to_library_functions_for(
        &app.schema, dialect,
    )?;
    Ok(library::emit_module_file_pair(&funcs, app, PathBuf::from("config/schema.rb")))
}

/// Emit `config/routes.rb` in spinel-blog shape — a `RouteTable` module
/// `RouteTable` module exposing the dispatch data via class methods:
/// `RouteTable.table` returns the array of `{method:, pattern:,
/// controller:, action:}` hashes; `RouteTable.root` returns the
/// shorthand `root "c#a"` route (when present). Companion to
/// `emit_lowered_models` and `emit_lowered_schema` for the spinel
/// emit pipeline.
///
/// Method-form (rather than `Routes::TABLE` constant) shares shape
/// with the universal LibraryFunction emit consumed by every other
/// target. Same data shape as Importmap.pins / Schema.statements.
///
/// A small controller-requires header lives at the top of the file
/// because the Spinel runtime expects per-controller files to be
/// loaded by side effect when `config/routes.rb` is required from
/// `main.rb`. The body itself (the data) flows through the
/// universal walker.
pub fn emit_lowered_routes(app: &App) -> EmittedFile {
    let funcs = crate::lower::lower_routes_to_dispatch_functions(app);
    let mut emitted = library::emit_module_file(
        &funcs,
        app,
        PathBuf::from("config/routes.rb"),
    );

    // Prepend require_relative headers for application_controller and
    // each unique controller used by the route table — Spinel runtime
    // loads controllers via require chain rooted at config/routes.rb.
    let flat = crate::lower::routes::flatten_routes(app);
    let mut header = String::new();
    use std::fmt::Write;
    let mut seen: Vec<String> = vec!["application_controller".to_string()];
    // Only controllers the app actually DEFINES. Rails resolves a
    // controller lazily at dispatch, so a route naming one that does not
    // exist raises only when that path is requested; campfire routes
    // `resource :settings` inside `scope module: "rooms"` and ships no
    // `Rooms::SettingsController` at all. Requiring it eagerly turned
    // that dangling route into a hard failure for the WHOLE app — a
    // LoadError at boot on the ruby lane and a build abort on spinel.
    // Skipping it keeps Rails' laziness: the route still dispatches,
    // and it fails where Rails fails it.
    let defined: crate::det::HashSet<&str> =
        app.controllers.iter().map(|c| c.name.0.as_str()).collect();
    // The same holds for the base: a model-only app (a component with
    // no controllers) has no application_controller to require.
    if defined.contains("ApplicationController") {
        writeln!(
            header,
            "require_relative \"../app/controllers/application_controller\""
        )
        .unwrap();
    }
    for r in &flat {
        let class_name = r.controller.0.as_str();
        if !defined.contains(class_name) {
            continue;
        }
        let stem = crate::naming::underscore(class_name);
        if seen.contains(&stem) {
            continue;
        }
        seen.push(stem.clone());
        writeln!(header, "require_relative \"../app/controllers/{stem}\"").unwrap();
    }
    writeln!(header).unwrap();
    emitted.content = format!("{header}{}", emitted.content);
    emitted
}

/// Emit each controller in spinel-blog shape: a `process_action(action_name)`
/// dispatcher (synthesizing before-action filters as conditional calls
/// and case-dispatching to per-action methods) plus the public actions
/// and private filter targets as ordinary methods. Output is one
/// `app/controllers/<name>.rb` per non-synthesized class; tagged
/// synthesized siblings (`<Resource>Params` holders) route to
/// `app/models/<name>.rb` because they're plain holders, not request
/// handlers.
pub fn emit_lowered_controllers(app: &App) -> Vec<EmittedFile> {
    // RSS_ONLY: the spinel tree compiles these, so the inline-json arms
    // stay out — they reference the CRuby-overlay JsonRender and don't
    // type under the AOT compile (CI caught `render(JsonRender.encode
    // (...))` as sp_RbVal-vs-char* in articles_controller). The rss arm
    // reaches only the emitted `Views::<X>.rss` template and the shared
    // `Rails.cache.fetch_str`, both of which this tree already carries.
    let mut lcs = lower_controllers_for_spinel(app, FormatBreadth::RSS_ONLY);
    library::apply_scope_lowering(&mut lcs, app);
    library::apply_helper_lowering(&mut lcs, app);
    // A record handed to a path helper becomes its slug here, not
    // inside the helper (no-op unless a model overrides `to_param`).
    library::apply_route_param_lowering(&mut lcs, app);
    // A helper handed `raw(x)` retargets to a `_raw` clone in which
    // that argument's escaping comes off (no-op without such a site).
    library::apply_raw_helper_monomorphization(&mut lcs, app);
    library::apply_nilsafe_empty_lowering(&mut lcs);
    library::apply_time_format_lowering(&mut lcs, app);
    library::apply_dynamic_render_options_lowering(&mut lcs);
    // Layout wrap at render call sites — the seam where the @ivars a
    // layout reads are in scope. Converged with the CRuby tree
    // (emit_lowered_controllers_with_layout below): the scaffold
    // dispatch ships `controller.body` verbatim on every tree, and a
    // multi-param layout (lobsters takes its 12-14 ivar-derived
    // params) can only be called where those ivars live. No-op for
    // apps without an emitted Layouts/application view.
    library::apply_layout_lowering(&mut lcs, app);
    emit_lowered_controllers_from_lcs(&lcs, app)
}

/// CRuby/JRuby-tree variant: controllers with the layout wrap —
/// `render(Views::X.y(...))` → `render(Views::Layouts.application(
/// Views::X.y(...), @<ivar>…, @flash…))` — so a layout reading
/// controller ivars receives them. Pairs with the ruby_overlay main.rb
/// shipping `controller.body` verbatim. MUST NOT run on the plain
/// spinel target: its dispatch still wraps body-only, and wrapping in
/// both places renders the layout twice (the second pass reads
/// already-consumed content_for slots — CI caught the double wrap as
/// nil crashes in the spinel compare). The ruby/jruby project layers
/// re-emit controllers through this and dedupe last-wins over the
/// spinel-shape files.
pub fn emit_lowered_controllers_with_layout(app: &App) -> Vec<EmittedFile> {
    let lcs = lowered_controller_lcs_with_layout(app);
    emit_lowered_controllers_from_lcs(&lcs, app)
}

/// The controller classes `emit_lowered_controllers_with_layout`
/// writes, before rendering. Shared with `emit::futamura::routes`.
pub(crate) fn lowered_controller_lcs_with_layout(app: &App) -> Vec<LibraryClass> {
    // FULL: the CRuby/JRuby trees ship the overlay (JsonRender) so the
    // widened respond_to arms all resolve; these files dedupe last-wins
    // over the spinel-shape ones.
    let mut lcs = lower_controllers_for_spinel(app, FormatBreadth::FULL);
    library::apply_scope_lowering(&mut lcs, app);
    library::apply_helper_lowering(&mut lcs, app);
    // A record handed to a path helper becomes its slug here, not
    // inside the helper (no-op unless a model overrides `to_param`).
    library::apply_route_param_lowering(&mut lcs, app);
    // A helper handed `raw(x)` retargets to a `_raw` clone in which
    // that argument's escaping comes off (no-op without such a site).
    library::apply_raw_helper_monomorphization(&mut lcs, app);
    library::apply_nilsafe_empty_lowering(&mut lcs);
    library::apply_time_format_lowering(&mut lcs, app);
    library::apply_dynamic_render_options_lowering(&mut lcs);
    library::apply_layout_lowering(&mut lcs, app);
    lcs
}

/// Bulk lower controllers in spinel-shape. Synthesized siblings
/// (`<Resource>Params`) ride alongside the controller classes in the
/// returned vec.
///
/// Lowers models first so the model `ClassInfo`s land in the
/// controller lowerer's registry as extras — the Arel pass needs
/// them to resolve `Article.includes(...).order(...)` chain
/// receivers to a TableRef.
fn lower_controllers_for_spinel(app: &App, format_breadth: FormatBreadth) -> Vec<LibraryClass> {
    // Use lower_models_with_registry (not lower_models_to_library_classes
    // + class_info_from_library_class) because the former returns
    // ClassInfo with `table` set — the Arel pass needs `info.table`
    // to map a Const recv to a TableRef when recognizing chains.
    let (_, model_registry) = crate::lower::model_to_library::lower_models_with_request_finders(
        &app.models,
        &app.schema,
        Vec::new(),
    );
    let model_extras: Vec<(crate::ident::ClassId, crate::analyze::ClassInfo)> =
        model_registry.into_iter().collect();
    // Association graph drives `includes(:assoc)` eager-load lowering
    // (issue #27) — without it the Arel pass drops includes and the
    // reader N+1s.
    let assocs = crate::lower::model_associations::compute_association_graph(app);
    // Route-reachability per controller: a public method is a routable
    // action (gets implicit render + process_action dispatch) only if a
    // route reaches it. Lets base-controller helper/filter methods keep
    // their return value instead of being clobbered by a synthesized render.
    let mut routed: crate::det::HashMap<
        crate::ident::ClassId,
        crate::det::HashSet<crate::ident::Symbol>,
    > = crate::det::HashMap::default();
    for r in crate::lower::flatten_routes(app) {
        routed.entry(r.controller).or_default().insert(r.action);
    }
    let route_ids = crate::lower::routes::helper_id_segments(app);
    crate::lower::lower_controllers_with_arel_views_assocs_and_routes(
        &app.controllers,
        model_extras,
        crate::lower::controller_to_library::LowerControllerOptions {
            ruby_read_values: true,
            schema: Some(&app.schema),
            views: &app.views,
            library_classes: &app.library_classes,
            assocs: &assocs,
            routed_by_controller: Some(&routed),
            format_breadth,
            route_id_segments: Some(&route_ids),
            inferred_params: Some(&app.inferred_method_params),
            models: &app.models,
            wrap_parameters_by_default: app.wrap_parameters_by_default,
            concern_spliced_actions: Some(&app.concern_spliced_actions),
        },
    )
}

/// Render pre-lowered controller `LibraryClass`es to one
/// `app/controllers/<stem>.rb` per non-synthesized class plus
/// `app/models/<stem>.rb` for tagged synthesized siblings.
fn emit_lowered_controllers_from_lcs(
    lcs: &[LibraryClass],
    app: &App,
) -> Vec<EmittedFile> {
    let synthesized: Vec<(String, String)> = lcs
        .iter()
        .filter(|lc| lc.origin.is_some())
        .map(|lc| {
            let name = lc.name.0.as_str().to_string();
            let stem = crate::naming::underscore(&name);
            (name, format!("app/models/{stem}"))
        })
        .collect();

    lcs.iter()
        .flat_map(|lc| {
            let file_stem = crate::naming::underscore(lc.name.0.as_str());
            let out_path = if lc.origin.is_some() {
                PathBuf::from(format!("app/models/{file_stem}.rb"))
            } else {
                PathBuf::from(format!("app/controllers/{file_stem}.rb"))
            };
            let mut files = library::emit_library_class_pair_with_synthesized(
                lc,
                app,
                out_path,
                &synthesized,
            );
            // Sibling error-class declarations captured from the
            // controller's source file (`class LoginFailedError <
            // StandardError; end` before `class LoginController`) —
            // re-declared ahead of the controller class so the
            // actions' raise/rescue sites resolve.
            if lc.origin.is_none() {
                if let Some(ctrl) =
                    app.controllers.iter().find(|c| c.name == lc.name)
                {
                    if !ctrl.sibling_classes.is_empty() {
                        for f in files.iter_mut() {
                            if f.path.extension().is_some_and(|e| e == "rb") {
                                prepend_sibling_classes(
                                    &mut f.content,
                                    &ctrl.sibling_classes,
                                    lc.name.0.as_str(),
                                );
                            }
                        }
                    }
                }
            }
            files
        })
        .collect()
}

/// Insert `class <Name> < <Parent>; end` declaration lines directly
/// above the controller's own `class` line (mirroring where the source
/// file put them). No-op when the class line isn't found.
fn prepend_sibling_classes(
    content: &mut String,
    siblings: &[crate::dialect::SiblingClass],
    class_name: &str,
) {
    let marker = format!("class {class_name}");
    let Some(pos) = content.find(&marker) else { return };
    let mut decls = String::new();
    for sibling in siblings {
        decls.push_str(&format!(
            "class {} < {}; end\n",
            sibling.name.as_str(),
            sibling.parent.as_str()
        ));
    }
    decls.push('\n');
    content.insert_str(pos, &decls);
}

/// Lower each html-format `app.views` entry through `view_to_library`
/// and emit the resulting `LibraryClass` as a Ruby source file under
/// `app/views/<dir>/<base>.rb`. Output is the universal post-lowering
/// shape: a `Views::<Plural>` module with one `def self.<action>(args)`
/// per view, body in `io = String.new ; io << ViewHelpers.x(...) ; io`
/// form. See `project_universal_post_lowering_ir.md`.
///
/// json-format views (`*.json.jbuilder`) go through
/// `emit_lowered_jbuilder_views` — same shape, separate file per
/// template named `<base>_json.rb` to avoid colliding with the html
/// sibling. Both files reopen the same `Views::<Plural>` module.
pub fn emit_lowered_views(app: &App) -> Vec<EmittedFile> {
    let (html_views, lcs) = lowered_view_lcs(app);
    html_views
        .iter()
        .zip(lcs.iter())
        .flat_map(|(v, lc)| {
            let out_path = view_output_path(&crate::lower::view::view_output_stem(v));
            library::emit_library_class_pair(lc, app, out_path)
        })
        .collect()
}

/// The html view classes `emit_lowered_views` writes, paired with their
/// views, before rendering. Shared with `emit::futamura::routes`.
pub(crate) fn lowered_view_lcs(app: &App) -> (Vec<&crate::dialect::View>, Vec<LibraryClass>) {
    // Lower every view through ONE per-app context, then run the
    // Ruby-family passes over the whole group at once — per-view pass
    // application rebuilt the scope/assoc/helper registries for every
    // template, which is quadratic in app size (the mastodon playground
    // transpile blew its budget on it).
    let vctx = crate::lower::ViewLowerCtx::new(app);
    let html_views: Vec<&crate::dialect::View> = app
        .views
        .iter()
        .filter(|v| crate::lower::view::lowers_through_view_path(v))
        .collect();
    let mut lcs: Vec<LibraryClass> = html_views.iter().map(|v| vctx.lower(v)).collect();
    // Normalize scope chains opened in the template itself — lobsters'
    // _listdetail runs `story.merged_stories.not_deleted.includes(...)`
    // — then resolve bare app-helper calls (`avatar_img(...)` →
    // `ApplicationHelper.avatar_img(...)`). Scope lowering is a strict
    // no-op for scope-free apps (the blog), and a view LC is never a
    // model, so only the call-site rewrite arm runs.
    library::apply_scope_lowering(&mut lcs, app);
    library::apply_helper_lowering(&mut lcs, app);
    // Whatever `apply_helper_lowering` did not claim as an app helper
    // and the route table DOES answer is a route helper called outside
    // URL position — `rails_blob_path` in a `data:` hash. It gets the
    // same `RouteHelpers.` receiver `link_to`'s URL argument gets.
    crate::lower::route_helper_receiver::qualify_lcs(&mut lcs, app);
    // A record handed to a path helper becomes its slug here, not
    // inside the helper (no-op unless a model overrides `to_param`).
    library::apply_route_param_lowering(&mut lcs, app);
    // A helper handed `raw(x)` retargets to a `_raw` clone in which
    // that argument's escaping comes off (no-op without such a site).
    library::apply_raw_helper_monomorphization(&mut lcs, app);
    library::apply_time_current_lowering(&mut lcs, app);
    library::apply_duration_lowering(&mut lcs, app);
    // Nullable columns hydrate to nil on the Ruby tree — synthesized
    // `.empty?` predicate forms in view bodies must tolerate it.
    library::apply_nilsafe_empty_lowering(&mut lcs);
    library::apply_time_format_lowering(&mut lcs, app);
    library::apply_dynamic_render_options_lowering(&mut lcs);
    // A view whose own namespace shadows a top-level app class
    // (`Views::Stats` over `Stats`) must reference it absolutely, or
    // Ruby resolves the inner module first. No-op unless a view
    // directory shares a name with a class.
    library::apply_constant_rooting(&mut lcs, app, library::RootingScope::AppAndRuntime);
    // Give each view a `<name>_into(io, …)` variant and point
    // view-to-view appends at it, so a nested fragment is written into
    // the caller's buffer instead of being materialised as a string
    // that the caller then appends. Ruby-family only: the construct
    // needs a mutable buffer a callee can write through.
    crate::lower::view_buffer_passing::apply(&mut lcs);
    // And the same for a block-taking tag helper: `io << Helper.tag(a)
    // do … end` becomes `Helper.tag_into(io, a) do … end`, the block
    // appending into the view's own buffer. The variants are added on
    // the helper side (`emit_library_class_decls`) from the same table.
    crate::lower::tag_block_passing::apply_views(&mut lcs, &crate::lower::tag_block_passing::variants(app));
    (html_views, lcs)
}

/// Lower each json-format `app.views` entry through `jbuilder_to_library`
/// and emit the resulting `LibraryClass` as a Ruby source file under
/// `app/views/<dir>/<base>_json.rb`. Method body uses the same
/// io-accumulator shape as html views; values flow through
/// `JsonBuilder.encode_value`.
pub fn emit_lowered_jbuilder_views(app: &App) -> Vec<EmittedFile> {
    app.views
        .iter()
        .filter(|v| v.jbuilder)
        .flat_map(|v| {
            let mut lc = crate::lower::lower_jbuilder_to_library_class(v, app);
            library::apply_zoned_json_time_lowering(std::slice::from_mut(&mut lc), app);
            let out_path = jbuilder_view_output_path(v.name.as_str());
            library::emit_library_class_pair(&lc, app, out_path)
        })
        .collect()
}

/// Map a view name (`articles/index`, `articles/_article`,
/// `layouts/application`) to the output path under `app/views/`.
/// Partials retain their leading underscore in the basename so the
/// require-relative graph keeps working without a separate alias step.
fn view_output_path(view_name: &str) -> PathBuf {
    PathBuf::from(format!("app/views/{view_name}.rb"))
}

/// Jbuilder counterpart of `view_output_path`. The `_json` suffix
/// matches the lowered method name and prevents path collision with
/// the html sibling: `articles/_article.json.jbuilder` →
/// `app/views/articles/_article_json.rb`.
fn jbuilder_view_output_path(view_name: &str) -> PathBuf {
    PathBuf::from(format!("app/views/{view_name}_json.rb"))
}

/// Spinel-shape emit: lowered IR rendered as runnable Ruby. Composes
/// the five `emit_lowered_*` functions into a single project — schema,
/// routes, models, controllers, views — laid out under the spinel
/// target's directory shape (app/, config/, test/). The natural
/// validation target of the lowering pipeline: CRuby executes the
/// output until spinel grows its own test runner.
///
/// Every emitted test file ends with an explicit per-test driver
/// shim (`render_autorun_shim`). Mirrors the role Crystal's
/// `macro inherited` plays at Crystal compile time and TS's
/// `discover_tests(klass)` plays at JS load time. The shim works
/// under both CRuby and spinel-AOT — same file, same mechanism, no
/// reliance on Minitest's at_exit autorun (which spinel can't see and
/// which would have CRuby double-run every test if combined with the
/// shim).
pub fn emit_spinel(app: &App) -> Vec<EmittedFile> {
    emit_with_assertions(app, crate::lower::test_module_to_library::inline_assertions::AssertionMode::Inline)
}

pub fn emit_with_assertions(
    app: &App,
    assertion_mode: crate::lower::test_module_to_library::inline_assertions::AssertionMode,
) -> Vec<EmittedFile> {
    let _indexes = library::install_lookup_indexes(app);
    let mut files = Vec::new();
    crate::timings::lap_start();
    files.extend(emit_lowered_schema_pair(app));
    crate::timings::lap("emit app: schema");
    // routes.rb gets a require-relative header prepended (see
    // emit_lowered_routes); emit the .rb via that path, then derive
    // the .rbs sidecar from the same funcs without the header.
    let route_funcs = crate::lower::lower_routes_to_dispatch_functions(app);
    files.push(emit_lowered_routes(app));
    files.extend(rbs_only_from_funcs(
        &route_funcs,
        app,
        PathBuf::from("config/routes.rb"),
    ));
    crate::timings::lap("emit app: routes");
    let importmap_funcs = crate::lower::lower_importmap_to_library_functions(app);
    if !importmap_funcs.is_empty() {
        files.extend(library::emit_module_file_pair(
            &importmap_funcs,
            app,
            PathBuf::from("config/importmap.rb"),
        ));
    } else {
        // The scaffold main.rb `require_relative`s this file
        // unconditionally — spinel's static require graph has no
        // begin/rescue escape hatch (the CRuby overlay's conditional
        // require is exactly that hatch). A source app without an
        // importmap still needs the file to exist; a bare reopen keeps
        // the `runtime/importmap.rb` fallback's pins/entry standing.
        files.push(EmittedFile {
            path: PathBuf::from("config/importmap.rb"),
            content: "# No importmap in the source app; the runtime/importmap\n\
                      # fallback's empty pins stand. This file exists because the\n\
                      # scaffold main.rb requires it unconditionally (spinel AOT\n\
                      # resolves the whole require graph statically).\n\
                      module Importmap\n\
                      end\n"
                .to_string(),
        });
    }
    crate::timings::lap("emit app: importmap");

    // Per-app Rails::Application reopen (config/application.rb) — the
    // app's real config methods (`read_only?`, `name`, `domain`) so
    // `Rails.application.<m>` dispatches to them instead of the empty
    // runtime shim. Same unconditional-require contract as importmap:
    // apps without one get a bare reopen stub.
    match emit_rails_application(app) {
        Some(f) => files.push(f),
        None => files.push(EmittedFile {
            path: PathBuf::from("config/application.rb"),
            content: "# No config/application.rb methods in the source app; the\n\
                      # runtime's empty Rails::Application shim stands. This file\n\
                      # exists because the scaffold main.rb requires it\n\
                      # unconditionally (spinel AOT resolves the whole require\n\
                      # graph statically).\n\
                      module Rails\n\
                      \x20\x20class Application\n\
                      \x20\x20end\n\
                      end\n"
                .to_string(),
        }),
    }
    crate::timings::lap("emit app: application");
    files.extend(emit_lowered_models(app));
    crate::timings::lap("emit app: models");
    if let Some(f) = library::emit_relation_scope_delegates(app) {
        files.push(f);
    }
    crate::timings::lap("emit app: scope_delegates");
    files.extend(emit_lowered_controllers(app));
    crate::timings::lap("emit app: controllers");
    files.extend(emit_lowered_views(app));
    crate::timings::lap("emit app: views");
    files.extend(emit_lowered_jbuilder_views(app));
    crate::timings::lap("emit app: jbuilder");

    // RouteHelpers — `app/route_helpers.rb` with `def self.<x>_path(args)`
    // per named route. Generated from `app.routes`; supersedes the
    // hand-written `runtime/ruby/action_view/route_helpers.rb` (which
    // is being kept for backward compat until callers migrate).
    let route_helper_funcs = crate::lower::lower_routes_to_library_functions(app);
    // The segment interpolations stay bare. `to_param` used to be
    // applied here, to every segment of every helper — which reads as
    // Rails-faithful but isn't, because our segments are declared
    // `String` and Rails' are untyped: under AOT the record was already
    // coerced away by the time the body ran. It now happens at the call
    // site, where the record still is one
    // (`library::apply_route_param_lowering`).
    if !route_helper_funcs.is_empty() {
        files.extend(library::emit_module_file_pair(
            &route_helper_funcs,
            app,
            PathBuf::from("app/route_helpers.rb"),
        ));
    }
    crate::timings::lap("emit app: route_helpers");

    // Seeds — `db/seeds.rb` as a `Seeds.run` module method. Mirrors
    // the TS pipeline; was previously missing from spinel emit.
    let seeds_funcs = crate::lower::lower_seeds_to_library_functions(app);
    if !seeds_funcs.is_empty() {
        files.extend(library::emit_module_file_pair(
            &seeds_funcs,
            app,
            PathBuf::from("db/seeds.rb"),
        ));
    }
    crate::timings::lap("emit app: seeds");

    // Lower fixtures up-front so the test_helper renderer can list them
    // explicitly (replacing the source-side `Object.constants.sort.each`
    // + `Object.const_get` scan, which violates the spinel subset).
    // Same `fixture_lcs` is reused below for the per-fixture file emit
    // and for `fixture_extras` synthesized siblings.
    //
    // The PostgreSQL CRuby tree (TestReset::FixtureSets below) loads each
    // set as Rails does, as raw table rows; the other trees build and save
    // a model per record.
    let minitest = assertion_mode == crate::lower::test_module_to_library::inline_assertions::AssertionMode::Minitest;
    let fixture_set_reset = minitest
        && app.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql);
    let mut fixture_lcs = if fixture_set_reset {
        crate::lower::fixture_to_library::lower_fixtures_to_raw_row_library_classes(app)
    } else {
        crate::lower::lower_fixtures_to_library_classes(app)
    };
    // Same vestige as the view pipeline above, for the same reason:
    // fixture classes are synthesized here, after the shared post-
    // analyze hooks have already run over the App, so an ERB tag's
    // `<%= 1.hour.ago %>` reaches emit ungrounded unless it takes the
    // rewrite on the way out. No-op for fixtures without ERB.
    library::apply_duration_lowering(&mut fixture_lcs, app);
    // Rails' fixture accessor is MEMOIZED per test; ours was a fresh
    // `find` per call, so a mutation and a save through two calls
    // touched two different objects. Ruby-family only — the cache is
    // class state on a module, which the strict targets' fixture
    // modules have no home for. See `apply_fixture_memoization`.
    library::apply_fixture_memoization(&mut fixture_lcs);

    // Per-test truncation, shared by `SchemaSetup.reset!` in the test
    // helper and by each test file's autorun shim below. ONE list: the
    // two disagreeing was the bug — the shim truncated correctly while
    // the helper's stale `Article`/`Comment` pair no-opped, so
    // `reset!` re-loaded every fixture onto rows it had not removed
    // (campfire: `UNIQUE constraint failed: accounts.singleton_guard`).
    //
    // Only models with a backing schema table get an
    // `_adapter_truncate` (synthesized by push_adapter_methods under
    // the same gate). Abstract parents like ApplicationRecord have no
    // table → no truncate method → calling it falls through to
    // ActiveRecord::Base and raises NotImplementedError. Skip them.
    let mut truncate_lines: Vec<String> = app
        .models
        .iter()
        .filter(|m| app.schema.tables.contains_key(&m.table.0))
        .map(|m| format!("{}._adapter_truncate", m.name.0.as_str()))
        .collect();
    // A table with NO model still holds rows a test wrote, and
    // reloading fixtures on top of them is the same bug the shared list
    // above exists to prevent. campfire's `message_search_index` is the
    // case: an fts5 virtual table the app keeps in step by hand
    // (`Message::Searchable`), so every message insert added a row that
    // nothing cleared, and the second fixture load hit `constraint
    // failed` on a rowid it had already used.
    //
    // `DELETE FROM` rather than a synthesized `_adapter_truncate`:
    // there is no model to synthesize it onto, and a virtual table has
    // no AUTOINCREMENT sequence to reset.
    let modelled: crate::det::HashSet<&str> =
        app.models.iter().map(|m| m.table.0.as_str()).collect();
    for (name, _table) in &app.schema.tables {
        if !modelled.contains(name.as_str()) {
            truncate_lines.push(format!("Db.exec(\"DELETE FROM {}\")", name.as_str()));
        }
    }
    // The PostgreSQL CRuby tree isolates tests as Rails does instead:
    // ActiveRecord::TestFixtures (runtime/active_record_test_fixtures.rb)
    // loads the fixture sets once per process, deleting only the fixture
    // tables, each through its model's writing pool, and every test runs
    // in a transaction that rolls back. Truncating all of the app's
    // tables before each test touched every model (Core: 1,421 of them,
    // an autoload each) and ran an unmodelled table's DELETE on
    // ActiveRecord::Base's pool, which Core points at a replica.
    let test_reset = if fixture_set_reset {
        TestReset::FixtureSets(fixture_set_rows(app, &fixture_lcs))
    } else {
        TestReset::Truncate(truncate_lines.clone())
    };

    // Test bootstrap. The canonical content (LOAD_PATH wiring,
    // SqliteAdapter setup, RequestDispatch + ActionResponse +
    // SchemaSetup modules) lives at `runtime/spinel/test/test_helper.rb`
    // so the standalone fixture and overlay flows share one source.
    // The renderer substitutes the three per-app pieces the source
    // carries blog-shaped stand-ins for — see `render_test_helper`.
    // Emitted unconditionally — every spinel project carries the helper
    // even when no test files are produced yet.
    files.push(EmittedFile {
        path: PathBuf::from("test/test_helper.rb"),
        content: render_test_helper(&fixture_lcs, &test_reset, minitest),
    });

    // Test fixtures — one `<Plural>Fixtures` LibraryClass per YAML file
    // under `test/fixtures/`, rendered to `test/fixtures/<plural>.rb`.
    // Mirrors the TS pattern at `typescript.rs:302-306`. Available for
    // emitted tests to consume via `ArticlesFixtures.one()` (the call
    // shape `lower_test_modules_to_library_classes` rewrites
    // `articles(:one)` to).
    for lc in &fixture_lcs {
        let stem = fixture_file_stem(lc.name.0.as_str());
        let out_path = PathBuf::from(format!("test/fixtures/{stem}.rb"));
        files.extend(library::emit_library_class_pair(lc, app, out_path));
    }
    crate::timings::lap("emit app: fixtures");

    // Test modules — lower each `XTest` class into a `LibraryClass`
    // whose methods are `def test_<snake>` blocks (one per `test "..."`
    // macro), then render to `test/models/<stem>_test.rb` or
    // `test/controllers/<stem>_test.rb` depending on the class name
    // suffix. Mirrors `typescript.rs:308-325`. Empty extras for now
    // (the lowerer registers minitest baseline + framework stubs +
    // fixture helpers internally); broader extras assembly can land
    // when a test body needs more than the lowerer's own registry.
    if !app.test_modules.is_empty() {
        // Each `<Plural>Fixtures` LibraryClass surfaces its label
        // methods (typed `() -> Class<Model>`) and `_fixtures_load!`
        // through the registry so test bodies that bind a local from
        // `ArticlesFixtures.one` get the parent's class type — which
        // is what the has-many `.create`/`.build` rewrite consults to
        // de-magic `article.comments.create(...)` into
        // `Comment.create(article_id: article.id, ...)`.
        // App models ride along beside the fixture classes. A test body
        // names them constantly — `users(:david).rooms.last` — and with
        // no entry the has_many reader resolves to nothing, so the
        // record never types and every type-directed rewrite downstream
        // (the route-helper id projection among them) declines.
        //
        // Inside a diagnostic scope, and the collected diagnostics are
        // DROPPED: this is a second call to the model lowerer purely for
        // its registry, and letting it report again duplicated 58
        // `unsupported` entries into campfire's ledger — the same gaps,
        // counted twice, which is worse than not knowing.
        let (model_registry, _dup_diags) = crate::emit::diagnostics::scope(|| {
            let (_, reg) =
                crate::lower::model_to_library::lower_models_with_request_finders(&app.models, &app.schema, Vec::new());
            reg
        });
        let fixture_extras: Vec<(crate::ident::ClassId, crate::analyze::ClassInfo)> = fixture_lcs
            .iter()
            .map(|lc| (lc.name.clone(), crate::lower::class_info_from_library_class(lc)))
            .chain(model_registry)
            .collect();
        // A test body's `_url` answers absolute, on the session's host,
        // as the Ruby family's jbuilder `_url` does (see the pass).
        let test_modules =
            crate::lower::test_module_to_library::ruby_family_absolute_test_urls(&app.test_modules);
        let mut test_lowered = crate::lower::test_module_to_library::lower_test_modules_with_assertions(
            &test_modules,
            &app.fixtures,
            &app.models,
            fixture_extras,
            &crate::lower::routes::helper_id_segments(app),
            assertion_mode,
            // The CRuby / JRuby trees' has_many readers keep their
            // target (`library::apply_collection_memoization`).
            expr::native_keywords(),
            &crate::lower::shadowed_fixture_accessors(&test_modules, &app.library_classes),
        );
        // A test body writes the same relation chains an app body does —
        // `@room.messages.ordered`, `user.push_subscriptions.find_by(…)`,
        // `room.memberships.grant_to(users)`. The has_many reader hands
        // back the arel-inlined Array, so every one of those needs the
        // association-seeded Relation that `apply_scope_lowering` builds
        // (`Relation.new(Message).where(room_id: @room.id).ordered`). It
        // runs over models, controllers, helpers and views; the test
        // classes were the one lowered family it never saw, so the exact
        // chain that compiled inside a controller raised NoMethodError
        // one file over.
        //
        // This is the same drift wall 21 found on the REGISTRY side (test
        // bodies typed against a registry with no scopes in it). Same two
        // sides, same fix: one seam, both callers.
        //
        // Inner classes ride along — a `class Validatable` declared in a
        // test body is an ordinary lowered class and may query too. No
        // entry in `app.models` matches a test class, so the model-only
        // half (scope-method synthesis, `push_scope_variants`) is inert
        // here and only the call-site rewrite runs.
        {
            let mut test_lcs: Vec<crate::dialect::LibraryClass> = test_lowered
                .iter()
                .flat_map(|m| {
                    std::iter::once(m.test_class.clone()).chain(m.inner_classes.iter().cloned())
                })
                .collect();
            // A record Array at a Relation-typed parameter, restated as
            // the Relation over those records — BEFORE the scope
            // lowering, which is what turns the `M.where` it writes
            // into a seeded Relation. See the module.
            crate::lower::records_to_relation_arg::rewrite_test_classes(&mut test_lcs, app);
            library::apply_scope_lowering(&mut test_lcs, app);
            // `1.minute.ago` in a class a test file carries (campfire's
            // `PushServiceTestHelper.sign`), as library classes get it.
            library::apply_duration_lowering(&mut test_lcs, app);
            // A test class nests under the class it tests
            // (`class User … class BotTest`), which shadows exactly as
            // the model-side concern does: campfire's
            // `User::BotTest#test_deliver_message_by_webhook` names
            // `Bot::WebhookJob`, and inside `User` that is `User::Bot`,
            // whose `WebhookJob` does not exist. Same pass, same guard.
            library::apply_constant_rooting(
                &mut test_lcs,
                app,
                library::RootingScope::AppAndRuntime,
            );
            let mut it = test_lcs.into_iter();
            for m in &mut test_lowered {
                m.test_class = it.next().expect("one lowered class per test module");
                for inner in &mut m.inner_classes {
                    *inner = it.next().expect("one lowered class per inner class");
                }
            }
        }
        // Fixture classes (`ArticlesFixtures`, etc.) live at
        // `test/fixtures/<plural>.rb` — outside the model/controller
        // require-resolution paths the library emitter knows. Pass them
        // as synthesized siblings so any test body that references one
        // gets an explicit `require_relative "../fixtures/<plural>"`.
        let declaration_programs = declaration_programs(app);
        let test_helper_anchors: Vec<(String, &str)> = app
            .test_helper_programs
            .iter()
            .filter_map(|(source, site)| {
                let program = app.file_programs.iter().find(|p| &p.source == source)?;
                Some((library::file_program_anchor(app, program), site.as_str()))
            })
            .collect();
        let fixture_siblings: Vec<(String, String)> = fixture_lcs
            .iter()
            .map(|lc| {
                let name = lc.name.0.as_str().to_string();
                let stem = fixture_file_stem(&name);
                (name, format!("test/fixtures/{stem}"))
            })
            .collect();
        // Explicit reset+fixture-load sequence used by the autorun
        // shim. The Minitest driver has none: TestBase#setup's
        // `SchemaSetup.reset!` is its reset, which a copy in the driver
        // only ran a second time, before it. Mirrors Crystal's `test_setup.cr` (which emits a static
        // `RoundhouseTest.fixture_loaders = [-> { ArticlesFixtures.
        // _fixtures_load!; nil }, …]`). The dynamic `Object.constants`
        // walk that `FixtureLoader.load_all!` uses under CRuby is not
        // reachable under spinel-AOT; materializing the list at emit
        // time is the AOT-friendly analog.
        let mut reset_lines: Vec<String> = truncate_lines.clone();
        for lc in &fixture_lcs {
            reset_lines.push(format!("{}._fixtures_load!", lc.name.0.as_str()));
        }
        // Two test classes can flatten to one stem: campfire's
        // `ActionTextAttachmentTest` (test/models) and
        // `ActionText::AttachmentTest` (test/lib/rails_ext) are both
        // `action_text_attachment`, and the second written would
        // silently replace the first. A namespaced class whose stem
        // is taken spells its `::` as `__` instead — decided from the
        // whole set, so the outcome does not depend on emit order.
        let stem_counts: crate::det::HashMap<String, usize> = test_lowered
            .iter()
            .map(|l| test_file_stem(l.test_class.name.0.as_str()))
            .fold(crate::det::HashMap::default(), |mut m, s| {
                *m.entry(s).or_insert(0) += 1;
                m
            });
        let out_path_of = |lc: &LibraryClass| {
            let class_name = lc.name.0.as_str();
            let mut stem = test_file_stem(class_name);
            if stem_counts.get(&stem).copied().unwrap_or(0) > 1 && class_name.contains("::") {
                stem = class_name
                    .strip_suffix("Test")
                    .unwrap_or(class_name)
                    .split("::")
                    .map(crate::naming::snake_case)
                    .collect::<Vec<_>>()
                    .join("__");
            }
            PathBuf::from(format!("test/{}/{stem}_test.rb", test_subdir(lc)))
        };
        // Every test module gets a file of its own. A class reopened in a
        // second authored file (`class SecondTest` in pair_test.rb and
        // again_test.rb, which Ruby merges into one suite) is a second
        // module with the same name and so the same path, and the second
        // written silently replaced the first, tests and all. A path
        // already taken takes the next free `_<n>` suffix, in module
        // order (authored files sorted, classes in source order).
        let out_paths: Vec<PathBuf> = {
            let mut taken: crate::det::HashSet<PathBuf> = crate::det::HashSet::default();
            test_lowered
                .iter()
                .map(|l| {
                    let base = out_path_of(&l.test_class);
                    let mut path = base.clone();
                    let mut n = 2;
                    while !taken.insert(path.clone()) {
                        let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("test");
                        let stem = stem.strip_suffix("_test").unwrap_or(stem);
                        path = base.with_file_name(format!("{stem}_{n}_test.rb"));
                        n += 1;
                    }
                    path
                })
                .collect()
        };
        // A test class may subclass another the app defines (a base in
        // test_helper.rb: `class Sqlite3QueryHelperTest <
        // SqliteTestCompat::TestCase`). Its file has to be loaded first:
        // the first file that declares it.
        let mut test_paths: crate::det::HashMap<&str, PathBuf> = crate::det::HashMap::default();
        for (l, path) in test_lowered.iter().zip(&out_paths) {
            test_paths.entry(l.test_class.name.0.as_str()).or_insert_with(|| path.clone());
        }
        // Minitest runs a test class's inherited `test_` methods too
        // (`runnable_methods` reads `public_instance_methods(true)`), so
        // `PermissionGeneratorTest::ValidateInitialTest <
        // PermissionGeneratorTest` runs the outer class's tests again,
        // as its own. The driver lists them: own first, then each app
        // test-class ancestor's, nearest first, each name once.
        let test_names_of = |lc: &LibraryClass| -> Vec<String> {
            let own = |lc: &LibraryClass| -> Vec<String> {
                lc.methods
                    .iter()
                    .filter(|m| matches!(m.receiver, MethodReceiver::Instance) && m.name.as_str().starts_with("test_"))
                    .map(|m| m.name.as_str().to_string())
                    .collect()
            };
            let mut names = own(lc);
            let mut seen: crate::det::HashSet<String> = names.iter().cloned().collect();
            let mut current = lc.parent.clone();
            for _ in 0..32 {
                let Some(parent) = current.take() else { break };
                let defs: Vec<&LibraryClass> = test_lowered
                    .iter()
                    .map(|l| &l.test_class)
                    .filter(|c| c.name == parent && c.name != lc.name)
                    .collect();
                if defs.is_empty() {
                    break;
                }
                for def in &defs {
                    for name in own(def) {
                        if seen.insert(name.clone()) {
                            names.push(name);
                        }
                    }
                }
                current = defs[0].parent.clone();
            }
            names
        };
        // A class declares tests as it loads (`TestModule::tests_at_load`),
        // or inherits from one that does: Minitest runs those too.
        let loads_tests = |tm: &crate::dialect::TestModule| -> bool {
            let mut current = Some(tm);
            for _ in 0..32 {
                let Some(m) = current.take() else { break };
                if m.tests_at_load {
                    return true;
                }
                current = m.parent.as_ref().and_then(|parent| app.test_modules.iter().find(|o| o.name == *parent && o.name != m.name));
            }
            false
        };
        let mut index: std::collections::BTreeMap<String, Vec<serde_json::Value>> = std::collections::BTreeMap::new();
        let test_parents: crate::det::HashSet<&str> = test_lowered
            .iter()
            .filter_map(|l| l.test_class.parent.as_ref().map(|p| p.0.as_str()))
            .collect();
        // Test modules that declare no tests (a support class such as
        // `MockGCSInterface`): loading one runs nothing, so a test file
        // that names it can require it.
        let support_paths: crate::det::HashMap<&str, PathBuf> = test_lowered
            .iter()
            .zip(&out_paths)
            .filter(|(l, _)| test_names_of(&l.test_class).is_empty())
            .map(|(l, path)| (l.test_class.name.0.as_str(), path.clone()))
            .collect();
        // The same by the authored file that declares them, for a test
        // file's `require` of it (`authored_requires`).
        let mut support_sources: std::collections::BTreeMap<&str, Vec<PathBuf>> = std::collections::BTreeMap::new();
        for ((l, path), module) in test_lowered.iter().zip(&out_paths).zip(&app.test_modules) {
            if let Some(source) = module.source_file.as_deref().filter(|_| test_names_of(&l.test_class).is_empty()) {
                support_sources.entry(source).or_default().push(path.clone());
            }
        }
        let program_anchors: crate::det::HashSet<String> =
            app.file_programs.iter().map(|program| library::file_program_anchor(app, program)).collect();
        assert_eq!(test_lowered.len(), app.test_modules.len(), "one lowered class per test module");
        for ((lowered, out_path), module) in test_lowered.iter().zip(out_paths).zip(&app.test_modules) {
            let lc = &lowered.test_class;
            let class_name = lc.name.0.as_str();
            // Map both `ActiveSupport::TestCase` (Rails app tests) and
            // `Minitest::Test` (framework's own tests) to roundhouse-
            // owned `TestBase` (defined in test_helper.rb). Insulates
            // emitted tests from Minitest's class hierarchy —
            // assert_*/refute_* are already inlined as `raise` by the
            // inline_assertions lowerer, and the per-test shim
            // replaces Minitest's autorun. The zero-arg `X.new` the
            // shim emits requires this rewrite: `Minitest::Test#
            // initialize(name)` requires a method-name argument, so
            // inheriting from it directly breaks the shim under
            // CRuby. Ruby-target-specific rewrite, lives here rather
            // than in the lowerer.
            let mut lc_for_emit = lc.clone();
            lc_for_emit.parent = lc.parent.as_ref().map(emitted_test_parent);
            let test_rb_path = out_path.clone();
            let mut emitted = library::emit_library_class_decl_with_synthesized(
                &lc_for_emit,
                app,
                out_path,
                &fixture_siblings,
            );
            emitted.content = require_declarations_through_programs(&emitted.content, &test_rb_path, &declaration_programs);
            // The class body's other statements (`use_gcs`, `let(:x) { … }`),
            // as written and in source order: those above every definition
            // after the class's mixins, the others at the end of the class.
            if !module.body_statements.is_empty() {
                if minitest {
                    emitted.content = splice_test_body_statements(&emitted.content, class_name, &module.body_statements, module.source_file.as_deref().unwrap_or(class_name), &test_rb_path);
                } else {
                    for statement in &module.body_statements {
                        crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
                            statement.expr.span,
                            None,
                            "test_class_body_statement",
                            format!("`{class_name}`'s class-body statement runs only in the Minitest (CRuby) test emit; it will not run"),
                        ));
                    }
                }
            }
            // Class-body constant assignments (`TABLE = [...]`) go back
            // INSIDE the class, at the top of its body — the source's
            // own scope. They used to hoist to file scope so bare-name
            // reads in test methods resolved, which CRuby's lexical
            // lookup allows; it also made every one of them a
            // TOP-LEVEL constant, and campfire's vips_loader_policy_test
            // declares `SVG = "<svg …>"` where `runtime/gem_facades.rb`
            // already has `module SVG` — a reassignment warning on CRuby
            // and "SVG is not a module" on spinel, which refused the
            // whole file. Inside the class, a bare read in a method of
            // that class resolves lexically to the class's own constant
            // first, on both runtimes, and a name the file shares with
            // the runtime is no longer a collision.
            if !lowered.constants.is_empty() {
                let mut consts_block = String::new();
                for (name, value) in &lowered.constants {
                    let value_s = expr::with_emitted_file(&test_rb_path, || {
                        super::ruby::expr::emit_expr(value)
                    });
                    writeln!(consts_block, "  {} = {}", name.as_str(), value_s).unwrap();
                }
                consts_block.push('\n');
                emitted.content = splice_after_class_header(&emitted.content, class_name, &consts_block);
            }
            // Inner classes and modules (declared inside the test
            // class body in Ruby — `class Validatable; include …; end`
            // inside ValidationsTest — or beside it) go above the test
            // class body. Each keeps its Ruby name: one in the test
            // class body is written nested in a reopen of the test
            // class (`ValidationsTest::Validatable`), so a bare read in
            // a test method resolves to it lexically, as in the source,
            // and its `name` is the source's.
            if !lowered.inner_classes.is_empty() {
                let mut companion_block = String::new();
                for inner in &lowered.inner_classes {
                    let inner_rb_path = PathBuf::from("test")
                        .join(format!("{}_inner.rb", test_file_stem(inner.name.0.as_str())));
                    let inner_emitted = library::emit_library_class_decl_with_synthesized(
                        inner,
                        app,
                        inner_rb_path.clone(),
                        &fixture_siblings,
                    );
                    companion_block.push_str(&strip_require_headers(&inner_emitted.content));
                    companion_block.push('\n');
                    // RBS sidecar for the inner class. Its `.rb` body is
                    // spliced into the test file above (no standalone
                    // `.rb`), but RBS matches classes by name globally,
                    // so a standalone `sig/test/<stem>_inner.rbs` applies
                    // to the spliced `Article`. Without it spinel infers
                    // the stand-in's `[]` itself and mis-compiles the
                    // heterogeneous `Integer | String` return as one C
                    // type (matz/spinel#1255).
                    files.push(library::emit_rbs_sidecar(inner, app, &inner_rb_path));
                }
                // Splice companions ahead of the main class body but
                // after the file-level `require_relative` headers
                // produced by `emit_library_class_decl_with_synthesized`.
                emitted.content = splice_after_requires(&emitted.content, &companion_block);
            }
            // Test files need the bootstrap (minitest/autorun + runtime
            // requires + adapter setup) before any model require resolves;
            // prepend the require before the body-derived require headers.
            //
            // Also prepend explicit `require_relative` for every fixture
            // file. Test_helper's `FixtureLoader.load_all!` walks
            // `Object.constants` for `*Fixtures` modules; under spinel
            // AOT (no dynamic `Dir[…]` + `require`), each fixture must be
            // statically required from somewhere in the require chain
            // that spinel follows from the test file root. Injecting at
            // every test file guarantees coverage regardless of which
            // fixtures the body itself names.
            let mut preamble = String::from("require_relative \"../test_helper\"\n");
            // The app helper's programs (`App::test_helper_programs`), where
            // the test's `require "test_helper"` ran the app's helper natively:
            // Core's `require "platform/testing/test_helper"`, its test
            // runtime, helpers and initializers, before the class is defined.
            // A load that raises is named and the test still runs, as a
            // file program's statement is (`RoundhouseAutoload.file_statement`).
            for (anchor, site) in &test_helper_anchors {
                writeln!(preamble, "RoundhouseAutoload.file_statement({site:?}) {{ require_relative \"../../app/{anchor}\" }}").unwrap();
            }
            // The file's own top-level loads, after its `require
            // "test_helper"`, in source order.
            for line in authored_requires(app, module, &support_sources, &program_anchors) {
                preamble.push_str(&line);
            }
            if let Some(parent_path) = lc.parent.as_ref().and_then(|p| test_paths.get(p.0.as_str())) {
                let rel = parent_path.strip_prefix("test").unwrap_or(parent_path).with_extension("");
                writeln!(preamble, "require_relative \"../{}\"", rel.display()).unwrap();
            }
            // A test-support class the test names (`MockGCSInterface`,
            // `MockAlpha::Bucket`) is emitted in a test file of its own,
            // which nothing else loads: the source loaded it with a
            // `require_relative` (or from `config/environments/test.rb`)
            // that the emitted test does not carry.
            let mut support_requires: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut paths: std::collections::BTreeSet<Vec<String>> = std::collections::BTreeSet::new();
            for method in &lc.methods {
                library::walk_const_paths(&method.body, &mut paths);
            }
            for (_, value) in &lowered.constants {
                library::walk_const_paths(value, &mut paths);
            }
            for statement in &module.body_statements {
                library::walk_const_paths(&statement.expr, &mut paths);
            }
            for path in &paths {
                let path: Vec<&str> =
                    path.iter().map(|s| s.trim_start_matches("::")).filter(|s| !s.is_empty()).collect();
                for len in 1..=path.len() {
                    let prefix = path[..len].join("::");
                    if prefix == class_name {
                        continue;
                    }
                    if let Some(support_path) = support_paths.get(prefix.as_str()) {
                        let rel = support_path.strip_prefix("test").unwrap_or(support_path).with_extension("");
                        support_requires.insert(format!("require_relative \"../{}\"\n", rel.display()));
                    }
                }
            }
            for line in support_requires {
                preamble.push_str(&line);
            }
            for (_, anchor) in &fixture_siblings {
                // Test files live at `test/{models,controllers}/…`,
                // fixture anchors at `test/fixtures/<stem>`, so the
                // relative path from a test file's dir is always
                // `../fixtures/<stem>`.
                let stem = anchor
                    .strip_prefix("test/fixtures/")
                    .unwrap_or(anchor.as_str());
                writeln!(preamble, "require_relative \"../fixtures/{stem}\"").unwrap();
            }
            emitted.content = format!("{preamble}{}", emitted.content);
            // A base with no tests of its own is loaded by its subclasses'
            // files; a driver there would report on it once per require.
            let test_names = test_names_of(lc);
            let at_load = loads_tests(module);
            let is_bare_base = test_parents.contains(class_name) && test_names.is_empty() && !at_load;
            if !is_bare_base && minitest {
                emitted.content.push_str(&render_minitest_shim(&lc_for_emit, &test_names, at_load));
            } else if !is_bare_base {
                emitted.content.push_str(&render_autorun_shim(&lc_for_emit, &test_names, &reset_lines));
            }
            index.entry(module.source_file.clone().unwrap_or_default()).or_default().push(serde_json::json!({
                "class": class_name,
                "file": emitted.path.display().to_string(),
                "tests": if is_bare_base { Vec::new() } else { test_names.clone() },
                // Tests the class declares as it loads are not listed:
                // a runner maps an id of the class's to its method by
                // name, and the loaded class has it or not.
                "tests_at_load": at_load && !is_bare_base,
            }));
            let rbs_path = emitted.path.clone();
            files.push(emitted);
            // RBS sidecar for the test class — describes the test
            // methods' (untyped, () -> void) signatures plus any
            // inferred helpers. No preamble / autorun shim — RBS is
            // pure type info, the runtime bootstrap doesn't apply.
            files.push(library::emit_rbs_sidecar(&lc_for_emit, app, &rbs_path));
        }
        // The emitted test index: each authored test file and the
        // emitted file, class and test names of every class it declares.
        // One authored file can become several emitted ones (a file per
        // class), so a runner given the authored file reads this to run
        // all of them; a class ingest refused is named in the ingest
        // ledger and is absent here.
        let sources: Vec<serde_json::Value> = index
            .into_iter()
            .map(|(source, classes)| serde_json::json!({ "source": source, "classes": classes }))
            .collect();
        files.push(EmittedFile {
            path: PathBuf::from("test/roundhouse_tests.json"),
            content: serde_json::to_string_pretty(&serde_json::json!({
                "schema": "roundhouse-test-index/v1",
                // "minitest": each class's `tests` are the methods
                // Minitest runs natively (ActiveSupport's `test_` name,
                // Core's `_tag:` segments included), so an emitted id is
                // the native id. "sanitized": identifiers made from the
                // test name (lowercase, other runs as one `_`).
                "test_names": if assertion_mode == crate::lower::test_module_to_library::inline_assertions::AssertionMode::Minitest { "minitest" } else { "sanitized" },
                "sources": sources,
            }))
            .expect("test index json")
                + "\n",
        });
    }

    // `app/models.rb` aggregator — loads every emitted app/models/*.rb.
    // Model files carry requires only for their LOAD-time deps
    // (superclass, includes, class-body constant refs); method-body
    // references between them resolve because main.rb / test_helper.rb
    // require this file before any dispatch (see the edge
    // classification in `library::emit_library_class_decl_with_
    // synthesized`). Emitted here so every consumer of `emit_spinel`
    // (the toolchain scratch trees included) satisfies those
    // unconditional requires; the project pipeline regenerates it
    // after `emit_library` adds the ingested support classes
    // (`apply_models_aggregator`).
    let mut model_anchors: Vec<String> = files
        .iter()
        .filter_map(|f| {
            let p = f.path.to_str()?;
            if p.starts_with("app/models/") && p.ends_with(".rb") {
                Some(p.strip_suffix(".rb").unwrap().strip_prefix("app/").unwrap().to_string())
            } else {
                None
            }
        })
        .collect();
    model_anchors.sort();
    let mut aggregator = String::from(
        "# Loads every model/support class under app/models/. Generated\n\
         # from the emitted tree (see emit_spinel / apply_models_aggregator).\n\
         # Each file requires its own LOAD-time deps (superclass, includes,\n\
         # class-body constants), so the order here is only for legibility;\n\
         # method-body references between these files rely on this\n\
         # aggregator having run by dispatch time.\n",
    );
    for anchor in &model_anchors {
        writeln!(aggregator, "require_relative {anchor:?}").unwrap();
    }
    files.push(EmittedFile {
        path: PathBuf::from("app/models.rb"),
        content: aggregator,
    });

    crate::timings::lap("emit app: tests and aggregators");
    files
}

/// Render RBS for a `LibraryFunction` group without emitting the .rb.
/// Used by `emit_spinel` for files whose .rb emit has custom
/// post-processing (e.g. `config/routes.rb`'s require-header
/// prepend) — the .rb flows through the bespoke path while the
/// `.rbs` is derived once from the same lowered functions.
fn rbs_only_from_funcs(
    funcs: &[crate::dialect::LibraryFunction],
    app: &crate::App,
    rb_path: PathBuf,
) -> Vec<EmittedFile> {
    if funcs.is_empty() {
        return Vec::new();
    }
    vec![library::emit_rbs_sidecar_from_funcs(funcs, app, &rb_path)]
}

/// Explicit per-test driver appended to each emitted test file.
/// Materializes the `reset → fixture-load → setup → test_X →
/// teardown` lifecycle for every `test_*` instance method in `lc`.
/// Replaces Minitest's at_exit autorun (not reachable under spinel-
/// AOT; would double-run with the shim under CRuby). See
/// spinel-AOT.
///
/// `reset_lines` carries pre-rendered `<Model>._adapter_truncate` +
/// `<X>Fixtures._fixtures_load!` lines (materialized once per emit;
/// see the caller in emit_spinel_with). Inlined rather than calling
/// `SchemaSetup.reset!` because the latter delegates to
/// `FixtureLoader.load_all!`, which walks `Object.constants`
/// dynamically — not reachable under spinel-AOT.
///
/// EVERY test runs, and a failing one does not take the rest of the
/// file with it. The straight-line form this replaced reported only a
/// file's FIRST failing test, which made the campfire suite 52
/// booleans over 240 tests — a file one wall from green and a file ten
/// walls from green were the same row, so a first-error histogram
/// ranked work by what fails FIRST rather than by what BLOCKS (two
/// consecutive mis-ranked probes; see the coverage-ladder notes).
///
/// Both contracts of the old shape survive:
///   * an all-green run prints EXACTLY `<Class>: <N> tests passed`,
///     which is what `project.rs`'s `.expected` snapshots pin;
///   * any failure still exits nonzero (the trailing `raise`), which
///     `make spinel-test` consumes as a fail signal.
///
/// `setup` runs inside the guard (minitest reports a setup error
/// against the test); `teardown` runs outside it so it fires whether
/// or not the body raised, and a raise from teardown itself still
/// aborts the file rather than being swallowed.
/// The CRuby (Minitest-assertion) driver. It runs when this file is the
/// program, or when the program is a loader rather than an emitted test
/// (`RoundhouseTestRunner.drive?`): a subclass's file requires its
/// parent's, and the parent's tests are not the subclass file's to run
/// (Minitest runs them once under the parent, and again, inherited,
/// under the subclass, which `test_names` lists).
///
/// It carries no reset of its own. The test's fixture phase is the
/// runner's (`RoundhouseTestRunner.run_one`): TestBase#setup's
/// `SchemaSetup.reset!`, and on the PostgreSQL tree
/// ActiveRecord::TestFixtures' load-once and transaction. A copy of the
/// reset here ran every truncate and fixture load a second time per test
/// (on Core, 1,421 model truncates, each naming and so autoloading its
/// model).
///
/// A class that declares tests as it loads (`at_load`) adds the ones its
/// loaded body defined (`RoundhouseTestRunner.loaded_tests`).
fn render_minitest_shim(lc: &LibraryClass, test_names: &[String], at_load: bool) -> String {
    // A native name is any text (`test_#call_ok?`, `test_x_#@y`): a
    // literal that does not interpolate.
    let methods: Vec<String> = test_names.iter().map(|name| expr::ruby_str_literal(name)).collect();
    let mut s = String::from("\nif RoundhouseTestRunner.drive?(__FILE__)\n");
    if at_load {
        writeln!(s, "  RoundhouseTestRunner.run({0}, RoundhouseTestRunner.loaded_tests({0}, [{1}]))", lc.name.0, methods.join(", ")).unwrap();
    } else {
        writeln!(s, "  RoundhouseTestRunner.run({}, [{}])", lc.name.0, methods.join(", ")).unwrap();
    }
    s.push_str("end\n");
    s
}

fn render_autorun_shim(lc: &LibraryClass, test_names: &[String], reset_lines: &[String]) -> String {
    let class_name = lc.name.0.as_str();
    let test_methods: Vec<&str> = test_names.iter().map(String::as_str).collect();

    let mut s = String::from(
        "\n# Spinel AOT autorun shim — see emit/ruby.rs::render_autorun_shim.\n",
    );
    // An Integer counter and per-failure `puts`, deliberately: no
    // array accumulator and no block, so the shim leans on nothing
    // beyond `begin/rescue`, `+`, `to_s` and string concatenation —
    // the surface spinel-AOT already compiles in lowered test bodies
    // (`assert_raises`) and `runtime/spinel`.
    writeln!(s, "__failed = 0").unwrap();
    writeln!(s, "__skipped = 0").unwrap();
    for tm in &test_methods {
        // Zero-arg `.new` — mirrors Crystal's `@type.new.test_X` shape.
        // Spinel doesn't propagate inherited `Minitest::Test#
        // initialize(name)` to subclasses, and the @name slot isn't
        // used by any assertion the lowered tests reach, so dropping
        // it is safe.
        writeln!(s, "__t = {class_name}.new").unwrap();
        writeln!(s, "__ok = true").unwrap();
        writeln!(s, "begin").unwrap();
        // Truncate + fixture-load INSIDE the guard: they are this
        // shim's spelling of minitest's fixture setup, and a fixture
        // that fails to load is a per-test error in Rails. Outside
        // the guard it would abort the file and hide every test
        // behind it — the exact failure mode this shim exists to end.
        for line in reset_lines {
            writeln!(s, "  {line}").unwrap();
        }
        writeln!(s, "  __t.setup").unwrap();
        writeln!(s, "  __t.{tm}").unwrap();
        // `rescue Exception`, not a bare rescue: a bare one catches
        // StandardError, and an assertion library may deliberately put
        // its failure OUTSIDE that. `Mocha::ExpectationError <
        // Exception` exactly so a test's own `rescue => e` cannot
        // swallow an unmet expectation — and this shim's rescue was
        // swallowing nothing, it was MISSING it, so the error walked
        // out of the file and took the remaining tests with it.
        // Minitest's own runner rescues Exception here for the same
        // reason.
        // SKIP BEFORE FAIL, and it is a third outcome rather than a
        // lenient second. Minitest's `skip` says "this test cannot be
        // run HERE" — campfire's vips policy test skips the formats
        // the local libvips did not compile in — and neither answer
        // the harness could otherwise give is true: counting it failed
        // reads as a transpiler gap (it took the macOS number to 16 of
        // 17 and a Linux runner's to 15, for the same emit), and
        // counting it passed claims a check that never ran. So it is
        // reported on its own line and taken out of BOTH sides of the
        // tally by `scripts/campfire-suite`.
        writeln!(s, "rescue TestSkipped => __e").unwrap();
        writeln!(s, "  __ok = false").unwrap();
        writeln!(s, "  __skipped = __skipped + 1").unwrap();
        writeln!(s, "  puts {:?} + __e.message", format!("SKIP {class_name}#{tm}: ")).unwrap();
        writeln!(s, "rescue Exception => __e").unwrap();
        writeln!(s, "  __ok = false").unwrap();
        writeln!(s, "  __failed = __failed + 1").unwrap();
        writeln!(s, "  puts {:?} + __e.message", format!("FAIL {class_name}#{tm}: ")).unwrap();
        writeln!(s, "end").unwrap();
        // Teardown gets a guard of its OWN, for the reason the comment
        // above gives for the reset lines: unguarded, a raise here
        // aborts the FILE and hides every test behind it. It did —
        // `mocha_verify` raises on an unmet `expects`, which is a
        // failure of THIS test, and one of them took
        // `membership_test` from 8 of 9 to 0 of 9.
        //
        // Its own begin/end rather than moving it above the rescue:
        // teardown has to run even when the test raised, or a stub
        // installed on a real object outlives the test that made it.
        // `__ok` keeps a test that already failed from being counted
        // twice — `__failed` is the tally's numerator and must not
        // exceed the test count.
        writeln!(s, "begin").unwrap();
        writeln!(s, "  __t.teardown").unwrap();
        writeln!(s, "rescue Exception => __e").unwrap();
        writeln!(s, "  if __ok").unwrap();
        writeln!(s, "    __failed = __failed + 1").unwrap();
        writeln!(
            s,
            "    puts {:?} + __e.message",
            format!("FAIL {class_name}#{tm}: ")
        )
        .unwrap();
        writeln!(s, "  end").unwrap();
        writeln!(s, "end").unwrap();
    }
    let n = test_methods.len();
    writeln!(s, "if __failed > 0").unwrap();
    writeln!(
        s,
        "  raise {:?} + __failed.to_s + {:?}",
        format!("{class_name}: "),
        format!(" of {n} tests failed")
    )
    .unwrap();
    writeln!(s, "end").unwrap();
    // The skip count rides the ALL-GREEN line, because a file with
    // skips and no failures is the case the tally has to read: the
    // suite subtracts it from both the numerator and the denominator,
    // so a run on a host missing an optional library reports what it
    // actually checked. A file with no skips prints the line
    // `project.rs`'s `.expected` snapshots have always pinned, byte
    // for byte — the suffix appears only when there is something to
    // say.
    writeln!(s, "if __skipped > 0").unwrap();
    writeln!(
        s,
        "  puts {:?} + __skipped.to_s + {:?}",
        format!("{class_name}: "),
        " tests skipped"
    )
    .unwrap();
    writeln!(s, "end").unwrap();
    // The passed count is the test methods MINUS the skips, so the
    // line never claims a check that did not run. With no skips it is
    // the literal `project.rs`'s `.expected` snapshots pin, byte for
    // byte.
    writeln!(
        s,
        "puts {:?} + ({n} - __skipped).to_s + {:?}",
        format!("{class_name}: "),
        " tests passed"
    )
    .unwrap();
    s
}

/// Each declarations file (`models/<stem>`) of a support file's file
/// program => that program's anchor, for a file exactly one program
/// declares (`ProgramItem::Declared`).
fn declaration_programs(app: &App) -> std::collections::BTreeMap<String, String> {
    let mut by_file: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for program in &app.file_programs {
        let anchor = library::file_program_anchor(app, program);
        for item in &program.items {
            if let crate::app::ProgramItem::Declared(names) = item {
                for name in names {
                    by_file
                        .entry(format!("models/{}", library_file_stem(name.0.as_str(), app)))
                        .or_default()
                        .insert(anchor.clone());
                }
            }
        }
    }
    by_file
        .into_iter()
        .filter_map(|(file, programs)| (programs.len() == 1).then(|| (file, programs.into_iter().next().unwrap())))
        .collect()
}

/// A test file's `require_relative` of a declarations file whose source
/// has a file program (`declaration_programs`), as a require of that
/// program. The test's source required the source file (Core's `require
/// "support/helpers/apps/test_helper"`, `require
/// "platform/essentials/testing/toggle_flags_helper"`), and Ruby loading
/// it runs the whole file: its requires and every declaration it makes,
/// not only the one the test names. Loaded alone, a declarations file
/// leaves out its siblings (`ToggleFlagsHelper::ClassMethods`, which the
/// module's `included` hook extends its includer with) where no autoload
/// reaches them.
fn require_declarations_through_programs(
    content: &str,
    test_path: &std::path::Path,
    programs: &std::collections::BTreeMap<String, String>,
) -> String {
    if programs.is_empty() {
        return content.to_string();
    }
    let dir: Vec<String> = test_path
        .parent()
        .map(|d| d.to_string_lossy().split('/').filter(|p| !p.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let mut out = String::with_capacity(content.len());
    let mut in_headers = true;
    for line in content.split_inclusive('\n') {
        let text = line.trim_end_matches('\n');
        // The header block: the synthesized `require_relative`s, with
        // stdlib `require "bigdecimal"` lines among them.
        if in_headers && !text.starts_with("require_relative \"") && !text.starts_with("require \"") && !text.trim().is_empty() {
            in_headers = false;
        }
        let rel = in_headers.then(|| text.strip_prefix("require_relative \"").and_then(|r| r.strip_suffix('"'))).flatten();
        let Some(rel) = rel else {
            out.push_str(line);
            continue;
        };
        let mut target: Vec<String> = dir.clone();
        for part in rel.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    target.pop();
                }
                other => target.push(other.to_string()),
            }
        }
        let anchor = target.join("/");
        match anchor.strip_prefix("app/").and_then(|a| programs.get(a)) {
            Some(program) => {
                let common = dir.iter().zip(format!("app/{program}").split('/')).take_while(|(a, b)| a.as_str() == *b).count();
                let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), dir.len() - common).collect();
                parts.extend(format!("app/{program}").split('/').skip(common).map(str::to_string));
                out.push_str(&format!("require_relative \"{}\"\n", parts.join("/")));
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Drop the leading `require_relative "..."` lines from an emitted
/// class file's content, leaving just the class body. Used when
/// splicing a companion class into a host file — the host already
/// emits its own require headers, and the companion's headers would
/// either duplicate or land in the wrong order.
/// The preamble lines that run a test file's top-level loads
/// (`TestModule::file_requires`), each under `RoundhouseAutoload.
/// file_statement` at its site, which names a load that fails and goes
/// on, as a support file's program statement does. A `require` of a
/// feature runs as written: in the test environment a carried
/// test-support file answers its feature on the test load path
/// (`project::test_load_path_features`), a compiled gem's its own, and
/// any other goes on to Kernel's. `require "test_helper"` is the
/// preamble's own. A file a relative load names is, in this order, a
/// support class's emitted test file, its feature on the test load
/// path, or its emitted file program; a file none of them carries, or a
/// load that names no literal file, raises its reason where it runs.
fn authored_requires(
    app: &App,
    module: &crate::dialect::TestModule,
    support_sources: &std::collections::BTreeMap<&str, Vec<PathBuf>>,
    program_anchors: &crate::det::HashSet<String>,
) -> Vec<String> {
    use crate::dialect::TestRequireTarget;
    let Some(source) = module.source_file.as_deref() else { return Vec::new() };
    // The helper's own loads are its programs' (`App::test_helper_programs`).
    if source == "test/test_helper.rb" {
        return Vec::new();
    }
    let from_test = |path: &Path| {
        let rel = path.strip_prefix("test").unwrap_or(path).with_extension("");
        format!("require_relative \"../{}\"", rel.display())
    };
    let support = |rel: &str| support_sources.get(rel).map(|paths| paths.iter().map(|p| from_test(p)).collect::<Vec<_>>());
    let mut out = Vec::new();
    for require in &module.file_requires {
        let site = format!("{source}:{}", require.line);
        let statements: Vec<String> = match &require.target {
            TestRequireTarget::Feature(feature) => {
                let feature = feature.strip_suffix(".rb").unwrap_or(feature);
                if feature == "test_helper" {
                    continue;
                }
                app.test_load_roots
                    .iter()
                    .find_map(|root| support(&format!("{root}{feature}.rb")))
                    .unwrap_or_else(|| vec![format!("require {feature:?}")])
            }
            TestRequireTarget::Relative(path) => {
                let rel = normalize_relative(Path::new(source).parent().unwrap_or(Path::new("")), path);
                let rel = if rel.ends_with(".rb") { rel } else { format!("{rel}.rb") };
                if rel == "test/test_helper.rb" {
                    continue;
                }
                let anchor = format!("models/roundhouse_files/{}", rel.strip_suffix(".rb").unwrap_or(&rel));
                let feature = app.test_load_roots.iter().find_map(|root| rel.strip_prefix(root.as_str()));
                if let Some(lines) = support(&rel) {
                    lines
                } else if let Some(feature) = feature {
                    vec![format!("require {:?}", feature.strip_suffix(".rb").unwrap_or(feature))]
                } else if program_anchors.contains(&anchor) {
                    vec![format!("require_relative \"../../app/{anchor}\"")]
                } else {
                    let why = format!("roundhouse: `require_relative {path:?}` loads {rel}, which the emitted tree does not carry");
                    vec![format!("raise LoadError, {why:?}")]
                }
            }
            TestRequireTarget::Unsupported(call) => {
                let why = format!("roundhouse: `{call}` is not supported (all targets): it loads no literal file");
                vec![format!("raise NotImplementedError, {why:?}")]
            }
        };
        for statement in statements {
            out.push(format!("RoundhouseAutoload.file_statement({site:?}) {{ {statement} }}\n"));
        }
    }
    out
}

/// `path` from the app-relative directory `dir`, with its `.` and `..`
/// segments resolved (a `..` above the app root stays).
fn normalize_relative(dir: &Path, path: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for segment in dir.to_string_lossy().split('/').chain(path.split('/')) {
        match segment {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| p != "..") => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}

fn strip_require_headers(content: &str) -> String {
    let lines = content.lines();
    let mut body_start = 0usize;
    let mut idx = 0usize;
    for line in content.lines() {
        idx += line.len() + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("require_relative ") || trimmed.is_empty() {
            body_start = idx;
            continue;
        }
        break;
    }
    let _ = lines; // silence lint
    content[body_start..].to_string()
}

/// Insert `block` into `content` right after the trailing
/// `require_relative` headers (and the blank line that separates
/// them from the body). When `content` has no requires, the block
/// is prepended.
/// Insert `block` directly after the test class's `class … <` header
/// line — the first line of the class body — so a class-scoped
/// constant is declared before any method that reads it.
///
/// A namespaced test (`module Push; class SubscriptionTest < TestBase`)
/// has its header indented under the module, so the match is on the
/// trimmed line and the block takes the header's indentation. `block`'s
/// lines arrive at two spaces — the body indent of a top-level class.
/// Write a test class's carried class-body statements
/// (`TestModule::body_statements`) into its emitted class: the ones above
/// every definition right after the class's mixin lines (its `include`s
/// and `extend`s, which the library emit writes first), the others just
/// before its `end`, each group in source order. Before the first one
/// below a `setup do` (`teardown do`) block, the class tells the runner
/// so (`RoundhouseTestCallbacks.after_compiled_blocks`). Each runs in
/// `RoundhouseTestCallbacks.class_body_statement`, which names one that
/// raises (a macro whose module did not load) and loads the rest of the
/// class, as a file program's statement is (`RoundhouseAutoload.
/// file_statement`); a local variable write stays bare, in the class
/// body's scope, for the statements after it.
fn splice_test_body_statements(content: &str, class_name: &str, statements: &[crate::dialect::TestBodyStatement], source: &str, rb_path: &std::path::Path) -> String {
    let (mut setup_marked, mut teardown_marked) = (false, false);
    let mut render = |group: &[&crate::dialect::TestBodyStatement]| -> String {
        let mut out = String::new();
        for statement in group {
            for (below, marked, chain) in [
                (statement.after_setup_blocks, &mut setup_marked, "setup"),
                (statement.after_teardown_blocks, &mut teardown_marked, "teardown"),
            ] {
                if below && !*marked {
                    writeln!(out, "RoundhouseTestCallbacks.after_compiled_blocks(self, :{chain})").unwrap();
                    *marked = true;
                }
            }
            let text = expr::with_emitted_file(rb_path, || expr::emit_expr(&statement.expr));
            let local_write = matches!(&*statement.expr.node, crate::expr::ExprNode::Assign { target: crate::expr::LValue::Var { .. }, .. });
            if local_write {
                out.push_str(&text);
                out.push('\n');
            } else if text.contains('\n') {
                writeln!(out, "RoundhouseTestCallbacks.class_body_statement(self, {:?}) do", format!("{}:{}", statement.source.as_deref().unwrap_or(source), statement.line)).unwrap();
                for line in text.lines() {
                    if line.trim().is_empty() {
                        out.push('\n');
                    } else {
                        writeln!(out, "  {line}").unwrap();
                    }
                }
                out.push_str("end\n");
            } else {
                writeln!(out, "RoundhouseTestCallbacks.class_body_statement(self, {:?}) {{ {text} }}", format!("{}:{}", statement.source.as_deref().unwrap_or(source), statement.line)).unwrap();
            }
        }
        out
    };
    let head: Vec<_> = statements.iter().filter(|s| !s.after_definitions).collect();
    let tail: Vec<_> = statements.iter().filter(|s| s.after_definitions).collect();
    let head = render(&head);
    let tail = render(&tail);
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let Some(header) = test_class_header(&lines, class_name) else {
        debug_assert!(false, "no class header to splice body statements into:\n{content}");
        return content.to_string();
    };
    let indent = &lines[header][..lines[header].len() - lines[header].trim_start().len()];
    let body_indent = format!("{indent}  ");
    let end = lines[header + 1..]
        .iter()
        .position(|l| l.trim_end_matches(['\n', '\r']) == format!("{indent}end"))
        .map(|at| header + 1 + at)
        .unwrap_or(lines.len());
    // The mixin lines below the header, and the comments and blank
    // lines among them.
    let mut after_mixins = header + 1;
    for (at, line) in lines.iter().enumerate().take(end).skip(header + 1) {
        let trimmed = line.trim();
        if ["include ", "extend ", "prepend "].iter().any(|m| trimmed.starts_with(m)) {
            after_mixins = at + 1;
        } else if !(trimmed.is_empty() || trimmed.starts_with('#')) {
            break;
        }
    }
    let indented = |block: &str| -> String {
        let mut out = String::new();
        for line in block.split_inclusive('\n') {
            if !line.trim().is_empty() {
                out.push_str(&body_indent);
            }
            out.push_str(line);
        }
        out
    };
    let mut out = String::with_capacity(content.len() + head.len() + tail.len());
    for (at, line) in lines.iter().enumerate() {
        if at == after_mixins && !head.is_empty() {
            out.push_str(&indented(&head));
            out.push('\n');
        }
        if at == end && !tail.is_empty() {
            out.push('\n');
            out.push_str(&indented(&tail));
        }
        out.push_str(line);
    }
    if end == lines.len() && !tail.is_empty() {
        out.push_str(&indented(&tail));
    }
    out
}

fn splice_after_class_header(content: &str, class_name: &str, block: &str) -> String {
    let mut out = String::with_capacity(content.len() + block.len());
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let header = test_class_header(&lines, class_name);
    let mut done = false;
    for (at, line) in lines.iter().enumerate() {
        out.push_str(line);
        if !done && Some(at) == header {
            let indent = &line[..line.len() - line.trim_start().len()];
            for b in block.split_inclusive('\n') {
                if b.trim().is_empty() {
                    out.push_str(b);
                } else {
                    out.push_str(indent);
                    out.push_str(b);
                }
            }
            done = true;
        }
    }
    debug_assert!(done, "no class header to splice constants after:\n{content}");
    out
}

/// The line that opens `class_name` itself in its emitted test file. A
/// nested test class (`class PlaybookActionTest::ApprovedTacticAction
/// StillRunningTest < PlaybookActionTest` in the source) is written
/// inside a reopen of its outer class, so the first `class` line is the
/// OUTER one. Its constants and class-body statements belong in the
/// inner class: on the outer, each split file of the same authored file
/// reassigned `PlaybookActionTest::TACTIC_INSTANCE_ID` ("already
/// initialized constant") and every sibling read the last file's value.
/// Walks the `module`/`class` headers along the name's segments
/// (a header may name several, `class A::B`); falls back to the first
/// `class` line.
fn test_class_header(lines: &[&str], class_name: &str) -> Option<usize> {
    let segments: Vec<&str> = class_name.trim_start_matches("::").split("::").collect();
    let mut matched = 0usize;
    let mut header = None;
    for (at, line) in lines.iter().enumerate() {
        if matched == segments.len() {
            break;
        }
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("class ").or_else(|| trimmed.strip_prefix("module ")) else {
            continue;
        };
        let name = rest
            .split(|c: char| c.is_whitespace() || c == ';' || c == '<')
            .next()
            .unwrap_or("")
            .trim_start_matches("::");
        let parts: Vec<&str> = name.split("::").collect();
        if segments[matched..].starts_with(&parts) {
            matched += parts.len();
            if matched == segments.len() && trimmed.starts_with("class ") {
                header = Some(at);
            }
        }
    }
    header.or_else(|| lines.iter().position(|l| l.trim_start().starts_with("class ")))
}

fn splice_after_requires(content: &str, block: &str) -> String {
    let mut split_at = 0usize;
    let mut idx = 0usize;
    let mut last_require_end = 0usize;
    for line in content.lines() {
        let line_end = idx + line.len() + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("require_relative ") {
            last_require_end = line_end;
        } else if trimmed.is_empty() && last_require_end > 0 && last_require_end == idx {
            // Blank line directly after the last require — splice
            // after this blank so spacing reads naturally.
            split_at = line_end;
            break;
        } else if !trimmed.is_empty() {
            split_at = last_require_end;
            break;
        }
        idx = line_end;
    }
    if split_at == 0 {
        split_at = last_require_end;
    }
    if split_at == 0 {
        return format!("{block}\n{content}");
    }
    let (head, tail) = content.split_at(split_at);
    format!("{head}{block}\n{tail}")
}

/// `ArticlesFixtures` → `articles` (strip Fixtures suffix, snake_case).
/// Mirrors `typescript.rs:fixture_file_stem` so the emitted file path
/// reads naturally without redundant suffixes.
fn fixture_file_stem(class_name: &str) -> String {
    let stem = class_name.strip_suffix("Fixtures").unwrap_or(class_name);
    crate::naming::snake_case(stem)
}

/// Render `test/test_helper.rb`, substituting the source file's
/// `Object.constants.sort.each` + `Object.const_get` scan in
/// `FixtureLoader.load_all!` with explicit per-fixture calls. The
/// scan was a CRuby-only convenience (the spinel subset rejects
/// `Object.const_get` and `Object.constants`); we already know the
/// fixture set at emit time, so emit the explicit list and let the
/// source file keep the const_get fallback for hand-written non-emit
/// uses.
///
/// Class names are sorted alphabetically — the same ordering the
/// source-side scan used (which approximates parent-before-child for
/// the `Articles → Comments` belongs_to shape; topological ordering
/// is the principled fix once a fixture set exposes a non-alphabetic
/// dependency).
/// Render `test/test_helper.rb` for an emitted app.
///
/// The source file at `runtime/spinel/test/test_helper.rb` has to run
/// verbatim under the source-side `framework_ruby_tests_pass` gate,
/// where there is no app and no `main.rb` — so it carries a
/// blog-shaped stand-in for each of the three things that are really
/// per-app, and each is substituted here. Everything blog-specific in
/// this harness is one of these three; if a fourth appears, it belongs
/// here rather than in the source file.
///
///   1. the runtime require chain → `require_relative "../main"`
///   2. `SchemaSetup.reset!`'s truncate list → the app's own models
///   3. `FixtureLoader.load_all!`'s constant scan → explicit calls
///
/// On the CRuby (Minitest) tree the require chain is preceded by Rails'
/// test base classes, and on the PostgreSQL one (2) is the fixture sets
/// ActiveRecord::TestFixtures loads (`TestReset::FixtureSets`).
fn render_test_helper(fixture_lcs: &[LibraryClass], reset: &TestReset, minitest: bool) -> String {
    // (1) The require chain. The source list is a hand-maintained
    // SUBSET that stopped tracking the runtime as it grew: by
    // 2026-08-16 `boot.rb` required 23 files this did not, so a
    // harness-loaded app died at `uninitialized constant ActionText`
    // — a require gap that reads exactly like a modeling gap.
    //
    // Defers to `boot.rb`, which is the app's whole chain in its
    // curated order and NOTHING that starts anything. Deliberately not
    // `main.rb`: the spinel-AOT main.rb boots the server
    // unconditionally (the `__FILE__ == $PROGRAM_NAME` guard cannot
    // work under AOT — see its own comment), so a test that required it
    // opened the dev database and died at `sqlite3_open(…) failed (14)`
    // before running an assertion. boot.rb is the half both can share.
    const REQUIRE_BLOCK: &str = "require_relative \"../runtime/base64\"";
    const REQUIRE_BLOCK_END: &str = "require_relative \"../app/models\"";

    // (2) Per-test isolation. `_adapter_truncate` exists only on models
    // with a backing schema table; the caller applies that gate.
    const TRUNCATE_BLOCK: &str = "    Article._adapter_truncate if defined?(Article)
    Comment._adapter_truncate if defined?(Comment)";

    // (3) Fixture loading — spinel's AOT model has no runtime constant
    // table, so the scan is materialized at emit time.
    const SCAN_BLOCK: &str = "    Object.constants.sort.each do |c|
      next unless c.to_s.end_with?(\"Fixtures\")
      mod = Object.const_get(c)
      next unless mod.is_a?(Module)
      next unless mod.respond_to?(:_fixtures_load!)
      mod._fixtures_load!
    end";

    debug_assert!(
        SPINEL_TEST_HELPER.contains(SCAN_BLOCK),
        "runtime/spinel/test/test_helper.rb FixtureLoader.load_all! body \
         changed; update SCAN_BLOCK in render_test_helper"
    );
    debug_assert!(
        SPINEL_TEST_HELPER.contains(TRUNCATE_BLOCK),
        "runtime/spinel/test/test_helper.rb SchemaSetup.reset! truncate \
         list changed; update TRUNCATE_BLOCK in render_test_helper"
    );

    let mut out = SPINEL_TEST_HELPER.to_string();

    // (1) — replace the whole span, comments included, so no stale
    // rationale survives next to the line that replaced it.
    let (Some(start), Some(end)) = (out.find(REQUIRE_BLOCK), out.find(REQUIRE_BLOCK_END)) else {
        debug_assert!(
            false,
            "runtime/spinel/test/test_helper.rb require chain changed; \
             update REQUIRE_BLOCK/_END in render_test_helper"
        );
        return out;
    };
    let mut boot = String::new();
    if minitest {
        boot.push_str(MINITEST_TEST_CASES);
    }
    boot.push_str(
        "# The app's whole boot chain, in its curated order: the runtime,\n\
         # config/schema, config/routes, app/models, app/views. boot.rb\n\
         # starts nothing — main.rb requires it too, then adds the server.\n\
         # One owner for the order; a harness-local copy of the list\n\
         # drifts, and did (23 files short, read as a modeling gap).\n\
         require_relative \"../boot\"\n\
         # The CGI/1.1 request parser. boot.rb does not carry it — the\n\
         # tep server never parses a query string through it — but this\n\
         # harness does (`CgiIo.parse_form_into`, below), so a `?query=`\n\
         # test was the only thing in the tree to reach an undefined\n\
         # constant. Test-only, like the resolver and HTTP stubs beside it.\n\
         require_relative \"../runtime/cgi_io\"\n\
         # Rails' generated config/environments/test.rb:\n\
         # `config.action_controller.allow_forgery_protection = false`.\n\
         # boot.rb loaded the ruby family's forgery check; a test posts\n\
         # without a token, as Rails' own integration tests do.\n\
         ActionController::Base.allow_forgery_protection = false",
    );
    out.replace_range(start..end + REQUIRE_BLOCK_END.len(), &boot);

    // (2)
    match reset {
        TestReset::Truncate(truncate_lines) => {
            let truncate = if truncate_lines.is_empty() {
                String::from("    # no table-backed models")
            } else {
                truncate_lines
                    .iter()
                    .map(|line| format!("    {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            out = out.replace(TRUNCATE_BLOCK, &truncate);
        }
        TestReset::FixtureSets(_) => {
            let reset_body = format!("{TRUNCATE_BLOCK}\n    FixtureLoader.load_all!\n");
            debug_assert!(
                out.contains(&reset_body),
                "runtime/spinel/test/test_helper.rb SchemaSetup.reset! body \
                 changed; update the FixtureSets reset in render_test_helper"
            );
            out = out.replace(
                &reset_body,
                "    # The PostgreSQL tree truncates and reloads nothing per test:\n\
                 \x20   # ActiveRecord::TestFixtures#setup_fixtures (runtime/\n\
                 \x20   # active_record_test_fixtures.rb) loads FixtureLoader.fixture_sets\n\
                 \x20   # once per process, before the test's transaction, which rolls\n\
                 \x20   # back after it.\n",
            );
        }
    }

    // (3)
    // `fixture_lcs` ORDER, not sorted. The lowerer topologically sorts
    // the fixtures parent-before-child (`fixture_to_library::
    // load_order`); re-sorting here alphabetically threw that away and
    // is how campfire loaded `users` last, after the five fixtures that
    // reference one. Two owners for one order, and the second had no
    // idea the first had computed anything.
    let names: Vec<&str> = fixture_lcs.iter().map(|lc| lc.name.0.as_str()).collect();
    let explicit = if names.is_empty() {
        // Empty FixtureLoader.load_all! body: no fixtures to load.
        // Keep the indentation consistent so the substitution result
        // is still valid Ruby (a `def`/`end` with no statements).
        String::from("    # no fixtures")
    } else {
        names
            .iter()
            .map(|name| format!("    {name}._fixtures_load!"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let TestReset::FixtureSets(rows) = reset else {
        return out.replace(SCAN_BLOCK, &explicit);
    };
    // The fixture sets ActiveRecord::TestFixtures loads, in the same
    // order, beside `load_all!` (which nothing calls on this tree).
    let scan_end = format!("{SCAN_BLOCK}\n  end\n");
    debug_assert!(
        out.contains(&scan_end),
        "runtime/spinel/test/test_helper.rb FixtureLoader.load_all! end \
         changed; update the FixtureSets render in render_test_helper"
    );
    let mut sets = format!(
        "{explicit}\n  end\n\n\
         \x20 # ActiveRecord::TestFixtures' fixture sets (runtime/\n\
         \x20 # active_record_test_fixtures.rb), parents first: the set's name,\n\
         \x20 # its fixture module, the model class its rows load and the table\n\
         \x20 # they go to. Named, not referenced: a model is resolved (and so\n\
         \x20 # autoloaded) only when its set loads.\n\
         \x20 def self.fixture_sets\n\
         \x20   [\n"
    );
    for row in rows {
        writeln!(
            sets,
            "      [{}, {}, {}, {}],",
            expr::ruby_str_literal(&row.set),
            expr::ruby_str_literal(&row.module),
            expr::ruby_str_literal(&row.model),
            expr::ruby_str_literal(&row.table),
        )
        .unwrap();
    }
    sets.push_str("    ]\n  end\n");
    out.replace(&scan_end, &sets)
}

/// How the emitted test helper isolates one test from the last.
enum TestReset {
    /// `SchemaSetup.reset!` truncates every table-backed model (and
    /// deletes every unmodelled table) and reloads every fixture before
    /// each test: the SQLite and spinel trees.
    Truncate(Vec<String>),
    /// Rails' transactional tests: the PostgreSQL CRuby tree's
    /// ActiveRecord::TestFixtures loads these sets once and rolls each
    /// test back.
    FixtureSets(Vec<FixtureSetRow>),
}

/// One fixture set as ActiveRecord::TestFixtures loads it.
struct FixtureSetRow {
    /// Rails' fixture set name: the fixture's path under test/fixtures
    /// without its extension (`customers/customer_active_records`).
    set: String,
    /// The emitted `<Camel>Fixtures` module with `_fixtures_load!`.
    module: String,
    /// The model class its rows load (`Fixture::class_id`).
    model: String,
    /// The model's table, or the set's name when the app has no such
    /// model (Rails' default table for a set).
    table: String,
}

/// The fixture sets of `fixture_lcs` (load order), each with the model
/// and table its `_fixtures_load!` writes.
fn fixture_set_rows(app: &App, fixture_lcs: &[LibraryClass]) -> Vec<FixtureSetRow> {
    let by_module: crate::det::HashMap<String, &crate::dialect::Fixture> = app
        .fixtures
        .iter()
        .map(|f| (format!("{}Fixtures", crate::naming::camelize(f.name.as_str())), f))
        .collect();
    let tables = crate::lower::fixtures::fixture_table_names(app);
    fixture_lcs
        .iter()
        .filter_map(|lc| {
            let module = lc.name.0.as_str();
            let fixture = by_module.get(module);
            debug_assert!(fixture.is_some(), "fixture module {module} has no fixture");
            let fixture = fixture?;
            let class = fixture.class_id();
            // The table the set's rows insert into, by the app's loader:
            // `_fixture: table_name`, an STI subclass's base table.
            let table = tables
                .get(&fixture.name)
                .map(|t| t.as_str().to_string())
                .unwrap_or_else(|| fixture.name.as_str().to_string());
            Some(FixtureSetRow {
                set: fixture.path.as_str().to_string(),
                module: module.to_string(),
                model: class.0.as_str().to_string(),
                table,
            })
        })
        .collect()
}

/// Rails' test base classes, declared ahead of boot on the CRuby tree.
/// The app's own files reopen them as they load (Core's
/// app/models/action_controller/test_case.rb includes a module into
/// `ActionController::TestCase` at boot; a support file's
/// `ActiveSupport::TestCase.class_eval { setup do … end }` loads with a
/// test file), and Rails defines both before any of that runs. Without
/// them the first reopening made a bare `Object` subclass, whose tests
/// had no `assertions`, and the second raised NameError.
/// `ActiveSupport::TestCase` is `TestBase` itself, the class emitted
/// test classes descend from for it (`emitted_test_parent`).
pub(crate) const MINITEST_TEST_CASES: &str = "\
# Rails' test base classes, before the app's files reopen them: TestBase
# (defined below) is ActiveSupport::TestCase, and ActionController::
# TestCase descends from it.
# A `setup`/`teardown` on them while the app boots (an `included do` a
# test-support initializer's include into ActiveSupport::TestCase runs as
# app/models eager-loads) is held, in order, until
# test/minitest_runner.rb's RoundhouseTestCallbacks registers it.
module RoundhouseEarlyTestCallbacks
  PENDING = []

  def setup(*filters, &block)
    PENDING << [self, :setup, filters, block]
    nil
  end

  def teardown(*filters, &block)
    PENDING << [self, :teardown, filters, block]
    nil
  end
end
# ActiveSupport::Testing::Declarative#test (activesupport 8.x), which Rails
# puts on `ActiveSupport::TestCase` itself: `test \"name\" do … end` in a
# class body, run as the body loads (a loop over a collection, a class
# method that declares tests), defines `test_<name, whitespace as _>`.
module RoundhouseTestDeclarative
  def test(name, &block)
    test_name = \"test_#{name.gsub(/\\s+/, '_')}\".to_sym
    defined = method_defined?(test_name)
    raise \"#{test_name} is already defined in #{self}\" if defined

    if block
      define_method(test_name, &block)
    else
      define_method(test_name) do
        flunk \"No implementation provided for #{name}\"
      end
    end
  end
end
class TestBase
  extend RoundhouseEarlyTestCallbacks
  extend RoundhouseTestDeclarative
end
module ActiveSupport
  TestCase = ::TestBase
end
module ActionController
  class TestCase < TestBase
    # ActionController::TestCase::Behavior (actionpack 8.2
    # test_case.rb): the controller, request and response a test's setup
    # and helpers read, built by the first setup callback. DIVERGENCE,
    # stated: there is no ActionDispatch::TestResponse, so `response` is
    # nil, and `process` (get/post/...) is refused by name.
    module Behavior
      CONTROLLERS_ROOT = File.expand_path('../app/controllers', __dir__)

      attr_reader :response, :request

      # ActionDispatch::TestProcess#session (actionpack action_dispatch/
      # testing/test_process.rb), which Behavior includes in Rails: the
      # request's session, as the helpers and tests read it.
      def session
        @request.session
      end

      def self.included(base)
        base.extend(ClassMethods)
        base.setup :setup_controller_request_and_response
      end

      module ClassMethods
        def tests(controller_class)
          case controller_class
          when String, Symbol
            self.controller_class = (controller_class.to_s.camelize + 'Controller').constantize
          when Class
            self.controller_class = controller_class
          else
            raise ArgumentError, 'controller class must be a String, Symbol, or Class'
          end
        end

        # `class_attribute :_controller_class`.
        def _controller_class
          return @_controller_class if instance_variable_defined?(:@_controller_class)
          superclass.respond_to?(:_controller_class) ? superclass._controller_class : nil
        end

        def controller_class=(new_class)
          @_controller_class = new_class
        end

        def controller_class
          if current_controller_class = _controller_class
            current_controller_class
          else
            self.controller_class = determine_default_controller_class(name)
          end
        end

        # ActiveSupport::Testing::ConstantLookup. A candidate's
        # app/controllers file loads first, as Zeitwerk autoloads it on
        # the constant's reference: this tree registers no autoload for
        # controllers.
        def determine_default_controller_class(name)
          names = name.to_s.split('::')
          while names.size > 0
            names[-1] = names[-1].sub(/Test$/, '')
            begin
              file = File.join(CONTROLLERS_ROOT, names.join('::').underscore + '.rb')
              require file if File.file?(file)
              constant = names.join('::').safe_constantize
              break(constant) if Class === constant && constant < ActionController::Metal
            ensure
              names.pop
            end
          end
        end
      end

      def process(action, **)
        raise NotImplementedError, \"roundhouse: ActionController::TestCase#process (#{action}): controller tests' requests are not ported\"
      end

      %i[get post patch put delete head].each do |verb|
        define_method(verb) { |action, **options| process(action, **options) }
      end

      def setup_controller_request_and_response
        @controller = nil unless defined? @controller

        if klass = self.class.controller_class
          if @controller.nil?
            begin
              @controller = klass.new
            rescue
              warn \"could not construct controller #{klass}\" if $VERBOSE
            end
          end
        end

        @request = ActionController::TestRequest.create(@controller.class)

        if @controller
          @controller.request = @request
          @controller.params = {}
        end
      end
    end
    include Behavior
  end
end
";

/// The superclass an emitted test class declares: `ActiveSupport::
/// TestCase` and `Minitest::Test` become the harness's `TestBase`, any
/// other parent is kept.
pub(crate) fn emitted_test_parent(parent: &crate::ident::ClassId) -> crate::ident::ClassId {
    match parent.0.as_str() {
        "ActiveSupport::TestCase" | "Minitest::Test" => {
            crate::ident::ClassId(crate::ident::Symbol::from("TestBase"))
        }
        _ => parent.clone(),
    }
}

/// `ArticleTest` → `article`, `ArticlesControllerTest` →
/// `articles_controller` (strip Test suffix, snake_case). Used for the
/// `test/<dir>/<stem>_test.rb` output path.
/// File stem for a test program, with the namespace FLATTENED into the
/// name: `Rooms::ClosedsControllerTest` → `rooms_closeds_controller`.
///
/// `snake_case` alone passes `::` straight through — the same trap
/// 147d5848 fixed for model/controller output paths and require anchors,
/// which the test path never got. It produced
/// `test/rooms::closeds_controller_test.rb`, and a `::` in a filename is
/// not merely ugly: the emitted Makefile globs `*.rbs` into `RBS_SRC`
/// and lists the programs in `SPINEL_TESTS`, and a `::` in a Make
/// prerequisite parses as a second target separator —
/// `Makefile:149: *** multiple target patterns.  Stop.` campfire emits
/// 33 such tests, so `make build` failed before spinel saw a line of it.
///
/// Flattened with `_` rather than nested with `underscore()`'s `/`,
/// because the spin relocation moves an in-lane test to `test/<basename>`
/// (`apply_spinel_test_layout`): a nested path would collapse to its last
/// segment there, so `Rooms::ClosedsControllerTest` and
/// `Accounts::ClosedsControllerTest` would both land on
/// `closeds_controller_test.rb` and one would overwrite the other.
fn test_file_stem(class_name: &str) -> String {
    let stem = class_name.strip_suffix("Test").unwrap_or(class_name);
    crate::naming::underscore(stem).replace('/', "_")
}

/// The `test/<dir>/` a lowered test class is written under — the
/// Rails layout, read back off the class: its parent says what kind
/// of test it is, and the `ControllerTest` suffix is the one
/// convention that predates the parent check. Everything else is a
/// model test, which is where a plain `ActiveSupport::TestCase` lives
/// in Rails too — campfire's `test/lib/` files are that, and land
/// under `models/` here rather than growing a fourth dir the Makefile
/// and the suite runner would each have to learn.
fn test_subdir(lc: &LibraryClass) -> &'static str {
    let parent = lc.parent.as_ref().map(|p| p.0.as_str()).unwrap_or("");
    if lc.name.0.as_str().ends_with("ControllerTest") {
        "controllers"
    } else if parent.ends_with("Channel::TestCase") || parent.ends_with("Connection::TestCase") {
        "channels"
    } else if parent == "ActionView::TestCase" {
        "helpers"
    } else {
        "models"
    }
}


#[cfg(test)]
mod method_sig_tests {
    use super::*;
    use crate::dialect::{AccessorKind, Param};
    use crate::effect::EffectSet;
    use crate::expr::{Expr, ExprNode, Literal};
    use crate::ident::Symbol;
    use crate::span::Span;

    // `def get_from_cache(opts = {}, &block)` — an optional positional with
    // a default plus a captured block param must both round-trip into the
    // emitted signature (regression: both were dropped → `def f` + arity
    // crash / undefined `block`). See the lobsters runtime-wiring probe.
    #[test]
    fn emit_method_renders_optional_default_and_block_param() {
        let m = MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("get_from_cache"),
            receiver: MethodReceiver::Instance,
            params: vec![Param::with_default(
                Symbol::from("opts"),
                Expr::new(Span::synthetic(), ExprNode::Hash { entries: vec![], kwargs: false }),
            )],
            body: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            signature: None,
            effects: EffectSet::pure(),
            enclosing_class: None,
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: Some(Param::positional(Symbol::from("block"))),
        };
        let out = emit_method(&m);
        assert!(
            out.contains("def get_from_cache(opts = {}, &block)"),
            "got:\n{out}"
        );
    }
}

#[cfg(test)]
mod declaration_program_header_tests {
    use super::require_declarations_through_programs;

    // `ToggleFlagsHelper::ClassMethods`: the test names `ToggleFlagsHelper`,
    // whose declarations file alone leaves out the `ClassMethods` its
    // `included` hook extends with. The header loads the source's program
    // (every declaration and require of the file), past a stdlib
    // `require` among the headers; a declarations file with no program,
    // and a `require_relative` in the body, stay.
    #[test]
    fn header_of_a_declarations_file_with_a_program_requires_the_program() {
        let programs = std::collections::BTreeMap::from([(
            "models/toggle_flags_helper".to_string(),
            "models/roundhouse_files/lib/testing/toggle_flags_helper".to_string(),
        )]);
        let content = "require_relative \"../test_helper\"\nrequire \"bigdecimal\"\nrequire_relative \"../../app/models/shop\"\nrequire_relative \"../../app/models/toggle_flags_helper\"\n\nclass FooTest < TestBase\n  require_relative \"../../app/models/toggle_flags_helper\"\nend\n";
        let out = require_declarations_through_programs(content, std::path::Path::new("test/models/foo_test.rb"), &programs);
        assert_eq!(
            out,
            "require_relative \"../test_helper\"\nrequire \"bigdecimal\"\nrequire_relative \"../../app/models/shop\"\nrequire_relative \"../../app/models/roundhouse_files/lib/testing/toggle_flags_helper\"\n\nclass FooTest < TestBase\n  require_relative \"../../app/models/toggle_flags_helper\"\nend\n"
        );
    }
}

#[cfg(test)]
mod nested_test_class_splice_tests {
    use super::{splice_after_class_header, splice_test_body_statements};
    use crate::dialect::TestBodyStatement;
    use crate::expr::{Expr, ExprNode, Literal};
    use crate::ident::Symbol;
    use crate::span::Span;

    const NESTED: &str = "require_relative \"../test_helper\"\nclass PlaybookActionTest < TestBase\n  class ApprovedTest < PlaybookActionTest\n    def test_a\n    end\n  end\nend\n";

    // Each split file of `playbook_action_test.rb` reopens the outer
    // class; a nested class's `TACTIC_INSTANCE_ID = …` went into that
    // reopen, so two loaded split files reassigned
    // `PlaybookActionTest::TACTIC_INSTANCE_ID` ("already initialized
    // constant"). It belongs in the nested class, as in the source.
    #[test]
    fn nested_test_class_constants_go_in_the_nested_class() {
        let out = splice_after_class_header(NESTED, "PlaybookActionTest::ApprovedTest", "  TACTIC_INSTANCE_ID = \"x\"\n\n");
        assert_eq!(
            out,
            "require_relative \"../test_helper\"\nclass PlaybookActionTest < TestBase\n  class ApprovedTest < PlaybookActionTest\n    TACTIC_INSTANCE_ID = \"x\"\n\n    def test_a\n    end\n  end\nend\n"
        );
        // A module namespace and a compound header resolve the same way.
        let out = splice_after_class_header("module A\n  class B::C < X\n  end\nend\n", "A::B::C", "  K = 1\n");
        assert_eq!(out, "module A\n  class B::C < X\n    K = 1\n  end\nend\n");
        // A top-level class keeps the first header.
        let out = splice_after_class_header("class FooTest < TestBase\nend\n", "FooTest", "  K = 1\n");
        assert_eq!(out, "class FooTest < TestBase\n  K = 1\nend\n");
    }

    #[test]
    fn nested_test_class_body_statements_go_in_the_nested_class() {
        let call = Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: None,
                method: Symbol::from("use_gcs"),
                args: vec![Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Int { value: 1 } })],
                block: None,
                parenthesized: true,
            },
        );
        let statement = TestBodyStatement { expr: call, line: 3, after_definitions: false, after_setup_blocks: false, after_teardown_blocks: false, source: None };
        let out = splice_test_body_statements(NESTED, "PlaybookActionTest::ApprovedTest", &[statement], "t.rb", std::path::Path::new("test/models/t.rb"));
        let at = out.find("use_gcs").expect(&out);
        let inner = out.find("class ApprovedTest").unwrap();
        assert!(at > inner, "statement should run in the nested class:\n{out}");
        assert!(out.contains("\n    RoundhouseTestCallbacks.class_body_statement(self, \"t.rb:3\") { use_gcs(1) }\n"), "got:\n{out}");
    }
}
