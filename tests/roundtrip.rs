//! Round-trip test: IR → JSON → IR preserves semantics.
//!
//! The forcing function for IR completeness. If constructing an App by hand
//! and round-tripping through JSON loses information, the shape is wrong.
//! Extend this test as the IR grows — every new node kind gets exercised here.

use indexmap::IndexMap;
use roundhouse::{
    Action, App, ClassId, Column, ColumnType, Controller, Effect, EffectSet, Expr, ExprNode,
    HttpMethod, Literal, Model, RenderTarget, RouteTable, Row, Schema, Symbol, Table,
    TableRef, Ty,
};
use roundhouse::span::Span;

fn sp() -> Span {
    Span::synthetic()
}

/// Verifies that a representative blog app survives an IR serialization round-trip.
#[test]
fn tiny_blog_round_trips() {
    let mut tables = IndexMap::new();
    tables.insert(
        Symbol::from("posts"),
        Table {
            name: Symbol::from("posts"),
            columns: vec![
                Column {
                    name: Symbol::from("id"),
                    col_type: ColumnType::BigInt,
                    nullable: false,
                    default: None,
                    primary_key: true,
                    generated: None,
                    generated_text_compatible: None,
                    generated_int4_compatible: None,
                },
                Column {
                    name: Symbol::from("title"),
                    col_type: ColumnType::String { limit: None },
                    nullable: false,
                    default: None,
                    primary_key: false,
                    generated: None,
                    generated_text_compatible: None,
                    generated_int4_compatible: None,
                },
            ],
            indexes: vec![],
            foreign_keys: vec![],
            virtual_module: None, primary_key_columns: Vec::new(),
        },
    );
    let schema = Schema { tables };

    let mut attrs = IndexMap::new();
    attrs.insert(Symbol::from("id"), Ty::Int);
    attrs.insert(Symbol::from("title"), Ty::Str);

    let post_model = Model {
        name: ClassId(Symbol::from("Post")),
        parent: None,
        parent_span: Default::default(),
        table: TableRef(Symbol::from("posts")),
        primary_key: None,
        attributes: Row { fields: attrs, rest: None },
        body: vec![],
        enums: Default::default(),
        enum_defaults: Default::default(),
        class_attr_defaults: Default::default(),
        lexical_json_shadow: false,
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
        composite_primary_key: Vec::new(),
        span: Span::synthetic(),
        visibility_overrides: Vec::new(),
    };

    // Action body: `Post.all` — a Send to a class-level method.
    let recv = Expr::new(sp(), ExprNode::Const { path: vec![Symbol::from("Post")] });
    let action_body = Expr::new(
        sp(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from("all"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );

    let index_action = Action {
        name_span: roundhouse::span::Span::synthetic(),
        name: Symbol::from("index"),
        params: Row::closed(),
        opt_params: vec![],
        kw_params: vec![],
        kwrest_param: None,
        block_param: None,
        body: action_body,
        renders: RenderTarget::Inferred,
        effects: EffectSet::singleton(Effect::DbRead {
            table: TableRef(Symbol::from("posts")),
        }),
    };

    let posts_controller = Controller {
        name: ClassId(Symbol::from("PostsController")),
        parent: Some(ClassId(Symbol::from("ApplicationController"))),
        parent_span: Default::default(),
        body: vec![roundhouse::ControllerBodyItem::Action {
            action: index_action,
            leading_comments: vec![],
            leading_blank_line: false,
        }],
        layout: Default::default(),
        sibling_classes: Vec::new(),
    };

    let routes = RouteTable {
        entries: vec![roundhouse::RouteSpec::Explicit {
            method: HttpMethod::Get,
            path: "/posts".into(),
            controller: ClassId(Symbol::from("PostsController")),
            action: Symbol::from("index"),
            as_name: Some(Symbol::from("posts")),
            constraints: IndexMap::new(),
            scope: roundhouse::ResourceScope::Nested,
        }],
        direct_helpers: vec![],
        redirects: vec![],
        diagnostics: vec![],
    };

    let app = App {
        schema_version: App::SCHEMA_VERSION,
        schema,
        // No database manifest: serde skips the empty database fields.
        databases: Vec::new(),
        model_databases: Default::default(),
        database_refusals: Vec::new(),
        // Passthrough-only and `serde(skip)` — it never round-trips, so
        // the roundtrip fixture has nothing to say about it.
        binary_assets: Vec::new(),
        locale_files: vec!["config/locales/en.yml".to_string()],
        inferred_method_params: Default::default(),
        models: vec![post_model],
        library_classes: vec![],
        singleton_scoped_classes: Vec::new(),
        current_attribute_classes: vec![],
        carried_included_blocks: vec![],
        included_block_statements: Default::default(),
        included_class_attributes: Default::default(),
        lifted_autoload_callbacks: Default::default(),
        boot_native_requires: Vec::new(),
        gem_source_dirs: Vec::new(),
        initializer_filters: Vec::new(),
        initializer_files: Vec::new(),
        initializer_gaps: Vec::new(),
        initializer_programs: Vec::new(),
        environment_programs: Vec::new(),
        framework_load_hooks: Vec::new(),
        class_load_hooks: Vec::new(),
        load_hook_bases: std::collections::BTreeMap::new(),
        load_hooks: Vec::new(),
        ingest_drops: Vec::new(),
        eager_load: Vec::new(),
        database_yml: None,
        sql_functions: Vec::new(),
        rbs_includes: Default::default(),
        controllers: vec![posts_controller],
        routes,
        views: vec![],
        test_modules: vec![],
        fixtures: vec![],
        seeds: None,
        importmap: None,
        stylesheets: vec![],
        rbs_signatures: roundhouse::det::HashMap::default(),
        rbs_singleton_methods: roundhouse::det::HashMap::default(),
        gem_lock: None,
        gem_boundary: Default::default(),
        content_helper_allowed_attributes: Vec::new(),
        test_helper_connections: Vec::new(),
        test_helper_connection_blockers: Vec::new(),
        helper_method_index: roundhouse::det::HashMap::default(),
        helper_singleton_methods: std::collections::BTreeMap::new(),
        view_visible_controller_methods: std::collections::BTreeSet::new(),
        global_id_locate_models: std::collections::BTreeSet::new(),
        global_id_locate_signed_models: std::collections::BTreeSet::new(),
        attachable_unsigned_models: Vec::new(),
        pending_attachment_on_load: Vec::new(),
        load_hook_class_macros: Vec::new(),
        framework_base_includes: Vec::new(),
        partial_local_types: roundhouse::det::HashMap::default(),
        view_ivar_types: roundhouse::det::HashMap::default(),
        html_safe_methods: std::collections::BTreeSet::new(),
        time_formats: std::collections::BTreeMap::new(),
        generated_helper_methods: std::collections::BTreeMap::new(),
        module_mixins: Vec::new(),
        rails_application: None,
        concern_filters: roundhouse::det::HashMap::default(),
        graphql_types: Vec::new(),
        graphql_signatures: Vec::new(),
        concern_spliced_actions: roundhouse::det::HashMap::default(),
        concern_spliced_class_methods: roundhouse::det::HashMap::default(),
        class_methods_carrier_splices: roundhouse::det::HashMap::default(),
        concern_model_items: roundhouse::det::HashMap::default(),
        render_edges: roundhouse::det::HashMap::default(),
        view_feeders: roundhouse::det::HashMap::default(),
        controller_resolutions: roundhouse::det::HashMap::default(),
        sources: vec![],
        // Derived from `sources` and `serde(skip)`, like `binary_assets`.
        const_resolver: Default::default(),
        // Survey-mode ingest record, `serde(skip)`: never round-trips.
        dropped_files: Vec::new(),
        source_index_required: false,
        root: String::new(),
        app_roots: vec!["app".to_string()],
        zeitwerk_loaders: Vec::new(),
        sorbet_type_aliases: Vec::new(),
        initializer_constants: Vec::new(),
        railties: Vec::new(),
        file_statement_gaps: Vec::new(),
        file_programs: Vec::new(),
        boot_loads: Vec::new(),
        boot_conditional_loads: Vec::new(),
        bundler_gems: Vec::new(),
        ruby_delegate_forwarders: Default::default(),
        ruby_declined_delegate_forwarders: Default::default(),
        class_eval_macro_expansions: Default::default(),
        concern_prepended_items: Default::default(),
        library_class_files: Vec::new(),
        library_class_reopens: Default::default(),
    };

    let json = serde_json::to_string_pretty(&app).expect("serialize");
    let back: App = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(app, back, "IR must survive JSON round-trip");
}

#[test]
fn literals_round_trip() {
    let lits = vec![
        Literal::Nil,
        Literal::Bool { value: true },
        Literal::Int { value: 42 },
        Literal::Str { value: "hello".into() },
        Literal::Sym { value: Symbol::from("name") },
    ];
    for lit in lits {
        let json = serde_json::to_string(&lit).unwrap();
        let back: Literal = serde_json::from_str(&json).unwrap();
        assert_eq!(lit, back);
    }
}
