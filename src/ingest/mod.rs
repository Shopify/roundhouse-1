//! Prism → Roundhouse IR.
//!
//! Reads Ruby source (a single file or a Rails app directory) and produces an
//! [`App`](crate::App). This is the reverse of [`crate::emit::ruby`]; together
//! they form the round-trip forcing function.
//!
//! Scope for the initial landing is the tiny-blog fixture: a single model, a
//! single controller with one action, a trivial routes file, and a schema.
//! The ingester deliberately panics on unrecognized constructs — a failed
//! ingest is a signal that the IR (or the recognizer) needs to grow.
//!
//! Organized one submodule per Rails concern: [`app`] orchestrates the
//! whole-directory walk, and [`model`], [`controller`], [`routes`],
//! [`schema`], [`view`], [`test`], [`fixture`] each handle a single source
//! type. The expression-level recursive descent lives in [`expr`]; small
//! cross-cutting Prism AST helpers live in [`util`].

mod alba;
mod boot_requires;
mod graphql_ruby;
pub(crate) mod class_attribute;
pub(crate) mod autoload_glob;
pub(crate) mod cattr;
mod class_configuration;
pub(crate) mod concern_class_attribute;
pub mod allow_browser;
pub mod app;
mod concern_accessors;
pub mod database_manifest;
pub(crate) mod database_ownership;
pub mod database_yml;
mod database_yml_adapters;
pub mod controller;
pub mod environments;
mod engine_config;
mod error_site;
pub mod expr;
mod factory_class;
pub mod fixture;
pub(crate) mod forwarding;
pub(crate) mod controller_macro_synth;
pub(crate) mod devise_routes;
pub mod impersonates;
pub(crate) mod inherited_alias;
mod hook_class_attribute;
mod external_include;
pub mod invisible_captcha;
pub mod generated_helpers;
pub(crate) mod helper_surface;
pub mod jbuilder;
pub mod library_class;
pub mod channel_callbacks;
pub mod current_attributes;
pub mod class_eval_macro;
pub mod delegate;
mod model_delegate;
pub mod thread_mattr;
pub mod model;
pub(crate) mod validates;
mod delegated_type;
mod base_includes;
mod load_hook_includes;
mod model_macros;
mod define_method_loops;
mod macro_associations;
mod trusted_id_attribute;
pub(crate) use define_method_loops::REFUSAL_CONSTRUCT as DEFINE_METHOD_REFUSAL;
pub mod on_load_hooks;
pub mod on_load_reopen;
pub mod opened_modules;
pub mod prism;
pub mod rbi;
pub mod rate_limit;
pub mod roda_app;
pub mod routes;
pub mod singleton_class;
mod sig_autoloads;
pub mod schema;
pub mod sequel_migration;
pub mod sequel_model;
pub mod sorbet_sig;
pub(crate) mod sorbet_check;
pub mod sources;
pub mod type_ascription;
pub mod initializers;
pub mod gem_source_constants;
mod initializer_constants;
mod load_order;
pub mod initializer_programs;
pub mod sql_functions;
pub mod structure_sql;
pub mod survey;
pub mod test;
mod test_mixin;
pub mod util;
pub mod view;
mod view_helper_includes;
mod visibility;
mod zeitwerk;

pub use app::{ingest_app, ingest_app_from_tree, ingest_app_with_vfs};
pub use controller::ingest_controller;
pub use expr::ingest_expr;
pub use fixture::ingest_fixture_file;
pub use jbuilder::ingest_jbuilder;
pub use library_class::{
    classify_class_file, ingest_library_class, ingest_library_classes, ClassKind,
};
pub use model::ingest_model;
pub use roda_app::{ingest_roda_app_with_vfs, is_roda_app};
pub use routes::ingest_routes;
pub use schema::{
    ingest_migration, ingest_migration_with_generated_expression_dialect, ingest_schema,
    ingest_schema_with_generated_expression_dialect,
};
pub use sequel_migration::ingest_sequel_migration;
pub use sequel_model::ingest_sequel_model;
pub use structure_sql::{
    ingest_structure_sql, ingest_structure_sql_with_generated_expression_dialect,
};
pub use test::{ingest_test_file, ingest_test_files};
pub use view::ingest_view;

// Errors ----------------------------------------------------------------

#[derive(Debug)]
pub enum IngestError {
    Io(std::io::Error),
    Parse { file: String, message: String },
    Unsupported { file: String, message: String },
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Parse { file, message } => write!(f, "parse error in {file}: {message}"),
            Self::Unsupported { file, message } => {
                write!(f, "unsupported construct in {file}: {message}")
            }
        }
    }
}

impl std::error::Error for IngestError {}

impl From<std::io::Error> for IngestError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

type IngestResult<T> = Result<T, IngestError>;
