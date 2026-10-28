//! Memory measurement, compiled only with the `memory-census` feature.
//!
//! A counting global allocator keeps the exact number of live heap bytes
//! and their high-water mark; `roundhouse-timing:` lines report both. With
//! `ROUNDHOUSE_MEMORY_CENSUS=1` the driver also prints what holds the heap
//! at each phase boundary: every `App` field, the body/header split of the
//! unit collections, IR node counts, and the analyzer's fields. Sizes are
//! measured as the live bytes a clone allocates (or a field's drop frees),
//! so they are exact for the data, not for allocator slack.
//!
//! Measuring clones briefly doubles the measured value; the census clones
//! one unit at a time so its own peak stays small. Nothing here changes
//! what the compiler produces.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
/// The high-water mark since the open windows last took it (see
/// [`open_window`]). Never above `PEAK`.
static SINCE: AtomicIsize = AtomicIsize::new(0);

fn grew(by: isize) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    if now > SINCE.load(Ordering::Relaxed) {
        SINCE.fetch_max(now, Ordering::Relaxed);
        if now > PEAK.load(Ordering::Relaxed) {
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grew(layout.size() as isize);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grew(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            grew(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Live heap bytes right now.
pub fn live_bytes() -> i64 {
    LIVE.load(Ordering::Relaxed) as i64
}

/// The most live heap bytes seen so far.
pub fn peak_bytes() -> i64 {
    PEAK.load(Ordering::Relaxed) as i64
}

/// The windows open now: an id and the highest live heap seen since it
/// opened, as of the last [`take_since`].
static WINDOWS: std::sync::Mutex<Vec<(u64, i64)>> = std::sync::Mutex::new(Vec::new());
static NEXT_WINDOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Hand the high-water mark since the last call to every open window and
/// start a new one from the live heap now. Windows can nest or overlap
/// in any order, since each keeps its own maximum.
fn take_since(windows: &mut [(u64, i64)]) {
    let live = LIVE.load(Ordering::Relaxed);
    let since = SINCE.swap(live, Ordering::Relaxed).max(live) as i64;
    for (_, peak) in windows {
        *peak = (*peak).max(since);
    }
}

/// Start measuring the live heap's high-water mark from now, for a phase
/// whose own peak [`close_window`] then returns. Unlike [`peak_bytes`],
/// that peak leaves out whatever the heap reached before the phase.
pub fn open_window() -> u64 {
    let mut windows = WINDOWS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    take_since(&mut windows);
    let id = NEXT_WINDOW.fetch_add(1, Ordering::Relaxed);
    windows.push((id, live_bytes()));
    id
}

/// The most live heap bytes seen since `open_window` returned `id`, and
/// stop tracking it. Zero for an id that is not open.
pub fn close_window(id: u64) -> i64 {
    let mut windows = WINDOWS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    take_since(&mut windows);
    match windows.iter().position(|(open, _)| *open == id) {
        Some(index) => windows.swap_remove(index).1,
        None => 0,
    }
}

/// Whether `ROUNDHOUSE_MEMORY_CENSUS=1` asked for the census.
pub fn enabled() -> bool {
    std::env::var("ROUNDHOUSE_MEMORY_CENSUS").is_ok_and(|value| value == "1" || value == "true")
}

/// The live bytes `make` leaves allocated, released before returning.
pub fn live_delta<T>(make: impl FnOnce() -> T) -> i64 {
    let before = live_bytes();
    let made = make();
    let delta = live_bytes() - before;
    drop(made);
    delta
}

/// The live bytes `release` frees.
pub fn freed_by(release: impl FnOnce()) -> i64 {
    let before = live_bytes();
    release();
    before - live_bytes()
}

fn megabytes(bytes: i64) -> f64 {
    bytes as f64 / 1e6
}

/// One `roundhouse-census:` line, when the census is on.
pub fn note(stage: &str, message: impl std::fmt::Display) {
    if enabled() {
        eprintln!("roundhouse-census: {stage}: {message}");
    }
}

fn distribution(stage: &str, label: &str, sizes: &mut [i64]) {
    if sizes.is_empty() {
        return;
    }
    sizes.sort_unstable();
    let count = sizes.len();
    let total: i64 = sizes.iter().sum();
    let at = |fraction: f64| sizes[((count as f64 - 1.0) * fraction) as usize] as f64 / 1e3;
    note(
        stage,
        format_args!(
            "{label}: n={count} total={:.1}MB p50={:.1}KB p90={:.1}KB p99={:.1}KB max={:.1}KB",
            megabytes(total),
            at(0.5),
            at(0.9),
            at(0.99),
            sizes[count - 1] as f64 / 1e3,
        ),
    );
}

fn unit_sizes<T: Clone>(units: &[T]) -> Vec<i64> {
    units
        .iter()
        .map(|unit| live_delta(|| unit.clone()))
        .collect()
}

fn count_nodes(expression: &crate::expr::Expr, nodes: &mut u64) {
    *nodes += 1;
    expression
        .node
        .for_each_child(&mut |child| count_nodes(child, nodes));
}

/// Print what holds the heap after `stage`. Does nothing unless the
/// census is on.
pub fn census(stage: &str, app: &crate::App) {
    if !enabled() {
        return;
    }
    let started = std::time::Instant::now();
    note(
        stage,
        format_args!(
            "live heap {:.1}MB, live peak {:.1}MB",
            megabytes(live_bytes()),
            megabytes(peak_bytes())
        ),
    );
    // Destructured without `..`: a field added to `App` must be added here.
    let crate::App {
        schema_version,
        schema,
        databases,
        model_databases,
        database_refusals,
        database_yml,
        models,
        library_classes,
        controllers,
        routes,
        views,
        test_modules,
        fixtures,
        seeds,
        importmap,
        inertia,
        vite_manifest,
        stylesheets,
        rbs_signatures,
        rbs_singleton_methods,
        rbs_includes,
        gem_lock,
        loaded_frameworks,
        gem_boundary,
        extensions,
        content_helper_allowed_attributes,
        test_helper_connections,
        test_helper_connection_blockers,
        content_helper_allowed_tags,
        inferred_method_params,
        binary_assets,
        locale_files,
        i18n,
        i18n_table,
        i18n_runtime_sites,
        i18n_table_sites,
        helper_method_index,
        helper_singleton_methods,
        view_visible_controller_methods,
        generated_helper_methods,
        global_id_locate_models,
        global_id_locate_signed_models,
        job_plans,
        attachable_unsigned_models,
        pending_attachment_on_load,
        load_hook_class_macros,
        framework_base_includes,
        load_hook_includes,
        partial_local_types,
        view_ivar_types,
        html_safe_methods,
        html_safe_method_owners,
        time_formats,
        module_mixins,
        initializer_filters,
        initializer_files,
        initializer_gaps,
        initializer_constants,
        initializer_programs,
        environment_programs,
        framework_load_hooks,
        class_load_hooks,
        load_hook_bases,
        file_statement_gaps,
        library_class_reopens,
        load_hooks,
        lifted_autoload_callbacks,
        autoload_callback_cpaths,
        autoload_includes,
        ingest_drops,
        railties,
        file_programs,
        boot_loads,
        boot_conditional_loads,
        boot_native_requires,
        bundler_gems,
        gem_source_dirs,
        test_load_roots,
        engine_autoload_roots,
        lib_load_roots,
        test_helper_programs,
        library_class_files,
        eager_load,
        zeitwerk_loaders,
        collection_cache_store,
        app_requires,
        sql_functions,
        database_pragmas,
        rails_application,
        application_namespace,
        concern_filters,
        concern_rescues,
        graphql_types,
        graphql_signatures,
        concern_spliced_actions,
        unreachable_views,
        helper_diagnostics,
        helper_modules,
        concern_spliced_class_methods,
        class_methods_carrier_splices,
        class_methods_blocks,
        ruby_delegate_forwarders,
        ruby_declined_delegate_forwarders,
        ruby_abstract_stubs,
        delegate_forwarder_sites,
        class_eval_macro_expansions,
        concern_model_items,
        concern_prepended_items,
        concern_guarded_items,
        concern_included_redefs,
        current_attribute_classes,
        carried_included_blocks,
        test_carried_included_blocks,
        singleton_scoped_classes,
        library_class_declarations,
        included_block_statements,
        controller_runtime_includes,
        included_class_attributes,
        render_edges,
        view_feeders,
        sources,
        wrap_parameters_by_default,
        belongs_to_required_skips_unchanged_foreign_key,
        const_resolver,
        dropped_files,
        sorbet_type_aliases,
        source_index_required,
        controller_resolutions,
        root,
        app_roots,
    } = app;
    let mut library_sizes = unit_sizes(library_classes);
    let mut model_sizes = unit_sizes(models);
    let mut controller_sizes = unit_sizes(controllers);
    let mut view_sizes = unit_sizes(views);
    let mut test_sizes = unit_sizes(test_modules);
    let mut fixture_sizes = unit_sizes(fixtures);
    let mut program_sizes = unit_sizes(file_programs);
    let mut source_sizes = unit_sizes(sources);
    let mut fields: Vec<(&str, i64)> = vec![
        ("library_classes", library_sizes.iter().sum()),
        ("models", model_sizes.iter().sum()),
        ("controllers", controller_sizes.iter().sum()),
        ("views", view_sizes.iter().sum()),
        ("test_modules", test_sizes.iter().sum()),
        ("fixtures", fixture_sizes.iter().sum()),
        ("file_programs", program_sizes.iter().sum()),
        ("sources", source_sizes.iter().sum()),
        ("schema_version", live_delta(|| schema_version.clone())),
        ("schema", live_delta(|| schema.clone())),
        ("databases", live_delta(|| databases.clone())),
        ("model_databases", live_delta(|| model_databases.clone())),
        (
            "database_refusals",
            live_delta(|| database_refusals.clone()),
        ),
        ("database_yml", live_delta(|| database_yml.clone())),
        ("routes", live_delta(|| routes.clone())),
        ("seeds", live_delta(|| seeds.clone())),
        ("importmap", live_delta(|| importmap.clone())),
        ("inertia", live_delta(|| inertia.clone())),
        ("vite_manifest", live_delta(|| vite_manifest.clone())),
        ("stylesheets", live_delta(|| stylesheets.clone())),
        ("rbs_signatures", live_delta(|| rbs_signatures.clone())),
        (
            "rbs_singleton_methods",
            live_delta(|| rbs_singleton_methods.clone()),
        ),
        ("rbs_includes", live_delta(|| rbs_includes.clone())),
        ("gem_lock", live_delta(|| gem_lock.clone())),
        (
            "loaded_frameworks",
            live_delta(|| loaded_frameworks.clone()),
        ),
        ("gem_boundary", live_delta(|| gem_boundary.clone())),
        ("extensions", live_delta(|| extensions.clone())),
        (
            "content_helper_allowed_attributes",
            live_delta(|| content_helper_allowed_attributes.clone()),
        ),
        (
            "test_helper_connections",
            live_delta(|| test_helper_connections.clone()),
        ),
        (
            "test_helper_connection_blockers",
            live_delta(|| test_helper_connection_blockers.clone()),
        ),
        (
            "content_helper_allowed_tags",
            live_delta(|| content_helper_allowed_tags.clone()),
        ),
        (
            "inferred_method_params",
            live_delta(|| inferred_method_params.clone()),
        ),
        ("binary_assets", live_delta(|| binary_assets.clone())),
        ("locale_files", live_delta(|| locale_files.clone())),
        // Shared through an `Arc`: cloning the handle would measure nothing.
        ("i18n", live_delta(|| (**i18n).clone())),
        ("i18n_table", live_delta(|| i18n_table.clone())),
        (
            "i18n_runtime_sites",
            live_delta(|| i18n_runtime_sites.clone()),
        ),
        ("i18n_table_sites", live_delta(|| i18n_table_sites.clone())),
        (
            "helper_method_index",
            live_delta(|| helper_method_index.clone()),
        ),
        (
            "helper_singleton_methods",
            live_delta(|| helper_singleton_methods.clone()),
        ),
        (
            "view_visible_controller_methods",
            live_delta(|| view_visible_controller_methods.clone()),
        ),
        (
            "generated_helper_methods",
            live_delta(|| generated_helper_methods.clone()),
        ),
        (
            "global_id_locate_models",
            live_delta(|| global_id_locate_models.clone()),
        ),
        (
            "global_id_locate_signed_models",
            live_delta(|| global_id_locate_signed_models.clone()),
        ),
        ("job_plans", live_delta(|| job_plans.clone())),
        (
            "attachable_unsigned_models",
            live_delta(|| attachable_unsigned_models.clone()),
        ),
        (
            "pending_attachment_on_load",
            live_delta(|| pending_attachment_on_load.clone()),
        ),
        (
            "load_hook_class_macros",
            live_delta(|| load_hook_class_macros.clone()),
        ),
        (
            "framework_base_includes",
            live_delta(|| framework_base_includes.clone()),
        ),
        (
            "load_hook_includes",
            live_delta(|| load_hook_includes.clone()),
        ),
        (
            "partial_local_types",
            live_delta(|| partial_local_types.clone()),
        ),
        ("view_ivar_types", live_delta(|| view_ivar_types.clone())),
        (
            "html_safe_methods",
            live_delta(|| html_safe_methods.clone()),
        ),
        (
            "html_safe_method_owners",
            live_delta(|| html_safe_method_owners.clone()),
        ),
        ("time_formats", live_delta(|| time_formats.clone())),
        ("module_mixins", live_delta(|| module_mixins.clone())),
        (
            "initializer_filters",
            live_delta(|| initializer_filters.clone()),
        ),
        (
            "initializer_files",
            live_delta(|| initializer_files.clone()),
        ),
        ("initializer_gaps", live_delta(|| initializer_gaps.clone())),
        (
            "initializer_constants",
            live_delta(|| initializer_constants.clone()),
        ),
        (
            "initializer_programs",
            live_delta(|| initializer_programs.clone()),
        ),
        (
            "environment_programs",
            live_delta(|| environment_programs.clone()),
        ),
        (
            "framework_load_hooks",
            live_delta(|| framework_load_hooks.clone()),
        ),
        ("class_load_hooks", live_delta(|| class_load_hooks.clone())),
        ("load_hook_bases", live_delta(|| load_hook_bases.clone())),
        (
            "file_statement_gaps",
            live_delta(|| file_statement_gaps.clone()),
        ),
        (
            "library_class_reopens",
            live_delta(|| library_class_reopens.clone()),
        ),
        ("load_hooks", live_delta(|| load_hooks.clone())),
        (
            "lifted_autoload_callbacks",
            live_delta(|| lifted_autoload_callbacks.clone()),
        ),
        (
            "autoload_callback_cpaths",
            live_delta(|| autoload_callback_cpaths.clone()),
        ),
        (
            "autoload_includes",
            live_delta(|| autoload_includes.clone()),
        ),
        ("ingest_drops", live_delta(|| ingest_drops.clone())),
        ("railties", live_delta(|| railties.clone())),
        ("boot_loads", live_delta(|| boot_loads.clone())),
        (
            "boot_conditional_loads",
            live_delta(|| boot_conditional_loads.clone()),
        ),
        (
            "boot_native_requires",
            live_delta(|| boot_native_requires.clone()),
        ),
        ("bundler_gems", live_delta(|| bundler_gems.clone())),
        ("gem_source_dirs", live_delta(|| gem_source_dirs.clone())),
        ("test_load_roots", live_delta(|| test_load_roots.clone())),
        (
            "engine_autoload_roots",
            live_delta(|| engine_autoload_roots.clone()),
        ),
        ("lib_load_roots", live_delta(|| lib_load_roots.clone())),
        (
            "test_helper_programs",
            live_delta(|| test_helper_programs.clone()),
        ),
        (
            "library_class_files",
            live_delta(|| library_class_files.clone()),
        ),
        ("eager_load", live_delta(|| eager_load.clone())),
        ("zeitwerk_loaders", live_delta(|| zeitwerk_loaders.clone())),
        (
            "collection_cache_store",
            live_delta(|| collection_cache_store.clone()),
        ),
        ("app_requires", live_delta(|| app_requires.clone())),
        ("sql_functions", live_delta(|| sql_functions.clone())),
        ("database_pragmas", live_delta(|| database_pragmas.clone())),
        (
            "rails_application",
            live_delta(|| rails_application.clone()),
        ),
        (
            "application_namespace",
            live_delta(|| application_namespace.clone()),
        ),
        ("concern_filters", live_delta(|| concern_filters.clone())),
        ("concern_rescues", live_delta(|| concern_rescues.clone())),
        ("graphql_types", live_delta(|| graphql_types.clone())),
        (
            "graphql_signatures",
            live_delta(|| graphql_signatures.clone()),
        ),
        (
            "concern_spliced_actions",
            live_delta(|| concern_spliced_actions.clone()),
        ),
        (
            "unreachable_views",
            live_delta(|| unreachable_views.clone()),
        ),
        (
            "helper_diagnostics",
            live_delta(|| helper_diagnostics.clone()),
        ),
        ("helper_modules", live_delta(|| helper_modules.clone())),
        (
            "concern_spliced_class_methods",
            live_delta(|| concern_spliced_class_methods.clone()),
        ),
        (
            "class_methods_carrier_splices",
            live_delta(|| class_methods_carrier_splices.clone()),
        ),
        (
            "class_methods_blocks",
            live_delta(|| class_methods_blocks.clone()),
        ),
        (
            "ruby_delegate_forwarders",
            live_delta(|| ruby_delegate_forwarders.clone()),
        ),
        (
            "ruby_declined_delegate_forwarders",
            live_delta(|| ruby_declined_delegate_forwarders.clone()),
        ),
        (
            "ruby_abstract_stubs",
            live_delta(|| ruby_abstract_stubs.clone()),
        ),
        (
            "delegate_forwarder_sites",
            live_delta(|| delegate_forwarder_sites.clone()),
        ),
        (
            "class_eval_macro_expansions",
            live_delta(|| class_eval_macro_expansions.clone()),
        ),
        (
            "concern_model_items",
            live_delta(|| concern_model_items.clone()),
        ),
        (
            "concern_prepended_items",
            live_delta(|| concern_prepended_items.clone()),
        ),
        (
            "concern_guarded_items",
            live_delta(|| concern_guarded_items.clone()),
        ),
        (
            "concern_included_redefs",
            live_delta(|| concern_included_redefs.clone()),
        ),
        (
            "current_attribute_classes",
            live_delta(|| current_attribute_classes.clone()),
        ),
        (
            "carried_included_blocks",
            live_delta(|| carried_included_blocks.clone()),
        ),
        (
            "test_carried_included_blocks",
            live_delta(|| test_carried_included_blocks.clone()),
        ),
        (
            "singleton_scoped_classes",
            live_delta(|| singleton_scoped_classes.clone()),
        ),
        (
            "library_class_declarations",
            live_delta(|| library_class_declarations.clone()),
        ),
        (
            "included_block_statements",
            live_delta(|| included_block_statements.clone()),
        ),
        (
            "controller_runtime_includes",
            live_delta(|| controller_runtime_includes.clone()),
        ),
        (
            "included_class_attributes",
            live_delta(|| included_class_attributes.clone()),
        ),
        ("render_edges", live_delta(|| render_edges.clone())),
        ("view_feeders", live_delta(|| view_feeders.clone())),
        (
            "wrap_parameters_by_default",
            live_delta(|| wrap_parameters_by_default.clone()),
        ),
        (
            "belongs_to_required_skips_unchanged_foreign_key",
            live_delta(|| belongs_to_required_skips_unchanged_foreign_key.clone()),
        ),
        ("const_resolver", live_delta(|| const_resolver.clone())),
        ("dropped_files", live_delta(|| dropped_files.clone())),
        (
            "sorbet_type_aliases",
            live_delta(|| sorbet_type_aliases.clone()),
        ),
        (
            "source_index_required",
            live_delta(|| source_index_required.clone()),
        ),
        (
            "controller_resolutions",
            live_delta(|| controller_resolutions.clone()),
        ),
        ("root", live_delta(|| root.clone())),
        ("app_roots", live_delta(|| app_roots.clone())),
    ];
    let total: i64 = fields.iter().map(|(_, size)| size).sum();
    fields.sort_by_key(|(name, size)| (-size, *name));
    for (name, size) in fields.iter().filter(|(_, size)| *size >= 1_000_000) {
        note(
            stage,
            format_args!("field {name}: {:.1}MB", megabytes(*size)),
        );
    }
    note(
        stage,
        format_args!(
            "app fields total {:.1}MB of live {:.1}MB",
            megabytes(total),
            megabytes(live_bytes())
        ),
    );

    let mut method_body_sizes = Vec::new();
    let mut nodes = 0u64;
    let mut class_body_bytes = 0i64;
    for class in library_classes {
        for method in &class.methods {
            // A stored body clones as its handle and is not decoded here.
            method_body_sizes.push(live_delta(|| method.body.clone()));
            if let Some(body) = method.body.resident() {
                count_nodes(body, &mut nodes);
            }
        }
        class_body_bytes += live_delta(|| {
            (
                class.class_ivar_initializers.clone(),
                class.constants.clone(),
            )
        });
    }
    let method_body_bytes: i64 = method_body_sizes.iter().sum();
    let library_bytes: i64 = library_sizes.iter().sum();
    note(
        stage,
        format_args!(
            "library classes: method bodies {:.1}MB, ivar initializers and constants {:.1}MB, headers {:.1}MB; {nodes} body nodes",
            megabytes(method_body_bytes),
            megabytes(class_body_bytes),
            megabytes(library_bytes - method_body_bytes - class_body_bytes),
        ),
    );
    note(
        stage,
        format_args!(
            "size_of Expr={} ExprNode={} Ty={} MethodDef={} LibraryClass={}",
            std::mem::size_of::<crate::expr::Expr>(),
            std::mem::size_of::<crate::expr::ExprNode>(),
            std::mem::size_of::<crate::ty::Ty>(),
            std::mem::size_of::<crate::dialect::MethodDef>(),
            std::mem::size_of::<crate::dialect::LibraryClass>(),
        ),
    );
    distribution(stage, "library classes", &mut library_sizes);
    distribution(stage, "library method bodies", &mut method_body_sizes);
    distribution(stage, "models", &mut model_sizes);
    distribution(stage, "controllers", &mut controller_sizes);
    distribution(stage, "views", &mut view_sizes);
    distribution(stage, "test modules", &mut test_sizes);
    distribution(stage, "fixtures", &mut fixture_sizes);
    distribution(stage, "file programs", &mut program_sizes);
    distribution(stage, "sources", &mut source_sizes);
    note(
        stage,
        format_args!("census took {:.1}s", started.elapsed().as_secs_f64()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 256 << 20;

    fn hold_block() {
        let block = vec![1u8; BLOCK];
        std::hint::black_box(&block);
    }

    #[test]
    fn a_window_reports_its_own_peak_and_not_an_earlier_one() {
        let outer = open_window();
        let started = live_bytes();
        let inner = open_window();
        hold_block();
        let inner_peak = close_window(inner);
        let outer_peak = close_window(outer);
        assert!(inner_peak >= started + BLOCK as i64, "inner {inner_peak}, started {started}");
        assert!(outer_peak >= inner_peak, "outer {outer_peak}, inner {inner_peak}");

        let later = open_window();
        let later_peak = close_window(later);
        assert!(later_peak < inner_peak - (BLOCK as i64) / 2, "later {later_peak}, inner {inner_peak}");
        assert!(peak_bytes() >= inner_peak);
    }

    #[test]
    fn closing_an_unknown_window_reports_nothing() {
        assert_eq!(close_window(u64::MAX), 0);
    }
}
