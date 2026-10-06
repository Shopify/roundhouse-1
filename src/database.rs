//! Database identity: which databases an app's environment has, which
//! schema each loads, and which database each model runs on.
//!
//! Rails resolves the first two at boot from `config/database.yml`
//! (through ERB, engine path swaps, and env); Roundhouse does not
//! re-derive that resolution. An app states it as data, a manifest
//! written by `scripts/rails-database-manifest.rb` and named by
//! `roundhouse.yml` (`ingest::database_manifest`). The third fact,
//! ownership, is never taken from the manifest: it is read from source
//! (`connects_to` in abstract classes, inherited), and every routing
//! shape the emitted runtime does not carry is a named refusal
//! (`ingest::database_ownership`, reported by
//! `analyze::database_identity`).
//!
//! An app without a manifest has no `databases`, and its IR serializes
//! exactly as before: every field here is skipped when empty.

use serde::{Deserialize, Serialize};

use crate::ident::{ClassId, Symbol};
use crate::schema::Schema;
use crate::span::Span;

/// The adapter a configuration names, kept per database so a dialect
/// choice downstream (schema rendering, SQL lowering, the runtime
/// driver) is made from the database a model belongs to, never assumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseAdapter {
    Sqlite3,
    Postgresql,
    /// PostGIS is its own adapter (spatial types, its own OIDs), not
    /// postgresql: a postgresql runtime does not cover it.
    Postgis,
    /// `mysql2` or `trilogy`, as written.
    Mysql(String),
}

impl DatabaseAdapter {
    /// The adapter for a configuration's `adapter:` value; `None` for one
    /// Roundhouse does not know.
    pub fn parse(adapter: &str) -> Option<Self> {
        match adapter {
            "sqlite3" => Some(Self::Sqlite3),
            "postgresql" => Some(Self::Postgresql),
            "postgis" => Some(Self::Postgis),
            "mysql2" | "trilogy" => Some(Self::Mysql(adapter.to_string())),
            _ => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Sqlite3 => "sqlite3",
            Self::Postgresql => "postgresql",
            Self::Postgis => "postgis",
            Self::Mysql(name) => name,
        }
    }

    /// Why the emitted runtime cannot run models on a database of this
    /// adapter, or `None` for the one it opens today (sqlite3).
    pub fn runtime_refusal(&self) -> Option<String> {
        match self {
            Self::Sqlite3 => None,
            Self::Postgresql => Some("no emitted runtime adapter for postgresql yet".to_string()),
            Self::Postgis => {
                Some("no emitted runtime adapter for postgis yet (postgis is not postgresql; not substituted)".to_string())
            }
            Self::Mysql(name) => Some(format!("no runtime adapter for {name}; not substituted")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFormat {
    /// `schema.rb`, read by `ingest::schema`.
    Ruby,
    /// `structure.sql`, read by `ingest::structure_sql`.
    Sql,
}

/// The schema file a database loads, as the manifest names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSource {
    /// App-relative.
    pub path: String,
    pub format: SchemaFormat,
    pub sha256: String,
    /// Who chose the file: Rails' `schema_dump_path`, or a declared
    /// source the app's authored setup loads.
    pub resolved_by: String,
}

/// One database configuration of the manifest's environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Database {
    pub name: String,
    pub adapter: DatabaseAdapter,
    pub primary: bool,
    pub replica: bool,
    pub database_tasks: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_source: Option<SchemaSource>,
    /// The database's tables. `None` for the primary, whose schema is
    /// `App::schema` (so its ~40 readers stay as they are), and for every
    /// other database at checkpoint 1: their models are refused, so their
    /// schemas are pinned by hash but not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_search_path: Option<String>,
    /// Non-secret scalar options (`foreign_keys: false`), as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pragmas: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrations_paths: Vec<String>,
}

/// The app's authored `config/database.yml`, resolved at emitted boot the
/// way Rails resolves it (`runtime/database_configurations.rb`): its ERB
/// is compiled into the app as the `renderer` module's `render` method,
/// so the code it calls is analyzed and emitted app code.
///
/// Present when the app states the adapter it is compiled for: by a
/// `database_configurations` manifest's primary, or by `roundhouse.yml`
/// `database_adapter:` alone. The adapter is a compile-time choice (SQL
/// dialect, driver); boot refuses a configuration whose primary names
/// another. Absent, the app keeps the single-database selection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatabaseYml {
    /// App-relative (`config/database.yml`).
    pub path: String,
    /// The adapter the program is compiled for.
    pub adapter: DatabaseAdapter,
    /// The module whose `render` returns the ERB result; `None` when the
    /// file is absent (boot uses `DATABASE_URL` alone, as Rails does) or
    /// its ERB did not compile (a refusal names the construct, and boot
    /// refuses rather than resolve without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<ClassId>,
    /// True when the file exists.
    pub present: bool,
    /// True when a manifest supplied `App::databases`: boot cross-checks
    /// the resolved configurations against those recorded facts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manifest: bool,
}

/// The database a model runs on, read from source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDatabase {
    /// No `connects_to` or routing override on the model or an ancestor.
    Primary,
    /// A literal `connects_to database: { writing:, reading: }`, on the
    /// model or the nearest ancestor that declares one.
    Named {
        writing: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reading: Option<String>,
        declared_by: ClassId,
        span: Span,
    },
    /// The app defines the class-side connection lookup itself
    /// (`def self.connection_pool`, also from a concern), so which
    /// database a query reaches is app code, not configuration.
    AppDefined { method: Symbol, defined_by: ClassId, span: Span },
    /// A routing declaration Roundhouse cannot resolve (non-literal,
    /// conditional, `shards:`, on a concrete class, ...).
    Unresolved { reason: String, span: Span },
}

/// A database-identity fact the emitted runtime does not carry: reported
/// as an error diagnostic at `span` by `analyze::database_identity`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatabaseRefusal {
    pub span: Span,
    /// Stable construct name (`database_configurations`, `connects_to`,
    /// `connected_to`, `connection_override`, `establish_connection`).
    pub construct: String,
    pub detail: String,
    /// Reported as a warning, not an error: a fact recorded but not
    /// verifiable from the tree (the manifest's unverified provenance).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warning: bool,
}
