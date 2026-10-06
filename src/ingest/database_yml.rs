//! `config/database.yml` as app code ([`crate::database::DatabaseYml`]).
//!
//! Rails reads the file through `ActiveSupport::ConfigurationFile`
//! (activesupport 8.1.4 `configuration_file.rb`): when the text contains
//! `<%` it is rendered with `ERB.new(content).result` (stdlib ERB, no trim
//! mode), then `YAML.unsafe_load`ed. The YAML half, environment
//! resolution and `DATABASE_URL` merging are runtime work
//! (`runtime/spinel/cruby/database_configurations.rb`). The ERB half is
//! compiled here, into a module method:
//!
//! ```ruby
//! module DatabaseYml
//!   def self.render
//!     _buf = ""
//!     _buf = _buf + "production:\n  timeout: "
//!     _buf = _buf + (Resiliency.semian_ticket_timeout).to_s
//!     ...
//!     _buf
//!   end
//! end
//! ```
//!
//! so every method the ERB calls is analyzed and emitted app code, with
//! spans on the template. Constructs whose meaning differs between ERB's
//! top-level binding and a module method (`self`, instance variables,
//! `__FILE__`, `binding`, ...), and ERB forms stdlib ERB itself rejects,
//! are named refusals: never silently compiled differently.

use std::path::Path;

use crate::App;
use crate::database::{DatabaseAdapter, DatabaseRefusal, DatabaseYml};
use crate::erb::{self, ErbSegment};
use crate::ident::ClassId;
use crate::span::Span;
use crate::vfs::Vfs;

use super::IngestResult;

/// Rails' `paths["config/database"]` (railties 8.1.4 `configuration.rb:418`).
pub const PATH: &str = "config/database.yml";
/// The emitted renderer module.
pub const RENDERER: &str = "DatabaseYml";

const PREFIX: &str = "module DatabaseYml\n  def self.render\n";
const SUFFIX: &str = "  end\nend\n";

/// Decide whether the app resolves `config/database.yml` at boot, and for
/// which compiled adapter: `roundhouse.yml` `database_adapter:` (at
/// `declared`), else a manifest (at `manifest`) whose primary is
/// postgresql. A sqlite3 app keeps the single-database selection (its
/// runtime opens one file and reads no configuration), so nothing is
/// compiled for it.
pub(super) fn ingest_declared<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    declared: Option<(String, Span)>,
    manifest: Option<Span>,
    app: &mut App,
) -> IngestResult<()> {
    let manifest_primary = app
        .databases
        .iter()
        .find(|db| db.primary && !db.replica)
        .map(|db| db.adapter.clone());
    let refuse = |app: &mut App, span: Span, detail: String| {
        app.database_refusals.push(DatabaseRefusal { span, construct: "database_adapter".into(), detail, warning: false });
    };
    let (adapter, span) = match declared {
        Some((name, span)) => {
            let Some(adapter) = DatabaseAdapter::parse(&name) else {
                refuse(app, span, format!("`roundhouse.yml` `database_adapter: {name}` names no adapter Roundhouse knows"));
                return Ok(());
            };
            if let Some(primary) = manifest_primary.as_ref().filter(|p| **p != adapter) {
                refuse(
                    app,
                    span,
                    format!(
                        "`roundhouse.yml` `database_adapter: {}` disagrees with the manifest's primary adapter `{}`",
                        adapter.name(),
                        primary.name()
                    ),
                );
                return Ok(());
            }
            if adapter != DatabaseAdapter::Postgresql {
                refuse(
                    app,
                    span,
                    format!(
                        "`database_adapter: {}`: boot-time `{PATH}` resolution drives the postgresql runtime only; {}",
                        adapter.name(),
                        adapter.runtime_refusal().unwrap_or_else(|| "the sqlite3 runtime opens its own file and reads no configuration".into())
                    ),
                );
                return Ok(());
            }
            // Admission is the manifest path's: the same refusal the
            // manifest loader gives a postgresql primary.
            if manifest.is_none() {
                if let Some(why) = adapter.runtime_refusal() {
                    refuse(app, span, format!("the compiled-for primary adapter `{}`: {why}", adapter.name()));
                }
            }
            (adapter, span)
        }
        None => match (manifest, manifest_primary) {
            (Some(span), Some(DatabaseAdapter::Postgresql)) => (DatabaseAdapter::Postgresql, span),
            _ => return Ok(()),
        },
    };
    ingest(vfs, dir, adapter, span, manifest.is_some() && !app.databases.is_empty(), app)
}

fn ingest<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    adapter: DatabaseAdapter,
    declared_at: Span,
    manifest: bool,
    app: &mut App,
) -> IngestResult<()> {
    let path = dir.join(PATH);
    let mut yml = DatabaseYml { path: PATH.to_string(), adapter, renderer: None, present: false, manifest };
    let text = match vfs.read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            app.database_refusals.push(DatabaseRefusal {
                span: declared_at,
                construct: "database_yml".into(),
                detail: format!(
                    "`{PATH}` is absent: boot resolves the configuration from `DATABASE_URL` alone and refuses when it is unset, as Rails does"
                ),
                warning: true,
            });
            app.database_yml = Some(yml);
            return Ok(());
        }
        Err(error) => {
            return Err(super::IngestError::Io(std::io::Error::new(
                error.kind(),
                format!("{}: {error}", path.display()),
            )));
        }
    };
    yml.present = true;
    let label = path.display().to_string();
    // First-text-wins: the template is the text every span indexes.
    let file = super::sources::register(&label, &text);
    let at = |start: usize, end: usize| Span { file, start: start as u32, end: end as u32 };
    let mut refusals: Vec<DatabaseRefusal> = Vec::new();
    let refuse = |refusals: &mut Vec<DatabaseRefusal>, span: Span, construct: &str, detail: String| {
        refusals.push(DatabaseRefusal { span, construct: construct.into(), detail, warning: false });
    };

    if app.library_classes.iter().any(|lc| lc.name.0.as_str() == RENDERER) {
        refuse(
            &mut refusals,
            at(0, 0),
            "database_yml",
            format!("the app defines `{RENDERER}`, the name the compiled `{PATH}` renderer takes"),
        );
    }

    match erb::compile_erb_stdlib_mapped(&text) {
        Err(e) => refuse(&mut refusals, at(e.start, e.end), e.construct, format!("`{PATH}`: {}", e.message)),
        Ok((compiled, map)) => {
            let wrapper = format!("{PREFIX}{compiled}{SUFFIX}");
            let shifted: Vec<ErbSegment> = map
                .iter()
                .map(|s| ErbSegment { c_start: s.c_start + PREFIX.len() as u32, c_end: s.c_end + PREFIX.len() as u32, ..*s })
                .collect();
            let to_template = |o: usize| erb::translate_offset(&shifted, o as u32) as usize;
            let before = refusals.len();
            {
                let parsed = ruby_prism::parse(wrapper.as_bytes());
                for error in parsed.errors() {
                    let loc = error.location();
                    refuse(
                        &mut refusals,
                        at(to_template(loc.start_offset()), to_template(loc.end_offset())),
                        "database_yml_ruby",
                        format!("`{PATH}`: the ERB code does not parse: {}", error.message()),
                    );
                }
                // Tag code is copied byte for byte (a text literal never
                // equals its template text: it is quoted), so these are the
                // author's code; `_buf` there would be the renderer's buffer.
                for seg in &map {
                    let (c, e) = (seg.c_start as usize..seg.c_end as usize, seg.e_start as usize..seg.e_end as usize);
                    let code = &text[e.clone()];
                    if compiled.get(c) != Some(code) {
                        continue;
                    }
                    let mut from = 0;
                    while let Some(i) = code[from..].find("_buf") {
                        let i = from + i;
                        let ident = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
                        if !ident(code.as_bytes().get(i.wrapping_sub(1)).filter(|_| i > 0)) && !ident(code.as_bytes().get(i + 4)) {
                            refuse(
                                &mut refusals,
                                at(e.start + i, e.start + i + 4),
                                "database_yml_erb_context",
                                format!("`{PATH}`: the name `_buf` is the compiled renderer's output buffer; not compiled"),
                            );
                        }
                        from = i + 4;
                    }
                }
                if refusals.len() == before {
                    let mut scan = ContextScan { found: Vec::new(), defs: 0, modules: 0 };
                    ruby_prism::Visit::visit(&mut scan, &parsed.node());
                    for (start, end, what) in scan.found {
                        refuse(
                            &mut refusals,
                            at(to_template(start), to_template(end)),
                            "database_yml_erb_context",
                            format!(
                                "`{PATH}`: {what} means something else in Rails' ERB binding (a top-level object) than in the compiled renderer; not compiled"
                            ),
                        );
                    }
                }
            }
            if refusals.len() == before {
                let classes = super::library_class::ingest_library_classes(wrapper.as_bytes(), &label)?;
                let mut found = false;
                for mut class in classes {
                    if class.name.0.as_str() != RENDERER {
                        continue;
                    }
                    for method in &mut class.methods {
                        erb::translate_spans(&mut method.body, &shifted);
                        method.name_span = Span::synthetic();
                    }
                    for e in &mut class.class_ivar_initializers {
                        erb::translate_spans(e, &shifted);
                    }
                    found = true;
                    app.library_classes.push(class);
                }
                if found {
                    yml.renderer = Some(ClassId(crate::ident::Symbol::from(RENDERER)));
                }
            }
        }
    }
    app.database_refusals.extend(refusals);
    app.database_yml = Some(yml);
    Ok(())
}

/// Constructs whose value in ERB's binding (`ERB#result` evaluates in a
/// fresh top-level binding) differs from the renderer method's. Offsets
/// are in the wrapper source.
struct ContextScan {
    found: Vec<(usize, usize, &'static str)>,
    /// The wrapper's own `def self.render` and `module DatabaseYml`.
    defs: usize,
    modules: usize,
}

impl ContextScan {
    fn note(&mut self, loc: ruby_prism::Location<'_>, what: &'static str) {
        self.found.push((loc.start_offset(), loc.end_offset(), what));
    }
}

impl<'pr> ruby_prism::Visit<'pr> for ContextScan {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.check(&node);
    }

    fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.check(&node);
    }
}

impl ContextScan {
    fn check(&mut self, node: &ruby_prism::Node<'_>) {
        use ruby_prism::Node as N;
        let loc = node.location();
        // The wrapper's own `def self.render` receiver.
        if loc.start_offset() < PREFIX.len() && matches!(node, N::SelfNode { .. }) {
            return;
        }
        match node {
            N::DefNode { .. } => {
                self.defs += 1;
                if self.defs > 1 {
                    self.note(loc, "a method definition");
                }
            }
            N::ModuleNode { .. } => {
                self.modules += 1;
                if self.modules > 1 {
                    self.note(loc, "a module definition");
                }
            }
            N::ClassNode { .. } | N::SingletonClassNode { .. } => self.note(loc, "a class definition"),
            N::SelfNode { .. } => self.note(loc, "`self`"),
            N::SourceFileNode { .. } => self.note(loc, "`__FILE__`"),
            N::SourceLineNode { .. } => self.note(loc, "`__LINE__`"),
            N::SourceEncodingNode { .. } => self.note(loc, "`__ENCODING__`"),
            N::YieldNode { .. } => self.note(loc, "`yield`"),
            N::ReturnNode { .. } => self.note(loc, "`return`"),
            N::SuperNode { .. } | N::ForwardingSuperNode { .. } => self.note(loc, "`super`"),
            N::InstanceVariableReadNode { .. }
            | N::InstanceVariableWriteNode { .. }
            | N::InstanceVariableOperatorWriteNode { .. }
            | N::InstanceVariableOrWriteNode { .. }
            | N::InstanceVariableAndWriteNode { .. }
            | N::InstanceVariableTargetNode { .. } => self.note(loc, "an instance variable"),
            N::ClassVariableReadNode { .. }
            | N::ClassVariableWriteNode { .. }
            | N::ClassVariableOperatorWriteNode { .. }
            | N::ClassVariableOrWriteNode { .. }
            | N::ClassVariableAndWriteNode { .. }
            | N::ClassVariableTargetNode { .. } => self.note(loc, "a class variable"),
            N::CallNode { .. } => {
                let call = node.as_call_node().expect("call node");
                if call.receiver().is_none() {
                    let name = call.name();
                    let what = match name.as_slice() {
                        b"binding" => Some("`binding`"),
                        b"__method__" => Some("`__method__`"),
                        b"__dir__" => Some("`__dir__`"),
                        b"local_variables" => Some("`local_variables`"),
                        b"block_given?" => Some("`block_given?`"),
                        b"caller" | b"caller_locations" => Some("the call stack"),
                        b"_erbout" => Some("ERB's `_erbout` buffer"),
                        _ => None,
                    };
                    if let Some(what) = what {
                        self.note(loc, what);
                    }
                }
            }
            N::LocalVariableReadNode { .. } => {
                let read = node.as_local_variable_read_node().expect("lvar");
                if read.name().as_slice() == b"_erbout" {
                    self.note(loc, "ERB's `_erbout` buffer");
                }
            }
            _ => {}
        }
    }
}
