//! Library-shape Ruby emission — for transpiled-shape input where class
//! bodies already contain explicit methods (no Rails DSL expansion).
//! Mirrors `src/emit/typescript/library.rs` in scope; produces one
//! `app/models/<name>.rb` per `LibraryClass`.
//!
//! Ruby is implicit about ivar declaration and global about constant
//! resolution, so this emitter is shorter than the TS analog: no ivar
//! field block, no import partition.

use std::collections::{BTreeMap, BTreeSet};
use crate::det::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::super::EmittedFile;
use crate::App;
use crate::dialect::{AccessorKind, LibraryClass, LibraryClassOrigin, MethodDef, MethodReceiver, MethodVisibility};
use crate::expr::{Expr, ExprNode, InterpPart, LValue, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;
use crate::ty::Ty;

/// Lower library classes for Ruby and emit their source/RBS file pairs, grouping
/// nested declarations with their owners where possible. Lifted Data factories
/// participate in lowering but emit inside their owner's constant declaration,
/// never as standalone class files.
pub(super) fn emit_library_class_decls(app: &App) -> Vec<EmittedFile> {
    let mut lcs: Vec<LibraryClass> = app.library_classes.clone();
    crate::timings::lap_start();
    // A block-taking tag helper gains a `<name>_into(io, …)` variant
    // that writes around a yield into the caller's buffer; the view
    // emit points its appends at it (`lower::tag_block_passing`).
    // FIRST, on the App's shape of the body: the passes below qualify
    // the `capture` the variant is recognised by, and the variant's
    // own body wants every one of them applied.
    crate::lower::tag_block_passing::apply_helpers(&mut lcs);
    crate::timings::lap("emit library: tag_block_passing");
    // A model subclass's own associations and callbacks
    // (`lower::subclass_macros`).
    let subclass_views = crate::lower::subclass_macros::views(app);
    for lc in lcs.iter_mut() {
        if let Some(view) = subclass_views.get(&lc.name) {
            let lineage = crate::lower::subclass_macros::lineage(app, &subclass_views, view);
            crate::lower::model_to_library::push_subclass_model_methods(&mut lc.methods, view, &lineage, &app.models);
        }
    }
    // An STI subclass's relations carry its type condition at runtime.
    crate::lower::sti_scope::push_type_condition_methods(&mut lcs, app);
    apply_scope_lowering(&mut lcs, app);
    crate::timings::lap("emit library: scope_lowering");
    apply_library_partial_render_lowering(&mut lcs, app);
    crate::timings::lap("emit library: partial_render");
    // The Attachment's readers on a content-type attachable (campfire's
    // `OpengraphEmbed`): Rails hands the partial the Attachment, which
    // delegates to the record; the emitted partial takes the record,
    // so the record delegates the Attachment's own readers back
    // through a slot the render seam fills.
    for binding in crate::lower::attachable::attachable_partial_bindings(app) {
        if binding.content_type.is_none() {
            continue;
        }
        if let Some(lc) = lcs.iter_mut().find(|lc| lc.name == binding.class) {
            crate::lower::attachable::push_attachment_delegation(&mut lc.methods, &binding.class);
        }
    }
    crate::timings::lap("emit library: attachment_delegation");
    apply_helper_lowering(&mut lcs, app);
    crate::timings::lap("emit library: helper_lowering");
    // A bare `<x>_path` that `apply_helper_lowering` did not claim and
    // the route table DOES answer is a route helper called somewhere
    // nothing qualifies — campfire's `Messages::AttachmentPresentation`
    // `delegate`s `rails_blob_path` to its context, which leaves three
    // calls to a method the emitted tree defines nowhere.
    crate::lower::route_helper_receiver::qualify_lcs(&mut lcs, app);
    crate::timings::lap("emit library: route_helper_receiver");
    apply_route_param_lowering(&mut lcs, app);
    crate::timings::lap("emit library: route_param");
    apply_raw_helper_monomorphization(&mut lcs, app);
    crate::timings::lap("emit library: raw_helper_monomorphization");
    // send→case grounding, update-kwargs inlining, mailer class-side
    // wrappers, and duration grounding run in the shared post-analyze
    // hook, which covers these library classes (dispatch's plural
    // duration-unit arms arrive already grounded).
    // Transpiled-shape classes carry hand-written accessors that
    // `synth_attr_reader` never sees, so the datetime reader/writer
    // rewrite still runs here for them (Ruby-only). Model-lowered classes
    // get the reader from `synth_attr_reader` (shared, all targets); this
    // re-applies the same reader idempotently and adds the Ruby writer
    // normalize.
    apply_datetime_lowering(&mut lcs, app);
    crate::timings::lap("emit library: datetime");
    apply_boolean_lowering(&mut lcs, app);
    crate::timings::lap("emit library: boolean");
    apply_hydration_nil_lowering(&mut lcs, app);
    crate::timings::lap("emit library: hydration_nil");
    apply_nilsafe_empty_lowering(&mut lcs);
    crate::timings::lap("emit library: nilsafe_empty");
    apply_time_format_lowering(&mut lcs);
    crate::timings::lap("emit library: time_format");
    // Same rooting the view pipeline applies, for the same reason one
    // step over: a class nested in a namespace that SHADOWS what it
    // references. `Message::Broadcasts` is where the broadcast lowering
    // emits `Broadcasts.append(...)`, and inside `module Broadcasts`
    // that constant is the concern itself. Idempotent (an already-rooted
    // head is skipped), so running it in both pipelines is safe.

    apply_constant_rooting(&mut lcs, app, RootingScope::RuntimeOnly);
    crate::timings::lap("emit library: constant_rooting");
    let context;
    let app = if lcs.iter().any(|lc| matches!(lc.origin, Some(LibraryClassOrigin::DataFactory { .. }))) {
        context = App { library_classes: lcs.clone(), ..app.clone() };
        &context
    } else {
        app
    };
    // A nested class goes in its parent's file, as the source wrote
    // it. Given its own, each part-file re-opened the outer class and
    // CLOSED it again, and `TracePoint(:end)` — how a gem implements
    // "end of class body" — fires on every close. A gem validating
    // there saw a body missing what the parent's own file sets, and
    // the tree stopped loading. Naming the same superclass in both
    // places (see `outer_header`) makes the ORDER irrelevant; it
    // cannot make the COUNT of body-ends one.
    // Only into an owner THIS collection emits. A class nested in a
    // MODEL (`Message::Broadcasts`) has its owner emitted by
    // `emit_lowered_models`, a different pipeline — filtering it out
    // here without anyone to splice it into made it vanish from the
    // tree entirely, which the broadcast tests caught at once. Those
    // keep their own file and the extra body-end with it; moving them
    // is a change to the model emit, not to this one.
    let emitted_here: crate::det::HashSet<&str> =
        lcs.iter().map(|lc| lc.name.0.as_str()).collect();
    let owner_in_this_tree = |lc: &LibraryClass| -> Option<String> {
        let owner = file_owner(lc.name.0.as_str(), app);
        (owner != lc.name.0.as_str() && emitted_here.contains(owner.as_str())).then_some(owner)
    };

    let mut children: crate::det::HashMap<String, Vec<&LibraryClass>> =
        crate::det::HashMap::default();
    for lc in &lcs {
        if matches!(lc.origin, Some(LibraryClassOrigin::DataFactory { .. })) {
            continue;
        }
        if let Some(owner) = owner_in_this_tree(lc) {
            children.entry(owner).or_default().push(lc);
        }
    }
    crate::timings::lap("emit library: owners");

    // `constant_readers` asks, per emitted class, which OTHER classes'
    // load-time code reads its constants. Answered by walking every
    // class per class, that is quadratic in the class count: on a
    // 9000-file Core slice the emit sat in `walk_const_paths` for good.
    // So the reads are indexed once, for the duration of this emit.
    let _readers = ConstantReaderIndex::install(app);
    // And every per-reference "is this an app class, and whose file is
    // it" question (`require_path_for_body_const`, `file_owner`) is a
    // lookup rather than a scan of every class: on the full Core
    // closure the scans made assembling the files the emit's longest
    // stage.
    let _names = NameIndex::install(app);
    crate::timings::lap("emit library: reader_index");
    // A module or class reopened in several source files is several
    // LibraryClasses of one name, and they share the one emitted path.
    // Each emits as a reopening of its own and the file holds them all,
    // in source order, as Ruby would have run them; written one per
    // path, the last reopening silently replaced the others (a bridge
    // `def self.included` in one file, its `module ClassMethods` in the
    // next, lost the hook). Nested children splice into the last.
    //
    // A factory class (`Box = Data.define(:w) do … end`) is the
    // declaration that CREATES its class, so it goes first among those
    // of its name, wherever the ingest listed it: a `class Box`
    // reopening written before it opened a plain class and the factory
    // assignment then redefined the constant ("already initialized
    // constant", which Core's StrictWarning raises; Core's
    // adaptive_pricing/breakdown.rb). Ruby itself only reaches a
    // reopening after the assignment, since `class Box` before it would
    // define a different class. Nested children never splice into the
    // factory's block, whose constants are the ENCLOSING scope's
    // (`factory_class::block_constant_writes`), but into the last plain
    // reopening, which a factory class with nested children always has
    // (`owns_a_file`).
    let is_factory = |lc: &LibraryClass| matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. }));
    let order: Vec<usize> = {
        let mut slots: crate::det::HashMap<&str, Vec<usize>> = crate::det::HashMap::default();
        for (i, lc) in lcs.iter().enumerate() {
            slots.entry(lc.name.0.as_str()).or_default().push(i);
        }
        let mut order: Vec<usize> = (0..lcs.len()).collect();
        for at in slots.values().filter(|at| at.len() > 1 && at.iter().any(|&i| is_factory(&lcs[i]))) {
            let mut sorted = at.clone();
            sorted.sort_by_key(|&i| (!is_factory(&lcs[i]), i));
            for (&slot, &i) in at.iter().zip(&sorted) {
                order[slot] = i;
            }
        }
        order
    };
    let mut last_of_name: crate::det::HashMap<&str, usize> = crate::det::HashMap::default();
    for (i, lc) in lcs.iter().enumerate().filter(|(_, lc)| !is_factory(lc)) {
        last_of_name.insert(lc.name.0.as_str(), i);
    }
    let mut out: Vec<EmittedFile> = Vec::new();
    let mut at_path: crate::det::HashMap<PathBuf, usize> = crate::det::HashMap::default();
    // (the declaration's path, the namespace, the target, the statements, their render there, the marker)
    let mut after_declarations: Vec<(PathBuf, String, String, LibraryClass, EmittedFile, String)> = Vec::new();
    // (the declaration's path, the constants, their render there, whether they go before it)
    let mut beside_declarations: Vec<(PathBuf, LibraryClass, EmittedFile, bool)> = Vec::new();
    for (i, lc) in order
        .iter()
        .map(|&i| (i, &lcs[i]))
        .filter(|(_, lc)| !matches!(lc.origin, Some(LibraryClassOrigin::DataFactory { .. })))
        .filter(|(_, lc)| owner_in_this_tree(lc).is_none())
    {
        // `underscore`, not `snake_case`: a namespaced reopen
        // (lobsters' `ActiveRecord::Base.q`, `Net::HTTP`,
        // `ShortId::CandidateId`) nests as `active_record/base.rb` —
        // a literal `::` in the filename breaks the emitted
        // Makefile's dependency list.
        let file_stem = crate::naming::underscore(lc.name.0.as_str());
        let out_path = PathBuf::from(format!("app/models/{file_stem}.rb"));
        // A namespace constant the source wrote in a file of its own
        // goes to that file (`constant_file`): the namespace's file
        // reopens the module without it, and the constant's file
        // reopens it with only the constant.
        let apart: Vec<(usize, String)> = lc
            .constants
            .iter()
            .enumerate()
            .filter_map(|(j, (n, _))| constant_file(lc.name.0.as_str(), n.as_str(), app).map(|stem| (j, stem)))
            .collect();
        // A namespace constant a file program writes after a statement of
        // its own runs in that program (`program_constants`).
        let ran: Vec<usize> = program_constants(lc, app).into_iter().map(|(j, _)| j).collect();
        // A namespace constant a required file writes beside a nested
        // declaration loads with that declaration's file (`nested_file_constant`).
        let beside: Vec<(usize, String, bool)> = lc
            .constants
            .iter()
            .enumerate()
            .filter(|(j, _)| !apart.iter().any(|(k, _)| k == j) && !ran.contains(j))
            .filter_map(|(j, (n, _))| nested_file_constant(lc, n.as_str(), app).map(|(stem, before)| (j, stem, before)))
            .collect();
        // Not one a constant left in the namespace's file reads at load:
        // that file would read it before it is defined.
        let beside: Vec<(usize, String, bool)> = {
            let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
            for (j, (_, value)) in lc.constants.iter().enumerate() {
                if !beside.iter().any(|(k, _, _)| *k == j) && !ran.contains(&j) {
                    walk_const_paths(value, &mut paths);
                }
            }
            let owner = lc.name.0.as_str().trim_start_matches("::");
            let read = |name: &str| {
                paths.iter().any(|p| match p.split_last() {
                    Some((last, prefix)) => last == name && (prefix.is_empty() || prefix.join("::").trim_start_matches("::") == owner),
                    None => false,
                })
            };
            if beside.iter().any(|(j, _, _)| read(lc.constants[*j].0.as_str())) {
                Vec::new()
            } else {
                beside
            }
        };
        // And a namespace statement the source wrote in a nested
        // declaration's file goes after that declaration (`statement_file`).
        let moved: Vec<(usize, String)> = lc
            .unknown_calls
            .iter()
            .enumerate()
            .filter_map(|(j, call)| statement_file(lc.name.0.as_str(), call, app).map(|target| (j, target)))
            .collect();
        let without_apart;
        let lc = if apart.is_empty() && moved.is_empty() && beside.is_empty() && ran.is_empty() {
            lc
        } else {
            let mut copy = lc.clone();
            copy.constants = lc
                .constants
                .iter()
                .enumerate()
                .filter(|(j, _)| {
                    !apart.iter().any(|(k, _)| k == j) && !beside.iter().any(|(k, _, _)| k == j) && !ran.contains(j)
                })
                .map(|(_, c)| c.clone())
                .collect();
            copy.unknown_calls = lc
                .unknown_calls
                .iter()
                .enumerate()
                .filter(|(j, _)| !moved.iter().any(|(k, _)| k == j))
                .map(|(_, c)| c.clone())
                .collect();
            without_apart = copy;
            &without_apart
        };
        // A nested class the source wrote below some of its parent's
        // constants is placed among them as it renders (`NestedPlacement`);
        // the rest are spliced after the parent's header.
        let nested = (last_of_name.get(lc.name.0.as_str()) == Some(&i))
            .then(|| children.get(lc.name.0.as_str()))
            .flatten()
            .map(|kids| nested_blocks(lc, kids, app, &out_path, None));
        let mut files = match &nested {
            Some(nested) => {
                let placing = NestedPlacement::install(lc, nested);
                let mut files = emit_library_class_pair(lc, app, out_path);
                let unplaced = placing.finish();
                splice_nested(&mut files, lc, nested, &unplaced);
                files
            }
            None => emit_library_class_pair(lc, app, out_path),
        };
        for (j, stem) in &apart {
            let constant = LibraryClass {
                includes: Vec::new(),
                methods: Vec::new(),
                class_ivar_initializers: Vec::new(),
                nullable_columns: Vec::new(),
                constants: vec![lcs[i].constants[*j].clone()],
                unknown_calls: Vec::new(),
                visibility_overrides: Vec::new(),
                method_aliases: Vec::new(),
                top_level_constants: Vec::new(),
                conditional_definitions: Vec::new(),
                ..lc.clone()
            };
            files.extend(emit_library_class_pair(&constant, app, PathBuf::from(format!("app/models/{stem}.rb"))));
        }
        let mut stems: Vec<(&str, bool)> = Vec::new();
        for (_, stem, before) in &beside {
            if !stems.contains(&(stem.as_str(), *before)) {
                stems.push((stem, *before));
            }
        }
        for (stem, before) in stems {
            let constants = LibraryClass {
                includes: Vec::new(),
                methods: Vec::new(),
                class_ivar_initializers: Vec::new(),
                nullable_columns: Vec::new(),
                constants: beside
                    .iter()
                    .filter(|(_, s, b)| s == stem && *b == before)
                    .map(|(j, _, _)| lcs[i].constants[*j].clone())
                    .collect(),
                unknown_calls: Vec::new(),
                visibility_overrides: Vec::new(),
                method_aliases: Vec::new(),
                top_level_constants: Vec::new(),
                conditional_definitions: Vec::new(),
                ..lc.clone()
            };
            let path = PathBuf::from(format!("app/models/{stem}.rb"));
            let rendered = emit_library_class_decl(&constants, app, path.clone());
            beside_declarations.push((path, constants, rendered, before));
        }
        let mut targets: Vec<&str> = Vec::new();
        for (_, target) in &moved {
            if !targets.contains(&target.as_str()) {
                targets.push(target);
            }
        }
        for target in targets {
            let statements = LibraryClass {
                includes: Vec::new(),
                methods: Vec::new(),
                class_ivar_initializers: Vec::new(),
                nullable_columns: Vec::new(),
                constants: Vec::new(),
                unknown_calls: moved.iter().filter(|(_, t)| t == target).map(|(j, _)| lcs[i].unknown_calls[*j].clone()).collect(),
                visibility_overrides: Vec::new(),
                method_aliases: Vec::new(),
                top_level_constants: Vec::new(),
                conditional_definitions: Vec::new(),
                ..lc.clone()
            };
            let path = PathBuf::from(format!("app/models/{}.rb", crate::naming::underscore(target)));
            let first = statements.unknown_calls.iter().map(|c| c.span).min_by_key(|s| s.start).unwrap_or_default();
            let site = app
                .sources
                .get(first.file.0 as usize - 1)
                .map(|source| format!("{}:{}", relative_source_path(app, &source.path), source.line_col(first.start).0))
                .unwrap_or_default();
            let owner = lc.name.0.as_str().trim_start_matches("::").to_string();
            let marker = format!("# {site}: the body of `{owner}` goes on here, after `{target}`, as the source writes it");
            let rendered = emit_library_class_decl(&statements, app, path.clone());
            after_declarations.push((path, owner, target.to_string(), statements, rendered, marker));
        }
        for file in files {
            merge_at_path(&mut out, &mut at_path, file);
        }
    }
    // Each into its declaration's file, above or below it as the source
    // writes them; with no such file, back into the namespace's.
    for (path, constants, file, before) in beside_declarations {
        match at_path.get(&path) {
            Some(&j) if before => {
                let mut content = file.content;
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push('\n');
                content.push_str(&out[j].content);
                out[j].content = content;
            }
            Some(_) => merge_at_path(&mut out, &mut at_path, file),
            None => {
                let own = PathBuf::from(format!("app/models/{}.rb", crate::naming::underscore(constants.name.0.as_str())));
                merge_at_path(&mut out, &mut at_path, emit_library_class_decl(&constants, app, own));
            }
        }
    }
    // After every declaration's own file, so each follows its body.
    for (path, owner, target, statements, file, marker) in after_declarations {
        // Statements written elsewhere (a load hook `App::class_load_hooks`
        // places after the declaration) leave the reopen empty: nothing
        // to write, here or in the namespace's file.
        if !reopen_has_body(&file.content, &owner) {
            continue;
        }
        let spliced = at_path.get(&path).and_then(|&j| spliced_into_namespace(&out[j].content, &owner, &target, &file.content, &marker));
        match spliced {
            Some(content) => out[at_path[&path]].content = content,
            // Where it cannot be written in place, it stays in the
            // namespace's file, as before, rendered from that file:
            // its requires are relative to where they run.
            None => {
                let own = PathBuf::from(format!("app/models/{}.rb", crate::naming::underscore(&owner)));
                let file = emit_library_class_decl(&statements, app, own);
                merge_at_path(&mut out, &mut at_path, file);
            }
        }
    }
    crate::timings::lap("emit library: render");
    out
}

/// `content`, a nested declaration's file that wraps it in its
/// namespace's `module` headers (`module Metafields` / `module Owners` /
/// `module Registry`), with the namespace's own statements (`rendered`,
/// a reopen of the namespace) written after the declaration inside the
/// namespace's last `end`s, as the source writes them, under `marker`.
/// `None` when either file has another shape (a compact
/// `module Metafields::Owners::Registry`, a class wrapper).
fn spliced_into_namespace(content: &str, owner: &str, target: &str, rendered: &str, marker: &str) -> Option<String> {
    let segments: Vec<&str> = owner.split("::").collect();
    let depth = segments.len();
    let pad = |k: usize| "  ".repeat(k);
    // A namespace header is a `module` or a `class` line (`class
    // Taxonomy < ApplicationRecord`), the same in both files.
    let is_header = |l: &str, k: usize, seg: &str| {
        l.strip_prefix(&pad(k)).is_some_and(|rest| {
            rest == format!("module {seg}") || rest == format!("class {seg}") || rest.starts_with(&format!("class {seg} < "))
        })
    };
    // The statements' render: anything above its headers (requires),
    // then the headers, its body, and the closing `end`s.
    let lines: Vec<&str> = rendered.lines().collect();
    let first = lines.iter().position(|l| is_header(l, 0, segments[0]))?;
    if lines.len() < first + depth || (0..depth).any(|k| !is_header(lines[first + k], k, segments[k])) {
        return None;
    }
    let headers: Vec<&str> = lines[first..first + depth].to_vec();
    let body = unwrapped(&lines[first..], depth)?;
    // A require of the declaration's own file (the reopen's render names
    // its target like any sibling) would load that file inside itself:
    // Ruby warns "loading in progress, circular require considered
    // harmful" and the require does nothing, so it is left out.
    let own_stem = crate::naming::underscore(target.rsplit("::").next()?);
    let above: Vec<String> = lines[..first]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !split_require_relative(l).is_some_and(|(required, _)| required == own_stem))
        .map(|l| format!("{}{l}", pad(depth)))
        .collect();
    // The declaration's file: the same headers, then the declaration.
    let own: Vec<&str> = content.lines().collect();
    let start = own.iter().position(|l| *l == headers[0])?;
    if own.len() < start + depth + 1 || own[start..start + depth].iter().zip(&headers).any(|(l, h)| l != h) {
        return None;
    }
    let leaf = target.rsplit("::").next()?;
    let declaration = own[start + depth].strip_prefix(&pad(depth))?;
    if !(declaration == format!("module {leaf}") || declaration.starts_with(&format!("class {leaf} ")) || declaration == format!("class {leaf}")) {
        return None;
    }
    // Its last lines are the namespace's `end`s, innermost first.
    let last = own.iter().rposition(|l| !l.trim().is_empty())?;
    let close = (last + 1).checked_sub(depth)?;
    if (0..depth).any(|k| own[close + k] != format!("{}end", pad(depth - 1 - k))) {
        return None;
    }
    let mut out: Vec<String> = own[..close].iter().map(|l| l.to_string()).collect();
    out.push(String::new());
    out.push(format!("{}{marker}", pad(depth)));
    out.extend(above);
    out.push(body);
    out.extend(own[close..].iter().map(|l| l.to_string()));
    let mut joined = out.join("\n");
    joined.push('\n');
    Some(joined)
}

/// Whether `rendered`, a reopen of the namespace `owner`, writes
/// anything inside its headers.
fn reopen_has_body(rendered: &str, owner: &str) -> bool {
    let depth = owner.split("::").count();
    let lines: Vec<&str> = rendered.lines().collect();
    let Some(first) = lines.iter().position(|l| l.starts_with("module ") || l.starts_with("class ")) else {
        return false;
    };
    unwrapped(&lines[first..], depth).is_some_and(|body| !body.trim().is_empty())
}

/// Adds `file` to the rendered files, after the content already
/// rendered at its path (several declarations share one path).
fn merge_at_path(out: &mut Vec<EmittedFile>, at_path: &mut crate::det::HashMap<PathBuf, usize>, file: EmittedFile) {
    match at_path.get(&file.path) {
        Some(&j) => {
            let merged = &mut out[j].content;
            if !merged.ends_with('\n') {
                merged.push('\n');
            }
            merged.push('\n');
            merged.push_str(&file.content);
        }
        None => {
            at_path.insert(file.path.clone(), out.len());
            out.push(file);
        }
    }
}


/// A child's `require_relative` target, expressed from its parent's
/// file instead of its own.
///
/// Both files live under `app/models/`, so the arithmetic is on that
/// tree: resolve the target against the directory the child's file
/// WOULD have had, then re-express it against the parent's.
fn rebase_relative(child_stem: &str, parent_name: &str, target: &str, app: &App) -> String {
    let _ = app;
    // Anchored at the TREE root, not at `app/models`: a require can
    // climb out of the models directory (`../../../runtime/x`), and
    // arithmetic that starts inside it loses those steps silently —
    // the `..` pops an empty stack and the path comes out too short.
    let mut child_dir: Vec<String> =
        vec!["app".to_string(), "models".to_string()];
    child_dir.extend(child_stem.split('/').map(str::to_string));
    child_dir.pop();
    let parent_stem = crate::naming::underscore(parent_name);
    let mut parent_dir: Vec<String> =
        vec!["app".to_string(), "models".to_string()];
    parent_dir.extend(parent_stem.split('/').map(str::to_string));
    parent_dir.pop();

    let mut resolved: Vec<String> = child_dir.clone();
    for seg in target.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                resolved.pop();
            }
            other => resolved.push(other.to_string()),
        }
    }
    // Longest common prefix, then `..` for what the parent has left.
    let common = parent_dir
        .iter()
        .zip(resolved.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = parent_dir.len() - common;
    let mut out: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();
    out.extend(resolved[common..].iter().cloned());
    out.join("/")
}

/// The lines of a nested class's render between its outer wrapper:
/// the `depth` header lines that re-open the parent and the `depth`
/// `end`s that close it. The lines between are already at the
/// indentation the parent's body wants. `None` when nothing is left
/// to splice.
fn unwrapped(body: &[&str], depth: usize) -> Option<String> {
    let first = body.iter().position(|l| !l.trim().is_empty())?;
    let open = first + depth;
    let last = body.iter().rposition(|l| l.trim() == "end")?;
    let close = (last + 1).checked_sub(depth)?;
    (open < close).then(|| body[open..close].join("\n"))
}

/// A nested class's block out of its standalone render, at the
/// indentation the parent's nested body wants (`depth` levels). The
/// render opens either with the parent's `depth` wrapper lines (`module
/// WebPush` / `class Connections`) around the child's own header
/// (`unwrapped`), or, when an outer segment is the runtime's, with ONE
/// compact header naming the whole path (`module
/// WebPush::Connections::Stages`), which is renamed to its last segment.
fn child_block(body: &[&str], depth: usize, full_name: &str) -> Option<String> {
    let first = body.iter().position(|l| !l.trim().is_empty())?;
    let header = body[first];
    for keyword in ["class ", "module "] {
        let compact = format!("{keyword}{full_name}");
        if header == compact || header.starts_with(&format!("{compact} ")) {
            let last = body.iter().rposition(|l| l.trim() == "end")?;
            let simple = full_name.rsplit("::").next().unwrap_or(full_name);
            let mut out = vec![format!("{keyword}{simple}{}", &header[compact.len()..])];
            out.extend(body[first + 1..=last].iter().map(|l| l.to_string()));
            return Some(shifted(&out.join("\n"), depth as isize));
        }
    }
    unwrapped(body, depth)
}

/// `block` moved `levels` two-space indents right (left when negative).
/// A line without that much indentation (heredoc text) stays as it is.
fn shifted(block: &str, levels: isize) -> String {
    let pad = "  ".repeat(levels.unsigned_abs());
    block
        .lines()
        .map(|l| match levels {
            _ if l.is_empty() => String::new(),
            n if n >= 0 => format!("{pad}{l}"),
            _ => l.strip_prefix(pad.as_str()).unwrap_or(l).to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `content` with `blocks` spliced after the parent's header: the nested
/// one at `header_indent` (`spliced_after_header`), else the compact one
/// at the margin (`class WebPush::Connections`, as a class under a
/// runtime namespace renders), whose body is `depth - 1` levels
/// shallower than the blocks are cut for. Without the second arm the
/// children, already left out of the tree as files of their own, were
/// silently gone.
fn spliced_after_either_header(
    content: &str,
    depth: usize,
    keyword: &str,
    full_name: &str,
    blocks: &[String],
) -> Option<String> {
    spliced_after_header(content, &"  ".repeat(depth - 1), keyword, blocks).or_else(|| {
        let compact = format!("{keyword}{full_name}");
        if depth < 2 || !content.lines().any(|l| l == compact || l.starts_with(&format!("{compact} "))) {
            return None;
        }
        let shallower: Vec<String> = blocks.iter().map(|b| shifted(b, 1 - depth as isize)).collect();
        spliced_after_header(content, "", &compact, &shallower)
    })
}

/// `content` with `blocks` placed right after the parent's own header
/// — the `class` line (`module` line for a module: `keyword`) at
/// `header_indent` — or `None` when there is no such line, since
/// anywhere else would be outside the body.
fn spliced_after_header(content: &str, header_indent: &str, keyword: &str, blocks: &[String]) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        out.push(line);
        if !inserted
            && line
                .strip_prefix(header_indent)
                .is_some_and(|rest| rest.starts_with(keyword))
        {
            out.extend(blocks.iter().map(String::as_str));
            inserted = true;
        }
    }
    inserted.then(|| out.join("\n"))
}

/// The nested classes spliced into a parent's file, rendered.
struct NestedBlocks {
    /// Each nested class's block, in load order, with where the source
    /// wrote it among the parent's constants (`nested_position`).
    blocks: Vec<(Option<crate::span::Span>, String)>,
    /// The class each of `blocks` renders.
    names: Vec<String>,
    /// Whether each of `blocks` builds the parent with a def the source
    /// has above it (`builds_after_parent_defs`): it goes after the
    /// parent's methods.
    late: Vec<bool>,
    sidecar_blocks: Vec<String>,
    hoisted: Vec<String>,
}

/// Put each nested class's block inside its parent's file, before the
/// parent's own body — the order the source has, and the only one that
/// works when the parent's class body references them. A nested class
/// the source wrote below some of the parent's constants goes among
/// them instead, before the first constant below it (`NestedPlacement`):
/// monorepo_slices' `IssueTrackingLabelValidator` reads the contract's
/// `SLICE_LABEL_PREFIX` as its body loads.
///
/// The child is rendered by the same function as before and its outer
/// wrapper removed by COUNT, not by pattern: the parent's name has D
/// segments, so the child's render opens with exactly D header lines
/// and closes with exactly D `end`s, and what lies between is already
/// at the indentation the parent's body wants.
///
/// The child's `.rbs` goes into the parent's sidecar the same way. A
/// child with no file of its own has no sidecar of its own either, and
/// a sidecar left out is a class declared nowhere: the typed view of
/// the tree lost every nested class while the `.rb` kept them all.
/// Spelled nested inside the parent's `class`, which is what the
/// standalone sidecar's `module Parent` wrapper said too.
///
/// `inline`: the file program the parent and its kids are all written
/// inline in (`ProgramItem::Inline`). Each kid renders at that path, so
/// its requires are already the file's own, and no sidecar is written.
fn nested_blocks(
    parent: &LibraryClass,
    kids: &[&LibraryClass],
    app: &App,
    parent_rb: &Path,
    inline: Option<&Path>,
) -> NestedBlocks {
    let depth = parent.name.0.as_str().split("::").count();
    // A kid nested in another kid (`Update::Input::VariantInput` under
    // `Update`) is written inside that kid's block, where the source has
    // it, so the enclosing class's body (`field :variants,
    // [VariantInput]`) runs after it as natively. A block of its own
    // after the enclosing one ran that body first: NameError at load.
    let (kids, hosted) = hosted_kids(kids);
    // In the order the source declares them, within each file: Ruby runs
    // `module ErrorHandler` above `class BadRequest …; include
    // ErrorHandler` (rack-oauth2's server/authorize/error.rb; Core's
    // checkouts/one test helper web/client.rb), and the collection may
    // list modules after the classes. Each file's kids take the slots
    // that file's kids had, so kids of different files and those with
    // no position keep their places.
    let kids: Vec<&LibraryClass> = {
        let at: Vec<Option<crate::span::Span>> = kids.iter().map(|kid| declared_at(kid, app)).collect();
        let mut sorted = kids.clone();
        let mut files: Vec<crate::span::FileId> = Vec::new();
        for span in at.iter().flatten() {
            if !files.contains(&span.file) {
                files.push(span.file);
            }
        }
        for file in files {
            let slots: Vec<usize> = (0..kids.len()).filter(|&i| at[i].is_some_and(|span| span.file == file)).collect();
            let mut by_start = slots.clone();
            by_start.sort_by_key(|&i| at[i].map(|span| span.start));
            for (&slot, &i) in slots.iter().zip(&by_start) {
                sorted[slot] = kids[i];
            }
        }
        sorted
    };
    let kids = &kids[..];
    // Order the children so a superclass precedes its subclass. They
    // share one file now, so `class Image < Sound::ImageStruct` runs
    // where the base must already be defined — the requirement the
    // require between two files used to satisfy. Left to the
    // collection's own order this works by luck.
    //
    // A class's constant that builds a class nested in it
    // (`InventoryError::FailedToReserveInventory =
    // FailedToReserveInventoryImpl.new`, Core's checkouts/one
    // ProcessingService) runs, natively, after that class's definition
    // earlier in the body. Each class is its own block here, so the
    // nested one's goes first. Its wrapper reopens the enclosing class
    // with that class's superclass, so it waits for that superclass, not
    // for the enclosing class's block.
    let kids = {
        let names: crate::det::HashSet<&str> =
            kids.iter().map(|k| k.name.0.as_str()).collect();
        let parent_of = |name: &str| -> Option<&str> {
            kids.iter().find(|k| k.name.0.as_str() == name).and_then(|k| k.parent.as_ref()).map(|p| p.0.as_str())
        };
        let nested_in = |inner: &str, outer: &str| inner.len() > outer.len() + 2 && inner.starts_with(outer) && inner[outer.len()..].starts_with("::");
        // The superclass a class's block waits for: one that encloses
        // it is created by the block's own wrapper, so its superclass.
        let superclass_wait = |k: &LibraryClass, built: bool| -> Option<String> {
            let mut wait = k.parent.as_ref().map(|p| p.0.as_str().to_string())?;
            let mut hops = 0;
            while built && nested_in(k.name.0.as_str(), &wait) && hops < 8 {
                wait = parent_of(&wait)?.to_string();
                hops += 1;
            }
            Some(wait)
        };
        let builds: Vec<Vec<String>> = kids
            .iter()
            .map(|k| {
                let own = k.name.0.as_str();
                let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
                for (_, value) in &k.constants {
                    walk_const_paths(value, &mut paths);
                }
                let mut out: Vec<String> = Vec::new();
                for path in &paths {
                    for n in 1..=path.len() {
                        let prefix = path[..n].join("::");
                        if nested_in(&prefix, own) && names.contains(prefix.as_str()) && !out.contains(&prefix) {
                            out.push(prefix);
                        }
                    }
                }
                out
            })
            .collect();
        let builds_of = |k: &LibraryClass| -> &[String] {
            kids.iter().position(|c| std::ptr::eq(*c, k)).map_or(&[][..], |i| builds[i].as_slice())
        };
        let mut pending: Vec<&LibraryClass> = kids.to_vec();
        let mut ordered: Vec<&LibraryClass> = Vec::new();
        let mut placed: crate::det::HashSet<String> = crate::det::HashSet::default();
        while !pending.is_empty() {
            let before = pending.len();
            pending.retain(|k| {
                let built = builds.iter().flatten().any(|d| d == k.name.0.as_str());
                let waits_for = superclass_wait(k, built)
                    .filter(|p| names.contains(p.as_str()) && !placed.contains(p.as_str()));
                // A module the class body `include`s is read the moment
                // the body runs, as its superclass is. (campfire's
                // `class HTTP < Net::HTTP; include Stages; end` beside
                // its `module Stages`.)
                let waits_for_include = k.includes.iter().map(|p| p.0.as_str()).any(|p| names.contains(p) && !placed.contains(p));
                if waits_for.is_some() || waits_for_include {
                    return true;
                }
                if builds_of(k).iter().any(|d| !placed.contains(d.as_str())) {
                    return true;
                }
                placed.insert(k.name.0.as_str().to_string());
                ordered.push(k);
                false
            });
            // A cycle cannot be ordered and cannot be Ruby either;
            // emit the rest as they came rather than loop forever.
            if pending.len() == before {
                ordered.extend(pending.drain(..));
            }
        }
        ordered
    };
    let kids = &kids[..];
    let mut blocks: Vec<(Option<crate::span::Span>, String)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut late: Vec<bool> = Vec::new();
    let mut sidecar_blocks: Vec<String> = Vec::new();
    let mut hoisted: Vec<String> = Vec::new();
    for kid in kids {
        let stem = crate::naming::underscore(kid.name.0.as_str());
        let rb_path = inline.map_or_else(|| PathBuf::from(format!("app/models/{stem}.rb")), Path::to_path_buf);
        let render = || emit_library_class_decl(kid, app, rb_path.clone());
        // The file the blocks run in: a `__dir__` read in a child
        // (`File.join(__dir__, "lookup.sql")`) is anchored on it.
        let own_kids = hosted.iter().find(|(host, _)| std::ptr::eq(*host, *kid)).map_or(&[][..], |(_, d)| d.as_slice());
        let rendered = if own_kids.is_empty() {
            super::expr::with_spliced_into(parent_rb, render)
        } else {
            // Its own kids go into its render the way it goes into the
            // parent's: their requires rebased onto its path, which the
            // lines below rebase again onto the parent's.
            let inner = nested_blocks(kid, own_kids, app, parent_rb, inline);
            let placing = NestedPlacement::install(kid, &inner);
            let mut files = vec![super::expr::with_spliced_into(parent_rb, render)];
            let unplaced = placing.finish();
            splice_nested(&mut files, kid, &inner, &unplaced);
            files.remove(0)
        };
        let lines: Vec<&str> = rendered.content.lines().collect();
        // Requires belong at the top of the file that now holds the
        // class, not buried inside a class body where they would still
        // execute but read as a mistake — and they have to be REBASED
        // on the way: the child's were computed relative to the file
        // it used to have, which sat one directory deeper per level of
        // nesting. Hoisted verbatim they read `../../x` where the
        // parent needs `../x`, which is a load error the day someone
        // reaches that constant.
        for line in lines.iter().filter(|l| l.starts_with("require_relative ")) {
            if inline.is_some() {
                if !hoisted.iter().any(|h| h == line) {
                    hoisted.push(line.to_string());
                }
                continue;
            }
            // A guarded require (`require_line`) keeps its guard.
            let Some((target, guard)) = split_require_relative(line) else { continue };
            let rebased = rebase_relative(&stem, parent.name.0.as_str(), target, app);
            // A sibling that is now in THIS file needs no require, and
            // naming it would name a path that no longer exists: one
            // child referencing another is the common case in a class
            // that nests several.
            let own_dir = format!("{}/", crate::naming::underscore(parent.name.0.as_str()));
            let own_dir = own_dir.rsplit('/').nth(1).map(|d| format!("{d}/"));
            if own_dir.as_ref().is_some_and(|d| rebased.starts_with(d.as_str())) {
                continue;
            }
            // Nor does the file that holds it now, which a nested
            // class's own file required as its enclosing class's
            // (`../processing_service`, rebased to `processing_service`):
            // a file requiring itself is Ruby's circular-require warning
            // (Core's checkouts/one ProcessingService::ForcedFail).
            if own_dir.is_some_and(|d| rebased == d.trim_end_matches('/')) {
                continue;
            }
            let line = format!("require_relative {rebased:?}{guard}");
            if !hoisted.contains(&line) {
                hoisted.push(line);
            }
        }
        // A plain `require "x"` names a library, not a path, and is
        // carried as written.
        for l in lines.iter().filter(|l| l.starts_with("require \"")) {
            if !hoisted.iter().any(|h| h == l) {
                hoisted.push(l.to_string());
            }
        }
        let body: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| !l.starts_with("require"))
            .collect();
        let Some(block) = child_block(&body, depth, kid.name.0.as_str()) else { continue };
        let block = in_singleton_scopes(&block, depth, kid.name.0.as_str());
        let after_defs = builds_after_parent_defs(parent, kid, app);
        let at = nested_position(parent, kid, app).or_else(|| after_defs.then(|| declared_at(kid, app)).flatten());
        blocks.push((at, block));
        late.push(after_defs && at.is_some());
        names.push(kid.name.0.as_str().to_string());
        if inline.is_some() {
            continue;
        }
        // The sidecar `emit_library_class_pair` would have written for
        // the child's own file, minus the same wrapper. Its type names
        // were resolved inside the child's enclosing segments, which
        // are the parent's followed by the child's own — the position
        // it is spliced into.
        // Its own kids' sidecars stay blocks of the parent's, as before:
        // each names its enclosing classes in its wrapper.
        for each in std::iter::once(*kid).chain(own_kids.iter().copied()) {
            let path = inline.map_or_else(
                || PathBuf::from(format!("app/models/{}.rb", crate::naming::underscore(each.name.0.as_str()))),
                Path::to_path_buf,
            );
            let sidecar = super::rbs::emit_library_class_rbs(each, &path, app);
            let lines: Vec<&str> = sidecar.content.lines().collect();
            if let Some(block) = child_block(&lines, depth, each.name.0.as_str()) {
                sidecar_blocks.push(block);
            }
        }
    }
    NestedBlocks { blocks, names, late, sidecar_blocks, hoisted }
}

/// `kids` split into those no other kid encloses, and, for each of
/// those that encloses some, the kids nested in it (at any depth), in
/// their order. Of several declarations of the enclosing name (a class
/// reopened in the file), the host is the last one the source writes
/// above the nested kid (`first_span`), else the last one.
fn hosted_kids<'a>(kids: &[&'a LibraryClass]) -> (Vec<&'a LibraryClass>, Vec<(&'a LibraryClass, Vec<&'a LibraryClass>)>) {
    let encloses = |outer: &str, inner: &str| inner.len() > outer.len() + 2 && inner.starts_with(outer) && inner[outer.len()..].starts_with("::");
    let host_of = |k: &LibraryClass| -> Option<&'a LibraryClass> {
        let own = k.name.0.as_str();
        // The outermost enclosing kid: it hosts every level below it.
        let outer = kids
            .iter()
            .filter(|c| encloses(c.name.0.as_str(), own))
            .map(|c| c.name.0.as_str())
            .min_by_key(|n| n.len())?;
        let at = first_span(k);
        let candidates: Vec<&'a LibraryClass> = kids.iter().copied().filter(|c| c.name.0.as_str() == outer).collect();
        candidates
            .iter()
            .rev()
            .find(|c| matches!((first_span(c), at), (Some(h), Some(a)) if h.file == a.file && h.start < a.start))
            .or(candidates.last())
            .copied()
    };
    let mut direct: Vec<&'a LibraryClass> = Vec::new();
    let mut hosted: Vec<(&'a LibraryClass, Vec<&'a LibraryClass>)> = Vec::new();
    for &k in kids {
        match host_of(k) {
            None => direct.push(k),
            Some(host) => match hosted.iter_mut().find(|(h, _)| std::ptr::eq(*h, host)) {
                Some((_, list)) => list.push(k),
                None => hosted.push((host, vec![k])),
            },
        }
    }
    (direct, hosted)
}

/// The first real source position of `lc`'s body: its superclass, a
/// method, a constant, a class-body call. `None` when it has none or
/// they span several files.
fn first_span(lc: &LibraryClass) -> Option<crate::span::Span> {
    let spans = std::iter::once(lc.parent_span)
        .chain(lc.methods.iter().flat_map(|m| [m.name_span, m.body.span]))
        .chain(lc.constants.iter().map(|(_, e)| e.span))
        .chain(lc.unknown_calls.iter().map(|e| e.span))
        .chain(lc.class_ivar_initializers.iter().map(|e| e.span))
        .filter(|span| span.file.0 != 0 && *span != crate::span::Span::synthetic());
    let mut first: Option<crate::span::Span> = None;
    for span in spans {
        match first {
            Some(f) if f.file != span.file => return None,
            Some(f) if f.start <= span.start => {}
            _ => first = Some(span),
        }
    }
    first
}

/// The classes of `App::singleton_scoped_classes` the Ruby family writes
/// back inside their owner's `class << self`: each one spliced into a
/// library owner's file (`file_owner`), as `nested_blocks` writes it.
/// One a load-time conditional declares, or written in a file of its
/// own, stays hoisted onto the owner, as on the other targets.
pub(crate) fn singleton_scoped(app: &App) -> crate::det::HashSet<String> {
    let library: crate::det::HashSet<&str> = app.library_classes.iter().map(|lc| lc.name.0.as_str()).collect();
    app.singleton_scoped_classes
        .iter()
        .map(|c| c.0.as_str())
        .filter(|name| {
            let Some((owner, _)) = name.rsplit_once("::") else { return false };
            let file = file_owner(name, app);
            library.contains(owner)
                && file != *name
                && library.contains(file.as_str())
                && conditional_owner(name, app).is_none()
        })
        .map(str::to_string)
        .collect()
}

/// `block`, the nested class `kid` unwrapped from its `depth` outer
/// header lines, with a `class << self … end` around each part the
/// source declared in one (`singleton_scoped`): `class Helper … end`
/// for `X::Helper` declared in `class X; class << self`. The block opens
/// with one header line per segment of `kid` below `depth` and closes
/// with as many `end`s (`unwrapped`), so the singleton class of the
/// first `k` segments opens after `k` headers and closes before the last
/// `k` ends.
fn in_singleton_scopes(block: &str, depth: usize, kid: &str) -> String {
    let segments: Vec<&str> = kid.split("::").collect();
    let headers = segments.len().saturating_sub(depth);
    let mut lines: Vec<String> = block.lines().map(str::to_string).collect();
    let structural = lines.len() >= 2 * headers
        && lines[..headers].iter().all(|l| {
            let t = l.trim_start();
            t.starts_with("class ") || t.starts_with("module ")
        })
        && lines[lines.len() - headers..].iter().all(|l| l.trim() == "end");
    if !structural {
        return block.to_string();
    }
    // Innermost first: the outer insertions then keep its positions.
    for n in (depth + 1..=segments.len()).rev() {
        if !super::expr::is_singleton_scoped(&segments[..n].join("::")) {
            continue;
        }
        let k = n - 1 - depth;
        let indent: String = lines[k].chars().take_while(|c| c.is_whitespace()).collect();
        lines.insert(k, format!("{indent}class << self"));
        let at = lines.len() - k;
        lines.insert(at, format!("{indent}end"));
    }
    lines.join("\n")
}

/// Where the source wrote `kid` among `parent`'s constants: the first
/// position of its body (its superclass, a method, a constant, a
/// class-body call) when that lies below one of the parent's constants
/// in the same file. `None` (a block spliced after the header) when it
/// is above them all, has no real span, or spans several files.
///
/// Or below one of the parent's class-body calls: Core's
/// customer_foundations/phone_number.rb writes `attribute(:value,
/// :string)` and then `class Valid < PhoneNumber`, whose `inherited`
/// hook copies the attributes the parent has so far; spliced after the
/// header, `Valid` had none, and its `transform(:value, …)` raised.
///
/// A module the parent mixes in keeps its place after the header too:
/// the emitted body opens with its includes, and a replayed `prepend`
/// or `extend` stands before the first constant below the call, which
/// is above the module's own body when ingest made the call from a
/// block that also defines the module. Ruby cannot mix in a module
/// before it is defined. `Module#concerning :Summary do … end` below a
/// constant is such a module: ingest writes it as `module Summary` and
/// `include Summary` (or `prepend`), and placed among the constants it
/// was written below the include (NameError: uninitialized constant
/// `Ledger::Summary`).
fn nested_position(parent: &LibraryClass, kid: &LibraryClass, app: &App) -> Option<crate::span::Span> {
    if mixes_in(parent, &kid.name) {
        return None;
    }
    let first = declared_at(kid, app)?;
    let above = |span: &crate::span::Span| span.file == first.file && *span != crate::span::Span::synthetic() && span.start < first.start;
    (parent.constants.iter().any(|(_, value)| above(&value.span)) || parent.unknown_calls.iter().any(|call| above(&call.span)))
        .then_some(first)
}

/// Whether `kid`'s body, as it loads, builds `parent` or calls one of its
/// class methods (`Parent.new(…)`, `Parent.m`) through a def `parent`'s
/// source has above `kid`. Core's checkouts/one web/violation_error_message.rb
/// writes `def initialize(code, non_localized_description, …)` and then
/// `module Proposal; LineItemLimitReached = ViolationErrorMessage.new(…)`:
/// written above the parent's methods the constant reached
/// `BasicObject#initialize` (given 3, expected 0) at load.
fn builds_after_parent_defs(parent: &LibraryClass, kid: &LibraryClass, app: &App) -> bool {
    let Some(at) = declared_at(kid, app) else { return false };
    let parent_name = parent.name.0.as_str();
    let defined_above = |name: &str, receiver: crate::dialect::MethodReceiver| {
        parent.methods.iter().any(|m| {
            m.receiver == receiver
                && m.name.as_str() == name
                && !m.name_span.is_synthetic()
                && m.name_span.file == at.file
                && m.name_span.start < at.start
        })
    };
    fn names_parent(recv: &Option<Expr>, parent_name: &str) -> bool {
        let Some(r) = recv else { return false };
        let ExprNode::Const { path } = &*r.node else { return false };
        let joined = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
        let joined = joined.trim_start_matches("::");
        joined == parent_name || parent_name.ends_with(&format!("::{joined}"))
    }
    fn walk(e: &Expr, hit: &mut dyn FnMut(&str) -> bool, parent_name: &str) -> bool {
        match &*e.node {
            ExprNode::Lambda { .. } => false,
            ExprNode::Send { recv, method, args, block, .. } => {
                if names_parent(recv, parent_name) && hit(method.as_str()) {
                    return true;
                }
                recv.iter().chain(args.iter()).any(|a| walk(a, hit, parent_name))
                    || (!stores_block(recv, method.as_str())
                        && block.as_ref().is_some_and(|b| match &*b.node {
                            ExprNode::Lambda { body, .. } => walk(body, hit, parent_name),
                            _ => walk(b, hit, parent_name),
                        }))
            }
            _ => {
                let mut found = false;
                e.node.for_each_child(&mut |child| {
                    if !found && walk(child, hit, parent_name) {
                        found = true;
                    }
                });
                found
            }
        }
    }
    let mut hit = |method: &str| {
        if method == "new" {
            defined_above("initialize", crate::dialect::MethodReceiver::Instance)
        } else {
            defined_above(method, crate::dialect::MethodReceiver::Class)
        }
    };
    kid.constants.iter().any(|(_, value)| walk(value, &mut hit, parent_name))
        || kid.unknown_calls.iter().any(|call| walk(call, &mut hit, parent_name))
}

/// Where the source defines `kid`: its superclass, which is in its
/// header, else its first body position (`first_span`), or an earlier
/// `class X` / `module X` of it in the same file that declares nothing
/// (`App::library_class_declarations`). Ruby defines the constant at
/// that one. Core's component/schema.rb forward-declares `class
/// Validator; end` above `class_attribute :_validator_class, …,
/// default: Validator` and writes the body at the bottom; placed by the
/// body, the block followed the seed that reads it (NameError at load).
/// A body can hold members the source wrote elsewhere: Core's
/// checkouts/one web/client.rb `SessionNegotiationInputFromResult <
/// SessionNegotiationInput` carries copies of its superclass's
/// `class << self` methods, which put it above the module it includes.
/// One with no position of its own is placed at its declaration when
/// the source declares it in one file: hedwig's
/// configuration/enqueue_retry_handler.rb `module Interface`, whose
/// sig-only body carries nothing, above the class that includes it.
fn declared_at(kid: &LibraryClass, app: &App) -> Option<crate::span::Span> {
    let header = kid.parent_span;
    let first = if header.file.0 != 0 && header != crate::span::Span::synthetic() {
        header
    } else if let Some(first) = first_span(kid) {
        first
    } else {
        let declared: Vec<crate::span::Span> =
            app.library_class_declarations.iter().filter(|(name, _)| *name == kid.name).map(|(_, at)| *at).collect();
        let first = declared.iter().min_by_key(|at| at.start)?;
        return declared.iter().all(|at| at.file == first.file).then_some(*first);
    };
    Some(
        app.library_class_declarations
            .iter()
            .filter(|(name, at)| *name == kid.name && at.file == first.file && at.start < first.start)
            .map(|(_, at)| *at)
            .min_by_key(|at| at.start)
            .unwrap_or(first),
    )
}

/// The `require_relative` lines, in source order, of the nested classes
/// of `parent` that keep a file of their own (a module's children) and
/// that `at`'s source file declares above it, not yet in `required`.
/// Natively a module body's statement runs after the classes written
/// above it; their own files loaded after the module's file, so the
/// statement ran first. Core's GraphApiTestSchemaHelper calls
/// `api_versioning_test_setup`, declares its test schema's classes
/// (whose `visibility:` names fixture-only ApiChange handles), then
/// `reset_api_versioning_test_setup`: the reset ran before the classes,
/// and each `visibility:` raised InvalidApiChange.
fn nested_files_above(
    parent: &LibraryClass,
    at: crate::span::Span,
    app: &App,
    out_path: &Path,
    required: &mut BTreeSet<String>,
) -> Vec<String> {
    if !super::expr::native_keywords() || at == crate::span::Span::synthetic() || at.file.0 == 0 {
        return Vec::new();
    }
    let owner = parent.name.0.as_str().trim_start_matches("::");
    let prefix = format!("{owner}::");
    let mut kids: Vec<(usize, String)> = Vec::new();
    for kid in &app.library_classes {
        let name = kid.name.0.as_str().trim_start_matches("::");
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        if rest.contains("::") || required.contains(name) || kids.iter().any(|(_, k)| k == name) {
            continue;
        }
        if file_owner(name, app) != name || is_model_named(name, app) {
            continue;
        }
        let Some(declared) = declared_at(kid, app) else { continue };
        if declared.file == at.file && declared.start < at.start {
            kids.push((declared.start as usize, name.to_string()));
        }
    }
    kids.sort();
    let dir = out_path.parent().unwrap_or(Path::new(""));
    kids.into_iter()
        .map(|(_, name)| {
            let anchor = format!("app/models/{}", super::library_file_stem(&name, app));
            required.insert(name);
            format!("require_relative {:?}", relpath(dir, &anchor))
        })
        .collect()
}

/// Whether `parent`'s body includes `module`, or prepends or extends it
/// by a replayed call naming it (relative to `parent` or in full).
fn mixes_in(parent: &LibraryClass, module: &ClassId) -> bool {
    let bare = |name: &str| name.trim_start_matches("::").to_string();
    let target = bare(module.0.as_str());
    let names = |written: &str| {
        let written = bare(written);
        written == target || format!("{}::{written}", bare(parent.name.0.as_str())) == target
    };
    parent.includes.iter().any(|inc| names(inc.0.as_str()))
        || parent.unknown_calls.iter().any(|call| {
            matches!(&*call.node, ExprNode::Send { recv: None, method, args, .. }
                if matches!(method.as_str(), "include" | "prepend" | "extend")
                    && args.iter().any(|arg| matches!(&*arg.node, ExprNode::Const { path }
                        if names(&path.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("::")))))
        })
}

/// Splice what `NestedPlacement` did not place among the constants
/// (`unplaced`, indices into `nested.blocks`) after the parent's
/// header, and the requires and sidecar blocks of them all.
fn splice_nested(files: &mut [EmittedFile], parent: &LibraryClass, nested: &NestedBlocks, unplaced: &[usize]) {
    if nested.blocks.is_empty() {
        return;
    }
    let depth = parent.name.0.as_str().split("::").count();
    let blocks: Vec<String> = unplaced.iter().map(|&j| nested.blocks[j].1.clone()).collect();
    let Some(rb) = files.iter_mut().find(|f| f.path.extension().is_some_and(|e| e == "rb")) else {
        return;
    };
    // Every block may already be among the constants; the requires
    // they need still open the file.
    let spliced = if blocks.is_empty() {
        rb.content.trim_end_matches('\n').to_string()
    } else {
        let Some(spliced) =
            spliced_after_either_header(&rb.content, depth, header_keyword(parent), parent.name.0.as_str(), &blocks)
        else {
            return;
        };
        spliced
    };
    // A require the parent's own file already carries is not repeated.
    let hoisted: Vec<&String> = nested.hoisted.iter().filter(|r| !rb.content.lines().any(|l| l == r.as_str())).collect();
    let mut content = String::new();
    for r in &hoisted {
        content.push_str(r);
        content.push('\n');
    }
    if !hoisted.is_empty() {
        content.push('\n');
    }
    content.push_str(&spliced);
    content.push('\n');
    rb.content = content;

    // Only once the `.rb` took the body: a sidecar must describe the
    // file beside it, and a child left out of the one stays out of the
    // other.
    splice_sidecar(files, depth, header_keyword(parent), parent.name.0.as_str(), &nested.sidecar_blocks);
}

/// The keyword of `parent`'s own header line. Only a module that holds
/// classes its `class << self` declares (`singleton_scope_owner`) has
/// nested classes spliced into its file.
fn header_keyword(parent: &LibraryClass) -> &'static str {
    if parent.is_module { "module " } else { "class " }
}

fn splice_sidecar(files: &mut [EmittedFile], depth: usize, keyword: &str, full_name: &str, sidecar_blocks: &[String]) {
    let Some(rbs) = files.iter_mut().find(|f| f.path.extension().is_some_and(|e| e == "rbs"))
    else {
        return;
    };
    if let Some(spliced) = spliced_after_either_header(&rbs.content, depth, keyword, full_name, sidecar_blocks) {
        rbs.content = format!("{spliced}\n");
    }
}

/// The nested classes of the class being rendered whose source lies
/// below some of its constants (`nested_position`): each is written
/// right before the first constant the source has below it, or after
/// the constants, rather than after the class header. Installed by the
/// library emit around rendering the class itself, and only for it.
struct NestedPlacement;

struct NestedPending {
    owner: String,
    /// Index into `NestedBlocks::blocks`, position, block, placed, and
    /// whether it waits for the owner's methods (`NestedBlocks::late`).
    blocks: Vec<(usize, crate::span::Span, String, bool, bool)>,
    /// The blocks of classes a load-time conditional of the owner
    /// declares (`ConditionalArm::declarations`): index, class, block,
    /// placed. Only that conditional's branch writes them.
    declared: Vec<(usize, String, String, bool)>,
}

thread_local! {
    static NESTED_PLACEMENT: std::cell::RefCell<Option<NestedPending>> = const { std::cell::RefCell::new(None) };
}

struct NestedPlacementGuard {
    count: usize,
}

impl NestedPlacement {
    fn install(parent: &LibraryClass, nested: &NestedBlocks) -> NestedPlacementGuard {
        let in_branch: Vec<&str> = parent
            .conditional_definitions
            .iter()
            .flat_map(|c| c.all_arms())
            .flat_map(|arm| arm.declarations.iter())
            .map(|d| d.name.0.as_str())
            .collect();
        let declared_in = |name: &str| in_branch.iter().any(|d| declares(d, name));
        let blocks = nested
            .blocks
            .iter()
            .enumerate()
            .filter(|(j, _)| !declared_in(&nested.names[*j]))
            .filter_map(|(j, (at, block))| at.map(|at| (j, at, block.clone(), false, nested.late[j])))
            .collect();
        let declared = nested
            .blocks
            .iter()
            .enumerate()
            .filter(|(j, _)| declared_in(&nested.names[*j]))
            .map(|(j, (_, block))| (j, nested.names[j].clone(), block.clone(), false))
            .collect();
        NESTED_PLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(NestedPending { owner: parent.name.0.as_str().to_string(), blocks, declared })
        });
        NestedPlacementGuard { count: nested.blocks.len() }
    }

    /// Write, in load order and indented one level more, the blocks of
    /// the classes `declaration` (one of `owner`'s conditional branches')
    /// declares: the class and what it nests.
    fn place_declared(owner: &ClassId, s: &mut String, declaration: &str) {
        NESTED_PLACEMENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(pending) = slot.as_mut().filter(|p| p.owner == owner.0.as_str()) else { return };
            for (_, name, block, placed) in pending.declared.iter_mut() {
                if !*placed && declares(declaration, name) {
                    for line in block.lines() {
                        if line.trim().is_empty() {
                            s.push('\n');
                        } else {
                            s.push_str("  ");
                            s.push_str(line);
                            s.push('\n');
                        }
                    }
                    *placed = true;
                }
            }
        });
    }

    /// Write, in load order, the pending blocks of `owner` that `here`
    /// accepts by their source position, except those that wait for the
    /// owner's methods.
    fn place(owner: &ClassId, s: &mut String, here: impl Fn(&crate::span::Span) -> bool) {
        Self::place_where(owner, s, |at, late| !late && here(at));
    }

    /// Write the pending blocks that wait for the owner's methods, and
    /// that `here` accepts: called once those methods are written.
    fn place_late(owner: &ClassId, s: &mut String, here: impl Fn(&crate::span::Span) -> bool) {
        Self::place_where(owner, s, |at, late| late && here(at));
    }

    fn place_where(owner: &ClassId, s: &mut String, here: impl Fn(&crate::span::Span, bool) -> bool) {
        NESTED_PLACEMENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(pending) = slot.as_mut().filter(|p| p.owner == owner.0.as_str()) else { return };
            for (_, at, block, placed, late) in pending.blocks.iter_mut() {
                if !*placed && here(at, *late) {
                    s.push_str(block);
                    s.push('\n');
                    *placed = true;
                }
            }
        });
    }
}

impl NestedPlacementGuard {
    /// The blocks left for the header, as indices in load order.
    fn finish(self) -> Vec<usize> {
        let placed: Vec<usize> = NESTED_PLACEMENT.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|p| {
                    let placed = p.blocks.iter().filter(|b| b.3).map(|b| b.0);
                    placed.chain(p.declared.iter().filter(|b| b.3).map(|b| b.0)).collect()
                })
                .unwrap_or_default()
        });
        (0..self.count).filter(|j| !placed.contains(j)).collect()
    }
}

impl Drop for NestedPlacementGuard {
    fn drop(&mut self) {
        NESTED_PLACEMENT.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Whether the class `name` is `declaration` or nested in it.
fn declares(declaration: &str, name: &str) -> bool {
    name.strip_prefix(declaration).is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

use crate::facades::{Facade, EXTRAS_FACADES};

/// Swap façade-fated extras emits (scaffold base: spinel + the trees
/// derived from it). No-op when the app doesn't define the class —
/// the path simply isn't present.
pub(super) fn apply_extras_facades(files: &mut [(String, String)], app: &App) {
    for f in EXTRAS_FACADES.iter().filter(|f| f.applies(app)) {
        let Facade { stem, rb, rbs, .. } = f;
        for (path, content) in files.iter_mut() {
            if path == &format!("{stem}.rb") {
                *content = (*rb).to_string();
            } else if path == &format!("{stem}.rbs") || path == &format!("sig/{stem}.rbs") {
                // BOTH forms, and the `sig/` one is the one that fires.
                // `emit_library_class_rbs` writes sidecars `sig/`-rooted
                // (`sig_path_for`), and `spin_shape` only flattens them to
                // file-adjacent LATER — so at this point every sidecar is
                // still `sig/app/models/<stem>.rbs`. Matching only the bare
                // form meant this arm never fired for any of the four
                // façades: the `.rb` swapped, the `.rbs` did not, and every
                // consumer read roundhouse's own inference instead of the
                // hand-written contract sitting beside the façade.
                //
                // What that cost, end to end: the contract declares
                // `OpenSSL::RandomSource.random_bytes: (Integer) -> String`,
                // and without it that call typed untyped, which widened
                // `str` in `Utils.random_str`, which made
                // `CandidateId#to_s` untyped, which put a
                // `def to_s: () -> untyped` in the program — and one of
                // those widens every poly `.to_s` (matz/spinel#4090). Ten
                // links on, the cookie jar's `@inbound[k.to_s] = v` had
                // untyped KEYS and the C build stopped on a PolyPolyHash
                // reaching a `Hash[String, String]` slot.
                *content = (*rbs).to_string();
            }
        }
    }
}

/// CRuby: put the verbatim source-shape emit back over the façades —
/// the real net/https / resolv / ipaddr are available there and the
/// vendored bodies run as written. Re-renders the library classes so
/// the restored bytes are exactly what the base would have emitted
/// without the swap.
pub(super) fn restore_extras_facades(files: &mut [(String, String)], app: &App) {
    // Only a tree that carries a facade file has one to restore; the
    // re-emit below is the whole library-class emit again, so a tree
    // without one (Shopify Core) skips it rather than paying twice.
    let is_facade = |p: &str| {
        EXTRAS_FACADES
            .iter()
            .any(|f| p == format!("{}.rb", f.stem) || p == format!("{}.rbs", f.stem))
    };
    if !files.iter().any(|(p, _)| is_facade(p)) {
        return;
    }
    for ef in emit_library_class_decls(app) {
        let p = ef.path.to_string_lossy().into_owned();
        if EXTRAS_FACADES
            .iter()
            .any(|f| p == format!("{}.rb", f.stem) || p == format!("{}.rbs", f.stem))
        {
            for (path, content) in files.iter_mut() {
                if *path == p {
                    content.clone_from(&ef.content);
                }
            }
        }
    }
}

/// Ruby-family pre-emit pass: a partial render in a LIBRARY-CLASS body
/// (lobsters' ApplicationHelper#link_post renders a partial from a
/// helper). A helper's render RETURNS the string, so the rewrite is the
/// bare `Views::<Mod>.<stem>(record, closure…, extras…)` call — locals
/// bind by name against the partial's contract, everything else nil
/// (a module body has no controller ivars to thread). Slashed partial
/// names only; a bare name has no module context here.
///
/// BOTH spellings Rails accepts: `render partial: "x/y", locals: {…}`
/// and the SHORTHAND `render "x/y"` (locals, if any, ride a trailing
/// hash). campfire's `MessagesHelper#message_tag` uses the shorthand in
/// a `rescue` body — `render "messages/unrenderable"` is what a message
/// row renders when building it raised — and unclaimed it emitted as a
/// bare `render` call, which is a method a helper module does not
/// have.
///
/// A name the locals do not bind falls back to the enclosing method's
/// own PARAMETER of that name before it falls back to nil. Same policy
/// as the controller-side rewrite ("a same-named local wins"), and here
/// it is what keeps the call type-correct: the partial's record
/// parameter is typed as its record (`unrenderable(Message message,
/// …)`), so a nil there is a seed contradiction that stops a strict
/// build. The divergence it buys is bounded and in the safe direction —
/// Rails supplies no local at all for a shorthand render, so a partial
/// that reads one raises there; ours renders with the caller's
/// same-named value.
pub(crate) fn apply_library_partial_render_lowering(lcs: &mut [LibraryClass], app: &App) {
    let contracts = crate::lower::view_to_library::partial_call_contracts(
        &app.views,
        &app.controllers,
        &app.library_classes,
    );
    if contracts.is_empty() {
        return;
    }
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            let in_scope: Vec<Symbol> = m.params.iter().map(|p| p.name.clone()).collect();
            rewrite_library_partial_render(&mut m.body, &contracts, &in_scope);
        }
    }
}

/// Symbol-keyed entries of a hash ARG, as partial locals. Rails lets a
/// shorthand render carry its locals in a trailing hash (`render
/// "messages/message", message: m`), which is the same map `locals:`
/// spells one level down.
fn hash_locals(arg: Option<&Expr>) -> Vec<(Symbol, Expr)> {
    use crate::expr::Literal;
    let Some(arg) = arg else { return Vec::new() };
    let ExprNode::Hash { entries, .. } = &*arg.node else { return Vec::new() };
    entries
        .iter()
        .filter_map(|(k, v)| match &*k.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some((value.clone(), v.clone())),
            _ => None,
        })
        .collect()
}

fn rewrite_library_partial_render(
    expr: &mut Expr,
    contracts: &crate::det::HashMap<
        (String, String),
        crate::lower::view_to_library::PartialCallContract,
    >,
    in_scope: &[Symbol],
) {
    use crate::expr::Literal;
    expr.node
        .for_each_child_mut(&mut |c| rewrite_library_partial_render(c, contracts, in_scope));
    let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { return };
    if method.as_str() != "render" && method.as_str() != "render_to_string" {
        return;
    }
    let Some(first) = args.first() else { return };
    let mut partial: Option<String> = None;
    let mut locals: Vec<(Symbol, Expr)> = Vec::new();
    match &*first.node {
        // The shorthand — the string IS the partial name.
        ExprNode::Lit { value: Literal::Str { value } } => {
            partial = Some(value.clone());
            locals = hash_locals(args.get(1));
        }
        ExprNode::Hash { entries, kwargs: true } => {
            for (k, v) in entries {
                let key = match &*k.node {
                    ExprNode::Lit { value: Literal::Sym { value } } => value.as_str(),
                    _ => "",
                };
                match key {
                    "partial" => {
                        if let ExprNode::Lit { value: Literal::Str { value } } = &*v.node {
                            partial = Some(value.clone());
                        }
                    }
                    "locals" => {
                        if let ExprNode::Hash { entries: le, .. } = &*v.node {
                            for (lk, lv) in le {
                                if let ExprNode::Lit { value: Literal::Sym { value } } = &*lk.node {
                                    locals.push((value.clone(), lv.clone()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => return,
    }
    let Some(pname) = partial else { return };
    let Some((dir, base)) = pname.rsplit_once('/') else { return };
    let module_camel = crate::naming::camelize(&crate::naming::snake_case(dir));
    let stem = base.trim_start_matches('_').to_string();
    let Some(contract) = contracts.get(&(module_camel.clone(), stem.clone())) else { return };
    let span = expr.span;
    let nil = || sp_expr(ExprNode::Lit { value: Literal::Nil });
    let lookup = |name: &str| -> Option<Expr> {
        locals
            .iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, v)| v.clone())
            .or_else(|| {
                in_scope.iter().find(|p| p.as_str() == name).map(|p| {
                    Expr::new(span, ExprNode::Var { id: VarId(0), name: p.clone() })
                })
            })
    };
    let mut view_args: Vec<Expr> = Vec::new();
    view_args.push(lookup(&contract.record).unwrap_or_else(nil));
    for n in &contract.closure {
        view_args.push(lookup(n).unwrap_or_else(nil));
    }
    view_args.extend(contract.extras_args(lookup, nil, span));
    *expr = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(sp_expr(ExprNode::Const {
                path: vec![Symbol::from("Views"), Symbol::from(module_camel)],
            })),
            method: crate::lower::view::view_method_name(&stem),
            args: view_args,
            block: None,
            parenthesized: true,
        },
    );
}

/// Ruby-family pre-emit pass: synthesize each model's scope class methods
/// and normalize scope chains (`Story.base(u).positive_ranked` ->
/// `Story.positive_ranked(Story.base(u))`) across a group of library
/// classes, so they run against `ActiveRecord::Relation`. Lives on the
/// Ruby emit path — these methods reference a runtime only the CRuby/JRuby
/// tree provides, so the shared `lower/` must stay target-agnostic. Run
/// once per LC group (models / controllers / library_classes) before
/// rendering. A strict no-op for scope-free apps (the blog).
/// Generate `app/models/relation_scopes.rb` — an
/// `ActiveRecord::Relation` reopen delegating each declared model
/// scope to the model's fixed-arity `__scope_` relation entry points
/// (`push_scope_variants`), so a scope chained on a relation VALUE
/// (lobsters' StoriesPaginator: `@scope.limit(n).for_presentation`
/// where `@scope` is an untyped ctor param) resolves without static
/// receiver-type knowledge. One def per scope NAME serves every model
/// sharing it, whatever their arities: SCOPE_UNSET sentinels detect
/// how many positionals the caller supplied, and each arm forwards
/// that exact shape. Positional-only arms dispatch through `klass` —
/// every model's `__scope_<name>__<n>` entry has the same full arity
/// by construction, which is what spinel's class-value dispatch
/// requires (it neither defaults omitted optionals through the
/// dispatch nor accepts kwargs). Keyword-carrying arms dispatch
/// through `case klass.name` constant receivers instead. An argument
/// shape no model accepts raises ArgumentError at the call, as Rails
/// would. Statically resolvable — explicit defs, no method_missing,
/// no splats. Emitted under app/models/ so the aggregator loads it.
/// None when the app declares no scopes. A name that can't render
/// (rest param, too many keywords, non-tail optional positionals,
/// `?`/`!` name) is skipped with a `lower_residue` diagnostic and a
/// mid-chain call raises NoMethodError.
pub(crate) fn emit_relation_scope_delegates(app: &App) -> Option<EmittedFile> {
    // Names Relation itself defines never delegate — a scope named
    // like a builtin would already dispatch to the builtin under
    // Rails' merge semantics, and the reopen must not clobber the
    // query-builder surface.
    const RELATION_BUILTINS: &[&str] = &[
        "where", "not", "or", "order", "limit", "offset", "group", "having", "joins",
        "left_outer_joins", "left_joins", "select", "distinct", "includes", "preload",
        "find", "find_by", "first", "last", "all", "each", "map", "to_a", "count",
        "exists?", "empty?", "any?", "none?", "sum", "maximum", "minimum", "pluck",
        "pick", "destroy_all", "delete_all", "update_all", "klass", "where_clauses",
        "spawn", "find_each", "find_in_batches",
    ];
    let scopes = crate::lower::scope_chain::build_scope_registry(&app.models);
    // name -> [(model, params)] in app-model order, names sorted — the
    // generated file must be byte-stable across runs.
    let mut by_name: std::collections::BTreeMap<
        String,
        Vec<(&crate::ident::ClassId, &[crate::dialect::Param])>,
    > = Default::default();
    for model in &app.models {
        let Some(per) = scopes.get(&model.name) else { continue };
        // The registry carries the SYNTHESIZED preload scopes too (it
        // must — the scope-body rewriter reads it to thread `__rel`
        // through a bare `with_attached_attachment`). They are handled
        // by the identity arm below, which is both faster and clearer
        // than a `__scope_` hop to a body that returns its argument;
        // leaving them here would make `by_name` claim them and the
        // identity arm skip them as "a declared scope of the same name".
        let plain_preload = if crate::lower::plain_text_attr::record_table_present(&app.schema) {
            crate::lower::plain_text_attr::preload_scope_names(model)
        } else {
            Vec::new()
        };
        let synthesized: crate::det::HashSet<String> =
            crate::lower::rich_text::preload_scope_names(model)
                .into_iter()
                .chain(plain_preload)
                .chain(crate::lower::attached::preload_scope_names(model))
                .map(|n| n.as_str().to_string())
                .collect();
        let mut names: Vec<&Symbol> = per.keys().collect();
        names.sort_by_key(|n| n.as_str());
        for n in names {
            if RELATION_BUILTINS.contains(&n.as_str()) || synthesized.contains(n.as_str()) {
                continue;
            }
            by_name
                .entry(n.as_str().to_string())
                .or_default()
                .push((&model.name, per[n].as_slice()));
        }
    }
    // A model subclass's own scopes (`lower::subclass_macros`), in name
    // order after the models'.
    let model_names: crate::det::HashSet<&crate::ident::ClassId> = app.models.iter().map(|m| &m.name).collect();
    let mut subclasses: Vec<&crate::ident::ClassId> = scopes.keys().filter(|k| !model_names.contains(k)).collect();
    subclasses.sort_by_key(|k| k.0.as_str());
    for sub in subclasses {
        let per = &scopes[sub];
        let mut names: Vec<&Symbol> = per.keys().collect();
        names.sort_by_key(|n| n.as_str());
        for n in names {
            if RELATION_BUILTINS.contains(&n.as_str()) {
                continue;
            }
            by_name.entry(n.as_str().to_string()).or_default().push((sub, per[n].as_slice()));
        }
    }
    // The SYNTHESIZED preload scopes — `with_attached_<attr>`,
    // `with_rich_text_<attr>`, `with_markdown_<attr>` — which Rails
    // declares beside the attachment / Action Text macros and this
    // compiler adds at emit time. They never pass through
    // `build_scope_registry`, which reads the app's own `scope`
    // declarations, so a call CHAINED ON A RELATION had no delegate at
    // all: campfire's
    // `find_autocompletable_users.with_attached_avatar.ordered` is a
    // NoMethodError on a class method that plainly exists, because the
    // receiver is a relation value and not the class.
    //
    // The delegate is `preload(:<assoc>)`, with no `__scope_` dispatch
    // behind it: there is no arity to detect and no model to pick,
    // because every model that declares the attachment preloads the
    // same named association (`<attr>_attachment`, `rich_text_<attr>`,
    // `markdown_<attr>`), and a model that does not never has the name
    // reached on it. It used to answer `self` — the scopes were
    // identity while the readers queried per record — and a delegate
    // that preloads is what lets the batch loader run when the scope is
    // reached mid-chain, not only from the class.
    let mut preloads: std::collections::BTreeMap<String, String> = Default::default();
    for model in &app.models {
        let plain_scopes = if crate::lower::plain_text_attr::record_table_present(&app.schema) {
            crate::lower::plain_text_attr::preload_scopes(model)
        } else {
            Vec::new()
        };
        let rich = crate::lower::rich_text::preload_scopes(model).into_iter().map(|(n, assoc)| {
            let spec = match crate::lower::rich_text::preload_scope_nested(&n) {
                Some(nested) => format!("{{ {}: :{} }}", assoc.as_str(), nested.as_str()),
                None => format!(":{}", assoc.as_str()),
            };
            (n, spec)
        });
        let scopes = rich.chain(
            plain_scopes
                .into_iter()
                .chain(crate::lower::attached::preload_scopes(model))
                .map(|(n, assoc)| (n, format!(":{}", assoc.as_str()))),
        );
        for (n, spec) in scopes {
            let n = n.as_str().to_string();
            // A declared scope of the same name wins — it has a real
            // body, and shadowing it would drop a filter.
            // A Relation builtin is never delegated, same rule as above.
            if by_name.contains_key(&n) || RELATION_BUILTINS.contains(&n.as_str()) {
                continue;
            }
            preloads.insert(n, spec);
        }
    }
    // Class methods some call site reaches THROUGH a relation, which
    // take that relation as a trailing `__rel` (see
    // `scope_chain::survey_assoc_class_methods`). The call-site rewrite
    // re-roots the chains it can recognize; a delegate here answers the
    // rest, whatever built the relation: campfire's
    // `Current.user.reachable_messages.search(q).last_page_of_matches(n)`
    // is a through-association, with no foreign key to seed from.
    let assocs = crate::lower::scope_chain::build_assoc_registry(&app.models);
    let (assoc_class_methods, _) =
        crate::lower::scope_chain::survey_assoc_class_methods(app, &assocs, &scopes);
    let mut class_methods: std::collections::BTreeMap<
        String,
        Vec<(&crate::ident::ClassId, Vec<crate::dialect::Param>)>,
    > = Default::default();
    for model in &app.models {
        let Some(per) = assoc_class_methods.get(&model.name) else { continue };
        let mut names: Vec<&Symbol> = per.keys().collect();
        names.sort_by_key(|n| n.as_str());
        for n in names {
            let key = n.as_str().to_string();
            if RELATION_BUILTINS.contains(&key.as_str())
                || by_name.contains_key(&key)
                || preloads.contains_key(&key)
            {
                continue;
            }
            class_methods.entry(key).or_default().push((&model.name, per[n].params.clone()));
        }
    }
    if by_name.is_empty() && preloads.is_empty() && class_methods.is_empty() {
        return None;
    }
    let mut skipped: Vec<String> = Vec::new();
    let mut body = String::new();
    for (name, decls) in &class_methods {
        match render_class_method_delegate(name, decls) {
            Ok(text) => body.push_str(&text),
            Err(reason) => {
                let borrowed: Vec<(&crate::ident::ClassId, &[crate::dialect::Param])> =
                    decls.iter().map(|(m, p)| (*m, p.as_slice())).collect();
                push_delegate_skip_diagnostic(name, &reason, &borrowed);
                skipped.push(name.clone());
            }
        }
    }
    for (name, decls) in &by_name {
        match render_scope_delegate(name, decls) {
            Ok(text) => body.push_str(&text),
            Err(reason) => {
                push_delegate_skip_diagnostic(name, &reason, decls);
                skipped.push(name.clone());
            }
        }
    }
    for (name, spec) in &preloads {
        body.push_str("\n    # Preload scope (Rails' `includes`): the association it names\n");
        body.push_str("    # joins this relation's preload specs, batched at `to_a`.\n");
        writeln!(body, "    def {name}").unwrap();
        writeln!(body, "      preload({spec})\n    end").unwrap();
    }
    let mut s = String::from(
        "# Generated Relation scope delegation (see\n\
         # emit_relation_scope_delegates): each model scope, callable on a\n\
         # relation value mid-chain, forwarding the caller's exact argument\n\
         # shape to the model's fixed-arity __scope_ entry — SCOPE_UNSET\n\
         # sentinels detect how many positionals were supplied.\n",
    );
    if !skipped.is_empty() {
        writeln!(
            s,
            "# No delegate (see the lower_residue diagnostics) — a mid-chain\n\
             # call raises NoMethodError: {}",
            skipped.join(", ")
        )
        .unwrap();
    }
    s.push_str("module ActiveRecord\n\x20\x20class Relation\n");
    s.push_str(
        "\x20\x20\x20\x20# Argument-omitted sentinel for the delegates below:\n\
         \x20\x20\x20\x20# distinguishes an omitted optional from every real value\n\
         \x20\x20\x20\x20# including nil. Compared with equal?, never ==.\n\
         \x20\x20\x20\x20SCOPE_UNSET = Object.new\n",
    );
    s.push_str(&body);
    s.push_str("  end\nend\n");
    Some(EmittedFile {
        path: PathBuf::from("app/models/relation_scopes.rb"),
        content: s,
    })
}

/// Everything the arm renderers need about one delegate name.
struct DelegateCtx<'a> {
    name: &'a Symbol,
    /// Display names for the delegate's positional params.
    pos_names: &'a [String],
    /// Every model declaring the name, with its admissible shape.
    shapes: &'a [(&'a crate::ident::ClassId, crate::lower::scope_chain::DelegableShape<'a>)],
    /// Max positional count across the shapes.
    n_max: usize,
}

/// One delegate def for `name` across every model declaring it, or the
/// reason it can't render (fed to the skip diagnostic).
/// One Relation delegate for a relation-taking class method: the
/// caller's positional arguments, then the relation itself as `__rel`.
/// Dispatched on `klass.name` with a constant receiver per model, so a
/// relation of a model without the method raises NoMethodError as Rails
/// would. Required positionals only; anything else is skipped and
/// reported.
fn render_class_method_delegate(
    name: &str,
    decls: &[(&crate::ident::ClassId, Vec<crate::dialect::Param>)],
) -> Result<String, String> {
    let arity = decls[0].1.len();
    for (model, params) in decls {
        if params.len() != arity {
            return Err(format!("arity differs across models ({})", model.0.as_str()));
        }
        if params.iter().any(|p| p.default.is_some() || p.keyword || p.rest || p.forwarding || p.name.as_str().is_empty()) {
            return Err(format!(
                "shape on {} is not required positionals only",
                model.0.as_str()
            ));
        }
    }
    let names: Vec<String> = decls[0].1.iter().map(|p| p.name.as_str().to_string()).collect();
    let sig = names.join(", ");
    let mut call_args = names.clone();
    call_args.push("self".to_string());
    let call = call_args.join(", ");
    let mut out = String::new();
    out.push_str("\n    # Class method run against this relation: the relation is its `__rel`.\n");
    if sig.is_empty() {
        writeln!(out, "    def {name}").unwrap();
    } else {
        writeln!(out, "    def {name}({sig})").unwrap();
    }
    out.push_str("      case klass.name\n");
    for (model, _) in decls {
        let m = model.0.as_str();
        writeln!(out, "      when \"{m}\" then {m}.{name}({call})").unwrap();
    }
    writeln!(
        out,
        "      else raise NoMethodError, \"undefined method '{name}' for a relation of #{{klass.name}}\""
    )
    .unwrap();
    out.push_str("      end\n    end\n");
    Ok(out)
}

fn render_scope_delegate(
    name: &str,
    decls: &[(&crate::ident::ClassId, &[crate::dialect::Param])],
) -> Result<String, String> {
    use crate::lower::scope_chain::{delegable_name, DelegableShape, MAX_DELEGATE_KEYWORDS};
    let name_sym = Symbol::from(name);
    if !delegable_name(&name_sym) {
        return Err("`?`/`!` names take no __scope_ arity mangling".into());
    }
    let mut shapes: Vec<(&crate::ident::ClassId, DelegableShape)> = Vec::new();
    for (model, params) in decls {
        let Some(shape) = DelegableShape::of(params) else {
            return Err(format!(
                "shape on {} has a rest param, more than {MAX_DELEGATE_KEYWORDS} keywords, \
                 or optional positionals not forming a tail",
                model.0.as_str()
            ));
        };
        shapes.push((*model, shape));
    }
    let n_max = shapes
        .iter()
        .map(|(_, s)| s.positionals.len())
        .max()
        .unwrap_or(0);
    // Keyword union across models, sorted by name — subset and variant
    // naming must line up with keyword_subsets/scope_variant_name.
    let mut kw_union: Vec<&crate::dialect::Param> = Vec::new();
    for (_, s) in &shapes {
        for kw in &s.keywords {
            if !kw_union.iter().any(|k| k.name == kw.name) {
                kw_union.push(kw);
            }
        }
    }
    kw_union.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
    if kw_union.len() > MAX_DELEGATE_KEYWORDS {
        return Err(format!(
            "keyword union across models exceeds {MAX_DELEGATE_KEYWORDS}"
        ));
    }
    if kw_union.iter().any(|k| k.name.as_str() == "klass") {
        return Err("a keyword named `klass` would shadow Relation#klass".into());
    }
    // Positional display names: the models' own param name where every
    // shape agrees at that position, `a<i>` otherwise (or when the name
    // would shadow Relation#klass in the forward).
    let pos_names: Vec<String> = (0..n_max)
        .map(|i| {
            let mut names = shapes
                .iter()
                .filter_map(|(_, s)| s.positionals.get(i).map(|p| p.name.as_str()));
            let n = match names.next() {
                Some(first) if names.all(|other| other == first) => first.to_string(),
                _ => format!("a{i}"),
            };
            if n == "klass" { format!("a{i}") } else { n }
        })
        .collect();
    let mut s = String::new();
    let mut decl: Vec<String> = pos_names
        .iter()
        .map(|n| format!("{n} = SCOPE_UNSET"))
        .collect();
    for kw in &kw_union {
        decl.push(format!("{}: SCOPE_UNSET", kw.name.as_str()));
    }
    if decl.is_empty() {
        writeln!(s, "\x20\x20\x20\x20def {name}").unwrap();
    } else {
        writeln!(s, "\x20\x20\x20\x20def {name}({})", decl.join(", ")).unwrap();
    }
    let ctx = DelegateCtx { name: &name_sym, pos_names: &pos_names, shapes: &shapes, n_max };
    render_kw_tree(&mut s, 3, &kw_union, &mut Vec::new(), &ctx);
    writeln!(s, "\x20\x20\x20\x20end").unwrap();
    Ok(s)
}

/// Branch on which of the delegate's keywords the caller supplied
/// (each independently sentinel-checked), then render the arity arms
/// for that exact keyword subset at the leaf.
fn render_kw_tree<'a>(
    out: &mut String,
    level: usize,
    remaining: &[&'a crate::dialect::Param],
    selected: &mut Vec<&'a crate::dialect::Param>,
    ctx: &DelegateCtx,
) {
    let pad = "\x20\x20".repeat(level);
    match remaining.split_first() {
        None => {
            let mut subset = selected.clone();
            subset.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
            render_arity_arms(out, level, &subset, ctx);
        }
        Some((kw, rest)) => {
            writeln!(out, "{pad}if SCOPE_UNSET.equal?({})", kw.name.as_str()).unwrap();
            render_kw_tree(out, level + 1, rest, selected, ctx);
            writeln!(out, "{pad}else").unwrap();
            selected.push(kw);
            render_kw_tree(out, level + 1, rest, selected, ctx);
            selected.pop();
            writeln!(out, "{pad}end").unwrap();
        }
    }
}

/// The supplied-arity arms for one keyword subset: arm `n` fires when
/// `a<n>` is the first unset positional (guards run in ascending order,
/// each returning or raising). Positional-only arms go through `klass`;
/// keyword arms need constant receivers (`case klass.name`) because
/// kwargs don't survive spinel's class-value dispatch.
fn render_arity_arms(
    out: &mut String,
    level: usize,
    subset: &[&crate::dialect::Param],
    ctx: &DelegateCtx,
) {
    use crate::lower::scope_chain::scope_variant_name;
    let pad = "\x20\x20".repeat(level);
    let kw_list = subset
        .iter()
        .map(|k| format!("{}:", k.name.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    for n in 0..=ctx.n_max {
        // Models whose shape accepts exactly this call: arity within
        // the positional range, subset within the declared keywords,
        // every required keyword supplied.
        let eligible: Vec<&crate::ident::ClassId> = ctx
            .shapes
            .iter()
            .filter(|(_, s)| {
                n >= s.min_required
                    && n <= s.positionals.len()
                    && subset
                        .iter()
                        .all(|kw| s.keywords.iter().any(|d| d.name == kw.name))
                    && s.required_keywords()
                        .iter()
                        .all(|r| subset.iter().any(|kw| &kw.name == *r))
            })
            .map(|(m, _)| *m)
            .collect();
        let guard = (n < ctx.n_max)
            .then(|| format!(" if SCOPE_UNSET.equal?({})", ctx.pos_names[n]))
            .unwrap_or_default();
        let pos_args: String = ctx.pos_names[..n]
            .iter()
            .map(|p| format!(", {p}"))
            .collect();
        if subset.is_empty() {
            let line = if eligible.is_empty() {
                format!(
                    "raise ArgumentError, \"scope {} does not accept {n} positional argument(s)\"",
                    ctx.name.as_str()
                )
            } else {
                format!(
                    "return klass.{}(self{pos_args})",
                    scope_variant_name(ctx.name, n, &[]).as_str()
                )
            };
            writeln!(out, "{pad}{line}{guard}").unwrap();
        } else if eligible.is_empty() {
            writeln!(
                out,
                "{pad}raise ArgumentError, \"scope {} does not accept ({kw_list}) with {n} \
                 positional argument(s)\"{guard}",
                ctx.name.as_str()
            )
            .unwrap();
        } else {
            let kw_args: String = subset
                .iter()
                .map(|kw| format!(", {0}: {0}", kw.name.as_str()))
                .collect();
            let variant = scope_variant_name(ctx.name, n, subset);
            let inner = if n < ctx.n_max {
                writeln!(out, "{pad}if SCOPE_UNSET.equal?({})", ctx.pos_names[n]).unwrap();
                level + 1
            } else {
                level
            };
            let ipad = "\x20\x20".repeat(inner);
            writeln!(out, "{ipad}case klass.name").unwrap();
            for m in &eligible {
                writeln!(
                    out,
                    "{ipad}when \"{0}\" then return {0}.{1}(self{pos_args}{kw_args})",
                    m.0.as_str(),
                    variant.as_str()
                )
                .unwrap();
            }
            writeln!(
                out,
                "{ipad}else raise ArgumentError, \"scope {} with ({kw_list}) unavailable on \
                 #{{klass.name}}\"",
                ctx.name.as_str()
            )
            .unwrap();
            writeln!(out, "{ipad}end").unwrap();
            if n < ctx.n_max {
                writeln!(out, "{pad}end").unwrap();
            }
        }
    }
}

/// Ledger a name the delegate emitter skipped: modeling debt, not an
/// app error — `lower_residue`, Warning severity, one per name.
fn push_delegate_skip_diagnostic(
    name: &str,
    reason: &str,
    decls: &[(&crate::ident::ClassId, &[crate::dialect::Param])],
) {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    let models = decls
        .iter()
        .map(|(m, _)| m.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let kind = DiagnosticKind::LowerResidue {
        pass: Symbol::from("relation_scope_delegates"),
        construct: Symbol::from(name),
        reason: Symbol::from(reason),
    };
    let d = Diagnostic {
        span: crate::span::Span::synthetic(),
        severity: Diagnostic::default_severity(&kind),
        kind,
        message: format!(
            "scope `{name}` (on {models}) gets no ActiveRecord::Relation mid-chain \
             delegate: {reason}; a mid-chain call raises NoMethodError"
        ),
    };
    crate::emit::diagnostics::push(d);
}

/// A class method a call site reaches through an association, whose
/// constructor cannot take the association's scope. Rails would have set
/// the foreign key here; we leave the call resolving against the folded
/// Array, which fails loudly, and say so.
fn push_assoc_scope_skip(model: &crate::ident::ClassId, method: &Symbol, reason: &str) {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    let model = model.0.as_str();
    let name = method.as_str();
    let kind = DiagnosticKind::LowerResidue {
        pass: Symbol::from("assoc_class_method_scope"),
        construct: Symbol::from(format!("{model}.{name}")),
        reason: Symbol::from(reason),
    };
    let d = Diagnostic {
        span: crate::span::Span::synthetic(),
        severity: Diagnostic::default_severity(&kind),
        kind,
        message: format!(
            "`{model}.{name}` is called through an association but cannot take its scope: \
             {reason}; the foreign key Rails would preset is not set"
        ),
    };
    crate::emit::diagnostics::push(d);
}

/// Give `m` the trailing `__rel = ActiveRecord::Relation.new(self)` that
/// makes it relation-taking — in the params AND in the signature the
/// `.rbs` is written from.
///
/// The signature half is not optional: a body the typer could resolve
/// carries a `Ty::Fn`, and the emitted `.rbs` prefers it over the
/// params, so inserting on one side only publishes `() -> bool` for a
/// method every call site now passes a relation to. Placement is before
/// the first keyword in both, since `def f(__rel = …, k:)` is the only
/// legal ordering.
///
/// A method with a REST param (`*tags`) is the exception: an optional
/// POSITIONAL can't follow a splat (`def f(*tags, __rel = …)` is a
/// syntax error — no slot for it to default INTO), so there `__rel`
/// becomes an optional KEYWORD instead (`def f(*tags, __rel: …)`,
/// legal after a splat), typed as the Relation rather than Untyped so
/// the `.rbs` reads `?__rel: ActiveRecord::Relation` instead of
/// `?__rel: untyped` — nothing else about the method declares
/// keywords, so Untyped would be the only typed thing losing its type.
/// It goes ahead of a `**opts` (`def f(*tags, __rel: …, **opts)`), the
/// only thing a keyword can't follow; a `&blk` lives in `block_param`,
/// not `params`, and is rendered last anyway. A method without a rest
/// param is untouched: same positional-with-default as always.
fn insert_rel_param(m: &mut crate::dialect::MethodDef, rel_param: &Symbol, owner: &ClassId) -> bool {
    if m.params.iter().any(|p| p.forwarding) {
        crate::emit::diagnostics::report_unsupported(m.name_span, "ruby", "full argument forwarding",
            "full forwarding cannot use the relation-threading argument ABI");
        return false;
    }
    let has_rest = m.params.iter().any(|p| p.rest && !p.keyword);
    if has_rest {
        let at = m.params.iter().position(|p| p.rest && p.keyword).unwrap_or(m.params.len());
        m.params.insert(
            at,
            crate::dialect::Param::keyword(
                rel_param.clone(),
                Some(crate::lower::model_to_library::relation_new_self()),
            ),
        );
        if let Some(Ty::Fn { params, .. }) = &mut m.signature {
            let at = params
                .iter()
                .position(|p| {
                    matches!(p.kind, crate::ty::ParamKind::KeywordRest | crate::ty::ParamKind::Block)
                })
                .unwrap_or(params.len());
            params.insert(at, crate::ty::Param {
                name: rel_param.clone(),
                ty: Ty::Relation { of: owner.clone() },
                kind: crate::ty::ParamKind::Keyword { required: false },
            });
        }
        return true;
    }
    let insert_at = m.params.iter().position(|p| p.keyword).unwrap_or(m.params.len());
    m.params.insert(
        insert_at,
        crate::dialect::Param::with_default(
            rel_param.clone(),
            crate::lower::model_to_library::relation_new_self(),
        ),
    );
    if let Some(Ty::Fn { params, .. }) = &mut m.signature {
        let at = params
            .iter()
            .position(|p| matches!(p.kind, crate::ty::ParamKind::Keyword { .. }))
            .unwrap_or(params.len());
        params.insert(
            at,
            crate::ty::Param {
                name: rel_param.clone(),
                ty: Ty::Untyped,
                kind: crate::ty::ParamKind::Optional,
            },
        );
    }
    true
}

/// Lower demanded model and association chains to Relations, including
/// scope-free apps; each body still has its own rewrite demand gate.
pub(crate) fn apply_scope_lowering(lcs: &mut [LibraryClass], app: &App) {
    // An `association(:x).target` the shared lowering collapsed onto the
    // reader is written back before anything here reads that reader as
    // an association read (`lower::assoc_loaded`).
    if super::expr::native_keywords() {
        for lc in lcs.iter_mut() {
            for m in &mut lc.methods {
                crate::lower::assoc_loaded::materialize_association_targets(&mut m.body);
            }
        }
    }
    // `has_rich_text` / `has_markdown` preload scopes, and
    // `has_one_attached`'s one. Ahead of the `any_scopes` early return
    // below, because an app can declare a rich-text / plain-text
    // attribute or an attachment and no `scope` at all — and these
    // still have to exist or every call site chaining through them is
    // a NoMethodError.
    // `attachable_sgid` for the models that mix in
    // `ActionText::Attachable` (campfire declares it one level down,
    // through `User::Mentionable`). Ruby-family only, like the
    // `MessageVerifier` the mint runs through.
    let _span = crate::timings::begin("scope lowering: preload");
    let attachable = crate::lower::attachable::attachable_models(app);
    // Per-class lookups by name, indexed once: a scan of `app.models`
    // (and, for the STI walk below, of `app.library_classes`) per class
    // made this pass quadratic in the class count on Core. First entry
    // wins, as the scans' `find` did.
    let mut model_named: HashMap<&ClassId, &crate::dialect::Model> = HashMap::default();
    for m in &app.models {
        model_named.entry(&m.name).or_insert(m);
    }
    let mut library_parent: HashMap<&ClassId, Option<&ClassId>> = HashMap::default();
    for other in &app.library_classes {
        library_parent.entry(&other.name).or_insert(other.parent.as_ref());
    }
    for lc in lcs.iter_mut() {
        if let Some(model) = model_named.get(&lc.name).copied() {
            crate::lower::rich_text::push_preload_scope_methods(&mut lc.methods, model);
            if crate::lower::plain_text_attr::record_table_present(&app.schema) {
                crate::lower::plain_text_attr::push_preload_scope_methods(&mut lc.methods, model);
            }
            crate::lower::attached::push_preload_scope_methods(&mut lc.methods, model);
            crate::lower::attachable::push_attachable_sgid(&mut lc.methods, model, &attachable);
            crate::lower::broadcasts::push_to_gid_param(&mut lc.methods, model);
        }
    }
    drop(_span);
    let _span = crate::timings::begin("scope lowering: registries");
    let scopes = crate::lower::scope_chain::build_scope_registry(&app.models);
    let assocs = crate::lower::scope_chain::build_assoc_registry(&app.models);
    // Class methods reached THROUGH an association (`user.sessions
    // .start!`) — demand-gated on the call sites this app actually
    // writes, so an app without that shape emits exactly what it did
    // before. Surveyed over the whole App, not over `lcs`: this pass
    // runs once for models and again for controllers, and the demand
    // for `Session.start!` lives in a CONTROLLER while the parameter
    // has to be inserted on the MODEL.
    let (assoc_class_methods, declined) =
        crate::lower::scope_chain::survey_assoc_class_methods(app, &assocs, &scopes);
    drop(_span);
    let _span = crate::timings::begin("scope lowering: ground");
    for lc in lcs.iter_mut() {
        for method in &mut lc.methods {
            crate::lower::scope_chain::ground_literal_model_dispatch(&mut method.body, app, &assocs);
        }
    }
    // Reported by the pass that owns the model's own file — this runs
    // once per emitted family over a different `lcs`, and the ledger
    // line should appear once, beside the class it is about.
    for d in &declined {
        if lcs.iter().any(|lc| lc.name == d.model) {
            push_assoc_scope_skip(&d.model, &d.method, &d.reason);
        }
    }
    drop(_span);
    let _span = crate::timings::begin("scope lowering: gates");
    let models = crate::lower::scope_chain::model_set(&app.models);
    // `BigAsset.order(:id)` on a plain model subclass seeds the same way.
    let plain_subclasses = crate::lower::sti_scope::plain_model_subclasses(app);
    // A model-root query needs the same Relation seed even when the app
    // declares no scopes. Otherwise `Widget.order(...)` reaches no method
    // and `Widget.where.not(...)` reaches Base.where with no argument.
    // Keep the whole-app gate consistent with the per-body gate below.
    let mut wants_model_chain = false;
    crate::lower::for_each_hook_body_ref(app, &mut |body| {
        wants_model_chain = wants_model_chain
            || crate::lower::scope_chain::mentions_model_chain_start(body, &models)
            || crate::lower::scope_chain::mentions_plain_subclass_chain_start(body, &plain_subclasses);
    });
    // …and so does a bare chain root in a plain subclass's own class
    // method (`def self.recent; order(:id); end` in `BigAsset < Asset`).
    wants_model_chain = wants_model_chain
        || lcs.iter().any(|lc| {
            plain_subclasses.contains_key(&lc.name)
                && lc.methods.iter().any(|m| {
                    m.receiver == MethodReceiver::Class
                        && crate::lower::scope_chain::mentions_bare_chain_start(&m.body)
                })
        });
    // …and an association read continuing into relation surface
    // (`@user.notifications.offset(n)`) needs the seed whether or not
    // the app declares a single scope: without this the whole pass was
    // skipped for a scope-free app, and every such chain stayed on the
    // has_many reader's Array.
    // An association CONSTRUCTOR (`@room.messages.create!(…)`) and a
    // parked association read chained later count too: they are the same
    // reader-to-relation hop, and a scope-free app that only CREATES
    // through its associations skipped the pass and called `create!` on
    // the reader's Array (found building tests/model_scenario_lowerings.rs,
    // whose app declares no scope; campfire declares several, which is
    // why it never showed there).
    // A tree whose has_many reader keeps what it loads also sends the
    // reads Rails answers without loading (`count`, `size`, …) to the
    // seeded Relation; see `mentions_assoc_unloaded_read`.
    let track_targets = super::expr::native_keywords();
    let mut wants_assoc_lookup = false;
    crate::lower::for_each_hook_body_ref(app, &mut |body| {
        wants_assoc_lookup = wants_assoc_lookup
            || crate::lower::scope_chain::mentions_assoc_lookup(body, &assocs)
            || crate::lower::scope_chain::mentions_assoc_constructor(body, &assocs)
            || crate::lower::scope_chain::mentions_assoc_alias(body, &assocs)
            || (track_targets && crate::lower::scope_chain::mentions_assoc_unloaded_read(body, &assocs));
    });
    if !crate::lower::scope_chain::any_scopes(&scopes)
        && assoc_class_methods.is_empty()
        && !wants_assoc_lookup
        // An app with no scopes at all can still declare an association
        // extension, and its call sites need the same rewrite.
        && !crate::lower::scope_chain::any_assoc_extensions(&assocs)
        && !wants_model_chain
    {
        return;
    }
    drop(_span);
    let _span = crate::timings::begin("scope lowering: setup");
    let names = crate::lower::scope_chain::all_scope_names(&scopes);
    let user_returns = crate::lower::scope_chain::build_user_method_returns(&app.models);
    let unique_keys = crate::lower::scope_chain::build_unique_keys(&app.models, &app.schema);
    let shadowed =
        crate::lower::scope_chain::shadowed_models(&app.models, app.library_classes.iter().map(|c| c.name.0.as_str()));
    let regs = crate::lower::scope_chain::Registries {
        scopes: &scopes,
        models: &models,
        assocs: &assocs,
        assoc_class_methods: &assoc_class_methods,
        user_returns: &user_returns,
        unique_keys: &unique_keys,
        track_targets,
        plain_subclasses: &plain_subclasses,
        shadowed: &shadowed,
    };
    drop(_span);
    let _span = crate::timings::begin("scope lowering: bodies");
    // An STI subclass's own class methods seed their implicit relation on
    // `self` as a plain subclass's do, its type condition applied at
    // runtime (`sti_scope::push_type_condition_methods`); scopes and
    // columns stay its base model's.
    crate::lower::sti_scope::unseat_implicit_roots(lcs, app);
    crate::lower::class_body_new::unbind_subclassed_model_new(lcs, app);
    let sti_subclasses = crate::lower::sti_scope::sti_bases(app);
    let mut self_seeded = plain_subclasses.clone();
    self_seeded.extend(sti_subclasses.iter().map(|(sub, base)| (sub.clone(), base.clone())));
    let sti_regs = crate::lower::scope_chain::Registries { plain_subclasses: &self_seeded, ..regs };
    let subclass_views = crate::lower::subclass_macros::views(app);
    for lc in lcs.iter_mut() {
        // Models gain their scope class methods (already chain-normalized).
        let is_model = model_named.contains_key(&lc.name);
        // A model CONCERN is a model body too — the third family
        // `lower::for_each_model_body` names. `module User::Bannable`'s
        // instance methods run on a `User`, so a bare `bans.create!` in
        // one is the same association read it would be in user.rb, and
        // resolving it needs the same owner. Without this the concern's
        // body reached `rewrite_call_site` with NO self model and the
        // reader's folded Array kept the call: `undefined method
        // 'create!' for an instance of Array`, which is what campfire's
        // `User::Bannable#create_bans_from_sessions` raised.
        //
        // MODULES ONLY, and only when the namespace names a model: a
        // plain nested class under a model namespace is its own
        // receiver, not the model's.
        let concern_owner: Option<crate::ident::ClassId> = (!is_model && lc.is_module)
            .then(|| lc.name.0.as_str().rsplit_once("::").map(|(ns, _)| ns.to_string()))
            .flatten()
            .map(|ns| crate::ident::ClassId(crate::ident::Symbol::from(ns.as_str())))
            .filter(|ns| model_named.contains_key(ns))
            // An STI SUBCLASS is the same fact by inheritance rather
            // than by namespace: `Rooms::Open < Room` declares no table,
            // so it is not a model, but its instance methods run on a
            // Room — and `memberships.grant_to(...)` in one is the same
            // association-extension call it would be in room.rb. Without
            // this it reached `rewrite_call_site` with no self model and
            // kept the un-flattened `memberships.grant_to`, which
            // nothing defines.
            .or_else(|| {
                if is_model {
                    return None;
                }
                let mut cursor = lc.parent.clone();
                for _ in 0..8 {
                    let parent = cursor?;
                    if model_named.contains_key(&parent) {
                        return Some(parent);
                    }
                    cursor = library_parent.get(&parent).copied().flatten().cloned();
                }
                None
            });
        if let Some(model) = model_named.get(&lc.name).copied() {
            crate::lower::model_to_library::push_scope_methods(
                &mut lc.methods,
                model,
                &scopes,
                &models,
                &assocs,
            );
            crate::lower::default_scope::push_methods(&mut lc.methods, model);
            crate::lower::sti_scope::push_inheritance_column_method(&mut lc.methods, model);
        } else if let Some(view) = subclass_views.get(&lc.name) {
            // A model subclass's own scopes (`lower::subclass_macros`).
            // An STI subclass's `Relation.new(self)` default carries its
            // type condition at runtime (`push_type_condition_methods`).
            crate::lower::model_to_library::push_scope_methods(&mut lc.methods, view, &scopes, &models, &assocs);
        }
        // User-written class methods the registry admitted as
        // relation-taking (`def self.arrange_for_user` with bare
        // `order(...)` roots) get the same treatment push_scope_methods
        // gives declared scopes: a trailing `__rel =
        // ActiveRecord::Relation.new(self)` param, and their bare chain
        // roots threaded through it. Skip-if-last-param-is-__rel makes
        // this idempotent AND distinguishes user methods from the
        // already-synthesized scope methods.
        if is_model {
            let rel_param = Symbol::from("__rel");
            let registered: Vec<Symbol> = scopes
                .get(&lc.name)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            for m in &mut lc.methods {
                if m.receiver == MethodReceiver::Class
                    && registered.contains(&m.name)
                    // Any-position check: push_scope_methods inserts
                    // __rel BEFORE keyword params, so a kwarg-taking
                    // scope's __rel is not last.
                    && !m.params.iter().any(|p| p.as_str() == "__rel")
                {
                    if !insert_rel_param(m, &rel_param, &lc.name) { continue; }
                    crate::lower::scope_chain::rewrite_scope_body(
                        &mut m.body,
                        &lc.name,
                        &rel_param,
                        &scopes,
                        &models,
                        &assocs,
                    );
                }
            }
            // The association-scope side of the same idea: a class
            // method some call site reaches through a has_many takes
            // the relation the same way, and reads it in one or both of
            // two ways. As a CREATE scope — `create!(attrs)` becomes
            // `create!(__rel.scope_attributes.merge(attrs))`, which is
            // how Rails' `scope_for_create` gets `user_id` onto the row.
            // And as a QUERY scope — a bare `count` / `page_before(m)`
            // roots on `__rel` exactly as it would inside a scope body,
            // which is what makes `@room.messages.paged?` count THIS
            // room. The default `Relation.new(self)` is empty and
            // records no scope attributes, so a direct `Session.start!`
            // / `Message.paged?` is unchanged.
            let assoc_registered: Vec<(Symbol, bool, bool)> = assoc_class_methods
                .get(&lc.name)
                .map(|m| m.iter().map(|(n, e)| (n.clone(), e.creates, e.queries)).collect())
                .unwrap_or_default();
            for m in &mut lc.methods {
                let Some(&(_, creates, queries)) = assoc_registered
                    .iter()
                    .find(|(n, _, _)| *n == m.name)
                else {
                    continue;
                };
                if m.receiver != MethodReceiver::Class
                    || m.params.iter().any(|p| p.as_str() == "__rel")
                {
                    continue;
                }
                if !insert_rel_param(m, &rel_param, &lc.name) { continue; }
                if creates {
                    crate::lower::scope_chain::merge_scope_attributes(
                        &mut m.body,
                        &lc.name,
                        &rel_param,
                    );
                }
                if queries {
                    crate::lower::scope_chain::rewrite_assoc_scope_body(
                        &mut m.body,
                        &lc.name,
                        &rel_param,
                        &scopes,
                        &models,
                        &assocs,
                    );
                }
            }
        }
        // Scope bodies and relation-taking class method bodies hand their
        // relation on to the class methods they call at implicit self.
        if is_model {
            let rel_param = Symbol::from("__rel");
            for m in &mut lc.methods {
                if m.receiver == MethodReceiver::Class
                    && m.params.iter().any(|p| p.as_str() == "__rel")
                {
                    crate::lower::scope_chain::thread_rel_into_class_method_calls(
                        &mut m.body,
                        &lc.name,
                        &rel_param,
                        &assoc_class_methods,
                    );
                }
            }
        }
        // Every method body: normalize scope chains (call-site form).
        // Scope-free bodies still need the rewrite when they start a
        // query chain on a model constant — the arel inline pass bails
        // on dynamic-value where-hashes, and those chains only run
        // against a seeded Relation. A model's own CLASS methods
        // additionally seed bare implicit-self roots (`where(key: key)`
        // in `Keystore.value_for`), signalled via `class_self`.
        for m in &mut lc.methods {
            // A `__scope_` entry's forward body already carries `__rel`
            // exactly once — never re-thread it (re-entrancy guard for
            // a second apply_scope_lowering over the same lcs).
            if m.name.as_str().starts_with("__scope_") {
                continue;
            }
            // A plain (non-STI) model subclass's own class methods root
            // on the subclass itself: `def self.recent; order(:id); end`
            // in `BigAsset < Asset` is `BigAsset.order(:id)`, rows
            // hydrated as BigAsset. scope_chain maps it to the parent
            // model for scopes and columns.
            let sti_own = sti_subclasses.contains_key(&lc.name) && !m.name_span.is_synthetic();
            let class_self = (m.receiver == MethodReceiver::Class)
                .then(|| {
                    if is_model || plain_subclasses.contains_key(&lc.name) || sti_own {
                        Some(lc.name.clone())
                    } else {
                        concern_owner.clone()
                    }
                })
                .flatten();
            // A model's own INSTANCE methods know their self model too —
            // `self.<has_many>.<scope>` there seeds a Relation from the
            // association's foreign key (recent_threads' comment chain).
            let instance_self = (m.receiver == MethodReceiver::Instance)
                .then(|| if is_model { Some(lc.name.clone()) } else { concern_owner.clone() })
                .flatten();
            let mentions = |e: &Expr| {
                crate::lower::scope_chain::mentions_scope(e, &names)
                    || crate::lower::scope_chain::mentions_model_chain_start(e, &models)
                    || crate::lower::scope_chain::mentions_plain_subclass_chain_start(e, &plain_subclasses)
                    || crate::lower::scope_chain::mentions_assoc_constructor(e, &assocs)
                    || crate::lower::scope_chain::mentions_assoc_lookup(e, &assocs)
                    || crate::lower::scope_chain::mentions_assoc_alias(e, &assocs)
                    || (track_targets && crate::lower::scope_chain::mentions_assoc_unloaded_read(e, &assocs))
                    || crate::lower::scope_chain::mentions_assoc_extension(e, &assocs)
                    || crate::lower::scope_chain::mentions_model_insert_all(e, &models)
                    || crate::lower::scope_chain::mentions_assoc_class_method(
                        e,
                        &assocs,
                        &names,
                        &assoc_class_methods,
                    )
                    || (class_self.is_some()
                        && crate::lower::scope_chain::mentions_bare_chain_start(e))
                    || (is_model
                        && class_self.is_some()
                        && super::expr::native_keywords()
                        && crate::lower::scope_chain::mentions_implicit_self_query(e, &lc.name))
            };
            let call_site_regs = if sti_own { &sti_regs } else { &regs };
            let model_self = is_model || plain_subclasses.contains_key(&lc.name) || sti_own;
            if mentions(&m.body) {
                crate::lower::scope_chain::rewrite_call_site_in(
                    &mut m.body,
                    call_site_regs,
                    class_self.as_ref(),
                    model_self,
                    instance_self.as_ref(),
                );
            }
            // An instance method's parameter DEFAULT is evaluated in the
            // method's own scope, so it is a call site too — campfire's
            // `def notification(badge: user.memberships.unread.count, …)`.
            // (A class method's default stays as written; see
            // tests/default_self_recv.rs.)
            let instance_defaults = m.receiver == MethodReceiver::Instance;
            for p in m.params.iter_mut().filter(|_| instance_defaults) {
                if let Some(d) = p.default.as_mut() {
                    if mentions(d) {
                        crate::lower::scope_chain::rewrite_call_site_in(
                            d,
                            call_site_regs,
                            class_self.as_ref(),
                            model_self,
                            instance_self.as_ref(),
                        );
                    }
                }
            }
        }
    }
    drop(_span);
    let _span = crate::timings::begin("scope lowering: variants");
    // Fixed-arity relation entry points, generated AFTER every body
    // rewrite above: their forward bodies mention scope names on model
    // constants, which `rewrite_call_site` would re-thread if it saw
    // them. Placement here (not inside push_scope_methods) is
    // load-bearing for the same reason.
    for lc in lcs.iter_mut() {
        if let Some(per_model) = scopes.get(&lc.name) {
            if !per_model.is_empty() {
                crate::lower::model_to_library::push_scope_variants(
                    &mut lc.methods,
                    &lc.name,
                    per_model,
                );
            }
        }
    }
}

/// Ruby-family pre-emit pass: a `belongs_to` reader MEMOIZES.
///
/// Rails loads a belongs_to target once and hands back the same object
/// on every later read. Ours re-queried, so two reads of
/// `membership.user` were two different objects — and campfire's
/// `@membership.user.expects :reset_remote_connections` stubbed one
/// while the `after_destroy_commit` callback called the other. The
/// callback fired correctly; the expectation was watching an object
/// nothing would ever call.
///
/// ```ruby
/// def user
///   return @user_cache if @user_loaded
///   @user_cache = (… the query …)
///   @user_loaded = true
///   @user_cache
/// end
///
/// def user_id=(value)
///   @user_id = value
///   @user_loaded = false        # <- the other half
/// end
/// ```
///
/// THE INVALIDATION IS NOT OPTIONAL. Rails resets the loaded target
/// when the foreign key is written directly, and a memoizing reader
/// without it answers `membership.user` from an object the key no
/// longer points at. `apply_belongs_to_autosave` above declined to
/// populate the cache for SAVED targets precisely because that reset
/// did not exist yet; with it here, the reader can cache every read and
/// the hazard that note describes is closed.
///
/// Ruby-family only, and for a reason the shared lowering states in
/// `synth_has_many_reader`: a reader that writes an ivar is no longer
/// read-only, so Rust would emit `&mut self` and every immutable caller
/// — views iterating a collection and reading each record's
/// association — would stop borrowing. The strict targets keep the
/// re-query, which is correct, just not identity-preserving.
///
/// The key's writer lives on the class with the column, which for a
/// belongs_to declared on an abstract base or an STI parent is the
/// subclass, so each model's writers are invalidated for its own
/// belongs_to and its model ancestors'. A polymorphic one's type
/// writer invalidates too, and the association's writer
/// (`record.author = other`) loads the assigned record as Rails'
/// `replace` does, so the reader does not answer the one it held
/// before. Where the app reads the
/// association object (`app_calls_association`), the writer also
/// records that a loaded target went stale and the key it was loaded
/// for, which the object's `loaded?` / `stale_target?` answer from as
/// Rails' do (runtime/spinel/active_record_associations.rb).
pub(crate) fn apply_belongs_to_memoization(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    /// (name, foreign key, type column for a polymorphic one)
    fn belongs_to(model: &crate::dialect::Model) -> Vec<(Symbol, Symbol, Option<Symbol>)> {
        model
            .associations()
            .filter_map(|a| match a {
                Association::BelongsTo { name, foreign_key, polymorphic, foreign_type, .. } => Some((
                    name.clone(),
                    foreign_key.clone(),
                    polymorphic.then(|| {
                        foreign_type.clone().unwrap_or_else(|| Symbol::from(format!("{}_type", name.as_str())))
                    }),
                )),
                _ => None,
            })
            .collect()
    }
    // The CRuby and JRuby trees, which run the native comparisons
    // (tests/errors_association_ports.rs), also memoize a polymorphic
    // belongs_to and invalidate an inherited key's writer; the other
    // trees keep the non-polymorphic memoization (and its writer's
    // load) and their own key writers.
    let native = super::expr::native_keywords();
    let stale_state = native && app_calls_association(app);
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        if native {
            synth_open_polymorphic_accessors(lc, model);
        }
        let mut own = belongs_to(model);
        if !native {
            own.retain(|(_, _, type_column)| type_column.is_none());
        }
        for (name, _, _) in &own {
            // The guard the preload pass puts on a reader when the app
            // runs `includes`; every reader memoizes behind it.
            ensure_cache_guard(lc, name);
            memoize_reader(lc, name);
            // Every tree: with the guard in place, a writer that left
            // the cache alone would leave the reader answering the
            // record it held before.
            load_on_assign(lc, name, stale_state);
            if stale_state {
                loaded_answers_stale(lc, name);
            }
        }
        let mut keys = own;
        let mut seen = vec![model.name.clone()];
        let mut parent = if native { model.parent.clone() } else { None };
        while let Some(p) = parent {
            if seen.contains(&p) {
                break;
            }
            let Some(pm) = app.models.iter().find(|m| m.name == p) else { break };
            keys.extend(belongs_to(pm));
            seen.push(p);
            parent = pm.parent.clone();
        }
        for (name, fk, type_column) in keys {
            invalidate_on_fk_write(lc, &name, &fk, stale_state);
            if let Some(type_column) = type_column {
                invalidate_on_fk_write(lc, &name, &type_column, stale_state);
            }
        }
    }
}

/// The synthesized `<name>_loaded?` (`association(:name).loaded?`,
/// collapsed by `lower::assoc_loaded`) answers as the association
/// object's does: a target a key write made stale is still loaded
/// (`@<name>_stale_loaded`) until the reader, a reset or a reload
/// replaces it. A hand-written predicate is left alone.
fn loaded_answers_stale(lc: &mut LibraryClass, name: &Symbol) {
    let predicate = Symbol::from(format!("{}_loaded?", name.as_str()));
    let loaded = Symbol::from(format!("{}_loaded", name.as_str()));
    let Some(m) = lc.methods.iter_mut().find(|m| {
        m.name == predicate && m.receiver == MethodReceiver::Instance && m.params.is_empty() && m.name_span.is_synthetic()
    }) else {
        return;
    };
    let src = format!("def __loaded\n  @{n}_loaded == true || @{n}_stale_loaded == true\nend\n", n = name.as_str());
    let answer = crate::runtime_src::parse_methods(&src)
        .unwrap_or_else(|e| panic!("loaded_answers_stale: generated source failed to parse: {e}\n{src}"))
        .pop()
        .expect("one method")
        .body;
    let last = match &mut *m.body.node {
        ExprNode::Seq { exprs } => exprs.last_mut(),
        _ => Some(&mut m.body),
    };
    if let Some(last) = last.filter(|e| matches!(&*e.node, ExprNode::Ivar { name } if name == &loaded)) {
        let ty = last.ty.clone();
        *last = answer;
        last.ty = ty.or(Some(crate::ty::Ty::Bool));
    }
}

/// A synthesized association reader opens with `return @<name>_cache
/// if @<name>_loaded` (`preload_cache_guard`): a has_many / has_one
/// reader always does, a belongs_to one once `apply_preload_lowering`
/// ran. One without it gets it here, so a target the writer holds (an
/// unsaved record, or any on the CRuby and JRuby trees) answers the
/// read, as Rails' does. A hand-written reader is left alone.
fn ensure_cache_guard(lc: &mut LibraryClass, name: &Symbol) {
    let loaded = Symbol::from(format!("{}_loaded", name.as_str()));
    let Some(m) = lc.methods.iter_mut().find(|m| {
        &m.name == name && m.receiver == MethodReceiver::Instance && m.params.is_empty() && m.name_span.is_synthetic()
    }) else {
        return;
    };
    let guarded = matches!(&*m.body.node, ExprNode::Seq { exprs }
        if exprs.first().is_some_and(|g| matches!(&*g.node,
            ExprNode::If { cond, .. } if matches!(&*cond.node, ExprNode::Ivar { name } if name == &loaded))));
    if guarded {
        return;
    }
    let old = std::mem::replace(&mut m.body, Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }));
    m.body = Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![preload_cache_guard(name), old] });
}

/// Ruby-family pre-emit pass: `record.reload` drops every association
/// target the record holds, as Rails' (`@association_cache = {}`): the
/// model's `_adapter_reload`, which re-reads the row, also clears each
/// `@<name>_loaded`, so the next read of an association loads it again
/// for the reloaded keys. The associations are the model's and its
/// model ancestors' (an abstract base's or an STI parent's), whose
/// readers it inherits.
pub(crate) fn apply_reload_resets_associations(lcs: &mut [LibraryClass], app: &App) {
    // The belongs_to stale record `invalidate_on_fk_write` keeps.
    let stale_state = super::expr::native_keywords() && app_calls_association(app);
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        let mut names: Vec<Symbol> = Vec::new();
        let mut stale: Vec<Symbol> = Vec::new();
        let mut seen: Vec<ClassId> = Vec::new();
        let mut cursor = Some(model);
        while let Some(m) = cursor {
            if seen.contains(&m.name) {
                break;
            }
            seen.push(m.name.clone());
            for a in m.associations() {
                if !names.contains(a.name()) {
                    names.push(a.name().clone());
                    if stale_state && matches!(a, crate::dialect::Association::BelongsTo { .. }) {
                        stale.push(a.name().clone());
                    }
                }
            }
            cursor = m.parent.as_ref().and_then(|p| app.models.iter().find(|pm| &pm.name == p));
        }
        let Some(reload) = lc
            .methods
            .iter_mut()
            .find(|m| m.name.as_str() == "_adapter_reload" && m.receiver == MethodReceiver::Instance)
        else {
            continue;
        };
        if names.is_empty() {
            continue;
        }
        let span = reload.body.span;
        let mut stmts = match &*reload.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![reload.body.clone()],
        };
        // Before the method's value (its last statement).
        let value = stmts.pop();
        let flags = names
            .iter()
            .map(|n| format!("{}_loaded", n.as_str()))
            .chain(stale.iter().map(|n| format!("{}_stale_loaded", n.as_str())));
        for flag in flags {
            stmts.push(Expr::new(
                span,
                ExprNode::Assign {
                    target: LValue::Ivar { name: Symbol::from(flag) },
                    value: Expr::new(span, ExprNode::Lit { value: Literal::Bool { value: false } }),
                },
            ));
        }
        stmts.extend(value);
        reload.body = Expr::new(span, ExprNode::Seq { exprs: stmts });
        reload.mutates_self = true;
    }
}

/// `def <name>=(value) … end` ends with `@<name>_cache = value;
/// @<name>_loaded = true`: Rails' writer replaces the target, so the
/// memoized reader answers the assigned record, not one loaded before.
/// With `stale_state`, the target is fresh (`@<name>_stale_loaded =
/// false`).
fn load_on_assign(lc: &mut LibraryClass, name: &Symbol, stale_state: bool) {
    let writer = Symbol::from(format!("{}=", name.as_str()));
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| m.name == writer && m.receiver == MethodReceiver::Instance)
    else {
        return;
    };
    let [param] = m.params.as_slice() else { return };
    if param.name.as_str().is_empty() || param.default.is_some() {
        return;
    }
    let n = name.as_str();
    let mut src = format!("def __assign({v})\n  @{n}_cache = {v}\n  @{n}_loaded = true\n", v = param.name.as_str());
    if stale_state {
        src.push_str(&format!("  @{n}_stale_loaded = false\n"));
    }
    src.push_str("end\n");
    let assign = crate::runtime_src::parse_methods(&src)
        .unwrap_or_else(|e| panic!("load_on_assign: generated source failed to parse: {e}\n{src}"))
        .pop()
        .expect("one method");
    let span = m.body.span;
    let mut stmts = match &*m.body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![m.body.clone()],
    };
    match &*assign.body.node {
        ExprNode::Seq { exprs } => stmts.extend(exprs.iter().cloned()),
        _ => stmts.push(assign.body.clone()),
    }
    m.body = Expr::new(span, ExprNode::Seq { exprs: stmts });
}

/// Ruby-family pre-emit pass: a has_many or has_one reader that runs its
/// query keeps the records, as Rails' association does.
///
/// Rails loads an association's target the first time it is read
/// (`record.cover`, `record.comments.each`) and answers from it after:
/// `association(:comments).loaded?` is then true, `target` the records,
/// and `record.comments` the same records until a reset or reload. The
/// emitted readers re-ran the query on every read and left `loaded?`
/// false, so Core's `association(:x).loaded?` guards and its
/// `record.x.loaded?` reads (`lower::assoc_loaded`) answered false where
/// Rails answers true. The reader now finishes as the belongs_to one
/// does (`apply_belongs_to_memoization`):
///
/// ```ruby
/// def comments
///   return @comments_cache if @comments_loaded
///   … the query …
///   @comments_cache = results
///   @comments_loaded = true
///   @comments_cache
/// end
/// ```
///
/// The reads Rails answers without loading (`first`, `count`, `size`,
/// `empty?`, a chained `where`) do not reach the reader: the scope-chain
/// lowering runs them on a Relation seeded from the cache. A record
/// created through the association (`record.comments.create!`) joins a
/// loaded cache (`ActiveRecord::Associations::Association.created`), as
/// Rails adds it to the target.
///
/// Only a reader that returns its records as an Array (a `:through`
/// reader answers a Relation, which memoizes its own records), only on
/// the CRuby and JRuby trees. Rails' reader answers the target it
/// loaded until `reload` / `reset`, so a record's in-memory changes
/// (what `validates_associated` and autosave read) survive the next
/// read.
pub(crate) fn apply_collection_memoization(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    if !super::expr::native_keywords() {
        return;
    }
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        // (name, :through or HABTM, singular, the unscoped has_many's
        // relation source)
        let names: Vec<(Symbol, bool, bool, Option<String>)> = model
            .associations()
            .filter_map(|a| match a {
                Association::HasMany { name, target, foreign_key, as_interface, scope, through: None, .. } => {
                    let plain = scope.is_none().then(|| {
                        let mut conditions = format!("{}: @id", foreign_key.as_str());
                        if let Some(intf) = as_interface {
                            conditions.push_str(&format!(", {}_type: {:?}", intf.as_str(), model.name.0.as_str()));
                        }
                        format!("ActiveRecord::Relation.new(::{}).where({conditions})", target.0.as_str())
                    });
                    Some((name.clone(), false, false, plain))
                }
                Association::HasOne { name, .. } => Some((name.clone(), false, true, None)),
                Association::HasMany { name, through: Some(_), .. }
                | Association::HasAndBelongsToMany { name, .. } => Some((name.clone(), true, false, None)),
                Association::BelongsTo { .. } => None,
            })
            .collect();
        for (name, through, singular, plain) in names {
            // A reader that answers a Relation (a `:through` one, or one
            // whose scope keeps it chainable) is tracked; one that
            // answers its records memoizes them.
            let relation = !singular && lc.methods.iter().any(|m| {
                m.name == name
                    && m.receiver == MethodReceiver::Instance
                    && matches!(m.signature.as_ref(), Some(Ty::Fn { ret, .. })
                        if matches!(&**ret, Ty::Class { id, .. } if id.0.as_str().ends_with("Relation"))
                            || matches!(&**ret, Ty::Relation { .. }))
            });
            if through || relation {
                track_through_reader(lc, &name);
            } else {
                memoize_query_reader(lc, &name, singular, plain.as_deref());
            }
        }
    }
}

/// A `:through` (or HABTM) reader, or any collection reader that answers
/// a Relation, which memoizes
/// its own records; its value is handed to `ActiveRecord::Associations::
/// Association.through(self, :<name>, …)`, which records what the
/// reader's own query loads as the association's target, as Rails'
/// CollectionProxy does.
fn track_through_reader(lc: &mut LibraryClass, name: &Symbol) {
    fn returns(e: &Expr) -> bool {
        let mut found = matches!(&*e.node, ExprNode::Return { .. });
        if !found {
            e.node.for_each_child(&mut |c| found = found || returns(c));
        }
        found
    }
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| &m.name == name && m.receiver == MethodReceiver::Instance && m.params.is_empty())
    else {
        return;
    };
    if returns(&m.body) {
        return;
    }
    let span = m.body.span;
    let wrap = |value: Expr| {
        let sp = value.span;
        let ty = value.ty.clone();
        let mut call = Expr::new(
            sp,
            ExprNode::Send {
                recv: Some(Expr::new(
                    sp,
                    ExprNode::Const {
                        path: vec![
                            Symbol::from("ActiveRecord"),
                            Symbol::from("Associations"),
                            Symbol::from("Association"),
                        ],
                    },
                )),
                method: Symbol::from("through"),
                args: vec![
                    Expr::new(sp, ExprNode::SelfRef),
                    Expr::new(sp, ExprNode::Lit { value: Literal::Sym { value: name.clone() } }),
                    value,
                ],
                block: None,
                parenthesized: true,
            },
        );
        call.ty = ty;
        call
    };
    match &mut *m.body.node {
        ExprNode::Seq { exprs } if !exprs.is_empty() => {
            let value = exprs.pop().expect("non-empty");
            exprs.push(wrap(value));
        }
        _ => {
            let body = std::mem::replace(&mut m.body, Expr::new(span, ExprNode::Lit { value: Literal::Nil }));
            m.body = wrap(body);
        }
    }
}

/// `Seq[guard, stmts…, value]` → `Seq[guard, stmts…, @cache = value,
/// @loaded = true, @cache]`, for the synthesized has_many/has_one reader
/// shape: the loaded guard first, no `return` after it, and an Array or
/// record value. Anything else is left alone.
fn memoize_query_reader(lc: &mut LibraryClass, name: &Symbol, singular: bool, plain: Option<&str>) {
    fn returns(e: &Expr) -> bool {
        let mut found = matches!(&*e.node, ExprNode::Return { .. });
        if !found {
            e.node.for_each_child(&mut |c| found = found || returns(c));
        }
        found
    }
    let cache = Symbol::from(format!("{}_cache", name.as_str()));
    let loaded = Symbol::from(format!("{}_loaded", name.as_str()));
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| &m.name == name && m.receiver == MethodReceiver::Instance)
    else {
        return;
    };
    if let Some(Ty::Fn { ret, .. }) = m.signature.as_ref() {
        if matches!(&**ret, Ty::Class { id, .. } if id.0.as_str().ends_with("Relation")) {
            return;
        }
    }
    let ExprNode::Seq { exprs } = &mut *m.body.node else { return };
    if exprs.len() < 2 {
        return;
    }
    let guard_matches = matches!(&*exprs[0].node,
        ExprNode::If { cond, .. } if matches!(&*cond.node, ExprNode::Ivar { name } if name == &loaded));
    if !guard_matches || exprs[1..].iter().any(returns) {
        return;
    }
    let query = exprs.pop().expect("len checked");
    let span = query.span;
    let syn = |n| Expr::new(span, n);
    // An unsaved owner's association loads nothing (Rails' `null_scope?`
    // / `find_target?`): the query would match the rows whose key is
    // NULL.
    let none = if singular {
        syn(ExprNode::Lit { value: Literal::Nil })
    } else {
        syn(ExprNode::Array { elements: vec![], style: crate::expr::ArrayStyle::Brackets })
    };
    let new_record = syn(ExprNode::Send {
        recv: None,
        method: Symbol::from("new_record?"),
        args: vec![],
        block: None,
        parenthesized: false,
    });
    // The reader's relation, before it loads: the collection proxy's
    // scope (`_<name>_association_scope`), for the creates and queries
    // it answers. A query that is a relation loaded with `to_a` and
    // reads no local of the reader is it; an unscoped has_many's
    // compiled query is its key (and `as:` type) on the target.
    // A has_one's is the same without its `.first`, and a scoped
    // association's is its query (a chain of relation methods that
    // reads no local of the reader), as Rails' `association.scope`
    // answers the relation before it loads.
    let scope = match &*query.node {
        ExprNode::Send { recv: Some(rel), method, args, block: None, .. }
            if args.is_empty()
                && method.as_str() == if singular { "first" } else { "to_a" }
                && !reads_local(rel) =>
        {
            Some(rel.clone())
        }
        _ if !singular && is_relation_chain(&query) && !reads_local(&query) => Some(query.clone()),
        _ => (!singular)
            .then(|| {
                plain.map(|src| {
                    let src = format!("def __scope\n  {src}\nend\n");
                    crate::runtime_src::parse_methods(&src)
                        .unwrap_or_else(|e| panic!("memoize_query_reader: generated source failed to parse: {e}\n{src}"))
                        .pop()
                        .expect("one method")
                        .body
                })
            })
            .flatten(),
    };
    let value = syn(ExprNode::If { cond: new_record, then_branch: none, else_branch: query });
    exprs.push(syn(ExprNode::Assign { target: LValue::Ivar { name: cache.clone() }, value }));
    exprs.push(syn(ExprNode::Assign {
        target: LValue::Ivar { name: loaded },
        value: syn(ExprNode::Lit { value: Literal::Bool { value: true } }),
    }));
    if singular {
        exprs.push(syn(ExprNode::Ivar { name: cache }));
        if let Some(rel) = scope {
            push_association_scope(lc, name, rel);
        }
        return;
    }
    // A collection reader answers Rails' CollectionProxy over its
    // records (runtime/spinel/active_record_associations.rb), loaded
    // here or preloaded: what a call the scope-chain lowering cannot
    // see (`record.send(:items).create(…)`, `items.first!`) reaches.
    let wrap = |records: Expr| {
        let sp = records.span;
        syn(ExprNode::Send {
            recv: Some(Expr::new(
                sp,
                ExprNode::Const {
                    path: vec![
                        Symbol::from("ActiveRecord"),
                        Symbol::from("Associations"),
                        Symbol::from("CollectionProxy"),
                    ],
                },
            )),
            method: Symbol::from("wrap"),
            args: vec![
                syn(ExprNode::SelfRef),
                syn(ExprNode::Lit { value: Literal::Sym { value: name.clone() } }),
                records,
            ],
            block: None,
            parenthesized: true,
        })
    };
    if let ExprNode::If { then_branch, .. } = &mut *exprs[0].node {
        if let ExprNode::Return { value } = &mut *then_branch.node {
            let records = std::mem::replace(value, syn(ExprNode::Lit { value: Literal::Nil }));
            *value = wrap(records);
        }
    }
    exprs.push(wrap(syn(ExprNode::Ivar { name: cache })));
    if let Some(rel) = scope {
        push_association_scope(lc, name, rel);
    }
}

/// `_<name>_association_scope`, beside the reader: the association's
/// query before it loads, for the collection proxy and
/// `association(:<name>).scope`.
fn push_association_scope(lc: &mut LibraryClass, name: &Symbol, rel: Expr) {
    let scope_name = Symbol::from(format!("_{}_association_scope", name.as_str()));
    if !lc.methods.iter().any(|m| m.name == scope_name && m.receiver == MethodReceiver::Instance) {
        let mut scope_method = lc
            .methods
            .iter()
            .find(|m| &m.name == name && m.receiver == MethodReceiver::Instance)
            .expect("the reader")
            .clone();
        scope_method.name = scope_name;
        scope_method.signature = None;
        scope_method.body = rel;
        lc.methods.push(scope_method);
    }
}

/// A chain of relation-building calls on a constant (`Model.where(…)
/// .order(…)`, `ActiveRecord::Relation.new(Model).where(…)`): it
/// answers a relation, not records.
fn is_relation_chain(e: &Expr) -> bool {
    const BUILDERS: [&str; 20] = [
        "where", "order", "reorder", "joins", "left_joins", "left_outer_joins", "includes", "preload",
        "eager_load", "limit", "offset", "group", "having", "distinct", "unscope", "merge", "references",
        "readonly", "rewhere", "extending",
    ];
    match &*e.node {
        ExprNode::Send { recv: Some(inner), method, .. } if BUILDERS.contains(&method.as_str()) => {
            is_relation_chain(inner)
        }
        ExprNode::Send { recv: Some(inner), method, .. } if method.as_str() == "new" => {
            matches!(&*inner.node, ExprNode::Const { path } if path.last().is_some_and(|s| s.as_str() == "Relation"))
        }
        ExprNode::Const { .. } => true,
        _ => false,
    }
}

/// Does `e` read a local variable (or a block parameter)?
fn reads_local(e: &Expr) -> bool {
    let mut found = matches!(&*e.node, ExprNode::Var { .. } | ExprNode::Lambda { .. });
    if !found {
        e.node.for_each_child(&mut |c| found = found || reads_local(c));
    }
    found
}

/// Ruby-family pre-emit pass: each model answers `association(name)`.
///
/// Rails' `record.association(:line_items)` is the association object,
/// which Core asks whether the records are in memory (`.loaded?`) and
/// reads, seeds or drops them through (`.target`, `.target =`, `.reset`,
/// `.reload`). The object is ported in
/// runtime/spinel/active_record_associations.rb as a view over the two
/// ivars every association already keeps (`@<name>_cache`,
/// `@<name>_loaded`); this pass gives each model the dispatch from a
/// name to the right kind:
///
/// ```ruby
/// def association(name)
///   case name.to_s
///   when "comments" then ActiveRecord::Associations::HasManyAssociation.new(self, :comments, "Comment", :post_id, true)
///   when "author" then ActiveRecord::Associations::BelongsToAssociation.new(self, :author, "Author", :author_id, true, :id)
///   else raise ActiveRecord::AssociationNotFoundError.new(self, name)
///   end
/// end
/// ```
///
/// Each arm passes the target class's name, the key and whether `scope`
/// can be built from them (`klass`, `scope`). The model's associations
/// include those spliced from its concerns; a name the model does not
/// declare raises AssociationNotFoundError, as Rails does, except on a
/// subclass of another model (an abstract base's or an STI parent's),
/// which hands it to its parent's dispatch (`super(name)`). An
/// association whose reader the model does not carry raises
/// NotImplementedError by name rather than answering wrong, and the
/// analyzer refuses a literal name no model in the family has
/// (`analyze::association_surface`). Every model gets the method, so a model with
/// no associations answers the error too, and every tree that calls
/// `association` carries the port. `name.to_s` because Rails'
/// `_reflect_on_association` takes a String or a Symbol.
///
/// CRuby and JRuby only (the port reads the ivars by computed name), and
/// only where some body calls `association` with one argument, so an app
/// that never asks emits nothing new. Spinel refuses the call at the
/// analyzer's gate (`analyze::runtime_reads`).
pub(crate) fn apply_association_reader(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    apply_association_reflections(lcs, app);
    if !super::expr::native_keywords() || !app_calls_association(app) {
        return;
    }
    let mut declarations = DeclarationOptions::default();
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        if lc.methods.iter().any(|m| m.name.as_str() == "association" && m.receiver == MethodReceiver::Instance) {
            continue; // the model's own wins
        }
        let mut arms = String::new();
        for (span, assoc) in model.spanned_associations() {
            let name = assoc.name();
            // The object runs the model's reader for its query; an
            // association whose reader the model does not carry is
            // refused by name rather than answered wrong.
            if !lc.methods.iter().any(|m| &m.name == name && m.receiver == MethodReceiver::Instance) {
                let _ = writeln!(
                    arms,
                    "  when {:?} then raise NotImplementedError, {:?}",
                    name.as_str(),
                    format!(
                        "roundhouse: {}#association(:{}): the association has no emitted reader",
                        model.name.0.as_str(),
                        name.as_str()
                    )
                );
                continue;
            }
            let key = |k: &Symbol| format!(":{}", k.as_str());
            let new = match assoc {
                Association::BelongsTo {
                    target, foreign_key, polymorphic: false, primary_key, ..
                } => format!(
                    "BelongsToAssociation.new(self, :{}, {:?}, {}, true, {}, {})",
                    name.as_str(),
                    target.0.as_str(),
                    key(foreign_key),
                    primary_key.as_ref().map_or_else(|| ":id".to_string(), key),
                    belongs_to_inverse(app, model, assoc, &declarations.of(app, span).0)
                        .map_or_else(|| "nil".to_string(), |i| format!(":{}", i.as_str()))
                ),
                Association::BelongsTo {
                    foreign_key, polymorphic: true, foreign_type, primary_key, ..
                } => format!(
                    "BelongsToPolymorphicAssociation.new(self, :{}, {}, {}, {})",
                    name.as_str(),
                    key(foreign_key),
                    foreign_type
                        .as_ref()
                        .map_or_else(|| format!(":{}_type", name.as_str()), key),
                    primary_key.as_ref().map_or_else(|| ":id".to_string(), key)
                ),
                Association::HasOne { target, foreign_key, as_interface, scope, .. } => format!(
                    "HasOneAssociation.new(self, :{}, {:?}, {}, {})",
                    name.as_str(),
                    target.0.as_str(),
                    key(foreign_key),
                    as_interface.is_none() && scope.is_none()
                ),
                Association::HasMany { target, foreign_key, through: None, as_interface, scope, .. } => {
                    format!(
                        "HasManyAssociation.new(self, :{}, {:?}, {}, {})",
                        name.as_str(),
                        target.0.as_str(),
                        key(foreign_key),
                        as_interface.is_none() && scope.is_none()
                    )
                }
                Association::HasMany { target, through: Some(_), .. }
                // Rails builds a HABTM as a has_many :through its join model.
                | Association::HasAndBelongsToMany { target, .. } => format!(
                    "HasManyThroughAssociation.new(self, :{}, {:?})",
                    name.as_str(),
                    target.0.as_str()
                ),
            };
            let _ = writeln!(
                arms,
                "  when {:?} then ActiveRecord::Associations::{new}",
                name.as_str()
            );
        }
        // A subclass of another model (STI) hands the names it does not
        // declare to its parent's dispatch; a root model raises.
        let inherits = model.parent.as_ref().is_some_and(|p| app.models.iter().any(|m| &m.name == p));
        let otherwise = if inherits {
            "super(name)"
        } else {
            "raise ActiveRecord::AssociationNotFoundError.new(self, name)"
        };
        let mut src = if arms.is_empty() {
            format!("def association(name)\n  {otherwise}\nend\n")
        } else {
            format!("def association(name)\n  case name.to_s\n{arms}  else {otherwise}\n  end\nend\n")
        };
        // Rails' `association_cached?(name)` (`@association_cache.key?`):
        // whether the record holds the association's state. Here that
        // state is the association's two ivars, held once a read, a
        // preload or a writer loaded it; a name the model does not
        // have answers false, as Rails' does.
        if !lc.methods.iter().any(|m| m.name.as_str() == "association_cached?" && m.receiver == MethodReceiver::Instance) {
            src.push_str("def association_cached?(name)\n  instance_variable_get(:\"@#{name}_loaded\") == true\nend\n");
        }
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("apply_association_reader: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }
    }
}

/// Ruby-family pre-emit pass: the singular association's builders.
///
/// Rails defines, per `has_one` and monomorphic `belongs_to`,
/// `build_<name>(attributes = {})`, `create_<name>`, `create_<name>!`,
/// `reload_<name>` and `reset_<name>` (associations/builder/
/// singular_association.rb `define_constructors`, `define_accessors`).
/// The call-site rewrite (`lower::has_one_builder`) covers a literal
/// keyword-hash call on a typed receiver; any other shape (a hash held in
/// a variable, `attrs.merge(...)`, a call from a method of another class,
/// a `belongs_to`) reaches the record itself, where the method was
/// missing. Each method runs the association object
/// (runtime/spinel/active_record_associations.rb), so a built record
/// replaces the target, carries the owner's key and inverse, and a
/// `create` saves it.
///
/// Only for the names the app calls, so a model's unused associations
/// carry no extra methods; a model's own definition wins. CRuby and
/// JRuby only: the object reads the record's ivars by computed name.
pub(crate) fn apply_singular_association_builders(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::{Association, Dependent};

    if !super::expr::native_keywords() {
        return;
    }
    let mut called: BTreeSet<String> = BTreeSet::new();
    fn walk(e: &Expr, called: &mut BTreeSet<String>) {
        // `lower::has_one_builder` made a constructor of the call; the
        // emit writes the authored call back (`IrHint::SingularBuilder`).
        if let Some(crate::expr::IrHint::SingularBuilder { method, .. }) = &e.hint {
            called.insert(method.as_str().to_string());
        }
        if let ExprNode::Send { method, .. } = &*e.node {
            let m = method.as_str();
            if ["build_", "create_", "reload_", "reset_"].iter().any(|p| m.starts_with(p)) {
                called.insert(m.to_string());
            }
        }
        // `accepts_nested_attributes_for :x` builds a new target through
        // `build_x` (nested_attributes.rb, one-to-one assignment).
        if let ExprNode::Send { method, args, .. } = &*e.node
            && method.as_str() == "accepts_nested_attributes_for"
        {
            for arg in args {
                if let ExprNode::Lit { value: Literal::Sym { value } } = &*arg.node {
                    called.insert(format!("build_{}", value.as_str()));
                }
            }
        }
        e.node.for_each_child(&mut |c| walk(c, called));
    }
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, &mut called));
    for view in &app.views {
        walk(&view.body, &mut called);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |root| walk(root, &mut called));
    if called.is_empty() {
        return;
    }
    let mut declarations = DeclarationOptions::default();
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        let mut src = String::new();
        for (span, assoc) in model.spanned_associations() {
            let name = assoc.name().as_str();
            let key = |k: &Symbol| format!(":{}", k.as_str());
            let object = match assoc {
                Association::BelongsTo { target, foreign_key, polymorphic: false, primary_key, .. } => {
                    format!(
                        "ActiveRecord::Associations::BelongsToAssociation.new(self, :{name}, {:?}, {}, true, {}, {})",
                        target.0.as_str(),
                        key(foreign_key),
                        primary_key.as_ref().map_or_else(|| ":id".to_string(), key),
                        belongs_to_inverse(app, model, assoc, &declarations.of(app, span).0)
                            .map_or_else(|| "nil".to_string(), |i| format!(":{}", i.as_str()))
                    )
                }
                Association::HasOne { target, foreign_key, as_interface, scope, dependent, .. } => {
                    format!(
                        "ActiveRecord::Associations::HasOneAssociation.new(self, :{name}, {:?}, {}, {}, {}, {}, {})",
                        target.0.as_str(),
                        key(foreign_key),
                        as_interface.is_none() && scope.is_none(),
                        as_interface
                            .as_ref()
                            .map_or_else(|| "nil".to_string(), |i| format!(":{}_type", i.as_str())),
                        match dependent {
                            Dependent::None => "nil",
                            Dependent::Destroy => ":destroy",
                            Dependent::DestroyAsync => ":destroy_async",
                            Dependent::Delete => ":delete",
                            Dependent::DeleteAll => ":delete_all",
                            Dependent::Nullify => ":nullify",
                            Dependent::Restrict => ":restrict",
                        },
                        has_one_inverse(app, model, assoc, &declarations.of(app, span).0)
                            .map_or_else(|| "nil".to_string(), |i| format!(":{}", i.as_str()))
                    )
                }
                _ => continue,
            };
            let defined = |n: &str| {
                lc.methods.iter().any(|m| m.name.as_str() == n && m.receiver == MethodReceiver::Instance)
                    || crate::lower::model_to_library::model_defines_instance_method(model, &Symbol::from(n))
            };
            let mut def = |method: String, body: String| {
                if called.contains(&method) && !defined(&method) && !src.contains(&format!("def {method}(")) && !src.contains(&format!("def {method}\n")) {
                    src.push_str(&body);
                }
            };
            for (method, call) in [(format!("build_{name}"), "build"), (format!("create_{name}"), "create"), (format!("create_{name}!"), "create!")] {
                def(method.clone(), format!("def {method}(attributes = {{}}, &block)\n  {object}.{call}(attributes, &block)\nend\n"));
            }
            def(
                format!("reload_{name}"),
                format!("def reload_{name}\n  association = {object}\n  association.reload\n  association.target\nend\n"),
            );
            def(format!("reset_{name}"), format!("def reset_{name}\n  {object}.reset\nend\n"));
        }
        if src.is_empty() {
            continue;
        }
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("apply_singular_association_builders: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }
    }
}

/// The belongs_to on a has_one's target that points back at it, which
/// Rails' `set_inverse_instance` sets: the declaration's literal
/// `inverse_of:`, else the automatic one (reflection.rb
/// `automatic_inverse_of`): the target's belongs_to named for the owner's
/// class, on the same foreign key, when the has_one has no
/// `foreign_key:`, scope or `as:` of its own.
fn has_one_inverse(app: &App, owner: &crate::dialect::Model, assoc: &crate::dialect::Association, options: &str) -> Option<Symbol> {
    use crate::dialect::Association;
    let Association::HasOne { target, foreign_key, foreign_key_explicit, as_interface, scope, .. } = assoc else {
        return None;
    };
    let target = app.models.iter().find(|m| &m.name == target)?;
    if let Some(at) = options.find("inverse_of: ") {
        let value: String =
            options[at + "inverse_of: ".len()..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | ':')).collect();
        return match value.strip_prefix(':') {
            Some(inverse) => target.associations().find(|a| a.name().as_str() == inverse).map(|a| a.name().clone()),
            None => None,
        };
    }
    if *foreign_key_explicit || as_interface.is_some() || scope.is_some() {
        return None;
    }
    let candidate = crate::naming::underscore(crate::naming::demodulize(owner.name.0.as_str()));
    target.associations().find_map(|a| match a {
        Association::BelongsTo { name, target: t, foreign_key: fk, polymorphic: false, .. }
            if name.as_str() == candidate && *t == owner.name && fk == foreign_key =>
        {
            Some(name.clone())
        }
        _ => None,
    })
}

/// The association on a belongs_to's target that points back at it,
/// which Rails' `set_inverse_instance` sets: the declaration's literal
/// `inverse_of:` (`inverse_of: false` names none), else the automatic
/// one (reflection.rb `automatic_inverse_of`): the target's has_one or
/// has_many named for the owner's class, on the same foreign key, with
/// no scope, `as:` or `through:`, when the belongs_to has no
/// `foreign_key:` or scope of its own. `options` is the declaration's
/// hash literal (`DeclarationOptions::of`).
fn belongs_to_inverse(app: &App, owner: &crate::dialect::Model, assoc: &crate::dialect::Association, options: &str) -> Option<Symbol> {
    use crate::dialect::Association;
    let Association::BelongsTo { name, target, foreign_key, .. } = assoc else { return None };
    let target = app.models.iter().find(|m| &m.name == target)?;
    if let Some(at) = options.find("inverse_of: ") {
        let value: String =
            options[at + "inverse_of: ".len()..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | ':')).collect();
        return match value.strip_prefix(':') {
            Some(inverse) => target.associations().find(|a| a.name().as_str() == inverse).map(|a| a.name().clone()),
            None => None,
        };
    }
    if foreign_key.as_str() != format!("{}_id", name.as_str()) {
        return None;
    }
    // The plural name is tried only under
    // `automatically_invert_plural_associations`, which is off by default.
    let candidate = crate::naming::underscore(crate::naming::demodulize(owner.name.0.as_str()));
    target.associations().find_map(|a| match a {
        Association::HasOne { name, foreign_key: fk, foreign_key_explicit: false, as_interface: None, scope: None, .. }
        | Association::HasMany {
            name,
            foreign_key: fk,
            foreign_key_explicit: false,
            as_interface: None,
            scope: None,
            through: None,
            ..
        } if name.as_str() == candidate && fk == foreign_key => Some(name.clone()),
        _ => None,
    })
}

/// `Model.reflect_on_association(name)` on the PostgreSQL CRuby tree: a
/// reflection for every association the model compiled, as Rails builds
/// one per declaration (reflection.rb; the classes are ported in
/// runtime/spinel/cruby/active_record_core.rb):
///
/// ```ruby
/// def self.reflect_on_association(association)
///   reflections = ::Product.instance_variable_get(:@__roundhouse_reflections)
///   if reflections.nil?
///     reflections = { metafields: ::ActiveRecord::Reflection::HasManyReflection.new(:metafields, "Metafield", "owner_id", { as: "owner", autosave: false }, ::Product) }
///     ::Product.instance_variable_set(:@__roundhouse_reflections, reflections)
///   end
///   reflections.fetch(association.to_sym) { superclass.reflect_on_association(association) if superclass.respond_to?(:reflect_on_association) }
/// end
/// ```
///
/// Each reflection is built once, on the class that declares it, and
/// answered after, so a write to its `options` is seen by every later
/// reader and by subclasses, as Rails shares one reflection object:
/// Core's HasSyncedActiveRecordMetafields declares `base.has_many
/// :metafields, ...` in its `included` hook and then writes
/// `base.reflect_on_association(:metafields).options[:autosave] = false`.
/// The class answered a reflection only for an association a class
/// macro declares (`model_to_library::macro_associations`), so its 30
/// includers failed with "undefined method 'options' for nil".
///
/// `options` is the declaration's keyword hash: each literal value is
/// copied from the call in the source (`DeclarationOptions`); any other
/// (a lambda, a constant, a `**splat`, a block's extension) is named,
/// and reading `options` raises NotImplementedError with the names
/// rather than answering a hash without them. An association a class
/// macro declares takes its options when the macro's `belongs_to` runs
/// (`_roundhouse_declared`). A later declaration of a name replaces the
/// earlier, last, as Rails' `add_reflection` does. A subclass of another
/// model hands the names it does not declare to its parent's method (nil
/// when the parent answers none, as Rails answers nil for a name no class
/// declares); a root model answers nil, and a subclass that declares
/// nothing inherits its parent's. A `reflect_on_association` the model defines itself (or
/// a concern's copy) wins; the one `macro_associations` writes is
/// replaced in place, so an include positioned after it
/// (`hooked_include_position`) stays there.
///
/// CRuby/JRuby with the PostgreSQL runtime only, where the reflection
/// classes are carried, and only where some body calls
/// `reflect_on_association` (or `accepts_nested_attributes_for`, which
/// reads it) or a class macro declares an association.
pub(crate) fn apply_association_reflections(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::{Association, ModelBodyItem};

    if !super::expr::native_keywords() || !app.carries_postgresql_runtime() {
        return;
    }
    let generated = |m: &MethodDef| {
        m.receiver == MethodReceiver::Class && m.name.as_str() == "reflect_on_association" && m.name_span.file.0 == 0
    };
    if !lcs.iter().any(|lc| lc.methods.iter().any(generated))
        && !app_calls_method(app, "reflect_on_association")
        && !app_joins_by_name(app)
        && !app_calls_method(app, "accepts_nested_attributes_for")
    {
        return;
    }
    let mut declarations = DeclarationOptions::default();
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        if lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "reflect_on_association" && !generated(m)) {
            continue; // the model's own wins
        }
        let inherits = model.parent.as_ref().is_some_and(|p| app.models.iter().any(|m| &m.name == p));
        let owner = format!("::{}", model.name.0.as_str().trim_start_matches("::"));
        let macro_declared = crate::lower::model_to_library::macro_associations::declaring_calls(model);
        let mut entries: Vec<(String, String)> = Vec::new();
        for item in &model.body {
            let ModelBodyItem::Association { assoc, span, .. } = item else { continue };
            let name = assoc.name().as_str();
            let (options, mut unsupported) = if !span.is_synthetic() && macro_declared.contains(span) {
                ("nil".to_string(), Vec::new())
            } else {
                declarations.of(app, *span)
            };
            if matches!(assoc, Association::HasMany { extension, .. } if !extension.is_empty())
                && !unsupported.iter().any(|u| u.contains("block"))
            {
                unsupported.push("`extend:` (the declaration's block)".to_string());
            }
            let unsupported = if unsupported.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = unsupported.iter().map(|u| ruby_single_quoted(u)).collect();
                format!(", unsupported: [{}]", names.join(", "))
            };
            let class = |t: &ClassId| format!("{:?}", t.0.as_str().trim_start_matches("::"));
            let reflection = "::ActiveRecord::Reflection";
            let new = match assoc {
                Association::BelongsTo { target, foreign_key, polymorphic, foreign_type, primary_key, .. } => {
                    let foreign_type = match (polymorphic, foreign_type) {
                        (true, Some(t)) => format!("{:?}", t.as_str()),
                        (true, None) => format!("{:?}", format!("{name}_type")),
                        (false, _) => "nil".to_string(),
                    };
                    format!(
                        "{reflection}::BelongsToReflection.new(:{name}, {:?}, {}, {foreign_type}, {}, {options}, {owner}{unsupported})",
                        foreign_key.as_str(),
                        if *polymorphic { "nil".to_string() } else { class(target) },
                        primary_key.as_ref().map_or_else(|| "nil".to_string(), |k| format!("{:?}", k.as_str())),
                    )
                }
                Association::HasMany { target, foreign_key, through: None, .. } => format!(
                    "{reflection}::HasManyReflection.new(:{name}, {}, {:?}, {options}, {owner}{unsupported})",
                    class(target),
                    foreign_key.as_str()
                ),
                Association::HasMany { target, through: Some(_), .. } => format!(
                    "{reflection}::ThroughReflection.new({reflection}::HasManyReflection.new(:{name}, {}, nil, {options}, {owner}{unsupported}))",
                    class(target)
                ),
                Association::HasOne { target, foreign_key, .. } => format!(
                    "{reflection}::HasOneReflection.new(:{name}, {}, {:?}, {options}, {owner}{unsupported})",
                    class(target),
                    foreign_key.as_str()
                ),
                Association::HasAndBelongsToMany { target, .. } => format!(
                    "{reflection}::HasAndBelongsToManyReflection.new(:{name}, {}, nil, {options}, {owner}{unsupported})",
                    class(target)
                ),
            };
            entries.retain(|(n, _)| n != name);
            entries.push((name.to_string(), new));
        }
        if entries.is_empty() && inherits {
            // The parent's answers every name.
            if let Some(i) = lc.methods.iter().position(generated) {
                lc.methods.remove(i);
            }
            continue;
        }
        let otherwise = if inherits {
            "reflections.fetch(association.to_sym) { superclass.reflect_on_association(association) if superclass.respond_to?(:reflect_on_association) }"
        } else {
            "reflections[association.to_sym]"
        };
        let table: Vec<String> = entries.iter().map(|(name, new)| format!("{name}: {new}")).collect();
        let src = format!(
            "def self.reflect_on_association(association)\n  \
             reflections = {owner}.instance_variable_get(:@__roundhouse_reflections)\n  \
             if reflections.nil?\n    \
             reflections = {{ {} }}\n    \
             {owner}.instance_variable_set(:@__roundhouse_reflections, reflections)\n  \
             end\n  \
             {otherwise}\n\
             end\n",
            table.join(", ")
        );
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("apply_association_reflections: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            m.enclosing_class = Some(lc.name.0.clone());
            m.name_span = Span::synthetic();
            match lc.methods.iter().position(generated) {
                Some(i) => lc.methods[i] = m,
                None => lc.methods.insert(0, m),
            }
        }
    }
}

/// `text` as a Ruby single-quoted string literal, which interpolates
/// nothing.
fn ruby_single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The keyword options of association declarations, read off the
/// source call at the declaration's span, parsed once per file.
#[derive(Default)]
struct DeclarationOptions {
    by_file: HashMap<u32, HashMap<u32, (String, Vec<String>)>>,
}

impl DeclarationOptions {
    /// The options of the call at `span` as a Ruby hash literal of its
    /// literal values in source order, and a description of each option
    /// it leaves out.
    fn of(&mut self, app: &App, span: Span) -> (String, Vec<String>) {
        let missing = || ("{}".to_string(), vec!["the declaration (no source call)".to_string()]);
        if span.is_synthetic() || span.file.0 == 0 {
            return missing();
        }
        let calls = self.by_file.entry(span.file.0).or_insert_with(|| {
            match (span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) {
                Some(source) => declaration_calls(&source.text),
                None => HashMap::default(),
            }
        });
        calls.get(&span.start).cloned().unwrap_or_else(missing)
    }
}

/// Every call in `text` by its start offset (the outermost of calls
/// starting together): its keyword options, as `DeclarationOptions::of`
/// answers them.
fn declaration_calls(text: &str) -> HashMap<u32, (String, Vec<String>)> {
    struct Calls {
        out: HashMap<u32, (String, Vec<String>)>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Calls {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            // By the call's start, and by its message's: a statement an
            // `included(base)` hook declares is read with `base.`
            // blanked, so its span starts at `has_many`.
            let starts = [Some(node.location().start_offset()), node.message_loc().map(|m| m.start_offset())];
            for at in starts.into_iter().flatten() {
                self.out.entry(at as u32).or_insert_with(|| call_options(node));
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    fn label(name: &str) -> bool {
        let mut chars = name.chars();
        chars.next().is_some_and(|c| c == '_' || c.is_ascii_lowercase())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    }
    // A literal whose source text means the same anywhere.
    fn literal(node: &ruby_prism::Node<'_>) -> bool {
        if let Some(s) = node.as_string_node() {
            return !s.opening_loc().is_some_and(|o| o.as_slice().starts_with(b"<<"));
        }
        if let Some(a) = node.as_array_node() {
            return a.elements().iter().all(|e| literal(&e));
        }
        if let Some(h) = node.as_hash_node() {
            return h.elements().iter().all(|e| {
                e.as_assoc_node().is_some_and(|a| a.key().as_symbol_node().is_some() && literal(&a.value()))
            });
        }
        node.as_symbol_node().is_some()
            || node.as_integer_node().is_some()
            || node.as_float_node().is_some()
            || node.as_true_node().is_some()
            || node.as_false_node().is_some()
            || node.as_nil_node().is_some()
    }
    fn call_options(call: &ruby_prism::CallNode<'_>) -> (String, Vec<String>) {
        let mut pairs: Vec<String> = Vec::new();
        let mut unsupported: Vec<String> = Vec::new();
        for arg in call.arguments().iter().flat_map(|a| a.arguments().iter()) {
            let Some(hash) = arg.as_keyword_hash_node() else { continue };
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else {
                    unsupported.push("a `**` splat".to_string());
                    continue;
                };
                let key = assoc.key();
                let name = key.as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned());
                match name {
                    Some(name) if label(&name) => {
                        let value = assoc.value();
                        if literal(&value) {
                            pairs.push(format!("{name}: {}", String::from_utf8_lossy(value.location().as_slice())));
                        } else {
                            unsupported.push(format!("`{name}:` (not a literal)"));
                        }
                    }
                    _ => unsupported.push(format!("the key `{}`", String::from_utf8_lossy(key.location().as_slice()))),
                }
            }
        }
        if call.block().is_some() {
            unsupported.push("`extend:` (the declaration's block)".to_string());
        }
        let hash = if pairs.is_empty() { "{}".to_string() } else { format!("{{ {} }}", pairs.join(", ")) };
        (hash, unsupported)
    }
    let mut calls = Calls { out: HashMap::default() };
    ruby_prism::Visit::visit(&mut calls, &ruby_prism::parse(text.as_bytes()).node());
    calls.out
}

/// Some emit-bound body, view or test sends `method`.
fn app_calls_method(app: &App, method: &str) -> bool {
    app_sends(app, &|m, _| m == method)
}

/// Some emit-bound body, view or test joins by something other than SQL
/// text (`base.left_joins(:name)`, `model.joins(spec)`). Where the
/// receiver's model is known `lower::scope_chain` expands the spec to
/// SQL; where it is only known at runtime (a parameter, a `Class#new`
/// result) `Relation#join_fragment` resolves it from the model's
/// reflections. Which of the two a call becomes is not known at this
/// stage, so any such call carries the reflections.
fn app_joins_by_name(app: &App) -> bool {
    app_sends(app, &|m, args| {
        matches!(m, "joins" | "left_joins" | "left_outer_joins")
            && args.first().is_some_and(|a| {
                !matches!(&*a.node, ExprNode::Lit { value: Literal::Str { .. } } | ExprNode::StringInterp { .. })
            })
    })
}

fn app_sends(app: &App, wanted: &dyn Fn(&str, &[Expr]) -> bool) -> bool {
    fn walk(e: &Expr, wanted: &dyn Fn(&str, &[Expr]) -> bool, found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { method: m, args, .. } = &*e.node {
            if wanted(m.as_str(), args) {
                *found = true;
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, wanted, found));
    }
    let mut found = false;
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, wanted, &mut found));
    for view in &app.views {
        walk(&view.body, wanted, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |root| walk(root, wanted, &mut found));
    found
}

/// Some emit-bound body, view or test calls `association(x)`, on a
/// receiver or on self, reads the `association(:x).target` that
/// `lower::assoc_loaded` collapsed onto a reader (`IrHint::
/// AssociationTarget`, which the Ruby emit writes back), or reloads or
/// resets a collection reader (`IrHint::AssociationProxy`, which runs
/// on the association object), or asks a belongs_to's
/// `association(:x).loaded?` (collapsed onto the flat `x_loaded?`,
/// which answers a stale target as loaded from the same record).
pub(crate) fn app_calls_association(app: &App) -> bool {
    let belongs_to_loaded: Vec<String> = app
        .models
        .iter()
        .flat_map(|m| m.associations())
        .filter_map(|a| match a {
            crate::dialect::Association::BelongsTo { name, polymorphic: false, .. } => {
                Some(format!("{}_loaded?", name.as_str()))
            }
            _ => None,
        })
        .collect();
    fn walk(e: &Expr, belongs_to_loaded: &[String], found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { method, args, block: None, .. } = &*e.node {
            if args.is_empty() && belongs_to_loaded.iter().any(|n| n == method.as_str()) {
                *found = true;
                return;
            }
        }
        if matches!(
            e.hint,
            Some(crate::expr::IrHint::AssociationTarget | crate::expr::IrHint::AssociationProxy)
        ) {
            *found = true;
            return;
        }
        if let ExprNode::Send { method, args, .. } = &*e.node {
            if matches!(method.as_str(), "association" | "association_cached?") && args.len() == 1 {
                *found = true;
                return;
            }
        }
        // `accepts_nested_attributes_for`'s writers read and build through
        // the association object (runtime/spinel/cruby/active_record_nested_attributes.rb).
        if let ExprNode::Send { method, .. } = &*e.node {
            if method.as_str() == "accepts_nested_attributes_for" {
                *found = true;
                return;
            }
        }
        // `Associations::Preloader` reads each record's association object.
        if matches!(&*e.node, ExprNode::Const { path } if path.len() >= 2 && path[path.len() - 2].as_str() == "Associations" && path[path.len() - 1].as_str() == "Preloader") {
            *found = true;
            return;
        }
        e.node.for_each_child(&mut |c| walk(c, belongs_to_loaded, found));
    }
    let mut found = false;
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, &belongs_to_loaded, &mut found));
    for view in &app.views {
        walk(&view.body, &belongs_to_loaded, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |root| walk(root, &belongs_to_loaded, &mut found));
    found
}

/// A polymorphic `belongs_to` whose implementors ingest could not name
/// (no `as:` inverse, no `types:`), on the CRuby and JRuby trees: the
/// shared lowering leaves it without accessors, since a strict target
/// cannot name the class, and `record.approver` was a NoMethodError.
/// Rails reads the class from the type column when asked
/// (`BelongsToPolymorphicAssociation#klass`, `polymorphic_class_for`),
/// so these do:
///
/// ```ruby
/// def approver
///   type = approver_type
///   if approver_id.nil? || type.nil? || type.empty?
///     nil
///   else
///     Object.const_get(type).find_by(id: approver_id)   # the primary_key:
///   end
/// end
///
/// def approver=(value)
///   self.approver_id = value.nil? ? nil : value.id
///   self.approver_type = value.nil? ? nil : value.class.polymorphic_name
/// end
/// ```
///
/// The memoization below then puts the reader behind the loaded guard
/// and the writer loads what it assigns, as for every belongs_to. A
/// reader or writer the model writes itself wins.
fn synth_open_polymorphic_accessors(lc: &mut LibraryClass, model: &crate::dialect::Model) {
    use crate::dialect::Association;
    for assoc in model.associations() {
        let Association::BelongsTo {
            name, foreign_key, polymorphic: true, polymorphic_targets, foreign_type, primary_key, ..
        } = assoc
        else {
            continue;
        };
        if !polymorphic_targets.is_empty() {
            continue;
        }
        let n = name.as_str();
        let fk = foreign_key.as_str();
        let type_column = foreign_type.as_ref().map_or_else(|| format!("{n}_type"), |t| t.as_str().to_string());
        let pk = primary_key.as_ref().map_or("id", |k| k.as_str());
        let has = |lc: &LibraryClass, method: &str| {
            lc.methods.iter().any(|m| m.name.as_str() == method && m.receiver == MethodReceiver::Instance)
                || crate::lower::model_to_library::model_defines_instance_method(model, &Symbol::from(method))
        };
        let mut src = String::new();
        if !has(lc, n) {
            src.push_str(&format!(
                "def {n}\n  if {fk}.nil? || {type_column}.nil? || {type_column}.empty?\n    nil\n  else\n    \
                 Object.const_get({type_column}).find_by({pk}: {fk})\n  end\nend\n"
            ));
        }
        if !has(lc, &format!("{n}=")) {
            src.push_str(&format!(
                "def {n}=(value)\n  self.{fk} = value.nil? ? nil : value.{pk}\n  \
                 self.{type_column} = value.nil? ? nil : value.class.polymorphic_name\nend\n"
            ));
        }
        if src.is_empty() {
            continue;
        }
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("synth_open_polymorphic_accessors: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            m.enclosing_class = Some(lc.name.0.clone());
            // Synthesized, so the loaded guard goes on (`ensure_cache_guard`).
            m.name_span = Span::synthetic();
            lc.methods.push(m);
        }
    }
}

/// `Seq[guard, query]` -> `Seq[guard, @cache = query, @loaded = true, @cache]`.
///
/// Splitting on the guard rather than reaching into the query is what
/// keeps this robust: the reader's second half differs per association
/// (a `Db.prepare` for most, a preloaded read for others) and this pass
/// does not need to know which.
fn memoize_reader(lc: &mut LibraryClass, name: &Symbol) {
    let cache = Symbol::from(format!("{}_cache", name.as_str()));
    let loaded = Symbol::from(format!("{}_loaded", name.as_str()));
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| &m.name == name && m.receiver == MethodReceiver::Instance)
    else {
        return;
    };
    let ExprNode::Seq { exprs } = &mut *m.body.node else { return };
    // Exactly the synthesized shape: the loaded-guard, then one
    // expression that produces the record. Anything else is a
    // hand-written reader and is left alone.
    if exprs.len() != 2 {
        return;
    }
    let guard_matches = matches!(&*exprs[0].node,
        ExprNode::If { cond, .. } if matches!(&*cond.node, ExprNode::Ivar { name } if name == &loaded));
    if !guard_matches {
        return;
    }
    let span = exprs[1].span;
    let query = exprs[1].clone();
    let syn = |n| Expr::new(span, n);
    *exprs = vec![
        exprs[0].clone(),
        syn(ExprNode::Assign { target: LValue::Ivar { name: cache.clone() }, value: query }),
        syn(ExprNode::Assign {
            target: LValue::Ivar { name: loaded },
            value: syn(ExprNode::Lit { value: Literal::Bool { value: true } }),
        }),
        syn(ExprNode::Ivar { name: cache }),
    ];
}

/// `def <fk>=(value); @<fk> = value; end` first drops a loaded target
/// when the key changes: `@<name>_loaded = false if value != <fk>`, and
/// with `stale_state`, records it going stale (`if @<name>_loaded;
/// @<name>_stale_state = <fk>; @<name>_stale_loaded = true; end`).
/// Rails' reader reloads only a stale target (`stale_target?`, the key
/// differing from the one it was loaded for): writing the key it
/// already holds keeps the target and its in-memory changes.
fn invalidate_on_fk_write(lc: &mut LibraryClass, name: &Symbol, fk: &Symbol, stale_state: bool) {
    let writer = Symbol::from(format!("{}=", fk.as_str()));
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| m.name == writer && m.receiver == MethodReceiver::Instance && m.params.len() == 1)
    else {
        return;
    };
    let span = m.body.span;
    let n = name.as_str();
    let (value, fk) = (m.params[0].name.as_str(), fk.as_str());
    let record = if stale_state {
        format!("    if @{n}_loaded\n      @{n}_stale_state = {fk}\n      @{n}_stale_loaded = true\n    end\n")
    } else {
        String::new()
    };
    let src = format!("def __invalidate({value})\n  if {value} != {fk}\n{record}    @{n}_loaded = false\n  end\nend\n");
    let guard = crate::runtime_src::parse_methods(&src)
        .unwrap_or_else(|e| panic!("invalidate_on_fk_write: generated source failed to parse: {e}\n{src}"))
        .pop()
        .expect("one method");
    let mut stmts = match &*m.body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![m.body.clone()],
    };
    stmts.insert(0, guard.body);
    m.body = Expr::new(span, ExprNode::Seq { exprs: stmts });
}

/// Ruby-family pre-emit pass: an STI row hydrates as its SUBCLASS.
///
/// `Room.first.open?` was false for a row whose `type` column says
/// `"Rooms::Open"`. Both hydration entry points build the BASE class
/// unconditionally, so `open?` — which campfire writes as
/// `is_a?(Rooms::Open)` — asked a question the object could never
/// answer yes to.
///
/// ```ruby
/// instance = case row.type
///            when "Rooms::Open" then Rooms::Open.new
///            when "Rooms::Closed" then Rooms::Closed.new
///            else Room.new
///            end
/// ```
///
/// BOTH entry points, and that is the part that cost a probe. A model
/// has two: `from_stmt` reads the typed `Db.column_*` path, `from_row`
/// reads a Row through `instantiate`. `Room.first` goes to `to_a` ->
/// `instantiate` -> `from_row`; patching only `from_stmt` moved
/// nothing, exactly as adding `Request#head?` to one of the two
/// Requests moved nothing. When a runtime concept has two doors,
/// measure which one the failing call uses.
///
/// The scrutinee is CLONED off the body's own `instance.type = <expr>`
/// assignment rather than rebuilt: `from_stmt` reads
/// `Db.column_text(stmt, 4)` and `from_row` reads `row.type`, and the
/// column INDEX in the first is a fact only that body knows.
///
/// Each arm passes the base `new`'s arguments on: a factory that
/// constructs with `ActiveRecord::Base::HYDRATE_ATTRS` (an
/// `after_initialize` hook or an `attribute ..., default:`; see
/// `constructs_with_hydrate_attrs`) must hand the sentinel to the
/// subclass too, or the record would run the hook or the default as if
/// it were new. Matching only an argument-free `new` left those models
/// hydrating the base class.
///
/// Ruby-family only, like its neighbours. A strict target's `Rooms::
/// Open` is not a subtype of `Room` — there is no inheritance to make
/// the case arms share a return type — so this is not expressible
/// there, and those targets keep hydrating the base class.
pub(crate) fn apply_sti_hydration(lcs: &mut [LibraryClass], app: &App) {
    // subclass -> base, from the one authority on the question.
    let bases = crate::lower::sti_bases(app);
    if bases.is_empty() {
        return;
    }
    let cols = crate::lower::sti_scope::sti_columns(app);
    let runtime_named = crate::lower::sti_scope::runtime_sti_named(app);
    let mut subs_of: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for (sub, base) in &bases {
        subs_of.entry(base.clone()).or_default().push(sub.clone());
    }
    // Deterministic arm order — the map's iteration order is not.
    for v in subs_of.values_mut() {
        v.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    }
    for lc in lcs.iter_mut() {
        let Some(subs) = subs_of.get(&lc.name) else { continue };
        let base = lc.name.clone();
        let column = cols.get(&base).map(|c| c.as_str().to_string()).unwrap_or_else(|| "type".to_string());
        let writer = format!("{column}=");
        for m in lc.methods.iter_mut() {
            if !matches!(m.name.as_str(), "from_stmt" | "from_row") {
                continue;
            }
            if m.receiver != crate::dialect::MethodReceiver::Class {
                continue;
            }
            let ExprNode::Seq { exprs } = &mut *m.body.node else { continue };
            // The column writes are SENDS (`instance.type=(row.type)`),
            // not `Assign { Attr }` — the model lowerer routes every
            // hydration write through the typed writer method. Both
            // spellings are matched anyway: this pass reads the body a
            // sibling pass wrote, and pinning it to one shape is how a
            // reader goes stale.
            let Some(type_read) = exprs.iter().find_map(|e| match &*e.node {
                ExprNode::Send { recv: Some(_), method, args, .. }
                    if method.as_str() == writer && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                ExprNode::Assign { target: LValue::Attr { name, .. }, value }
                    if name.as_str() == column =>
                {
                    Some(value.clone())
                }
                _ => None,
            }) else {
                continue;
            };
            for e in exprs.iter_mut() {
                let ExprNode::Assign { target: LValue::Var { .. }, value } = &mut *e.node else {
                    continue;
                };
                let new_args = match &*value.node {
                    ExprNode::Send { recv: Some(r), method, args, .. }
                        if method.as_str() == "new"
                            && matches!(&*r.node, ExprNode::Const { path }
                                if const_path_is(path, &base)) =>
                    {
                        args.clone()
                    }
                    _ => continue,
                };
                *value = if runtime_named.contains(&base) {
                    sti_class_dispatch(value.clone(), &type_read, &new_args, value.span)
                } else {
                    sti_dispatch(value.clone(), &type_read, subs, &new_args, value.span)
                };
                break;
            }
        }
    }
}

fn const_path_is(path: &[Symbol], id: &ClassId) -> bool {
    let joined: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    joined.join("::") == id.0.as_str()
}

/// `case <type_read> when "<Sub>" then <Sub>.new(<args>) … else <base_new> end`
fn sti_dispatch(base_new: Expr, type_read: &Expr, subs: &[ClassId], args: &[Expr], span: Span) -> Expr {
    let mut arms: Vec<crate::expr::Arm> = subs
        .iter()
        .map(|sub| crate::expr::Arm {
            pattern: crate::expr::Pattern::Lit {
                value: Literal::Str { value: sub.0.as_str().to_string() },
            },
            guard: None,
            body: Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(Expr::new(
                        span,
                        ExprNode::Const {
                            path: sub.0.as_str().split("::").map(Symbol::from).collect(),
                        },
                    )),
                    method: Symbol::from("new"),
                    args: args.to_vec(),
                    block: None,
                    parenthesized: true,
                },
            ),
        })
        .collect();
    // `else` — a row whose type names no known subclass (or is empty)
    // stays the base class, which is what Rails does for a blank
    // inheritance column.
    arms.push(crate::expr::Arm {
        pattern: crate::expr::Pattern::Wildcard,
        guard: None,
        body: base_new,
    });
    Expr::new(span, ExprNode::Case { scrutinee: type_read.clone(), arms })
}

/// `case <type_read> when nil, "" then <base_new> else
/// find_sti_class(<type_read>).new(<args>) end`
///
/// A runtime-named hierarchy (`sti_scope::runtime_sti_named`) stores
/// the app's `sti_name`, so its rows hydrate as Rails'
/// `discriminate_class_for_record` does: a present type goes through
/// `find_sti_class`, which asks the app's `sti_class_for` and raises
/// SubclassNotFound for a type naming no subclass.
fn sti_class_dispatch(base_new: Expr, type_read: &Expr, args: &[Expr], span: Span) -> Expr {
    let blank = |value: Literal| crate::expr::Arm {
        pattern: crate::expr::Pattern::Lit { value },
        guard: None,
        body: base_new.clone(),
    };
    let found = Expr::new(
        span,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("find_sti_class"),
            args: vec![type_read.clone()],
            block: None,
            parenthesized: true,
        },
    );
    let arms = vec![
        blank(Literal::Nil),
        blank(Literal::Str { value: String::new() }),
        crate::expr::Arm {
            pattern: crate::expr::Pattern::Wildcard,
            guard: None,
            body: Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(found),
                    method: Symbol::from("new"),
                    args: args.to_vec(),
                    block: None,
                    parenthesized: true,
                },
            ),
        },
    ];
    Expr::new(span, ExprNode::Case { scrutinee: type_read.clone(), arms })
}

/// Ruby-family pre-emit pass: `belongs_to` autosave.
///
/// `room.creator = User.new(attrs); room.save!` is ordinary Rails —
/// the unsaved creator is saved with its owner and the foreign key
/// follows. The shared writer stores `value.id` and nothing else, so
/// the key stayed 0 and campfire's whole first run died on
/// `Validation failed: Creator must exist` (four tests, two files).
///
/// Two edits per non-polymorphic belongs_to:
///
///   1. the writer stashes an UNSAVED value in the reader's cache —
///      a record that is not in the table yet cannot be found again from
///      the foreign key, so something has to hold it until the save;
///   2. `_autosave_<name>` saves it and takes the id, folded into
///      `before_validation`.
///
/// ```ruby
/// def creator=(value)
///   if value.nil?
///     @creator_id = 0
///   else
///     @creator_id = value.id
///     if value.new_record?
///       @creator_cache = value
///       @creator_loaded = true
///     end
///   end
/// end
///
/// def _autosave_creator
///   if @creator_loaded && !@creator_cache.nil? && !@creator_cache.destroyed?
///     @creator_cache.save if @creator_cache.new_record?
///     @creator_id = @creator_cache.id   # the `primary_key:`, `id` by default
///   end
/// end
/// ```
///
/// That is Rails' `save_belongs_to_association` (autosave_association.rb):
/// a loaded, not-stale, not-destroyed target is saved when it is a new
/// record, and the key then takes its id (`self[fk] = record.id` when
/// `association.updated?`), saved here or not. A target assigned new
/// and saved by hand before its owner (`p.owner = o; o.save!; p.save!`)
/// gives the owner its id at the owner's save; taking the id only after
/// saving it here left that key nil. A key written after the target
/// (`owner_id=`) clears `@creator_loaded`, Rails' stale target, so the
/// write stands. A target the reader loaded has the key's id already.
///
/// "Unsaved" is Rails' rule (`save_belongs_to_association`: save when
/// `association.new_record?`), read off the record, not off the key it
/// carries. The key misreads both ways: a retyped key (Core's
/// `model_uuid_attributes`) is nil, not 0, while its target is unsaved,
/// and a target given its id before its save (`Shop.new(id: …)`) has a
/// key and is still not in the table. The old `== 0` test saved
/// neither (tests/belongs_to_autosave_pg_runtime.rs). The cache is read
/// at save time, so a target assigned new and saved by hand before its
/// owner is not saved twice, and the owner still takes its id.
///
/// A SAVED value deliberately does not populate the cache, so the
/// reader keeps re-querying by foreign key for those and a later direct
/// write to the key cannot be answered from a stale object. (The CRuby
/// and JRuby trees' writer also loads every assigned value into the
/// cache, `load_on_assign`; the `new_record?` test is what keeps a saved
/// one from being saved again.)
///
/// WHY RUBY-FAMILY AND NOT THE SHARED LOWERING. This started shared and
/// the strict targets refused it, each in its own way — the two of them
/// do not even agree on the shape of the slot being read:
///
///   * Rust emits the cache field as a plain `Article`, so `.nil?`
///     became `is_null()` on a struct that has no such method;
///   * Crystal emits it as `Article?`, so dropping the nil test to
///     satisfy Rust gave `undefined method 'save' for Nil`;
///   * `new_record?` lives on the runtime `Base`, which a strict
///     target's model — a plain struct — never inherits;
///   * and an unstamped `@fk = <cache>.id` typed the foreign-key column
///     `Ty::Var`, which Rust reads as "already a Value": `article_id`
///     flipped from `i64` to `serde_json::Value` and took eleven E0308s
///     with it.
///
/// There is no single spelling that satisfies both, so this joins
/// `apply_through_assoc_lowering` as a Ruby-family capability and the
/// strict targets keep the gap they already had, ledgered in
/// docs/pipeline/runtime.md.
///
/// Ordering inside `before_validation` is load-bearing: the autosave
/// call is pushed BEFORE the belongs_to `default:` statement, which is
/// guarded on `@creator_id == 0`. Fill the key first or an explicitly
/// assigned unsaved creator is overwritten by the default.
pub(crate) fn apply_belongs_to_autosave(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        let names: Vec<(Symbol, Symbol, Symbol)> = model
            .associations()
            .filter_map(|a| match a {
                // `autosave: false` never saves the target.
                Association::BelongsTo { name, foreign_key, polymorphic: false, autosave, primary_key, .. }
                    if *autosave != Some(false) =>
                {
                    // `primary_key: :uuid`: the key takes the target's
                    // `uuid`, as Rails' `record._read_attribute(primary_key)`.
                    let pk = primary_key.clone().unwrap_or_else(|| Symbol::from("id"));
                    Some((name.clone(), foreign_key.clone(), pk))
                }
                _ => None,
            })
            .collect();
        for (name, fk, pk) in names {
            let writer = Symbol::from(format!("{}=", name.as_str()));
            let Some(w) = lc
                .methods
                .iter_mut()
                .find(|m| m.name == writer && m.receiver == MethodReceiver::Instance)
            else {
                continue;
            };
            // Only the SYNTHESIZED writer is patched, recognized by its
            // shape: `if value.nil? then @fk = 0 else @fk = value.id`.
            // A model that wrote its own `<name>=` keeps it untouched
            // and gets no autosave either — the stash is the only thing
            // `_autosave_<name>` reads, and watching a slot nobody
            // fills would be worse than the gap.
            let ExprNode::If { else_branch, .. } = &mut *w.body.node else { continue };
            if !matches!(&*else_branch.node, ExprNode::Assign { target: LValue::Ivar { name: n }, .. } if n == &fk)
            {
                continue;
            }
            let span = else_branch.span;
            let assign = else_branch.clone();
            *else_branch = Expr::new(
                span,
                ExprNode::Seq { exprs: vec![assign, stash_unsaved(&name, span)] },
            );
            lc.methods.push(autosave_method(&lc.name, &name, &fk, &pk));
            fold_before_validation(&mut lc.methods, &lc.name, &name);
            for m in lc.methods.iter_mut().filter(|m| {
                m.receiver == MethodReceiver::Instance
                    && matches!(m.name.as_str(), "initialize" | "update" | "update!")
            }) {
                route_object_key_through_writer(&mut m.body, &name, &fk);
            }
        }
    }
}

/// `Product.new(shop: s)` / `update(shop: s)` call Rails' `shop=` writer.
/// The shared constructor and `update` write the key instead,
/// `self.shop_id = attrs[:shop].id unless attrs[:shop].nil?` (one shape
/// every target compiles), so an unsaved target given as a keyword never
/// reached the writer's stash and was never autosaved: on a retyped key
/// its nil id failed `belongs_to`'s presence check, and an assigned id
/// was written with no row behind it. Here, beside the patched writer,
/// that statement calls the writer: `self.shop = attrs[:shop] unless
/// attrs[:shop].nil?`. Only that exact synthesized shape (a `Cast` of
/// `<attrs>[:<name>]` read for `.id` into `self.<fk>=`) is rewritten.
fn route_object_key_through_writer(e: &mut Expr, name: &Symbol, fk: &Symbol) {
    let fk_writer = format!("{}=", fk.as_str());
    let lookup = match &*e.node {
        ExprNode::Send { recv: Some(recv), method, args, .. }
            if matches!(&*recv.node, ExprNode::SelfRef) && method.as_str() == fk_writer && args.len() == 1 =>
        {
            match &*args[0].node {
                ExprNode::Send { recv: Some(obj), method, args: id_args, .. }
                    if method.as_str() == "id" && id_args.is_empty() =>
                {
                    match &*obj.node {
                        ExprNode::Cast { value, .. } => match &*value.node {
                            ExprNode::Send { recv: Some(_), method, args: key, .. }
                                if method.as_str() == "[]"
                                    && matches!(key.as_slice(), [k] if matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } } if value == name)) =>
                            {
                                Some(value.clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(lookup) = lookup {
        let span = e.span;
        let recv = match &*e.node {
            ExprNode::Send { recv: Some(recv), .. } => recv.clone(),
            _ => unreachable!("matched a Send with a receiver above"),
        };
        *e = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(recv),
                method: Symbol::from(format!("{}=", name.as_str())),
                args: vec![lookup],
                block: None,
                parenthesized: false,
            },
        );
        return;
    }
    e.node.for_each_child_mut(&mut |c| route_object_key_through_writer(c, name, fk));
}

/// `if value.new_record? then @<name>_cache = value; @<name>_loaded = true end`
fn stash_unsaved(name: &Symbol, span: Span) -> Expr {
    let syn = |n| Expr::new(span, n);
    let value = || syn(ExprNode::Var { id: VarId(0), name: Symbol::from("value") });
    syn(ExprNode::If {
        cond: syn(ExprNode::Send {
            recv: Some(value()),
            method: Symbol::from("new_record?"),
            args: vec![],
            block: None,
            parenthesized: false,
        }),
        then_branch: syn(ExprNode::Seq {
            exprs: vec![
                syn(ExprNode::Assign {
                    target: LValue::Ivar {
                        name: Symbol::from(format!("{}_cache", name.as_str())),
                    },
                    value: value(),
                }),
                syn(ExprNode::Assign {
                    target: LValue::Ivar {
                        name: Symbol::from(format!("{}_loaded", name.as_str())),
                    },
                    value: syn(ExprNode::Lit { value: Literal::Bool { value: true } }),
                }),
            ],
        }),
        else_branch: syn(ExprNode::Lit { value: Literal::Nil }),
    })
}

fn autosave_method(
    owner: &ClassId,
    name: &Symbol,
    fk: &Symbol,
    pk: &Symbol,
) -> crate::dialect::MethodDef {
    let span = Span::synthetic();
    let syn = |n| Expr::new(span, n);
    let cache = || syn(ExprNode::Ivar { name: Symbol::from(format!("{}_cache", name.as_str())) });
    let send = |recv: Expr, method: &str| {
        syn(ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args: vec![],
            block: None,
            parenthesized: false,
        })
    };
    let and = |l: Expr, r: Expr| {
        syn(ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::And,
            surface: crate::expr::BoolOpSurface::default(),
            left: l,
            right: r,
        })
    };
    let cond = and(
        and(
            syn(ExprNode::Ivar { name: Symbol::from(format!("{}_loaded", name.as_str())) }),
            send(send(cache(), "nil?"), "!"),
        ),
        send(send(cache(), "destroyed?"), "!"),
    );
    let body = syn(ExprNode::If {
        cond,
        then_branch: syn(ExprNode::Seq {
            exprs: vec![
                syn(ExprNode::If {
                    cond: send(cache(), "new_record?"),
                    then_branch: send(cache(), "save"),
                    else_branch: syn(ExprNode::Lit { value: Literal::Nil }),
                }),
                syn(ExprNode::Assign {
                    target: LValue::Ivar { name: fk.clone() },
                    value: send(cache(), pk.as_str()),
                }),
            ],
        }),
        else_branch: syn(ExprNode::Lit { value: Literal::Nil }),
    });
    crate::dialect::MethodDef {
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from(format!("_autosave_{}", name.as_str())),
        receiver: MethodReceiver::Instance,
        visibility: MethodVisibility::Public,
        params: Vec::new(),
        body,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: true,
        block_param: None,
    }
}

/// PREPEND, not append: see the ordering note on
/// [`apply_belongs_to_autosave`].
fn fold_before_validation(
    methods: &mut Vec<crate::dialect::MethodDef>,
    owner: &ClassId,
    name: &Symbol,
) {
    let span = Span::synthetic();
    let call = Expr::new(
        span,
        ExprNode::Send {
            recv: None,
            method: Symbol::from(format!("_autosave_{}", name.as_str())),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let hook = Symbol::from("before_validation");
    if let Some(existing) =
        methods.iter_mut().find(|m| m.name == hook && m.receiver == MethodReceiver::Instance)
    {
        let mut stmts = match &*existing.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![existing.body.clone()],
        };
        stmts.insert(0, call);
        existing.body = Expr::new(span, ExprNode::Seq { exprs: stmts });
        return;
    }
    methods.push(crate::dialect::MethodDef {
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: hook,
        receiver: MethodReceiver::Instance,
        visibility: MethodVisibility::Public,
        params: Vec::new(),
        body: call,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: true,
        block_param: None,
    });
}

/// Ruby-family pre-emit pass: correct `has_many :through` readers. The
/// shared lowering synthesizes EVERY has_many reader as a direct
/// foreign-key query (`Tag.where(story_id: @id)`) — wrong for `through:`,
/// where the foreign key lives on the join table. Rebuild those readers
/// as a Relation join through the intermediate:
///
///   def tags
///     return @tags_cache if @tags_loaded
///     ActiveRecord::Relation.new(Tag)
///       .joins("INNER JOIN taggings ON taggings.tag_id = tags.id")
///       .where("taggings.story_id = ?", @id)
///   end
///
/// The through-model's `belongs_to` whose target matches the assoc's
/// target supplies the source foreign key (works for `source:` renames —
/// `upvoted_stories, through: :votes, source: :story` finds
/// `Vote.belongs_to :story`); when the source is a `has_many` instead
/// the key is on the target table and the join reverses (campfire's
/// `reachable_messages, through: :rooms, source: :messages`). Nested chains — the source association on
/// the join model is itself `:through` (`Category has_many :stories,
/// through: :tags` where `Tag#stories` goes through taggings), or the
/// first hop is — recurse, adding one INNER JOIN per hop. Shapes a hop
/// can't prove (missing models, no matching source) are left on the
/// shared reader rather than guessed.
/// KNOWN GAP: association scope-lambdas (`-> { order(...) }`, the
/// upvoted vote-conditions) are dropped at ingest, so row order/filter
/// can diverge from Rails until the lambda lands in the IR.
pub(crate) fn apply_through_assoc_lowering(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        for assoc in model.associations() {
            let Association::HasMany {
                name, target, through: Some(thr_name), scope: assoc_scope, ..
            } = assoc
            else {
                continue;
            };
            let mut hop_scopes = Vec::new();
            let Some((joins, edge_table, edge_fk)) =
                resolve_through_chain(&app.models, model, thr_name, target, 0, &mut hop_scopes)
            else {
                continue;
            };
            let join_sql = joins.join(" ");
            let where_sql = format!("{edge_table}.{edge_fk} = ?");

            let Some(m) =
                lc.methods.iter_mut().find(|m| {
                    m.name == *name && m.receiver == crate::dialect::MethodReceiver::Instance
                })
            else {
                continue;
            };
            m.body = through_reader_body(name, target, &join_sql, &where_sql, &hop_scopes, assoc_scope);
        }
    }
}

/// Resolve the SQL join chain for a `has_many :through` on `model`
/// reaching `target` via the sibling association named `thr_name`.
/// Returns the INNER JOIN fragments (target-nearest first, ready to
/// `join(" ")`) plus the WHERE edge `(table, fk)` that points back at
/// the owner's id. One recursion per indirection: a first hop that is
/// itself `:through`, or a join-model source association that is.
/// `None` when a hop can't be proven — missing join model, no
/// `belongs_to`/`has_many`/through source matching the target class —
/// and for pathological depth (cyclic `through:` declarations).
fn resolve_through_chain(
    models: &[crate::dialect::Model],
    model: &crate::dialect::Model,
    thr_name: &Symbol,
    target: &ClassId,
    depth: usize,
    hop_scopes: &mut Vec<Expr>,
) -> Option<(Vec<String>, String, Symbol)> {
    use crate::dialect::Association;

    if depth > 4 {
        return None;
    }
    // The through association on the owner (`:votes`, `:taggings`,
    // `:tags`): a has_many, or a has_one (`has_many :x, through:
    // :primary_entity`), whose rows tie back to the owner by the same
    // foreign key. Its scope narrows the join rows, as Rails' through
    // scope chain merges every reflection's scope.
    let (thr_target, thr_fk, thr_through, as_interface, thr_scope) = model.associations().find_map(|a| match a {
        Association::HasMany { name, target, foreign_key, through, as_interface, scope, .. }
            if name == thr_name =>
        {
            Some((target, foreign_key, through.as_ref(), as_interface, scope))
        }
        Association::HasOne { name, target, foreign_key, as_interface, scope, .. } if name == thr_name => {
            Some((target, foreign_key, None, as_interface, scope))
        }
        _ => None,
    })?;
    if let Some(scope) = thr_scope {
        // Only a SQL-string condition is portable onto the target's
        // relation: a hash `where(primary: true)` would be qualified by
        // the target table, where Rails qualifies it by the through's.
        if through_scope_graftable(scope) {
            hop_scopes.push(scope.clone());
        }
    }
    // How the join model's rows tie back to the owner: directly by the
    // sibling's fk, or through the sibling's own chain.
    let (back_joins, edge_table, edge_fk) = match thr_through {
        None => (Vec::new(), model_table_name(models, thr_target), thr_fk.clone()),
        Some(inner) => resolve_through_chain(models, model, inner, thr_target, depth + 1, hop_scopes)?,
    };
    let thr_model = models.iter().find(|m| &m.name == thr_target)?;
    let thr_table = model_table_name(models, thr_target);
    let target_table = model_table_name(models, target);
    let owner_type = through_owner_type_predicate(&thr_table, as_interface, &model.name);
    // The source belongs_to on the join model (`Vote.belongs_to :story`)
    // — matched by target class, so `source:` renames resolve without a
    // name convention.
    if let Some(src_fk) = thr_model.associations().find_map(|a| match a {
        Association::BelongsTo { target: t, foreign_key, .. } if t == target => Some(foreign_key),
        _ => None,
    }) {
        let mut joins = vec![format!(
            "INNER JOIN {thr_table} ON {thr_table}.{src_fk} = {target_table}.id{owner_type}"
        )];
        joins.extend(back_joins);
        return Some((joins, edge_table, edge_fk));
    }
    // The source is a plain `has_many` on the join model, so the
    // foreign key is on the TARGET table and the join points the other
    // way (campfire: `has_many :reachable_messages, through: :rooms,
    // source: :messages` — Room#messages, `messages.room_id`). The
    // belongs_to branch above cannot serve this: there is no column on
    // the join table naming the target.
    if let Some((src_fk, src_scope)) = thr_model.associations().find_map(|a| match a {
        Association::HasMany { target: t, foreign_key, through: None, scope, .. } if t == target => {
            Some((foreign_key, scope))
        }
        _ => None,
    }) {
        // The source association's own scope (`has_many :gateways, ->
        // { merge(Gateway.enabled) }`) is part of the through query.
        if let Some(scope) = src_scope {
            if !reads_local(scope) {
                hop_scopes.push(scope.clone());
            }
        }
        let mut joins = vec![format!(
            "INNER JOIN {thr_table} ON {thr_table}.id = {target_table}.{src_fk}{owner_type}"
        )];
        joins.extend(back_joins);
        return Some((joins, edge_table, edge_fk));
    }
    // Nested: the join model reaches the target through its own
    // `:through` association. Resolve that chain (phrased from the same
    // target table), then graft the join model onto its owner edge.
    let src_through = thr_model.associations().find_map(|a| match a {
        Association::HasMany { target: t, through: Some(thru), .. } if t == target => Some(thru),
        _ => None,
    })?;
    let (src_joins, src_edge_table, src_edge_fk) =
        resolve_through_chain(models, thr_model, src_through, target, depth + 1, hop_scopes)?;
    let mut joins = src_joins;
    joins.push(format!(
        "INNER JOIN {thr_table} ON {thr_table}.id = {src_edge_table}.{src_edge_fk}{owner_type}"
    ));
    joins.extend(back_joins);
    Some((joins, edge_table, edge_fk))
}

/// The table a model reads: its own (`self.table_name`, a namespace's
/// `table_name_prefix`), else Rails' default from the demodulized class
/// name (`compute_table_name`).
fn model_table_name(models: &[crate::dialect::Model], class: &ClassId) -> String {
    match models.iter().find(|m| &m.name == class) {
        Some(m) => m.table.0.as_str().to_string(),
        None => crate::naming::rails_table_name(class.0.as_str()),
    }
}

/// Keep the same polymorphic owner restriction on lazy and batched through joins.
/// An id alone is not unique across the classes sharing an `as:` interface.
fn through_owner_type_predicate(table: &str, as_interface: &Option<Symbol>, owner: &ClassId) -> String {
    match as_interface {
        Some(interface) => {
            let owner_name = owner.0.as_str().replace('\'', "''");
            format!(" AND {table}.{interface}_type = '{owner_name}'")
        }
        None => String::new(),
    }
}

/// The joined Relation chain, carrying the eager-load cache (see
/// `apply_through_assoc_lowering`).
///
/// ONE return path, and it is a Relation. The shared
/// `synth_has_many_reader` opens its body with `return @<name>_cache if
/// @<name>_loaded` — an Array — which is the right answer for a direct
/// has_many, whose reader materializes rows and is declared
/// `Array[T]`. A `through:` reader is declared `ActiveRecord::Relation`
/// (`associations.rs`, and the campfire routing bug its comment names),
/// so that guard made the method answer two unrelated types. On CRuby
/// that was latent — a preloaded `user.upvoted_stories.includes(:tags)
/// .order(...)` would reach `Array#includes`, which does not exist —
/// and under spinel's AOT it is a hard compile stop: `--rbs seed
/// contradicted: User#upvoted_stories is declared to return Relation
/// but this returns int_array` (spinel judges a seeded return as of
/// 2368afd7; the cache ivar types `int_array` off the bare `[]` in
/// `initialize` when nothing in the app preloads it). It took the
/// lobsters spinel bench lane down for three days.
///
/// So the preload seam moves ONTO the relation: `_preload_<name>` still
/// fills the cache ivar, and the reader hands it to the relation it was
/// going to answer with anyway. `preloaded` seeds the loaded-records
/// memo when the flag is set and is a no-op when it is not — the flag
/// cannot be inferred from the cache, which is `[]` both for "empty"
/// and for "never loaded". A caller that chains on further clears that
/// memo and re-queries, which is what every chain method here does and
/// what Rails does to a loaded relation.
fn through_reader_body(
    name: &Symbol,
    target: &ClassId,
    join_sql: &str,
    where_sql: &str,
    hop_scopes: &[Expr],
    assoc_scope: &Option<Expr>,
) -> Expr {
    let span = Span::synthetic;
    let target_const = Expr::new(
        span(),
        ExprNode::Const {
            path: target.0.as_str().split("::").map(Symbol::from).collect(),
        },
    );
    let seed = Expr::new(
        span(),
        ExprNode::Send {
            recv: Some(Expr::new(
                span(),
                ExprNode::Const {
                    path: vec![Symbol::from("ActiveRecord"), Symbol::from("Relation")],
                },
            )),
            method: Symbol::from("new"),
            args: vec![target_const],
            block: None,
            parenthesized: true,
        },
    );
    let joined = Expr::new(
        span(),
        ExprNode::Send {
            recv: Some(seed),
            method: Symbol::from("joins"),
            args: vec![Expr::new(
                span(),
                ExprNode::Lit {
                    value: crate::expr::Literal::Str { value: join_sql.to_string() },
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    let chain = Expr::new(
        span(),
        ExprNode::Send {
            recv: Some(joined),
            method: Symbol::from("where"),
            args: vec![
                Expr::new(
                    span(),
                    ExprNode::Lit {
                        value: crate::expr::Literal::Str { value: where_sql.to_string() },
                    },
                ),
                Expr::new(span(), ExprNode::Ivar { name: Symbol::from("id") }),
            ],
            block: None,
            parenthesized: true,
        },
    );

    // Association scope lambda (`-> { where('votes.vote' => 1)... }`) —
    // graft its receiver-less chain onto the seeded relation so the
    // reader filters the way Rails does (without it /upvoted served
    // every joined row).
    let chain = hop_scopes.iter().fold(chain, |chain, scope| graft_chain_root(scope, chain));
    let chain = match assoc_scope {
        Some(scope_body) => graft_chain_root(scope_body, chain),
        None => chain,
    };

    // Last in the chain, after any scope graft: the scope's conditions
    // belong to the QUERY, and this hands the finished relation the
    // records an eager load already fetched.
    Expr::new(
        span(),
        ExprNode::Send {
            recv: Some(chain),
            method: Symbol::from("preloaded"),
            args: vec![
                Expr::new(
                    span(),
                    ExprNode::Ivar { name: Symbol::from(format!("{}_cache", name.as_str())) },
                ),
                Expr::new(
                    span(),
                    ExprNode::Ivar { name: Symbol::from(format!("{}_loaded", name.as_str())) },
                ),
            ],
            block: None,
            parenthesized: true,
        },
    )
}

/// A through hop's scope that runs the same on the target's relation:
/// a chain of calls whose arguments hold no hash (a hash condition is
/// qualified by the table the scope was declared on) and no local.
fn through_scope_graftable(scope: &Expr) -> bool {
    fn chain_ok(e: &Expr) -> bool {
        match &*e.node {
            ExprNode::Send { recv, args, block: None, .. } => {
                args.iter().all(|a| !matches!(&*a.node, ExprNode::Hash { .. }))
                    && recv.as_ref().is_none_or(|r| chain_ok(r))
            }
            _ => false,
        }
    }
    chain_ok(scope) && !reads_local(scope)
}

/// Replace the receiver-less root of a `where(...).order(...)` chain
/// with `seed`, turning an association-scope lambda body into a call
/// chain on the seeded relation. Non-chain shapes (a Seq, a literal)
/// return the seed untouched — better an unfiltered relation than a
/// mis-grafted one.
fn graft_chain_root(chain: &Expr, seed: Expr) -> Expr {
    match &*chain.node {
        ExprNode::Send { recv: Some(r), method, args, block, parenthesized } => {
            let new_recv = graft_chain_root(r, seed);
            Expr::new(
                chain.span,
                ExprNode::Send {
                    recv: Some(new_recv),
                    method: method.clone(),
                    args: args.clone(),
                    block: block.clone(),
                    parenthesized: *parenthesized,
                },
            )
        }
        ExprNode::Send { recv: None, method, args, block, parenthesized } => Expr::new(
            chain.span,
            ExprNode::Send {
                recv: Some(seed),
                method: method.clone(),
                args: args.clone(),
                block: block.clone(),
                parenthesized: *parenthesized,
            },
        ),
        _ => seed,
    }
}

/// Ruby-family pre-emit pass: resolve `app/helpers/*` references. Rails
/// mixes every helper module into every view as instance methods, but the
/// post-lowering IR emits views/controllers/helpers as module-functions
/// (`Views::Stories.listdetail`, `ApplicationHelper.avatar_img`), so a bare
/// `avatar_img(...)` rendered into a view body has no `self` to dispatch on
/// and raises `NoMethodError`. This pass (a) flips each helper module's own
/// methods to class methods so `ApplicationHelper.avatar_img` is a real
/// call target, and (b) rewrites every bare call whose name the helper
/// registry knows — in whatever LibraryClass bodies it's run over — into
/// `<DefiningModule>.method(...)`. Lives on the Ruby emit path: helper
/// modules are app-specific and the rewrite targets a CRuby call shape, so
/// shared `lower/` stays target-agnostic (same rule as scope lowering). A
/// strict no-op when the app has no non-empty helpers — the blog's helper
/// modules are empty, so `helper_method_index` is empty.
pub(crate) fn apply_helper_lowering(lcs: &mut [LibraryClass], app: &App) {
    // NO early return on an empty `helper_method_index`. This pass does
    // two jobs and only one of them is about the app's own helpers: the
    // other resolves bare FRAMEWORK helper calls (`image_tag`,
    // `sanitize`, `dom_id`) to `ActionView::ViewHelpers.<name>`, and an
    // app with no `app/helpers/` directory needs that just as much. The
    // guard that used to stand here made the qualification depend on
    // whether some unrelated file existed.
    let helper_modules: BTreeSet<ClassId> =
        app.helper_method_index.values().cloned().collect();
    // Generated route-helper names (`active_path`, `story_path`, …) —
    // bare calls to these in layout/helper bodies resolve to the
    // generated `RouteHelpers` module. (The view walker rewrites route
    // helpers in the URL positions it classifies; bare calls nested in
    // unclassified expressions fall through to this pass.)
    let route_helpers: crate::det::HashSet<Symbol> =
        crate::lower::lower_routes_to_library_functions(app)
            .into_iter()
            .map(|f| f.name)
            .collect();
    // App classes that `include Rails.application.routes.url_helpers`
    // (ingest records the marker as an include of RouteHelpers —
    // lobsters' Routes). Explicit `X.<helper>` call sites anywhere in
    // the tree rewrite through RouteHelpers below. Sourced from
    // `app.library_classes`, not the local `lcs` slice — this pass
    // also runs over the lowered-models stack, whose slice doesn't
    // contain the including class itself.
    let url_helper_classes: crate::det::HashSet<Symbol> = app
        .library_classes
        .iter()
        .filter(|lc| lc.includes.iter().any(|i| i.0.as_str() == "RouteHelpers"))
        .map(|lc| lc.name.0.clone())
        .collect();
    let helper_ivars = helper_read_ivars(app);
    let view_ivars = view_assigned_ivars(app);
    let no_helpers: HashMap<Symbol, ClassId> = HashMap::default();
    let renames = helper_instance_renames(app);
    let reach = HelperReach::new(lcs, app, &helper_modules);
    // The declarations of each class declared more than once (a
    // reopening), for the own-method shadowing below.
    let mut declarations: crate::det::HashMap<&str, Vec<&LibraryClass>> = crate::det::HashMap::default();
    for other in &app.library_classes {
        declarations.entry(other.name.0.as_str()).or_default().push(other);
    }
    declarations.retain(|_, all| all.len() > 1);
    for lc in lcs.iter_mut() {
        // CONTROLLERS in the index provide `helper_method`s to views:
        // only the call-site rewrite applies to them — their methods
        // must NOT flip class-side wholesale (actions are instance
        // methods; the class-side clone for the marked helpers comes
        // from the controller lowering itself).
        let is_controller = app.controllers.iter().any(|c| c.name == lc.name);
        let is_helper_module = helper_modules.contains(&lc.name) && !is_controller;
        // Helper and view module functions have no controller context —
        // a bare `request` read there resolves to the per-dispatch
        // `ActionController::Current.request` (controllers keep their
        // own `request` accessor and are left alone).
        let rewrite_request =
            is_helper_module || lc.name.0.as_str().starts_with("Views::");
        // WHICH app helpers a bare call here can reach, as Rails decides
        // it: a template sees every helper (`helper :all`), and so does a
        // helper body, which runs on that same view object. Any other
        // class sees only the helper modules it actually `include`s — a
        // model's `format("%06i", n)` is Kernel#format whatever some
        // helper happens to define. The pass runs over every emitted
        // class, models included, so without this the flat view index
        // captured names app-wide.
        let sees_all_helpers = is_helper_module || lc.name.0.as_str().starts_with("Views::");
        let included_index: HashMap<Symbol, ClassId>;
        let class_index: &HashMap<Symbol, ClassId> = if sees_all_helpers {
            &app.helper_method_index
        } else {
            let reachable = reach.helpers_for(lc);
            if reachable.is_empty() {
                &no_helpers
            } else {
                included_index = app
                    .helper_method_index
                    .iter()
                    .filter(|(_, owner)| reachable.contains(*owner))
                    .map(|(name, owner)| (name.clone(), owner.clone()))
                    .collect();
                &included_index
            }
        };
        // A helper module has two method namespaces the flip below merges:
        // its instance surface (what the view calls, including
        // `module_function` / `extend self` copies) and its own singleton
        // (`def self.x`, `class << self`). Rails resolves a bare call in a
        // body on that body's own side, so each side shadows the index
        // only with its own names.
        let singletons = app
            .helper_singleton_methods
            .get(&lc.name)
            .filter(|_| is_helper_module);
        let is_singleton_side = |m: &MethodDef| {
            m.receiver == MethodReceiver::Class
                && singletons.is_some_and(|s| s.contains(&m.name))
        };
        // The class's own method names shadow the helper index inside its
        // bodies — a bare call to one of them is a self-dispatch, never a
        // cross-module helper reference. Per SIDE: an instance body's bare
        // call never reaches a class method (a Concern fold's class-side
        // copy on an includer does not hide the helper it included), and
        // a class body's never reaches an instance method.
        //
        // In a helper module the instance side also holds the
        // `module_function` / `extend self` copies (Class receivers that
        // are not singleton-defined), and loses the names whose flipped
        // copy is renamed below: the bare call has to go to that copy, so
        // the index rewrite takes it.
        let mut own_instance_side: crate::det::HashSet<Symbol> = lc
            .methods
            .iter()
            .filter(|m| {
                m.receiver == MethodReceiver::Instance
                    || (is_helper_module && !is_singleton_side(m))
            })
            .filter(|m| !renames.contains_key(&(lc.name.clone(), m.name.clone())))
            .map(|m| m.name.clone())
            .collect();
        let mut own_class_side: crate::det::HashSet<Symbol> = lc
            .methods
            .iter()
            .filter(|m| m.receiver == MethodReceiver::Class)
            .map(|m| m.name.clone())
            .collect();
        // A reopening is the same class: what its other declarations
        // define is self-dispatch here too. Core's adaptive_pricing/
        // breakdown.rb reopens `Breakdown = Data.define(...)` with a
        // `class Breakdown` whose bare `h` is the member reader the
        // factory class defines, not the view helper `h`.
        if !is_helper_module {
            for other in declarations.get(lc.name.0.as_str()).into_iter().flatten() {
                for m in &other.methods {
                    match m.receiver {
                        MethodReceiver::Instance => own_instance_side.insert(m.name.clone()),
                        MethodReceiver::Class => own_class_side.insert(m.name.clone()),
                    };
                }
            }
        }
        for m in &mut lc.methods {
            // A singleton method body runs on the module, never on the
            // view: it reaches no helper bare, and none of this pass's
            // view-context rewrites apply to it.
            let singleton_side = is_singleton_side(m);
            let instance_side = m.receiver == MethodReceiver::Instance
                || (is_helper_module && !singleton_side);
            // A helper module's own methods become module-functions so the
            // rewritten `Module.method` call has a real target — Rails mixed
            // them into a view instance, but the emitted views are module
            // functions with no instance to receive them.
            if is_helper_module && m.receiver == MethodReceiver::Instance {
                m.receiver = MethodReceiver::Class;
                // The flip lands the instance method in the singleton's
                // namespace; when the module ALSO defines that name on its
                // singleton (Core's MoneyHelper keeps both a
                // `class << self` and an instance `money_with_symbol`,
                // the second delegating to the first), the later `def
                // self.` would replace the singleton and recurse into
                // itself. The flipped copy takes a name of its own, and
                // every bare call the index binds to it follows.
                if let Some(renamed) = renames.get(&(lc.name.clone(), m.name.clone())) {
                    m.name = renamed.clone();
                }
            }
            // Ground `…url_helpers.<x>_url(record?, host:)` → absolute-URL
            // interpolation BEFORE `rewrite_helper_calls` collapses the
            // `Rails.application.routes.url_helpers` chain to `RouteHelpers`
            // (that collapse is children-first, so it would erase the chain
            // shape this grounding matches on). mod_note's `user_url` lives
            // in a model body, reached by this same pass.
            rewrite_url_helpers_absolute(&mut m.body);
            // The method's OWN params shadow the helper index too: a bare
            // `tag` in a partial whose strict-locals header declares `tag:`
            // is that local, not `ApplicationHelper.tag`. (Rails mixes
            // helpers BENEATH a template's locals.) Passed SEPARATELY from
            // own_methods so the arity guard (bare-reference only) applies
            // to params but not to real self-dispatched methods.
            let own_params: crate::det::HashSet<Symbol> =
                m.params.iter().map(|p| p.name.clone()).collect();
            if is_controller && !own_instance_side.contains(&Symbol::from("helpers")) {
                rewrite_controller_helpers_proxy(&mut m.body, &app.helper_method_index);
            }
            rewrite_helper_calls(
                &mut m.body,
                if singleton_side { &no_helpers } else { class_index },
                &renames,
                &route_helpers,
                &url_helper_classes,
                rewrite_request && !singleton_side,
                if instance_side { &own_instance_side } else { &own_class_side },
                &own_params,
                &app.view_visible_controller_methods,
            );
            // What the flip cannot keep apart by renaming: a bare call in
            // an instance-side body to a name only the SINGLETON defines.
            // Rails resolves it on the view object, where the singleton
            // is invisible; on the flipped `def self.` it would land on
            // the singleton (MoneyHelper's private two-keyword `format`
            // answering a `format("%.2f", m)`). Kernel's module functions
            // are the names that reach this point and have a qualified
            // spelling with the same meaning.
            if instance_side {
                if let Some(singletons) = singletons {
                    let captured: crate::det::HashSet<Symbol> = singletons
                        .iter()
                        .filter(|n| !own_instance_side.contains(*n))
                        .filter(|n| is_kernel_module_function(n.as_str()))
                        .cloned()
                        .collect();
                    if !captured.is_empty() {
                        qualify_kernel_calls(&mut m.body, &captured);
                    }
                }
            }
            // A CONTROLLER IVAR read in a helper body. Rails mixes
            // helpers into the view instance, which carries the
            // controller's assigns, so `@room` there IS the
            // controller's — campfire's `RoomsHelper.link_to_edit_room`
            // reads it for the edit link's style and data attributes,
            // and lobsters' `StoriesHelper` reads `@user`/`@ribbon` the
            // same way. A module function has no instance, so the read
            // was a bare nil and every `rooms#show` died on
            // `undefined method 'id' for nil`.
            //
            // Routed through the same per-dispatch seam `flash` /
            // `cookies` / a `helper_method` already use, which is the
            // one place a module function can reach the live
            // controller.
            if is_helper_module && !singleton_side {
                rewrite_helper_ivars(&mut m.body, &helper_ivars);
            }
            if lc.name.0.as_str().starts_with("Views::") {
                rewrite_view_ivar_writes(&mut m.body, &view_ivars);
            }
        }
        // The other half: the reader the rewrite dispatches to. It goes
        // on the BASE controller, so every controller inherits one —
        // a helper runs under whichever controller is current, and
        // Rails' answer for an assign that controller never made is
        // nil, which is exactly what reading an unassigned ivar gives.
        if is_base_controller(lc) {
            push_helper_ivar_readers(&mut lc.methods, &helper_ivars, &lc.name);
            // The name a template WRITES needs a reader too, not only
            // the setter: `page_title_tag` reads what `rooms/show`
            // set, and a name no helper happens to read still has to
            // answer nil rather than NoMethodError.
            push_helper_ivar_readers(&mut lc.methods, &view_ivars, &lc.name);
            push_helper_ivar_writers(&mut lc.methods, &view_ivars, &lc.name);
        }
    }
    // LAST, because it matches the qualified call shape the rewrite above
    // produces — and over the whole slice at once, because constant NAMES
    // have to be unique across every class that shares a Ruby namespace.
    hoist_pure_helper_constants(lcs);
}

/// The flipped instance methods of a helper module that would collide
/// with a method the module defines on its own singleton, keyed by
/// (module, name), with the name the flipped copy takes instead:
/// `money_with_symbol` -> `money_with_symbol__instance`, `html?` ->
/// `html__instance?`. Read off the App, not the pass's slice, so every
/// emitted stack (views, controllers, models) agrees on the name.
fn helper_instance_renames(app: &App) -> std::collections::BTreeMap<(ClassId, Symbol), Symbol> {
    let mut out = std::collections::BTreeMap::new();
    for lc in &app.library_classes {
        let Some(singletons) = app.helper_singleton_methods.get(&lc.name) else { continue };
        if app.controllers.iter().any(|c| c.name == lc.name) {
            continue;
        }
        for m in &lc.methods {
            if m.receiver != MethodReceiver::Instance || !singletons.contains(&m.name) {
                continue;
            }
            let name = m.name.as_str();
            let (stem, suffix) = match name.char_indices().last() {
                Some((i, '?' | '!' | '=')) => name.split_at(i),
                _ => (name, ""),
            };
            out.insert(
                (lc.name.clone(), m.name.clone()),
                Symbol::from(format!("{stem}__instance{suffix}")),
            );
        }
    }
    out
}

/// Kernel's module functions whose `Kernel.<name>(…)` spelling means what
/// the bare call means anywhere (frame-reading ones such as
/// `block_given?`, `binding` and `__method__` are deliberately absent).
fn is_kernel_module_function(name: &str) -> bool {
    matches!(
        name,
        "Array" | "Complex" | "Float" | "Hash" | "Integer" | "Rational" | "String"
            | "abort" | "catch" | "exit" | "exit!" | "fail" | "format" | "gets" | "loop"
            | "p" | "print" | "printf" | "putc" | "puts" | "raise" | "rand" | "readline"
            | "readlines" | "sleep" | "sprintf" | "srand" | "system" | "spawn" | "throw"
            | "warn"
    )
}

/// `format(…)` -> `Kernel.format(…)` for the receiver-less calls named.
fn qualify_kernel_calls(e: &mut Expr, names: &crate::det::HashSet<Symbol>) {
    e.node.for_each_child_mut(&mut |c| qualify_kernel_calls(c, names));
    let span = e.span;
    if let ExprNode::Send { recv: recv @ None, method, .. } = &mut *e.node {
        if names.contains(method) {
            *recv = Some(Expr::new(
                span,
                ExprNode::Const { path: vec![Symbol::from("Kernel")] },
            ));
        }
    }
}

/// Which helper modules a class's bare calls can reach outside the view.
///
/// A class reaches the helpers it `include`s, transitively (a concern
/// that includes a helper hands it on). A MODULE's body also runs as each
/// of its includers, so a concern sees every helper its includers do:
/// `def label; money_label(price); end` in a concern of a model that
/// includes MoneyHelper is that helper's method at run time.
///
/// Built from the App (library classes and models) plus the pass's own
/// slice, whose synthesized classes the App does not hold.
struct HelperReach {
    direct: HashMap<Symbol, BTreeSet<ClassId>>,
    via_includers: HashMap<Symbol, BTreeSet<ClassId>>,
}

impl HelperReach {
    fn new(lcs: &[LibraryClass], app: &App, helpers: &BTreeSet<ClassId>) -> Self {
        if helpers.is_empty() {
            return HelperReach { direct: HashMap::default(), via_includers: HashMap::default() };
        }
        let mut includes: std::collections::BTreeMap<ClassId, Vec<ClassId>> =
            std::collections::BTreeMap::new();
        for lc in app.library_classes.iter().chain(lcs.iter()) {
            if !lc.includes.is_empty() {
                includes.entry(lc.name.clone()).or_default().extend(lc.includes.iter().cloned());
            }
        }
        for model in &app.models {
            let incs = crate::analyze::model_includes(model);
            if !incs.is_empty() {
                includes.entry(model.name.clone()).or_default().extend(incs);
            }
        }
        let closure = |start: &ClassId| -> BTreeSet<ClassId> {
            let mut seen = BTreeSet::new();
            let mut pending: Vec<ClassId> = includes.get(start).cloned().unwrap_or_default();
            while let Some(next) = pending.pop() {
                if seen.insert(next.clone()) {
                    pending.extend(includes.get(&next).into_iter().flatten().cloned());
                }
            }
            seen
        };
        let mut direct: HashMap<Symbol, BTreeSet<ClassId>> = HashMap::default();
        let mut via_includers: HashMap<Symbol, BTreeSet<ClassId>> = HashMap::default();
        for class in includes.keys() {
            let reached = closure(class);
            let reached_helpers: BTreeSet<ClassId> =
                reached.iter().filter(|m| helpers.contains(*m)).cloned().collect();
            if reached_helpers.is_empty() {
                continue;
            }
            for module in &reached {
                via_includers
                    .entry(module.0.clone())
                    .or_default()
                    .extend(reached_helpers.iter().cloned());
            }
            direct.insert(class.0.clone(), reached_helpers);
        }
        HelperReach { direct, via_includers }
    }

    fn helpers_for(&self, lc: &LibraryClass) -> BTreeSet<ClassId> {
        let mut out = self.direct.get(&lc.name.0).cloned().unwrap_or_default();
        if lc.is_module {
            out.extend(self.via_includers.get(&lc.name.0).into_iter().flatten().cloned());
        }
        out
    }
}

/// Is this the app's base controller — the one every other controller
/// inherits from (`class ApplicationController < ActionController::Base`)?
fn is_base_controller(lc: &LibraryClass) -> bool {
    lc.parent.as_ref().is_some_and(|p| p.0.as_str() == "ActionController::Base")
}

/// The controller ivars helper-module bodies read.
///
/// Helper modules only — a `Views::` module's ivars were already bound
/// to locals by the view lowering's closure threading, and re-routing
/// one here would silently prefer a stale controller read over the
/// value the caller passed.
///
/// A name any controller already DEFINES as a method is excluded: the
/// reader this pass synthesizes would collide with it, and the app's
/// own method is the one that should win.
fn helper_read_ivars(app: &App) -> std::collections::BTreeSet<Symbol> {
    let helper_modules: BTreeSet<ClassId> = app
        .helper_method_index
        .values()
        .filter(|id| !app.controllers.iter().any(|c| &c.name == *id))
        .cloned()
        .collect();
    let mut out = std::collections::BTreeSet::new();
    for lc in &app.library_classes {
        if !helper_modules.contains(&lc.name) {
            continue;
        }
        for m in &lc.methods {
            collect_ivar_reads(&m.body, &mut out);
        }
    }
    let controller_methods: crate::det::HashSet<Symbol> = app
        .controllers
        .iter()
        .flat_map(|c| c.body.iter())
        .filter_map(|item| match item {
            crate::dialect::ControllerBodyItem::Action { action, .. } => {
                Some(action.name.clone())
            }
            _ => None,
        })
        .collect();
    out.retain(|n| !controller_methods.contains(n));
    out
}

fn collect_ivar_reads(e: &Expr, out: &mut std::collections::BTreeSet<Symbol>) {
    if let ExprNode::Ivar { name } = &*e.node {
        out.insert(name.clone());
    }
    e.node.for_each_child(&mut |c| collect_ivar_reads(c, out));
}

/// `@room` -> `ActionController::Current.controller.room`, for the
/// names `helper_read_ivars` collected.
fn rewrite_helper_ivars(e: &mut Expr, names: &std::collections::BTreeSet<Symbol>) {
    e.node.for_each_child_mut(&mut |c| rewrite_helper_ivars(c, names));
    let name = match &*e.node {
        ExprNode::Ivar { name } if names.contains(name) => name.clone(),
        _ => return,
    };
    let span = e.span;
    let controller = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(
                span,
                ExprNode::Const {
                    path: vec![Symbol::from("ActionController"), Symbol::from("Current")],
                },
            )),
            method: Symbol::from("controller"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    *e.node = ExprNode::Send {
        recv: Some(controller),
        method: name,
        args: vec![],
        block: None,
        parenthesized: false,
    };
}

/// The ivars a `Views::` module ASSIGNS.
///
/// Rails' `@page_title = …` in a template writes the view context the
/// layout and every mixed-in helper share, so the write has to land
/// where the helper-side READ already looks — on the live controller.
/// The view lowering leaves the assign as an Ivar precisely so this
/// pass can see it; a helper module's ivar is a controller assign it
/// only reads, which is `helper_read_ivars`' half of the same seam.
///
/// Read off `app.views` — the TEMPLATE bodies — not off the lowered
/// `Views::` classes: `apply_helper_lowering` runs once per emitted
/// stack, and the stack holding the views is not the one holding the
/// base controller that has to grow the accessors. Sourced from the
/// App, both halves see the same names.
fn view_assigned_ivars(app: &App) -> std::collections::BTreeSet<Symbol> {
    let mut out = std::collections::BTreeSet::new();
    for view in &app.views {
        collect_ivar_writes(&view.body, &mut out);
    }
    out
}

fn collect_ivar_writes(e: &Expr, out: &mut std::collections::BTreeSet<Symbol>) {
    match &*e.node {
        ExprNode::Assign { target: LValue::Ivar { name }, .. }
        | ExprNode::OpAssign { target: LValue::Ivar { name }, .. } => {
            out.insert(name.clone());
        }
        _ => {}
    }
    e.node.for_each_child(&mut |c| collect_ivar_writes(c, out));
}

/// `@page_title = x` -> `ActionController::Current.controller.page_title = x`,
/// the write half of the seam `rewrite_helper_ivars` reads through.
fn rewrite_view_ivar_writes(e: &mut Expr, names: &std::collections::BTreeSet<Symbol>) {
    e.node.for_each_child_mut(&mut |c| rewrite_view_ivar_writes(c, names));
    let (name, value) = match &*e.node {
        ExprNode::Assign { target: LValue::Ivar { name }, value } if names.contains(name) => {
            (name.clone(), value.clone())
        }
        _ => return,
    };
    let span = e.span;
    *e.node = ExprNode::Assign {
        target: LValue::Attr { recv: current_controller_expr(span), name },
        value,
    };
}

/// The `ActionController::Current.controller` receiver both halves of
/// the seam dispatch through.
fn current_controller_expr(span: Span) -> Expr {
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(
                span,
                ExprNode::Const {
                    path: vec![Symbol::from("ActionController"), Symbol::from("Current")],
                },
            )),
            method: Symbol::from("controller"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    )
}

/// `def page_title=(v); @page_title = v; end` on the base controller,
/// the target of `rewrite_view_ivar_writes`. Paired with the reader
/// `push_helper_ivar_readers` pushes for the same name — a title a
/// template sets and the layout's helper reads travels through both.
fn push_helper_ivar_writers(
    methods: &mut Vec<MethodDef>,
    names: &std::collections::BTreeSet<Symbol>,
    class_name: &ClassId,
) {
    for name in names {
        let setter = Symbol::from(format!("{}=", name.as_str()));
        if methods.iter().any(|m| m.name == setter) {
            continue;
        }
        let span = Span::synthetic();
        methods.push(MethodDef {
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: setter,
            receiver: MethodReceiver::Instance,
            visibility: MethodVisibility::Public,
            params: vec![crate::dialect::Param::positional(Symbol::from("value"))],
            body: Expr::new(
                span,
                ExprNode::Assign {
                    target: LValue::Ivar { name: name.clone() },
                    value: Expr::new(
                        span,
                        ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from("value") },
                    ),
                },
            ),
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(class_name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: true,
            block_param: None,
        });
    }
}

/// `def room; @room; end` on the base controller, one per name.
fn push_helper_ivar_readers(
    methods: &mut Vec<MethodDef>,
    names: &std::collections::BTreeSet<Symbol>,
    class_name: &ClassId,
) {
    for name in names {
        if methods.iter().any(|m| &m.name == name) {
            continue;
        }
        methods.push(MethodDef {
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: name.clone(),
            receiver: MethodReceiver::Instance,
            visibility: MethodVisibility::Public,
            params: vec![],
            body: Expr::new(Span::synthetic(), ExprNode::Ivar { name: name.clone() }),
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(class_name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// Framework view helpers a bare call resolves to as
/// `ActionView::ViewHelpers.<name>`. The list lives in
/// `crate::view_helpers`, beside the include decision that relies on it.
fn is_framework_view_helper(name: &str) -> bool {
    crate::view_helpers::emit_qualifies(name)
}

/// Is this a call on `ActionView::ViewHelpers` (or a bare `ViewHelpers`
/// const)? Shared by the post-rewrite transforms below.
fn is_view_helpers_const(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Const { path }
        if path.last().map(|s| s.as_str() == "ViewHelpers").unwrap_or(false))
}

/// View helpers a call site can be hoisted OUT of, when every argument is
/// a literal: each is a pure function of its arguments and always returns
/// a freshly built String.
///
/// PURITY IS DECLARED HERE, NOT INFERRED, and the list is grown by
/// measurement rather than by reasoning about what looks pure. Two traps
/// make inference the wrong tool:
///
///   - `csrf_token_hidden_input` takes NO arguments, so "every argument is
///     a literal" is vacuously true of it. Hoisting it would compute one
///     session's token at load and serve it to every reader afterwards.
///     Arity is not evidence of purity.
///   - An empty `EffectSet` on the call is not evidence either: it means
///     the analyzer did not model the callee, not that the callee is pure.
///
/// The "freshly built String" half matters as much as the purity half.
/// `image_path("/already/absolute")` RETURNS ITS ARGUMENT, so hoisting it
/// would put a literal's own String object in a constant where a caller
/// could mutate it; the three below all interpolate into a new String.
///
/// `stylesheet_link_tag` / `javascript_include_tag` map a literal name to
/// an undigested `/assets/` path (the `image_path` note explains why no
/// digests), so they are pure over their literals the way `image_tag`
/// is; `javascript_importmap_tags` is pure over the importmap table,
/// which is the one non-literal argument this list admits (see
/// `HOISTABLE_CONST_READERS`). campfire's layout pays all three on every
/// page — 27 stylesheet links, and the importmap's 92 pins rebuilt as
/// Hashes and rendered to JSON — for a header that never changes
/// between one boot and the next: about 360 allocations per request
/// that the room page's profile put beside the message loop's.
const HOISTABLE_TAG_HELPERS: &[&str] = &[
    "image_tag",
    "content_tag",
    "hidden_field_tag",
    "stylesheet_link_tag",
    "javascript_include_tag",
    "javascript_importmap_tags",
];

/// Argument-less reads of emitted TABLES that a hoistable call may take
/// as an argument and still be a per-process constant: `Importmap.pins`
/// and `Importmap.entry` are `config/importmap.rb` rendered as methods
/// answering literals (see `emit_importmap`), so a call over them is as
/// constant as a call over the literals themselves. Declared, like the
/// helpers: a reader is on this list because its body was read, not
/// because it takes no arguments.
const HOISTABLE_CONST_READERS: &[(&str, &str)] = &[("Importmap", "pins"), ("Importmap", "entry")];

/// A canonical spelling of an all-literal expression — `None` the moment
/// any part of it is not a literal. Doubles as the dedupe key, so two call
/// sites spelled the same way share one constant, and as the guard that
/// decides whether a call is hoistable at all.
fn literal_key(e: &Expr) -> Option<String> {
    match &*e.node {
        ExprNode::Lit { value } => Some(match value {
            Literal::Nil => "nil".to_string(),
            Literal::Bool { value } => format!("b:{value}"),
            Literal::Int { value } => format!("i:{value}"),
            Literal::BigInt { value } => format!("i:{value}"),
            Literal::Float { value } => format!("f:{value}"),
            Literal::Str { value } => format!("s:{value:?}"),
            Literal::Sym { value } => format!("y:{}", value.as_str()),
            Literal::Regex { pattern, flags } => format!("r:{pattern:?}/{flags}"),
        }),
        ExprNode::Hash { entries, kwargs } => {
            let mut out = String::from(if *kwargs { "kw{" } else { "h{" });
            for (k, v) in entries {
                out.push_str(&literal_key(k)?);
                out.push_str("=>");
                out.push_str(&literal_key(v)?);
                out.push(',');
            }
            out.push('}');
            Some(out)
        }
        ExprNode::Array { elements, .. } => {
            let mut out = String::from("[");
            for el in elements {
                out.push_str(&literal_key(el)?);
                out.push(',');
            }
            out.push(']');
            Some(out)
        }
        // An already-hoisted constant is a literal for the purposes of
        // the call that takes it (a `+` chain over hoisted tags, below).
        ExprNode::Const { path } if path.len() == 1 && path[0].as_str().starts_with("HOISTED_") => {
            Some(format!("c:{}", path[0].as_str()))
        }
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } if args.is_empty() => {
            let ExprNode::Const { path } = &*r.node else { return None };
            let [konst] = path.as_slice() else { return None };
            HOISTABLE_CONST_READERS
                .iter()
                .any(|(k, m)| *k == konst.as_str() && *m == method.as_str())
                .then(|| format!("{}.{}", konst.as_str(), method.as_str()))
        }
        _ => None,
    }
}

/// A `+` chain whose every leaf is a hoisted constant or a String
/// literal — the layout's `stylesheet_link_tag(…) + "\n" +
/// stylesheet_link_tag(…) + …` once each call is a constant. String `+`
/// is pure and answers a fresh String, so the WHOLE chain is one more
/// constant (its key is the chain's spelling); without this the tags
/// would be hoisted and the 27 concatenations still run per request.
/// Answers the key for a chain, `None` for anything else — including a
/// bare leaf, which is not a chain.
fn hoistable_concat_key(e: &Expr) -> Option<String> {
    fn leaf_or_chain(x: &Expr) -> Option<String> {
        match &*x.node {
            ExprNode::Lit { value: Literal::Str { .. } } => literal_key(x),
            ExprNode::Const { path } if path.len() == 1 && path[0].as_str().starts_with("HOISTED_") => {
                literal_key(x)
            }
            _ => hoistable_concat_key(x),
        }
    }
    let ExprNode::Send { recv: Some(l), method, args, block: None, .. } = &*e.node else {
        return None;
    };
    if method.as_str() != "+" || args.len() != 1 {
        return None;
    }
    Some(format!("plus({},{})", leaf_or_chain(l)?, leaf_or_chain(&args[0])?))
}

/// The dedupe key for a hoistable call, or `None` if this expression is
/// not one: `ActionView::ViewHelpers.<helper>(<literals>)`, no block.
fn hoistable_call_key(e: &Expr) -> Option<String> {
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*e.node else {
        return None;
    };
    if !is_view_helpers_const(r) || !HOISTABLE_TAG_HELPERS.contains(&method.as_str()) {
        return None;
    }
    let mut key = format!("{}(", method.as_str());
    for a in args {
        key.push_str(&literal_key(a)?);
        key.push(',');
    }
    key.push(')');
    Some(key)
}

/// `image_tag("boost.svg", …)` -> `HOISTED_IMAGE_TAG_BOOST_SVG`. The first
/// String argument names it, because that is what a reader of the emitted
/// view is looking for; a suffix disambiguates two calls that differ only
/// in their options.
fn hoisted_const_name(
    method: &str,
    args: &[Expr],
    used: &mut crate::det::HashSet<String>,
) -> Symbol {
    let mut base = format!("HOISTED_{}", method.to_uppercase());
    if let Some(first) = args.iter().find_map(|a| match &*a.node {
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
        _ => None,
    }) {
        let slug: String = first
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect();
        let slug = slug.trim_matches('_').to_string();
        if !slug.is_empty() {
            base = format!("{base}_{slug}");
        }
    }
    let mut name = base.clone();
    let mut n = 2;
    while !used.insert(name.clone()) {
        name = format!("{base}_{n}");
        n += 1;
    }
    Symbol::from(name.as_str())
}

/// Replace hoistable calls in one expression tree with references to
/// class constants, minting a constant the first time each distinct call
/// is seen. Children first, so a nested call is hoisted before its parent
/// is examined (a parent that contains one is not itself all-literal, so
/// the two can never both fire on the same node).
fn hoist_in_expr(
    e: &mut Expr,
    minted: &mut crate::det::HashMap<String, Symbol>,
    order: &mut Vec<(Symbol, Expr)>,
    used: &mut crate::det::HashSet<String>,
) {
    hoist_in_expr_at(e, false, minted, order, used)
}

/// `in_concat`: this node is an operand of a `+` the parent will fold
/// if the whole chain qualifies — so a qualifying `+` here is left for
/// the parent, and only the outermost `+` of a chain mints a constant.
fn hoist_in_expr_at(
    e: &mut Expr,
    in_concat: bool,
    minted: &mut crate::det::HashMap<String, Symbol>,
    order: &mut Vec<(Symbol, Expr)>,
    used: &mut crate::det::HashSet<String>,
) {
    let child_in_concat = matches!(&*e.node, ExprNode::Send { method, args, block: None, .. }
        if method.as_str() == "+" && args.len() == 1);
    e.node.for_each_child_mut(&mut |c| hoist_in_expr_at(c, child_in_concat, minted, order, used));
    let (key, label) = match hoistable_call_key(e) {
        Some(key) => (key, None),
        None => match hoistable_concat_key(e) {
            Some(key) => (key, Some("concat")),
            None => return,
        },
    };
    // A chain is hoisted at its TOP: a `+` whose parent is also a `+`
    // over the same kind of leaves is folded by the parent, so the
    // whole chain becomes one constant rather than one per operator.
    if label.is_some() && in_concat {
        return;
    }
    let name = match minted.get(&key) {
        Some(name) => name.clone(),
        None => {
            let ExprNode::Send { method, args, .. } = &*e.node else { unreachable!() };
            let name = match label {
                Some(label) => hoisted_const_name(label, &[], used),
                None => hoisted_const_name(method.as_str(), args, used),
            };
            minted.insert(key, name.clone());
            order.push((name.clone(), e.clone()));
            name
        }
    };
    let span = e.span;
    let ty = e.ty.clone();
    *e = Expr::new(span, ExprNode::Const { path: vec![name] });
    e.ty = ty;
}

/// Hoist a pure view-helper call whose arguments are all literals to a
/// class constant, so the string it always produces is built once at load
/// rather than on every call.
///
/// WHY THIS IS WORTH A PASS. campfire's `_actions.rb` calls `image_tag`
/// seven times per MESSAGE — about 200 times on a 40-message room page —
/// and each call allocates the options hash, the nested `aria:` hash,
/// another hash from `opts.to_h`, the materialised key/value pairs, then
/// an array and a string per attribute in `render_attrs`, to produce a
/// byte-identical `<img>` tag every time. Profiled on the spinel binary,
/// `image_tag` was 13.9% of the whole server's CPU on `/rooms/1`, and
/// `sp_PolyArray_new` was the single hottest leaf underneath it. Hoisting
/// those seven took the page from 206 to 224-238 req/s, byte-identical.
///
/// Hoisting rather than constant-FOLDING is deliberate: computing the
/// string in Rust here would make a second implementation of `image_tag`
/// that can drift from the runtime's, and the emitted constant keeps the
/// one implementation while paying it once per process.
///
/// NAMES ARE UNIQUE ACROSS THE WHOLE SLICE, not per class, and that is not
/// tidiness. campfire's `_actions.rb` and `_template.rb` are two
/// LibraryClasses that both reopen `Views::Messages`, and both ask for the
/// same icon — one with `size: 20` and one without. Named per class they
/// both minted `HOISTED_IMAGE_TAG_MENU_DOTS_HORIZONTAL_SVG`, whichever
/// file loaded second silently won, and every message rendered its
/// options menu with an `<img>` that had lost its `width` and `height`.
/// `scripts/campfire-compare` caught it against Rails; nothing else would
/// have. Dedupe stays PER CLASS (a class must define what it references,
/// since sibling files of one namespace have no load-order contract), so
/// two files hoisting the identical call get two constants with equal
/// values — redundant, and the only alternative that is safe.
fn hoist_pure_helper_constants(lcs: &mut [LibraryClass]) {
    // Seeded from every class, so a hoisted name can never collide with an
    // app constant defined in a sibling file of the same namespace either.
    let mut used: crate::det::HashSet<String> = lcs
        .iter()
        .flat_map(|lc| lc.constants.iter().map(|(n, _)| n.as_str().to_string()))
        .collect();
    for lc in lcs.iter_mut() {
        let mut minted: crate::det::HashMap<String, Symbol> =
            crate::det::HashMap::default();
        // Source order of first use, so the emitted constants read down the
        // file in the order the template reaches them.
        let mut order: Vec<(Symbol, Expr)> = Vec::new();
        for m in &mut lc.methods {
            hoist_in_expr(&mut m.body, &mut minted, &mut order, &mut used);
        }
        lc.constants.extend(order);
    }
}

/// Strip one trailing `.to_s` (the view walker's `coerce_to_s` wrap) so
/// the safe-call check sees the helper call itself.
fn strip_to_s(e: &Expr) -> &Expr {
    if let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*e.node {
        if method.as_str() == "to_s" && args.is_empty() {
            return r;
        }
    }
    e
}

/// Calls whose result Rails treats as an html_safe buffer: tag-producing
/// framework helpers, `raw`, and app-helper module functions (which
/// compose those). The view walker's default `html_escape(<call>.to_s)`
/// wrap must NOT apply to these — escaping a safe buffer ships literal
/// `&lt;img&gt;` markup. Plain-string helpers (truncate,
/// time_ago_in_words) stay wrapped: their escape is Rails-correct.
/// Treating every app-helper as safe is a simplification (Rails escapes
/// an app helper that returns a plain string); the corpus' helpers all
/// return tag-helper compositions, and per-method safety inference can
/// refine this when a counterexample shows up.
fn is_html_safe_call(e: &Expr, index: &HashMap<Symbol, ClassId>) -> bool {
    let ExprNode::Send { recv: Some(r), method, .. } = &*e.node else {
        return false;
    };
    let ExprNode::Const { path } = &*r.node else {
        return false;
    };
    let joined =
        path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
    if joined.ends_with("ViewHelpers") {
        return matches!(
            method.as_str(),
            "raw" | "link_to" | "link_to_if" | "link_to_raw" | "button_to" | "mail_to" | "image_tag" | "content_tag"
                | "javascript_include_tag" | "label_tag" | "submit_tag" | "form_tag" | "form_with"
                // The rest of FormTagHelper — the BUILDER-LESS field
                // tags, which return an element exactly the way
                // `label_tag` and `submit_tag` beside them do. Listed
                // as a set rather than one at a time because the
                // dividing line is not which one an app happened to
                // reach: a helper that returns an ELEMENT is safe, a
                // helper that returns TEXT (`truncate`, `dom_id`,
                // `time_ago_in_words`) is not, and every name here is
                // on the element side. `hidden_field_tag` is how the
                // omission surfaced: campfire's quick boosts carry
                // their emoji in one, eight per message, and the
                // escape shipped `&lt;input …&gt;` — 320 inputs Rails
                // renders and the room page did not, with the forms
                // around them all present and correct.
                | "hidden_field_tag"
                | "text_field_tag"
                | "password_field_tag"
                | "text_area_tag"
                | "check_box_tag"
                | "radio_button_tag"
                | "select_tag"
                | "button_tag"
                | "field_set_tag"
                | "file_field_tag"
                | "email_field_tag"
                | "number_field_tag"
                | "search_field_tag"
                | "telephone_field_tag"
                | "url_field_tag"
                | "date_field_tag"
                | "color_field_tag"
                // The `<option>` builders, same rule.
                | "options_for_select"
                | "options_from_collection_for_select"
                | "grouped_options_for_select"
                | "time_zone_options_for_select"
        );
    }
    index.values().any(|cid| cid.0.as_str() == joined)
}

/// `<Const ending in Base>.helpers` — the Rails idiom
/// `ActionController::Base.helpers.image_path(...)` used to reach view
/// helpers from a model. Collapses to the ViewHelpers module so the
/// trailing call resolves as `ActionView::ViewHelpers.image_path(...)`.
fn is_base_dot_helpers(node: &ExprNode) -> bool {
    if let ExprNode::Send { recv: Some(r), method, args, block, .. } = node {
        if method.as_str() == "helpers" && args.is_empty() && block.is_none() {
            if let ExprNode::Const { path } = &*r.node {
                return path.last().map(|s| s.as_str() == "Base").unwrap_or(false);
            }
        }
    }
    false
}

/// `Rails.application.routes.url_helpers` — the Rails idiom for reaching
/// route helpers from a model body (`Story#short_id_path` does
/// `...url_helpers.root_path + "s/#{short_id}"`). Collapses to the
/// generated `RouteHelpers` module so the trailing `.root_path` resolves
/// as `RouteHelpers.root_path`.
fn is_rails_url_helpers(node: &ExprNode) -> bool {
    let mut cur = node;
    for step in ["url_helpers", "routes", "application"] {
        let ExprNode::Send { recv: Some(r), method, args, block, .. } = cur else {
            return false;
        };
        if method.as_str() != step || !args.is_empty() || block.is_some() {
            return false;
        }
        cur = &r.node;
    }
    matches!(cur, ExprNode::Const { path }
        if path.last().map(|s| s.as_str() == "Rails").unwrap_or(false))
}

/// `helpers.<m>(args)` → `<AppHelper>.<m>(args)` when `index` names an
/// app helper defining `m`, else `ActionView::ViewHelpers.<m>(args)`.
/// Only the receiver-less `helpers` reader is matched.
fn qualify_helpers_proxy(expr: &mut Expr, index: &HashMap<Symbol, ClassId>) {
    let ExprNode::Send { recv: Some(r), method, .. } = &mut *expr.node else { return };
    let bare_helpers = matches!(&*r.node,
        ExprNode::Send { recv: None, method: h, args, block: None, .. }
            if h.as_str() == "helpers" && args.is_empty());
    if bare_helpers {
        let path = match index.get(method) {
            Some(module) => module.0.as_str().split("::").map(Symbol::from).collect(),
            None => view_helpers_path(),
        };
        *r.node = ExprNode::Const { path };
    }
}

/// `helpers.<m>(args)` throughout a controller body. Rails' controller
/// view context includes every app helper (`helper :all`), not only the
/// modules the controller itself includes, so the whole index answers.
fn rewrite_controller_helpers_proxy(expr: &mut Expr, index: &HashMap<Symbol, ClassId>) {
    expr.node
        .for_each_child_mut(&mut |c| rewrite_controller_helpers_proxy(c, index));
    qualify_helpers_proxy(expr, index);
}

fn view_helpers_path() -> Vec<Symbol> {
    vec![Symbol::from("ActionView"), Symbol::from("ViewHelpers")]
}

/// Walk a method body and rewrite helper calls so they resolve against the
/// module-function surfaces. Four cases (children first, so nested calls
/// and the `.helpers` receiver are rewritten before their parent):
///   1. `<…Base>.helpers` → the `ActionView::ViewHelpers` constant.
///   2. `Rails.application.routes.url_helpers` → the `RouteHelpers` constant.
///   3. bare `name(args)` where `name` is an app helper → `<Module>.name(args)`.
///   4. bare `name(args)` where `name` is a framework view helper →
///      `ActionView::ViewHelpers.name(args)`.
/// Only receiver-less Sends are rewritten in (3)/(4): a call with a receiver
/// already resolves, and a bare identifier with no call shape is a local read.
fn rewrite_helper_calls(
    expr: &mut Expr,
    index: &HashMap<Symbol, ClassId>,
    renames: &std::collections::BTreeMap<(ClassId, Symbol), Symbol>,
    route_helpers: &crate::det::HashSet<Symbol>,
    url_helper_classes: &crate::det::HashSet<Symbol>,
    rewrite_request: bool,
    own_methods: &crate::det::HashSet<Symbol>,
    own_params: &crate::det::HashSet<Symbol>,
    view_visible: &std::collections::BTreeSet<Symbol>,
) {
    expr.node.for_each_child_mut(&mut |c| {
        rewrite_helper_calls(
            c,
            index,
            renames,
            route_helpers,
            url_helper_classes,
            rewrite_request,
            own_methods,
            own_params,
            view_visible,
        )
    });

    // Destructive `<lvalue>.<m>!(args)` on a frozen string raises under
    // spinel (frozen literals) — lobsters' link_to_different_page does
    // `path.sub!(/…/, "")` on a route-helper literal (`active_path` → a
    // frozen "/active"). For the in-place string mutators whose bang form
    // modifies self and returns self-or-nil, the non-destructive
    // `<lvalue> = <lvalue>.<m>(args)` is equivalent when the bang return is
    // unused (statement position) and is frozen-safe. Assignable receiver
    // (Var/Ivar) only; `slice!` and friends (which return the removed part,
    // not the modified string) are deliberately excluded.
    let bang_rewrite: Option<Symbol> =
        if let ExprNode::Send { recv: Some(r), method, block: None, .. } = &*expr.node {
            // A reserved-word local (`class:`) or a numbered parameter
            // (`_1`) is not assignable, so it keeps the bang call.
            let is_lv = match &*r.node {
                ExprNode::Var { name, .. } => {
                    !crate::naming::is_reserved_local(name.as_str())
                        && !crate::naming::is_numbered_param(name.as_str())
                }
                ExprNode::Ivar { .. } => true,
                _ => false,
            };
            method
                .as_str()
                .strip_suffix('!')
                .filter(|base| {
                    is_lv
                        && matches!(
                            *base,
                            "sub" | "gsub" | "chomp" | "chop" | "strip" | "lstrip"
                                | "rstrip" | "squeeze" | "upcase" | "downcase"
                                | "capitalize" | "swapcase" | "tr" | "tr_s" | "delete"
                                | "reverse"
                        )
                })
                .map(Symbol::from)
        } else {
            None
        };
    if let Some(base) = bang_rewrite {
        let span = expr.span;
        let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
        let ExprNode::Send { recv, args, parenthesized, .. } = node else { unreachable!() };
        let r = recv.unwrap();
        let target = match &*r.node {
            ExprNode::Var { id, name } => LValue::Var { id: *id, name: name.clone() },
            ExprNode::Ivar { name } => LValue::Ivar { name: name.clone() },
            _ => unreachable!(),
        };
        let call = Expr::new(
            span,
            ExprNode::Send { recv: Some(r), method: base, args, block: None, parenthesized },
        );
        *expr.node = ExprNode::Assign { target, value: call };
        return;
    }

    // ActiveSupport `String#pluralize(count)` — count-aware inflection of
    // the string ITSELF (singular when count == 1, else the inflected
    // plural), distinct from the count-labeling `Inflector.pluralize(count,
    // word)` grounded below. Spinel can't reopen the built-in String, so
    // lower the String-receiver form to the ruby-family `Inflector.
    // pluralize_word` module function. String-typed receiver only, so a
    // model's own one-arg `pluralize` (should one exist) is left alone.
    if let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node {
        if method.as_str() == "pluralize"
            && args.len() == 1
            && matches!(r.ty, Some(crate::ty::Ty::Str))
        {
            let span = expr.span;
            let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
            let ExprNode::Send { recv, args, .. } = node else { unreachable!() };
            let count = args.into_iter().next().unwrap();
            *expr.node = ExprNode::Send {
                recv: Some(Expr::new(
                    span,
                    ExprNode::Const { path: vec![Symbol::from("Inflector")] },
                )),
                method: Symbol::from("pluralize_word"),
                args: vec![recv.unwrap(), count],
                block: None,
                parenthesized: true,
            };
            return;
        }
    }

    // `X.<helper>` where X singleton-includes url_helpers (lobsters'
    // `Routes.user_url reparent_user`): a `<x>_path` retargets to the
    // generated RouteHelpers module; a `<x>_url` whose path sibling is
    // generated re-lands as the bare `<x>_url` form so the absolute-URL
    // grounding a few blocks down claims it in this same visit.
    if let ExprNode::Send { recv: Some(r), method, .. } = &*expr.node {
        if let ExprNode::Const { path } = &*r.node {
            if path.len() == 1 && url_helper_classes.contains(&path[0]) {
                let named_path = route_helpers.contains(method);
                let named_url = method
                    .as_str()
                    .strip_suffix("_url")
                    .is_some_and(|stem| {
                        route_helpers.contains(&Symbol::from(format!("{stem}_path")))
                    });
                if named_path {
                    let ExprNode::Send { recv, .. } = &mut *expr.node else { unreachable!() };
                    *recv = Some(Expr::new(
                        expr.span,
                        ExprNode::Const { path: vec![Symbol::from("RouteHelpers")] },
                    ));
                } else if named_url {
                    let ExprNode::Send { recv, .. } = &mut *expr.node else { unreachable!() };
                    *recv = None;
                }
            }
        }
    }

    // Bare controller-context reads in a helper/view module body →
    // the per-dispatch parked objects (module functions have no
    // controller to delegate to): `request` →
    // `ActionController::Current.request`; `cookies`/`session`/`flash`
    // → `ActionController::Current.controller.<x>` (lobsters'
    // ApplicationHelper.filtered_tags reads the tag-filter cookie).
    if rewrite_request {
        // `helper_method :platform` — the app SAYING this controller
        // method is view-callable, for the marked methods the existing
        // path cannot serve.
        //
        // TWO ARMS OF ONE RULE, and the discriminator already exists.
        // `controller_helper_method_names` clones a marked method
        // CLASS-SIDE when its body is pure over its arguments, and the
        // view calls `DomainsController.caption_of_button(domain)` —
        // a static call, which is right because there is no per-request
        // state to reach. A marked method that READS REQUEST STATE
        // (campfire's `platform` is `@platform ||= ApplicationPlatform
        // .new(request.user_agent)`) cannot be cloned for exactly that
        // reason, and was left as honest residue. It routes through the
        // live controller instead.
        //
        // `index` is the "already served" test: a name in
        // `helper_method_index` has a module or a class-side clone
        // behind it, and this arm must not take it — doing so turned
        // lobsters' working static call into a dynamic one.
        //
        // Shadowed by the module's own methods and its params, because a
        // partial local spelled `platform` is that local (Rails mixes
        // helpers BENEATH a template's locals). Args are FORWARDED
        // rather than required to be empty: Rails allows a helper_method
        // to take them.
        if let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node {
            if view_visible.contains(method)
                && !index.contains_key(method)
                && !own_methods.contains(method)
                && !own_params.contains(method)
            {
                let span = expr.span;
                let controller = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(Expr::new(
                            span,
                            ExprNode::Const {
                                path: vec![
                                    Symbol::from("ActionController"),
                                    Symbol::from("Current"),
                                ],
                            },
                        )),
                        method: Symbol::from("controller"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                );
                *expr.node = ExprNode::Send {
                    recv: Some(controller),
                    method: method.clone(),
                    args: args.clone(),
                    block: block.clone(),
                    parenthesized: !args.is_empty(),
                };
                return;
            }
        }
        if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
            let m = method.as_str();
            if (m == "request" || m == "controller" || m == "cookies" || m == "session"
                || m == "flash" || m == "params")
                && args.is_empty()
                && (m != "controller"
                    || (!own_methods.contains(method) && !own_params.contains(method)))
            {
                let span = expr.span;
                let current = Expr::new(
                    span,
                    ExprNode::Const {
                        path: vec![
                            Symbol::from("ActionController"),
                            Symbol::from("Current"),
                        ],
                    },
                );
                let (recv, meth) = if m == "request" || m == "controller" {
                    (current, method.clone())
                } else {
                    (
                        Expr::new(
                            span,
                            ExprNode::Send {
                                recv: Some(current),
                                method: Symbol::from("controller"),
                                args: vec![],
                                block: None,
                                parenthesized: false,
                            },
                        ),
                        Symbol::from(m),
                    )
                };
                // The typed receiver is what arms the emitter's
                // string-key coercion (`is_string_keyed_hash`):
                // `@params` inside a controller is declared
                // `Hash[String, ParamValue]`, and a view's
                // `params[:join_code]` must not lose that fact on its
                // way through `Current.controller` — a Symbol key
                // against the string-keyed store reads nil, and
                // campfire's join form rendered `action="/join/"`.
                if m == "params" {
                    expr.ty = Some(crate::ty::Ty::Hash {
                        key: Box::new(crate::ty::Ty::Str),
                        value: Box::new(crate::ty::Ty::Class {
                            id: ClassId(Symbol::from("Roundhouse::ParamValue")),
                            args: vec![],
                        }),
                    });
                }
                *expr.node = ExprNode::Send {
                    recv: Some(recv),
                    method: meth,
                    args: vec![],
                    block: None,
                    parenthesized: false,
                };
                return;
            }
        }
    }

    // Case 1: collapse `<…Base>.helpers` to the ViewHelpers module constant.
    if is_base_dot_helpers(&expr.node) {
        *expr.node = ExprNode::Const { path: view_helpers_path() };
        return;
    }

    // Case 1b: a controller's own `helpers.<m>(…)` — Rails' view-context
    // proxy (campfire's `helpers.dom_id(@boost.message, :boosts)`). An
    // app helper resolves to its module, anything else to ViewHelpers.
    // (A controller's calls were already qualified against every helper
    // by `rewrite_controller_helpers_proxy`.)
    if !own_methods.contains(&Symbol::from("helpers")) {
        qualify_helpers_proxy(expr, index);
    }

    // Case 2: collapse `Rails.application.routes.url_helpers` to RouteHelpers.
    if is_rails_url_helpers(&expr.node) {
        *expr.node = ExprNode::Const { path: vec![Symbol::from("RouteHelpers")] };
        return;
    }

    // Bare `<x>_url` whose `<x>_path` sibling is generated — use the
    // shared view URL seam so request-derived origins get the same port
    // normalization as jbuilder. It still resolves Rails.application.domain,
    // preserving apps' canonical-domain overrides. Lobsters' hats page
    // links `request_hat_url` bare.
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
        if let Some(stem) = method.as_str().strip_suffix("_url") {
            let path_name = Symbol::from(format!("{stem}_path"));
            if route_helpers.contains(&path_name) {
                *expr = crate::lower::view_to_library::absolute_url_interp(stem, args.clone());
                return;
            }
        }
    }

    // Cases 3/4: a bare call resolving to an app or framework helper module.
    let mut renamed: Option<Symbol> = None;
    let path: Option<Vec<Symbol>> = match &*expr.node {
        ExprNode::Send { recv: None, method, args, block, .. } => {
            // A method's own PARAM shadows the helper index only as a BARE
            // reference (no args, no block) — that's a local read. `tag(x)`
            // or `tag { }` is a method call that still needs helper
            // qualification, so a same-named param must NOT capture it
            // (that dropped the qualification → latent NameError). Real
            // own-METHODS shadow at any arity (a self-dispatch).
            let param_shadow =
                own_params.contains(method) && args.is_empty() && block.is_none();
            if own_methods.contains(method) || param_shadow {
                // Self wins: a bare call naming a method the enclosing
                // class itself defines is a self-dispatch (Tag#to_param's
                // `tag` column reader), not a helper reference — Rails
                // helpers are mixed in beneath the receiver's own
                // methods, and models never see helpers at all. Without
                // this, an app helper that happens to share a model
                // accessor's name (lobsters' ApplicationHelper#tag
                // builder override vs Tag#tag) captures every read.
                None
            } else if let Some(module) = index.get(method) {
                renamed = renames.get(&(module.clone(), method.clone())).cloned();
                Some(module.0.as_str().split("::").map(Symbol::from).collect())
            } else if method.as_str() == "pluralize" && args.len() == 2 {
                // Count-labeling `pluralize(count, word)` in a helper
                // body — the same home the view pipeline's classifier
                // already grounds to (`Inflector.pluralize`, the
                // spinel-blog convention), NOT a second ViewHelpers
                // impl. Two-arg form only: the optional plural-word /
                // locale variants aren't in the runtime's surface, so
                // they stay verbatim rather than mis-bind arity.
                Some(vec![Symbol::from("Inflector")])
            } else if is_framework_view_helper(method.as_str()) {
                Some(view_helpers_path())
            } else if route_helpers.contains(method) {
                Some(vec![Symbol::from("RouteHelpers")])
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(path) = path {
        let span = expr.span;
        let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
        let ExprNode::Send { method, mut args, block, .. } = node else { unreachable!() };
        let method = renamed.unwrap_or(method);
        if path.len() == 1 && path[0].as_str() == "Inflector"
            && method.as_str() == "pluralize" && args.len() == 2 && !index.contains_key(&method)
        {
            let word = args.pop().unwrap();
            *expr = crate::lower::view::pluralize_helper_call(args.pop().unwrap(), word);
            return;
        }
        // `link_to(37, url)` — Rails stringifies the text arg; the runtime
        // link_to is deliberately monomorphic (String text), so coercion
        // belongs here at the call boundary. Literal strings stay bare.
        if matches!(method.as_str(), "link_to" | "link_to_raw") {
            if let Some(text) = args.first_mut() {
                if !matches!(
                    &*text.node,
                    ExprNode::Lit { value: crate::expr::Literal::Str { .. } }
                ) {
                    let inner = std::mem::replace(
                        &mut *text.node,
                        ExprNode::Seq { exprs: vec![] },
                    );
                    *text.node = ExprNode::Send {
                        recv: Some(Expr::new(text.span, inner)),
                        method: Symbol::from("to_s"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    };
                }
            }
        }
        // Rails' `distance_of_time_in_words` takes seconds as readily as
        // Times — the authentication generator's reset mailer passes
        // `(0, @user.password_reset_token_expires_in)`. The runtime keeps
        // the two apart (one parameter type each; a Time-or-Integer slot
        // is poly on spinel), so a call whose first time is an Integer
        // goes to the seconds entry when the second is an Integer too or
        // has no type here (a mailer view's `user` parameter reaches emit
        // untyped). A Time in that second slot does not slip through:
        // spinel refuses `Time - Integer` there, and CRuby raises on it.
        let is_int = |e: &Expr| matches!(e.ty, Some(crate::ty::Ty::Int));
        let int_or_open = |e: &Expr| {
            matches!(e.ty, None | Some(crate::ty::Ty::Int) | Some(crate::ty::Ty::Var { .. }))
        };
        let method = if method.as_str() == "distance_of_time_in_words"
            && path == view_helpers_path()
            && args.len() >= 2
            && is_int(&args[0])
            && int_or_open(&args[1])
        {
            Symbol::from("distance_of_seconds_in_words")
        } else {
            method
        };
        *expr.node = ExprNode::Send {
            recv: Some(Expr::new(span, ExprNode::Const { path })),
            method,
            args,
            block,
            parenthesized: true,
        };
    }

    // `RouteHelpers.<x>_path(format: :rss)` → `RouteHelpers.<x>_path +
    // ".rss"`. Rails path helpers accept `format:` whether or not the
    // route spells `(.:format)`; the IR has no keyword params, so the
    // suffix moves to the call site — arity-independent, and helpers
    // without a format slot stay format-capable.
    let format_suffix = match &*expr.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str().ends_with("_path")
                && matches!(&*r.node, ExprNode::Const { path }
                    if path.last().map(|s| s.as_str() == "RouteHelpers").unwrap_or(false)) =>
        {
            match args.last().map(|a| &*a.node) {
                Some(ExprNode::Hash { entries, kwargs: true }) if entries.len() == 1 => {
                    match &*entries[0].0.node {
                        ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
                            if value.as_str() == "format" =>
                        {
                            Some(entries[0].1.clone())
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(fmt) = format_suffix {
        let span = expr.span;
        let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
        let ExprNode::Send { recv, method, mut args, block, parenthesized } = node else {
            unreachable!()
        };
        args.pop();
        let call = Expr::new(span, ExprNode::Send { recv, method, args, block, parenthesized });
        let suffix = Expr::new(
            span,
            ExprNode::StringInterp {
                parts: vec![
                    crate::expr::InterpPart::Text { value: ".".to_string() },
                    crate::expr::InterpPart::Expr { expr: fmt },
                ],
            },
        );
        *expr.node = ExprNode::Send {
            recv: Some(call),
            method: Symbol::from("+"),
            args: vec![suffix],
            block: None,
            parenthesized: false,
        };
        return;
    }

    // `link_to(raw(x), …)` → `link_to_raw(x, …)`: Rails skips the label
    // escape for a safe buffer; with no safe-buffer type the exemption is
    // decided here. Children were rewritten first (and the bare-call
    // rewrite above may have just fired), so both calls are already in
    // their ViewHelpers.* form.
    if let ExprNode::Send { recv: Some(r), method, args, .. } = &mut *expr.node {
        if method.as_str() == "link_to" && is_view_helpers_const(r) && !args.is_empty() {
            let raw_inner = match &*args[0].node {
                ExprNode::Send { recv: Some(r2), method: m2, args: a2, .. }
                    if m2.as_str() == "raw"
                        && a2.len() == 1
                        && is_view_helpers_const(r2) =>
                {
                    Some(a2[0].clone())
                }
                _ => None,
            };
            if let Some(inner) = raw_inner {
                *method = Symbol::from("link_to_raw");
                args[0] = inner;
            }
        }
    }

    // Unwrap the view walker's default `html_escape(<call>.to_s)` when
    // the call is html_safe (see is_html_safe_call) — Rails doesn't
    // escape safe buffers, and escaping them ships literal &lt;img&gt;.
    let unwrap: Option<Expr> = match &*expr.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "html_escape"
                && args.len() == 1
                && is_view_helpers_const(r)
                && is_html_safe_call(strip_to_s(&args[0]), index) =>
        {
            Some(args[0].clone())
        }
        _ => None,
    };
    if let Some(inner) = unwrap {
        *expr = inner;
    }
}

/// Ruby-emit-path pass: wrap each action's html render in the layout
/// call — `render(Views::X.y(...))` → `render(Views::Layouts.application(
/// Views::X.y(...), @<ivar>…, @flash…))`. Lives here (not the shared
/// controller lowering) because the wrap shape and the CRuby dispatch
/// contract move together: the overlay main.rb ships `controller.body`
/// verbatim, while other targets' dispatchers still wrap body-only.
/// The controller render seam is where a layout's ivar reads (@user,
/// @title) are statically in scope — the generic dispatch had no way to
/// pass them. Skipped renders: jbuilder json (`*_json` view call or a
/// `content_type:` kwarg), non-view renders (`render html:`/`plain:` —
/// Rails skips the layout for those too), and an already-wrapped
/// Layouts call (idempotence). No-op when the app has no
/// layouts/application view.
pub(crate) fn apply_layout_lowering(lcs: &mut [LibraryClass], app: &App) {
    // Cheap probe: no layouts/application view → nothing to wrap.
    let probe = Expr::new(
        crate::span::Span::synthetic(),
        ExprNode::Lit { value: crate::expr::Literal::Nil },
    );
    if crate::lower::view_to_library::layout_wrap_expr(app, probe).is_none() {
        return;
    }
    for lc in lcs.iter_mut() {
        // `layout false` on the controller this class came from, scoped
        // by the `only:`/`except:` the declaration carried. Resolved
        // per METHOD because the scoping is per action: campfire's
        // `MessagesController` writes `layout false, only: :index`, so
        // `index` must render bare — it is a turbo-frame fragment
        // loaded into a page that already has a layout, and wrapping it
        // shipped a whole second `<html>` inside the first, 117 tags
        // Rails does not send — while `show` and `edit` keep theirs.
        let decl = app
            .controllers
            .iter()
            .find(|c| c.name == lc.name)
            .map(|c| c.layout.clone());
        for m in &mut lc.methods {
            if decl.as_ref().is_some_and(|d| d.suppresses(&m.name)) {
                continue;
            }
            rewrite_layout_wrap(&mut m.body, app);
        }
    }
}

fn rewrite_layout_wrap(expr: &mut Expr, app: &App) {
    expr.node.for_each_child_mut(&mut |c| rewrite_layout_wrap(c, app));
    // Post-lowering action bodies carry `render` as a SelfRef-receiver
    // Send (`self.render(...)` shape); accept the bare form too.
    let ExprNode::Send { recv, method, args, .. } = &mut *expr.node else {
        return;
    };
    let self_recv = match recv {
        None => true,
        Some(r) => matches!(&*r.node, ExprNode::SelfRef),
    };
    if !self_recv || method.as_str() != "render" || args.is_empty() {
        return;
    }
    // Extract-and-strip any `layout:` kwarg first: the shared lowering
    // keeps it as the wrap marker for body renders (`render html: X,
    // layout: "application"`), and the runtime `render(body, status:,
    // content_type:, location:)` doesn't accept it, so it must never
    // survive to the call.
    let mut layout_kwarg: Option<Expr> = None;
    for a in args.iter_mut().skip(1) {
        if let ExprNode::Hash { entries, .. } = &mut *a.node {
            entries.retain(|(k, v)| {
                let is_layout = matches!(
                    &*k.node,
                    ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
                        if value.as_str() == "layout"
                );
                if is_layout {
                    layout_kwarg = Some(v.clone());
                }
                !is_layout
            });
        }
    }
    args.retain(|a| !matches!(&*a.node, ExprNode::Hash { entries, .. } if entries.is_empty()));
    if args.is_empty() {
        return;
    }
    // An explicit `layout: false` VETOES the wrap outright — the
    // controller-side partial-render rewrite plants it (Rails renders
    // partials without a layout), and a user's `render …, layout:
    // false` means the same thing.
    if layout_kwarg.as_ref().is_some_and(|v| {
        matches!(&*v.node, ExprNode::Lit { value: crate::expr::Literal::Bool { value: false } })
    }) {
        return;
    }
    // An explicit `layout: "application"` / `layout: true` wraps a body
    // render (non-Views literal html) the way Rails does. Other layout
    // names are left unwrapped — honest residue, only `application`
    // exists as an emitted layout.
    let layout_requested = layout_kwarg.as_ref().is_some_and(|v| match &*v.node {
        ExprNode::Lit { value: crate::expr::Literal::Str { value } } => value == "application",
        ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => {
            value.as_str() == "application"
        }
        ExprNode::Lit { value: crate::expr::Literal::Bool { value } } => *value,
        _ => false,
    });
    // Trailing kwargs are fine (`status: :unprocessable_entity` renders
    // WITH layout in Rails) — except the jbuilder branch's
    // `content_type:`, which marks a non-html response.
    let has_content_type = args.iter().skip(1).any(|a| match &*a.node {
        ExprNode::Hash { entries, .. } => entries.iter().any(|(k, _)| {
            matches!(&*k.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
                if value.as_str() == "content_type")
        }),
        _ => false,
    });
    if has_content_type {
        return;
    }
    // An explicit `layout: "application"`/`true` wraps WHATEVER the
    // body is — `render html: content.html_safe, layout:
    // "application"` (lobsters /u serves a cached tree string) has a
    // Send body that isn't a Views call, and the Views-shape test
    // alone wrongly skipped it. Without an explicit request, only a
    // non-layout non-json Views call wraps (the implicit html render).
    let wrappable = layout_requested
        || match &*args[0].node {
            ExprNode::Send { recv: Some(r), method: vm, .. } => {
                !vm.as_str().ends_with("_json")
                    && matches!(&*r.node, ExprNode::Const { path }
                        if path.len() == 2
                            && path[0].as_str() == "Views"
                            && path[1].as_str() != "Layouts")
            }
            _ => false,
        };
    if !wrappable {
        return;
    }
    let inner = args[0].clone();
    if let Some(wrapped) = crate::lower::view_to_library::layout_wrap_expr(app, inner) {
        args[0] = wrapped;
    }
}

// `record.update!(k: v, ...)` kwargs inlining moved to the shared
// post-analyze hook (`lower::apply_update_kwargs_inline`) — hook
// bodies arrive here already in writer-assign + save form, with
// unknown-receiver and impure-receiver sites on the residue ledger.

// `errors.add(:field, "msg")` grounding moved to the shared
// post-analyze hook (`lower::apply_errors_add_lowering`) — every hook
// body arrives here already rewritten to the `errors << "Field msg"`
// accumulator shape, with non-self receivers on the residue ledger.

/// Ground `Rails.application.routes.url_helpers.<x>_url(record?, host: H,
/// protocol: P)` → `"#{P}://#{H}#{RouteHelpers.<x>_path(record?)}"`. The
/// routing object graph behind `url_helpers` isn't modeled (and never
/// needs to be for this shape — an absolute URL is protocol + host + the
/// generated path helper). Leading positional args (the record in
/// `user_url(sender, host:)`) and any non-host/protocol kwargs (real
/// path params) are forwarded to the `<x>_path` helper; the record rides
/// whole so its custom `to_param` resolves. Non-matching url_helpers
/// uses are left alone. Applied to the Rails::Application reopen (whose
/// kwargs-only `root_url` is the original occurrence) and mod_note's
/// `user_url(sender, host:)`.
pub(crate) fn rewrite_url_helpers_absolute(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut rewrite_url_helpers_absolute);
    let matches = matches!(
        &*expr.node,
        ExprNode::Send { recv: Some(uh), method, args, block: None, .. }
            if method.as_str().ends_with("_url")
                && !args.is_empty()
                && matches!(&*args[args.len() - 1].node, ExprNode::Hash { .. })
                && is_url_helpers_chain(uh)
    );
    if !matches {
        return;
    }
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { method, mut args, .. } = node else { unreachable!() };
    // Split leading positional path args (e.g. the record in
    // `user_url(sender, host:)`) from the trailing kwargs hash.
    let trailing = args.pop().unwrap();
    let positional = args;
    let ExprNode::Hash { entries, .. } = &*trailing.node else { unreachable!() };
    let mut host: Option<Expr> = None;
    let mut protocol: Option<Expr> = None;
    let mut path_kwargs: Vec<(Expr, Expr)> = Vec::new();
    for (k, v) in entries {
        if let ExprNode::Lit { value: Literal::Sym { value } } = &*k.node {
            match value.as_str() {
                "host" => {
                    host = Some(v.clone());
                    continue;
                }
                "protocol" => {
                    protocol = Some(v.clone());
                    continue;
                }
                _ => {}
            }
        }
        // Any non-host/protocol key is a real path parameter — forward it.
        path_kwargs.push((k.clone(), v.clone()));
    }
    // Forward leading positionals plus any surviving path kwargs to the
    // generated `<stem>_path` helper (the record rides whole, so custom
    // `to_param` — User=username — resolves inside the path helper).
    let mut path_args: Vec<Expr> = positional;
    if !path_kwargs.is_empty() {
        path_args.push(Expr::new(
            trailing.span,
            ExprNode::Hash { entries: path_kwargs, kwargs: true },
        ));
    }
    let stem = method.as_str().trim_end_matches("_url");
    let path_call = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(
                span,
                ExprNode::Const { path: vec![Symbol::from("RouteHelpers")] },
            )),
            method: Symbol::from(format!("{stem}_path")),
            args: path_args,
            block: None,
            parenthesized: true,
        },
    );
    let lit = |s: &str| crate::expr::InterpPart::Text { value: s.to_string() };
    let dyn_part = |e: Expr| crate::expr::InterpPart::Expr { expr: e };
    let mut parts: Vec<crate::expr::InterpPart> = Vec::new();
    if let Some(p) = protocol {
        parts.push(dyn_part(p));
    } else {
        parts.push(lit("http"));
    }
    parts.push(lit("://"));
    if let Some(h) = host {
        parts.push(dyn_part(h));
    }
    parts.push(dyn_part(path_call));
    *expr.node = ExprNode::StringInterp { parts };
    expr.ty = Some(crate::ty::Ty::Str);
}

fn is_url_helpers_chain(e: &Expr) -> bool {
    let ExprNode::Send { recv: Some(routes), method, .. } = &*e.node else { return false };
    if method.as_str() != "url_helpers" {
        return false;
    }
    let ExprNode::Send { recv: Some(rails_app), method: routes_m, .. } = &*routes.node else {
        return false;
    };
    if routes_m.as_str() != "routes" {
        return false;
    }
    matches!(&*rails_app.node, ExprNode::Send { recv: Some(r), method: app_m, .. }
        if app_m.as_str() == "application"
            && matches!(&*r.node, ExprNode::Const { path }
                if path.len() == 1 && path[0].as_str() == "Rails"))
}

// Mailer class-side wrappers (`def self.notify = new.notify(...)`)
// moved to the shared post-analyze hook
// (`lower::apply_mailer_class_side`) — mailer classes arrive here
// with the wrappers already synthesized, keyword/block-taking methods
// on the residue ledger.

// Block-form `create!/create do |kv| ... end` inlining moved to the
// shared post-analyze hook (`lower::apply_create_block_inline`) —
// every hook body arrives here with the factory block already inlined
// (kv = X.new; body; save-or-raise; kv).

/// View-pipeline vestige of the shared `Time.current` grounding
/// (`lower::apply_time_current_lowering`): the post-analyze hook skips
/// view bodies, so lowered view classes still take the rewrite here.
/// Delete when the view pipeline migrates to shared lowerings. Every
/// other body class arrives already grounded (re-running is an
/// idempotent no-op — `Time.current` no longer occurs).
///
/// A PostgreSQL tree's views keep Rails' zone-aware `Time.current`
/// (`lower::time_current::carries_zone_port`), as its other bodies do.
pub(crate) fn apply_time_current_lowering(lcs: &mut [LibraryClass], app: &App) {
    let zone_port = crate::lower::time_current::carries_zone_port(app);
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            crate::lower::time_current::rewrite_time_current_on(&mut m.body, &app.time_formats, zone_port);
        }
    }
}

/// View-pipeline vestige of the shared duration grounding
/// (`lower::apply_duration_lowering`): the post-analyze hook skips view
/// bodies, so lowered view classes still take the rewrite here
/// (lobsters' `_commentbox.html.erb` both subtracts a Duration from a
/// Time and calls `before?` on the result). Delete when the view
/// pipeline migrates to shared lowerings. Every other body class
/// arrives already grounded (re-running is an idempotent no-op — the
/// grounded forms no longer match).
///
/// The Time-vs-Duration comparison rule reads a receiver type the view
/// pipeline doesn't stamp, so in practice only the unit, predicate and
/// arithmetic rules bite here; running the full sequence keeps the one
/// order declaration (`lower::duration::apply_duration_rewrites_on`).
/// Ruby-family pre-emit pass: a fixture ACCESSOR is memoized, because
/// Rails' is.
///
/// `ActiveRecord::TestFixtures` keeps a `@fixture_cache` and clears it
/// between tests, so two `accounts(:signal)` calls in one test answer
/// the SAME object. The lowered accessor is `Account.find(1)` — a new
/// object every call — so campfire's `accounts(:signal).settings.
/// restrict_room_creation_to_administrators = true` followed by
/// `accounts(:signal).save!` is a mutation of one record and a save of
/// a DIFFERENT one. The write
/// vanished and campfire's room-creation restriction was never in
/// force. It is also what makes `.reload` mean anything: reloading a
/// throwaway instance is a no-op by construction.
///
/// RUBY-FAMILY, not the shared lowering, because the cache is CLASS
/// STATE on a module — `@__fx_<label>` inside a `def self.<label>`.
/// Rust, Crystal and Swift each build a fixture module too, and none of
/// them has a home for that slot; measured, all three dropped the
/// accessor entirely (`cannot find function 'one' in module
/// fixtures::articles`). Their accessors keep the per-call lookup,
/// which is the behaviour they have today.
///
/// The slot is cleared at the top of `_fixtures_load!` rather than by a
/// separate hook: the loader runs once per test, right after the schema
/// reset, so that IS the between-tests boundary.
///
/// Spelled `@slot = <lookup> if @slot.nil?` then a bare read, not
/// `@slot ||= <lookup>` — the python lowering declines `OpAssign`, and
/// the two-statement form leaves the method ending in a read.
pub(crate) fn apply_fixture_memoization(lcs: &mut [LibraryClass]) {
    for lc in lcs.iter_mut() {
        let labels: Vec<Symbol> = lc
            .methods
            .iter()
            .filter(|m| m.receiver == MethodReceiver::Class && is_fixture_lookup(&m.body))
            .map(|m| m.name.clone())
            .collect();
        if labels.is_empty() {
            continue;
        }
        for m in &mut lc.methods {
            if m.receiver != MethodReceiver::Class {
                continue;
            }
            if is_fixture_lookup(&m.body) {
                m.body = memoized_fixture_body(&m.name, &m.body);
            } else if m.name.as_str() == "_fixtures_load!" {
                let mut exprs: Vec<Expr> = labels
                    .iter()
                    .map(|label| {
                        Expr::new(
                            Span::synthetic(),
                            ExprNode::Assign {
                                target: LValue::Ivar { name: fixture_cache_ivar(label) },
                                value: Expr::new(
                                    Span::synthetic(),
                                    ExprNode::Lit { value: Literal::Nil },
                                ),
                            },
                        )
                    })
                    .collect();
                exprs.push(m.body.clone());
                m.body = Expr::new(Span::synthetic(), ExprNode::Seq { exprs });
            }
        }
    }
}

/// The lowered accessor shape — `<Model>.find(<literal id>)` and
/// nothing else. `by_label` (an if/elsif chain) and `_fixtures_load!`
/// (a Seq of inserts) both fail this, which is how the pass tells the
/// three kinds of method in a fixture module apart without knowing
/// their names.
fn is_fixture_lookup(body: &Expr) -> bool {
    matches!(&*body.node, ExprNode::Send { recv: Some(r), method, args, block: None, .. }
        if method.as_str() == "find"
            && args.len() == 1
            && matches!(&*args[0].node, ExprNode::Lit { value: Literal::Int { .. } })
            && matches!(&*r.node, ExprNode::Const { .. }))
}

/// `@__fx_<label>` — the cache slot. Prefixed so it cannot collide with
/// anything the fixture file names.
///
/// A fixture label is any YAML key, and an instance variable name is
/// not: Core's domains.yml labels hosts (`ihatetaxes.myshopify.com`,
/// `wwwstorefront-api_com`), and `@__fx_wwwstorefront-api_com` parses
/// as a subtraction, a load-time warning that Core's StrictWarning
/// raises on, in a file 540 emitted tests require. (The accessor itself
/// is already `define_singleton_method(:"<label>")`.)
///
/// So a label that is not all `[A-Za-z0-9_]` gets its own prefix,
/// `@__fxe_`, and an escape: ASCII letters and digits stand, `_` is
/// `__`, and any other character is `_<hex codepoint>_`. The map is
/// injective: the two prefixes differ at their fifth character, and
/// the escape decodes left to right (after `_`, a second `_` is the
/// underscore; a hex digit starts a codepoint ended by `_`). So
/// `a.b-c` (`@__fxe_a_2e_b_2d_c`) and `a_b_c` (`@__fx_a_b_c`) are two
/// slots, and so are `a.b` and a label spelled `a_2e_b`.
fn fixture_cache_ivar(label: &Symbol) -> Symbol {
    let label = label.as_str();
    if label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Symbol::from(format!("__fx_{label}"));
    }
    let mut slot = String::from("__fxe_");
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            slot.push(c);
        } else if c == '_' {
            slot.push_str("__");
        } else {
            slot.push_str(&format!("_{:x}_", c as u32));
        }
    }
    Symbol::from(slot)
}

fn memoized_fixture_body(label: &Symbol, lookup: &Expr) -> Expr {
    let slot = fixture_cache_ivar(label);
    let read = || {
        let mut e = Expr::new(Span::synthetic(), ExprNode::Ivar { name: slot.clone() });
        e.ty = lookup.ty.clone();
        e
    };
    let fill = Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(read()),
                    method: Symbol::from("nil?"),
                    args: vec![],
                    block: None,
                    parenthesized: true,
                },
            ),
            then_branch: Expr::new(
                Span::synthetic(),
                ExprNode::Assign {
                    target: LValue::Ivar { name: slot.clone() },
                    value: lookup.clone(),
                },
            ),
            else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        },
    );
    let mut seq = Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![fill, read()] });
    seq.ty = lookup.ty.clone();
    seq
}

pub(crate) fn apply_duration_lowering(lcs: &mut [LibraryClass], app: &App) {
    let temporal_predicates = !crate::lower::duration::app_defines_temporal_predicates(app);
    let zone_port = crate::lower::time_current::carries_zone_port(app);
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            crate::lower::duration::apply_duration_rewrites_on(&mut m.body, temporal_predicates, zone_port);
        }
    }
}

/// A record handed to a path helper becomes its slug HERE, at the call
/// site, not inside the helper.
///
/// Rails puts `to_param` in the helper body and gets away with it: the
/// param is untyped, so the record arrives whole and `to_param`
/// dispatches to the model's override. Our helpers declare their
/// segments `String` (`routes_to_library::param_ty` — a slug segment
/// typed Int made every strict-target call site passing `short_id` a C
/// type error), and a declared type is a promise the caller has to
/// keep. Under AOT it IS kept: a `Tag` handed to a `String` slot is
/// coerced on the way in, so the helper body never saw a record at all
/// — `tag_path(tag)` rendered `/t/` with an EMPTY segment long before
/// `String#to_param` raised on it. The raise was the visible half of a
/// silent wrong-URL bug.
///
/// So the conversion moves to where the value's identity is still
/// known. Three signals, in order of how much they know:
///
///   1. the stamped type is a model that overrides `to_param` —
///      analyzed bodies (models, controllers) answer here;
///   2. the arg reads a SINGULAR association (`story.domain`,
///      `showing_user.invited_by_user`) whose target overrides it —
///      the `reference_targets` relation, rebuilt from `app.models`
///      because view bodies reach emit with `Ty::Var` args;
///   3. the arg is a bare name that IS a model name (`tag`, `@user`) —
///      the same convention the view lowerer's `ivar_ty` commits to.
///
/// Everything else is left alone, which is the whole reason the rule is
/// positive-signal-only: `tag.id`, `story.short_id`, `user.username`,
/// `user[:username]`, `@params["username"]` and an already-written
/// `story.to_param` all reach a path helper in this corpus, and every
/// one of them is already a slug. A rule phrased as "wrap unless it
/// looks scalar" would have to enumerate those instead, and would wrap
/// the next unfamiliar shape by default.
pub(crate) fn apply_route_param_lowering(lcs: &mut [LibraryClass], app: &App) {
    let all_models: crate::det::HashSet<String> =
        app.models.iter().map(|m| m.name.0.as_str().to_string()).collect();
    if all_models.is_empty() {
        return;
    }
    let slug_models = models_overriding_to_param(app);
    let assoc_targets = singular_association_targets(app);
    let collection_targets = collection_association_targets(app);
    // `direct :name do |record, options| … end` helpers take their
    // arguments VERBATIM — Rails hands them to the block, and the block
    // decides what to read. campfire's
    // `direct :fresh_user_avatar do |user, options| route_for
    // :user_avatar, user.avatar_token, v: user.updated_at… end` wants
    // the RECORD; projecting it to `user.id` first handed the block an
    // Integer and every call died on `undefined method 'updated_at'`.
    // Skipping them by name is the one place this pass can tell the
    // difference: a generated resource helper's segment is a param, a
    // direct helper's argument is whatever its block says it is.
    let direct_helpers: crate::det::HashSet<String> = app
        .routes
        .direct_helpers
        .iter()
        .map(|h| format!("{}_path", h.name.as_str()))
        .collect();
    let class_method_targets = record_answering_class_methods(app);
    // The route helpers' OWN lowered signatures — read off the slice we
    // are rewriting, not re-derived from the route table. `param_ty`
    // decides a segment's type from CALL-SITE evidence (a String
    // reaching it makes it a slug), and this pass decides a record's
    // projection from whether the model overrides `to_param`. Those are
    // two different questions and campfire answers them differently for
    // the same helper, so the projection has to read what the signature
    // actually says.
    let helper_param_tys = route_helper_param_types(app);
    let mut sig = RecordSignals {
        all_models: &all_models,
        slug_models: &slug_models,
        assoc_targets: &assoc_targets,
        collection_targets: &collection_targets,
        class_method_targets: &class_method_targets,
        direct_helpers: &direct_helpers,
        helper_param_tys: &helper_param_tys,
        controller_names: app
            .controllers
            .iter()
            .filter_map(|c| c.name.0.as_str().rsplit("::").next().map(str::to_string))
            .collect(),
        self_returns: crate::det::HashMap::default(),
        local_records: crate::det::HashMap::default(),
        self_model: None,
    };
    // What each INSTANCE method in this slice answers, to a fixpoint.
    //
    // Across the whole slice, not per class, because the caller and the
    // method are routinely in different ones: campfire's
    // `WelcomeController#index` reads `last_room_visited`, which
    // `ApplicationController` declares. Walking an ancestor chain would
    // say the same thing for these two and less for a concern; the
    // uniqueness rule below is what keeps a slice-wide map honest, and
    // it is the same standard the association maps hold.
    //
    // Surveyed from the CONTROLLERS TOO, off `app.controllers` rather
    // than the slice: the slice being rewritten is routinely the
    // models/helpers or views one, and the method a helper names lives
    // in a controller — campfire's
    // `RoomsHelper.link_back_to_last_room_visited` reads
    // `Current.controller.last_room_visited`, declared on
    // `ApplicationController`. On the controllers slice the same
    // methods are surveyed twice and agree, which the uniqueness rule
    // absorbs.
    //
    // A FIXPOINT because the methods call each other:
    // `last_room_visited` is `…find_by(id: cookie) || default_room`,
    // and `default_room` has to be resolved first. Bounded by the
    // method count; two rounds in practice.
    {
        let controller_methods: Vec<(&str, &Expr)> = app
            .controllers
            .iter()
            .flat_map(|c| c.body.iter())
            .filter_map(|item| match item {
                crate::dialect::ControllerBodyItem::Action { action, .. } => {
                    Some((action.name.as_str(), &action.body))
                }
                _ => None,
            })
            .collect();
        let mut ambiguous: crate::det::HashSet<String> = crate::det::HashSet::default();
        for _ in 0..8 {
            let mut grew = false;
            let slice_methods = lcs.iter().flat_map(|lc| {
                lc.methods
                    .iter()
                    .filter(|m| m.receiver == MethodReceiver::Instance)
                    .map(|m| (m.name.as_str(), &m.body))
            });
            for (name, body) in slice_methods.chain(controller_methods.iter().copied()) {
                if ambiguous.contains(name) {
                    continue;
                }
                let Some(model) = arg_record_model(body_tail(body), &sig) else {
                    continue;
                };
                match sig.self_returns.get(name) {
                    Some(existing) if existing == &model => {}
                    Some(_) => {
                        ambiguous.insert(name.to_string());
                        sig.self_returns.remove(name);
                        grew = true;
                    }
                    None => {
                        sig.self_returns.insert(name.to_string(), model);
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
    }
    for lc in lcs.iter_mut() {
        sig.self_model = Some(lc.name.0.as_str().to_string())
            .filter(|n| all_models.contains(n));
        for m in &mut lc.methods {
            sig.local_records.clear();
            sig.local_records = collect_local_records(&m.body, &sig);
            rewrite_route_params(&mut m.body, &sig);
        }
    }
}

/// Locals a body binds to ONE resolvable record, by name — the map
/// behind `arg_record_model`'s local-variable signal. campfire's
///
/// ```text
/// if last_room = last_room_visited
///   link_back_to room_path(last_room)
/// ```
///
/// hands the helper a plain local; every signal the pass reads is
/// gone from the name itself, but the ASSIGNMENT still says what it
/// holds. Held to the standard every map here shares: a name every
/// assignment resolves to the same model answers that model; a name
/// any assignment does not resolve (or two disagree about) answers
/// nothing — including through `OpAssign`/`MultiAssign`, which this
/// pass cannot read.
fn collect_local_records(
    body: &Expr,
    sig: &RecordSignals<'_>,
) -> crate::det::HashMap<String, String> {
    fn walk(
        e: &Expr,
        sig: &RecordSignals<'_>,
        out: &mut crate::det::HashMap<String, String>,
        poisoned: &mut crate::det::HashSet<String>,
    ) {
        let unresolve = |name: &Symbol,
                             out: &mut crate::det::HashMap<String, String>,
                             poisoned: &mut crate::det::HashSet<String>| {
            out.remove(name.as_str());
            poisoned.insert(name.as_str().to_string());
        };
        match &*e.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
                match arg_record_model(value, sig) {
                    Some(model) if !poisoned.contains(name.as_str()) => {
                        match out.get(name.as_str()) {
                            Some(existing) if existing == &model => {}
                            Some(_) => unresolve(name, out, poisoned),
                            None => {
                                out.insert(name.as_str().to_string(), model);
                            }
                        }
                    }
                    _ => unresolve(name, out, poisoned),
                }
            }
            ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => {
                unresolve(name, out, poisoned);
            }
            ExprNode::MultiAssign { targets, .. } => {
                for t in targets {
                    if let LValue::Var { name, .. } = t {
                        unresolve(name, out, poisoned);
                    }
                }
            }
            _ => {}
        }
        e.node.for_each_child(&mut |c| walk(c, sig, out, poisoned));
    }
    let mut out = crate::det::HashMap::default();
    let mut poisoned = crate::det::HashSet::default();
    walk(body, sig, &mut out, &mut poisoned);
    out
}

/// The last expression a body evaluates to — its return value for
/// every shape this pass reads. A `Seq`'s tail; anything else is
/// already the tail.
fn body_tail(body: &Expr) -> &Expr {
    match &*body.node {
        ExprNode::Seq { exprs } => exprs.last().unwrap_or(body),
        _ => body,
    }
}

/// Model class methods that answer ONE RECORD of their own model —
/// a body whose tail is `.first` / `.last`.
///
/// campfire's `Room.original` (`order(:created_at).first`) is the
/// caller: `default_room` reads `Current.user.rooms.original`, whose
/// receiver is a `has_many :through` and so carries no type at all.
/// The METHOD NAME carries it instead, on the same uniqueness rule the
/// association maps use — a name two models declare answers None.
fn record_answering_class_methods(app: &App) -> crate::det::HashMap<String, String> {
    let mut out: crate::det::HashMap<String, String> = crate::det::HashMap::default();
    let mut ambiguous: crate::det::HashSet<String> = crate::det::HashSet::default();
    // Read from `app.models`, not from the LibraryClass slice this pass
    // was handed: the pass runs over CONTROLLERS too, and the class
    // method a controller body names lives on a model that slice does
    // not contain.
    for model_def in &app.models {
        let model = model_def.name.0.as_str().to_string();
        for m in model_def.body.iter().filter_map(|item| match item {
            crate::dialect::ModelBodyItem::Method { method, .. } => Some(method),
            _ => None,
        }) {
            if m.receiver != MethodReceiver::Class {
                continue;
            }
            let tail = body_tail(&m.body);
            let answers_one = matches!(&*tail.node,
                ExprNode::Send { recv: Some(_), method, args, block: None, .. }
                    if args.is_empty() && matches!(method.as_str(), "first" | "last"));
            if !answers_one {
                continue;
            }
            let key = m.name.as_str().to_string();
            match out.get(&key) {
                Some(existing) if existing != &model => {
                    ambiguous.insert(key);
                }
                _ => {
                    out.insert(key, model.clone());
                }
            }
        }
    }
    for k in ambiguous {
        out.remove(&k);
    }
    out
}

/// Model names (`Tag`, `User`) whose class defines its own `to_param`.
fn models_overriding_to_param(app: &App) -> crate::det::HashSet<String> {
    app.models
        .iter()
        .filter(|m| {
            m.body.iter().any(|item| matches!(item, crate::dialect::ModelBodyItem::Method { method, .. }
                if method.name.as_str() == "to_param"))
        })
        .map(|m| m.name.0.as_str().to_string())
        .collect()
}

/// Singular association reader → target model name. `belongs_to` and
/// `has_one` only: a `has_many` reader is a collection and never lands
/// in a path segment. An ambiguous name (two models naming the same
/// reader at different targets) is dropped rather than guessed.
/// has_many reader name → target model, when unambiguous across models.
///
/// Only consulted for `.first` / `.last` on such a read: those answer
/// ONE record of the collection's type, which is a record a path helper
/// needs converted. campfire's front door is
/// `redirect_to room_url(Current.user.rooms.last)` — a Send chain, so
/// none of the name-based signals see it, and it redirected to
/// `/rooms/#<Room:0x…>`. An ambiguous name is dropped rather than
/// guessed, exactly as the singular map does.
fn collection_association_targets(app: &App) -> crate::det::HashMap<String, String> {
    use crate::dialect::Association;
    let mut out: crate::det::HashMap<String, String> = crate::det::HashMap::default();
    let mut ambiguous: crate::det::HashSet<String> = crate::det::HashSet::default();
    for m in &app.models {
        for a in m.associations() {
            let Association::HasMany { name, target, .. } = a else { continue };
            let key = name.as_str().to_string();
            let val = target.0.as_str().to_string();
            match out.get(&key) {
                Some(existing) if existing != &val => {
                    ambiguous.insert(key);
                }
                _ => {
                    out.insert(key, val);
                }
            }
        }
    }
    for k in ambiguous {
        out.remove(&k);
    }
    out
}

fn singular_association_targets(app: &App) -> crate::det::HashMap<String, String> {
    use crate::dialect::Association;
    let mut out: crate::det::HashMap<String, String> = crate::det::HashMap::default();
    let mut ambiguous: crate::det::HashSet<String> = crate::det::HashSet::default();
    for m in &app.models {
        for a in m.associations() {
            let (name, target) = match a {
                Association::BelongsTo { name, target, .. }
                | Association::HasOne { name, target, .. } => (name, target),
                _ => continue,
            };
            let key = name.as_str().to_string();
            let val = target.0.as_str().to_string();
            match out.get(&key) {
                Some(existing) if existing != &val => {
                    ambiguous.insert(key);
                }
                _ => {
                    out.insert(key, val);
                }
            }
        }
    }
    for k in ambiguous {
        out.remove(&k);
    }
    out
}

/// Everything `arg_record_model` reads, in one place — the signal set
/// grew past what a positional argument list explains.
///
/// `self_returns` is the one that varies per LibraryClass: it maps THIS
/// class's own method names to the model each one answers, so a bare
/// `room_path(last_room_visited)` can resolve through the method it
/// names. The other four are App-wide.
/// `<helper>_path` -> the declared type of each positional parameter.
///
/// Read off the SAME builder the emit ships
/// (`lower_routes_to_library_functions`), not re-derived from the route
/// table: `param_ty`'s slug rule reads call-site evidence, and a second
/// copy of that reasoning here would drift the first time either moved.
/// The route helpers are `LibraryFunction`s and never appear in the
/// `lcs` slice this pass rewrites, so they have to be asked for.
///
/// Called BEFORE this pass mutates anything — the builder surveys the
/// IR to size each helper, which is the ordering constraint
/// `route_helper_receiver` already documents.
fn route_helper_param_types(
    app: &App,
) -> crate::det::HashMap<String, Vec<crate::ty::Ty>> {
    let mut out = crate::det::HashMap::default();
    for f in crate::lower::routes_to_library::lower_routes_to_library_functions(app) {
        if f.module_path.last().map(|s| s.as_str()) != Some("RouteHelpers") {
            continue;
        }
        let Some(crate::ty::Ty::Fn { params, .. }) = &f.signature else {
            continue;
        };
        out.insert(
            f.name.as_str().to_string(),
            params.iter().map(|p| p.ty.clone()).collect(),
        );
    }
    out
}

pub(crate) struct RecordSignals<'a> {
    all_models: &'a crate::det::HashSet<String>,
    slug_models: &'a crate::det::HashSet<String>,
    assoc_targets: &'a crate::det::HashMap<String, String>,
    collection_targets: &'a crate::det::HashMap<String, String>,
    /// A model CLASS METHOD whose body answers one record of that model
    /// — `Room.original` is `order(:created_at).first`. Unambiguous
    /// names only.
    class_method_targets: &'a crate::det::HashMap<String, String>,
    direct_helpers: &'a crate::det::HashSet<String>,
    /// `<helper>_path` -> declared type per positional parameter.
    helper_param_tys: &'a crate::det::HashMap<String, Vec<crate::ty::Ty>>,
    /// Controller class names (last path segment), so a class-side
    /// respelling (`ApplicationController.last_room_visited`) is
    /// recognized as the controller-method read it is.
    controller_names: crate::det::HashSet<String>,
    self_returns: crate::det::HashMap<String, String>,
    /// Locals THE BODY BEING REWRITTEN binds to a resolvable record —
    /// campfire's `if last_room = last_room_visited` puts the record
    /// behind a name no other signal can read. Rebuilt per method;
    /// a name is held to the same standard as every map here: every
    /// assignment must resolve, and to the same model, or the name
    /// answers nothing.
    local_records: crate::det::HashMap<String, String>,
    /// The model whose methods are being rewritten, when the enclosing
    /// class is one — what a bare `self` names there.
    self_model: Option<String>,
}

fn rewrite_route_params(expr: &mut Expr, sig: &RecordSignals<'_>) {
    expr.node.for_each_child_mut(&mut |e| rewrite_route_params(e, sig));
    let is_helper_call = matches!(
        &*expr.node,
        ExprNode::Send { recv: Some(r), method, block: None, .. }
            if method.as_str().ends_with("_path")
                && !sig.direct_helpers.contains(method.as_str())
                && matches!(&*r.node, ExprNode::Const { path }
                    if path.last().map(|s| s.as_str() == "RouteHelpers").unwrap_or(false))
    );
    if !is_helper_call {
        return;
    }
    let ExprNode::Send { args, method: helper, .. } = &mut *expr.node else { unreachable!() };
    let declared = sig.helper_param_tys.get(helper.as_str()).cloned();
    for (idx, arg) in args.iter_mut().enumerate() {
        let wants_str_here = matches!(
            declared.as_ref().and_then(|tys| tys.get(idx)),
            Some(crate::ty::Ty::Str)
        );
        let Some(model) = arg_record_model(arg, sig) else {
            // The record may ALREADY be projected: the view lowerer's
            // `rewrite_path_arg` turns a local record into `<local>.id`
            // long before this pass runs, so by here the argument is a
            // bare `id` read and `arg_record_model` sees no record at
            // all. Same defect, later in the pipeline — coerce it the
            // same way.
            if wants_str_here && is_bare_id_read(arg) {
                let span = arg.span;
                let inner =
                    std::mem::replace(arg, Expr::new(span, ExprNode::Seq { exprs: vec![] }));
                let mut to_s = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(inner),
                        method: Symbol::from("to_s"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                );
                to_s.ty = Some(crate::ty::Ty::Str);
                *arg = to_s;
            }
            continue;
        };
        // A model that overrides `to_param` answers its slug there and
        // the segment is declared `Str`. Every other model's `to_param`
        // IS `id`, and `param_ty` USUALLY declares that segment `Int` —
        // so call `id` rather than a `to_param` the emitted model
        // doesn't have.
        //
        // …unless the SIGNATURE says otherwise, which is a different
        // question from the one this pass asks. `param_ty` reads
        // CALL-SITE evidence: campfire's `direct :fresh_user_avatar`
        // hands `user.avatar_token` to `:user_avatar`, so that segment
        // is a slug and declared `Str` — while `User` overrides no
        // `to_param`, and the other call site is
        // `user_avatar_path(@user)`. A bare `id` there hands an Int to
        // a String slot. Rails renders a plain record with the DEFAULT
        // `to_param`, which IS `id.to_s`; emit that, and both call
        // sites agree on a text segment.
        //
        // This stayed invisible for as long as `@id` was boxed — a poly
        // interpolates into a path either way. Pinning it (da9c5d89) is
        // what made the contradiction reachable.
        project_record_arg(arg, &model, sig, wants_str_here);
    }
    // A POSITIONAL empty hash (`new_mod_reparent_path({}, id: …)`) is
    // Rails' empty options argument. The generated helper takes its
    // options as keywords only, so the `{}` is an extra positional
    // there — an ArgumentError under CRuby. It says nothing; drop it.
    if let ExprNode::Send { args, .. } = &mut *expr.node {
        args.retain(|a| !matches!(&*a.node,
            ExprNode::Hash { entries, kwargs: false } if entries.is_empty()));
    }
    // Query options (`new_mod_reparent_path({}, id: @reparent_user)`):
    // Rails renders a record there with `to_param` too. The helper's
    // query key is typed to match (routes_to_library::query_param_demand
    // types a record-valued key `String` for a slug model, else `Integer`).
    if let ExprNode::Send { args, .. } = &mut *expr.node {
        if let Some(last) = args.last_mut() {
            if let ExprNode::Hash { entries, kwargs: true } = &mut *last.node {
                for (_, v) in entries.iter_mut() {
                    if let Some(model) = arg_record_model(v, sig) {
                        project_record_arg(v, &model, sig, false);
                    }
                }
            }
        }
    }
}

/// A record in a route helper's argument, as Rails renders it: the
/// model's own `to_param` (a slug model), else its id — as text when
/// the slot is declared `String`.
fn project_record_arg(arg: &mut Expr, model: &str, sig: &RecordSignals<'_>, wants_str: bool) {
    {
        let slug = sig.slug_models.contains(model);
        let span = arg.span;
        let record = std::mem::replace(arg, Expr::new(span, ExprNode::Seq { exprs: vec![] }));
        let (method, ty) = if slug {
            ("to_param", crate::ty::Ty::Str)
        } else {
            ("id", crate::ty::Ty::Int)
        };
        let mut projected = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(record),
                method: Symbol::from(method),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        );
        projected.ty = Some(ty);
        if !slug && wants_str {
            let mut to_s = Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(projected),
                    method: Symbol::from("to_s"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            );
            to_s.ty = Some(crate::ty::Ty::Str);
            projected = to_s;
        }
        *arg = projected;
    }
}

/// A bare `<recv>.id` read with no arguments — what the view lowerer
/// leaves behind for a record handed to a path helper.
fn is_bare_id_read(arg: &Expr) -> bool {
    matches!(&*arg.node,
        ExprNode::Send { recv: Some(_), method, args, block: None, .. }
            if method.as_str() == "id" && args.is_empty())
}

fn arg_record_model(arg: &Expr, sig: &RecordSignals<'_>) -> Option<String> {
    let all_models = sig.all_models;
    let assoc_targets = sig.assoc_targets;
    let collection_targets = sig.collection_targets;
    // (1) A stamped model type.
    if let Some(crate::ty::Ty::Class { id, .. }) =
        arg.ty.as_ref().map(crate::ty::Ty::peel_nilable)
    {
        if all_models.contains(id.0.as_str()) {
            return Some(id.0.as_str().to_string());
        }
    }
    // (5) `a || b` — Rails' "this one, else that one" (campfire's
    // `last_room_visited` is `…find_by(id: cookie) || default_room`).
    // BOTH sides must answer the SAME model: one side resolving proves
    // nothing about the value that actually arrives.
    if let ExprNode::BoolOp { op: crate::expr::BoolOpKind::Or, left, right, .. } = &*arg.node {
        let (l, r) = (arg_record_model(left, sig), arg_record_model(right, sig));
        return match (l, r) {
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        };
    }
    match &*arg.node {
        // (8) `self` inside a model — lobsters'
        // `comment_short_id_path(self)` in Comment#as_json.
        ExprNode::SelfRef => sig.self_model.clone(),
        // (2) A singular association read, or (4) `.first` / `.last` on
        // a has_many read — one record of the collection's type. Both
        // are receiver-ful zero-arg Sends, so they share an arm: a
        // separate `first`/`last` arm placed after this one would be
        // unreachable, since this pattern matches those method names too.
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } if args.is_empty() => {
            if let Some(target) = assoc_targets
                .get(method.as_str())
                .filter(|target| all_models.contains(*target))
            {
                return Some(target.clone());
            }
            // (7b) `self.<method>` — the same self-call as (7) below,
            // with the receiver spelled out. campfire writes
            // `… || self.default_room`, and the emitted controller
            // keeps that spelling.
            if matches!(&*r.node, ExprNode::SelfRef) {
                if let Some(model) = sig.self_returns.get(method.as_str()) {
                    return Some(model.clone());
                }
            }
            // (7c) The helper-module spelling of (7): once a helper
            // method leaves its controller for a module function, the
            // helper lowering rewrites its bare controller-method
            // reads to `ActionController::Current.controller.<method>`
            // — the receiver is by construction the same controller
            // instance (7) resolves against, so the same map answers.
            if let ExprNode::Send { recv: Some(cr), method: cm, args: cargs, block: None, .. } =
                &*r.node
            {
                if cm.as_str() == "controller"
                    && cargs.is_empty()
                    && matches!(&*cr.node, ExprNode::Const { path }
                        if path.last().map(|s| s.as_str() == "Current").unwrap_or(false))
                {
                    if let Some(model) = sig.self_returns.get(method.as_str()) {
                        return Some(model.clone());
                    }
                }
            }
            // (7d) The other helper-module spelling: a state-free
            // controller method gets a class-side twin, and the helper
            // reads `ApplicationController.last_room_visited`. The
            // receiver must NAME a controller — a model's class-side
            // call stays with (6).
            if let ExprNode::Const { path } = &*r.node {
                if path.last().is_some_and(|s| sig.controller_names.contains(s.as_str())) {
                    if let Some(model) = sig.self_returns.get(method.as_str()) {
                        return Some(model.clone());
                    }
                }
            }
            // (6) A model CLASS METHOD that answers one record —
            // campfire's `Current.user.rooms.original`, where `Room
            // .original` is `order(:created_at).first`. The receiver's
            // own type is exactly what does not resolve here; the
            // METHOD NAME is what carries it, on the same uniqueness
            // rule the association maps use.
            if let Some(target) = sig
                .class_method_targets
                .get(method.as_str())
                .filter(|target| all_models.contains(*target))
            {
                return Some(target.clone());
            }
            if !matches!(method.as_str(), "first" | "last") {
                return None;
            }
            let ExprNode::Send { method: assoc, args: aargs, block: None, .. } = &*r.node else {
                return None;
            };
            if !aargs.is_empty() {
                return None;
            }
            collection_targets
                .get(assoc.as_str())
                .filter(|target| all_models.contains(*target))
                .cloned()
        }
        // (4b) `find_by`/`find`/`find_by!` on a has_many read — ONE
        // record of the collection's type, exactly like `.first` above
        // and split out only because those take no arguments. The
        // receiver stays a zero-arg association read: `@room.messages
        // .where(…).find_by(…)` is a chain this pass does not follow.
        ExprNode::Send { recv: Some(r), method, block: None, .. }
            if matches!(method.as_str(), "find" | "find_by" | "find_by!") =>
        {
            let ExprNode::Send { method: assoc, args: aargs, block: None, .. } = &*r.node else {
                return None;
            };
            if !aargs.is_empty() {
                return None;
            }
            collection_targets
                .get(assoc.as_str())
                .filter(|target| all_models.contains(*target))
                .cloned()
        }
        // (3) A bare name that IS a model name — including the
        // zero-arg receiver-less bareword form. A template local
        // (`render "users/user", user: u` → `user` in the partial) and a
        // helper's own reader (`Room::MessagePusher#room`) both parse as
        // a receiver-less Send, not a Var, because prism cannot prove
        // the name is a local inside an ERB-ingested body. Matching only
        // Var/Ivar left campfire's `account_user_path(user)` and
        // `room_path(room)` unconverted — the same owner-form note
        // `scope_chain::owner_model_from_name` carries.
        ExprNode::Var { name, .. } | ExprNode::Ivar { name } => {
            let camel = crate::naming::camelize(name.as_str());
            if all_models.contains(&camel) {
                return Some(camel);
            }
            // (3b) A local whose ASSIGNMENT resolved — campfire's
            // `if last_room = last_room_visited` binds the record
            // behind a name that is no model's. Locals only: an ivar's
            // writes live in other methods this body-scoped map never
            // saw.
            if matches!(&*arg.node, ExprNode::Var { .. }) {
                if let Some(model) = sig.local_records.get(name.as_str()) {
                    return Some(model.clone());
                }
            }
            // (3c) `<qualifier>_<model>` — lobsters' `@reparent_user`,
            // `parent_comment`: the name ends in a model's singular.
            // Longest suffix first, so `mod_mail` is ModMail, not Mail.
            let segs: Vec<&str> = name.as_str().trim_start_matches('@').split('_').collect();
            (1..segs.len())
                .map(|i| crate::naming::camelize(&segs[i..].join("_")))
                .find(|camel| all_models.contains(camel))
        }
        ExprNode::Send { recv: None, method, args, block: None, .. } if args.is_empty() => {
            let camel = crate::naming::camelize(method.as_str());
            if all_models.contains(&camel) {
                return Some(camel);
            }
            // (7) …else a call to one of THIS class's own methods,
            // resolved through what that method's body answers.
            // `redirect_to room_path(last_room_visited)` is the shape:
            // the name is not a model's, and nothing upstream typed it.
            sig.self_returns.get(method.as_str()).cloned()
        }
        _ => None,
    }
}

/// Ruby-family pre-emit pass for TRANSPILED-SHAPE classes only: models
/// whose accessors were hand-written in the source (`attr_accessor
/// :created_at`) and so never passed through the shared model lowering.
/// Schema-synthesized models don't need it — `schema::synth_attr_reader`
/// already splits storage (`@<col>_raw`, String) from access (a reader
/// parsing via `ActiveSupport.parse_db_time`), for every target.
///
/// For the hand-written shape (storage under `@<col>` itself), the
/// reader becomes `@col && ActiveSupport.parse_db_time(@col)` — short-
/// circuits a nullable column's `nil` without needing to know
/// nullability; `parse_db_time` (not bare `Time.parse`) treats a
/// zone-less column as UTC rather than the system's local zone (see
/// `active_support_time_parsing.rb`). The writer becomes
/// `@col = (value.respond_to?(:iso8601) ? value.iso8601 : value)` —
/// normalizing a `Time` passed by app code back to text so every write
/// lands on the same on-disk format. A strict no-op for apps with no
/// Date/DateTime/Time columns (the blog) and for schema-synthesized
/// models (the plain-ivar-read gate below).
pub(crate) fn apply_datetime_lowering(lcs: &mut [LibraryClass], app: &App) {
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else {
            continue;
        };
        let temporal: BTreeSet<Symbol> = table
            .columns
            .iter()
            .filter(|c| {
                matches!(
                    c.col_type,
                    crate::schema::ColumnType::Date
                        | crate::schema::ColumnType::DateTime
                        | crate::schema::ColumnType::Time
                )
            })
            .map(|c| c.name.clone())
            .collect();
        if temporal.is_empty() {
            continue;
        }
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else {
            continue;
        };
        for m in &mut lc.methods {
            if m.receiver != MethodReceiver::Instance {
                continue;
            }
            match m.kind {
                // Only a PLAIN `@col`-read body — the hand-written
                // `attr_reader` shape from transpiled fixtures. A
                // schema-synthesized model's temporal reader already
                // parses its `@<col>_raw` storage ivar (see
                // `schema::synth_attr_reader`); overwriting it here
                // would re-point the read at a nonexistent `@<col>`.
                AccessorKind::AttributeReader
                    if temporal.contains(&m.name)
                        && is_plain_ivar_read(&m.body, &m.name) =>
                {
                    let column = table.columns.iter().find(|c| c.name == m.name).unwrap();
                    let (_, parser, _) = crate::lower::model_to_library::schema::temporal_seam(column);
                    m.body = temporal_reader_body(&m.name, parser);
                }
                // Hand-written temporal writers only, same reasoning:
                // synthesized models write storage via `<col>_raw=`
                // (never named after a temporal column), so any
                // `<col>=` writer matching the temporal set is the
                // transpiled-fixture shape that needs the Time→text
                // normalize.
                AccessorKind::AttributeWriter => {
                    let col = Symbol::from(m.name.as_str().trim_end_matches('='));
                    if temporal.contains(&col) {
                        if let Some(param) = m.params.first() {
                            let column = table.columns.iter().find(|c| c.name == col).unwrap();
                            let (ty, _, formatter) = crate::lower::model_to_library::schema::temporal_seam(column);
                            // Date uses the shared date-only formatter.
                            // Keep the pre-existing legacy timestamp
                            // iso8601 policy, not a Time storage refactor.
                            m.body = temporal_writer_body(&col, &param.name, (ty == Ty::Date).then_some(formatter));
                        }
                    }
                    // The synthesized `<col>_raw=` writer's own store
                    // is invalidated by the general walk below, along
                    // with every OTHER bare write to the same slot.
                }
                // Schema-synthesized temporal reader:
                // `ActiveSupport.parse_db_time(@<col>_raw)`. Profiling
                // the lobsters bench put Date._parse + its regexps at
                // ~4% of wall time — every `created_at` read re-parses
                // the same string. Memoize per instance:
                // `@__t_<col> ||= ActiveSupport.parse_db_time(@<col>_raw)`
                // (writer above invalidates). nil/"" raw re-evaluates
                // each read — parse_db_time's empty path is cheap.
                AccessorKind::Method | AccessorKind::AttributeReader
                    if temporal.contains(&m.name) && is_parse_db_time_body(&m.body) =>
                {
                    let memo = Expr::new(
                        Span::synthetic(),
                        ExprNode::OpAssign {
                            target: LValue::Ivar { name: parse_memo_ivar(&m.name) },
                            op: crate::expr::OpAssignOp::OrOr,
                            value: m.body.clone(),
                        },
                    );
                    // Not the memo as read: Rails converts an attribute into `Time.zone` on every read, and `use_zone` may have moved it.
                    m.body = Expr::new(
                        Span::synthetic(),
                        ExprNode::Send {
                            recv: Some(Expr::new(
                                Span::synthetic(),
                                ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
                            )),
                            method: Symbol::from("present_db"),
                            args: vec![memo],
                            block: None,
                            parenthesized: true,
                        },
                    );
                }
                _ => {}
            }
        }

        // EVERY bare `@<col>_raw = …` invalidates the memo, not just
        // the one inside the `<col>_raw=` writer.
        //
        // `_adapter_reload` is why this is a walk rather than a
        // special case on that writer: it re-reads the row and stores
        // each column with a plain ivar assign (it writes into `self`,
        // where `from_stmt` builds a fresh instance and can go through
        // the writers). The memo survived, so `record.reload.
        // updated_at` answered the value from BEFORE the reload —
        // invisible until something holds one instance across a write,
        // which is exactly what Rails' memoized fixture accessors make
        // routine.
        //
        // Statement positions only (a method body, a `Seq` element, an
        // `If` branch): the replacement is a two-statement `Seq`, and
        // a `Seq` spliced into an expression slot is not Ruby.
        for m in &mut lc.methods {
            if m.receiver != MethodReceiver::Instance {
                continue;
            }
            invalidate_parse_memo_on_raw_writes(&mut m.body, &temporal);
        }

        // The public `<col>=` writer (`self.<col>_raw =
        // ActiveSupport.format_db_time(value)`) is synthesized by the
        // shared model lowering (`schema::synth_temporal_writer`, kind
        // `Method` so the AttributeWriter arm above can't re-point it
        // at a nonexistent `@<col>`) — it arrives here already present,
        // and its raw-writer dispatch picks up the memo invalidation
        // installed above.
    }
}

/// Rewrite every STATEMENT-position `@<col>_raw = …` for a temporal
/// column into `@__t_<col> = nil; @<col>_raw = …`, so the parse memo
/// can never outlive the string it was parsed from.
fn invalidate_parse_memo_on_raw_writes(e: &mut Expr, temporal: &BTreeSet<Symbol>) {
    match &mut *e.node {
        ExprNode::Seq { exprs } => {
            for x in exprs.iter_mut() {
                invalidate_parse_memo_on_raw_writes(x, temporal);
            }
            return;
        }
        ExprNode::If { then_branch, else_branch, .. } => {
            invalidate_parse_memo_on_raw_writes(then_branch, temporal);
            invalidate_parse_memo_on_raw_writes(else_branch, temporal);
            return;
        }
        _ => {}
    }
    let ExprNode::Assign { target: LValue::Ivar { name }, .. } = &*e.node else {
        return;
    };
    let Some(base) = name.as_str().strip_suffix("_raw") else {
        return;
    };
    let base = Symbol::from(base);
    if !temporal.contains(&base) {
        return;
    }
    let span = e.span;
    let store = e.clone();
    *e = Expr::new(
        span,
        ExprNode::Seq {
            exprs: vec![
                Expr::new(
                    span,
                    ExprNode::Assign {
                        target: LValue::Ivar { name: parse_memo_ivar(&base) },
                        value: Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
                    },
                ),
                store,
            ],
        },
    );
}

/// `@__t_<col>` — the per-instance memo slot for a parsed temporal
/// column. Underscore-prefixed so it can't collide with a real column.
fn parse_memo_ivar(col: &Symbol) -> Symbol {
    Symbol::from(format!("__t_{}", col.as_str()))
}

/// Is this body exactly `ActiveSupport.parse_db_time(<anything>)` —
/// the synthesized temporal-reader shape?
fn is_parse_db_time_body(body: &Expr) -> bool {
    matches!(&*body.node, ExprNode::Send { recv: Some(r), method, args, .. }
        if method.as_str() == "parse_db_time"
            && args.len() == 1
            && matches!(&*r.node, ExprNode::Const { path }
                if path.len() == 1 && path[0].as_str() == "ActiveSupport"))
}

/// Ruby-family pre-emit pass: SQL NULL survives hydration as real nil.
///
/// The shared `<Model>Row.from_raw` synthesis coerces every scalar slot
/// (`(row["col"] || 0).to_i`, `(row["col"]).to_s`) so strict targets get
/// non-nilable fields — but on the Ruby tree that turns NULL into 0/""
/// and breaks Rails semantics: `group_by(&:invited_by_user_id)[nil]`
/// finds no root users (the /u tree renders empty), `banned_at?` is
/// true for everyone. For NULLABLE, non-primary-key columns, rewrite
/// the slot assign to `row["col"].nil? ? nil : <original coercion>`.
///
/// The strict targets' belongs_to writers store the 0 sentinel for nil
/// (CRuby/JRuby store nil, `apply_belongs_to_nil_writes`); readers'
/// `@fk == 0` guards are WIDENED to `@fk.nil? || @fk ==
/// 0` so both representations mean "no parent". CRuby-only by
/// placement: strict targets keep the defaulted non-nilable slots
/// until the nullable-column typing workstream lands.
pub(crate) fn apply_hydration_nil_lowering(lcs: &mut [LibraryClass], app: &App) {
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else {
            continue;
        };
        let nullable: BTreeSet<Symbol> = table
            .columns
            .iter()
            .filter(|c| c.nullable && !c.primary_key && c.name.as_str() != "id")
            .map(|c| c.name.clone())
            .collect();
        if nullable.is_empty() {
            continue;
        }
        let row_id = ClassId(Symbol::from(format!("{}Row", model.name.0.as_str())));
        if let Some(row_lc) = lcs.iter_mut().find(|lc| lc.name == row_id) {
            for m in &mut row_lc.methods {
                if m.name.as_str() == "from_raw" {
                    nil_guard_from_raw_slots(&mut m.body, &nullable);
                }
            }
        }
        let nullable_fks: BTreeSet<Symbol> = nullable
            .iter()
            .filter(|n| n.as_str().ends_with("_id"))
            .cloned()
            .collect();
        if nullable_fks.is_empty() {
            continue;
        }
        if let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) {
            for m in &mut lc.methods {
                widen_fk_zero_guards(&mut m.body, &nullable_fks);
                // Polymorphic readers dispatch on the type discriminator,
                // so they have no zero-sentinel guard to widen. Guard their
                // nullable FK here too, before any key-typed adapter call.
                if m.name_span.is_synthetic() {
                    for assoc in model.associations() {
                        if let crate::dialect::Association::BelongsTo {
                            name, foreign_key, polymorphic: true, ..
                        } = assoc {
                            if m.name == *name && nullable.contains(foreign_key) {
                                let cond = Expr::new(Span::synthetic(), ExprNode::Send {
                                    recv: Some(Expr::new(Span::synthetic(), ExprNode::Ivar { name: foreign_key.clone() })),
                                    method: Symbol::from("nil?"), args: vec![], block: None,
                                    parenthesized: false,
                                });
                                m.body = Expr::new(m.body.span, ExprNode::If {
                                    cond,
                                    then_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
                                    else_branch: m.body.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// CRuby/JRuby pre-emit pass: the create and update broadcasts a
/// `broadcasts_to` declares render the partial for the record located
/// again by its id, as Rails' `broadcast_action_later_to` /
/// `broadcast_replace_later_to` job does (the record travels as a
/// GlobalID, `self.class.find(id)`), not for the saving instance. These
/// trees keep the associations a reader loads (`library::
/// apply_collection_memoization`), so a partial reading
/// `article.comments.size` on the saving record would leave that
/// record holding what the render loaded, which Rails' record never
/// holds. The broadcast is the `Broadcasts.<action>(…)` call
/// `model_to_library::broadcasts` folds into `after_create_commit` /
/// `after_update_commit`, which carries the declaration's span; a
/// hand-written `broadcast_*_to` (rendered at once, with the record, in
/// Rails too) is left alone.
pub(crate) fn apply_broadcast_located_record(lcs: &mut [LibraryClass], app: &App) {
    if !super::expr::native_keywords() {
        return;
    }
    for model in &app.models {
        let declarations: Vec<Span> = model
            .body
            .iter()
            .filter_map(|item| match item {
                crate::dialect::ModelBodyItem::Unknown { expr, .. } => match &*expr.node {
                    ExprNode::Send { recv: None, method, .. } if method.as_str() == "broadcasts_to" => Some(expr.span),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if declarations.is_empty() {
            continue;
        }
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else { continue };
        for m in lc.methods.iter_mut().filter(|m| {
            m.receiver == MethodReceiver::Instance
                && matches!(m.name.as_str(), "after_create_commit" | "after_update_commit")
        }) {
            locate_broadcast_records(&mut m.body, &declarations);
        }
    }
}

fn locate_broadcast_records(expr: &mut Expr, declarations: &[Span]) {
    let declared = declarations.contains(&expr.span)
        && matches!(&*expr.node, ExprNode::Send { recv: Some(r), .. }
            if matches!(&*r.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "Broadcasts"));
    if !declared {
        expr.node.for_each_child_mut(&mut |c| locate_broadcast_records(c, declarations));
        return;
    }
    fn locate(e: &mut Expr) {
        e.node.for_each_child_mut(&mut locate);
        let ExprNode::Send { recv: Some(r), args, .. } = &mut *e.node else { return };
        let renders = matches!(&*r.node, ExprNode::Const { path } if path.len() == 2 && path[0].as_str() == "Views");
        if !renders || !matches!(args.as_slice(), [a] if matches!(&*a.node, ExprNode::SelfRef)) {
            return;
        }
        let span = args[0].span;
        let send = |recv: Option<Expr>, method: &str, args: Vec<Expr>| {
            Expr::new(span, ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized: false })
        };
        let class = send(Some(Expr::new(span, ExprNode::SelfRef)), "class", vec![]);
        let mut find = send(Some(class), "find", vec![send(None, "id", vec![])]);
        if let ExprNode::Send { parenthesized, .. } = &mut *find.node {
            *parenthesized = true;
        }
        args[0] = find;
    }
    locate(expr);
}

/// CRuby/JRuby pre-emit pass: writing nil to a `belongs_to` stores nil
/// in its key (and, polymorphic, in its `_type`), so the record saves
/// NULL and reads the association back as nil, as Rails does
/// (`replace_keys(nil)`: `owner[foreign_key] = nil`). The shared writer
/// (`model_to_library::associations`, `synth_belongs_to_writer` /
/// `synth_polymorphic_writer`) stores the typed "no row" sentinel
/// (`0`, `""`) that the strict targets' non-nilable slot needs; those
/// trees keep it. Every `@fk == <sentinel>` guard of a key that
/// `apply_hydration_nil_lowering` does not already widen (a NOT NULL
/// column, which a nil write now reaches too) is widened the same way.
pub(crate) fn apply_belongs_to_nil_writes(lcs: &mut [LibraryClass], app: &App) {
    if !super::expr::native_keywords() {
        return;
    }
    for model in &app.models {
        let mut writers: BTreeSet<Symbol> = BTreeSet::new();
        let mut keys: BTreeSet<Symbol> = BTreeSet::new();
        for assoc in model.associations() {
            let crate::dialect::Association::BelongsTo { name, foreign_key, polymorphic, foreign_type, .. } = assoc
            else {
                continue;
            };
            writers.insert(Symbol::from(format!("{}=", name.as_str())));
            keys.insert(foreign_key.clone());
            if *polymorphic {
                keys.insert(foreign_type.clone().unwrap_or_else(|| Symbol::from(format!("{}_type", name.as_str()))));
            }
        }
        if writers.is_empty() {
            continue;
        }
        // The keys whose guards `apply_hydration_nil_lowering` widens.
        let nullable: BTreeSet<Symbol> = app
            .schema
            .tables
            .get(&model.table.0)
            .map(|t| {
                t.columns
                    .iter()
                    .filter(|c| c.nullable && !c.primary_key && c.name.as_str() != "id" && c.name.as_str().ends_with("_id"))
                    .map(|c| c.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let widen: BTreeSet<Symbol> = keys.iter().filter(|k| !nullable.contains(*k)).cloned().collect();
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else { continue };
        for m in &mut lc.methods {
            if m.receiver == MethodReceiver::Instance && m.name_span.is_synthetic() && writers.contains(&m.name) {
                store_nil_on_nil_write(&mut m.body, &keys);
            }
            widen_fk_zero_guards(&mut m.body, &widen);
        }
    }
}

/// In `if value.nil? … end`'s then-branch, `@<key> = 0` / `@<key> = ""`
/// becomes `@<key> = nil`.
fn store_nil_on_nil_write(expr: &mut Expr, keys: &BTreeSet<Symbol>) {
    expr.node.for_each_child_mut(&mut |c| store_nil_on_nil_write(c, keys));
    let ExprNode::If { cond, then_branch, .. } = &mut *expr.node else { return };
    let tests_nil = matches!(&*cond.node, ExprNode::Send { recv: Some(r), method, args, .. }
        if method.as_str() == "nil?" && args.is_empty() && matches!(&*r.node, ExprNode::Var { name, .. } if name.as_str() == "value"));
    if !tests_nil {
        return;
    }
    fn store_nil(stmt: &mut Expr, keys: &BTreeSet<Symbol>) {
        if let ExprNode::Assign { target: LValue::Ivar { name }, value } = &mut *stmt.node {
            let sentinel = matches!(&*value.node, ExprNode::Lit { value: Literal::Int { value: 0 } })
                || matches!(&*value.node, ExprNode::Lit { value: Literal::Str { value } } if value.is_empty());
            if keys.contains(name) && sentinel {
                *value = Expr::new(value.span, ExprNode::Lit { value: Literal::Nil });
            }
        }
    }
    if let ExprNode::Seq { exprs } = &mut *then_branch.node {
        exprs.iter_mut().for_each(|s| store_nil(s, keys));
    } else {
        store_nil(then_branch, keys);
    }
}

/// Inside a `from_raw` body, rewrite `instance.<col> = <coercion>` for
/// nullable cols to `instance.<col> = <coercion-sans-|| default>`: the
/// `|| 0` an id-shaped column carries is stripped so NULL reaches the
/// coercion, and the coercion itself keeps it alive -- a Cast to a
/// nilable target off a pure read emits `row["col"]&.to_i` (see
/// `emit_cast`). The `if row["col"].nil? then nil else … end` this once
/// wrapped around that was a second guard on the same read, and the
/// `if` was the shape spinel could not type (spinel#4567). A BOOLEAN
/// column keeps the `if`: its cast is `!["0", "", "false"].include?(…)`,
/// not a method on the value, so `&.` has nothing to attach to and NULL
/// would read as false.
fn nil_guard_from_raw_slots(body: &mut Expr, nullable: &BTreeSet<Symbol>) {
    let ExprNode::Seq { exprs } = &mut *body.node else { return };
    for stmt in exprs.iter_mut() {
        let ExprNode::Send { method, args, .. } = &mut *stmt.node else { continue };
        let Some(col) = method.as_str().strip_suffix('=') else { continue };
        if !nullable.contains(&Symbol::from(col)) {
            continue;
        }
        let Some(value) = args.first_mut() else { continue };
        // Only Cast-wrapped scalars coerce; raw slots (bools) already
        // carry nil through.
        let ExprNode::Cast { value: inner, target_ty } = &*value.node else { continue };
        // Strip a `|| <default>` fallback (the id-shaped-column form) so
        // NULL isn't defaulted before the coercion sees it.
        let lookup = match &*inner.node {
            ExprNode::BoolOp { left, right, .. }
                if matches!(&*right.node, ExprNode::Lit { .. }) =>
            {
                left.clone()
            }
            _ => inner.clone(),
        };
        let cast = Expr::new(
            Span::synthetic(),
            ExprNode::Cast { value: lookup.clone(), target_ty: target_ty.clone() },
        );
        let is_bool = matches!(target_ty.peel_nilable(), crate::ty::Ty::Bool);
        *value = if is_bool {
            let nil_check = Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(lookup),
                    method: Symbol::from("nil?"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            );
            Expr::new(
                Span::synthetic(),
                ExprNode::If {
                    cond: nil_check,
                    then_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
                    else_branch: cast,
                },
            )
        } else {
            cast
        };
    }
}

/// `@<fk> == 0` → `@<fk>.nil? || @<fk> == 0` for nullable fks, walking
/// the whole method body (belongs_to reader guards, app-code sentinel
/// checks alike).
fn widen_fk_zero_guards(expr: &mut Expr, fks: &BTreeSet<Symbol>) {
    expr.node.for_each_child_mut(&mut |c| widen_fk_zero_guards(c, fks));
    let is_sentinel_eq = match &*expr.node {
        ExprNode::Send { recv: Some(r), method, args, .. }
            if method.as_str() == "==" && args.len() == 1 =>
        {
            matches!(&*r.node, ExprNode::Ivar { name } if fks.contains(name))
                && (matches!(&*args[0].node, ExprNode::Lit { value: Literal::Int { value: 0 } })
                    || matches!(&*args[0].node, ExprNode::Lit { value: Literal::Str { value } } if value.is_empty()))
        }
        _ => false,
    };
    if !is_sentinel_eq {
        return;
    }
    let ExprNode::Send { recv: Some(r), .. } = &*expr.node else { return };
    let nil_check = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(r.clone()),
            method: Symbol::from("nil?"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let original = expr.clone();
    *expr = Expr::new(
        expr.span,
        ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: crate::expr::BoolOpSurface::Symbol,
            left: nil_check,
            right: original,
        },
    );
}

/// Ruby-family pre-emit pass, companion to
/// `apply_hydration_nil_lowering`: once nullable columns hydrate to
/// real nil, the `.empty?` forms the predicate lowering synthesized
/// from `present?`/`blank?` (which assumed never-nil reads) crash.
/// Rewrite `<recv>.empty?` → `(<recv> || "").empty?` — single
/// evaluation, transparent for arrays/strings that are never nil, and
/// nil reads get Rails' blank-when-nil semantics. Only the
/// zero-arg `empty?` shape is touched.
/// `<time>.rfc2822` → `ActiveSupport.rfc2822(<time>)`.
///
/// `Time#rfc2822` is stdlib `time`, not core Ruby — `require "time"`
/// installs it. CRuby has it because the overlay requires that stdlib;
/// spinel has no `time` package, and the method cannot be supplied by
/// reopening `Time` (a reopened built-in loses its own method table for
/// self-calls, so even `self.strftime` inside the reopen is undefined).
/// So the call grounds to the module function every ruby-family tree
/// ships — the same posture `lower::duration` takes for `Integer#days`,
/// and for the same reason: no built-in reopening.
///
/// Both trees satisfy the one `active_support_time_parsing.rbs`
/// contract. The CRuby/JRuby overlay's implementation delegates
/// straight back to `t.rfc2822`, so grounding the call site cannot move
/// that lane's bytes; the spinel twin composes the same string from
/// strftime.
///
/// Zero-arg receiver sends only. Ruby-emit-path pass: the module
/// function it names is ruby-family runtime, so this must not run from
/// shared `lower/`.
pub(crate) fn apply_time_format_lowering(lcs: &mut [LibraryClass]) {
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            rewrite_rfc2822(&mut m.body);
            rewrite_time_parse(&mut m.body);
        }
    }
}

// Not left as `Time.parse`: spinel ships no stdlib `time` (matz/spinel#1118), so it grounds like `rfc2822` above.
fn rewrite_time_parse(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut |c| rewrite_time_parse(c));
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &mut *expr.node else {
        return;
    };
    if method.as_str() != "parse" || args.len() != 1 {
        return;
    }
    let is_time = |e: &Expr| {
        matches!(&*e.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "Time")
    };
    let target = if is_time(r) {
        "parse_time"
    } else if matches!(&*r.node,
        ExprNode::Send { recv: Some(z), method, args, block: None, .. }
            if method.as_str() == "zone" && args.is_empty() && is_time(z))
    {
        "zone_parse"
    } else {
        return;
    };
    let arg = args[0].clone();
    *expr.node = ExprNode::Send {
        recv: Some(Expr::new(
            Span::synthetic(),
            ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
        )),
        method: Symbol::from(target),
        args: vec![arg],
        block: None,
        parenthesized: true,
    };
}

fn rewrite_rfc2822(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut |c| rewrite_rfc2822(c));
    let ExprNode::Send { recv: Some(r), method, args, .. } = &mut *expr.node else {
        return;
    };
    if method.as_str() != "rfc2822" || !args.is_empty() {
        return;
    }
    let recv = r.clone();
    *expr.node = ExprNode::Send {
        recv: Some(Expr::new(
            Span::synthetic(),
            ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
        )),
        method: Symbol::from("rfc2822"),
        args: vec![recv],
        block: None,
        parenthesized: true,
    };
}

pub(crate) fn apply_nilsafe_empty_lowering(lcs: &mut [LibraryClass]) {
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            rewrite_empty_nilsafe(&mut m.body);
        }
    }
}

/// `::`-root a view body's constant references when the view's own
/// namespace shadows the constant.
///
/// A view emits as `module Views\n  module Stats`, so an unqualified
/// `Stats.get_cached_graph(:users)` inside it resolves LEXICALLY to
/// `Views::Stats` — the view module itself — and not to the top-level
/// `Stats` model the template meant. Ruby finds the inner constant
/// first and raises `NoMethodError: undefined method
/// 'get_cached_graph' for module Views::Stats`.
///
/// This is not a spinel-only concern: the CRuby target emits the same
/// unqualified reference and fails the same way at runtime (GET /stats
/// on lobsters). The AOT lane just surfaces it at compile time.
///
/// The rewrite is deliberately narrow — a head segment is rooted only
/// when it BOTH collides with a namespace visible in the view's lexical
/// scope AND names a real top-level app namespace. Emitted views always
/// spell sibling views fully from the root (`Views::Stories.similar(...)`),
/// so nothing here depends on lexical shorthand and the rooting cannot
/// retarget a reference that was already resolving correctly.
///
/// "Visible in the lexical scope" is the part that was wrong at first:
/// it read the view's OWN segments only. But every view is emitted
/// inside `module Views`, so the scope of ANY view body includes EVERY
/// OTHER view's namespace. campfire's `messages/_message` calls
/// `Users::AvatarsHelper.avatar_tag(...)`; `Views::Users` exists because
/// the app has `app/views/users/` templates, and Ruby stops there —
/// `uninitialized constant Views::Users::AvatarsHelper`, on the page
/// that renders every message.
/// Which shadows this pipeline roots. The view pipeline takes both —
/// every view sits under `module Views` beside every other view. The
/// LIBRARY pipeline takes only the runtime's, because an app class
/// referencing its OWN nested constant (`Sound::Image` inside `class
/// Sound`) already resolves, and rooting it would be churn for nothing.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RootingScope {
    AppAndRuntime,
    RuntimeOnly,
}

/// Root method-body constant references that emission would otherwise shadow,
/// restricting candidate namespaces according to `scope`. A Data factory block
/// retains its enclosing owner's lexical scope, not the generated class's scope.
pub(crate) fn apply_constant_rooting(
    lcs: &mut [LibraryClass],
    app: &App,
    scope: RootingScope,
) {
    // …and "names a real top-level app namespace" has to mean the
    // NAMESPACE, not a class of exactly that name: campfire has no class
    // called `Users`, only `Users::AvatarsHelper` under it.
    //
    // A name the shared RUNTIME owns counts too, and for the same
    // reason a view's does: campfire's `Message::Broadcasts` concern is
    // where the broadcast lowering emits `Broadcasts.append(...)`, and
    // inside `module Broadcasts` that constant resolves to the concern
    // ITSELF. One authority for "is this the runtime's" —
    // `require_path_for_body_const`, which already had to know.
    let runtime_owned = |name: &str| {
        require_path_for_body_const(&[name.to_string()], app, "")
            .is_some_and(|p| p.starts_with("runtime/"))
    };
    let shadows = |name: &str| {
        if scope == RootingScope::RuntimeOnly {
            return runtime_owned(name);
        }
        let prefix = format!("{name}::");
        let under = |n: &str| n == name || n.starts_with(&prefix);
        app.models.iter().any(|m| under(m.name.0.as_str()))
            || app.library_classes.iter().any(|c| under(c.name.0.as_str()))
            || runtime_owned(name)
    };
    // Every `Views::<Seg>` in this emit, which is exactly the set of
    // names a view body can accidentally resolve to.
    let mut view_namespaces: Vec<String> = lcs
        .iter()
        .filter_map(|lc| lc.name.0.as_str().strip_prefix("Views::"))
        .filter_map(|rest| rest.split("::").next())
        // `Views` itself is never the shadow: a `Views::X` reference
        // inside `module Views` finds no `Views::Views` and lands on the
        // top-level one, which is the intended target.
        .filter(|seg| *seg != "Views" && shadows(seg))
        .map(|seg| seg.to_string())
        .collect();
    view_namespaces.sort();
    view_namespaces.dedup();

    // Every class name this emit defines, app and runtime alike — the
    // authority for "does the inner reading have a target?". A class's
    // value constants count as targets too (`Gqlc::Shapes::ALL`): since
    // constant values are rooted like method bodies, a nested source's
    // `SIZE = Shapes::ALL.size` inside `module Gqlc; class Plain` would
    // otherwise read as a compact-form shadow (`Gqlc::Shapes` known,
    // `Gqlc::Shapes::ALL` not) and be rooted to a `::Shapes` that does
    // not exist.
    let class_names: crate::det::HashSet<String> = app
        .models
        .iter()
        .map(|m| m.name.0.as_str().to_string())
        .chain(app.library_classes.iter().map(|c| c.name.0.as_str().to_string()))
        .chain(lcs.iter().map(|c| c.name.0.as_str().to_string()))
        .chain(
            app.library_classes
                .iter()
                .chain(lcs.iter())
                .flat_map(|c| c.constants.iter().map(move |(k, _)| format!("{}::{}", c.name.0.as_str(), k.as_str()))),
        )
        .collect();
    let known = |n: &str| class_names.contains(n);
    for lc in lcs.iter_mut() {
        let name = if matches!(lc.origin, Some(LibraryClassOrigin::DataFactory { .. })) {
            lc.name.0.as_str().rsplit_once("::").unwrap().0
        } else {
            lc.name.0.as_str()
        };
        let mut shadowing: Vec<String> = name
            .split("::")
            .filter(|seg| shadows(seg))
            .map(|seg| seg.to_string())
            .collect();
        if name.starts_with("Views::") {
            shadowing.extend(view_namespaces.iter().cloned());
        }
        shadowing.sort();
        shadowing.dedup();
        // NESTING the emit introduced. A source file that writes the
        // COMPACT form (`module User::Bot`) has lexical scope
        // [User::Bot, Object] — `User`'s own constants are NOT in it.
        // The emit nests (`class User … module Bot`), which puts them
        // in, and campfire's `Bot::WebhookJob` stopped meaning the
        // top-level job and started meaning the concern itself.
        //
        // So: for every enclosing namespace, the constants it defines
        // are candidates for rooting — but only where the inner reading
        // has NO target for the FULL path. `User::Bot` exists, which is
        // the shadow; `User::Bot::WebhookJob` does not, which is what
        // proves the body meant the outer one.
        let prefixes: Vec<String> = {
            let segs: Vec<&str> = name.split("::").collect();
            (1..segs.len()).map(|i| segs[..i].join("::")).collect()
        };
        let scope = LexicalScope { own: name.to_string(), prefixes: &prefixes };
        for m in &mut lc.methods {
            root_shadowed_constants(&mut m.body, &shadowing, &scope, &known);
        }
        // The class body is the same lexical scope as its methods: a
        // constant's value and a class-level `@ivar = …` read their
        // constants exactly as a method body does.
        for (_, value) in &mut lc.constants {
            root_shadowed_constants(value, &shadowing, &scope, &known);
        }
        for init in &mut lc.class_ivar_initializers {
            root_shadowed_constants(init, &shadowing, &scope, &known);
        }
    }
}

/// The lexical scope an emitted class body reads constants in, innermost
/// first: the class itself (`own`), then each namespace the emit opens
/// around it (`prefixes`, outermost first), then Object.
struct LexicalScope<'a> {
    own: String,
    prefixes: &'a [String],
}

impl LexicalScope<'_> {
    /// Innermost first, the class itself included.
    fn innermost_first(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.own.as_str()).chain(self.prefixes.iter().rev().map(String::as_str))
    }
}

fn root_shadowed_constants(
    expr: &mut Expr,
    shadowing: &[String],
    scope: &LexicalScope<'_>,
    known: &dyn Fn(&str) -> bool,
) {
    expr.node
        .for_each_child_mut(&mut |c| root_shadowed_constants(c, shadowing, scope, known));
    let prefixes = scope.prefixes;
    let ExprNode::Const { path } = &mut *expr.node else {
        return;
    };
    let joined = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
    let Some(head) = path.first_mut() else { return };
    // Idempotent: a head already carrying the root marker is skipped, so
    // running the pass twice can't produce `::::Stats`.
    if head.as_str().starts_with("::") {
        return;
    }
    // Emission nests compact declarations such as `class UI::Explicit`
    // under `module UI`. Rubydex already resolved the source reference.
    // Root its actual class when the new nesting would bind the head to
    // another class; a typed VALUE constant never carries this mark.
    //
    // The class's OWN constants are the innermost scope, and analysis
    // writes a resolved reference as its full name, which puts a head
    // there the source never looked up. Core's `lib/counters.rb` nests a
    // `module Counters` inside `module Counters`; the bare `LiveCounters`
    // in `Counters.live` resolves to `Counters::LiveCounters` and, written
    // so inside `module Counters`, reads `Counters::Counters::LiveCounters`:
    // a NameError on every query (the 087_counters.rb subscriber).
    if expr.decisions & crate::expr::RESOLVED_CLASS_REF != 0 {
        if let Some(crate::ty::Ty::Class { id, .. }) = &expr.ty {
            let resolved = id.0.as_str();
            for prefix in scope.innermost_first() {
                if known(&format!("{prefix}::{}", head.as_str())) {
                    if format!("{prefix}::{joined}") != resolved {
                        *path = resolved.split("::").map(Symbol::from).collect();
                        path[0] = Symbol::from(format!("::{}", path[0].as_str()));
                    }
                    return;
                }
            }
        }
    }
    // Either the name's OWN segments shadow it (the pre-existing rule:
    // `Views::Stats` referencing `Stats`, `Message::Broadcasts`
    // referencing the runtime's `Broadcasts`) …
    let by_own_segment = shadowing.iter().any(|s| s == head.as_str());
    // … or an ENCLOSING namespace defines it and the full path under
    // that namespace does not exist, which is the nesting the emit
    // introduced. See the prefix note at the call site.
    let by_enclosing = prefixes.iter().any(|p| {
        known(&format!("{p}::{}", head.as_str())) && !known(&format!("{p}::{joined}"))
    });
    if !by_own_segment && !by_enclosing {
        return;
    }
    // The path renders as `path.join("::")`, so a leading marker on the
    // head segment is all an absolute reference needs — there is no
    // separate "absolute" flag in the IR to set.
    *head = Symbol::from(format!("::{}", head.as_str()));
}

/// Collapse a controller's dynamic-render-options assignment to its bare
/// partial-name string: `@above = {partial: "stories/subnav", locals:
/// {…}}` → `@above = "stories/subnav"`. The matching view (`<%= render
/// @above %>`) dispatches over a `case @above when "stories/subnav" …`
/// pool of string names, so the runtime value must be the string. Upstream
/// lobsters uses both this hash form and the bare-string form (`@above =
/// "saved/subnav"`) — normalizing here lets the view stay string-uniform.
/// The `locals:` sub-hash is dropped HERE; the pool dispatch captures it
/// separately (see the DynPoolEntry / strict-locals kwargs path).
///
/// SCOPE: this is a Ruby-emit-path pass, but the `case @x when "…"` string
/// dispatch it feeds is emitted by the SHARED view lowerer. That's correct
/// only while lobsters (the sole dynamic-partial app) is ruby-only — a
/// strict target rendering `render @above` would see the un-collapsed hash
/// and never match a string arm. Revisit when another target grows dynamic
/// partials (move this collapse to the shared controller lowering then).
pub(crate) fn apply_dynamic_render_options_lowering(lcs: &mut [LibraryClass]) {
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            rewrite_render_options_assign(&mut m.body);
        }
    }
}

fn rewrite_render_options_assign(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut |c| rewrite_render_options_assign(c));
    let ExprNode::Assign { target: crate::expr::LValue::Ivar { .. }, value } = &mut *expr.node
    else {
        return;
    };
    let ExprNode::Hash { entries, .. } = &*value.node else {
        return;
    };
    // Collapse ONLY a render-options hash: symbol keys drawn exactly from
    // {partial, locals}, with a String-literal `partial:`. Gating on the
    // full shape (not merely "contains a partial: key") avoids rewriting an
    // unrelated config hash like `@x = {partial: "y", scope: z}` app-wide —
    // `render @above` is the only consumer of this collapse.
    let mut partial_name: Option<String> = None;
    for (k, v) in entries.iter() {
        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else {
            return; // non-symbol key → not a render-options hash
        };
        match key.as_str() {
            "partial" => match &*v.node {
                ExprNode::Lit { value: Literal::Str { value: s } } => {
                    partial_name = Some(s.clone())
                }
                _ => return, // dynamic partial name — leave the hash intact
            },
            "locals" => {} // allowed; dropped in the collapse (ledgered)
            _ => return, // any foreign key → not a render-options hash
        }
    }
    if let Some(name) = partial_name {
        let span = value.span;
        *value = Expr::new(span, ExprNode::Lit { value: Literal::Str { value: name } });
    }
}

fn rewrite_empty_nilsafe(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut |c| rewrite_empty_nilsafe(c));
    let ExprNode::Send { recv: Some(r), method, args, .. } = &mut *expr.node else {
        return;
    };
    if method.as_str() != "empty?" || !args.is_empty() {
        return;
    }
    // THE GUARD GOES ON THE VALUE, NOT ON THE STRIP. `lower::blank`
    // grounds a String `blank?` as `r.strip.empty?` (ActiveSupport's
    // is a whitespace match, not `empty?`), so the receiver of
    // `empty?` is now `r.strip` at half the sites — and `nil.strip`
    // raises before any guard wrapped around it could answer. Descend
    // one link so a nullable column still reads `(r || "").strip.empty?`.
    // This is the interaction the whitespace form used to be deferred
    // behind; four lines is the whole of it.
    let target: &mut Expr = match &mut *r.node {
        ExprNode::Send { recv: Some(inner), method, args, .. }
            if method.as_str() == "strip" && args.is_empty() =>
        {
            inner
        }
        _ => r,
    };
    // Idempotence: an already-guarded `(x || "").empty?` keeps its shape.
    if matches!(&*target.node, ExprNode::BoolOp { right, .. }
        if matches!(&*right.node, ExprNode::Lit { value: Literal::Str { value } } if value.is_empty()))
    {
        return;
    }
    let guarded = Expr::new(
        target.span,
        ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: crate::expr::BoolOpSurface::Symbol,
            left: target.clone(),
            right: Expr::new(
                target.span,
                ExprNode::Lit { value: Literal::Str { value: String::new() } },
            ),
        },
    );
    *target = guarded;
}

/// True when a reader body is exactly `@<name>` — the untouched
/// hand-written `attr_reader` shape (vs a synthesized parsing body).
fn is_plain_ivar_read(body: &Expr, name: &Symbol) -> bool {
    matches!(&*body.node, ExprNode::Ivar { name: n } if n == name)
}

fn datetime_ivar(col: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Ivar { name: col.clone() })
}

fn datetime_var(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Var { id: VarId(0), name: name.clone() })
}

/// `@col && ActiveSupport.parse_db_time(@col)`.
fn temporal_reader_body(col: &Symbol, parser: &str) -> Expr {
    // `ActiveSupport.parse_db_time` (not bare `Time.parse`) — a stored
    // column with no zone marker is always implicitly UTC (Rails/sqlite3
    // convention), but `Time.parse` defaults an absent zone to the
    // *system's local zone*. See `active_support_time_parsing.rb`.
    let parse_call = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
            )),
            method: Symbol::from(parser),
            args: vec![datetime_ivar(col)],
            block: None,
            parenthesized: true,
        },
    );
    Expr::new(
        Span::synthetic(),
        ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::And,
            surface: crate::expr::BoolOpSurface::Symbol,
            left: datetime_ivar(col),
            right: parse_call,
        },
    )
}

/// `@col = (value.respond_to?(:iso8601) ? value.iso8601 : value)`.
fn temporal_writer_body(col: &Symbol, value_param: &Symbol, date_formatter: Option<&str>) -> Expr {
    if let Some(formatter) = date_formatter {
        return Expr::new(
            Span::synthetic(),
            ExprNode::Assign {
                target: LValue::Ivar { name: col.clone() },
                value: Expr::new(Span::synthetic(), ExprNode::Send {
                    recv: Some(Expr::new(Span::synthetic(), ExprNode::Const {
                        path: vec![Symbol::from("ActiveSupport")],
                    })),
                    method: Symbol::from(formatter),
                    args: vec![datetime_var(value_param)],
                    block: None,
                    parenthesized: true,
                }),
            },
        );
    }
    let responds = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(datetime_var(value_param)),
            method: Symbol::from("respond_to?"),
            args: vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit {
                    value: crate::expr::Literal::Sym { value: Symbol::from("iso8601") },
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    let iso_call = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(datetime_var(value_param)),
            method: Symbol::from("iso8601"),
            args: Vec::new(),
            block: None,
            parenthesized: true,
        },
    );
    let normalized = Expr::new(
        Span::synthetic(),
        ExprNode::If {
            cond: responds,
            then_branch: iso_call,
            else_branch: datetime_var(value_param),
        },
    );
    Expr::new(
        Span::synthetic(),
        ExprNode::Assign {
            target: LValue::Ivar { name: col.clone() },
            value: normalized,
        },
    )
}

/// Emit both the `.rb` file and its `.rbs` sidecar for a single
/// LibraryClass. The sidecar carries the typed-signature view of the
/// same class shape — spinel reads it as an inference hint (see
/// project_rbs_emit_opportunity.md / spinel#571), and Steep/TypeProf
/// can consume it from the CRuby target.
pub(super) fn emit_library_class_pair(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
) -> Vec<EmittedFile> {
    let rb = emit_library_class_decl(lc, app, out_path.clone());
    let rbs = super::rbs::emit_library_class_rbs_with_factories(lc, &out_path, app, &app.library_classes);
    vec![rb, rbs]
}

/// Pair variant for callers that pass synthesized sibling anchors.
pub(super) fn emit_library_class_pair_with_synthesized(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
    synthesized_siblings: &[(String, String)],
) -> Vec<EmittedFile> {
    let rb = emit_library_class_decl_with_synthesized(
        lc,
        app,
        out_path.clone(),
        synthesized_siblings,
    );
    let rbs = super::rbs::emit_library_class_rbs_with_factories(lc, &out_path, app, &app.library_classes);
    vec![rb, rbs]
}

/// A lowered read owns its statement from prepare until finalize, including
/// argument serialization and row hydration. Release within that scope even
/// when the caller rescues inside a longer connection lease. This Ruby-only
/// pass leaves strict-target IR unchanged and covers reloads and preloads too.
fn read_statement_cleanup(lc: &LibraryClass) -> std::borrow::Cow<'_, LibraryClass> {
    fn db_send(expr: &Expr, name: &str) -> bool {
        matches!(&*expr.node, ExprNode::Send { recv: Some(recv), method, .. }
            if method.as_str() == name && matches!(&*recv.node,
                ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "Db"))
    }
    fn prepare(expr: &Expr) -> bool {
        db_send(expr, "prepare") || db_send(expr, "prepare_uncached")
    }
    fn prepared_name(expr: &Expr) -> Option<&Symbol> {
        match &*expr.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } if prepare(value) => Some(name),
            _ => None,
        }
    }
    fn finalize(expr: &Expr, name: &Symbol) -> bool {
        db_send(expr, "finalize") && matches!(&*expr.node,
            ExprNode::Send { args, .. } if args.len() == 1
                && matches!(&*args[0].node, ExprNode::Var { name: actual, .. } if actual == name))
    }
    fn needs_cleanup(expr: &Expr) -> bool {
        let mut found = prepared_name(expr).is_some();
        expr.node.for_each_child(&mut |child| found |= needs_cleanup(child));
        found
    }
    fn rewrite(expr: &mut Expr) {
        expr.node.for_each_child_mut(&mut rewrite);
        let ExprNode::Seq { exprs } = expr.node.as_mut() else { return };
        let mut start = 0;
        while start < exprs.len() {
            if let Some(name) = prepared_name(&exprs[start]) {
                if let Some(end) = (start + 1..exprs.len()).find(|&i| finalize(&exprs[i], name)) {
                    let mut work: Vec<_> = exprs.drain(start + 1..=end).collect();
                    let cleanup = work.pop().unwrap();
                    let body = Expr::new(Span::synthetic(), ExprNode::Seq { exprs: work });
                    exprs.insert(start + 1, Expr::new(Span::synthetic(), ExprNode::BeginRescue {
                        body,
                        rescues: vec![],
                        else_branch: None,
                        ensure: Some(cleanup),
                        implicit: false,
                    }));
                }
            }
            start += 1;
        }
    }
    if !lc.methods.iter().any(|m| needs_cleanup(&m.body)) {
        return std::borrow::Cow::Borrowed(lc);
    }
    let mut adapted = lc.clone();
    for method in &mut adapted.methods {
        rewrite(&mut method.body);
    }
    std::borrow::Cow::Owned(adapted)
}

/// Emit a group of LibraryFunctions sharing a `module_path` as a
/// single Ruby file. Mirrors `typescript::library::emit_module_file`
/// — converts the function group into a synthetic
/// `LibraryClass{is_module:true}` with class-method (`def self.X`)
/// declarations, then delegates to `emit_library_class_decl` so
/// require resolution, nested-module rendering, and method body
/// emission share one code path with class-shaped artifacts.
///
/// `module_function` would be the more idiomatic Ruby spelling,
/// but `def self.X` is what the existing spinel-blog hand-written
/// modules use AND what `emit_method` already produces — going
/// through that path keeps shapes byte-identical.
pub(super) fn emit_module_file(
    funcs: &[crate::dialect::LibraryFunction],
    app: &App,
    out_path: PathBuf,
) -> EmittedFile {
    if funcs.is_empty() {
        // No functions in the module — emit a placeholder file with
        // just the module wrapper. Callers can guard upstream by
        // checking the lowerer's output and not calling this when
        // they know the module would be empty.
        return EmittedFile { path: out_path, content: String::new() };
    }
    let lc = synthesize_module_lc(funcs);
    emit_library_class_decl(&lc, app, out_path)
}

/// Pair variant of `emit_module_file` — emits both `.rb` and `.rbs`.
pub(super) fn emit_module_file_pair(
    funcs: &[crate::dialect::LibraryFunction],
    app: &App,
    out_path: PathBuf,
) -> Vec<EmittedFile> {
    if funcs.is_empty() {
        return vec![EmittedFile { path: out_path, content: String::new() }];
    }
    let lc = synthesize_module_lc(funcs);
    let rb = emit_library_class_decl(&lc, app, out_path.clone());
    let rbs = super::rbs::emit_library_class_rbs(&lc, &out_path, app);
    vec![rb, rbs]
}

/// Emit only the `.rbs` sidecar for a `LibraryClass`. Used when the
/// `.rb` emit has bespoke post-processing the pair helpers can't
/// model (e.g. test files with autorun shim + preamble).
pub(super) fn emit_rbs_sidecar(lc: &LibraryClass, app: &App, rb_path: &std::path::Path) -> EmittedFile {
    super::rbs::emit_library_class_rbs(lc, rb_path, app)
}

/// Emit only the `.rbs` sidecar derived from a `LibraryFunction` group.
/// Companion to `emit_rbs_sidecar` for module-shaped output whose `.rb`
/// emit flows through a bespoke path (e.g. `config/routes.rb`).
pub(super) fn emit_rbs_sidecar_from_funcs(
    funcs: &[crate::dialect::LibraryFunction],
    app: &App,
    rb_path: &std::path::Path,
) -> EmittedFile {
    let lc = synthesize_module_lc(funcs);
    super::rbs::emit_library_class_rbs(&lc, rb_path, app)
}

fn synthesize_module_lc(
    funcs: &[crate::dialect::LibraryFunction],
) -> LibraryClass {
    use crate::dialect::{AccessorKind, MethodDef, MethodReceiver};
    use crate::ident::Symbol;

    let module_id = funcs
        .first()
        .map(|f| {
            ClassId(Symbol::from(
                f.module_path
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("::"),
            ))
        })
        .unwrap_or_else(|| ClassId(Symbol::from("")));
    let methods: Vec<MethodDef> = funcs
        .iter()
        .map(|f| MethodDef {
            unsupported_formals: f.unsupported_formals,
            has_anonymous_block: f.has_anonymous_block,
            name_span: crate::span::Span::synthetic(),
            name: f.name.clone(),
            receiver: MethodReceiver::Class,
            visibility: MethodVisibility::Public,
            params: f.params.clone(),
            body: f.body.clone(),
            signature: f.signature.clone(),
            effects: f.effects.clone(),
            enclosing_class: Some(module_id.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        })
        .collect();
    LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: module_id,
        is_module: true,
        parent: None,
        parent_span: Default::default(),
        includes: Vec::new(),
        methods,
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: Vec::new(),
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
    }
}

#[test]
fn module_adapter_preserves_parameter_declaration_facts() {
    let source = crate::ingest::ingest_library_class(
        b"class Probe; def self.call((a,b)); 7; end; end",
        "probe.rb",
    ).unwrap().unwrap();
    let functions = crate::lower::view_to_library::flatten_lcs_to_functions(&[source]);
    let restored = synthesize_module_lc(&functions);
    assert_eq!(restored.methods[0].unsupported_formals,
        Some(crate::dialect::UnsupportedFormal::Destructured));
}

/// Emit a single library-shape file. `out_path` is the project-root-relative
/// destination for the file; the require resolver computes paths relative to
/// `out_path`'s parent, so files emitted to `app/views/<plural>/` get
/// `../../../runtime/<x>` while files in `app/models/` get `../../runtime/<x>`.
pub(super) fn emit_library_class_decl(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
) -> EmittedFile {
    emit_library_class_decl_with_synthesized(lc, app, out_path, &[])
}

/// Variant that also accepts a list of (class_name, anchor) pairs for
/// synthesized siblings (e.g. `<Model>Row`, `<Resource>Params`) that
/// aren't in `app.library_classes` / `app.models`. Synthesized classes
/// have no separate require chain — nothing else loads them — so a
/// file that references one needs an explicit `require_relative`,
/// even when the target is in the same directory. Callers that don't
/// emit synthesized siblings pass an empty slice.
pub(super) fn emit_library_class_decl_with_synthesized(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
    synthesized_siblings: &[(String, String)],
) -> EmittedFile {
    let guarded = read_statement_cleanup(lc);
    let lc = guarded.as_ref();
    // The one chokepoint every library-shape file goes through, and the
    // only place that knows which class is being emitted — the send
    // emitter is a free function reached from a dozen callers. A reopen
    // of a core class needs its `self.` receivers kept (see the elision
    // in `expr.rs`); everything else is emitted exactly as before.
    let path = out_path.clone();
    super::expr::with_emitted_file(&path, || {
        super::expr::with_core_class_reopen(is_core_class_name(lc.name.0.as_str()), || {
            emit_library_class_decl_inner(lc, app, out_path, synthesized_siblings)
        })
    })
}

/// Render an indented method with named visibility directives that cannot leak
/// to neighboring definitions. Explicitly public constructor/copy hooks need a
/// directive too, because Ruby makes those instance methods private by default.
fn render_library_method(s: &mut String, m: &MethodDef, body_pad: &str) {
    for line in super::emit_method(m).lines() {
        if line.is_empty() {
            writeln!(s).unwrap();
        } else {
            writeln!(s, "{body_pad}{line}").unwrap();
        }
    }
    let implicit_private = m.receiver == MethodReceiver::Instance
        && matches!(m.name.as_str(), "initialize" | "initialize_copy" | "initialize_dup" | "initialize_clone");
    let directive = match (m.receiver, m.visibility) {
        (_, MethodVisibility::Public) if !implicit_private => None,
        (MethodReceiver::Instance, MethodVisibility::Public) => Some("public"),
        (MethodReceiver::Instance, MethodVisibility::Protected) => Some("protected"),
        (MethodReceiver::Instance, MethodVisibility::Private) => Some("private"),
        (MethodReceiver::Class, MethodVisibility::Private) => Some("private_class_method"),
        (MethodReceiver::Class, MethodVisibility::Public) => None,
        (MethodReceiver::Class, MethodVisibility::Protected) => {
            writeln!(s, "{body_pad}class << self").unwrap();
            writeln!(s, "{body_pad}  protected :{}", m.name).unwrap();
            writeln!(s, "{body_pad}end").unwrap();
            None
        }
    };
    if let Some(directive) = directive {
        writeln!(s, "{body_pad}{directive} :{}", m.name).unwrap();
    }
}

/// Assemble a library class's Ruby file with its requires and declarations.
/// Reattach lifted Data methods by declaration span, preserving the factory's
/// position and including dependencies from its methods and parameter defaults.
fn emit_library_class_decl_inner(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
    synthesized_siblings: &[(String, String)],
) -> EmittedFile {
    // A factory class's member readers/writers are the factory's own;
    // ingest synthesized them for analysis only.
    let without_generated;
    let lc = match lc.origin.as_ref().and_then(|o| o.factory_generated_methods()) {
        Some(generated) => {
            let mut copy = lc.clone();
            copy.methods
                .retain(|m| !(m.name_span.is_synthetic() && generated.contains(&m.name)));
            without_generated = copy;
            &without_generated
        }
        None => lc,
    };
    // `delegate :m, to: :t` forwards `(...)` in Ruby; the IR's forwarder
    // is the zero-arg one the strict targets get (`App::ruby_delegate_forwarders`).
    // A declaration ingest declined for the strict targets (it needs
    // argument forwarding) gets its `(...)` forwarders here
    // (`App::ruby_declined_delegate_forwarders`).
    let with_full_forwarders;
    let full = app.ruby_delegate_forwarders.get(&lc.name);
    let declined = app.ruby_declined_delegate_forwarders.get(&lc.name);
    let lc = if full.is_some() || declined.is_some() {
        let mut copy = lc.clone();
        // An alias copy of a forwarder (frozen_record 0.27.4's Scope
        // `alias_method :find_each, :each` below its `delegate ...,
        // :each`) is the same `(...)` forwarder under its name, so it
        // stays the native alias of the one written above it.
        for m in &mut copy.methods {
            // The copy keeps the zero-arg forwarder's body span (its name
            // span is its `alias_method`'s).
            let original = lc
                .method_aliases
                .iter()
                .find(|a| !a.inherited && a.receiver == MethodReceiver::Instance && a.name == m.name)
                .map(|a| &a.original)
                .filter(|o| {
                    lc.methods.iter().any(|f| {
                        &&f.name == o && f.receiver == MethodReceiver::Instance && f.body.span == m.body.span
                    })
                });
            let Some(f) = full
                .into_iter()
                .flatten()
                .find(|f| f.name == m.name)
                .or_else(|| original.and_then(|o| full.into_iter().flatten().find(|f| &f.name == o)))
            else {
                continue;
            };
            // An indexer forwarder (`[]`, `[]=`) has a fixed arity in the
            // IR; its `(...)` twin sits at the same declaration.
            let indexer = matches!(m.name.as_str(), "[]" | "[]=") && m.name_span == f.name_span;
            if m.receiver == MethodReceiver::Instance && (indexer || (m.params.is_empty() && m.block_param.is_none())) {
                *m = crate::dialect::MethodDef { visibility: m.visibility, name: m.name.clone(), ..f.clone() };
            }
        }
        for f in declined.into_iter().flatten() {
            if !copy.methods.iter().any(|m| m.receiver == MethodReceiver::Instance && m.name == f.name) {
                copy.methods.push(f.clone());
            }
        }
        with_full_forwarders = copy;
        &with_full_forwarders
    } else {
        lc
    };
    // A `class_eval` heredoc macro's methods are written by the macro
    // itself when its call is replayed, or when the include whose
    // `included` hook calls it runs (`App::class_eval_macro_expansions`).
    let without_macro_copies;
    let lc = match app.class_eval_macro_expansions.get(&lc.name) {
        Some(expansions) => {
            let replayed: Vec<crate::span::Span> = app_extension_calls(lc, app).iter().map(|c| c.span).collect();
            let dropped: Vec<&Symbol> = expansions
                .iter()
                .filter(|e| e.hook || replayed.contains(&e.call))
                .flat_map(|e| e.methods.iter())
                .collect();
            let mut copy = lc.clone();
            copy.methods.retain(|m| !(m.receiver == MethodReceiver::Instance && dropped.contains(&&m.name)));
            without_macro_copies = copy;
            &without_macro_copies
        }
        None => lc,
    };
    // A concern's nested `module ClassMethods` reaches a library-class
    // includer through `extend`, as in Ruby: the authored `included` hook
    // or (written here) ActiveSupport::Concern's `append_features`. The
    // class-side copies the splice made are analysis's view of that
    // extend, not methods of this class (`App::class_methods_carrier_splices`).
    let carrier_splices = app.class_methods_carrier_splices.get(&lc.name);
    let without_carrier_copies;
    let lc = match carrier_splices {
        Some(splices) if splices.iter().any(|splice| !splice.methods.is_empty()) => {
            let mut copy = lc.clone();
            copy.methods.retain(|m| {
                !(m.receiver == MethodReceiver::Class
                    && splices.iter().any(|splice| splice.methods.contains(&m.name)))
            });
            without_carrier_copies = copy;
            &without_carrier_copies
        }
        _ => lc,
    };
    // A Concern's `class_methods do` defines `ClassMethods` with the
    // block's defs as its instance methods (`App::class_methods_blocks`);
    // the IR's class-side copies on the Concern are that module here.
    let block_spans = app.class_methods_blocks.get(&lc.name).filter(|_| lc.is_module && lc.top_level_constants.is_empty());
    let (class_methods_block, without_block_methods);
    let lc = match block_spans {
        Some(spans) if lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && spans.contains(&m.name_span)) => {
            let in_block = |m: &MethodDef| m.receiver == MethodReceiver::Class && spans.contains(&m.name_span);
            let mut module = lc.clone();
            module.methods.retain(&in_block);
            module.method_aliases.clear();
            for m in &mut module.methods {
                m.receiver = MethodReceiver::Instance;
            }
            let mut rest = lc.clone();
            rest.methods.retain(|m| !in_block(m));
            (class_methods_block, without_block_methods) = (Some(module), rest);
            &without_block_methods
        }
        _ => {
            class_methods_block = None;
            lc
        }
    };
    let spliced_extends: Vec<&crate::ident::ClassId> = carrier_splices
        .into_iter()
        .flatten()
        .filter(|splice| splice.extend)
        .map(|splice| &splice.carrier)
        .collect();
    // A `class_eval` body is no library class the ingest spliced
    // concerns into, so its includes' carriers are found here, each
    // include's extended right after it, as its `append_features` does.
    let mut include_extends: Vec<(&ClassId, Vec<ClassId>)> = Vec::new();
    if name_is_class_eval_body(lc) {
        let mut extended: Vec<ClassId> = Vec::new();
        for inc in &lc.includes {
            let carriers: Vec<ClassId> = concern_carrier_extends(std::slice::from_ref(inc), app)
                .into_iter()
                .filter(|c| !extended.contains(c))
                .collect();
            extended.extend(carriers.iter().cloned());
            if !carriers.is_empty() {
                include_extends.push((inc, carriers));
            }
        }
    } else if app.models.iter().any(|m| m.name == lc.name) {
        include_extends = carrier_included_modules(lc, &spliced_extends, app);
    }
    let carrier_extends: Vec<&crate::ident::ClassId> = spliced_extends
        .iter()
        .copied()
        .chain(include_extends.iter().flat_map(|(_, carriers)| carriers.iter()))
        .collect();
    if !lc.top_level_constants.is_empty() {
        return emit_with_top_level_constants(lc, app, out_path, synthesized_siblings);
    }
    // A model's compiled column accessors go in its own
    // GeneratedAttributeMethods module, included first, as Rails defines
    // them, so a method of the class (the source's, or one a replayed
    // macro defines) overrides them and supers into them.
    let (generated_attributes, without_generated_attributes);
    let lc = match generated_attribute_methods(lc, app) {
        Some((module, rest, retargeted)) => {
            (generated_attributes, without_generated_attributes) = (Some((module, retargeted)), rest);
            &without_generated_attributes
        }
        None => {
            generated_attributes = None;
            lc
        }
    };
    let name = lc.name.0.as_str();
    let out_dir = out_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(PathBuf::new);
    let self_anchor = out_path.with_extension("").to_string_lossy().into_owned();
    let mut s = String::new();

    // A constant whose initializer runs this class's own methods at load
    // time (see `partition_deferred_constants`) drags those method
    // bodies' constant refs into LOAD time with it — `Sound::BUILTIN`
    // calls `initialize`, which reads `Sound::Image`. Those refs would
    // otherwise be classified body-only and left to the aggregator.
    // Only the bodies it runs (`load_time_methods`): a T::Enum's values
    // run `initialize`, not the `graphql_type` that names other files.
    let (mut eager, mut deferred, initializers_call_self) = partition_deferred_constants(lc, app);
    // State the source body writes on the class object (`@value = 1`,
    // `@value ||= 7`) is read by the constants, calls and defs between
    // its writes (`FIRST = @value`), so a body simple enough to keep in
    // source order (`ordered_class_state`) writes its constants, state
    // writes and defs in that order (`state_body` below), not constants,
    // then state, then defs. Every constant is then written where the
    // source has it, after the defs above it; one that calls them, or an
    // initializer that does, runs those method bodies at load.
    let ordered_state = ordered_class_state(lc, app);
    let load_time_methods = load_time_methods(lc, app, &deferred, ordered_state, initializers_call_self);
    if ordered_state {
        eager = (0..lc.constants.len()).collect();
        deferred.clear();
    } else {
        refuse_unordered_class_state(lc);
    }
    let (eager, deferred) = (eager, deferred);

    // Parent + body-derived `require_relative` headers. Helpers return
    // project-root-anchored paths; we relpath each one against `out_dir`
    // so emit works correctly from any output directory.
    let mut requires: Vec<String> = Vec::new();
    if let Some(parent) = lc.parent.as_ref() {
        let written = ClassId(Symbol::from(written_parent(lc, parent, app).as_str()));
        let parent = lexical_parent(name, &written, app);
        let parent = ClassId(Symbol::from(parent.0.as_str().trim_start_matches("::")));
        if let Some(anchor) = require_path_for_parent(&parent, app) {
            if anchor != self_anchor {
                requires.push(relpath(&out_dir, &anchor));
            }
        }
    }
    // A namespaced file reopens its outer segments (`class Account`
    // around `module Joinable`), and the header names their superclass
    // to survive either load order — so this file needs THAT class's
    // parent loaded too, exactly as if it were its own.
    let outer_segments: Vec<&str> = name.split("::").collect();
    for i in 0..outer_segments.len().saturating_sub(1) {
        let qualified = outer_segments[..=i].join("::");
        let Some(outer_parent) = outer_class_parent(&qualified, app) else { continue };
        if let Some(anchor) = require_path_for_parent(&outer_parent, app) {
            if anchor != self_anchor {
                requires.push(relpath(&out_dir, &anchor));
            }
        }
    }
    // A reopen of a runtime framework class (lobsters' `module
    // ActiveRecord; class Base; def q ...`) must load the runtime's
    // definition first — under plain Ruby a bare reopen would otherwise
    // DEFINE an empty class, and under spinel the reopen's method
    // bodies reference runtime members. Parent-less and named into a
    // runtime namespace is the reopen signature.
    if lc.parent.is_none() {
        if let Some(anchor) = runtime_reopen_anchor(name) {
            requires.push(relpath(&out_dir, anchor));
        }
    }
    // The same rule for a namespace the runtime owns through a require
    // anchor rather than that table: the header below is the COMPOUND
    // `class Net::HTTP` whenever `Net` resolves to `runtime/net_http`,
    // and a compound header looks the outer constant up instead of
    // creating it. Nothing else loads that file first — boot.rb carries
    // no line for it, and lobsters' `extras/sponge.rb` reopen is loaded
    // through the models aggregator — so the file that needs the
    // constant requires the file that provides it.
    let outer_runtime_anchor = (outer_segments.len() > 1)
        .then(|| require_path_for_body_const(&[outer_segments[0].to_string()], app, ""))
        .flatten()
        .filter(|p| p.starts_with("runtime/"));
    if let Some(anchor) = outer_runtime_anchor.as_deref() {
        let rel = relpath(&out_dir, anchor);
        if !requires.contains(&rel) {
            requires.push(rel);
        }
    }
    // What the header itself needs (the superclasses, a runtime reopen's
    // file) is required above it; the body's needs follow.
    let header_requires: BTreeSet<String> = requires.iter().cloned().collect();
    // `include`d modules must be LOADED before the `include` executes at
    // class-definition time — unlike body const-refs (request-time), so we
    // require them even when they're same-dir siblings (plain Ruby has no
    // Rails autoload). Resolve through the same model/library_class anchor.
    for inc in lc.includes.iter().chain(carrier_extends.iter().copied()) {
        // SPLIT on `::` — an include's ClassId is one Symbol holding the
        // whole path, and the const resolver keys on the ROOT segment.
        // Unsplit, `Turbo::Streams::StreamName::ClassMethods` was its
        // own root, matched nothing, and campfire's guarded channel
        // included a module with no require to load it.
        let path: Vec<String> = inc.0.as_str().split("::").map(str::to_string).collect();
        if let Some(anchor) = require_path_for_body_const(&path, app, name) {
            if anchor != self_anchor {
                requires.push(relpath(&out_dir, &anchor));
            }
        }
    }
    let mixin_requires: BTreeSet<String> = requires.iter().filter(|r| !header_requires.contains(*r)).cloned().collect();
    // Require-edge classification: LOAD-time refs vs body-only refs.
    //
    // Class-body constant initializers execute while the file is being
    // required (lobsters' markdowner.rb interpolates
    // `User::VALID_USERNAME` into a class-body regexp constant), so
    // their targets need explicit requires — same footing as the
    // parent and `include`s above.
    //
    // Method-body refs resolve at request time, after boot completes.
    // Their `app/models/*` targets load through the `app/models.rb`
    // aggregator (required from main.rb / test_helper.rb before any
    // dispatch — see `apply_models_aggregator`), NOT through per-file
    // requires. Emitting requires for them is what broke the lobsters
    // boot: user.rb's body-only edge to markdowner.rb closed a cycle
    // against markdowner's genuine load-time need on user.rb, and
    // `require_relative`'s mid-load short-circuit left `User` undefined
    // where the class body read it. Non-aggregated anchors (runtime/*,
    // app/views, app/controllers/*, test/fixtures/*) keep their
    // requires — nothing else loads those.
    // Resolve one referenced constant path to the require it needs, or
    // None: no anchor, this file itself, or a body-only ref into
    // `app/models/*` (which the aggregator loads).
    let resolve = |path: &Vec<String>, load_time: bool| -> Option<String> {
        let first = path.first()?;
        // Synthesized siblings (`<Model>Row`, `<Resource>Params`,
        // `<Plural>Fixtures`) match by exact first-segment name; deeper
        // paths (`X::Y`) don't match here since synthesized classes are
        // flat. Their anchors flow through the same aggregator gate
        // below (model-dir synthesized classes are in `app/models.rb`;
        // fixture siblings keep explicit requires).
        let anchor = synthesized_siblings
            .iter()
            .find(|(n, _)| n == first)
            .map(|(_, a)| a.clone())
            .or_else(|| require_path_for_body_const(path, app, name))?;
        if !load_time && anchor.starts_with("app/models/") {
            return None;
        }
        (anchor != self_anchor).then(|| relpath(&out_dir, &anchor))
    };
    let resolve_all = |e: &Expr, load_time: bool| -> BTreeSet<String> {
        let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
        walk_const_paths(e, &mut paths);
        paths.iter().filter_map(|p| resolve(p, load_time)).collect()
    };
    // Code that runs as the file loads (a constant's value, a
    // class-variable initializer, a method body it runs), whose stored
    // closures do not: `OPTIONS = { type: proc { Widget } }` reads
    // `Widget` when the proc is called (`walk_load_time_paths`).
    let resolve_load = |e: &Expr| -> BTreeSet<String> {
        let mut now: BTreeSet<Vec<String>> = BTreeSet::new();
        let mut later: BTreeSet<Vec<String>> = BTreeSet::new();
        walk_load_time_paths(e, &mut now, &mut later);
        let mut out: BTreeSet<String> = now.iter().filter_map(|p| resolve(p, true)).collect();
        out.extend(later.iter().filter(|p| !now.contains(*p)).filter_map(|p| resolve(p, false)));
        out
    };
    // Every require the file needs at its TOP: the class-body calls and
    // deferred constants (both run at load), and the method bodies.
    let mut body_requires: BTreeSet<String> = BTreeSet::new();
    // A carried railtie's or engine's body statements reach the app's
    // constants through app/models.rb's autoload, each under its own
    // `run_body_statement`, so a constant a statement reads loads when
    // the statement runs and a file that cannot load is named there.
    // Most of them sit in blocks Rails runs later (`initializer`,
    // `config.to_prepare`); a require at the top of the file loaded
    // their files when the engine loaded.
    //
    // Only the calls the body replays (`replays_foreign_class_body`,
    // `app_extension_calls`): a dropped one is not emitted, so what it
    // names is no load-time need, and a require for it can close a
    // cycle the source does not have. A closure a replayed call only
    // stores (`scope :x, -> { Plan.active }`, `if: -> { ... }`) runs
    // later, so what its body names resolves like a method body's.
    let railtie = carried_railtie(lc, app);
    let replays_all = replays_foreign_class_body(lc, app);
    // The same calls the body writes below (`replayed` there): a
    // lowered `mixes_in_class_methods` runs in every body, and reads its
    // modules at load (widget/describable.rb's `ClassMethods`, a plain
    // gem's nested module no autoload defines). A `prepend` of an app
    // module too: essentials' patches/money_ext.rb reopens the
    // shopify-money gem's `Money` to `prepend MoneyOverride`, the reopen
    // is written in the gem's class file, and nothing else loads
    // MoneyOverride's file before it runs.
    let own_calls: Vec<&Expr> = if railtie || replays_all {
        Vec::new()
    } else {
        let mut calls = app_extension_calls(lc, app);
        calls.extend(app_prepend_calls(lc, app).into_iter().map(|(call, _)| call));
        // A replayed `validates_with SomeValidator` reads its validator
        // at load.
        calls.extend(active_model_validation_calls(lc, app));
        calls
    };
    let replayed = |call: &&Expr| {
        replays_all
            || crate::ingest::sorbet_check::is_mixes_in(call)
            || own_calls.iter().any(|c| std::ptr::eq(*c, *call))
    };
    for call in lc.unknown_calls.iter().filter(|_| !railtie).filter(replayed) {
        let mut now: BTreeSet<Vec<String>> = BTreeSet::new();
        let mut later: BTreeSet<Vec<String>> = BTreeSet::new();
        walk_load_time_paths(call, &mut now, &mut later);
        body_requires.extend(now.iter().filter_map(|p| resolve(p, true)));
        body_requires.extend(later.iter().filter(|p| !now.contains(*p)).filter_map(|p| resolve(p, false)));
    }
    // A body statement the file keeps where the source has it
    // (`carries_body_statement`) runs as the body loads, after the
    // `require`s above it: `Format.register_lazy(...)` after a body
    // `require "x/format"` reads the class that file defines. The body
    // `require` itself is dropped (the tree builds its own require graph),
    // so what the statement names loads at the top of the file.
    for call in lc.unknown_calls.iter().filter(|call| carries_body_statement(lc, call, app)) {
        body_requires.extend(resolve_load(call));
    }
    for &i in &deferred {
        body_requires.extend(resolve_load(&lc.constants[i].1));
    }
    // A class-variable initializer runs at load, after the constants
    // (an ordered body's go with its constants, below).
    for init in lc.class_ivar_initializers.iter().filter(|_| !ordered_state) {
        body_requires.extend(resolve_load(init));
    }
    for (i, m) in lc.methods.iter().enumerate() {
        if load_time_methods.as_ref().is_none_or(|run| run.contains(&i)) {
            body_requires.extend(resolve_load(&m.body));
        } else {
            body_requires.extend(resolve_all(&m.body, false));
        }
    }
    let factory_methods = app.library_classes.iter().filter(|class| {
        matches!(class.origin, Some(LibraryClassOrigin::DataFactory { declaration_span })
            if lc.constants.iter().any(|(_, value)| value.span == declaration_span))
    }).flat_map(|class| &class.methods);
    for m in factory_methods {
        body_requires.extend(resolve_all(&m.body, false));
        for default in m.params.iter().filter_map(|param| param.default.as_ref()) {
            body_requires.extend(resolve_all(default, false));
        }
    }
    // …and each eager constant's own, in order, because WHERE those go
    // matters. A class body runs top to bottom, and a required file can
    // read this one's earlier constants — campfire's `ContentFilters`
    // defines `EDITOR_FORMATTING_TAGS`, then builds
    // `TextMessagePresentationFilters` from `SanitizeTags`, whose own
    // class body reads `ContentFilters::EDITOR_FORMATTING_TAGS`. Rails
    // autoloads `SanitizeTags` at that reference, after the constant
    // exists; a require at the top of the file ran it first, and
    // `require_relative`'s mid-load short-circuit left the constant
    // undefined. So the first constant (after at least one other) that
    // needs a file nothing earlier needs SPLITS the body: the file
    // closes, requires what the rest needs, and reopens. Spinel splices
    // a `require_relative` where it stands, so both lanes read the
    // same order.
    //
    // Only when there IS such a reader: a split for a file that never
    // looks at this class's constants is churn (lobsters' `Markdowner`
    // requiring `user`), so the trigger is a required file whose own
    // load-time code reads one of the constants above it.
    //
    // An ordered body (`ordered_class_state`) runs its constants, state
    // writes and defs in source order (`state_body`), and splits before
    // each constant or state write that first needs a file reading an
    // earlier constant, requiring only what that item needs: a later
    // dependency waits for its own use, as Rails' autoload has it.
    let state_body: Vec<StateItem> = if ordered_state { ordered_state_items(lc, &eager) } else { Vec::new() };
    let mut state_splits: std::collections::BTreeMap<usize, BTreeSet<String>> = std::collections::BTreeMap::new();
    let per_const: Vec<BTreeSet<String>> = if ordered_state {
        Vec::new()
    } else {
        eager.iter().map(|&i| resolve_load(&lc.constants[i].1)).collect()
    };
    let readers = constant_readers(lc, app, &resolve);
    if ordered_state {
        let per_body: Vec<BTreeSet<String>> = state_body
            .iter()
            .map(|item| match item {
                StateItem::Constant(i) => resolve_load(&lc.constants[*i].1),
                StateItem::Initializer(i) => resolve_load(&lc.class_ivar_initializers[*i]),
                StateItem::Method(_) => BTreeSet::new(),
            })
            .collect();
        for p in 1..per_body.len() {
            let mut before: BTreeSet<String> = body_requires.clone();
            before.extend(requires.iter().cloned());
            for q in &per_body[..p] {
                before.extend(q.iter().cloned());
            }
            let earlier: BTreeSet<&String> = state_body[..p]
                .iter()
                .filter_map(|item| match item {
                    StateItem::Constant(i) => readers.get(lc.constants[*i].0.as_str()),
                    _ => None,
                })
                .flatten()
                .collect();
            if per_body[p].iter().any(|r| !before.contains(r) && earlier.contains(r)) {
                let needed: BTreeSet<String> = per_body[p].iter().filter(|r| !before.contains(*r)).cloned().collect();
                state_splits.insert(p, needed);
            }
        }
        let split_requires: BTreeSet<&String> = state_splits.values().flatten().collect();
        for r in per_body.iter().flatten() {
            if !split_requires.contains(r) {
                body_requires.insert(r.clone());
            }
        }
    }
    let mut split: Option<(usize, BTreeSet<String>)> = None;
    // A factory class cannot reopen: its header builds the class.
    let factory_class = matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. }));
    for p in (1..per_const.len()).filter(|_| !factory_class) {
        let mut before: BTreeSet<String> = body_requires.clone();
        before.extend(requires.iter().cloned());
        for q in &per_const[..p] {
            before.extend(q.iter().cloned());
        }
        let earlier: BTreeSet<&String> = eager[..p]
            .iter()
            .filter_map(|&i| readers.get(lc.constants[i].0.as_str()))
            .flatten()
            .collect();
        if per_const[p].iter().any(|r| !before.contains(r) && earlier.contains(r)) {
            let rest: BTreeSet<String> = per_const[p..]
                .iter()
                .flatten()
                .filter(|r| !before.contains(*r))
                .cloned()
                .collect();
            split = Some((p, rest));
            break;
        }
    }
    for (p, reqs) in per_const.iter().enumerate() {
        for r in reqs {
            let deferred_here = split.as_ref().is_some_and(|(at, rest)| p >= *at && rest.contains(r));
            if !deferred_here {
                body_requires.insert(r.clone());
            }
        }
    }
    body_requires.retain(|r| !requires.contains(r));
    requires.extend(body_requires);
    // A body need in this file's own load-time require cycle
    // (`load_cycle_components`) is required inside the body, right after
    // the header, where natively its autoload runs: this class is then
    // defined, and the file it loads, reaching back for it, finds it
    // (`require_line`'s guard) instead of requiring this file while it
    // loads, Ruby's circular require. Core's payments/vault
    // LegacyPaymentInstrument: its `CLASS_MAPPING` reads the classes
    // that include it, each of which requires it at the top of its file.
    let in_body: Vec<String> = if super::expr::native_keywords() {
        let cycles = load_cycle_components(app);
        let own = cycles.get(&self_anchor);
        requires
            .iter()
            .filter(|r| !header_requires.contains(*r))
            .filter(|r| own.is_some() && cycles.get(&anchor_of_relative(&out_dir, r)) == own)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    requires.retain(|r| !in_body.contains(r));
    // Only an include's own module loads above the includes; the rest
    // load below them, where the body first reads them natively. Core's
    // GraphApi::Admin::Home::Card names its card classes in
    // `orphan_types` after `include GraphApi::InterfaceType`, and each
    // card class `implements` Card as it loads: required above the
    // include, Card was no GraphQL interface yet.
    let (in_body, in_body_after_includes): (Vec<String>, Vec<String>) =
        in_body.into_iter().partition(|r| mixin_requires.contains(r));
    // The source's own top-level requires first, as Ruby runs them
    // before the body (`authored_requires`). A load-time need that one
    // of them already loads is not repeated.
    let (authored, after, trailing) = authored_requires(lc, app, &out_dir, &self_anchor);
    for line in &authored {
        writeln!(s, "{line}").unwrap();
    }
    for r in &requires {
        if authored.iter().any(|a| split_require_relative(a).is_some_and(|(target, _)| target == r)) {
            continue;
        }
        writeln!(s, "{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
    }
    for line in &after {
        writeln!(s, "{line}").unwrap();
    }
    if !requires.is_empty() || !authored.is_empty() || !after.is_empty() {
        writeln!(s).unwrap();
    }

    // Compound names like `Views::Articles` emit as nested
    // `module Views\n  module Articles` rather than `module Views::Articles`.
    // Compound-form headers blow up at load time when the outer namespace
    // isn't already defined (Ruby looks up `Views` as a constant); nested
    // headers create the chain on the fly. Spinel-blog's hand-written
    // views use the nested form for the same reason.
    let mut segments: Vec<&str> = name.split("::").collect();
    // …UNLESS an outer segment belongs to the shared RUNTIME, where we
    // cannot know whether to reopen it as `class` or `module` and the
    // wrong guess is a TypeError at load. campfire extends Action Text
    // with `class ActionText::Attachment::OpengraphEmbed`;
    // `ActionText::Attachment` is a CLASS in runtime/action_text.rb, and
    // nesting emitted `module Attachment` around it.
    //
    // The COMPOUND header the source itself wrote needs no such
    // knowledge — it only requires the outer constants to already
    // exist, which the `require_relative` of that runtime file written
    // above guarantees. That is also why this is not the default:
    // `Views::Articles` has no owner but the nesting itself, and a
    // compound header there would look up a `Views` nothing created.
    if outer_runtime_anchor.is_some() {
        segments = vec![name];
    }
    let segments = segments;
    let depth = segments.len();
    let body_pad = "  ".repeat(depth);

    // An outer segment that names a known app CLASS must reopen as
    // `class`, not `module` — lobsters nests `CandidateId` inside the
    // `ShortId` model, and `module ShortId` after the model file loaded
    // is a TypeError under Ruby. (Aggregator order guarantees the owner
    // file loads first: `x.rb` sorts before `x/y.rb` because '.' < '/'.)
    // …and it must reopen with the SAME superclass, because the load
    // order the aggregator guarantees is not the only one: `account.rb`
    // requires `account/joinable` at its top so the `include` resolves,
    // which runs the nested file FIRST. A bare `class Account` there
    // creates it under Object, and the real `class Account <
    // ApplicationRecord` then dies with "superclass mismatch". Naming
    // the parent in both places makes the order irrelevant.
    //
    // Resolution is by QUALIFIED prefix, not bare segment:
    // `Opengraph::Fetch::RedirectDeniedError` nests inside the CLASS
    // `Opengraph::Fetch`, and looking up a bare `Fetch` finds nothing
    // ("Fetch is not a module" at load).
    //
    // A test file's helper class nesting another reopens that helper
    // (`test_file_outer_class`), with the parent it was written with.
    let outer_header = |i: usize, seg: &str| {
        let qualified = segments[..=i].join("::");
        if let Some(sibling) = test_file_outer_class(lc, &qualified, app) {
            return match sibling.parent.as_ref() {
                Some(p) => format!(
                    "class {seg} < {}",
                    super::expr::singleton_scoped_path(&lexically_qualified(&written_parent(sibling, p, app), &segments[..i], app))
                ),
                None => format!("class {seg}"),
            };
        }
        let parent = outer_class_parent(&qualified, app);
        match (is_app_class(&qualified, app), parent) {
            (true, Some(p)) => {
                // As the class's own header writes it: `class Rails <
                // ::Rails::Railtie` inside `module Measured` stays rooted,
                // since a bare `Rails` there is the `Measured::Rails` being
                // reopened.
                let written = library_classes_named(&qualified, app)
                    .into_iter()
                    .find(|c| !c.is_module)
                    .map_or_else(|| p.0.as_str().to_string(), |c| written_parent(c, &p, app));
                format!("class {seg} < {}", super::expr::singleton_scoped_path(&lexically_qualified(&written, &segments[..i], app)))
            }
            (true, None)
                if library_classes_named(&qualified, app)
                    .into_iter()
                    .any(crate::ingest::library_class::is_lowered_sorbet_enum) =>
            {
                format!("class {seg} < (defined?(::T::Enum) ? ::T::Enum : ::Object)")
            }
            (true, None) => format!("class {seg}"),
            (false, _) => format!("module {seg}"),
        }
    };

    let open_header = |s: &mut String| {
        if lc.is_module {
            // Modules don't take a parent; ingest already enforces this.
            for (i, seg) in segments.iter().enumerate() {
                let header = if i < depth - 1 {
                    outer_header(i, seg)
                } else {
                    format!("module {seg}")
                };
                writeln!(s, "{}{header}", "  ".repeat(i)).unwrap();
            }
        } else {
            // Outer segments (if any) are namespace modules; the last is the class.
            for (i, seg) in segments.iter().take(depth - 1).enumerate() {
                writeln!(s, "{}{}", "  ".repeat(i), outer_header(i, seg)).unwrap();
            }
            let last = segments[depth - 1];
            let pad = "  ".repeat(depth - 1);
            match (&lc.origin, lc.parent.as_ref()) {
                // `X = Struct.new(:a) do … end`: the factory builds the
                // class and evaluates the body below as its body.
                (Some(crate::dialect::LibraryClassOrigin::FactoryClass { factory }), _) => {
                    writeln!(s, "{pad}{last} = {} do", super::emit_expr(factory)).unwrap()
                }
                (_, Some(p)) => {
                    // As written (`written_parent`: a rooted `::X` stays
                    // rooted), then qualified against the emitted nesting.
                    let written = written_parent(lc, p, app);
                    // A parent declared in a `class << self` is read off
                    // its owner's singleton class (`singleton_scoped_path`).
                    let parent = super::expr::singleton_scoped_path(&lexically_qualified(&written, &segments[..depth - 1], app));
                    writeln!(s, "{pad}class {last} < {parent}").unwrap()
                }
                // A lowered `T::Enum` subclasses the real one where
                // sorbet-runtime is loaded (`is_lowered_sorbet_enum`).
                (_, None) if crate::ingest::library_class::is_lowered_sorbet_enum(lc) => {
                    writeln!(s, "{pad}class {last} < (defined?(::T::Enum) ? ::T::Enum : ::Object)").unwrap()
                }
                (_, None) => writeln!(s, "{pad}class {last}").unwrap(),
            }
        }
    };
    open_header(&mut s);
    for r in &in_body {
        writeln!(s, "{body_pad}{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
    }
    if !in_body.is_empty() {
        writeln!(s).unwrap();
    }
    if let Some((module, retargeted)) = &generated_attributes {
        let inner_pad = format!("{body_pad}  ");
        writeln!(s, "{body_pad}module GeneratedAttributeMethods").unwrap();
        for index in 0..module.methods.len() {
            if index > 0 {
                writeln!(s).unwrap();
            }
            write_library_method(&mut s, module, index, &inner_pad, &[]);
        }
        if !retargeted.is_empty() {
            writeln!(s).unwrap();
        }
        for writer in retargeted {
            writeln!(s, "{inner_pad}alias_method :{}, :{}", generated_writer_alias(writer).as_str(), writer.as_str()).unwrap();
        }
        writeln!(s, "{body_pad}end").unwrap();
        writeln!(s, "{body_pad}private_constant :GeneratedAttributeMethods").unwrap();
        writeln!(s, "{body_pad}include GeneratedAttributeMethods").unwrap();
        writeln!(s).unwrap();
    }
    if let Some(module) = &class_methods_block {
        let inner_pad = format!("{body_pad}  ");
        writeln!(s, "{body_pad}module ClassMethods").unwrap();
        for index in 0..module.methods.len() {
            if index > 0 {
                writeln!(s).unwrap();
            }
            write_library_method(&mut s, module, index, &inner_pad, &[]);
        }
        writeln!(s, "{body_pad}end").unwrap();
        writeln!(s).unwrap();
    }

    let write_extend = |s: &mut String, carrier: &ClassId| {
        writeln!(s, "{body_pad}extend {}", lexically_qualified(carrier.0.as_str(), &segments, app)).unwrap();
    };
    let extends_of = |inc: &ClassId| -> Vec<ClassId> {
        include_extends.iter().filter(|(i, _)| *i == inc).flat_map(|(_, c)| c.iter().cloned()).collect()
    };
    // The `class_attribute`s an include's Concern declares in its
    // `included do` (`App::included_class_attributes`): their defs and
    // seeds are written right after that `include`, where Ruby runs the
    // block, so a class-body macro below it (`retries 3` writing
    // `retry_limit`) finds the writer and writes over the seeded
    // default. A group whose include this body does not write keeps the
    // general placement.
    let root_of = |id: &ClassId| id.0.as_str().trim_start_matches("::").to_string();
    let carried_attributes: Vec<&crate::app::IncludedClassAttributes> = if railtie {
        Vec::new()
    } else {
        app.included_class_attributes
            .get(&lc.name)
            .into_iter()
            .flatten()
            .filter(|g| lc.includes.iter().any(|inc| root_of(inc) == root_of(&g.after)))
            .filter(|g| app.gem_boundary.refused_include(&lc.name, &g.after).is_none())
            .collect()
    };
    let carried_method = |m: &crate::dialect::MethodDef| {
        carried_attributes.iter().any(|g| g.methods.iter().any(|(receiver, name)| *receiver == m.receiver && *name == m.name))
    };
    let carried_seed = |e: &Expr| carried_attributes.iter().any(|g| g.seeds.contains(&e.span));
    // The Concerns an include runs (itself and, transitively, what its
    // module includes): their spliced `ClassMethods` extends go right
    // after an include with carried attributes, ahead of its seeds, which
    // may call one (`self.jitter = default_jitter`).
    let reached_by = |root: &str| -> Vec<String> {
        let mut out = vec![root.to_string()];
        let mut at = 0;
        while at < out.len() {
            let name = out[at].clone();
            at += 1;
            for m in app.library_classes.iter().filter(|m| m.is_module && root_of(&m.name) == name) {
                for inc in &m.includes {
                    let id = root_of(inc);
                    if !out.contains(&id) {
                        out.push(id);
                    }
                }
            }
        }
        out
    };
    // Class-body calls the ingest didn't model (`LibraryClass::
    // unknown_calls`), replayed verbatim when — and only when — the
    // class extends a base we don't model at all, or reopens a loaded
    // gem's class. See `replays_foreign_class_body`.
    //
    // Never an analyzer include refusal (`is_include_refusal`): a ledger
    // record for check, not a statement. The include it names is written
    // above (`write_include`), where the source has it; replayed, the
    // record raised again at the end of a body whose include had run.
    let replayed: Vec<&Expr> = if railtie || replays_foreign_class_body(lc, app) {
        lc.unknown_calls
            .iter()
            .filter(|c| !crate::analyze::is_include_refusal(c) && !is_body_statement(c))
            .collect()
    } else {
        for call in dropped_class_body_calls(lc, app) {
            report_dropped_class_body_call(lc, call);
        }
        modelled_class_body_calls(lc, app).into_iter().filter(|c| !is_body_statement(c)).collect()
    };
    // The body statements the ingest kept (`carries_body_statement`),
    // written where the source has them on CRuby/JRuby: among the other
    // replayed calls when no def is above them, else just before the
    // first def below them (`positioned_statements`). Every other target
    // refuses them by name (`project::report_body_statements`).
    let mut positioned_statements: Vec<(usize, &Expr)> = Vec::new();
    let mut flowing_statements: Vec<&Expr> = Vec::new();
    if super::expr::native_keywords() && !railtie {
        for call in lc.unknown_calls.iter().filter(|c| is_body_statement(c)) {
            match body_statement_place(lc, call, app) {
                Some(Some(at)) => positioned_statements.push((at, call)),
                Some(None) => flowing_statements.push(call),
                None => {}
            }
        }
    }
    let replayed: Vec<&Expr> = if flowing_statements.is_empty() {
        replayed
    } else {
        lc.unknown_calls
            .iter()
            .filter(|c| {
                replayed.iter().chain(flowing_statements.iter()).any(|r| std::ptr::eq(*r, *c))
            })
            .collect()
    };
    // A replayed `extend M` of the app's own module the source writes
    // above an include goes right before that include
    // (`extends_before_includes`), and leaves the replayed calls.
    // A model's replayed class-body calls are its class-side
    // initializers (`lower::model_to_library::collect_class_attr_initializers`).
    let hoistable: Vec<&Expr> = replayed
        .iter()
        .copied()
        .chain(lc.class_ivar_initializers.iter().filter(|e| !carried_seed(e)).filter(|_| !ordered_state))
        .collect();
    let extends_before = if railtie { Vec::new() } else { extends_before_includes(lc, app, &hoistable) };
    let replayed: Vec<&Expr> = replayed
        .into_iter()
        .filter(|c| !extends_before.iter().any(|(_, e)| std::ptr::eq(*e, *c)))
        .collect();
    let extended_at_include: std::cell::RefCell<Vec<ClassId>> = std::cell::RefCell::new(Vec::new());
    // `with_extends`: the include's `ClassMethods` extends follow its
    // line, ahead of its carried block, as `Concern#append_features` runs
    // them (`base.extend(ClassMethods)`, then `base.class_eval(&block)`).
    // A positioned include has them at the top of the body already.
    let write_include = |s: &mut String, inc: &ClassId, with_extends: bool| {
        for (_, call) in extends_before.iter().filter(|(before, _)| *before == inc) {
            for line in super::emit_expr(call).lines() {
                writeln!(s, "{body_pad}{line}").unwrap();
            }
        }
        // An include analysis refused (error[gem_requires_rails]): the
        // module is only the RBI of a gem no tree loads, so the source's
        // include fails here, by name, as a NotImplementedError (a
        // ScriptError, past an app's `rescue => e`), where Ruby would
        // raise a bare NameError.
        if let Some(refused) = app.gem_boundary.refused_include(&lc.name, inc) {
            let text = format!("roundhouse: error[gem_requires_rails]: {}", refused.detail);
            let quoted = text.replace('\\', "\\\\").replace('\'', "\\'");
            writeln!(s, "{body_pad}::Kernel.raise(::NotImplementedError, '{quoted}')").unwrap();
            return;
        }
        writeln!(s, "{body_pad}include {}", lexically_qualified(inc.0.as_str(), &segments, app)).unwrap();
        if with_extends {
            extends_of(inc).iter().for_each(|carrier| write_extend(s, carrier));
        }
        let root = inc.0.as_str().trim_start_matches("::");
        let groups = || carried_attributes.iter().filter(|g| root_of(&g.after) == root);
        // Concern#append_features extends the includer with ClassMethods
        // before it runs the `included do` block: a carried statement may
        // call one (Core's MemcachedHelper: `included do
        // install_memory_memcached_callbacks end`).
        let carries_statements = app
            .included_block_statements
            .get(&lc.name)
            .is_some_and(|statements| statements.iter().any(|(after, _)| after.0.as_str().trim_start_matches("::") == root));
        if groups().next().is_some() || carries_statements {
            let reached = reached_by(root);
            for carrier in &spliced_extends {
                let owned = crate::app::class_methods_carrier_owner(carrier).is_some_and(|o| reached.contains(&root_of(&o)));
                if owned && !extended_at_include.borrow().contains(*carrier) {
                    write_extend(s, carrier);
                    extended_at_include.borrow_mut().push((*carrier).clone());
                }
            }
        }
        let mut wrote_def = false;
        for (index, _) in lc.methods.iter().enumerate().filter(|(_, m)| {
            groups().any(|g| g.methods.iter().any(|(receiver, name)| *receiver == m.receiver && *name == m.name))
        }) {
            writeln!(s).unwrap();
            // A generated def, never an `extend self` copy (`instance_form`).
            write_library_method(s, lc, index, &body_pad, &[]);
            wrote_def = true;
        }
        if wrote_def {
            writeln!(s).unwrap();
        }
        let mut seeds = lc.class_ivar_initializers.iter().filter(|e| groups().any(|g| g.seeds.contains(&e.span))).peekable();
        let write_seed = |s: &mut String, seed: &Expr| {
            for line in super::emit_expr(seed).lines() {
                writeln!(s, "{body_pad}{line}").unwrap();
            }
        };
        // The include's carried `included do` statements, where Ruby runs
        // them: right after it (`App::included_block_statements`), each
        // after the seeds its block writes above it and before those
        // below it, in the block's own order.
        for (_, statement) in app
            .included_block_statements
            .get(&lc.name)
            .into_iter()
            .flatten()
            .filter(|(after, _)| after.0.as_str().trim_start_matches("::") == root)
        {
            let at = statement.span;
            while let Some(seed) = seeds.next_if(|seed| seed.span.file != at.file || seed.span.start < at.start) {
                write_seed(s, seed);
            }
            for line in super::emit_expr(statement).lines() {
                if line.is_empty() {
                    writeln!(s).unwrap();
                } else {
                    writeln!(s, "{body_pad}{line}").unwrap();
                }
            }
        }
        for seed in seeds {
            write_seed(s, seed);
        }
    };
    // The defs an ordered body wrote above.
    let written_in_state: Vec<usize> = state_body
        .iter()
        .filter_map(|item| match item {
            StateItem::Method(i) => Some(*i),
            _ => None,
        })
        .collect();
    let instance_form = extend_self_instance_defs(lc, app, &replayed);
    // An include whose `self.included(base)` hook calls class methods of
    // this class runs after them, as in the source (see
    // `hooked_include_position`); the rest open the body as before. An
    // include the source has after a positioned one is written no
    // earlier than it, so the includes keep their source order, and with
    // it the class's ancestors.
    let mut positioned: Vec<(usize, &ClassId)> = Vec::new();
    for inc in &lc.includes {
        let floor = positioned.last().map(|(at, _)| *at);
        if let Some(at) = hooked_include_position(lc, inc, app).max(floor) {
            positioned.push((at, inc));
        }
    }
    // Includes the source has below a callback registration of the
    // body's own (`late_includes`): written where the body's statements
    // reach them (`flush_late`), not at its top, so an include's carried
    // callbacks register after the body's above it, as Ruby runs them.
    let late = std::cell::RefCell::new(if railtie { Vec::new() } else { late_includes(lc, app, &positioned) });
    let flush_late = |s: &mut String, upto: Option<Span>| {
        let mut late = late.borrow_mut();
        let due = late
            .iter()
            .take_while(|(at, _)| upto.is_none_or(|u| !u.is_synthetic() && u.file == at.file && at.start < u.start))
            .count();
        for (_, inc) in late.drain(..due) {
            write_include(s, inc, true);
        }
    };
    for inc in &lc.includes {
        if positioned.iter().any(|(_, p)| *p == inc) || late.borrow().iter().any(|(_, l)| *l == inc) {
            continue;
        }
        write_include(&mut s, inc, true);
    }
    for carrier in &spliced_extends {
        if !extended_at_include.borrow().contains(*carrier) {
            write_extend(&mut s, carrier);
            extended_at_include.borrow_mut().push((*carrier).clone());
        }
    }
    for (_, inc) in &positioned {
        extends_of(inc).iter().for_each(|carrier| write_extend(&mut s, carrier));
    }
    // A load-time require that reaches back for this module (a cycle)
    // runs the module's `included`/`extended`/`prepended`/`inherited`
    // hooks as it loads, so a hook the source defines above its body
    // statements exists before the requires, as in Ruby, where a `def`
    // runs where it stands (Module#included, Class#inherited).
    //
    // So does a `delegate` forwarder: the macro defines its method where
    // it is written, and what that require loads may ask the class what
    // it answers to at load (`instance_methods`, `method_defined?`,
    // ActiveModel::AttributeMethods' `instance_method_already_implemented?`
    // style checks). A forwarder runs nothing when it is defined.
    let forwarder_sites = app.delegate_forwarder_sites.get(&lc.name);
    let is_delegate_forwarder = |m: &MethodDef| {
        m.receiver == MethodReceiver::Instance
            && (forwarder_sites.into_iter().flatten().any(|(name, at)| *name == m.name && *at == m.name_span)
                || declined.into_iter().flatten().any(|f| f.name == m.name && f.name_span == m.name_span))
    };
    let hoisted_hooks: Vec<usize> = if in_body_after_includes.is_empty() {
        Vec::new()
    } else {
        lc.methods
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                (is_delegate_forwarder(m)
                    || (m.receiver == MethodReceiver::Class
                        && matches!(m.name.as_str(), "included" | "extended" | "prepended" | "inherited")))
                    && !carried_method(m)
                    && !written_in_state.contains(i)
                    && !instance_form.get(*i).copied().unwrap_or(false)
                    && inherited_alias(lc, m).is_none()
                    && !lc.conditional_definitions.iter().any(|c| c.contains(m))
            })
            .map(|(i, _)| i)
            .collect()
    };
    for &index in &hoisted_hooks {
        writeln!(s).unwrap();
        write_library_method(&mut s, lc, index, &body_pad, &instance_form);
    }
    if !hoisted_hooks.is_empty() {
        writeln!(s).unwrap();
    }
    for r in &in_body_after_includes {
        writeln!(s, "{body_pad}{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
    }
    if (lc.includes.len() > positioned.len() || !carrier_extends.is_empty()) && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }

    // Class-level constants (`NAME = <expr>`), emitted before methods so
    // refs in method bodies resolve. A multi-line value (proc/array) keeps
    // its continuation lines indented to the class body.
    //
    // EXCEPT one whose initializer CALLS this class — campfire's
    // `Sound::BUILTIN = [ new(name: "56k", …), … ]` builds a table of
    // instances, and a class body runs top to bottom, so `new` there
    // reaches Object#initialize and raises. Those (and any constant
    // reading them) emit after the methods, which is where the source
    // had them. `emit_deferred_constants` renders that tail.
    // (The replayed class-body calls are computed above the includes.)
    // A replayed `prepend` whose module's `self.prepended(base)` hook
    // reaches this class's methods is written after them, the way
    // `hooked_include_position` places an include; the rest stay with
    // the other replayed calls. Source order among prepends is kept.
    let mut positioned_prepends: Vec<(usize, &Expr)> = Vec::new();
    if !railtie {
        for (call, modules) in app_prepend_calls(lc, app) {
            if !replayed.iter().any(|c| std::ptr::eq(*c, call)) {
                continue;
            }
            let floor = positioned_prepends.last().map(|(at, _)| *at);
            let at = modules.iter().filter_map(|m| hooked_mixin_position(lc, m, app, "prepended")).max();
            if let Some(at) = at.max(floor) {
                positioned_prepends.push((at, call));
            }
        }
    }
    let replayed: Vec<&Expr> = replayed
        .into_iter()
        .filter(|c| !positioned_prepends.iter().any(|(_, p)| std::ptr::eq(*p, *c)))
        .collect();
    // A replayed call the source has below a deferred constant runs
    // after it, with the deferred constants below the methods: worldwide
    // 1.29.0's `ALL_CURRENCIES.sort_by!(&:currency_code)` follows the
    // deferred `ALL_CURRENCIES =`; above it, NameError. Each goes after
    // the last deferred constant the source has above it. Real spans in
    // one file only; a railtie's statements keep `run_body_statement`.
    let mut calls_after_deferred: crate::det::HashMap<usize, Vec<&Expr>> = crate::det::HashMap::default();
    let replayed: Vec<&Expr> = replayed
        .into_iter()
        .filter(|call| {
            let at = deferred
                .iter()
                .copied()
                .filter(|_| !railtie)
                .filter(|&i| {
                    let value = &lc.constants[i].1;
                    !value.span.is_synthetic()
                        && !call.span.is_synthetic()
                        && value.span.file == call.span.file
                        && value.span.start < call.span.start
                })
                .max_by_key(|&i| lc.constants[i].1.span.start);
            match at {
                Some(i) => {
                    calls_after_deferred.entry(i).or_default().push(*call);
                    false
                }
                None => true,
            }
        })
        .collect();
    // So does a body statement written among the defs: Core's
    // AttributionConfig ends its body with `raise "…" unless
    // CONFIGS.key?(DEFAULT)` below the deferred `CONFIGS = { DEFAULT =>
    // new(…) }`; written before the deferred tail, NameError at load.
    positioned_statements.retain(|(_, call)| {
        let at = deferred
            .iter()
            .copied()
            .filter(|_| !railtie)
            .filter(|&i| {
                let value = &lc.constants[i].1;
                !value.span.is_synthetic() && value.span.file == call.span.file && value.span.start < call.span.start
            })
            .max_by_key(|&i| lc.constants[i].1.span.start);
        match at {
            Some(i) => {
                let calls = calls_after_deferred.entry(i).or_default();
                calls.push(*call);
                calls.sort_by_key(|c| c.span.start);
                false
            }
            None => true,
        }
    });
    // A replayed call the source has above an eager constant runs
    // before it, as the class body does: Core's liquid_ext.rb
    // `remove_const :SYNTAX` and then assigns the new `SYNTAX`, which
    // written the other way round would remove the new one. Only by
    // real spans in one file; the rest follow the constants, as before.
    //
    // Except a call that runs this class's own code (`OwnClassCalls`:
    // `self.x = v` through an `attr_accessor` the body declares) and
    // every call after it: those go after the methods (`tail_effects`
    // below), because the writer they call is one of them. Hoisted
    // above a later constant, Core's `Shopify::Config` ran
    // `self.zendesk_account_integrity_group_id = ...` before its writer
    // existed.
    let own_runs = crate::ingest::library_class::OwnClassCalls::new(&lc.name, &lc.methods);
    let first_own =
        if railtie { None } else { replayed.iter().position(|c| own_runs.runs(c) || names_own_method_above(lc, c)) };
    let mut calls_before: crate::det::HashMap<usize, Vec<&Expr>> = crate::det::HashMap::default();
    let mut leading: crate::det::HashSet<crate::span::Span> = crate::det::HashSet::default();
    for call in replayed.iter().take(first_own.unwrap_or(replayed.len())) {
        let at = eager
            .iter()
            .copied()
            .filter(|&i| {
                let value = &lc.constants[i].1;
                value.span != crate::span::Span::synthetic()
                    && call.span != crate::span::Span::synthetic()
                    && value.span.file == call.span.file
                    && call.span.start < value.span.start
            })
            .min_by_key(|&i| lc.constants[i].1.span.start);
        if let Some(i) = at {
            calls_before.entry(i).or_default().push(*call);
            leading.insert(call.span);
        }
    }
    // The eager constants written so far, by index.
    let written: std::cell::RefCell<BTreeSet<usize>> = std::cell::RefCell::new(BTreeSet::new());
    let render_constants = |s: &mut String, which: &[usize]| {
        // A nested class the source has above this point, below an
        // earlier constant (`NestedPlacement`). One another file of the
        // class declares goes in once that file's constants above it
        // are written, before anything from another file: Ruby loads
        // the whole file where it is required. graphql-client's
        // collocated_enforcement.rb writes `WHITELISTED_GEM_NAMES` and
        // then `module CollocatedEnforcement`, which client.rb's
        // `extend CollocatedEnforcement` (after its require) reads.
        let place_above = |s: &mut String, at: crate::span::Span| {
            if at != crate::span::Span::synthetic() {
                let written = written.borrow();
                NestedPlacement::place(&lc.name, s, |kid| {
                    if kid.file == at.file {
                        return kid.start < at.start;
                    }
                    eager.iter().all(|&i| {
                        let value = lc.constants[i].1.span;
                        value.file != kid.file || value.start >= kid.start || written.contains(&i)
                    })
                });
            }
        };
        for &i in which {
            for call in calls_before.get(&i).into_iter().flatten() {
                flush_late(s, Some(call.span));
                place_above(s, call.span);
                for line in super::emit_expr(call).lines() {
                    if line.is_empty() {
                        writeln!(s).unwrap();
                    } else {
                        writeln!(s, "{body_pad}{line}").unwrap();
                    }
                }
            }
            flush_late(s, Some(lc.constants[i].1.span));
            place_above(s, lc.constants[i].1.span);
            written.borrow_mut().insert(i);
            let (cname, value) = &lc.constants[i];
            let mut rendered = super::emit_expr(value);
            if rendered.trim().is_empty() {
                rendered = "nil".to_string();
            }
            if let Some(factory) = app.library_classes.iter().find(|class| {
                matches!(class.origin, Some(LibraryClassOrigin::DataFactory { declaration_span })
                    if declaration_span == value.span)
            }) {
                writeln!(s, "{body_pad}{} = {rendered} do", cname.as_str()).unwrap();
                for method in &factory.methods {
                    render_library_method(s, method, &format!("{body_pad}  "));
                }
                writeln!(s, "{body_pad}end").unwrap();
                continue;
            }
            // `NAME = value unless const_defined?(:NAME)`, as written
            // (`expr::GUARDED_CONSTANT_WRITE`): a value defined first
            // wins. A value over several lines (a heredoc may end one)
            // takes the guard's statement form, which Ruby runs the same.
            let guard = (value.decisions & crate::expr::GUARDED_CONSTANT_WRITE != 0)
                .then(|| format!("unless const_defined?(:{})", cname.as_str()));
            let multi_line = rendered.trim_end().contains('\n');
            // The lines keep the unguarded indentation: a heredoc's
            // content is whitespace-significant.
            match (&guard, multi_line) {
                (Some(guard), true) => writeln!(s, "{body_pad}{guard}").unwrap(),
                (Some(guard), false) => rendered = format!("{} {guard}", rendered.trim_end()),
                (None, _) => {}
            }
            // One the source wrote in `class << self` lives on the
            // singleton class (`SINGLETON_CONSTANT_WRITE`): written back
            // there, one line indented under it (a value over several
            // keeps its lines as below).
            let singleton = value.decisions & crate::expr::SINGLETON_CONSTANT_WRITE != 0
                && !factory_class
                && super::expr::native_keywords();
            if singleton {
                writeln!(s, "{body_pad}class << self").unwrap();
            }
            let nested_pad = format!("{body_pad}  ");
            let pad = if singleton && !multi_line { nested_pad.as_str() } else { body_pad.as_str() };
            let mut lines = rendered.lines();
            // A factory class's body is a block, which keeps the cref
            // of the scope around it: a bare `NAME =` there writes that
            // scope's constant. Its own constants (the ones its source
            // wrote in `class << self`) are written through `self`, the
            // class the block runs on.
            let written = if factory_class { format!("self::{}", cname.as_str()) } else { cname.to_string() };
            if let Some(first_line) = lines.next() {
                writeln!(s, "{pad}{written} = {first_line}").unwrap();
                for line in lines {
                    if line.is_empty() {
                        writeln!(s).unwrap();
                    } else {
                        writeln!(s, "{pad}{line}").unwrap();
                    }
                }
            }
            if guard.is_some() && multi_line {
                writeln!(s, "{body_pad}end").unwrap();
            }
            if singleton {
                writeln!(s, "{body_pad}end").unwrap();
            }
            // A stored class-body local stays out of `constants` and
            // `Foo::NAME`, as the local was (ingest `BodyLocal`).
            if cname.as_str().starts_with(crate::ingest::library_class::BODY_LOCAL_CONSTANT_PREFIX) {
                writeln!(s, "{body_pad}private_constant :{}", cname.as_str()).unwrap();
            }
        }
    };
    // An ordered body's constants, state writes and defs, in source
    // order, each nested class the source has above one written first.
    let render_state = |s: &mut String, items: &[StateItem]| {
        for item in items {
            let at = match item {
                StateItem::Constant(i) => {
                    render_constants(s, &[*i]);
                    continue;
                }
                StateItem::Initializer(i) => lc.class_ivar_initializers[*i].span,
                StateItem::Method(i) => lc.methods[*i].name_span,
            };
            NestedPlacement::place(&lc.name, s, |kid| kid.file == at.file && kid.start < at.start);
            match item {
                StateItem::Initializer(i) => {
                    write_body_lines(s, &body_pad, &super::emit_expr(&lc.class_ivar_initializers[*i]));
                }
                StateItem::Method(i) => {
                    writeln!(s).unwrap();
                    write_library_method(s, lc, *i, &body_pad, &instance_form);
                    writeln!(s).unwrap();
                }
                StateItem::Constant(_) => {}
            }
        }
    };
    if ordered_state {
        let mut start = 0;
        for (at, reqs) in &state_splits {
            render_state(&mut s, &state_body[start..*at]);
            for i in (0..depth).rev() {
                writeln!(s, "{}end", "  ".repeat(i)).unwrap();
            }
            writeln!(s).unwrap();
            for r in reqs {
                writeln!(s, "{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
            }
            writeln!(s).unwrap();
            open_header(&mut s);
            start = *at;
        }
        render_state(&mut s, &state_body[start..]);
    } else {
        match &split {
            Some((at, reqs)) => {
                render_constants(&mut s, &eager[..*at]);
                for i in (0..depth).rev() {
                    writeln!(s, "{}end", "  ".repeat(i)).unwrap();
                }
                writeln!(s).unwrap();
                for r in reqs {
                    writeln!(s, "{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
                }
                writeln!(s).unwrap();
                open_header(&mut s);
                render_constants(&mut s, &eager[*at..]);
            }
            None => render_constants(&mut s, &eager),
        }
    }
    // One below every eagerly written constant: after them, unless a
    // constant written after the methods is above it too, or a
    // class-body call written below them (`nested_position`), which it
    // follows.
    let below_deferred = |kid: &crate::span::Span| {
        deferred.iter().any(|&i| {
            let at = lc.constants[i].1.span;
            at != crate::span::Span::synthetic() && at.file == kid.file && at.start < kid.start
        })
    };
    let below_call = |kid: &crate::span::Span| {
        lc.unknown_calls.iter().any(|call| {
            !leading.contains(&call.span)
                && call.span != crate::span::Span::synthetic()
                && call.span.file == kid.file
                && call.span.start < kid.start
        })
    };
    NestedPlacement::place(&lc.name, &mut s, |kid| !below_deferred(kid) && !below_call(kid));
    if !eager.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }

    // The replayed calls not written above a constant: after the
    // constants (a captured call may reference one) and before the
    // methods.
    //
    // Finite class-side initialization (`class_ivar_initializers`) is
    // lowered IR, not replay of a framework DSL. Each assignment runs
    // once on this class object; unset subclasses deliberately keep
    // their ivar absent. A replayed call and a body ivar write keep
    // their source order (`@registry = {}` then `register :x`), so the
    // two interleave by source position when every one has a real span.
    //
    // An initializer that calls one of this class's own class methods, or
    // is itself a class-body call (a Concern macro writing its
    // `class_attribute`, `ingest::class_attribute`, possibly inherited
    // and dispatching back to a reader this class defines), needs those
    // methods defined first, so it and every initializer after it are
    // written after the methods instead.
    let replayed: Vec<&Expr> = replayed.into_iter().filter(|call| !leading.contains(&call.span)).collect();
    let own_class_methods: crate::det::HashSet<&Symbol> = lc
        .methods
        .iter()
        .filter(|m| m.receiver == crate::dialect::MethodReceiver::Class)
        .map(|m| &m.name)
        .collect();
    // The seeds written at their include (`carried_seed`) are not here,
    // nor an ordered body's state, written with its constants above.
    let own_inits: Vec<&Expr> =
        lc.class_ivar_initializers
            .iter()
            .filter(|e| !carried_seed(e))
            .filter(|_| !ordered_state)
            .filter(|e| !extends_before.iter().any(|(_, x)| std::ptr::eq(*x, *e)))
            .collect();
    let late_at = own_inits
        .iter()
        .position(|init| is_self_call(init) || calls_own_class_method(init, &own_class_methods))
        .unwrap_or(own_inits.len());
    let (early_inits, late_inits) = own_inits.split_at(late_at);
    let mut body_effects: Vec<&Expr> = replayed.iter().copied().chain(early_inits.iter().copied()).collect();
    let real_span = |e: &&Expr| e.span != crate::span::Span::synthetic();
    if !replayed.is_empty()
        && !early_inits.is_empty()
        && body_effects.iter().all(real_span)
        && body_effects.windows(2).all(|w| w[0].span.file == w[1].span.file)
    {
        body_effects.sort_by_key(|e| e.span.start);
    }
    // An effect that runs this class's own code (`self.x = v` through a
    // `class_attribute` writer, a carried `included do` statement calling
    // one) needs the defs above it, as the source had them: it and every
    // effect after it run after the methods, keeping their order.
    let mut tail_effects: Vec<&Expr> = if railtie {
        Vec::new()
    } else {
        let own = crate::ingest::library_class::OwnClassCalls::new(&lc.name, &lc.methods);
        let carried = carried_class_macro_calls(lc, app);
        let wraps_own = |e: &Expr| carried.iter().any(|c| std::ptr::eq(*c, e)) && names_own_method(lc, e);
        // A replayed `singleton_class.alias_method :new, :old` written
        // below this class's own def of `old` (same file) needs that def
        // above it. One written above the def aliases the method as it
        // was before (a reopening that wraps it) and stays in place.
        let aliases_own = |e: &Expr| {
            matches!(&*e.node, ExprNode::Send { recv: Some(r), method, args, .. }
                if method.as_str() == "alias_method"
                    && matches!(&*r.node, ExprNode::Send { method, .. } if method.as_str() == "singleton_class")
                    && args.get(1).is_some_and(|old| matches!(&*old.node,
                        ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
                            if own_class_methods.contains(value)
                                && lc.methods.iter().any(|m| m.receiver == crate::dialect::MethodReceiver::Class
                                    && &m.name == value
                                    && !m.name_span.is_synthetic()
                                    && m.name_span.file == e.span.file
                                    && m.name_span.start < e.span.start))))
        };
        // A gem port's class macro on a model (`database_documentation do
        // ... end` reads `table_name`, which Active Record answers in the
        // class body and the lowered model defines among its methods).
        let port_macros: Vec<crate::span::Span> = app
            .models
            .iter()
            .find(|m| m.name == lc.name)
            .map(|m| {
                crate::lower::model_to_library::gem_port_macro_calls(m, &app.models).iter().map(|c| c.span).collect()
            })
            .unwrap_or_default();
        let port_macro = |e: &Expr| e.span != crate::span::Span::synthetic() && port_macros.contains(&e.span);
        let at = body_effects
            .iter()
            .position(|e| own.runs(e) || wraps_own(e) || names_own_method_above(lc, e) || aliases_own(e) || port_macro(e))
            .unwrap_or(body_effects.len());
        body_effects.split_off(at)
    };
    // A body statement written among the defs, or after the last, is
    // written before the tail effects. One with a tail effect above it
    // in the source (`self.logger = Logger.new` through the body's own
    // `def self.logger=`, above `self.logger.progname = "x"`) would run
    // ahead of the effect it reads, so it joins the tail, which keeps
    // the source's order.
    let tail_statements: Vec<&Expr> = positioned_statements
        .iter()
        .filter(|(at, call)| {
            *at >= lc.methods.len()
                || tail_effects.iter().any(|t| {
                    !t.span.is_synthetic() && t.span.file == call.span.file && t.span.start < call.span.start
                })
        })
        .map(|(_, call)| *call)
        .collect();
    if !tail_effects.is_empty() && !tail_statements.is_empty() {
        positioned_statements.retain(|(_, call)| !tail_statements.iter().any(|t| std::ptr::eq(*t, *call)));
        tail_effects.extend(tail_statements);
        if tail_effects.iter().all(|e| !e.span.is_synthetic() && e.span.file == tail_effects[0].span.file) {
            tail_effects.sort_by_key(|e| e.span.start);
        }
    }
    // A carried railtie's body: its locals first (a statement's block
    // closes over a local an earlier statement writes), then each
    // statement under `run_body_statement`, which names one that fails
    // and goes on (runtime/rails_railtie.rb).
    if railtie {
        let mut locals: Vec<String> = Vec::new();
        for effect in &body_effects {
            body_scope_locals(effect, &mut locals);
        }
        for local in &locals {
            writeln!(s, "{body_pad}{local} = nil").unwrap();
        }
    }
    let railtie_root = app.railties.iter().find(|r| r.name == lc.name).and_then(|r| r.root.clone());
    let mut sources: Option<std::rc::Rc<CarriedSources>> = None;
    let mut nested_required: BTreeSet<String> = BTreeSet::new();
    for effect in &body_effects {
        flush_late(&mut s, Some(effect.span));
        // A nested class the source writes above this call, below an
        // earlier one.
        if !railtie && effect.span != crate::span::Span::synthetic() {
            NestedPlacement::place(&lc.name, &mut s, |kid| {
                kid.file == effect.span.file && kid.start < effect.span.start && !below_deferred(kid)
            });
            for line in nested_files_above(lc, effect.span, app, &out_path, &mut nested_required) {
                writeln!(s, "{body_pad}{line}").unwrap();
            }
        }
        let text = if railtie {
            super::emit_expr(&carried_loads(effect, app, railtie_root.as_deref(), &mut sources))
        } else {
            super::emit_expr(effect)
        };
        let (pad, text) = if railtie {
            writeln!(s, "{body_pad}run_body_statement({:?}) do", statement_site(app, effect.span)).unwrap();
            (format!("{body_pad}  "), text)
        } else {
            (body_pad.to_string(), text)
        };
        for line in text.lines() {
            if line.is_empty() {
                writeln!(s).unwrap();
            } else {
                writeln!(s, "{pad}{line}").unwrap();
            }
        }
        if railtie {
            writeln!(s, "{body_pad}end").unwrap();
        }
    }
    // The rest: below every body effect, above the methods.
    flush_late(&mut s, None);
    if !railtie {
        NestedPlacement::place(&lc.name, &mut s, |kid| !below_deferred(kid));
    }
    if !replayed.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }

    // A load-time conditional definition (`ConditionalDefinitions`) is
    // written whole, as the source has it, where its first member stands.
    // One that defines no method (its branches only declare classes)
    // stands before the first method the source has below it.
    let mut first = true;
    let mut written = vec![false; lc.conditional_definitions.len()];
    let methodless: Vec<(usize, usize)> = lc
        .conditional_definitions
        .iter()
        .enumerate()
        .filter(|(_, c)| c.all_arms().iter().all(|arm| arm.methods.is_empty()))
        .map(|(at, c)| {
            let before = lc
                .methods
                .iter()
                .position(|m| m.name_span.file == c.span.file && m.name_span.start > c.span.start)
                .unwrap_or(lc.methods.len());
            (at, before)
        })
        .collect();
    let write_methodless = |s: &mut String, first: &mut bool, index: usize| {
        for &(at, _) in methodless.iter().filter(|(_, before)| *before == index) {
            if !std::mem::replace(first, false) {
                writeln!(s).unwrap();
            }
            write_conditional_definitions(s, lc, &lc.conditional_definitions[at], &body_pad, &instance_form);
        }
    };
    for (index, m) in lc.methods.iter().enumerate() {
        write_methodless(&mut s, &mut first, index);
        // Every def above it is written, and none below.
        for (_, call) in positioned_statements.iter().filter(|(at, _)| *at == index) {
            if !std::mem::replace(&mut first, false) {
                writeln!(s).unwrap();
            }
            write_body_lines(&mut s, &body_pad, &super::emit_expr(call));
        }
        // Written at its include (`carried_method`).
        if carried_method(m) {
            for (_, inc) in positioned.iter().filter(|(at, _)| *at == index) {
                if !std::mem::replace(&mut first, false) {
                    writeln!(s).unwrap();
                }
                write_include(&mut s, inc, false);
            }
            continue;
        }
        // Written in an ordered body's source order (`state_body`).
        if written_in_state.contains(&index) {
            continue;
        }
        // Written above the load-cycle requires (`hoisted_hooks`).
        if hoisted_hooks.contains(&index) {
            continue;
        }
        let conditional = lc.conditional_definitions.iter().position(|c| c.contains(m));
        if let Some(at) = conditional {
            if std::mem::replace(&mut written[at], true) {
                continue;
            }
        }
        if !first {
            writeln!(s).unwrap();
        }
        first = false;
        for (_, inc) in positioned.iter().filter(|(at, _)| *at == index) {
            write_include(&mut s, inc, false);
            writeln!(s).unwrap();
        }
        for (_, call) in positioned_prepends.iter().filter(|(at, _)| *at == index) {
            writeln!(s, "{body_pad}{}", super::emit_expr(call)).unwrap();
            writeln!(s).unwrap();
        }

        match conditional {
            // After every def (below).
            None if inherited_alias(lc, m).is_some() => {}
            Some(at) => write_conditional_definitions(&mut s, lc, &lc.conditional_definitions[at], &body_pad, &instance_form),
            None => write_library_method(&mut s, lc, index, &body_pad, &instance_form),
        }
    }
    // An alias of an ancestor's method (`MethodAlias::inherited`) binds
    // what the class then answers to the name: an override a pass wrote
    // here for an inherited method stands for that method (a class
    // method building `self`), so the alias follows every def. The
    // class's own definition of the name would have kept the alias
    // from resolving to an ancestor (`ingest::inherited_alias`).
    write_methodless(&mut s, &mut first, lc.methods.len());
    for (index, m) in lc.methods.iter().enumerate() {
        if inherited_alias(lc, m).is_some() {
            writeln!(s).unwrap();
            write_library_method(&mut s, lc, index, &body_pad, &instance_form);
        }
    }

    for (_, inc) in positioned.iter().filter(|(at, _)| *at >= lc.methods.len()) {
        writeln!(s).unwrap();
        write_include(&mut s, inc, false);
    }
    for (_, call) in positioned_prepends.iter().filter(|(at, _)| *at >= lc.methods.len()) {
        writeln!(s).unwrap();
        writeln!(s, "{body_pad}{}", super::emit_expr(call)).unwrap();
    }
    for (_, call) in positioned_statements.iter().filter(|(at, _)| *at >= lc.methods.len()) {
        writeln!(s).unwrap();
        write_body_lines(&mut s, &body_pad, &super::emit_expr(call));
    }

    if !tail_effects.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for effect in &tail_effects {
        for line in nested_files_above(lc, effect.span, app, &out_path, &mut nested_required) {
            writeln!(s, "{body_pad}{line}").unwrap();
        }
        for line in super::emit_expr(effect).lines() {
            if line.is_empty() {
                writeln!(s).unwrap();
            } else {
                writeln!(s, "{body_pad}{line}").unwrap();
            }
        }
    }

    // Named visibility for methods without a local definition (inherited,
    // or generated by a replayed class-body call), after everything that
    // could define them. Unlike the per-def directives above, public is
    // written too: it widens an ancestor's nonpublic method.
    //
    // A late class-body statement below (`late_inits`: a model's replayed
    // macro call) can define the method one names, so each is written
    // after the last of them the source has above it, and before the
    // rest: Core's checkout.rb runs `metadata_accessor :secret_token`
    // (line 127), which defines `secret_token=`, and makes it private at
    // line 1718. Written ahead of the replayed calls, `private
    // :secret_token=` raised NameError in the class body, and Checkout
    // never loaded. Compared by real spans in one file; one with no such
    // statement above it stays ahead of them all.
    let after_late = |o: &crate::dialect::VisibilityOverride| -> usize {
        late_inits
            .iter()
            .rposition(|init| {
                !o.span.is_synthetic()
                    && !init.span.is_synthetic()
                    && init.span.file == o.span.file
                    && init.span.start < o.span.start
            })
            .map_or(0, |at| at + 1)
    };
    let write_override = |s: &mut String, o: &crate::dialect::VisibilityOverride| {
        let name = o.name.as_str();
        match (o.receiver, o.visibility) {
            (MethodReceiver::Instance, MethodVisibility::Public) => writeln!(s, "{body_pad}public :{name}"),
            (MethodReceiver::Instance, MethodVisibility::Protected) => writeln!(s, "{body_pad}protected :{name}"),
            (MethodReceiver::Instance, MethodVisibility::Private) => writeln!(s, "{body_pad}private :{name}"),
            (MethodReceiver::Class, MethodVisibility::Public) => writeln!(s, "{body_pad}public_class_method :{name}"),
            (MethodReceiver::Class, MethodVisibility::Private) => writeln!(s, "{body_pad}private_class_method :{name}"),
            (MethodReceiver::Class, MethodVisibility::Protected) => {
                writeln!(s, "{body_pad}class << self").unwrap();
                writeln!(s, "{body_pad}  protected :{name}").unwrap();
                writeln!(s, "{body_pad}end")
            }
        }
        .unwrap();
    };
    // One the source has below a deferred constant goes after the last
    // of them above it, as `calls_after_deferred`: apps_runtime's
    // placeholders.rb writes `STATIC = Static.new.freeze` and then
    // `private_class_method :new`. Written first, the nested subclass's
    // inherited `new` was already private: NoMethodError at boot.
    let override_after_deferred = |o: &crate::dialect::VisibilityOverride| -> Option<usize> {
        deferred
            .iter()
            .copied()
            .filter(|_| !railtie)
            .filter(|&i| {
                let value = &lc.constants[i].1;
                !value.span.is_synthetic() && !o.span.is_synthetic() && value.span.file == o.span.file && value.span.start < o.span.start
            })
            .max_by_key(|&i| lc.constants[i].1.span.start)
    };
    let deferred_override_places: Vec<Option<usize>> = lc.visibility_overrides.iter().map(override_after_deferred).collect();
    let override_places: Vec<usize> = lc
        .visibility_overrides
        .iter()
        .zip(&deferred_override_places)
        .map(|(o, d)| if d.is_some() { usize::MAX } else { after_late(o) })
        .collect();
    let overrides_at = |at: usize| {
        lc.visibility_overrides.iter().zip(&override_places).filter(move |(_, place)| **place == at).map(|(o, _)| o)
    };
    if overrides_at(0).next().is_some() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for o in overrides_at(0) {
        write_override(&mut s, o);
    }

    // Finite class-side initialization is lowered IR, not replay of a
    // framework DSL. Each statement runs once on this class object; the
    // ones from the first call to a class method this class defines (a
    // Concern macro writing its `class_attribute`) run here, after those
    // methods. Unset subclasses keep their ivar absent.
    if !late_inits.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for (at, init) in late_inits.iter().enumerate() {
        for line in super::emit_expr(init).lines() {
            writeln!(s, "{body_pad}{line}").unwrap();
        }
        for o in overrides_at(at + 1) {
            write_override(&mut s, o);
        }
    }

    // A nested class whose body builds this class with its own defs
    // (`builds_after_parent_defs`) follows them, as the source has it.
    NestedPlacement::place_late(&lc.name, &mut s, |kid| !below_deferred(kid));
    if !deferred.is_empty() {
        if !lc.methods.is_empty() {
            writeln!(s).unwrap();
        }
        for &i in &deferred {
            render_constants(&mut s, &[i]);
            for call in calls_after_deferred.get(&i).into_iter().flatten() {
                for line in super::emit_expr(call).lines() {
                    if line.is_empty() {
                        writeln!(s).unwrap();
                    } else {
                        writeln!(s, "{body_pad}{line}").unwrap();
                    }
                }
            }
            for (o, _) in lc.visibility_overrides.iter().zip(&deferred_override_places).filter(|(_, d)| **d == Some(i)) {
                write_override(&mut s, o);
            }
        }
        NestedPlacement::place(&lc.name, &mut s, |_| true);
    }
    NestedPlacement::place_late(&lc.name, &mut s, |_| true);

    if !railtie && !replays_foreign_class_body(lc, app) && let Some(call) = trailing_freeze(lc, app) {
        writeln!(s, "{body_pad}{}", super::emit_expr(call)).unwrap();
    }

    for i in (0..depth).rev() {
        writeln!(s, "{}end", "  ".repeat(i)).unwrap();
    }
    if !trailing.is_empty() {
        writeln!(s).unwrap();
        for line in &trailing {
            writeln!(s, "{line}").unwrap();
        }
    }

    EmittedFile { path: out_path, content: s }
}

/// The includes of `lc` the source has below the first callback
/// registration of its own body (`job_class_side::is_callbacks_port_call`,
/// a `before_perform` in a job), each with where it stands, in source
/// order: the emitted body writes them at their statement instead of
/// opening the body with them.
///
/// The include bucket opens the body, and an include's carried
/// `included do` callbacks are written right after it (`write_include`).
/// Ruby registers them where the include is: Core's BackgroundQueue::Job
/// registers its own `before_perform`s and then includes
/// Podding::JobShopLock, whose `included do` adds one more, so the chain
/// runs the body's first. Written at the top, the include's ran first.
///
/// Matched by the written name (`job_class_side::include_offsets`) in
/// the file of that registration. An include not written there (one a
/// carried `included do` adds) stays with the one before it, so the
/// includes keep their order; `positioned` ones (`hooked_include_position`)
/// are written among the methods and are none of these.
fn late_includes<'a>(lc: &'a LibraryClass, app: &App, positioned: &[(usize, &ClassId)]) -> Vec<(Span, &'a ClassId)> {
    let first = lc
        .unknown_calls
        .iter()
        .filter(|c| !c.span.is_synthetic() && crate::lower::job_class_side::is_callbacks_port_call(c))
        .map(|c| c.span)
        .chain(macro_hooked_include_floor(lc, app, positioned))
        .min_by_key(|span| (span.file.0, span.start));
    let Some(first) = first else { return Vec::new() };
    let Some(source) = (first.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) else {
        return Vec::new();
    };
    let offsets = crate::lower::job_class_side::include_offsets(&source.text, lc.name.0.as_str());
    let mut cursor = 0;
    let mut previous: Option<u32> = None;
    let mut late = Vec::new();
    for inc in lc.includes.iter().filter(|inc| !positioned.iter().any(|(_, p)| p == inc)) {
        let id = inc.0.as_str().trim_start_matches("::");
        let found = offsets[cursor..]
            .iter()
            .position(|(written, _)| id == written || id.ends_with(&format!("::{written}")));
        let at = match found {
            Some(k) => {
                cursor += k + 1;
                Some(offsets[cursor - 1].1)
            }
            None => previous,
        };
        previous = at;
        if let Some(at) = at.filter(|at| *at > first.start) {
            late.push((Span { file: first.file, start: at, end: at }, inc));
        }
    }
    late
}

/// Whether the app module `inc`'s `self.included(base)` hook sends `base`
/// a class macro: a call that is no `Module` builtin (`extend`,
/// `class_eval`, ...), so one a superclass or an extended module defines,
/// which reads the state the including body's earlier macro calls set
/// (Rails' `Module#included` runs where `include` stands).
fn included_hook_sends_class_macro(inc: &ClassId, app: &App) -> bool {
    const MODULE_BUILTINS: &[&str] = &[
        "extend", "include", "prepend", "class_eval", "class_exec", "module_eval", "module_exec", "instance_eval",
        "instance_exec", "send", "public_send", "__send__", "define_method", "define_singleton_method", "alias_method",
        "attr_accessor", "attr_reader", "attr_writer", "const_set", "const_get", "const_defined?", "method_defined?",
        "instance_methods", "instance_method", "name", "to_s", "is_a?", "kind_of?", "respond_to?", "ancestors",
        "include?", "private", "public", "protected", "private_constant", "remove_method", "undef_method",
        "instance_variable_get", "instance_variable_set", "class_variable_get", "class_variable_set", "singleton_class",
        "module_function", "freeze", "tap", "then", "nil?", "equal?", "object_id", "instance_variable_defined?",
    ];
    let bare = inc.0.as_str().trim_start_matches("::");
    let Some(module) = first_module_bare_named(bare, app) else { return false };
    let Some(hook) = module
        .methods
        .iter()
        .find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "included" && m.params.len() == 1)
    else {
        return false;
    };
    let base = hook.params[0].name.clone();
    fn walk(e: &Expr, base: &Symbol, hit: &mut bool) {
        if let ExprNode::Send { recv: Some(r), method, .. } = &*e.node
            && matches!(&*r.node, ExprNode::Var { name, .. } if name == base)
            && !MODULE_BUILTINS.contains(&method.as_str())
        {
            *hit = true;
        }
        e.node.for_each_child(&mut |c| walk(c, base, hit));
    }
    let mut hit = false;
    walk(&hook.body, &base, &mut hit);
    hit
}

/// The first body statement the source writes above an include of `lc`
/// whose `included` hook sends `base` a class macro
/// (`included_hook_sends_class_macro`): from it on the includes are
/// written where the body's statements reach them (`late_includes`), as
/// Ruby runs the hook at the `include`. Core's `AppCatalog` calls
/// `implements` and `pre_resolve_access`, then includes a module whose hook
/// calls `base.post_resolve_access`, which raises when the class has no
/// access declared yet; the include bucket wrote it first.
fn macro_hooked_include_floor(lc: &LibraryClass, app: &App, positioned: &[(usize, &ClassId)]) -> Option<Span> {
    let candidates: Vec<&ClassId> = lc
        .includes
        .iter()
        .filter(|inc| !positioned.iter().any(|(_, p)| p == inc))
        .filter(|inc| included_hook_sends_class_macro(inc, app))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let calls: Vec<Span> = lc.unknown_calls.iter().map(|c| c.span).filter(|s| !s.is_synthetic()).collect();
    let file = calls.iter().map(|s| s.file).min_by_key(|f| f.0)?;
    let source = (file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i))?;
    let offsets = crate::lower::job_class_side::include_offsets(&source.text, lc.name.0.as_str());
    let mut cursor = 0;
    let mut floor: Option<Span> = None;
    for inc in lc.includes.iter().filter(|inc| !positioned.iter().any(|(_, p)| p == inc)) {
        let id = inc.0.as_str().trim_start_matches("::");
        let Some(k) = offsets[cursor..].iter().position(|(written, _)| id == written || id.ends_with(&format!("::{written}")))
        else {
            continue;
        };
        cursor += k + 1;
        if !candidates.contains(&inc) {
            continue;
        }
        let at = offsets[cursor - 1].1;
        let above = calls.iter().filter(|s| s.file == file && s.start < at).min_by_key(|s| s.start);
        if let Some(above) = above
            && floor.is_none_or(|f| above.start < f.start)
        {
            floor = Some(*above);
        }
    }
    floor
}

/// The replayed `extend M`s of the app's own modules that the source
/// writes above an include of `lc`, each with that include (the first
/// one below it in the same file), in source order.
///
/// The include bucket opens the emitted body and the replayed calls
/// follow the methods, but a class body runs its mixins in the source's
/// order, and an include's `included(base)` hook may call what the
/// extend gave `base`: Core's Customer writes `extend
/// SensitiveAttributeValidations` above `include
/// CustomerPasswordProtection`, whose hook runs
/// `base.validate_sensitive(...)`. Replayed after the methods, the
/// extend came too late: "undefined method 'validate_sensitive' for
/// class Customer". Written right before its include, it runs where the
/// source has it relative to the includes. An extend only adds the
/// module to the singleton's ancestors, so writing it above the methods
/// and constants changes nothing else, except through the module's own
/// `extended` hook, which may read them: an extend of a module with one
/// stays with the replayed calls. Matched by the written name
/// (`job_class_side::include_offsets`), as `late_includes` does.
fn extends_before_includes<'a, 'e>(
    lc: &'a LibraryClass,
    app: &App,
    replayed: &[&'e Expr],
) -> Vec<(&'a ClassId, &'e Expr)> {
    let mut by_file: BTreeMap<u32, Vec<(u32, &'a ClassId)>> = BTreeMap::new();
    let mut out = Vec::new();
    for call in replayed {
        if call.span.is_synthetic() {
            continue;
        }
        let Some(modules) = app_module_extend(lc, call, app) else { continue };
        let hooked = modules.iter().any(|m| {
            m.methods.iter().any(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == "extended")
        });
        if hooked {
            continue;
        }
        let includes = by_file.entry(call.span.file.0).or_insert_with(|| {
            let Some(source) = (call.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) else {
                return Vec::new();
            };
            let offsets = crate::lower::job_class_side::include_offsets(&source.text, lc.name.0.as_str());
            let mut cursor = 0;
            let mut placed = Vec::new();
            for inc in &lc.includes {
                let id = inc.0.as_str().trim_start_matches("::");
                let found = offsets[cursor..]
                    .iter()
                    .position(|(written, _)| id == written || id.ends_with(&format!("::{written}")));
                if let Some(k) = found {
                    cursor += k + 1;
                    placed.push((offsets[cursor - 1].1, inc));
                }
            }
            placed
        });
        let next = includes.iter().filter(|(at, _)| *at > call.span.start).min_by_key(|(at, _)| *at);
        if let Some((_, inc)) = next {
            out.push((*inc, *call));
        }
    }
    out
}

/// Where `include inc` must be written in `lc`'s body when the source's
/// order is load-bearing: the index into `lc.methods` to write it before.
///
/// The include bucket opens the emitted body, ahead of the constants and
/// methods. That is Ruby-equivalent for method lookup, but not for an
/// app module's `self.included(base)` hook, which runs at the include
/// and may call `base`'s own class methods. secure_headers 5.2's
/// `ContentSecurityPolicyConfig` defines `ATTRS` and `def self.attrs`,
/// then includes `DynamicConfig`, whose hook reads `base.attrs`;
/// written first, the include raised NoMethodError at load. Such an
/// include goes right after the last class method of `lc` its hook
/// calls on `base` (the source must have had it below that def), and so
/// also after every eagerly emitted constant. `None` keeps it first.
///
/// A hook may also name one of `lc`'s methods by symbol in a call on
/// `base`: asset_cloud 2.7.3's `AssetExtension` defines `store`, then
/// includes `Validations`, whose hook runs `base.send(:alias_method,
/// :store_without_validation, :store)`. Written first, the include raised
/// NameError (undefined method 'store') at load. Such an include goes
/// right after the last method of `lc`, of either side, the hook names.
fn hooked_include_position(lc: &LibraryClass, inc: &ClassId, app: &App) -> Option<usize> {
    hooked_mixin_position(lc, inc, app, "included")
}

/// `hooked_include_position` for the module's `hook` (`included`, or
/// `prepended` for a `prepend`).
fn hooked_mixin_position(lc: &LibraryClass, inc: &ClassId, app: &App, hook: &str) -> Option<usize> {
    let bare = |c: &ClassId| c.0.as_str().trim_start_matches("::").to_string();
    let module = first_module_bare_named(&bare(inc), app)?;
    let hook = module.methods.iter().find(|m| {
        m.receiver == MethodReceiver::Class && m.name.as_str() == hook && m.params.len() == 1
    })?;
    let base = hook.params[0].name.clone();
    let mut called: Vec<Symbol> = Vec::new();
    let mut named: Vec<Symbol> = Vec::new();
    fn walk(e: &Expr, base: &Symbol, called: &mut Vec<Symbol>, named: &mut Vec<Symbol>) {
        if let ExprNode::Send { recv: Some(r), method, args, .. } = &*e.node {
            if matches!(&*r.node, ExprNode::Var { name, .. } if name == base) {
                if !called.contains(method) {
                    called.push(method.clone());
                }
                for arg in args {
                    if let ExprNode::Lit { value: crate::expr::Literal::Sym { value } } = &*arg.node {
                        if !named.contains(value) {
                            named.push(value.clone());
                        }
                    }
                }
            }
        }
        e.node.for_each_child(&mut |c| walk(c, base, called, named));
    }
    walk(&hook.body, &base, &mut called, &mut named);
    lc.methods
        .iter()
        .rposition(|m| (m.receiver == MethodReceiver::Class && called.contains(&m.name)) || named.contains(&m.name))
        .map(|i| i + 1)
}

/// Which of `lc`'s class-side methods are `extend self` copies, to be
/// written as the instance methods the source defines. Ingest gives a
/// bare `def` below `extend self` its module-side form, the call shape
/// every target uses; a module that replays the `extend self`
/// (`plain_extension_calls`) answers its instance methods on itself, so
/// writing them as instance methods keeps both, as Ruby has them. As
/// `def self.x` they were lost to whatever `extend`s or `include`s the
/// module: frozen_record 0.27.4's `Backends::Yaml` `extend self`s, and
/// api_versioning's `FrozenApplicationRecordBackend` `extend`s it and
/// calls `super` from its own `filename`, which found no `filename`
/// above it and raised NoMethodError.
///
/// A method defined on the singleton (`def self.x`, `class << self`) stays
/// one, read off the source (`helper_surface::SingletonOrigins`), and so
/// does a possible `module_function` copy (any def below a bare
/// `module_function` in its file), whose two forms differ in visibility.
/// Without the source text nothing is rewritten.
fn extend_self_instance_defs(lc: &LibraryClass, app: &App, replayed: &[&Expr]) -> Vec<bool> {
    let extends_self = lc.is_module
        && replayed.iter().any(|c| {
            matches!(&*c.node, ExprNode::Send { recv: None, method, args, block: None, .. }
                if method.as_str() == "extend"
                    && matches!(args.as_slice(), [only] if matches!(&*only.node, ExprNode::SelfRef)))
        });
    if !extends_self {
        return vec![false; lc.methods.len()];
    }
    struct Origins {
        singleton: crate::ingest::helper_surface::SingletonOrigins,
        module_functions: Vec<usize>,
    }
    struct ModuleFunctions(Vec<usize>);
    impl<'pr> ruby_prism::Visit<'pr> for ModuleFunctions {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_none() && node.name().as_slice() == b"module_function" {
                self.0.push(node.location().start_offset());
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut by_file: BTreeMap<u32, Option<Origins>> = BTreeMap::new();
    lc.methods
        .iter()
        .map(|m| {
            if m.receiver != MethodReceiver::Class {
                return false;
            }
            let at = if !m.name_span.is_synthetic() { m.name_span } else { m.body.span };
            if at.is_synthetic() {
                return false;
            }
            let origins = by_file.entry(at.file.0).or_insert_with(|| {
                let source = (at.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i))?;
                let mut module_functions = ModuleFunctions(Vec::new());
                ruby_prism::Visit::visit(&mut module_functions, &ruby_prism::parse(source.text.as_bytes()).node());
                Some(Origins {
                    singleton: crate::ingest::helper_surface::SingletonOrigins::of(source.text.as_bytes()),
                    module_functions: module_functions.0,
                })
            });
            let Some(origins) = origins else { return false };
            let after_module_function = origins.module_functions.iter().any(|&mf| mf < at.start as usize);
            !after_module_function && !origins.singleton.is_singleton_defined(m)
        })
        .collect()
}

/// On the Ruby family, a model's compiled column accessors
/// (`lower::model_to_library::generated_attribute_method_names`, the
/// synthesized defs of those names) as the methods of its
/// GeneratedAttributeMethods module, and the class without them. An
/// alias copy stays in the class, where its alias is written.
///
/// Rails writes a record's default and loaded attributes without calling
/// a writer (`_default_attributes`, `init_with_attributes`), so the
/// synthesized `initialize`'s default writes and the hydration factories'
/// column writes go through the module's own writers, under the
/// `_generated_` aliases of the third element: an override of a writer
/// runs for a value given to it, not for those.
fn generated_attribute_methods(lc: &LibraryClass, app: &App) -> Option<(LibraryClass, LibraryClass, Vec<Symbol>)> {
    if !super::expr::native_keywords() || lc.is_module || lc.origin.is_some() {
        return None;
    }
    let model = app.models.iter().find(|m| m.name == lc.name)?;
    let table = app.schema.tables.get(&model.table.0)?;
    let mut names = crate::lower::model_to_library::generated_attribute_method_names(model, &app.models, table);
    // The reader of an association a class macro declares, which the
    // reader its `class_eval` writes supers into.
    names.extend(crate::lower::model_to_library::macro_associations::declared_readers(model));
    let generated = |m: &MethodDef| {
        m.receiver == MethodReceiver::Instance
            && m.name_span.is_synthetic()
            && names.contains(&m.name)
            && !lc.method_aliases.iter().any(|a| a.receiver == MethodReceiver::Instance && (a.name == m.name || a.original == m.name))
    };
    if !lc.methods.iter().any(&generated) {
        return None;
    }
    let mut module = lc.clone();
    module.methods.retain(&generated);
    module.method_aliases.clear();
    let writers: crate::det::HashSet<Symbol> =
        module.methods.iter().filter(|m| m.name.as_str().ends_with('=')).map(|m| m.name.clone()).collect();
    let mut retargeted = Vec::new();
    let mut rest = lc.clone();
    rest.methods.retain(|m| !generated(m));
    for method in rest.methods.iter_mut().filter(|m| m.name_span.is_synthetic()) {
        match (method.receiver, method.name.as_str()) {
            (MethodReceiver::Class, "from_row" | "from_stmt") => {
                write_through_generated_writers(&mut method.body, &writers, None, &mut retargeted);
            }
            (MethodReceiver::Instance, "initialize") => {
                let attrs = method.params.first().map(|p| p.name.clone());
                if let ExprNode::Seq { exprs } = &mut *method.body.node {
                    for stmt in exprs {
                        write_through_generated_writers(stmt, &writers, attrs.as_ref(), &mut retargeted);
                    }
                }
            }
            _ => {}
        }
    }
    Some((module, rest, retargeted))
}

/// `_generated_<col>=`: the module's alias of its writer `<col>=`.
fn generated_writer_alias(writer: &Symbol) -> Symbol {
    Symbol::from(format!("_generated_{}", writer.as_str()))
}

/// Retarget a write `recv.<writer> value` (of a local or `self`) at the
/// writer's `_generated_` alias unless its value reads `given` (the
/// constructor's attrs), collecting each writer retargeted.
fn write_through_generated_writers(
    expr: &mut Expr,
    writers: &crate::det::HashSet<Symbol>,
    given: Option<&Symbol>,
    retargeted: &mut Vec<Symbol>,
) {
    if let ExprNode::Send { recv: Some(recv), method, args, .. } = &mut *expr.node {
        if args.len() == 1
            && matches!(&*recv.node, ExprNode::Var { .. } | ExprNode::SelfRef)
            && writers.contains(method)
            && !given.is_some_and(|attrs| reads_var(&args[0], attrs))
        {
            if !retargeted.contains(method) {
                retargeted.push(method.clone());
            }
            *method = generated_writer_alias(method);
        }
    }
    expr.node.for_each_child_mut(&mut |c| write_through_generated_writers(c, writers, given, retargeted));
}

fn reads_var(expr: &Expr, name: &Symbol) -> bool {
    if matches!(&*expr.node, ExprNode::Var { name: n, .. } if n == name) {
        return true;
    }
    let mut found = false;
    expr.node.for_each_child(&mut |c| found = found || reads_var(c, name));
    found
}

/// `lc.methods[index]`, then its visibility.
///
/// An `extend self` copy (`instance_form[index]`) is written as the
/// instance method the source defines (`extend_self_instance_defs`).
fn write_library_method(s: &mut String, lc: &LibraryClass, index: usize, body_pad: &str, instance_form: &[bool]) {
    let source = &lc.methods[index];
    let as_instance;
    let m = if instance_form.get(index).copied().unwrap_or(false) {
        as_instance = MethodDef { receiver: MethodReceiver::Instance, ..source.clone() };
        &as_instance
    } else {
        source
    };
    // An alias copy is written as the native alias, so a `super` in
    // it keeps looking up the original's name, as in the source. The
    // alias takes the original's visibility; a different one on the
    // copy is a later visibility change, written after it.
    // One of an ancestor's method (`MethodAlias::inherited`) is the
    // native alias too: Ruby finds the original through the ancestors,
    // and the alias keeps its visibility.
    if let Some(alias) = inherited_alias(lc, source) {
        let (to, from) = (m.name.as_str(), alias.original.as_str());
        match m.receiver {
            MethodReceiver::Instance => writeln!(s, "{body_pad}alias_method :{to}, :{from}").unwrap(),
            MethodReceiver::Class => {
                writeln!(s, "{body_pad}class << self").unwrap();
                writeln!(s, "{body_pad}  alias_method :{to}, :{from}").unwrap();
                writeln!(s, "{body_pad}end").unwrap();
            }
        }
        return;
    }
    if let Some(original) = native_alias_original(lc, index) {
        let (to, from) = (m.name.as_str(), original.name.as_str());
        match m.receiver {
            MethodReceiver::Instance => writeln!(s, "{body_pad}alias_method :{to}, :{from}").unwrap(),
            MethodReceiver::Class => {
                writeln!(s, "{body_pad}class << self").unwrap();
                writeln!(s, "{body_pad}  alias_method :{to}, :{from}").unwrap();
                writeln!(s, "{body_pad}end").unwrap();
            }
        }
        if m.visibility != original.visibility {
            match (m.receiver, m.visibility) {
                (MethodReceiver::Instance, MethodVisibility::Public) => writeln!(s, "{body_pad}public :{to}"),
                (MethodReceiver::Instance, MethodVisibility::Protected) => writeln!(s, "{body_pad}protected :{to}"),
                (MethodReceiver::Instance, MethodVisibility::Private) => writeln!(s, "{body_pad}private :{to}"),
                (MethodReceiver::Class, MethodVisibility::Public) => writeln!(s, "{body_pad}public_class_method :{to}"),
                (MethodReceiver::Class, MethodVisibility::Private) => writeln!(s, "{body_pad}private_class_method :{to}"),
                (MethodReceiver::Class, MethodVisibility::Protected) => {
                    writeln!(s, "{body_pad}class << self").unwrap();
                    writeln!(s, "{body_pad}  protected :{to}").unwrap();
                    writeln!(s, "{body_pad}end")
                }
            }
            .unwrap();
        }
        return;
    }
    if lc.method_aliases.iter().any(|a| a.name == source.name && a.receiver == source.receiver)
        && contains_super(&m.body)
    {
        crate::emit::diagnostics::report_unsupported(
            m.body.span,
            "ruby",
            "alias_method copy calling super",
            "its original is not the def emitted before it, so it cannot be a native alias, \
             and a copied def would look `super` up under the alias's name",
        );
    }
    let body = super::emit_method(m);
    for line in body.lines() {
        if line.is_empty() {
            writeln!(s).unwrap();
        } else {
            writeln!(s, "{body_pad}{line}").unwrap();
        }
    }
    // Named, immediately after its own def: a sticky `private` section
    // would privatize unrelated methods once source bodies are flattened.
    // Public needs no annotation, except for Ruby's implicitly private
    // constructor/copy hooks when the source explicitly made one public.
    let implicit_private = m.receiver == MethodReceiver::Instance
        && matches!(m.name.as_str(), "initialize" | "initialize_copy" | "initialize_dup" | "initialize_clone");
    let directive = match (m.receiver, m.visibility) {
        (_, MethodVisibility::Public) if !implicit_private => None,
        (MethodReceiver::Instance, MethodVisibility::Public) => Some("public"),
        (MethodReceiver::Instance, MethodVisibility::Protected) => Some("protected"),
        (MethodReceiver::Instance, MethodVisibility::Private) => Some("private"),
        (MethodReceiver::Class, MethodVisibility::Private) => Some("private_class_method"),
        (MethodReceiver::Class, MethodVisibility::Public) => None,
        (MethodReceiver::Class, MethodVisibility::Protected) => {
            writeln!(s, "{body_pad}class << self").unwrap();
            writeln!(s, "{body_pad}  protected {}", super::expr::ruby_sym_literal(m.name.as_str())).unwrap();
            writeln!(s, "{body_pad}end").unwrap();
            None
        }
    };
    if let Some(directive) = directive {
        writeln!(s, "{body_pad}{directive} {}", super::expr::ruby_sym_literal(m.name.as_str())).unwrap();
    }
}

/// A load-time conditional definition: the source's `if` / `elsif` /
/// `else` (or `unless` / `else`) around each branch's methods, so the
/// branch Ruby selects when the body loads is the one that defines them.
fn write_conditional_definitions(
    s: &mut String,
    lc: &LibraryClass,
    conditional: &crate::dialect::ConditionalDefinitions,
    body_pad: &str,
    instance_form: &[bool],
) {
    let inner_pad = format!("{body_pad}  ");
    for (at, arm) in conditional.arms.iter().enumerate() {
        let head = match (&arm.condition, at) {
            (Some(condition), 0) => {
                format!("{} {}", if conditional.negated { "unless" } else { "if" }, super::emit_expr(condition))
            }
            (Some(condition), _) => format!("elsif {}", super::emit_expr(condition)),
            (None, _) => "else".to_string(),
        };
        for (n, line) in head.lines().enumerate() {
            let pad = if n == 0 { body_pad } else { inner_pad.as_str() };
            writeln!(s, "{pad}{line}").unwrap();
        }
        // The branch's methods, declarations and nested conditionals,
        // in source order.
        enum Item<'a> {
            Method(usize),
            Declaration(&'a crate::dialect::ConditionalDeclaration),
            Nested(&'a crate::dialect::ConditionalDefinitions),
        }
        let mut items: Vec<(u32, Item<'_>)> = arm
            .methods
            .iter()
            .filter_map(|member| lc.methods.iter().position(|m| member.is(m)))
            .map(|index| (lc.methods[index].name_span.start, Item::Method(index)))
            .chain(arm.declarations.iter().map(|d| (d.start, Item::Declaration(d))))
            .chain(arm.nested.iter().map(|n| (n.span.start, Item::Nested(n))))
            .collect();
        items.sort_by_key(|(start, _)| *start);
        let mut first = true;
        for (_, item) in items {
            if !first {
                writeln!(s).unwrap();
            }
            first = false;
            match item {
                Item::Method(index) => write_library_method(s, lc, index, &inner_pad, instance_form),
                Item::Declaration(declaration) => {
                    NestedPlacement::place_declared(&lc.name, s, declaration.name.0.as_str())
                }
                Item::Nested(nested) => write_conditional_definitions(s, lc, nested, &inner_pad, instance_form),
            }
        }
    }
    writeln!(s, "{body_pad}end").unwrap();
}

/// The method `lc.methods[index]` is a native alias of: it is recorded
/// as an alias copy, and the nearest def of the original's name on the
/// same side before it is the very def it was copied from (same body
/// span; a replacing def has its own, and the copy's name span is its
/// alias declaration). Anything else (an original redefined in place,
/// or a copy whose original a later pass dropped or moved) keeps the
/// copied def.
fn inherited_alias<'a>(lc: &'a LibraryClass, m: &MethodDef) -> Option<&'a crate::dialect::MethodAlias> {
    lc.method_aliases.iter().find(|a| a.inherited && a.name == m.name && a.receiver == m.receiver)
}

fn native_alias_original(lc: &LibraryClass, index: usize) -> Option<&MethodDef> {
    let m = &lc.methods[index];
    let alias = lc.method_aliases.iter().find(|a| a.name == m.name && a.receiver == m.receiver)?;
    let original = lc.methods[..index]
        .iter()
        .rev()
        .find(|o| o.name == alias.original && o.receiver == m.receiver)?;
    (original.body.span == m.body.span && original.params.len() == m.params.len() && original.kind == m.kind)
        .then_some(original)
}

fn contains_super(expr: &Expr) -> bool {
    if matches!(&*expr.node, ExprNode::Super { .. }) {
        return true;
    }
    let mut found = false;
    expr.node.for_each_child(&mut |child| found |= contains_super(child));
    found
}

/// `lc`'s file with the source's FILE-level constants (see
/// `LibraryClass::top_level_constants`) written at the top level, around
/// the class: Ruby binds them on Object, so a bare read from any class
/// in any file finds them. Written inside the class they were
/// `Owner::NAME`, and every reader outside `Owner` raised NameError.
/// Those the source wrote ahead of its first declaration come first,
/// the rest after the class, so each runs where the source ran it.
///
/// A line at the top level ahead of the class header also makes this a
/// boot file for the autoloader (`autoload_primary`), which is what a
/// constant nothing names a file for needs: it exists from boot.
fn emit_with_top_level_constants(
    lc: &LibraryClass,
    app: &App,
    out_path: PathBuf,
    synthesized_siblings: &[(String, String)],
) -> EmittedFile {
    let top: crate::det::HashSet<&str> =
        lc.top_level_constants.iter().map(|c| c.name.as_str()).collect();
    let mut class = lc.clone();
    class.constants.retain(|(name, _)| !top.contains(name.as_str()));
    class.top_level_constants.clear();
    // A file of constants alone has no class to write around them.
    let inner = if matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FileConstants)) {
        EmittedFile { path: out_path.clone(), content: String::new() }
    } else {
        emit_library_class_decl_inner(&class, app, out_path.clone(), synthesized_siblings)
    };

    let out_dir = out_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let self_anchor = out_path.with_extension("").to_string_lossy().into_owned();
    let value_of = |name: &Symbol| lc.constants.iter().find(|(n, _)| n == name).map(|(_, v)| v);
    // A top-level initializer runs at load, so what it names is loaded
    // first, exactly as for a class-body constant.
    let mut requires: BTreeSet<String> = BTreeSet::new();
    for c in &lc.top_level_constants {
        let Some(value) = value_of(&c.name) else { continue };
        let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
        walk_const_paths(value, &mut paths);
        for p in &paths {
            if let Some(anchor) = require_path_for_body_const(p, app, "") {
                if anchor != self_anchor {
                    requires.insert(relpath(&out_dir, &anchor));
                }
            }
        }
    }
    let render = |s: &mut String, after: bool| {
        for c in lc.top_level_constants.iter().filter(|c| c.after_declarations == after) {
            let Some(value) = value_of(&c.name) else { continue };
            let rendered = super::emit_expr(value);
            match rendered.lines().next() {
                Some(_) => writeln!(s, "{} = {rendered}", c.name.as_str()).unwrap(),
                None => writeln!(s, "{} = nil", c.name.as_str()).unwrap(),
            }
        }
    };
    let mut s = String::new();
    for r in &requires {
        writeln!(s, "{}", require_line(&out_dir, r, &self_anchor, app)).unwrap();
    }
    if !requires.is_empty() {
        writeln!(s).unwrap();
    }
    let before = s.len();
    render(&mut s, false);
    if s.len() > before && !inner.content.is_empty() {
        writeln!(s).unwrap();
    }
    s.push_str(&inner.content);
    let mut tail = String::new();
    render(&mut tail, true);
    if !tail.is_empty() {
        writeln!(s).unwrap();
        s.push_str(&tail);
    }
    EmittedFile { path: inner.path, content: s }
}

/// For each of `lc`'s constants, the requires (as `resolve` spells them
/// from `lc`'s file) of the OTHER library classes whose load-time code —
/// constant initializers and class-body calls — reads it: qualified
/// (`ContentFilters::EDITOR_FORMATTING_TAGS`), or bare from a class
/// nested inside `lc` (which finds it lexically).
fn constant_readers(
    lc: &LibraryClass,
    app: &App,
    resolve: &dyn Fn(&Vec<String>, bool) -> Option<String>,
) -> crate::det::HashMap<String, BTreeSet<String>> {
    let own = lc.name.0.as_str();
    let own_segs: Vec<&str> = own.split("::").collect();
    let names: crate::det::HashSet<&str> =
        lc.constants.iter().map(|(n, _)| n.as_str()).collect();
    let mut out: crate::det::HashMap<String, BTreeSet<String>> =
        crate::det::HashMap::default();
    if let Some(index) = ConstantReaderIndex::current(app) {
        // The same reads as the walk below, looked up: a qualified
        // `Own::NAME` from any other class, or a bare `NAME` from a
        // class nested inside this one.
        for (cname, _) in &lc.constants {
            let mut qualified: Vec<String> = own_segs.iter().map(|s| s.to_string()).collect();
            qualified.push(cname.as_str().to_string());
            let bare = vec![cname.as_str().to_string()];
            let qualified_readers = index.get(&qualified).into_iter().flatten().map(|&j| (j, false));
            let bare_readers = index.get(&bare).into_iter().flatten().map(|&j| (j, true));
            for (j, needs_nesting) in qualified_readers.chain(bare_readers) {
                let other_name = app.library_classes[j].name.0.as_str();
                if other_name == own || (needs_nesting && !other_name.starts_with(&format!("{own}::"))) {
                    continue;
                }
                let other_path: Vec<String> = other_name.split("::").map(str::to_string).collect();
                if let Some(req) = resolve(&other_path, true) {
                    out.entry(cname.as_str().to_string()).or_default().insert(req);
                }
            }
        }
        return out;
    }
    for other in &app.library_classes {
        let other_name = other.name.0.as_str();
        if other_name == own {
            continue;
        }
        let nested = other_name.starts_with(&format!("{own}::"));
        let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
        for (_, v) in &other.constants {
            walk_const_paths(v, &mut paths);
        }
        for call in other.unknown_calls.iter().chain(&other.class_ivar_initializers) {
            walk_const_paths(call, &mut paths);
        }
        for path in &paths {
            let read = match path.split_last() {
                Some((last, prefix)) if names.contains(last.as_str()) => {
                    prefix.iter().map(String::as_str).eq(own_segs.iter().copied())
                        || (nested && prefix.is_empty())
                }
                _ => false,
            };
            if !read {
                continue;
            }
            let other_path: Vec<String> = other_name.split("::").map(str::to_string).collect();
            if let Some(req) = resolve(&other_path, true) {
                out.entry(path.last().unwrap().clone()).or_default().insert(req);
            }
        }
    }
    out
}

/// Every constant path each library class's load-time code (constant
/// initializers and class-body calls) names, inverted: path → indices
/// into `app.library_classes` of the classes naming it. Built once per
/// `emit_library_class_decls` and dropped when it returns; outside that
/// scope (or for another `App`) `constant_readers` walks as before.
struct ConstantReaderIndex;

type ReaderIndex = std::rc::Rc<HashMap<Vec<String>, Vec<usize>>>;

thread_local! {
    static CONSTANT_READER_INDEX: std::cell::RefCell<Option<(usize, ReaderIndex)>> =
        const { std::cell::RefCell::new(None) };
}

/// Restores the previous index when the emit that installed this one
/// returns. Borrows the `App` for its whole life, so the address the
/// index is keyed by cannot be reused by another `App` meanwhile.
struct ConstantReaderIndexGuard<'a>(Option<(usize, ReaderIndex)>, std::marker::PhantomData<&'a App>);

impl Drop for ConstantReaderIndexGuard<'_> {
    fn drop(&mut self) {
        let previous = self.0.take();
        CONSTANT_READER_INDEX.with(|slot| *slot.borrow_mut() = previous);
    }
}

impl ConstantReaderIndex {
    fn install(app: &App) -> ConstantReaderIndexGuard<'_> {
        let mut index: HashMap<Vec<String>, Vec<usize>> = HashMap::default();
        for (j, other) in app.library_classes.iter().enumerate() {
            let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
            for (_, v) in &other.constants {
                walk_const_paths(v, &mut paths);
            }
            for call in other.unknown_calls.iter().chain(&other.class_ivar_initializers) {
                walk_const_paths(call, &mut paths);
            }
            for path in paths {
                index.entry(path).or_default().push(j);
            }
        }
        let key = app as *const App as usize;
        let previous =
            CONSTANT_READER_INDEX.with(|slot| slot.borrow_mut().replace((key, std::rc::Rc::new(index))));
        ConstantReaderIndexGuard(previous, std::marker::PhantomData)
    }

    /// The installed index, when it was built from this `app`.
    fn current(app: &App) -> Option<ReaderIndex> {
        let key = app as *const App as usize;
        CONSTANT_READER_INDEX
            .with(|slot| slot.borrow().as_ref().filter(|(k, _)| *k == key).map(|(_, index)| index.clone()))
    }
}

/// Both lookup indexes, installed for the life of the returned guard.
/// `emit_library_class_decls` installs its own; the model, controller and
/// view emits in `emit_with_assertions` run outside it and, without
/// these, walked every library class per emitted class
/// (`constant_readers`) and scanned them per constant reference.
pub(crate) struct LookupIndexes<'a> {
    _readers: ConstantReaderIndexGuard<'a>,
    _names: NameIndexGuard<'a>,
}

pub(crate) fn install_lookup_indexes(app: &App) -> LookupIndexes<'_> {
    LookupIndexes { _readers: ConstantReaderIndex::install(app), _names: NameIndex::install(app) }
}

/// The app's class names, for the per-reference lookups: model and
/// controller names, and each library class name's indices into
/// `app.library_classes` (a name can repeat: a reopen is its own entry).
/// Installed and keyed like `ConstantReaderIndex`; without it the
/// lookups scan as before.
struct NameIndex {
    models: crate::det::HashSet<String>,
    controllers: crate::det::HashSet<String>,
    library_classes: HashMap<String, Vec<usize>>,
    /// Each library class => the source files that declare it
    /// (`declared_files`).
    declared_in: HashMap<String, Vec<u32>>,
    /// `carried_sources`, on first use.
    carried: std::cell::OnceCell<std::rc::Rc<CarriedSources>>,
    /// `app_constant_paths`, on first use.
    constant_paths: std::cell::OnceCell<std::rc::Rc<crate::det::HashSet<String>>>,
    /// `superclass_nesting`'s parses, by source file id.
    superclass_nestings: std::cell::RefCell<std::collections::BTreeMap<u32, std::rc::Rc<Vec<(usize, Vec<String>)>>>>,
    /// `conditional_declarations`.
    conditional_declarations: HashMap<String, String>,
    /// `first_module_bare_named`'s answers, on first use.
    first_modules_bare: std::cell::OnceCell<HashMap<String, usize>>,
    /// `require_guard_owners`, on first use.
    require_guards: std::cell::OnceCell<std::rc::Rc<HashMap<String, Option<String>>>>,
    /// `authored_require_sources`, on first use.
    authored_requires: std::cell::OnceCell<std::rc::Rc<HashMap<String, Vec<usize>>>>,
    /// `load_cycle_components`, on first use.
    load_cycles: std::cell::OnceCell<std::rc::Rc<HashMap<String, usize>>>,
}

thread_local! {
    static NAME_INDEX: std::cell::RefCell<Option<(usize, std::rc::Rc<NameIndex>)>> =
        const { std::cell::RefCell::new(None) };
}

struct NameIndexGuard<'a>(Option<(usize, std::rc::Rc<NameIndex>)>, std::marker::PhantomData<&'a App>);

impl Drop for NameIndexGuard<'_> {
    fn drop(&mut self) {
        let previous = self.0.take();
        NAME_INDEX.with(|slot| *slot.borrow_mut() = previous);
    }
}

impl NameIndex {
    fn install(app: &App) -> NameIndexGuard<'_> {
        let mut library_classes: HashMap<String, Vec<usize>> = HashMap::default();
        for (j, lc) in app.library_classes.iter().enumerate() {
            library_classes.entry(lc.name.0.as_str().to_string()).or_default().push(j);
        }
        let index = NameIndex {
            models: app.models.iter().map(|m| m.name.0.as_str().to_string()).collect(),
            controllers: app.controllers.iter().map(|c| c.name.0.as_str().to_string()).collect(),
            library_classes,
            declared_in: declaring_files(app),
            carried: std::cell::OnceCell::new(),
            constant_paths: std::cell::OnceCell::new(),
            superclass_nestings: std::cell::RefCell::default(),
            conditional_declarations: conditional_declarations(app),
            first_modules_bare: std::cell::OnceCell::new(),
            require_guards: std::cell::OnceCell::new(),
            authored_requires: std::cell::OnceCell::new(),
            load_cycles: std::cell::OnceCell::new(),
        };
        let key = app as *const App as usize;
        let previous = NAME_INDEX.with(|slot| slot.borrow_mut().replace((key, std::rc::Rc::new(index))));
        NameIndexGuard(previous, std::marker::PhantomData)
    }

    fn current(app: &App) -> Option<std::rc::Rc<NameIndex>> {
        let key = app as *const App as usize;
        NAME_INDEX.with(|slot| slot.borrow().as_ref().filter(|(k, _)| *k == key).map(|(_, index)| index.clone()))
    }
}

/// Each name a library class's load-time conditional declares
/// (`ConditionalArm::declarations`) => that class.
fn conditional_declarations(app: &App) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::default();
    for lc in &app.library_classes {
        for declaration in lc.conditional_definitions.iter().flat_map(|c| c.all_arms()).flat_map(|arm| arm.declarations.iter()) {
            out.entry(declaration.name.0.as_str().to_string()).or_insert_with(|| lc.name.0.as_str().to_string());
        }
    }
    out
}

/// The library class whose load-time conditional declares `name` or a
/// class enclosing it: the outermost such declaration's owner.
fn conditional_owner(name: &str, app: &App) -> Option<String> {
    let lookup = |declarations: &HashMap<String, String>| {
        let mut end = 0;
        loop {
            let prefix_end = name[end..].find("::").map_or(name.len(), |at| end + at);
            if let Some(owner) = declarations.get(&name[..prefix_end]) {
                return Some(owner.clone());
            }
            if prefix_end == name.len() {
                return None;
            }
            end = prefix_end + 2;
        }
    };
    match NameIndex::current(app) {
        Some(index) => lookup(&index.conditional_declarations),
        None => lookup(&conditional_declarations(app)),
    }
}

/// Whether `name` is a model of this app.
fn is_model_named(name: &str, app: &App) -> bool {
    match NameIndex::current(app) {
        Some(index) => index.models.contains(name),
        None => app.models.iter().any(|m| m.name.0.as_str() == name),
    }
}

/// Whether `name` is a controller of this app.
fn is_controller_named(name: &str, app: &App) -> bool {
    match NameIndex::current(app) {
        Some(index) => index.controllers.contains(name),
        None => app.controllers.iter().any(|c| c.name.0.as_str() == name),
    }
}

/// The library classes named `name`, in `app.library_classes` order.
pub(crate) fn library_classes_named<'a>(name: &str, app: &'a App) -> Vec<&'a LibraryClass> {
    match NameIndex::current(app) {
        Some(index) => index
            .library_classes
            .get(name)
            .map(|js| js.iter().map(|&j| &app.library_classes[j]).collect())
            .unwrap_or_default(),
        None => app.library_classes.iter().filter(|lc| lc.name.0.as_str() == name).collect(),
    }
}

/// The first library module whose name, less any leading `::`, is
/// `target`.
fn first_module_bare_named<'a>(target: &str, app: &'a App) -> Option<&'a LibraryClass> {
    let bare = |lc: &'a LibraryClass| lc.name.0.as_str().trim_start_matches("::");
    match NameIndex::current(app) {
        Some(index) => {
            let first = index.first_modules_bare.get_or_init(|| {
                let mut first: HashMap<String, usize> = HashMap::default();
                for (j, lc) in app.library_classes.iter().enumerate() {
                    if lc.is_module {
                        first.entry(bare(lc).to_string()).or_insert(j);
                    }
                }
                first
            });
            first.get(target).map(|&j| &app.library_classes[j])
        }
        None => app.library_classes.iter().find(|lc| lc.is_module && bare(lc) == target),
    }
}

/// Whether `name` is a model or a library class of this app.
fn is_model_or_library_class(name: &str, app: &App) -> bool {
    is_model_named(name, app) || !library_classes_named(name, app).is_empty()
}

/// Whether `lc` is written in a gem the app compiles as source
/// (roundhouse.yml `gem_sources:`, `App::gem_source_dirs`): the first
/// real span of its methods, constants, superclass or class-body calls
/// falls in one of those dirs.
fn declared_in_gem_source(lc: &LibraryClass, app: &App) -> bool {
    if app.gem_source_dirs.is_empty() {
        return false;
    }
    let span = lc
        .methods
        .iter()
        .map(|m| m.name_span)
        .chain(lc.constants.iter().map(|(_, v)| v.span))
        .chain(std::iter::once(lc.parent_span))
        .chain(lc.unknown_calls.iter().map(|c| c.span))
        .find(|s| !s.is_synthetic());
    let Some(source) = span.and_then(|s| (s.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i))) else {
        return false;
    };
    let path = source.path.as_str();
    let path = if app.root.is_empty() {
        path
    } else {
        path.strip_prefix(app.root.as_str()).map(|p| p.trim_start_matches('/')).unwrap_or(path)
    };
    app.gem_source_dirs.iter().any(|dir| path.starts_with(dir.as_str()))
}

/// The class-body statements of a gem compiled as source
/// (`declared_in_gem_source`) that run as written, outside the modelled
/// DSL: the gem's own Ruby, whose effects its methods depend on.
///
/// - a call on one of the class's own constants: worldwide 1.29.0's
///   `ALL_CURRENCIES.sort_by!(&:currency_code)` and `.freeze` (dropped,
///   `Currencies.all` kept the CLDR file's order), `FIRST_DAY_DATA.freeze`;
/// - `extend` of a core Ruby mixin (`Enumerable`, `Comparable`), which
///   ingest writes for an `include` inside `class << self`: worldwide's
///   `Locales` (dropped, `Locales.known` was NoMethodError);
/// - `singleton_class.alias_method :new, :old` with two symbol names:
///   `Locales`'s `alias_method :known, :to_a`;
/// - `self.x = v` through a class-side writer the class or an app
///   superclass defines: activemerchant 1.137.0's `CreditCard` declares
///   `class << self; attr_accessor :require_name; end` and then runs
///   `self.require_name = true` (dropped, `CreditCard.require_name` was
///   nil, and a card with no name passed validation). One through its
///   own writer is written after the methods (`OwnClassCalls`). Or a
///   writer the `self.included(base)` hook of a module the class or an
///   app superclass includes may define on `base` at the include, in a
///   hook the emit writes as text (`included_hook_may_define`): Ruby runs
///   that hook at the include, as the emitted tree does, and the write
///   after it. A hook with computed names (`NAMES.each { |n|
///   base.class_attribute n }`) is not expanded by
///   `ingest::hook_class_attribute`, so no app method answers the writer.
///
/// `private_constant` stays dropped: the emitter qualifies constant reads
/// (`Worldwide::Currencies::X`), which a private constant refuses.
fn gem_source_body_effects<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    if !declared_in_gem_source(lc, app) {
        return Vec::new();
    }
    let class_name = lc.name.0.as_str().trim_start_matches("::");
    let own_constant = |e: &Expr| match &*e.node {
        ExprNode::Const { path } => {
            let Some((last, scope)) = path.split_last() else { return false };
            let scope = scope.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
            (scope.is_empty() || scope.trim_start_matches("::") == class_name)
                && lc.constants.iter().any(|(name, _)| name == last)
        }
        _ => false,
    };
    let implicit_self = |r: &Option<Expr>| r.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
    let symbol = |e: &Expr| matches!(&*e.node, ExprNode::Lit { value: crate::expr::Literal::Sym { .. } });
    let hooked = crate::ingest::inherited_alias::included_modules(&app.library_classes, lc);
    lc.unknown_calls
        .iter()
        .filter(|call| match &*call.node {
            ExprNode::Send { recv: Some(r), method, .. }
                if own_constant(r) && !matches!(method.as_str(), "private_constant" | "autoload") =>
            {
                true
            }
            ExprNode::Send { recv, method, args, block: None, .. } if method.as_str() == "extend" => {
                implicit_self(recv)
                    && !args.is_empty()
                    && args.iter().all(|a| {
                        matches!(&*a.node, ExprNode::Const { path }
                            if path.len() == 1 && matches!(path[0].as_str(), "Enumerable" | "Comparable"))
                    })
            }
            ExprNode::Send { recv: Some(r), method, args, block: None, .. } if method.as_str() == "alias_method" => {
                matches!(&*r.node, ExprNode::Send { recv, method, args: inner, block: None, .. }
                    if method.as_str() == "singleton_class" && inner.is_empty() && implicit_self(recv))
                    && args.len() == 2
                    && args.iter().all(symbol)
            }
            _ => self_write_or_assign(call).is_some_and(|(name, reads)| {
                let answers = |method: &str| {
                    crate::ingest::inherited_alias::answers(&app.library_classes, lc, method, MethodReceiver::Class)
                        || hooked.iter().any(|m| included_hook_may_define(m, method))
                };
                answers(&format!("{name}=")) && (!reads || answers(name))
            }),
        })
        .collect()
}

/// The attribute a class-body statement writes through `self.x=`, and
/// whether it also reads `self.x`: `self.x = v`, `self.x += v`, and
/// `self.x ||= v` / `&&=`, which the ingest writes as `self.x ||
/// self.x = v`.
fn self_write_or_assign(call: &Expr) -> Option<(&str, bool)> {
    if let Some(name) = crate::ingest::class_attribute::self_write(call) {
        return Some((name, matches!(&*call.node, ExprNode::OpAssign { .. })));
    }
    let ExprNode::BoolOp { left, right, .. } = &*call.node else { return None };
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*left.node else { return None };
    let name = crate::ingest::class_attribute::self_write(right)?;
    (matches!(&*r.node, ExprNode::SelfRef) && args.is_empty() && method.as_str() == name).then_some((name, true))
}

/// Whether `module`'s `self.included(base)` hook may define the
/// class-side method `method` (`x` or `x=`) on `base` when it runs: a
/// call on `base` of `class_attribute` or a `cattr_*` / `mattr_*` macro,
/// an `attr_*` or `define_method` on `base.singleton_class` (also
/// through `send`, `public_send` or `__send__`), `define_singleton_method`
/// on `base`, or those macros receiverless inside a `class_eval` /
/// `class_exec` block on `base` or on its singleton class. Each macro
/// names what it defines (an `_accessor` both sides, a `_writer` only
/// `x=`); a computed name, a splat or a local, may be any.
///
/// activemerchant 1.137.0's `PostsData` hook declares its attributes
/// with literal names, which `ingest::hook_class_attribute` expands into
/// defs on the includers; a hook it does not read keeps its text.
fn included_hook_may_define(module: &LibraryClass, method: &str) -> bool {
    use crate::expr::Literal;
    let Some(hook) = module
        .methods
        .iter()
        .find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "included" && m.params.len() == 1)
    else {
        return false;
    };
    let base = &hook.params[0].name;
    let is_base = |e: &Expr| matches!(&*e.node, ExprNode::Var { name, .. } if name == base);
    let singleton_of_base = |e: &Expr| {
        matches!(&*e.node, ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "singleton_class" && args.is_empty() && is_base(r))
    };
    let (attr, writer) = match method.strip_suffix('=') {
        Some(attr) => (attr, true),
        None => (method, false),
    };
    // Whether `macro_name` called with `args` may define `method`.
    let defines = |macro_name: &str, args: &[Expr]| -> bool {
        let names_attr = matches!(
            macro_name,
            "class_attribute" | "cattr_accessor" | "mattr_accessor" | "attr_accessor"
        ) || (writer && matches!(macro_name, "cattr_writer" | "mattr_writer" | "attr_writer"))
            || (!writer && matches!(macro_name, "cattr_reader" | "mattr_reader" | "attr_reader"));
        let names_method = matches!(macro_name, "define_method" | "define_singleton_method");
        if !names_attr && !names_method {
            return false;
        }
        let wanted = if names_method { method } else { attr };
        let args = if names_method { &args[..args.len().min(1)] } else { args };
        args.iter().any(|a| match &*a.node {
            ExprNode::Lit { value: Literal::Sym { value } } => value.as_str() == wanted,
            ExprNode::Lit { value: Literal::Str { value } } => value == wanted,
            // `instance_writer: false` and the like.
            ExprNode::Hash { .. } => false,
            _ => true,
        })
    };
    let sent = |send_method: &str, args: &[Expr]| -> Option<(String, Vec<Expr>)> {
        if !matches!(send_method, "send" | "public_send" | "__send__") {
            return None;
        }
        let (first, rest) = args.split_first()?;
        match &*first.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some((value.as_str().to_string(), rest.to_vec())),
            ExprNode::Lit { value: Literal::Str { value } } => Some((value.clone(), rest.to_vec())),
            _ => None,
        }
    };
    #[derive(Clone, Copy, PartialEq)]
    enum Body {
        Hook,
        // A `class_eval` block on `base`: self is `base`.
        Base,
        // A `class_eval` block on `base.singleton_class`.
        Singleton,
    }
    fn walk(e: &Expr, at: Body, visit: &dyn Fn(&Expr, Body) -> Option<Body>) {
        let inner = visit(e, at).unwrap_or(at);
        e.node.for_each_child(&mut |c| walk(c, inner, visit));
    }
    let hit = std::cell::Cell::new(false);
    let visit = |e: &Expr, at: Body| -> Option<Body> {
        let ExprNode::Send { recv, method: m, args, block, .. } = &*e.node else { return None };
        let m = m.as_str();
        let self_recv = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
        let on_base = recv.as_ref().is_some_and(|r| is_base(r)) || (at == Body::Base && self_recv);
        let on_singleton = recv.as_ref().is_some_and(|r| singleton_of_base(r))
            || (at == Body::Singleton && self_recv)
            || (at == Body::Base
                && recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::Send { recv, method, args, block: None, .. }
                    if method.as_str() == "singleton_class" && args.is_empty()
                        && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)))));
        let (name, args): (String, Vec<Expr>) = sent(m, args).unwrap_or_else(|| (m.to_string(), args.clone()));
        let class_side = |n: &str| {
            matches!(
                n,
                "class_attribute" | "cattr_accessor" | "cattr_reader" | "cattr_writer"
                    | "mattr_accessor" | "mattr_reader" | "mattr_writer" | "define_singleton_method"
            )
        };
        if (on_base && class_side(&name) && defines(&name, &args))
            || (on_singleton
                && matches!(name.as_str(), "attr_accessor" | "attr_reader" | "attr_writer" | "define_method")
                && defines(&name, &args))
        {
            hit.set(true);
        }
        if block.is_some() && matches!(m, "class_eval" | "class_exec" | "module_eval" | "module_exec") {
            if on_singleton {
                return Some(Body::Singleton);
            }
            if on_base {
                return Some(Body::Base);
            }
        }
        None
    };
    walk(&hook.body, Body::Hook, &visit);
    hit.get()
}

/// The methods of `lc` (indexes into `lc.methods`) whose bodies run
/// while its file loads, so that what they reference is a load-time
/// need of the file, like a constant's value. Only a class whose load
/// runs its own methods has any (`partition_deferred_constants`): a
/// constant that dispatches to the class, or, in a body written in
/// source order, a class-variable initializer that does. They are the
/// methods that code calls (`new` runs `initialize`; a name it sends,
/// passes as a symbol, `method(:x)` or `&:x`), what those call in turn,
/// and the hooks Ruby calls on its own (`to_s` for an interpolation,
/// `hash` and `eql?` for a Hash key, and so on). A closure the code
/// only stores runs later and adds none.
///
/// Core's `Merchandising::Metafields::FileReferenceType::FileTypes` is a
/// T::Enum: its values (`Image = new("Image")`) run `initialize`, not
/// `graphql_type` or `model_class`, whose `Merchandising::Media` and
/// `Merchandising::GraphApi::Admin::Model3dEnvironmentImage` are read
/// when those are called. Counting every body made the file require
/// media.rb, and media.rb's load reaches
/// `StoreLocalization::LocalizableContentTypes`, which requires
/// file_reference_type.rb back: a circular require natively absent.
///
/// `None` when the methods that run cannot be told from the code: a
/// dynamic dispatch (`send(name)`), or a constant that hands control to
/// another app class at load (`Registry.build(self)`), whose code can
/// call any method of this one. Every body then counts.
fn load_time_methods(
    lc: &LibraryClass,
    app: &App,
    deferred: &[usize],
    ordered_state: bool,
    initializers_call_self: bool,
) -> Option<BTreeSet<usize>> {
    if deferred.is_empty() && !(ordered_state && initializers_call_self) {
        return Some(BTreeSet::new());
    }
    /// The methods Ruby itself calls on an object or class as code runs.
    const IMPLICIT: &[&str] = &[
        "initialize", "initialize_copy", "to_s", "inspect", "to_str", "to_ary", "to_a", "to_hash", "to_h",
        "to_proc", "hash", "eql?", "==", "===", "<=>", "each", "coerce", "method_missing",
        "respond_to_missing?", "respond_to?", "const_missing", "method_added", "singleton_method_added",
    ];
    const DYNAMIC: &[&str] = &[
        "send", "public_send", "__send__", "try", "try!", "method", "public_method", "instance_method",
        "public_instance_method",
    ];
    struct Walk<'a> {
        own: crate::det::HashSet<&'a str>,
        class_name: &'a str,
        app: &'a App,
        reached: BTreeSet<String>,
        opaque: bool,
    }
    impl Walk<'_> {
        fn reach(&mut self, name: &str) {
            if self.own.contains(name) {
                self.reached.insert(name.to_string());
            }
        }
        fn is_own(&self, recv: &Option<Expr>) -> bool {
            match recv.as_ref().map(|r| &*r.node) {
                None | Some(ExprNode::SelfRef) => true,
                Some(ExprNode::Const { path }) => {
                    path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::")
                        == self.class_name
                }
                _ => false,
            }
        }
        /// `seed`: the constant or initializer itself, not a method
        /// body it runs.
        fn walk(&mut self, e: &Expr, seed: bool) {
            match &*e.node {
                // A closure literal is stored, not run.
                ExprNode::Lambda { .. } => {}
                ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => self.reach(value.as_str()),
                ExprNode::MethodRef { name, .. } => self.reach(name.as_str()),
                ExprNode::Send { recv, method, args, block, .. } => {
                    let m = method.as_str();
                    if m == "new" && self.is_own(recv) {
                        self.reach("initialize");
                    }
                    self.reach(m);
                    if DYNAMIC.contains(&m) {
                        match args.first().map(|a| &*a.node) {
                            Some(ExprNode::Lit { value: crate::expr::Literal::Sym { .. } }) => {}
                            Some(ExprNode::Lit { value: crate::expr::Literal::Str { value } }) => self.reach(value),
                            _ => self.opaque = true,
                        }
                    }
                    if seed {
                        if let Some(ExprNode::Const { path }) = recv.as_ref().map(|r| &*r.node) {
                            let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                            let name = name.trim_start_matches("::");
                            if name != self.class_name && is_model_or_library_class(name, self.app) {
                                self.opaque = true;
                            }
                        }
                    }
                    if let Some(r) = recv {
                        self.walk(r, seed);
                    }
                    for a in args {
                        self.walk(a, seed);
                    }
                    if let Some(b) = block.as_ref().filter(|_| !stores_block(recv, m)) {
                        match &*b.node {
                            ExprNode::Lambda { body, .. } => self.walk(body, seed),
                            _ => self.walk(b, seed),
                        }
                    }
                }
                _ => e.node.for_each_child(&mut |child| self.walk(child, seed)),
            }
        }
    }
    let mut walk = Walk {
        own: lc.methods.iter().map(|m| m.name.as_str()).collect(),
        class_name: lc.name.0.as_str().trim_start_matches("::"),
        app,
        reached: BTreeSet::new(),
        opaque: false,
    };
    for &i in deferred {
        walk.walk(&lc.constants[i].1, true);
    }
    for init in &lc.class_ivar_initializers {
        walk.walk(init, true);
    }
    for name in IMPLICIT {
        walk.reach(name);
    }
    let mut done: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = walk.reached.iter().find(|n| !done.contains(*n)).cloned() {
        for m in lc.methods.iter().filter(|m| m.name.as_str() == name) {
            walk.walk(&m.body, false);
        }
        done.insert(name);
    }
    if walk.opaque {
        return None;
    }
    Some(
        lc.methods
            .iter()
            .enumerate()
            .filter(|(_, m)| walk.reached.contains(m.name.as_str()))
            .map(|(i, _)| i)
            .collect(),
    )
}

/// Split a class's constants into the ones that can be initialized
/// before its methods exist and the ones that can't. A constant defers
/// when its initializer dispatches to this class — a bare `new(…)` or a
/// receiverless call naming one of its own methods, or either of those
/// SPELLED WITH THE CLASS NAME (`Sound.new(…)`) — or when it reads a
/// constant that already deferred. A self-dispatch inside a STORED
/// closure doesn't count: that body runs on call, not on load. Returns
/// index lists so both groups keep source order, and whether a
/// class-ivar initializer dispatches to this class the same way (its
/// method bodies then run at load too).
///
/// The class-name spelling is not hypothetical: `lower::class_body_new`
/// gives a class body's bare `new` its receiver (a receiverless call is
/// unresolvable on a strict target), and a rule that keyed on the bare
/// spelling alone stopped seeing campfire's `Sound::BUILTIN` the moment
/// it did. That emitted fifty-six constructor calls ABOVE the
/// `initialize` they call — `BasicObject#initialize: wrong number of
/// arguments (given 1, expected 0)` at load, on every one of the
/// suite's 52 files. The fact the rule is about is "does this
/// initializer dispatch to this class", and both spellings are that
/// fact.
fn partition_deferred_constants(lc: &LibraryClass, app: &App) -> (Vec<usize>, Vec<usize>, bool) {
    fn is_own_class(recv: &Option<Expr>, class_name: &str) -> bool {
        let Some(r) = recv else { return false };
        if matches!(&*r.node, ExprNode::SelfRef) { return true; }
        let ExprNode::Const { path } = &*r.node else { return false };
        path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::") == class_name
    }
    /// Implicit self, `self`, or the class spelled out.
    fn is_self_or_own_class(recv: &Option<Expr>, class_name: &str) -> bool {
        recv.is_none()
            || recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::SelfRef))
            || is_own_class(recv, class_name)
    }
    /// Does this Const name a constant already marked deferred — bare
    /// `BUILTIN`, or `OwnClass::BUILTIN` (qualified by a later rewrite)?
    fn const_names_deferred(
        path: &[crate::ident::Symbol],
        deferred_names: &crate::det::HashSet<String>,
        class_name: &str,
    ) -> bool {
        if path.is_empty() {
            return false;
        }
        if path.len() == 1 {
            return deferred_names.contains(path[0].as_str());
        }
        let leaf = path[path.len() - 1].as_str();
        if !deferred_names.contains(leaf) {
            return false;
        }
        let prefix = path[..path.len() - 1]
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("::");
        prefix == class_name
    }
    fn calls_self(expr: &Expr, own: &crate::det::HashSet<&str>, deferred_names: &crate::det::HashSet<String>, class_name: &str) -> bool {
        match &*expr.node {
            // A closure that is STORED rather than run — `proc { … }`,
            // `lambda { … }`, `Proc.new { … }`, `->() { … }` — executes
            // its body when something CALLS it, not when the constant
            // initializes. lobsters' `CACHE_PAGE = proc { …
            // clear_session_cookie? }` otherwise reads as a self-dispatch,
            // and deferring it marks the whole class load-time-bodies,
            // which pulls every method-body const ref into the require
            // header: that closed a cycle (application_controller →
            // user → avatars_controller → application_controller) and
            // left the emitted tree unable to load at all.
            //
            // Blocks that DO run now (`map`, `each`) are descended into
            // at the Send arm below, so a Lambda reaching this arm is a
            // stored literal.
            ExprNode::Lambda { .. } => false,
            // `&method(:own)`, ingested as a MethodRef: the same lookup.
            ExprNode::MethodRef { recv, name }
                if is_self_or_own_class(recv, class_name) && own.contains(name.as_str()) =>
            {
                true
            }
            ExprNode::Send { recv, method, args, block, .. } => {
                if (recv.is_none() || is_own_class(recv, class_name))
                    && (method.as_str() == "new" || own.contains(method.as_str()))
                {
                    return true;
                }
                // `method(:own)` (and `instance_method`, `public_method`,
                // `public_instance_method`) looks the method up as it
                // runs: above the `def` it is `NameError: undefined
                // method`. paquito's `TYPES = [{ packer:
                // method(:time_pack_deprecated), ... }]` follows its
                // `class << self` defs natively.
                if is_self_or_own_class(recv, class_name)
                    && matches!(
                        method.as_str(),
                        "method" | "public_method" | "instance_method" | "public_instance_method"
                    )
                    && args.first().is_some_and(|a| {
                        matches!(&*a.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } } if own.contains(value.as_str()))
                    })
                {
                    return true;
                }
                let stores_block = matches!(method.as_str(), "proc" | "lambda")
                    || (method.as_str() == "new"
                        && matches!(
                            recv.as_ref().map(|r| &*r.node),
                            Some(ExprNode::Const { path })
                                if path.len() == 1 && path[0].as_str() == "Proc"
                        ));
                recv.iter()
                    .chain(args.iter())
                    .any(|e| calls_self(e, own, deferred_names, class_name))
                    || (!stores_block
                        && block.as_ref().is_some_and(|b| match &*b.node {
                            ExprNode::Lambda { body, .. } => {
                                calls_self(body, own, deferred_names, class_name)
                            }
                            _ => calls_self(b, own, deferred_names, class_name),
                        }))
            }
            // Bare `BUILTIN` *and* `Sound::BUILTIN` — index_by grounding
            // (and similar) rewrites the receiver to the qualified form,
            // and a rule that only matched path.len() == 1 left INDEX
            // eager while BUILTIN deferred: `uninitialized constant
            // Sound::BUILTIN` at load (campfire models.rb → sound.rb).
            ExprNode::Const { path } if const_names_deferred(path, deferred_names, class_name) => {
                true
            }
            _ => {
                let mut found = false;
                expr.node.for_each_child(&mut |child| {
                    if !found && calls_self(child, own, deferred_names, class_name) {
                        found = true;
                    }
                });
                found
            }
        }
    }

    /// The constants a value writes as it runs (`ALL = [VISIT = new(…)]`
    /// writes VISIT): they exist only once that value has run.
    fn written_constants(expr: &Expr, out: &mut crate::det::HashSet<String>) {
        if let ExprNode::Assign { target: crate::expr::LValue::Const { path }, .. } = &*expr.node {
            if let [name] = path.as_slice() {
                out.insert(name.as_str().to_string());
            }
        }
        expr.node.for_each_child(&mut |child| written_constants(child, out));
    }

    /// Whether a value, as it runs, calls a method of one of the app's
    /// classes (`X.new`, `X.m` on a model or library class): that code
    /// may call back into this class. A stored closure runs later.
    fn runs_app_code(expr: &Expr, app: &App) -> bool {
        match &*expr.node {
            ExprNode::Lambda { .. } => false,
            ExprNode::Send { recv: Some(r), .. }
                if matches!(&*r.node, ExprNode::Const { path }
                    if is_model_or_library_class(path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::"), app)) =>
            {
                true
            }
            // A block a call runs now (`map`, `each`) runs with the value;
            // a `proc`/`lambda` literal is stored, as in `calls_self`.
            ExprNode::Send { recv, method, args, block, .. } => {
                let stores_block = matches!(method.as_str(), "proc" | "lambda");
                recv.iter().chain(args.iter()).any(|e| runs_app_code(e, app))
                    || (!stores_block
                        && block.as_ref().is_some_and(|b| match &*b.node {
                            ExprNode::Lambda { body, .. } => runs_app_code(body, app),
                            _ => runs_app_code(b, app),
                        }))
            }
            _ => {
                let mut found = false;
                expr.node.for_each_child(&mut |child| {
                    if !found && runs_app_code(child, app) {
                        found = true;
                    }
                });
                found
            }
        }
    }

    /// The parent of the model or library class `name`, if any.
    fn parent_of<'a>(name: &str, app: &'a App) -> Option<&'a str> {
        if let Some(m) = app.models.iter().find(|m| m.name.0.as_str() == name) {
            return m.parent.as_ref().map(|p| p.0.as_str());
        }
        library_classes_named(name, app).into_iter().find_map(|c| c.parent.as_ref()).map(|p| p.0.as_str())
    }

    /// Whether `name` is a class below `ancestor`, by the app's models
    /// and library classes.
    fn descends_from(name: &str, ancestor: &str, app: &App) -> bool {
        let mut at = parent_of(name, app);
        let mut steps = 0;
        while let Some(parent) = at {
            let parent = parent.trim_start_matches("::");
            if parent == ancestor {
                return true;
            }
            steps += 1;
            if steps > 32 {
                return false;
            }
            at = parent_of(parent, app);
        }
        false
    }

    /// Whether a value, as it runs, names a subclass of `class_name`
    /// (`PRIORITIES = { Country.name => 1 }` in Core's Markets::Region):
    /// naming it loads it, and its class body runs against this class —
    /// Country's `model_uuid_attributes` asks Region's compiled
    /// `_retypable_attribute_names`. Above the defs, that is the Base's
    /// answer. A stored closure runs later.
    fn names_subclass(expr: &Expr, class_name: &str, app: &App) -> bool {
        match &*expr.node {
            ExprNode::Lambda { .. } => false,
            ExprNode::Const { path } => {
                let joined = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                let joined = joined.trim_start_matches("::");
                descends_from(joined, class_name, app)
                    || (path.len() == 1 && descends_from(&format!("{class_name}::{joined}"), class_name, app))
            }
            ExprNode::Send { method, .. } if matches!(method.as_str(), "proc" | "lambda") => false,
            _ => {
                let mut found = false;
                expr.node.for_each_child(&mut |child| {
                    if !found && names_subclass(child, class_name, app) {
                        found = true;
                    }
                });
                found
            }
        }
    }

    let own: crate::det::HashSet<&str> =
        lc.methods.iter().map(|m| m.name.as_str()).collect();
    // A gem source's class body runs as the gem's Ruby does, top to
    // bottom: a constant the source writes below one of the class's own
    // defs, whose value runs app code, may reach that def through
    // another class. worldwide 1.29.0's `Currencies::ALL_CURRENCIES`
    // builds `Currency.new(code:)` below `def numeric_code_for`, and
    // `Currency#initialize` calls `Currencies.numeric_code_for`; hoisted
    // above the defs it raised NoMethodError at load. Such a constant
    // waits for the methods, where the source had it.
    let below_own_def = |value: &Expr| {
        !value.span.is_synthetic()
            && lc.methods.iter().any(|m| {
                !m.name_span.is_synthetic()
                    && m.name_span.file == value.span.file
                    && m.name_span.start < value.span.start
            })
    };
    let gem_source = declared_in_gem_source(lc, app);
    // A constant whose value calls a class method the body defines
    // nowhere itself (`VALID = statuses`, which a class-body macro
    // above it defined on the singleton) waits for the replayed class-body
    // calls the source has above it: those are written after the
    // eager constants. A model's replayed calls are its class-ivar
    // initializers (`collect_class_attr_initializers`).
    let reads_macro_defined = |value: &Expr| {
        !value.span.is_synthetic()
            && calls_unowned_class_method(value, &own)
            && lc.unknown_calls.iter().chain(&lc.class_ivar_initializers).any(|c| {
                !c.span.is_synthetic() && c.span.file == value.span.file && c.span.end <= value.span.start
            })
    };
    let mut deferred_names: crate::det::HashSet<String> = crate::det::HashSet::default();
    let (mut eager, mut deferred) = (Vec::new(), Vec::new());
    for (i, (name, value)) in lc.constants.iter().enumerate() {
        if calls_self(value, &own, &deferred_names, lc.name.0.as_str())
            || reads_macro_defined(value)
            || (gem_source && below_own_def(value) && runs_app_code(value, app))
            || names_subclass(value, lc.name.0.as_str().trim_start_matches("::"), app)
        {
            deferred_names.insert(name.as_str().to_string());
            written_constants(value, &mut deferred_names);
            deferred.push(i);
        } else {
            eager.push(i);
        }
    }
    let initializers_call_self = lc
        .class_ivar_initializers
        .iter()
        .any(|init| calls_self(init, &own, &deferred_names, lc.name.0.as_str()));
    (eager, deferred, initializers_call_self)
}

/// Whether `value` calls, on the class itself (no receiver, or `self`), a
/// method that is neither one of the class's own defs nor one any class
/// answers ([`ANY_CLASS_SELF_CALLS`]): a class method an earlier class-body
/// macro defined (`symbol_enum :status, [...]` gives the class `statuses`).
/// A stored closure's body runs later and does not count.
fn calls_unowned_class_method(value: &Expr, own: &crate::det::HashSet<&str>) -> bool {
    match &*value.node {
        ExprNode::Lambda { .. } => false,
        ExprNode::Send { recv, method, args, block, .. } => {
            let on_self = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
            if on_self
                && !own.contains(method.as_str())
                && !ANY_CLASS_SELF_CALLS.contains(&method.as_str())
                && !matches!(method.as_str(), "new" | "require" | "require_relative" | "binding" | "block_given?")
            {
                return true;
            }
            let stores_block = matches!(method.as_str(), "proc" | "lambda");
            recv.iter().chain(args.iter()).any(|e| calls_unowned_class_method(e, own))
                || (!stores_block && block.as_ref().is_some_and(|b| calls_unowned_class_method(b, own)))
        }
        _ => {
            let mut found = false;
            value.node.for_each_child(&mut |child| {
                if !found && calls_unowned_class_method(child, own) {
                    found = true;
                }
            });
            found
        }
    }
}

/// One statement of a body written in source order (`ordered_class_state`):
/// a constant, a class-ivar initializer or a def, by index.
#[derive(Clone, Copy)]
enum StateItem {
    Constant(usize),
    Initializer(usize),
    Method(usize),
}

/// A class-body write to the class object's own state: `@x = v`,
/// `@x ||= v`, `@x &&= v`, `@x += v` (`ingest::library_class::
/// with_class_body_ivar_writes`).
fn is_class_state_write(expr: &Expr) -> bool {
    matches!(&*expr.node,
        ExprNode::Assign { target: crate::expr::LValue::Ivar { .. }, .. }
            | ExprNode::OpAssign { target: crate::expr::LValue::Ivar { .. }, .. })
}

/// Whether `lc`'s body is written in source order: its constants, its
/// class-ivar initializers and its defs interleaved as the source has
/// them, instead of constants, then initializers, then defs. Ruby runs a
/// class body top to bottom, and state the body writes on the class
/// object (`@value = 1` … `FIRST = @value` … `@value = 2`) is read by
/// what stands between the writes: a constant snapshot, a class method
/// a later constant calls, an accessor a `class << self` redefines.
///
/// Only a body whose every one of those statements has a real position
/// in one file and that has no other statement whose placement the emit
/// decides separately: no replayed or modelled class-body calls, body
/// statements, load-time conditional definitions, positioned or late
/// includes, or Concern-carried attributes and `included do` statements.
/// Lowered classes keep their own policy: a model, a controller (whose
/// Concern methods carry another file's spans), a factory class, a
/// railtie. Every other body keeps the constants-first order, and one
/// that state makes wrong is refused (`refuse_unordered_class_state`).
fn ordered_class_state(lc: &LibraryClass, app: &App) -> bool {
    let Some(first) = lc.class_ivar_initializers.iter().find(|e| is_class_state_write(e) && !e.span.is_synthetic()) else {
        return false;
    };
    let file = first.span.file;
    let here = |span: &Span| !span.is_synthetic() && span.file == file;
    lc.origin.is_none()
        && !app.models.iter().any(|m| m.name == lc.name)
        && !app.controllers.iter().any(|c| c.name == lc.name)
        && !carried_railtie(lc, app)
        && !replays_foreign_class_body(lc, app)
        && modelled_class_body_calls(lc, app).is_empty()
        && !lc.unknown_calls.iter().any(is_body_statement)
        && lc.conditional_definitions.is_empty()
        && app.included_class_attributes.get(&lc.name).is_none_or(|groups| groups.is_empty())
        && app.included_block_statements.get(&lc.name).is_none_or(|statements| statements.is_empty())
        && lc.includes.iter().all(|inc| hooked_include_position(lc, inc, app).is_none())
        && late_includes(lc, app, &[]).is_empty()
        && lc.class_ivar_initializers.iter().all(|e| here(&e.span))
        && lc.constants.iter().all(|(_, value)| here(&value.span))
        && lc.methods.iter().all(|m| m.name_span.is_synthetic() || here(&m.name_span))
}

/// An ordered body's statements (`ordered_class_state`) in source order:
/// every constant, every initializer, and every def with a source
/// position. A synthesized def (`class_attribute :knob`'s `self.knob=`)
/// has none and comes first: a state write below its macro may call it
/// (`self.knob = base + 1`), and a def runs nothing when it is written.
/// An alias of an inherited method follows with the rest of the defs.
fn ordered_state_items(lc: &LibraryClass, eager: &[usize]) -> Vec<StateItem> {
    let mut items: Vec<(u32, StateItem)> =
        eager.iter().map(|&i| (lc.constants[i].1.span.start, StateItem::Constant(i))).collect();
    items.extend(lc.class_ivar_initializers.iter().enumerate().map(|(i, e)| (e.span.start, StateItem::Initializer(i))));
    items.extend(
        lc.methods
            .iter()
            .enumerate()
            .filter(|(_, m)| inherited_alias(lc, m).is_none())
            .map(|(i, m)| (if m.name_span.is_synthetic() { 0 } else { m.name_span.start }, StateItem::Method(i))),
    );
    // Stable: the accessors one `attr_accessor` declares share its position.
    items.sort_by_key(|(start, _)| *start);
    items.into_iter().map(|(_, item)| item).collect()
}

/// A body that writes class state but is not written in source order
/// (`ordered_class_state`) writes its constants before that state: a
/// constant the source has below a state write, and that reads an
/// instance variable, would read the state before the write. Refused by
/// name rather than emitted reading `nil`.
fn refuse_unordered_class_state(lc: &LibraryClass) {
    let Some(first) = lc.class_ivar_initializers.iter().filter(|e| is_class_state_write(e) && !e.span.is_synthetic()).map(|e| e.span).min_by_key(|span| span.start) else {
        return;
    };
    for (name, value) in &lc.constants {
        if value.span.is_synthetic() || value.span.file != first.file || value.span.start < first.start {
            continue;
        }
        if crate::ingest::library_class::reads_instance_variable(value) {
            let mut diagnostic = crate::lower::residue_diagnostic(
                "library_class_body",
                name.as_str(),
                value.span,
                "class state order",
                format!(
                    "`{}::{}` reads an instance variable after a class-level instance-variable write, \
                     which requires source-order initialization; the other statements of the body of `{}` \
                     (replayed calls, includes, conditional definitions, or lowered framework members) keep it \
                     from being written in source order",
                    lc.name.0.as_str(),
                    name.as_str(),
                    lc.name.0.as_str(),
                ),
            );
            diagnostic.severity = crate::diagnostic::Severity::Error;
            crate::emit::diagnostics::push(diagnostic);
        }
    }
}

/// Project-root-anchored require target for a parent class, if one is needed.
/// `ActiveRecord::Base` lives in the runtime; same-dir parents
/// (ApplicationRecord, custom abstract bases) resolve to a sibling under
/// `app/models/`. Everything else returns `None` (assume the loader sees
/// the parent some other way).
/// The runtime stem a framework-class REOPEN must load first, if the
/// class lives in a runtime namespace this tree ships. App classes with
/// `::` names outside these namespaces (ShortId::CandidateId) get None.
fn runtime_reopen_anchor(name: &str) -> Option<&'static str> {
    for (prefix, anchor) in [
        ("ActiveRecord", "runtime/active_record"),
        ("ActionController", "runtime/action_controller"),
        ("ActionView", "runtime/action_view"),
        ("ActionDispatch", "runtime/action_dispatch"),
        ("ActionMailer", "runtime/action_mailer"),
        ("ActiveJob", "runtime/active_job"),
    ] {
        if name == prefix || name.strip_prefix(prefix).is_some_and(|r| r.starts_with("::")) {
            return Some(anchor);
        }
    }
    None
}

/// Is `expr` itself a call on the implicit or explicit `self` (a
/// class-body macro), rather than an assignment?
fn is_self_call(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Send { recv, .. }
        if recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)))
}

/// Does `expr` call, on the implicit or explicit `self`, a class method
/// named in `own`? Such a class-body statement must follow the `def`s.
fn calls_own_class_method(expr: &Expr, own: &crate::det::HashSet<&Symbol>) -> bool {
    if let ExprNode::Send { recv, method, .. } = &*expr.node {
        if (recv.is_none() || recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::SelfRef)))
            && own.contains(method)
        {
            return true;
        }
    }
    let mut found = false;
    expr.node.for_each_child(&mut |child| {
        found = found || calls_own_class_method(child, own);
    });
    found
}

/// Whether the emitted class replays its unmodelled class-body calls
/// verbatim (a railtie the tree carries, or [`replays_foreign_class_body`])
/// rather than dropping them: `check` asks before it calls a dropped
/// validation an error (`lower::active_model_model::diagnose_dropped_validations`).
pub(crate) fn replays_class_body(lc: &LibraryClass, app: &App) -> bool {
    carried_railtie(lc, app) || replays_foreign_class_body(lc, app)
}

/// Whether this class's captured `unknown_calls` should be replayed
/// into the emitted class body.
///
/// The test is "does the class extend a base we model at all?".
/// `require_path_for_parent` already answers that — it resolves every
/// framework base (`ActiveJob::Base`, `ActionMailer::Base`, …) and
/// every app-local parent to a require target, and returns `None` only
/// for a superclass that is neither. `SearchParser < Parslet::Parser`
/// lands in that `None` case: the base comes from a gem, the gem is in
/// the app's bundle, and its class-body DSL is the class. Replaying is
/// the only way to emit it, and it is safe because the DSL's receiver
/// is the gem's own class.
///
/// Everything else deliberately stays dropped, because a call we don't
/// recognize on a base we DO model is already the lowering's business
/// and replaying it would be wrong twice over:
///
///   - a concern's `included do … end` is spliced into each includer by
///     `splice_concerns_into_models`; replaying it here would apply the
///     callbacks a second time, on a module the runtime gives no
///     `included` hook to,
///   - `queue_as` / `rescue_from` / `default from:` / `delegate` are
///     framework macros whose receiver is our own runtime base class.
///     Where the runtime doesn't define one, replaying turns a silent
///     modelling gap into a NoMethodError at class-definition time,
///     i.e. an app that won't boot.
///
/// A dropped call is no longer silent either way — see
/// `report_dropped_class_body_call`.
///
/// A reopen of a loaded gem's class or module (`reopens_loaded_gem_class`)
/// replays for the same reason: the receiver is the gem's own class.
///
/// The base is found through the app's own classes: a superclass the
/// app compiles (a gem source's, carried as app code) is not the end of
/// the chain. Core's `GraphApi::ObjectType < GraphQL::Shopify::Types::BaseObject`
/// (graphql-shopify, a gem source) `< GraphQL::Schema::Object` (the
/// graphql gem, loaded natively): every `field` / `description` /
/// `argument` in a Core type's body is graphql's DSL on graphql's class,
/// and dropping it loads the type with no fields. A module replays by
/// the same rule when it includes, directly or through app modules, a
/// module of a natively loaded gem (`GraphApi::InterfaceType` includes
/// graphql-shopify's `BaseInterface`, which includes
/// `GraphQL::Schema::Interface`), unless it is a concern (its hooks are
/// spliced into each includer).
fn replays_foreign_class_body(lc: &LibraryClass, app: &App) -> bool {
    InlineDeclaration::active() || replays_foreign_base(lc, app)
}

/// [`replays_foreign_class_body`] by the class's base alone, as it is
/// wherever the class is written.
fn replays_foreign_base(lc: &LibraryClass, app: &App) -> bool {
    if reopens_loaded_gem_class(lc, app) {
        return true;
    }
    if lc.is_module {
        return !is_concern_body(lc) && includes_loaded_gem_module(lc, app, &mut Vec::new());
    }
    let mut seen: Vec<&str> = vec![lc.name.0.as_str()];
    let mut parent = lc.parent.as_ref();
    while let Some(p) = parent {
        if require_path_for_parent(p, app).is_none() {
            return !is_core_class_name(p.0.as_str());
        }
        // An app class (a gem source's included): its own superclass
        // decides. A model, a controller or a runtime base ends here.
        if is_model_named(p.0.as_str(), app) || seen.contains(&p.0.as_str()) {
            return false;
        }
        seen.push(p.0.as_str());
        parent = library_classes_named(p.0.as_str(), app).into_iter().filter(|c| !c.is_module).find_map(|c| c.parent.as_ref());
    }
    false
}

/// The spans of the body statements the app's classes carry
/// (`carries_body_statement`; the ingest keeps only those): no
/// `file_statement_unsupported` gap where Ruby runs them, the file's
/// program not naming them on CRuby/JRuby.
pub(crate) fn carried_body_statement_spans(app: &App) -> crate::det::HashSet<Span> {
    body_statement_classes(app)
        .into_iter()
        .flat_map(|lc| lc.unknown_calls.iter())
        .filter(|c| is_body_statement(c))
        .map(|c| c.span)
        .collect()
}

/// The classes whose bodies can keep a statement whole: the library
/// classes and the declarations a file program carries inline (in a
/// file-level branch or behind a `return`).
pub(crate) fn body_statement_classes(app: &App) -> Vec<&LibraryClass> {
    fn inline<'a>(items: &'a [crate::app::ProgramItem], out: &mut Vec<&'a LibraryClass>) {
        for item in items {
            match item {
                crate::app::ProgramItem::Inline(classes) => out.extend(classes.iter()),
                crate::app::ProgramItem::Branch { then_items, else_items, .. } => {
                    inline(then_items, out);
                    inline(else_items, out);
                }
                _ => {}
            }
        }
    }
    let mut classes: Vec<&LibraryClass> = app.library_classes.iter().collect();
    for program in &app.file_programs {
        inline(&program.items, &mut classes);
    }
    classes
}

/// `text`'s lines at `pad`, a blank line kept blank.
fn write_body_lines(s: &mut String, pad: &str, text: &str) {
    for line in text.lines() {
        if line.is_empty() {
            writeln!(s).unwrap();
        } else {
            writeln!(s, "{pad}{line}").unwrap();
        }
    }
}

/// Whether `call` is a class or module body statement the ingest
/// captured whole (`expr::BODY_STATEMENT`).
fn is_body_statement(call: &Expr) -> bool {
    call.decisions & crate::expr::BODY_STATEMENT != 0
}

/// Whether the Ruby family writes `call`, a body statement the ingest
/// captured (`expr::BODY_STATEMENT`: a call on a computed receiver,
/// `x if cond`, `begin`, `case`, a loop), back into `lc`'s emitted body
/// where the source has it, which runs it as Ruby does when the body
/// loads. Core's GraphQL enum bodies, `Foo.values.each do |d| value
/// d.serialize, description: d.description end` on graphql's
/// `GraphQL::Schema::Enum`, and essentials' `Trilogy.public_instance_
/// methods(false).each do |m| define_method(m) { … } end`.
///
/// It is, when
/// - the body replays its class-body DSL (a foreign base's, a loaded
///   gem's reopen, a carried railtie: `replays_class_body`), whose
///   receiverless calls are the base's own; or the statement calls on
///   the class only what any Ruby class answers (`define_method`,
///   `const_get`, …) or a class method the class itself defines
///   (`self_calls_answered`), so no runtime base's missing macro is
///   reached; and
/// - its place is kept (`body_statement_place`): it is written before
///   the first def the source has below it, so every def above it is
///   defined and none below, and when a def is above it nothing the
///   body runs before the defs follows it in the source.
///
/// Not in a `class_eval` body (written by `class_eval_parts`) or a
/// Concern's (spliced into each includer). Decided with the app in, for
/// the ingest (`ingest::app::keep_carried_body_statements`), check (a
/// carried statement is no `file_statement_unsupported` gap) and the
/// emit alike; a statement it does not carry stays the gap its file's
/// program names.
pub(crate) fn carries_body_statement(lc: &LibraryClass, call: &Expr, app: &App) -> bool {
    if !is_body_statement(call) || name_is_class_eval_body(lc) || call.span.is_synthetic() {
        return false;
    }
    let replays = carried_railtie(lc, app) || replays_foreign_base(lc, app);
    // A Concern's own body statement runs once, on the module, as the
    // module loads (its `included do` is what runs on each includer);
    // the emitted module is a plain one its includers include.
    if !replays && !self_calls_answered(lc, call, app) && !singleton_answered_by_app(lc, call, app) {
        return false;
    }
    // The emitted module writes no `extend ActiveSupport::Concern`
    // (`modelled_elsewhere`), so it is no mixin the statement could
    // be reordered against.
    let written_mixin = |c: &Expr| statement_mixes_in(c) && !(lc.is_module && is_concern_extend(c));
    if statement_mixes_in(call)
        && (!lc.includes.is_empty() || lc.unknown_calls.iter().any(|c| !std::ptr::eq(c, call) && written_mixin(c)))
    {
        return false;
    }
    body_statement_place(lc, call, app).is_some()
}

/// Whether `call`, a body statement of `lc`'s `class << self`
/// (`expr::SINGLETON_BODY`), runs on a singleton class no runtime base
/// gives methods to: `lc` is a module, or a class whose superclasses are
/// the app's own classes up to a core class. What it calls is then the
/// singleton's Ruby methods or what the app's own code gives it, so the
/// tree runs it as Ruby does. activeresource 6.2.0's `ActiveResource::Base`
/// has `class << self; include ThreadsafeAttributes; threadsafe_attribute
/// :_headers, …`: the include's `included` hook extends the singleton with
/// the macro.
fn singleton_answered_by_app(lc: &LibraryClass, call: &Expr, app: &App) -> bool {
    if call.decisions & crate::expr::SINGLETON_BODY == 0 {
        return false;
    }
    if lc.is_module {
        return true;
    }
    let mut seen: Vec<&str> = vec![lc.name.0.as_str()];
    let mut parent = lc.parent.as_ref();
    while let Some(p) = parent {
        let name = p.0.as_str().trim_start_matches("::");
        if is_core_class_name(name) {
            return true;
        }
        if is_model_named(name, app) || seen.contains(&name) {
            return false;
        }
        let classes = library_classes_named(name, app);
        let Some(class) = classes.iter().find(|c| !c.is_module) else { return false };
        seen.push(name);
        parent = class.parent.as_ref();
    }
    true
}

/// Where a carried body statement is written: `Some(None)` among the
/// body's other replayed calls, before the defs (no def of this file is
/// above it in the source); `Some(Some(k))` just before `lc.methods[k]`
/// (`k == methods.len()`: after the last), the first def below it. `None`
/// when no place keeps the source's order: the same file's defs are not
/// in source order, or a def is above it and a constant, replayed call or
/// class-side initializer the body runs before the defs is below it (a
/// body statement below it is written after it, below the same def).
fn body_statement_place(lc: &LibraryClass, call: &Expr, app: &App) -> Option<Option<usize>> {
    let here = |span: Span| !span.is_synthetic() && span.file == call.span.file;
    let above = |span: Span| here(span) && span.start < call.span.start;
    let below = |span: Span| here(span) && span.start > call.span.start;
    // An alias copy carries its original's body span; it takes effect
    // where its `alias_method` stands (its name span too).
    let mut defs: Vec<(usize, Span)> = Vec::new();
    for (i, m) in lc.methods.iter().enumerate() {
        let copied_at: Vec<Span> = lc
            .method_aliases
            .iter()
            .filter(|a| a.name == m.name && a.receiver == m.receiver && here(a.span))
            .filter(|a| {
                if a.inherited {
                    !here(m.name_span)
                } else {
                    lc.methods.iter().any(|o| {
                        o.name == a.original && o.receiver == m.receiver && o.body.span == m.body.span
                    })
                }
            })
            .map(|a| a.span)
            .collect();
        match copied_at.as_slice() {
            [] if here(m.name_span) => defs.push((i, m.name_span)),
            [] => {}
            [at] => defs.push((i, *at)),
            _ => return None,
        }
    }
    // An `attr_accessor`'s reader and writer share its span: one place.
    if !defs.windows(2).all(|w| w[0].1.start <= w[1].1.start) {
        return None;
    }
    if !defs.iter().any(|(_, span)| above(*span)) {
        return Some(None);
    }
    // A constant the emitted body writes above the defs commutes with a
    // statement that only defines methods (`only_defines`) when its value
    // calls nothing on the class, which could reach those methods:
    // identity_client's `LEGACY_METHODS = LEGACY_ATTR_WRITERS + [...]`
    // below its `LEGACY_ATTR_WRITERS.each do |a| define_method(...) end`.
    let defines_only = only_defines(call);
    // A replayed call that runs the class's own code (`self.format =
    // :json` through the body's `def self.format=`) is a tail effect,
    // written after every def (`tail_effects`), so after the statement.
    let own = crate::ingest::library_class::OwnClassCalls::new(&lc.name, &lc.methods);
    // A call the body drops (`dropped_class_body_calls`, reported where
    // it stands) is written nowhere, so no order is kept against it:
    // paquito's `TYPES.freeze` below its `begin; require
    // "msgpack/bigint"; TYPES << { … }; rescue LoadError; end`.
    let dropped = dropped_class_body_calls(lc, app);
    // Below a deferred constant (written after the defs), the statement
    // is written after it with the replayed calls the source has below
    // it, in source order (`calls_after_deferred`), and the deferred
    // constants below follow: neither is written before it. paquito's
    // `TYPES.freeze` below its `begin … TYPES << { … } … end`, both
    // below the deferred `TYPES = [{ packer: method(:time_pack), … }]`.
    let (_, deferred, _) = partition_deferred_constants(lc, app);
    let after_deferred = !carried_railtie(lc, app) && deferred.iter().any(|&i| above(lc.constants[i].1.span));
    let later_effect = lc
        .constants
        .iter()
        .enumerate()
        .filter(|(i, _)| !(after_deferred && deferred.contains(i)))
        .map(|(_, c)| c)
        .filter(|(_, value)| !(defines_only && !calls_on_self(value)))
        .map(|(_, value)| value.span)
        .chain(
            lc.unknown_calls
                .iter()
                .filter(|_| !after_deferred)
                .filter(|c| {
                    !is_body_statement(c)
                        && !dropped.iter().any(|d| std::ptr::eq(*d, *c))
                        // Run on each includer, not where it stands: Core's
                        // MemcachedHelper registers on and includes its
                        // blocker above its `included do`.
                        && !carried_included_block(lc, c, app)
                        && !(defines_only && defines_apart(lc, call, c))
                        && !own.runs(c)
                        && !names_own_method_above(lc, c)
                        && !commutes_with_singleton_statement(call, c, app)
                })
                .map(|c| c.span),
        )
        .chain(lc.class_ivar_initializers.iter().map(|e| e.span))
        .any(below);
    if later_effect {
        return None;
    }
    Some(Some(defs.iter().find(|(_, span)| below(*span)).map(|(i, _)| *i).unwrap_or(lc.methods.len())))
}

/// Whether `later`, a replayed call the body writes before its defs,
/// leaves `statement`, a body statement of the class's `class << self`
/// (`expr::SINGLETON_BODY`) the source has above it, as Ruby leaves it
/// when run first: an instance-side `alias_method`, which the singleton
/// does not reach, or an `extend` of modules whose `self.extended` the
/// app does not define, whose methods the singleton's own (what the
/// statement defines) come before however the two are ordered.
/// activeresource 6.2.0's `ActiveResource::Base` ends its body with
/// `extend ActiveModel::Naming` and `alias_method
/// :respond_to_without_attributes?, :respond_to?` below its `class <<
/// self; threadsafe_attribute :_headers, …`.
fn commutes_with_singleton_statement(statement: &Expr, later: &Expr, app: &App) -> bool {
    if statement.decisions & crate::expr::SINGLETON_BODY == 0 {
        return false;
    }
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*later.node else { return false };
    match method.as_str() {
        "alias_method" => true,
        "extend" => !args.is_empty() && args.iter().all(|arg| {
            let ExprNode::Const { path } = &*arg.node else { return false };
            let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
            library_classes_named(name.trim_start_matches("::"), app).iter().all(|m| {
                !m.methods.iter().any(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == "extended")
            })
        }),
        _ => false,
    }
}

/// Whether all `call` does as the body loads is define methods: it
/// iterates (`each` and kin) and calls `define_method`, `alias_method`,
/// an `attr_*` or `delegate` on the class, building their arguments from literals,
/// locals, constants and interpolation. A `define_method` block is a
/// method body, run later. It mutates nothing another body statement or
/// constant reads.
fn only_defines(call: &Expr) -> bool {
    fn walk(e: &Expr) -> bool {
        match &*e.node {
            ExprNode::Send { recv, method, args, block, .. } => {
                let on_self = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
                let defines = on_self
                    && matches!(
                        method.as_str(),
                        "define_method" | "alias_method" | "attr_reader" | "attr_writer" | "attr_accessor" | "delegate"
                    );
                let iterates = recv.is_some()
                    && matches!(method.as_str(), "each" | "each_with_index" | "each_pair" | "each_key" | "each_value");
                let pure = recv.is_some() && matches!(method.as_str(), "to_s" | "to_sym" | "freeze");
                if !(defines || iterates || pure) {
                    return false;
                }
                recv.as_ref().is_none_or(walk)
                    && args.iter().all(walk)
                    && (method.as_str() == "define_method" || block.as_ref().is_none_or(walk))
            }
            ExprNode::Lit { .. } | ExprNode::Const { .. } | ExprNode::Var { .. } | ExprNode::SelfRef => true,
            _ => {
                let mut ok = matches!(
                    &*e.node,
                    ExprNode::Lambda { .. }
                        | ExprNode::Seq { .. }
                        | ExprNode::StringInterp { .. }
                        | ExprNode::Array { .. }
                        | ExprNode::Splat { .. }
                        | ExprNode::Hash { .. }
                );
                if ok {
                    e.node.for_each_child(&mut |child| ok = ok && walk(child));
                }
                ok
            }
        }
    }
    walk(call)
}

/// Whether `statement`, which only defines methods (`only_defines`),
/// and `later`, a replayed call that only defines methods too (`delegate`,
/// on the class or its singleton, or what `only_defines` admits), define
/// names known to be disjoint, so neither replaces the other's method and
/// their order does not matter: identity_client's `delegate(*LEGACY_METHODS,
/// to: :default_config)` below its `LEGACY_ATTR_WRITERS.each do |attr|
/// define_method("#{attr}=") … end`.
fn defines_apart(lc: &LibraryClass, statement: &Expr, later: &Expr) -> bool {
    let is_delegate = matches!(
        &*later.node,
        ExprNode::Send { recv, method, .. } if method.as_str() == "delegate"
            && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::Send { recv: None, method, args, .. }
                if method.as_str() == "singleton_class" && args.is_empty()))
    );
    if !is_delegate && !only_defines(later) {
        return false;
    }
    let (Some(ours), Some(theirs)) = (defined_names(lc, statement, &[]), defined_names(lc, later, &[])) else {
        return false;
    };
    ours.iter().all(|name| !theirs.contains(name))
}

/// The names of the methods `call` defines as the body loads, when each
/// is known: a `define_method`, `alias_method`, `attr_*` or `delegate`
/// whose names are literals, interpolations of a loop's variable, or a
/// constant of the class bound to a literal list (`literal_names`), and
/// an `each` over such a list.
fn defined_names(lc: &LibraryClass, call: &Expr, env: &[(Symbol, String)]) -> Option<Vec<String>> {
    match &*call.node {
        ExprNode::Send { recv, method, args, block, .. } => match method.as_str() {
            "define_method" | "alias_method" => Some(vec![name_value(args.first()?, env)?]),
            "attr_reader" | "attr_writer" | "attr_accessor" => {
                let mut names = Vec::new();
                for a in args {
                    let n = name_value(a, env)?;
                    if method.as_str() != "attr_writer" {
                        names.push(n.clone());
                    }
                    if method.as_str() != "attr_reader" {
                        names.push(format!("{n}="));
                    }
                }
                Some(names)
            }
            "delegate" => {
                let mut names = Vec::new();
                for a in args {
                    match &*a.node {
                        ExprNode::Hash { entries, .. } => {
                            // `prefix:` renames every forwarder.
                            let prefixed = entries.iter().any(|(k, _)| {
                                matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "prefix")
                            });
                            if prefixed {
                                return None;
                            }
                        }
                        ExprNode::Splat { value } => names.extend(literal_names(lc, value, 0)?),
                        _ => names.push(name_value(a, env)?),
                    }
                }
                Some(names)
            }
            "each" if args.is_empty() => {
                let list = literal_names(lc, recv.as_ref()?, 0)?;
                let ExprNode::Lambda { params, body, .. } = &*block.as_ref()?.node else { return None };
                let [param] = params.as_slice() else { return None };
                let mut names = Vec::new();
                for item in list {
                    let mut env = env.to_vec();
                    env.push((param.clone(), item));
                    names.extend(defined_names(lc, body, &env)?);
                }
                Some(names)
            }
            _ => None,
        },
        ExprNode::Seq { exprs } => {
            let mut names = Vec::new();
            for e in exprs {
                names.extend(defined_names(lc, e, env)?);
            }
            Some(names)
        }
        _ => None,
    }
}

/// A method name `e` evaluates to: a String or Symbol literal, or an
/// interpolation of literal text and loop variables bound in `env`.
fn name_value(e: &Expr, env: &[(Symbol, String)]) -> Option<String> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        ExprNode::Var { name, .. } => env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone()),
        ExprNode::StringInterp { parts } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    crate::expr::InterpPart::Text { value } => out.push_str(value),
                    crate::expr::InterpPart::Expr { expr } => out.push_str(&name_value(expr, env)?),
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The Strings and Symbols of a literal list: an Array literal of them,
/// a constant of `lc` bound to one, their `+`, or a `freeze` of one.
fn literal_names(lc: &LibraryClass, e: &Expr, depth: usize) -> Option<Vec<String>> {
    if depth > 8 {
        return None;
    }
    match &*e.node {
        ExprNode::Array { elements, .. } => elements.iter().map(|el| name_value(el, &[])).collect(),
        ExprNode::Const { path } => {
            let [name] = path.as_slice() else { return None };
            let mut bound = lc.constants.iter().filter(|(n, _)| n == name);
            let (_, value) = bound.next()?;
            if bound.next().is_some() {
                return None;
            }
            literal_names(lc, value, depth + 1)
        }
        ExprNode::Send { recv: Some(r), method, args, block: None, .. } => match (method.as_str(), args.as_slice()) {
            ("+", [other]) => {
                let mut names = literal_names(lc, r, depth + 1)?;
                names.extend(literal_names(lc, other, depth + 1)?);
                Some(names)
            }
            ("freeze", []) => literal_names(lc, r, depth + 1),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `value` calls anything on the class (no receiver, or `self`).
fn calls_on_self(value: &Expr) -> bool {
    let mut found = false;
    fn walk(e: &Expr, found: &mut bool) {
        if let ExprNode::Send { recv, .. } = &*e.node
            && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        {
            *found = true;
        }
        e.node.for_each_child(&mut |child| walk(child, found));
    }
    walk(value, &mut found);
    found
}

/// Whether `call` includes, extends or prepends: the emitted body writes
/// the class's own mixins at its top, so a statement mixing in is
/// carried only when nothing else in the body does.
fn statement_mixes_in(call: &Expr) -> bool {
    let mut found = false;
    fn walk(e: &Expr, found: &mut bool) {
        if let ExprNode::Send { method, .. } = &*e.node
            && matches!(method.as_str(), "include" | "extend" | "prepend")
        {
            *found = true;
        }
        e.node.for_each_child(&mut |child| walk(child, found));
    }
    walk(call, &mut found);
    found
}

/// What a class body statement may call on the class (no receiver, or
/// `self`) outside a block that runs as a method: what `Module`, `Class`
/// and `Kernel` give every class, so the call runs the same on any base.
const ANY_CLASS_SELF_CALLS: &[&str] = &[
    "define_method", "include", "extend", "prepend", "alias_method", "attr_reader", "attr_writer", "attr_accessor",
    // ActiveSupport's, which the Ruby family's runtime gives every
    // Module (runtime/spinel/module_delegate.rb).
    "delegate",
    "const_get", "const_set", "const_defined?", "constants", "remove_const", "private_constant", "public_constant",
    "instance_method", "public_instance_method", "instance_methods", "public_instance_methods",
    "private_instance_methods", "protected_instance_methods", "method_defined?", "public_method_defined?",
    "private_method_defined?", "protected_method_defined?", "remove_method", "undef_method", "ruby2_keywords",
    "name", "to_s", "inspect", "ancestors", "superclass", "singleton_class", "instance_variable_get",
    "instance_variable_set", "instance_variable_defined?", "respond_to?", "is_a?", "kind_of?", "frozen?",
    "raise", "fail", "warn", "puts", "print", "p", "format", "sprintf", "lambda", "proc", "loop",
    "Integer", "Float", "String", "Array", "Hash", "Rational", "Complex", "caller", "caller_locations", "freeze", "method",
];

/// Whether every call `call` makes on the class itself (no receiver, or
/// `self`) is one any class answers ([`ANY_CLASS_SELF_CALLS`]) or a
/// class method `lc` defines. A `define_method` block is a method body,
/// run later on an instance, so its calls are not the class's. A block
/// that rebinds `self` (`instance_eval`, `class_exec`, …) is not read.
fn self_calls_answered(lc: &LibraryClass, call: &Expr, app: &App) -> bool {
    // In `class << self` (`expr::SINGLETON_BODY`) `self` is the
    // singleton class, which the class's own class methods do not answer.
    let singleton = call.decisions & crate::expr::SINGLETON_BODY != 0;
    let own: Vec<&Symbol> = lc
        .methods
        .iter()
        .filter(|m| m.receiver == MethodReceiver::Class && !singleton)
        .map(|m| &m.name)
        .collect();
    fn walk(e: &Expr, own: &[&Symbol], app: &App, ok: &mut bool) {
        if !*ok {
            return;
        }
        if let ExprNode::Send { recv, method, args, block, .. } = &*e.node {
            let on_self = match recv {
                None => true,
                Some(r) => matches!(&*r.node, ExprNode::SelfRef),
            };
            if on_self && method.as_str() == "require" && block.is_none() && requires_library(args, app) {
                return;
            }
            if matches!(
                method.as_str(),
                "instance_eval" | "instance_exec" | "class_eval" | "class_exec" | "module_eval" | "module_exec"
            ) {
                *ok = false;
                return;
            }
            if on_self && !ANY_CLASS_SELF_CALLS.contains(&method.as_str()) && !own.contains(&method) {
                *ok = false;
                return;
            }
            if let Some(r) = recv {
                walk(r, own, app, ok);
            }
            for a in args {
                walk(a, own, app, ok);
            }
            if let Some(b) = block
                && !(on_self && method.as_str() == "define_method")
            {
                walk(b, own, app, ok);
            }
            return;
        }
        e.node.for_each_child(&mut |child| walk(child, own, app, ok));
    }
    let mut ok = true;
    walk(call, &own, app, &mut ok);
    ok
}

/// Whether `require`'s `args` are one literal feature Kernel#require
/// loads as it does natively: a library on the load path (the process's
/// gems and stdlib), not a file the app carries (whose emitted copy the
/// tree's own loader names) nor a Rails feature (the runtime ports answer
/// it). paquito's `Paquito::Types` body runs `begin; require
/// "msgpack/bigint"; TYPES << { … class: "Integer" … }; rescue LoadError;
/// end`.
fn requires_library(args: &[Expr], app: &App) -> bool {
    let [arg] = args else { return false };
    let ExprNode::Lit { value: crate::expr::Literal::Str { value: feature } } = &*arg.node else { return false };
    if feature.is_empty() || feature.starts_with('.') || feature.starts_with('/') || crate::project::is_framework_feature(feature) {
        return false;
    }
    let rb = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
    let (lib, nested) = (format!("lib/{rb}"), format!("/lib/{rb}"));
    !app.sources.iter().any(|f| {
        let path = relative_source_path(app, &f.path);
        path == lib || path.ends_with(&nested)
    })
}

/// Whether `lc` is a `class_eval` block's body (`ProgramItem::ClassEval`).
fn name_is_class_eval_body(lc: &LibraryClass) -> bool {
    lc.name.0.as_str() == crate::ingest::library_class::CLASS_EVAL_BODY
}

/// Whether a module body `extend`s ActiveSupport::Concern.
fn extends_concern(lc: &LibraryClass) -> bool {
    lc.unknown_calls.iter().any(|call| matches!(&*call.node,
        ExprNode::Send { recv: None, method, args, .. } if method.as_str() == "extend"
            && args.iter().any(|a| matches!(&*a.node, ExprNode::Const { path }
                if path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::") == "ActiveSupport::Concern"))))
}

/// The nested `ClassMethods` modules that ActiveSupport::Concern's
/// `append_features` extends a receiver with when it includes
/// `includes`, in the order Ruby extends them, as the ingest's
/// `splice_concern_class_methods_into_includers` finds them for a
/// library class (`App::class_methods_carrier_splices`): through
/// Concerns only, since a Concern defers its own includes to its
/// includer and a plain module performs them on itself, whose authored
/// `included` hook does any extending. asset_cloud's
/// `AssetCloud::Base.class_eval do include AssetCloud::Callbacks;
/// callback_methods :write, :delete end`: the macro is a method of the
/// Concern's `ClassMethods`.
fn concern_carrier_extends(includes: &[ClassId], app: &App) -> Vec<ClassId> {
    let is_concern = |name: &str| library_classes_named(name, app).iter().any(|c| c.is_module && extends_concern(c));
    let mut queue: std::collections::VecDeque<(String, bool)> =
        includes.iter().map(|i| (i.0.as_str().trim_start_matches("::").to_string(), true)).collect();
    let mut seen: crate::det::HashSet<String> = crate::det::HashSet::default();
    let mut out: Vec<ClassId> = Vec::new();
    while let Some((name, via)) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let concern = via && is_concern(&name);
        for c in library_classes_named(&name, app) {
            queue.extend(c.includes.iter().map(|n| (n.0.as_str().trim_start_matches("::").to_string(), concern)));
        }
        let carrier = format!("{name}::ClassMethods");
        let block = app.class_methods_blocks.contains_key(&ClassId(Symbol::from(name.as_str())));
        if concern && (block || library_classes_named(&carrier, app).iter().any(|c| c.is_module)) {
            out.push(ClassId(Symbol::from(carrier.as_str())));
        }
    }
    // Ruby runs a Concern's deferred includes before its own extend, and
    // `include A, B` includes B first; the last extend has precedence.
    out.reverse();
    out
}

/// For a model, the app modules its Concerns' `ClassMethods` carriers
/// include, per include of the model, in the order Ruby extends them.
///
/// ActiveSupport::Concern's `append_features` extends the includer with
/// the carrier, which brings the carrier's own includes to the class
/// side. A model takes the carrier's `def`s as class-side copies instead
/// (`ingest::app::splice_concern_class_methods_into_includers`), which
/// reach none of those: Core's ParamConditions::ClassMethods does
/// `include ApiConditions`, and its includers (Order, Theme, ...) raised
/// "There are no methods named (since_id_filter) found in ApiConditions"
/// from `param_conditions :since_id_filter`, whose `respond_to?` found no
/// ApiConditions method. So the model extends the carrier's included
/// modules right after the include, where `append_features` extends the
/// carrier. Not the carrier itself, which would put its methods below
/// the copies, so a copy's `super` would run the carrier's own `def` of
/// the same name; a copy's `super` reaches the included module, as the
/// carrier's does in Ruby. A carrier the model extends already
/// (`spliced_extends`) brings them itself. A module the carrier names
/// that is not one of the app's is left out: the copies never had it
/// either, and the tree may not define it.
fn carrier_included_modules<'a>(
    lc: &'a LibraryClass,
    spliced_extends: &[&ClassId],
    app: &App,
) -> Vec<(&'a ClassId, Vec<ClassId>)> {
    let mut extended: Vec<ClassId> = Vec::new();
    let mut out = Vec::new();
    for inc in &lc.includes {
        let mut modules: Vec<ClassId> = Vec::new();
        for carrier in concern_carrier_extends(std::slice::from_ref(inc), app) {
            if spliced_extends.iter().any(|s| s.0.as_str().trim_start_matches("::") == carrier.0.as_str()) {
                continue;
            }
            for carrier_lc in library_classes_named(carrier.0.as_str(), app).into_iter().filter(|c| c.is_module) {
                for written in &carrier_lc.includes {
                    let path: Vec<Symbol> =
                        written.0.as_str().trim_start_matches("::").split("::").map(Symbol::from).collect();
                    let Some(module) = resolve_app_module(carrier_lc, &path, app) else { continue };
                    let id = ClassId(Symbol::from(module.name.0.as_str().trim_start_matches("::")));
                    if !extended.contains(&id) {
                        extended.push(id.clone());
                        modules.push(id);
                    }
                }
            }
        }
        if !modules.is_empty() {
            out.push((inc, modules));
        }
    }
    out
}

/// Whether a module body uses ActiveSupport::Concern's hooks, which
/// `splice_concerns_into_models` applies to each includer.
/// `extend ActiveSupport::Concern`.
fn is_concern_extend(call: &Expr) -> bool {
    matches!(&*call.node, ExprNode::Send { recv: None, method, args, block: None, .. }
        if method.as_str() == "extend"
            && matches!(args.as_slice(), [only] if matches!(&*only.node, ExprNode::Const { path }
                if path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::") == "ActiveSupport::Concern")))
}

fn is_concern_body(lc: &LibraryClass) -> bool {
    lc.unknown_calls.iter().any(|call| match &*call.node {
        ExprNode::Send { recv: None, method, args, .. } => match method.as_str() {
            "included" | "class_methods" | "prepended" => true,
            "extend" => args.iter().any(|a| matches!(&*a.node, ExprNode::Const { path } if path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::") == "ActiveSupport::Concern")),
            _ => false,
        },
        _ => false,
    })
}

/// Whether `lc` includes a module of a gem the tree loads natively,
/// directly or through the app's own modules. An include written
/// relative to the module's namespace resolves lexically, innermost
/// first.
fn includes_loaded_gem_module<'a>(lc: &'a LibraryClass, app: &'a App, seen: &mut Vec<&'a str>) -> bool {
    if seen.contains(&lc.name.0.as_str()) {
        return false;
    }
    seen.push(lc.name.0.as_str());
    let scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    for inc in &lc.includes {
        let written = inc.0.as_str().trim_start_matches("::");
        let candidates: Vec<String> = if inc.0.as_str().starts_with("::") {
            vec![written.to_string()]
        } else {
            (0..=scope.len()).rev().map(|n| if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) }).collect()
        };
        for name in &candidates {
            let own: Vec<&LibraryClass> = library_classes_named(name, app).into_iter().filter(|c| c.is_module).collect();
            if !own.is_empty() {
                if own.iter().any(|m| includes_loaded_gem_module(m, app, seen)) {
                    return true;
                }
                break;
            }
            let id = ClassId(Symbol::from(name.as_str()));
            let gem_module = app.gem_boundary.loaded_reopen(&id).is_some_and(|g| g.is_module)
                || app.gem_boundary.loaded_source_reopen(&id).is_some_and(|(_, kind)| kind.is_module);
            if gem_module {
                return true;
            }
        }
    }
    false
}

/// The body calls of a plain module or class (no superclass, or a core
/// one) that only the app's own code answers, which Ruby runs on the
/// module or class itself:
/// - `extend M` of an app module and the bare calls after it to that
///   module's methods (`extend SymbolEnum` then `symbol_enum :state,
///   [:on, :off]`; Core's `extend LazyAttrAccessor` then
///   `lazy_attr_accessor :x` in `Shopify::Config`);
/// - `extend self` in a module. The ingest already gives its `def`s and
///   `attr_*`s their module-side form; the replayed `extend self` makes
///   the instance methods a replayed macro defines module methods too;
/// - `self.x = v` through the body's own class-side writer
///   (`attr_accessor :x` then `self.x = 1` under `extend self`), or
///   after an `extend` of an app module (a writer its macro defines).
///   One that runs the body's own code is written after the methods
///   (`OwnClassCalls`), keeping their order.
///
/// No runtime base answers these, so replaying them is what Ruby runs.
/// Anything else stays dropped and reported (`replays_foreign_class_body`).
///
/// A class whose superclass is an app class replays the class macros it
/// inherits instead (`inherited_class_macro_calls`). Either kind of class
/// also replays the class macros a loaded gem's mixin gives it
/// (`gem_mixin_macro_calls`). The result keeps the source order.
fn app_extension_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let own = if lc.parent.as_ref().is_some_and(|p| !is_core_class_name(p.0.as_str())) {
        inherited_class_macro_calls(lc, app)
    } else {
        plain_extension_calls(lc, app)
    };
    let gem = gem_mixin_macro_calls(lc, app);
    let carried = carried_class_macro_calls(lc, app);
    let mut runtime = runtime_mixin_macro_calls(lc, app);
    runtime.extend(model_callback_calls(lc, app));
    runtime.extend(sti_macro_calls(lc, app));
    if gem.is_empty() && carried.is_empty() && runtime.is_empty() {
        return own;
    }
    lc.unknown_calls
        .iter()
        .filter(|call| own.iter().chain(gem.iter()).chain(carried.iter()).chain(runtime.iter()).any(|c| std::ptr::eq(*c, *call)))
        .collect()
}

/// A lowered model's ActiveSupport::Callbacks class-body calls
/// (`model_to_library::callback_dsl_calls`: `define_callbacks
/// :revocation`, `set_callback`, and an app macro over them such as
/// Core's `after_revocation :notify`), which `model_class` carries with
/// the module included. ActiveRecord::Base includes the module natively;
/// the runtime port answers the calls, and one that calls the class's
/// own macro runs after its defs (`OwnClassCalls`), as Ruby defines the
/// macro before the body reaches it.
///
/// So are an STI subclass's (a library class whose superclass is a
/// model, `sti_subclass_callbacks`): Core's
/// `Credential::Shop::ShopPayBillingAgreement < Shop` calls
/// `after_revocation :disable_in_external_systems_async`, the macro its
/// model ancestor's concern defines.
fn model_callback_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    use crate::lower::model_to_library::{callback_calls, callback_dsl_calls};
    if let Some(model) = app.models.iter().find(|m| m.name == lc.name) {
        let calls = callback_dsl_calls(model, &app.models);
        return lc.unknown_calls.iter().filter(|call| calls.iter().any(|c| c.span == call.span)).collect();
    }
    if !lc.parent.as_ref().is_some_and(|p| app.models.iter().any(|m| &m.name == p)) {
        return Vec::new();
    }
    let own: Vec<&crate::dialect::MethodDef> =
        lc.methods.iter().filter(|m| m.receiver == MethodReceiver::Class).collect();
    callback_calls(lc.unknown_calls.iter(), &own, lc.parent.as_ref(), &app.models)
}

/// An STI subclass's body calls of app class macros (a model ancestor's,
/// its own, or an extended app module's) and `extend`s of app modules
/// that pass the reach gate (`model_to_library::macro_replay`): what Ruby
/// runs at load is something the emitted class answers.
fn sti_macro_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    crate::lower::model_to_library::macro_replay::sti_macro_verdicts(lc, app)
        .into_iter()
        .filter(|(_, v)| *v == crate::lower::model_to_library::macro_replay::MacroVerdict::Replay)
        .map(|(e, _)| e)
        .collect()
}

/// The class-body calls the Ruby-family emit of `lc` writes when it
/// replays only what it models (not a railtie's or a foreign base's whole
/// body): sorbet's `mixes_in_class_methods`, lowered by the ingest
/// (`sorbet_check::mixes_in_class_methods`), which every body runs
/// whatever its base, the `app_extension_calls` and the app prepends, in
/// source order. The rest is dropped (`report_dropped_class_body_call`);
/// `model_to_library::diagnose_unclaimed_model_dsl` refuses what an STI
/// subclass drops.
pub(crate) fn modelled_class_body_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let extensions = app_extension_calls(lc, app);
    let prepends = app_prepend_calls(lc, app);
    // An STI subclass's class macro call its model claims runs as
    // written: one that retypes columns the model compiled as retypable
    // declares the types on the subclass, one that sets the model's
    // query constraints sets the same list
    // (`analyze::retypable_attributes::claimed_body_calls`).
    let parent = lc.parent.as_ref().and_then(|p| app.models.iter().find(|m| &m.name == p));
    let claimed = |c: &Expr| parent.is_some_and(|m| m.claimed_body_calls.contains(&c.span));
    let gem_effects = gem_source_body_effects(lc, app);
    let validations = active_model_validation_calls(lc, app);
    lc.unknown_calls
        .iter()
        .filter(|c| {
            aliases_ancestor_method(lc, c)
                || crate::ingest::inherited_alias::aliases_core_method(&app.library_classes, lc, c)
                || crate::ingest::sorbet_check::is_mixes_in(c)
                || extensions.iter().any(|e| std::ptr::eq(*e, *c))
                || prepends.iter().any(|(p, _)| std::ptr::eq(*p, *c))
                || gem_effects.iter().any(|e| std::ptr::eq(*e, *c))
                || validations.iter().any(|e| std::ptr::eq(*e, *c))
                || claimed(c)
                || crate::lower::typed_store::replays_on_sti_subclass(lc, c, app)
        })
        .collect()
}

/// The class-body validation calls (`validates`, `validate`,
/// `validates_with`, `validates_each`, the `validates_*_of` helpers, a
/// `with_options` of them, and `before_validation`/`after_validation`
/// where it includes ActiveModel::Validations::Callbacks) of a library
/// class that includes ActiveModel::Validations, itself or through a
/// library superclass or an included app module, on a tree that carries
/// the ActiveModel::Validations port: the PostgreSQL Ruby-family tree
/// (`project::active_model_ports_carried`, runtime/spinel/
/// active_model_validations.rb). Replayed as written, they declare the
/// rules on the class as activemodel's would, and `valid?` runs them.
/// api_versioning's ApiVersionRecord and ApiChangeRecord (FrozenRecord
/// classes, whose superclass no lowering models) are such classes.
///
/// Not a class below a model (an STI subclass, whose validations
/// `lower::sti_subclass_validations` handles), and not a module body:
/// a module's `validates` runs on the module itself, which the module
/// does not answer.
fn active_model_validation_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let callback_call = |call: &Expr| {
        matches!(&*call.node, ExprNode::Send { recv: None, method, .. }
            if matches!(method.as_str(), "before_validation" | "after_validation"))
    };
    // A body with no candidate call answers none, whatever its
    // ancestry: decided before the ancestry walks (a scan of the
    // library classes per parent), asked for every library class.
    if lc.is_module
        || !lc.unknown_calls.iter().any(|call| {
            crate::lower::active_model_model::validation_call(call).is_some() || callback_call(call)
        })
        || app.primary_database_adapter() != Some(&crate::database::DatabaseAdapter::Postgresql)
        || crate::lower::active_model_model::below_model(lc, app)
        || !includes_active_model_validations(lc, app)
    {
        return Vec::new();
    }
    let callbacks = includes_module(lc, app, "ActiveModel::Validations::Callbacks");
    lc.unknown_calls
        .iter()
        .filter(|call| {
            crate::lower::active_model_model::validation_call(call).is_some() || (callbacks && callback_call(call))
        })
        .collect()
}

/// Whether `lc` includes ActiveModel::Validations: in its own body, a
/// library superclass's, or an app module either includes; directly or
/// through ActiveModel::API or ActiveModel::Model, which include it
/// (runtime/spinel/cruby/active_model_api.rb).
pub(crate) fn includes_active_model_validations(lc: &LibraryClass, app: &App) -> bool {
    ["ActiveModel::Validations", "ActiveModel::API", "ActiveModel::Model"]
        .iter()
        .any(|module| includes_module(lc, app, module))
}

pub(crate) fn includes_module(lc: &LibraryClass, app: &App, module: &str) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut queue: Vec<&LibraryClass> = vec![lc];
    while let Some(c) = queue.pop() {
        if seen.iter().any(|s| s == c.name.0.as_str()) {
            continue;
        }
        seen.push(c.name.0.as_str().to_string());
        for include in &c.includes {
            let name = include.0.as_str().trim_start_matches("::");
            if name == module {
                return true;
            }
            queue.extend(library_classes_named(name, app).into_iter().filter(|m| m.is_module));
        }
        if !c.is_module {
            if let Some(parent) = &c.parent {
                let name = parent.0.as_str().trim_start_matches("::");
                queue.extend(library_classes_named(name, app).into_iter().filter(|p| !p.is_module));
            }
        }
    }
    false
}

/// A captured `alias_method :to, :from` (bare, or on `singleton_class`
/// for one inside `class << self`) whose class defines `from` on that
/// side only below it, in the same file: Ruby aliases the method `from`
/// names where the alias runs, which is an ancestor's (Class#new for
/// frozen_record 0.27.4's `alias_method :load, :new` above its own `def
/// new`, inside `class << self`). Replayed as written, above the defs
/// (where the replayed calls go), the native alias finds the same one;
/// dropped, `load(record)` reached Kernel#load. Ingest leaves exactly
/// these captured (`ingest::inherited_alias::resolve` copies the ones
/// whose def runs first).
fn aliases_ancestor_method(lc: &LibraryClass, call: &Expr) -> bool {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { return false };
    if method.as_str() != "alias_method" || call.span.is_synthetic() {
        return false;
    }
    let receiver = match recv {
        None => MethodReceiver::Instance,
        Some(r) => match &*r.node {
            ExprNode::Send { recv: None, method, args, block: None, .. }
                if method.as_str() == "singleton_class" && args.is_empty() =>
            {
                MethodReceiver::Class
            }
            _ => return false,
        },
    };
    let symbol = |e: &Expr| match &*e.node {
        ExprNode::Lit { value: crate::expr::Literal::Sym { value } } => Some(value.clone()),
        _ => None,
    };
    let [to, from] = args.as_slice() else { return false };
    let (Some(_), Some(from)) = (symbol(to), symbol(from)) else { return false };
    let mut own = lc.methods.iter().filter(|m| m.receiver == receiver && m.name == from).peekable();
    own.peek().is_some()
        && own.all(|m| !m.name_span.is_synthetic() && m.name_span.file == call.span.file && m.name_span.start > call.span.start)
}

/// The body calls that name a class method a Ruby-family runtime port
/// module the class (or an app superclass) includes gives it
/// (`runtime_reads::mixed_in_class_methods`): Core's Plus::Plan::Change
/// includes ActiveSupport::Callbacks and calls `define_callbacks
/// :perform`; dropped, `run_callbacks(:perform)` found no chain. The
/// port's `append_features` extends the includer, as Concern does, and the
/// tree carries the port where the app names the module
/// (`project::carries_named`); a tree without it refuses the include
/// (`report_unsupported_runtime_reads`). Bare and `self.` calls only; an
/// app module of the same name hides the port's.
///
/// So is a bare `extend M, ...` whose every module a gated port declares
/// (Core's `extend ActiveSupport::DescendantsTracker`): the port answers
/// what the class then asks of itself, and the tree that carries the port
/// loads it before any app file (runtime/gem_facades.rb).
fn runtime_mixin_macro_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    // A gated port module, or one a gem runtime port declares
    // (database_documentation's `ModelMetadata::Metadata`).
    let port_module = |written: &str| -> bool {
        let name = written.trim_start_matches("::");
        if app.library_classes.iter().any(|own| own.name.0.as_str() == name)
            || app.models.iter().any(|own| own.name.0.as_str() == name)
        {
            return false;
        }
        crate::analyze::runtime_reads::ruby_family_runtime_names().contains_key(name)
            || crate::project::gem_port_declaring(name).is_some()
    };
    let provided = |c: &LibraryClass| -> Vec<&'static str> {
        c.includes
            .iter()
            .filter(|inc| port_module(inc.0.as_str()))
            .filter_map(|inc| crate::analyze::runtime_reads::mixed_in_class_methods(inc.0.as_str().trim_start_matches("::")))
            .flatten()
            .map(String::as_str)
            .collect()
    };
    let mut methods = provided(lc);
    for ancestor in app_library_ancestors(lc, app) {
        methods.extend(provided(ancestor));
    }
    let port_extend = |call: &Expr| -> bool {
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*call.node else { return false };
        method.as_str() == "extend"
            && !args.is_empty()
            && args.iter().all(|a| match &*a.node {
                ExprNode::Const { path } => {
                    let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                    port_module(&name)
                        && crate::analyze::runtime_reads::ruby_family_runtime_names()
                            .contains_key(name.trim_start_matches("::"))
                }
                _ => false,
            })
    };
    // A module boot includes into ActiveRecord::Base
    // (`App::framework_base_includes`) is included at run time, after the
    // app's own concerns were spliced into their includers at ingest, so
    // its `extend ActiveSupport::Concern` is what gives the base its
    // `ClassMethods` (measured 3.2.1's `Measured::Rails::ActiveRecord::
    // Weight`). The tree's Concern port (runtime/active_support_concern.rb)
    // runs it.
    // The same holds for one a lazy-load hook includes into another base
    // (`App::load_hook_includes`: `ActiveSupport::TestCase`).
    let base_included_concern =
        lc.is_module && (app.framework_base_includes.contains(&lc.name) || app.load_hook_includes.contains(&lc.name));
    lc.unknown_calls
        .iter()
        .filter(|call| {
            if port_extend(call) || (base_included_concern && is_concern_extend(call)) {
                return true;
            }
            let ExprNode::Send { recv, method, .. } = &*call.node else { return false };
            recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) && methods.contains(&method.as_str())
        })
        .collect()
}

/// The body calls that name a class method an app module the class
/// includes gives it through its `ClassMethods` carrier
/// (`App::class_methods_carrier_splices`: ActiveSupport::Concern's
/// `append_features`, or an authored `included` hook's
/// `base.extend(ClassMethods)`). asset_cloud 2.7.3's `AssetExtension`
/// includes the Concern `AssetCloud::Callbacks` and then calls
/// `callback_methods :store, :delete, :validate`, which prepends wrappers
/// around those methods; dropped, the class had no `validate` and no
/// callbacks. The emitted class extends the carrier (or the include's
/// hook does), so replaying the call is what Ruby runs. Bare and `self.`
/// calls only. One that names a method of the class by symbol runs after
/// the methods (`names_own_method`), as the source had it below them.
fn carried_class_macro_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let Some(splices) = app.class_methods_carrier_splices.get(&lc.name) else { return Vec::new() };
    lc.unknown_calls
        .iter()
        .filter(|call| {
            let ExprNode::Send { recv, method, .. } = &*call.node else { return false };
            recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                && splices.iter().any(|splice| splice.methods.contains(method))
        })
        .collect()
}

/// Whether `call` names one of `lc`'s methods by a symbol argument
/// (`callback_methods :store`): a macro that wraps or reads the def, so
/// it runs after it.
fn names_own_method(lc: &LibraryClass, call: &Expr) -> bool {
    let ExprNode::Send { args, .. } = &*call.node else { return false };
    args.iter().any(|arg| {
        matches!(&*arg.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
            if lc.methods.iter().any(|m| m.name == *value))
    })
}

/// Whether `call` names by a symbol argument one of `lc`'s defs the
/// source writes above it in the same file: a macro that reads or wraps
/// the def as the body runs, so it runs after the defs. ActsAsTheme's
/// `def generate_unique_cityhash` and then `statsd_measure
/// :generate_unique_cityhash, ...` (statsd-instrument raises "could not
/// find method" for a method not defined yet).
fn names_own_method_above(lc: &LibraryClass, call: &Expr) -> bool {
    let ExprNode::Send { args, .. } = &*call.node else { return false };
    if call.span.is_synthetic() {
        return false;
    }
    args.iter().any(|arg| {
        matches!(&*arg.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } }
            if lc.methods.iter().any(|m| m.name == *value
                && !m.name_span.is_synthetic()
                && m.name_span.file == call.span.file
                && m.name_span.start < call.span.start))
    })
}

/// `lc`'s body `prepend M, ...` calls whose every module is an app
/// module, each with those modules' names. Native Ruby puts each `M`
/// ahead of the class in its ancestors, so `M`'s methods run around the
/// class's own and reach them through `super`; dropped, the wrapper
/// silently never ran. The written path resolves the way Ruby reads it:
/// a rooted `::M` from the top level, a relative one from `lc`'s
/// namespace outward. A prepend naming anything else stays a reported
/// residue.
fn app_prepend_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<(&'a Expr, Vec<ClassId>)> {
    let module_named = |path: &[Symbol]| -> Option<ClassId> {
        let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
        let is_module = |name: &str| app.library_classes.iter().any(|c| c.is_module && c.name.0.as_str() == name);
        if let Some(top) = written.strip_prefix("::") {
            return is_module(top).then(|| ClassId(Symbol::from(top)));
        }
        let mut scope = Some(lc.name.0.as_str());
        while let Some(prefix) = scope {
            let candidate = format!("{prefix}::{written}");
            if is_module(&candidate) {
                return Some(ClassId(Symbol::from(candidate)));
            }
            scope = prefix.rsplit_once("::").map(|(prefix, _)| prefix);
        }
        is_module(&written).then(|| ClassId(Symbol::from(written)))
    };
    lc.unknown_calls
        .iter()
        .filter_map(|call| {
            let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { return None };
            if method.as_str() != "prepend" || args.is_empty() {
                return None;
            }
            if !recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) {
                return None;
            }
            let modules: Option<Vec<ClassId>> = args
                .iter()
                .map(|a| match &*a.node {
                    ExprNode::Const { path } => module_named(path),
                    _ => None,
                })
                .collect();
            modules.map(|m| (call, m))
        })
        .collect()
}

/// The `app_extension_calls` of a plain module or class.
fn plain_extension_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let own_class_side = |name: &str| {
        lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == name)
    };
    let app_module = |path: &[Symbol]| resolve_app_module(lc, path, app);
    let mut extended: Vec<&LibraryClass> = Vec::new();
    // The Concerns whose `class_methods do` module is extended.
    let mut extended_blocks: Vec<&LibraryClass> = Vec::new();
    let mut out = Vec::new();
    for call in &lc.unknown_calls {
        // `self.x = v` through the body's own class-side writer, or after
        // an `extend` of an app module, whose methods or macros
        // (`lazy_attr_accessor :x`) may define it. A plain body has no
        // other base to answer it, so native Ruby runs exactly this.
        let ExprNode::Send { recv, method, args, .. } = &*call.node else { continue };
        let self_write = method.as_str().ends_with('=')
            && recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::SelfRef));
        if self_write && (own_class_side(method.as_str()) || !extended.is_empty()) {
            out.push(call);
            continue;
        }
        if recv.is_some() {
            continue;
        }
        if lc.is_module
            && method.as_str() == "extend"
            && matches!(args.as_slice(), [only] if matches!(&*only.node, ExprNode::SelfRef))
        {
            out.push(call);
            continue;
        }
        if method.as_str() == "extend" {
            let modules: Option<Vec<&LibraryClass>> = args
                .iter()
                .map(|a| match &*a.node {
                    ExprNode::Const { path } => app_module(path),
                    _ => None,
                })
                .collect();
            if let Some(modules) = modules.filter(|m| !m.is_empty()) {
                extended.extend(modules);
                out.push(call);
                continue;
            }
            let blocks: Option<Vec<&LibraryClass>> = args
                .iter()
                .map(|a| match &*a.node {
                    ExprNode::Const { path } => resolve_class_methods_block(lc, path, app),
                    _ => None,
                })
                .collect();
            if let Some(owners) = blocks.filter(|m| !m.is_empty()) {
                extended_blocks.extend(owners);
                out.push(call);
            }
            continue;
        }
        let defines = |c: &LibraryClass| {
            c.methods.iter().any(|m| m.name == *method && m.receiver == MethodReceiver::Instance)
        };
        let block_defines = |c: &LibraryClass| {
            let spans = &app.class_methods_blocks[&c.name];
            c.methods.iter().any(|m| m.name == *method && m.receiver == MethodReceiver::Class && spans.contains(&m.name_span))
        };
        if extended.iter().any(|m| defines(m)) || extended_blocks.iter().any(|m| block_defines(m)) {
            out.push(call);
        }
    }
    out
}

/// The body calls of a class whose superclass is an app library class
/// that name a class-side method one of its app ancestors defines
/// (`class TransactionInput < Component::Schema` then `attribute :id,
/// Integer`, where `Component::Schema` has `class << self; def
/// attribute(name, type, ...)`). Ruby runs them on the subclass through
/// that inherited method; the emitted ancestor defines it, so replaying
/// the call is what Ruby runs. Bare calls and `self.` calls only; a
/// chain that reaches a model or a non-app class stops there.
///
/// An ancestor's singleton methods include what an `extend` of an app
/// module gives it (Core's `CustomerFoundations::ComplexStruct::Base`
/// does `extend(ClassMethods)`, and each struct then calls `attribute
/// :value, String`, whose `class_eval` heredoc defines the reader). Such
/// an `extend` in the class itself is replayed too, ahead of the calls
/// it answers.
fn inherited_class_macro_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let ancestors = app_library_ancestors(lc, app);
    if ancestors.is_empty() {
        return Vec::new();
    }
    let extended: Vec<&LibraryClass> = std::iter::once(lc)
        .chain(ancestors.iter().copied())
        .flat_map(|c| extended_app_modules(c, app))
        .collect();
    let inherited = |name: &str| {
        ancestors.iter().any(|a| {
            a.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == name)
        }) || extended.iter().any(|m| module_defines(m, name, app, &mut Vec::new()))
    };
    lc.unknown_calls
        .iter()
        .filter(|call| {
            let ExprNode::Send { recv, method, .. } = &*call.node else { return false };
            let on_self = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
            on_self && (inherited(method.as_str()) || app_module_extend(lc, call, app).is_some())
        })
        .collect()
}

/// `M` for a bare `extend M` naming the app's own module, as Ruby
/// resolves the constant from `lc`'s namespace outwards (`extend
/// ClassMethods` inside `module Cs; class Base` is `Cs::ClassMethods`).
/// A rooted `extend ::A::B` (its path's empty first segment) is the
/// top-level `A::B` only: Core's CompanyMetafieldOwner is `class << self;
/// include ::Metafields::Owners::AbstractOwner`, and joined with its root
/// that named no module, so the `extend` was dropped from the emit.
fn resolve_app_module<'a>(lc: &LibraryClass, path: &[Symbol], app: &'a App) -> Option<&'a LibraryClass> {
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let written = path.iter().skip(usize::from(rooted)).map(|s| s.as_str()).collect::<Vec<_>>().join("::");
    let scope: Vec<&str> = if rooted { Vec::new() } else { lc.name.0.as_str().split("::").collect() };
    (0..=scope.len()).rev().find_map(|n| {
        let name = if n == 0 { written.clone() } else { format!("{}::{written}", scope[..n].join("::")) };
        library_classes_named(&name, app).into_iter().find(|c| c.is_module && c.origin.is_none())
    })
}

/// The Concern whose `class_methods do` defines the `ClassMethods` that
/// `path` names from `lc`'s namespace (`App::class_methods_blocks`):
/// Core's `extend FixtureFactory::Registry::ClassMethods`.
fn resolve_class_methods_block<'a>(lc: &LibraryClass, path: &[Symbol], app: &'a App) -> Option<&'a LibraryClass> {
    let (last, owner) = path.split_last()?;
    if last.as_str() != "ClassMethods" || owner.is_empty() {
        return None;
    }
    resolve_app_module(lc, owner, app).filter(|m| app.class_methods_blocks.contains_key(&m.name))
}

/// The modules of a bare `extend M, …` whose every argument is an app
/// module (`resolve_app_module`), or `None`.
fn app_module_extend<'a>(lc: &LibraryClass, call: &Expr, app: &'a App) -> Option<Vec<&'a LibraryClass>> {
    let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { return None };
    if method.as_str() != "extend" || args.is_empty() {
        return None;
    }
    args.iter()
        .map(|a| match &*a.node {
            ExprNode::Const { path } => resolve_app_module(lc, path, app),
            _ => None,
        })
        .collect()
}

/// The app modules `lc`'s body extends itself with.
fn extended_app_modules<'a>(lc: &LibraryClass, app: &'a App) -> Vec<&'a LibraryClass> {
    lc.unknown_calls.iter().filter_map(|call| app_module_extend(lc, call, app)).flatten().collect()
}

/// Whether app module `m`, or an app module it includes, defines the
/// instance method `name`.
fn module_defines<'a>(m: &'a LibraryClass, name: &str, app: &'a App, seen: &mut Vec<&'a str>) -> bool {
    if seen.contains(&m.name.0.as_str()) {
        return false;
    }
    seen.push(m.name.0.as_str());
    m.methods.iter().any(|d| d.receiver == MethodReceiver::Instance && d.name.as_str() == name)
        || m.includes.iter().any(|inc| {
            library_classes_named(inc.0.as_str().trim_start_matches("::"), app)
                .into_iter()
                .filter(|c| c.is_module)
                .any(|c| module_defines(c, name, app, seen))
        })
}

/// The app library classes `lc` inherits from, nearest first: its
/// superclass chain while each link is an app library class
/// (`app_library_superclass`); a model or a non-app class ends it.
fn app_library_ancestors<'a>(lc: &'a LibraryClass, app: &'a App) -> Vec<&'a LibraryClass> {
    let mut ancestors: Vec<&LibraryClass> = Vec::new();
    let mut at = lc;
    while let Some(parent) = at.parent.as_ref() {
        let Some(next) = app_library_superclass(at, &written_parent(at, parent, app), app) else { break };
        if ancestors.iter().any(|a| std::ptr::eq(*a, next)) || std::ptr::eq(next, lc) {
            break;
        }
        ancestors.push(next);
        at = next;
    }
    ancestors
}

/// `lc`'s superclass `parent` as the source spells it: with the leading
/// `::` when the source writes it from the top level (`class Track <
/// ::Operation`, read back from `parent_span`). Ingest keeps a superclass
/// as its path segments, without the `::`, yet the two spellings name
/// different classes: inside `module Delivery`, `::Operation` is the
/// top-level class and `Operation` is `Delivery::Operation` once that
/// exists. Core's delivery operations subclass both.
///
/// A file-level `A::B = Class.new(Parent)` (the ingest's
/// `file_class_new_path_write`, whose parent span is the `Class.new`
/// argument) is rooted too: the write runs at the top level, so Ruby
/// reads `Parent` there, while the emitted header sits inside `module A`,
/// where a bare `Parent` would find `A::Parent` first.
fn written_parent(lc: &LibraryClass, parent: &ClassId, app: &App) -> String {
    let text = parent.0.as_str();
    let span = lc.parent_span;
    let source = (!span.is_synthetic())
        .then(|| (span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)))
        .flatten();
    let written = source.and_then(|file| file.text.get(span.start as usize..span.end as usize));
    let rooted_as_written = written
        .is_some_and(|written| written.trim_start().starts_with("::") && written.trim_start().trim_start_matches("::") == text);
    let class_new_argument = lc.name.0.as_str().contains("::")
        && source
            .and_then(|file| file.text.get(..span.start as usize))
            .is_some_and(|before| before.trim_end().ends_with("Class.new("));
    if !text.starts_with("::") && (rooted_as_written || class_new_argument) {
        return format!("::{text}");
    }
    if !text.starts_with("::") {
        if let Some(target) = source.and_then(|file| compact_header_parent(lc, text, span, &file.text, &file.path, app)) {
            return target;
        }
    }
    text.to_string()
}

/// The superclass of a class whose emitted nesting is not its source's,
/// rooted at the class Ruby reads it as. Ruby evaluates the superclass in
/// the lexical nesting at the `class` keyword, and a compact path does
/// not enter its prefix: a top-level `class Errs::Unavailable < Fallback`
/// has none, and in `module Errs::Api; class Client < Base` it is
/// `Errs::Api` alone. The emitted header sits inside one body per
/// segment (`module Errs` around `class Unavailable`), where a bare
/// `Fallback` finds `Errs::Fallback` first. So the written name is
/// resolved against the source's nesting (`superclass_nesting`),
/// innermost first, then the top level: an app constant found there is
/// the target. `None` where the two nestings bind the name alike.
fn compact_header_parent(lc: &LibraryClass, text: &str, span: crate::span::Span, source: &str, path: &str, app: &App) -> Option<String> {
    if !lc.name.0.as_str().contains("::") {
        return None;
    }
    let nesting = superclass_nesting(span, source, path, app)?;
    let segments: Vec<&str> = lc.name.0.as_str().split("::").collect();
    let emitted: Vec<String> = (1..segments.len()).rev().map(|i| segments[..i].join("::")).collect();
    if nesting == emitted {
        return None;
    }
    let paths = app_constant_paths(app);
    let first = text.split("::").next().unwrap_or(text);
    // The app constant each nesting binds the name's head to, else the
    // top level's name.
    let binds = |scopes: &[String]| -> Option<String> {
        scopes.iter().find(|scope| paths.contains(&format!("{scope}::{first}"))).map(|scope| format!("{scope}::{text}"))
    };
    // A head no enclosing body of the source defines as an app constant
    // is the top level's (Ruby then searches the innermost body's
    // ancestors and Object; an app constant only the emitted nesting
    // adds is in neither).
    let native = binds(&nesting).unwrap_or_else(|| text.to_string());
    let emitted = binds(&emitted).unwrap_or_else(|| text.to_string());
    (native != emitted).then(|| format!("::{native}"))
}

/// The lexical nesting at the `class` keyword whose superclass starts at
/// `span` (`ingest::util::superclass_nestings`), each file parsed once per
/// installed `NameIndex`.
fn superclass_nesting(span: crate::span::Span, source: &str, path: &str, app: &App) -> Option<Vec<String>> {
    let parse = || std::rc::Rc::new(crate::ingest::util::superclass_nestings(source.as_bytes(), path));
    let nestings = match NameIndex::current(app) {
        Some(index) => index.superclass_nestings.borrow_mut().entry(span.file.0).or_insert_with(parse).clone(),
        None => parse(),
    };
    nestings.iter().find(|(start, _)| *start == span.start as usize).map(|(_, nesting)| nesting.clone())
}

/// The body calls that name a class-side method a loaded gem's module
/// gives the class, which Ruby runs on the class through that module:
/// - after `include G`, where `G`'s RBI says `mixes_in_class_methods M`,
///   or, with no RBI, its installed source's `included(base)` hook runs
///   `base.extend(M)` (Core's `include SmartProperties` then `property!
///   :name, accepts: String`), the instance methods of `M`
///   (`GemBoundary::loaded_mixed_in_class_methods`);
/// - after `extend G`, `G`'s instance methods, and the `extend` itself.
///
/// `G` is a module of a gem the emitted tree loads, outside Ruby's core
/// and standard library (`GemBoundary::is_loaded_gem_module`), named as Ruby
/// resolves it from the class's namespace outwards; an app constant of
/// that name hides it. The gem itself runs the hook and defines the
/// method, so replaying the call is what Ruby runs. An app superclass's
/// own includes and extends count too: the singleton methods they give
/// it are inherited.
///
/// Bare and `self.` calls only, after the `extend` that provides them;
/// an `include` carries no position (`LibraryClass::includes`) and is
/// written at the top of the emitted body, ahead of every replayed call.
/// Anything else stays dropped and reported.
fn gem_mixin_macro_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    let boundary = &app.gem_boundary;
    if boundary.is_empty() && boundary.source_constants.mixins.is_empty() {
        return Vec::new();
    }
    let included = |c: &LibraryClass| -> BTreeSet<Symbol> {
        c.includes
            .iter()
            .filter_map(|inc| loaded_gem_module(c, inc.0.as_str(), app))
            .flat_map(|g| boundary.loaded_mixed_in_class_methods(&g))
            .collect()
    };
    // `extend G, H` of loaded gem modules only: the modules, else `None`.
    let gem_extend = |c: &LibraryClass, call: &Expr| -> Option<Vec<ClassId>> {
        let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { return None };
        if method.as_str() != "extend" || args.is_empty() {
            return None;
        }
        args.iter()
            .map(|a| match &*a.node {
                ExprNode::Const { path } => {
                    let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                    loaded_gem_module(c, &written, app)
                }
                _ => None,
            })
            .collect()
    };
    let mut provided = included(lc);
    for ancestor in app_library_ancestors(lc, app) {
        provided.extend(included(ancestor));
        for call in &ancestor.unknown_calls {
            for g in gem_extend(ancestor, call).into_iter().flatten() {
                provided.extend(boundary.loaded_instance_surface(&g));
            }
        }
    }
    let mut out = Vec::new();
    for call in &lc.unknown_calls {
        if let Some(modules) = gem_extend(lc, call) {
            for g in &modules {
                provided.extend(boundary.loaded_instance_surface(g));
            }
            out.push(call);
            continue;
        }
        let ExprNode::Send { recv, method, .. } = &*call.node else { continue };
        let on_self = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
        if on_self && provided.contains(method) {
            out.push(call);
        }
    }
    out
}

/// The module of a loaded gem (`GemBoundary::is_loaded_gem_module`) that the
/// constant `written` in `lc`'s body names, resolved as Ruby does from
/// `lc`'s namespace outwards: the first enclosing scope that has the
/// name decides, so an app class or module of that name (one that is
/// not itself a reopen of the gem's) hides the gem's.
fn loaded_gem_module(lc: &LibraryClass, written: &str, app: &App) -> Option<ClassId> {
    let rooted = written.starts_with("::");
    let written = written.trim_start_matches("::");
    let mut scope: Vec<&str> = if rooted { Vec::new() } else { lc.name.0.as_str().split("::").collect() };
    loop {
        let candidate =
            if scope.is_empty() { written.to_string() } else { format!("{}::{written}", scope.join("::")) };
        let id = ClassId(Symbol::from(candidate.as_str()));
        let boundary = &app.gem_boundary;
        if boundary.loaded_reopen(&id).is_some() || boundary.loaded_source_reopen(&id).is_some() {
            return boundary.is_loaded_gem_module(&id).then_some(id);
        }
        if is_model_named(&candidate, app) || !library_classes_named(&candidate, app).is_empty() {
            return None;
        }
        scope.pop()?;
    }
}

/// The app library class (not a module, not a model) a superclass
/// spelling names, resolved as Ruby does from the subclass's lexical
/// namespace outwards (`Component::Schema` written inside `module
/// Billing` is `Billing::Component::Schema` when that exists).
fn app_library_superclass<'a>(lc: &LibraryClass, written: &str, app: &'a App) -> Option<&'a LibraryClass> {
    let rooted = written.starts_with("::");
    let written = written.trim_start_matches("::");
    let mut scope: Vec<&str> = if rooted { Vec::new() } else { lc.name.0.as_str().split("::").collect() };
    scope.pop();
    loop {
        let candidate =
            if scope.is_empty() { written.to_string() } else { format!("{}::{written}", scope.join("::")) };
        if is_model_named(&candidate, app) {
            return None;
        }
        if let Some(found) = library_classes_named(&candidate, app).into_iter().find(|c| !c.is_module) {
            return Some(found);
        }
        if scope.pop().is_none() {
            return None;
        }
    }
}

/// A declaration a file program carries inline (`ProgramItem::Inline`:
/// one under a condition, or in a file only a `require_relative`
/// reaches) is not analyzed or lowered, so nothing models the class-body
/// calls the ingest left uncaptured: they are replayed as the source has
/// them, before the methods, as for a foreign class body. A test mock
/// reopening an app service (`class << self; alias_method :price_from_service,
/// :price` before redefining `price`) needs its alias run.
struct InlineDeclaration(bool);

thread_local! {
    static INLINE_DECLARATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl InlineDeclaration {
    fn enter() -> Self {
        InlineDeclaration(INLINE_DECLARATION.with(|slot| slot.replace(true)))
    }

    fn active() -> bool {
        INLINE_DECLARATION.with(|slot| slot.get())
    }
}

impl Drop for InlineDeclaration {
    fn drop(&mut self) {
        let previous = self.0;
        INLINE_DECLARATION.with(|slot| slot.set(previous));
    }
}

/// The rendered declarations of one `ProgramItem::Inline`, enclosing
/// class first. A class nested in another class of the same declaration
/// (a factory class `DomainMock::MockDomain`, a `class Plain` in its
/// body) is spliced into that class's body where the source has it, as
/// the library emit does for a class's own file (`nested_blocks`,
/// `NestedPlacement`). Rendered on its own after it, it reopened the
/// enclosing class too late for that class's constants that read it
/// (`DEFAULT_DOMAINS = [MockDomain.new(…)]`), and, the enclosing class
/// being no library class, as a `module`.
fn inline_declarations(classes: &[LibraryClass], app: &App, out_path: &Path) -> Vec<String> {
    let owns = |c: &LibraryClass| {
        !c.is_module && !matches!(c.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. }))
    };
    // The outermost class of the batch that encloses `name`.
    let owner_of = |name: &str| -> Option<usize> {
        let segments: Vec<&str> = name.split("::").collect();
        (1..segments.len()).find_map(|n| {
            let prefix = segments[..n].join("::");
            classes.iter().rposition(|c| c.name.0.as_str() == prefix && owns(c))
        })
    };
    let owners: Vec<Option<usize>> = classes.iter().map(|c| owner_of(c.name.0.as_str())).collect();
    let mut out = Vec::new();
    for (i, lc) in classes.iter().enumerate() {
        if owners[i].is_some() {
            continue;
        }
        let kids: Vec<&LibraryClass> =
            classes.iter().zip(&owners).filter(|(_, o)| **o == Some(i)).map(|(c, _)| c).collect();
        if kids.is_empty() {
            out.push(emit_library_class_decl(lc, app, out_path.to_path_buf()).content);
            continue;
        }
        let nested = nested_blocks(lc, &kids, app, out_path, Some(out_path));
        let placing = NestedPlacement::install(lc, &nested);
        let mut files = vec![emit_library_class_decl(lc, app, out_path.to_path_buf())];
        let unplaced = placing.finish();
        splice_nested(&mut files, lc, &nested, &unplaced);
        out.push(files.remove(0).content);
    }
    out
}

/// A railtie or engine the app carries (`App::railties`): its class body
/// is replayed statement by statement under `run_body_statement`.
fn carried_railtie(lc: &LibraryClass, app: &App) -> bool {
    !lc.is_module && app.railties.iter().any(|r| r.name == lc.name)
}

/// The locals a class-body statement writes in the body's own scope:
/// not inside a block or lambda, whose writes are its own.
fn body_scope_locals(e: &Expr, out: &mut Vec<String>) {
    use crate::expr::LValue;
    let mut add = |name: &str| {
        // A global (`$VERBOSE = true`) or a class variable is no local.
        if name.starts_with('$') || name.starts_with('@') {
            return;
        }
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    };
    match &*e.node {
        ExprNode::Lambda { .. } => return,
        ExprNode::Assign { target: LValue::Var { name, .. }, .. } => add(name.as_str()),
        ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => add(name.as_str()),
        ExprNode::MultiAssign { targets, .. } => {
            for target in targets {
                if let LValue::Var { name, .. } = target {
                    add(name.as_str());
                }
            }
        }
        _ => {}
    }
    e.node.for_each_child(&mut |child| body_scope_locals(child, out));
}

/// A source file's path relative to the app root.
fn relative_source_path<'a>(app: &App, path: &'a str) -> &'a str {
    if app.root.is_empty() {
        return path;
    }
    path.strip_prefix(app.root.as_str()).map(|p| p.trim_start_matches('/')).unwrap_or(path)
}

/// Each app source file the tree carries => the emitted file
/// (`models/<stem>`, an app/models.rb anchor) its classes and modules
/// are in. A file that reopens a class carried from several sources maps
/// to that one emitted file.
fn carried_source_files(app: &App) -> CarriedSources {
    let mut table: BTreeMap<String, String> = BTreeMap::new();
    // A file with a file program loads as that program, which loads its
    // declarations in turn.
    for program in &app.file_programs {
        table.insert(relative_source_path(app, &program.source).to_string(), file_program_anchor(app, program));
    }
    for lc in &app.library_classes {
        let anchor = format!("models/{}", super::library_file_stem(lc.name.0.as_str(), app));
        let mut add = |span: crate::span::Span| {
            if let Some(source) = (span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) {
                table.entry(relative_source_path(app, &source.path).to_string()).or_insert_with(|| anchor.clone());
            }
        };
        for method in &lc.methods {
            add(method.name_span);
        }
        for call in lc.unknown_calls.iter().chain(&lc.class_ivar_initializers) {
            add(call.span);
        }
        for (_, value) in &lc.constants {
            add(value.span);
        }
    }
    // A file that declares only empty classes (shopify-money's
    // errors.rb) has no span to find it by: its first declaration.
    for (source, names) in &app.library_class_files {
        if let Some(first) = names.iter().min_by_key(|n| n.0.as_str().matches("::").count()) {
            table
                .entry(relative_source_path(app, source).to_string())
                .or_insert_with(|| format!("models/{}", super::library_file_stem(first.0.as_str(), app)));
        }
    }
    // A file with no program declares only classes and modules, each
    // emitted to a file of its own: requiring it loads every one of
    // them, in the order the file declares them, as Ruby running the
    // file defines them all. One anchor would leave the rest to an
    // autoload a plain gem's constants do not have (widget/taggable.rb's
    // `Taggable::ClassMethods`, which `Taggable.included` reads).
    //
    // Not a namespace another file's program defines, whose path names
    // it (paquito/errors.rb's `module Paquito`, paquito.rb's): this file
    // only reopens it, each of its own files opens it on the way in, and
    // that program is what loads this file. Its file comes with the
    // program, where Ruby defines it.
    //
    // In source order, each file where the file finishes giving it: a
    // value the file builds last (rollouts_mutex policy.rb's
    // `Policy::DEFAULT = Policy.new(...)`, written in the namespace's
    // file) needs what the file defined above it (`Declaration =
    // Data.define`). A file the source writes nothing into (an empty
    // class, a `Class.new`) goes first, in declaration order.
    let programs: BTreeSet<&str> = app.file_programs.iter().map(|p| p.source.as_str()).collect();
    let fold = |text: &str| text.chars().filter(|c| *c != '_').collect::<String>().to_ascii_lowercase();
    let mut anchor_sources: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (source, names) in &app.library_class_files {
        for name in names {
            anchor_sources
                .entry(format!("models/{}", super::library_file_stem(name.0.as_str(), app)))
                .or_default()
                .push(source.as_str());
        }
    }
    let owned_elsewhere = |anchor: &str, source: &str| {
        let wanted = format!("/{}", fold(anchor.strip_prefix("models/").unwrap_or(anchor)));
        anchor_sources.get(anchor).is_some_and(|sources| {
            sources.iter().any(|other| {
                *other != source
                    && programs.contains(other)
                    && fold(relative_source_path(app, other).strip_suffix(".rb").unwrap_or(other)).ends_with(&wanted)
            })
        })
    };
    let file_ids: BTreeMap<&str, u32> =
        app.sources.iter().enumerate().map(|(i, f)| (f.path.as_str(), i as u32 + 1)).collect();
    let last_written = |name: &str, file: u32| -> Option<u32> {
        let mut last: Option<u32> = None;
        let mut see = |span: crate::span::Span| {
            if span.file.0 == file {
                last = last.max(Some(span.end));
            }
        };
        for lc in library_classes_named(name, app) {
            see(lc.parent_span);
            for method in &lc.methods {
                see(method.name_span);
                see(method.body.span);
            }
            for (_, value) in &lc.constants {
                see(value.span);
            }
            for call in lc.unknown_calls.iter().chain(&lc.class_ivar_initializers) {
                see(call.span);
            }
        }
        last
    };
    let mut declarations: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (source, names) in &app.library_class_files {
        if programs.contains(source.as_str()) {
            continue;
        }
        let file = file_ids.get(source.as_str()).copied().unwrap_or(0);
        let mut anchors: Vec<(String, Option<u32>)> = Vec::new();
        for name in names {
            let anchor = format!("models/{}", super::library_file_stem(name.0.as_str(), app));
            if owned_elsewhere(&anchor, source) {
                continue;
            }
            let at = last_written(name.0.as_str(), file);
            match anchors.iter_mut().find(|(a, _)| *a == anchor) {
                Some((_, last)) => *last = (*last).max(at),
                None => anchors.push((anchor, at)),
            }
        }
        anchors.sort_by_key(|(_, at)| *at);
        declarations
            .entry(relative_source_path(app, source).to_string())
            .or_default()
            .extend(anchors.into_iter().map(|(anchor, _)| anchor));
    }
    let app_files = app.sources.iter().map(|f| relative_source_path(app, &f.path).to_string()).collect();
    let left_out = app
        .ingest_drops
        .iter()
        .filter(|drop| drop.construct.is_none())
        .filter_map(|drop| (drop.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)))
        .map(|source| relative_source_path(app, &source.path).to_string())
        .collect();
    CarriedSources { anchors: table, declarations, app_files, left_out }
}

/// `carried_source_files`, with every app source's root-relative path.
pub(super) struct CarriedSources {
    anchors: BTreeMap<String, String>,
    /// A source with no file program => the files of the classes and
    /// modules it declares, in its order.
    declarations: BTreeMap<String, Vec<String>>,
    app_files: crate::det::HashSet<String>,
    /// The app files whose classes, modules and constants ingest left
    /// out (a whole-file `ingest_unsupported` drop).
    left_out: BTreeSet<String>,
}

impl CarriedSources {
    /// An app file ingest took whole that gives the tree nothing to
    /// load: no program, no declaration of its own (kafka-shopify's
    /// writers.rb, `module Writers` holding only `autoload` calls, which
    /// app/models.rb registers). Requiring it loads nothing the tree
    /// does not already define.
    fn loads_nothing(&self, source: &str) -> bool {
        self.app_files.contains(source) && !self.left_out.contains(source) && !self.anchors.contains_key(source)
    }

    /// The emitted files that requiring `source` (an app file the tree
    /// carries nothing of: `loads_nothing`) loads through its own
    /// top-level requires, as Ruby runs the file: each carried file it
    /// requires, and those an uncarried file it requires requires in turn
    /// (a test mixin ingest splices into its includers declares no module
    /// of its own, but its `require` of the support module it includes
    /// still loads that module). `emitted` filters a carried file's
    /// declared anchors, as a program's `require` does.
    fn requires_of_uncarried(&self, app: &App, source: &str, emitted: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::from([source.to_string()]);
        self.collect_uncarried_requires(app, source, emitted, &mut seen, &mut out);
        out
    }

    fn collect_uncarried_requires(
        &self,
        app: &App,
        source: &str,
        emitted: &dyn Fn(&str) -> bool,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) {
        let Some(file) = app.sources.iter().find(|f| relative_source_path(app, &f.path) == source) else { return };
        for (feature, relative, _) in top_level_requires(file) {
            let mut resolved = program_require_source(source, &feature, relative, self);
            if resolved.is_none() && !relative {
                // A feature on the test load path (`require "support/helpers/x"`
                // for test/support/helpers/x.rb).
                let rb = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
                let mut hits = app.test_load_roots.iter().map(|root| format!("{root}{rb}")).filter(|p| self.app_files.contains(p));
                if let (Some(path), None) = (hits.next(), hits.next()) {
                    resolved = Some(match self.anchors.get(&path) {
                        Some(anchor) => Ok((path, anchor.clone())),
                        None => Err(path),
                    });
                }
            }
            match resolved {
                Some(Ok((required, anchor))) => {
                    let anchors: Vec<String> = match self.declarations.get(&required) {
                        Some(declared) if declared.is_empty() => Vec::new(),
                        Some(declared) => {
                            let mut anchors: Vec<String> = declared.iter().filter(|a| emitted(a)).cloned().collect();
                            if anchors.is_empty() {
                                anchors.push(anchor);
                            }
                            anchors
                        }
                        None => vec![anchor],
                    };
                    for anchor in anchors {
                        if !out.contains(&anchor) {
                            out.push(anchor);
                        }
                    }
                }
                Some(Err(required)) if self.loads_nothing(&required) && seen.insert(required.clone()) => {
                    self.collect_uncarried_requires(app, &required, emitted, seen, out);
                }
                _ => {}
            }
        }
    }
}

/// Each app source with no file program => the emitted files of the
/// classes and modules it declares, in its order: what requiring it
/// loads (`CarriedSources::declarations`).
pub(super) fn declarations_by_source(app: &App) -> BTreeMap<String, Vec<String>> {
    carried_sources(app).declarations.clone()
}

/// `carried_source_files`, built once per `App` while the lookup indexes
/// are installed: every file program and carried railtie asks, and the
/// table walks every library class.
fn carried_sources(app: &App) -> std::rc::Rc<CarriedSources> {
    match NameIndex::current(app) {
        Some(index) => index.carried.get_or_init(|| std::rc::Rc::new(carried_source_files(app))).clone(),
        None => std::rc::Rc::new(carried_source_files(app)),
    }
}

/// Where a file program's `require`/`require_relative` of `feature`
/// lands: `Some(Ok(anchor))` an app file the tree carries, `Some(Err)`
/// an app file it does not (the source names it, the tree has nothing
/// for it), `None` a feature outside the app (a gem's, the stdlib's).
/// `rel` is the program's source, relative to the app root.
fn program_require(
    rel: &str,
    feature: &str,
    relative: bool,
    table: &CarriedSources,
) -> Option<Result<String, String>> {
    program_require_source(rel, feature, relative, table).map(|found| found.map(|(_, anchor)| anchor))
}

/// `program_require`, with the source the feature resolves to.
fn program_require_source(
    rel: &str,
    feature: &str,
    relative: bool,
    table: &CarriedSources,
) -> Option<Result<(String, String), String>> {
    let rb = if feature.ends_with(".rb") { feature.to_string() } else { format!("{feature}.rb") };
    let source = if relative {
        let base = Path::new(rel).parent().unwrap_or(Path::new(""));
        let mut parts: Vec<String> = Vec::new();
        for part in base.join(&rb).to_string_lossy().split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other.to_string()),
            }
        }
        Some(parts.join("/"))
    } else {
        let (lib, nested) = (format!("lib/{rb}"), format!("/lib/{rb}"));
        let mut hits = table.anchors.keys().filter(|src| **src == lib || src.ends_with(&nested));
        match (hits.next(), hits.next()) {
            (Some(src), None) => Some(src.clone()),
            // No carried file, but one app file the feature names
            // (graphql-client's `require "graphql/client/operation_definition"`,
            // whose classes ingest left out): named, not required from
            // the gem home, which the emitted tree does not load a
            // carried gem's files from.
            (None, _) => {
                let mut files = table.app_files.iter().filter(|src| **src == lib || src.ends_with(&nested));
                match (files.next(), files.next()) {
                    (Some(src), None) => Some(src.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    if let Some((src, anchor)) = source.as_ref().and_then(|src| table.anchors.get(src).map(|a| (src, a))) {
        return Some(Ok((src.clone(), anchor.clone())));
    }
    let app_file = source.as_ref().is_some_and(|src| table.app_files.contains(src));
    (app_file || relative).then(|| Err(source.unwrap_or_default()))
}

/// A file program statement's `require "f"` / `require_relative "f"` of
/// an app file, resolved as `ProgramItem::Require` is: the emitted file,
/// or a named error for one the tree does not carry. A feature outside
/// the app stays as written.
fn rewrite_program_requires(e: &mut Expr, app: &App, rel: &str, sources: &mut Option<std::rc::Rc<CarriedSources>>) {
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node {
        let relative = method.as_str() == "require_relative";
        let feature = match args.as_slice() {
            [one] => match &*one.node {
                ExprNode::Lit { value: crate::expr::Literal::Str { value } } => Some(value.clone()),
                _ => None,
            },
            _ => None,
        };
        if let (true, Some(feature)) = (relative || method.as_str() == "require", feature) {
            let table = sources.get_or_insert_with(|| carried_sources(app));
            let span = e.span;
            let mk = |node: ExprNode| Expr::new(span, node);
            let lit = |value: String| mk(ExprNode::Lit { value: crate::expr::Literal::Str { value } });
            let call = |recv: Option<Expr>, method: &str, args: Vec<Expr>, parenthesized: bool| {
                mk(ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized })
            };
            match program_require(rel, &feature, relative, table) {
                Some(Ok(anchor)) => {
                    let path = call(Some(mk(ExprNode::Const { path: vec![Symbol::from("RoundhouseAutoload")] })), "path", vec![lit(anchor)], true);
                    *e = call(None, "require", vec![path], false);
                }
                Some(Err(source)) if table.loads_nothing(&source) => {
                    *e = mk(ExprNode::Lit { value: crate::expr::Literal::Bool { value: false } });
                }
                Some(Err(_)) => {
                    let message = format!(
                        "roundhouse: error[file_require_not_carried]: {}: `{method} {feature:?}` names an app file the tree does not carry; it was not loaded",
                        statement_site(app, span)
                    );
                    // STDERR, not `warn`: a diagnostic is not a Ruby warning, and
                    // Core's strict_warnings raises on every warning.
                    *e = call(Some(mk(ExprNode::Const { path: vec![Symbol::from("STDERR")] })), "puts", vec![lit(message)], true);
                }
                None => {}
            }
            return;
        }
    }
    e.node.for_each_child_mut(&mut |child| rewrite_program_requires(child, app, rel, sources));
}

/// A file program's `include M` of a Concern, followed by the extends
/// ActiveSupport::Concern's `append_features` performs on the includer
/// (`concern_carrier_extends`). The tree carries no Concern, so a class
/// body's include has them written after it (`write_include`); one in a
/// program statement had none. paquito 1.0.2's
/// `ActiveSupport.on_load(:active_record) do include
/// Paquito::ColumnsDigestCache end` left ActiveRecord::Base without
/// `paquito_columns_digest`, which its ActiveRecordCoder calls on every
/// record it dumps. Only constants that name an app module as written
/// (the statement is at the file's top level) are taken.
fn program_concern_extends(e: &mut Expr, app: &App) {
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node
        && method.as_str() == "include"
        && !args.is_empty()
    {
        let names: Option<Vec<ClassId>> = args
            .iter()
            .map(|a| match &*a.node {
                ExprNode::Const { path } => {
                    let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                    let name = name.trim_start_matches("::");
                    (!library_classes_named(name, app).is_empty()).then(|| ClassId(Symbol::from(name)))
                }
                _ => None,
            })
            .collect();
        let carriers = names.map(|n| concern_carrier_extends(&n, app)).unwrap_or_default();
        if !carriers.is_empty() {
            let span = e.span;
            let mut exprs = vec![e.clone()];
            for carrier in carriers {
                let path = std::iter::once(Symbol::from("")).chain(carrier.0.as_str().split("::").map(Symbol::from)).collect();
                let arg = Expr::new(span, ExprNode::Const { path });
                exprs.push(Expr::new(span, ExprNode::Send { recv: None, method: Symbol::from("extend"), args: vec![arg], block: None, parenthesized: false }));
            }
            *e = Expr::new(span, ExprNode::Seq { exprs });
        }
        return;
    }
    e.node.for_each_child_mut(&mut |child| program_concern_extends(child, app));
}

/// A carried engine body statement's loads of the app's own Ruby files,
/// resolved against the tree, where the source's files are not on
/// `$LOAD_PATH` (railties 8.2.0.alpha loads them from the engine root):
/// `require "feature"` that an app `lib/feature.rb` answers requires the
/// emitted file, and `Dir[config.root.join("lib/x/**/*.rb")]` (or
/// `root.join`, `Dir.glob`) lists the emitted files of the sources it
/// matches, naming at boot each matched source the tree does not carry
/// (`RoundhouseAutoload.carried_files`). Anything else stays as written.
fn carried_loads(
    e: &Expr,
    app: &App,
    root: Option<&str>,
    sources: &mut Option<std::rc::Rc<CarriedSources>>,
) -> Expr {
    let mut e = e.clone();
    rewrite_carried_loads(&mut e, app, root, sources);
    e
}

fn rewrite_carried_loads(
    e: &mut Expr,
    app: &App,
    root: Option<&str>,
    sources: &mut Option<std::rc::Rc<CarriedSources>>,
) {
    let str_arg = |args: &[Expr]| -> Option<String> {
        match args {
            [one] => match &*one.node {
                ExprNode::Lit { value: crate::expr::Literal::Str { value } } => Some(value.clone()),
                _ => None,
            },
            _ => None,
        }
    };
    let prefix = root.filter(|r| !r.is_empty()).map(|r| format!("{r}/")).unwrap_or_default();
    let mk = |span: crate::span::Span, node: ExprNode| Expr::new(span, node);
    let lit = |span: crate::span::Span, value: &str| {
        Expr::new(span, ExprNode::Lit { value: crate::expr::Literal::Str { value: value.to_string() } })
    };
    // `require_relative "../../test/support/x"` names a file beside the
    // engine's source, which the tree does not have beside the emitted
    // engine: resolved against the source as a file program's is.
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node {
        if method.as_str() == "require_relative" && str_arg(args).is_some() {
            let source = (e.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i));
            if let Some(source) = source {
                let rel = relative_source_path(app, &source.path).to_string();
                rewrite_program_requires(e, app, &rel, sources);
            }
            return;
        }
    }
    // `Dir[config.root.join("lib/patches/**/*.rb")].each { |file| require
    // file }` (essentials' engine): each carried file is required in turn
    // under its own refusal (`RoundhouseAutoload.require_carried_files`),
    // so one this tree cannot load is named and the others still load,
    // instead of its error ending the loop and the initializer with it.
    if let ExprNode::Send { recv: Some(glob), method, args, block: Some(block), .. } = &*e.node {
        let requires_each = method.as_str() == "each"
            && args.is_empty()
            && matches!(&*block.node, ExprNode::Lambda { params, rest_param: None, extra_params, block_param: None, body, .. }
                if params.len() == 1 && extra_params.is_empty() && {
                    let body = match &*body.node {
                        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
                        _ => body,
                    };
                    matches!(&*body.node, ExprNode::Send { recv: None, method, args, block: None, .. }
                        if method.as_str() == "require"
                            && matches!(args.as_slice(), [arg] if matches!(&*arg.node, ExprNode::Var { name, .. } if *name == params[0])))
                });
        if requires_each {
            let glob = carried_loads(glob, app, root, sources);
            if let ExprNode::Send { recv: Some(owner), method, args, .. } = &*glob.node {
                if method.as_str() == "carried_files"
                    && matches!(&*owner.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "RoundhouseAutoload")
                {
                    *e = Expr::new(e.span, ExprNode::Send {
                        recv: Some(owner.clone()),
                        method: Symbol::from("require_carried_files"),
                        args: args.clone(),
                        block: None,
                        parenthesized: true,
                    });
                    return;
                }
            }
        }
    }
    let mut replacement: Option<Expr> = None;
    if let ExprNode::Send { recv, method, args, block: None, .. } = &*e.node {
        let span = e.span;
        if recv.is_none() && method.as_str() == "require" {
            if let Some(feature) = str_arg(args) {
                let table = sources.get_or_insert_with(|| carried_sources(app));
                let rb = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
                let own = format!("{prefix}lib/{rb}");
                let (lib, nested) = (format!("lib/{rb}"), format!("/lib/{rb}"));
                let anchor = table.anchors.get(&own).or_else(|| {
                    let mut hits = table.anchors.iter().filter(|(src, _)| **src == lib || src.ends_with(&nested));
                    match (hits.next(), hits.next()) {
                        (Some((_, a)), None) => Some(a),
                        _ => None,
                    }
                });
                if let Some(anchor) = anchor {
                    let path = mk(span, ExprNode::Send {
                        recv: Some(mk(span, ExprNode::Const { path: vec![Symbol::from("RoundhouseAutoload")] })),
                        method: Symbol::from("path"),
                        args: vec![lit(span, anchor)],
                        block: None,
                        parenthesized: true,
                    });
                    replacement = Some(mk(span, ExprNode::Send {
                        recv: None,
                        method: Symbol::from("require"),
                        args: vec![path],
                        block: None,
                        parenthesized: false,
                    }));
                }
            }
        }
        let dir_glob = matches!(recv.as_ref().map(|r| &*r.node), Some(ExprNode::Const { path }) if path.len() == 1 && path[0].as_str() == "Dir")
            && matches!(method.as_str(), "[]" | "glob");
        if dir_glob && args.len() == 1 {
            let pattern = match &*args[0].node {
                ExprNode::Send { recv: Some(base), method, args: join, block: None, .. } if method.as_str() == "join" => {
                    let engine_root = match &*base.node {
                        ExprNode::Send { recv: None, method, args, .. } => method.as_str() == "root" && args.is_empty(),
                        ExprNode::Send { recv: Some(config), method, args, .. } => {
                            method.as_str() == "root"
                                && args.is_empty()
                                && matches!(&*config.node, ExprNode::Send { recv: None, method, args, .. } if method.as_str() == "config" && args.is_empty())
                        }
                        _ => false,
                    };
                    if engine_root { str_arg(join) } else { None }
                }
                _ => None,
            };
            let supported = |p: &str| !p.contains(['?', '{', '}', '[', ']', '\\']);
            if let Some(pattern) = pattern.filter(|p| supported(p)) {
                let full = format!("{prefix}{}", pattern.trim_start_matches('/'));
                let table = sources.get_or_insert_with(|| carried_sources(app));
                let mut matched: Vec<String> = app
                    .sources
                    .iter()
                    .map(|s| relative_source_path(app, &s.path).to_string())
                    .filter(|p| crate::ingest::routes::glob_matches(&full, p))
                    .collect();
                matched.sort();
                matched.dedup();
                let mut anchors: Vec<Expr> = Vec::new();
                let mut missing: Vec<Expr> = Vec::new();
                for source in &matched {
                    let Some(anchor) = table.anchors.get(source) else {
                        missing.push(lit(span, source));
                        continue;
                    };
                    // A file with no program loads every class and module
                    // it declares, as a boot `require` of it does
                    // (`declarations_by_source`): essentials'
                    // patches/request.rb declares RequestHelperMethods and
                    // reopens ActionDispatch::Request to include it, and
                    // its one anchor (the module's) left the reopen, which
                    // nothing autoloads, unloaded.
                    let files = match table.declarations.get(source) {
                        Some(files) if !files.is_empty() => crate::project::namespace_files_first(files),
                        _ => vec![anchor.clone()],
                    };
                    for file in files {
                        if !anchors.iter().any(|a| matches!(&*a.node, ExprNode::Lit { value: crate::expr::Literal::Str { value } } if *value == file)) {
                            anchors.push(lit(span, &file));
                        }
                    }
                }
                let array = |elements: Vec<Expr>| mk(span, ExprNode::Array { elements, style: Default::default() });
                replacement = Some(mk(span, ExprNode::Send {
                    recv: Some(mk(span, ExprNode::Const { path: vec![Symbol::from("RoundhouseAutoload")] })),
                    method: Symbol::from("carried_files"),
                    args: vec![lit(span, &full), array(anchors), array(missing)],
                    block: None,
                    parenthesized: true,
                }));
            }
        }
    }
    if let Some(replacement) = replacement {
        *e = replacement;
        return;
    }
    e.node.for_each_child_mut(&mut |child| rewrite_carried_loads(child, app, root, sources));
}

/// The app/models.rb anchor of a support file's file program
/// (`App::file_programs`): `models/roundhouse_files/<source path>`.
pub(super) fn file_program_anchor(app: &App, program: &crate::app::FileProgram) -> String {
    let rel = relative_source_path(app, &program.source).trim_start_matches('/');
    let rel = rel.strip_suffix(".rb").unwrap_or(rel);
    format!("models/roundhouse_files/{rel}")
}

/// A support file's file program as one Ruby file: what Ruby runs when
/// it loads the source, in source order (`ingest::library_class::
/// file_program`). `emitted` is every app/models.rb anchor the tree
/// has. A top-level declaration loads its emitted file where it stands;
/// a statement runs under `RoundhouseAutoload.file_statement`, which
/// names one that fails and goes on; a guard and a conditional run as
/// written, with the declarations they hold inline; a `require` of an
/// app file loads the emitted file, and of anything else is kept as
/// written.
pub(super) fn emit_file_program(
    program: &crate::app::FileProgram,
    app: &App,
    out_path: &Path,
    emitted: &BTreeSet<String>,
) -> String {
    use crate::app::ProgramItem;
    let mut sources: Option<std::rc::Rc<CarriedSources>> = None;
    let rel = relative_source_path(app, &program.source).to_string();
    let mut s = format!(
        "# The file-level program of {rel}: what Ruby runs when it loads\n\
         # that file, in source order. Loaded where a file in this tree\n\
         # requires it, else at boot (app/models.rb).\n"
    );
    fn locals_of(items: &[ProgramItem], out: &mut Vec<String>) {
        for item in items {
            match item {
                ProgramItem::Statement(e) | ProgramItem::Guard(e) => body_scope_locals(e, out),
                ProgramItem::Branch { then_items, else_items, .. } => {
                    locals_of(then_items, out);
                    locals_of(else_items, out);
                }
                _ => {}
            }
        }
    }
    let mut locals = Vec::new();
    locals_of(&program.items, &mut locals);
    for local in &locals {
        writeln!(s, "{local} = nil").unwrap();
    }
    let mut loaded: BTreeSet<String> = BTreeSet::new();
    fn render(
        items: &[ProgramItem],
        pad: &str,
        app: &App,
        rel: &str,
        out_path: &Path,
        emitted: &BTreeSet<String>,
        sources: &mut Option<std::rc::Rc<CarriedSources>>,
        loaded: &mut BTreeSet<String>,
        s: &mut String,
    ) {
        let lines = |s: &mut String, pad: &str, text: &str| {
            for line in text.lines() {
                if line.is_empty() {
                    s.push('\n');
                } else {
                    writeln!(s, "{pad}{line}").unwrap();
                }
            }
        };
        let require_anchor = |s: &mut String, anchor: &str| {
            writeln!(s, "{pad}require RoundhouseAutoload.path({anchor:?})").unwrap();
        };
        for item in items {
            match item {
                ProgramItem::Declared(names) => {
                    for name in names {
                        let anchor = format!("models/{}", super::library_file_stem(name.0.as_str(), app));
                        if emitted.contains(&anchor) && loaded.insert(anchor.clone()) {
                            require_anchor(s, &anchor);
                        }
                        // The constants this file wrote that have a file
                        // of their own (`constant_file`) load with it.
                        for stem in constant_files_written_in(name.0.as_str(), rel, app) {
                            let anchor = format!("models/{stem}");
                            if emitted.contains(&anchor) && loaded.insert(anchor.clone()) {
                                require_anchor(s, &anchor);
                            }
                        }
                        // And those it runs after statements of its own
                        // (`program_constants`), here.
                        if loaded.insert(format!("constants of {}", name.0.as_str())) {
                            if let Some(reopen) = program_constants_written_in(name.0.as_str(), rel, app, out_path) {
                                lines(s, pad, &reopen);
                            }
                        }
                    }
                }
                ProgramItem::Statement(expr) => {
                    let mut expr = carried_loads(expr, app, None, sources);
                    rewrite_program_requires(&mut expr, app, rel, sources);
                    program_concern_extends(&mut expr, app);
                    let text = super::emit_expr(&expr);
                    writeln!(s, "{pad}RoundhouseAutoload.file_statement({:?}) do", statement_site(app, expr.span)).unwrap();
                    lines(s, &format!("{pad}  "), &text);
                    writeln!(s, "{pad}end").unwrap();
                }
                // A `return` in the block still ends the file's load.
                ProgramItem::Guard(expr) => {
                    writeln!(s, "{pad}RoundhouseAutoload.file_statement({:?}) do", statement_site(app, expr.span)).unwrap();
                    lines(s, &format!("{pad}  "), &super::emit_expr(expr));
                    writeln!(s, "{pad}end").unwrap();
                }
                ProgramItem::Require { span, feature, relative } => {
                    let table = sources.get_or_insert_with(|| carried_sources(app));
                    let site = statement_site(app, *span);
                    let resolved = program_require_source(rel, feature, *relative, table);
                    // A gem file requiring itself (carriers_db's
                    // country_index.rb, `require "carriers_db/country_index"`):
                    // the file is loading, so natively the require is a
                    // circular no-op that answers false, and its $VERBOSE
                    // warning is located at RubyGems' `<internal:` require
                    // or the gem's installed path, both of which Core's
                    // StrictWarning exempts. In the emitted tree a
                    // `require` wrapper in a plain file (the boot survey's
                    // Kernel hook) is the location, and the warning raised
                    // at boot. An app file's own self-require is kept.
                    if let Some(Ok((source, _))) = &resolved {
                        if source.trim_start_matches('/') == rel.trim_start_matches('/')
                            && app.gem_source_dirs.iter().any(|dir| rel.trim_start_matches('/').starts_with(dir.as_str()))
                        {
                            writeln!(s, "{pad}# {site}: `require{} {feature:?}` names this gem file itself, which is loading: Ruby's circular require, a no-op", if *relative { "_relative" } else { "" }).unwrap();
                            continue;
                        }
                    }
                    match resolved {
                        Some(Ok((source, anchor))) => {
                            // A file that only reopens what another
                            // program defines loads nothing of its own.
                            let anchors: Vec<String> = match table.declarations.get(&source) {
                                Some(declared) if declared.is_empty() => Vec::new(),
                                Some(declared) => {
                                    let mut anchors: Vec<String> =
                                        declared.iter().filter(|a| emitted.contains(*a)).cloned().collect();
                                    // Every class it declares was dropped (a Rails
                                    // generator class the runtime does not port, a
                                    // `lower_residue` warning) and no file of it is
                                    // written: Ruby's require has nothing to load here.
                                    if anchors.is_empty() && emitted.contains(&anchor) {
                                        anchors.push(anchor);
                                    }
                                    anchors
                                }
                                None => vec![anchor],
                            };
                            // A gem source's require keeps Ruby's circular
                            // no-op for a file still loading
                            // (`RoundhouseAutoload.require_gem_file`).
                            let gem_file = app.gem_source_dirs.iter().any(|dir| rel.trim_start_matches('/').starts_with(dir.as_str()));
                            for anchor in anchors {
                                if loaded.insert(anchor.clone()) {
                                    if gem_file {
                                        writeln!(s, "{pad}RoundhouseAutoload.require_gem_file({anchor:?})").unwrap();
                                    } else {
                                        require_anchor(s, &anchor);
                                    }
                                }
                            }
                        }
                        // Natively it opens its modules (kafka-shopify's
                        // writers.rb, `module Writers` holding only
                        // `autoload`s): they are opened here, which
                        // registers the autoloads under them.
                        Some(Err(source)) if table.loads_nothing(&source) => {
                            let text = app.sources.iter().find(|f| relative_source_path(app, &f.path) == source).map(|f| f.text.as_str());
                            let opened = text.and_then(crate::ingest::opened_modules::opened_modules).unwrap_or_default();
                            writeln!(s, "{pad}# {site}: `require{} {feature:?}` loads {source}, which declares nothing the tree carries beyond the modules it opens", if *relative { "_relative" } else { "" }).unwrap();
                            for path in opened {
                                let open: Vec<String> = path.iter().map(|segment| format!("module {segment}")).collect();
                                writeln!(s, "{pad}{}; {}", open.join("; "), vec!["end"; path.len()].join("; ")).unwrap();
                            }
                            // Its own requires still run.
                            for anchor in table.requires_of_uncarried(app, &source, &|a| emitted.contains(a)) {
                                if loaded.insert(anchor.clone()) {
                                    require_anchor(s, &anchor);
                                }
                            }
                        }
                        Some(Err(_)) => {
                            let message = format!(
                                "roundhouse: error[file_require_not_carried]: {site}: `require{} {feature:?}` names an app file the tree does not carry; it was not loaded",
                                if *relative { "_relative" } else { "" }
                            );
                            writeln!(s, "{pad}$stderr.puts {message:?}").unwrap();
                        }
                        // graphql-shopify's `require "active_support/core_ext/
                        // class/attribute"`: the runtime ports are the
                        // framework here, loaded before any app file, and
                        // no such file is on the tree's load path (as the
                        // gem-source dependency block says of the same).
                        None if !*relative && crate::project::is_framework_feature(feature) => {
                            writeln!(s, "{pad}# {site}: `require {feature:?}` is a Rails feature this tree's runtime ports answer; not required").unwrap();
                        }
                        None => {
                            writeln!(s, "{pad}RoundhouseAutoload.file_statement({site:?}) do").unwrap();
                            writeln!(s, "{pad}  require {feature:?}").unwrap();
                            writeln!(s, "{pad}end").unwrap();
                        }
                    }
                }
                ProgramItem::Inline(classes) => {
                    let _inline = InlineDeclaration::enter();
                    for rendered in inline_declarations(classes, app, out_path) {
                        lines(s, pad, &rendered);
                    }
                }
                // Under `file_statement`, as a plain statement: a receiver
                // this tree does not define (essentials' patches/
                // action_view_ext.rb reopens `ActionView::Helpers::
                // AssetUrlHelper`) is named and the file's other
                // statements still run, as does the engine's
                // `lib/patches/**` glob that requires the file.
                ProgramItem::ClassEval { receiver, method, parts } => {
                    writeln!(s, "{pad}RoundhouseAutoload.file_statement({:?}) do", statement_site(app, receiver.span)).unwrap();
                    let outer = pad;
                    let pad = &format!("{outer}  ");
                    writeln!(s, "{pad}{}.{method} do", super::emit_expr(receiver)).unwrap();
                    // A model's class-body macros are the lowering's (see
                    // `replays_foreign_class_body`); any other receiver
                    // runs its body's calls itself.
                    let replays = match &*receiver.node {
                        ExprNode::Const { path } => {
                            let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                            !is_model_named(name.trim_start_matches("::"), app)
                        }
                        _ => false,
                    };
                    class_eval_parts(parts, &format!("{pad}  "), app, out_path, method, replays, s);
                    writeln!(s, "{pad}end").unwrap();
                    writeln!(s, "{outer}end").unwrap();
                }
                // A condition that raises runs neither branch, named.
                ProgramItem::Branch { cond, negate, then_items, else_items } => {
                    let site = statement_site(app, cond.span);
                    let cond_text = super::emit_expr(cond);
                    if cond_text.contains('\n') {
                        writeln!(s, "{pad}roundhouse_condition = RoundhouseAutoload.file_condition({site:?}) do").unwrap();
                        lines(s, &format!("{pad}  "), &cond_text);
                        writeln!(s, "{pad}end").unwrap();
                    } else {
                        writeln!(s, "{pad}roundhouse_condition = RoundhouseAutoload.file_condition({site:?}) {{ {cond_text} }}").unwrap();
                    }
                    writeln!(s, "{pad}if roundhouse_condition.equal?(RoundhouseAutoload::NOT_RUN)").unwrap();
                    writeln!(s, "{pad}  nil").unwrap();
                    writeln!(s, "{pad}elsif {}roundhouse_condition", if *negate { "!" } else { "" }).unwrap();
                    let inner = format!("{pad}  ");
                    render(then_items, &inner, app, rel, out_path, emitted, sources, loaded, s);
                    if !else_items.is_empty() {
                        writeln!(s, "{pad}else").unwrap();
                        render(else_items, &inner, app, rel, out_path, emitted, sources, loaded, s);
                    }
                    writeln!(s, "{pad}end").unwrap();
                }
                // A body statement its class writes back in place
                // (`carries_body_statement`) runs there.
                ProgramItem::NotCarried { span, .. }
                    if super::expr::native_keywords() && carried_body_statement_spans(app).contains(span) => {}
                ProgramItem::NotCarried { span, message, .. } => {
                    let text = format!(
                        "roundhouse: error[file_statement_not_carried]: {}: {message}",
                        statement_site(app, *span)
                    );
                    writeln!(s, "{pad}$stderr.puts {text:?}").unwrap();
                }
            }
        }
    }
    render(&program.items, "", app, &rel, out_path, emitted, &mut sources, &mut loaded, &mut s);
    s
}

/// A `class_eval` body's parts (`ProgramItem::ClassEval`): each body the
/// module's, without its `module`/`end`; each branch an `if`/`unless`.
/// `replays`: the body's uncaptured class-body calls run on the receiver
/// (any receiver but a model).
fn class_eval_parts(
    parts: &[crate::app::ClassEvalPart],
    pad: &str,
    app: &App,
    out_path: &Path,
    method: &str,
    replays: bool,
    s: &mut String,
) {
    use crate::app::ClassEvalPart;
    for part in parts {
        match part {
            ClassEvalPart::Body(lc) => {
                // Not analyzed or lowered, as an inline declaration is
                // not: its class-body calls (asset_cloud's
                // `callback_methods :write, :delete`, a macro an included
                // module extends the receiver with) replay as the source
                // has them, on the receiver.
                let rendered = {
                    let _inline = replays.then(InlineDeclaration::enter);
                    emit_library_class_decl(lc, app, out_path.to_path_buf()).content
                };
                let header = format!("module {}", crate::ingest::library_class::CLASS_EVAL_BODY);
                let mut body: Vec<&str> = rendered.lines().collect();
                // The load-time requires the body's constants need
                // (written relative to this file program), run first
                // inside the block, where `require_relative` resolves
                // from this file as at its top.
                let lead = body.iter().take_while(|l| **l != header && (l.is_empty() || l.starts_with("require"))).count();
                for line in body.drain(..lead).filter(|l| !l.is_empty()) {
                    writeln!(s, "{pad}{line}").unwrap();
                }
                match body.as_slice() {
                    [first, inner @ .., "end"] if *first == header => {
                        for line in inner {
                            let line = line.strip_prefix("  ").unwrap_or(line);
                            if line.is_empty() {
                                s.push('\n');
                            } else {
                                writeln!(s, "{pad}{line}").unwrap();
                            }
                        }
                    }
                    _ => {
                        let message = format!(
                            "roundhouse: error[file_statement_not_carried]: a part of this `{method}` body did not emit as a module body; it was not carried"
                        );
                        writeln!(s, "{pad}$stderr.puts {message:?}").unwrap();
                    }
                }
            }
            ClassEvalPart::Branch { cond, negate, then_parts, else_parts } => {
                let keyword = if *negate { "unless" } else { "if" };
                writeln!(s, "{pad}{keyword} {}", super::emit_expr(cond)).unwrap();
                class_eval_parts(then_parts, &format!("{pad}  "), app, out_path, method, replays, s);
                if !else_parts.is_empty() {
                    writeln!(s, "{pad}else").unwrap();
                    class_eval_parts(else_parts, &format!("{pad}  "), app, out_path, method, replays, s);
                }
                writeln!(s, "{pad}end").unwrap();
            }
        }
    }
}

/// `path:line` of a body statement, the path relative to the app root.
fn statement_site(app: &App, span: crate::span::Span) -> String {
    let Some(source) = (span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) else {
        return "(unknown)".to_string();
    };
    let (line, _) = source.line_col(span.start);
    let path = source.path.as_str();
    let path = if app.root.is_empty() {
        path
    } else {
        path.strip_prefix(app.root.as_str()).map(|p| p.trim_start_matches('/')).unwrap_or(path)
    };
    format!("{path}:{line}")
}

/// Whether `lc` is an app declaration of a class or module that a gem
/// the tree loads defines (`GemBoundary::loaded_reopen`), naming no other
/// superclass: a reopen. Ruby runs its body on the gem's class object
/// once the gem is loaded, which the CRuby tree does at boot before any
/// app file (`runtime/gem_facades.rb`), and the autoloader requires the
/// reopen's file as soon as it registers the already defined constant.
/// Core's essentials liquid_ext.rb `prepend`s a patch onto and
/// `remove_const`s from the liquid gem's `Liquid::Render` this way. Not
/// a framework class the runtime models (`runtime_reopen_anchor`).
fn reopens_loaded_gem_class(lc: &LibraryClass, app: &App) -> bool {
    if lc.origin.is_some() || runtime_reopen_anchor(lc.name.0.as_str()).is_some() {
        return false;
    }
    let written = |id: &crate::ident::ClassId| id.0.as_str().trim_start_matches("::").to_string();
    let (is_module, parent) = match app.gem_boundary.loaded_reopen(&lc.name) {
        Some(gem) => (gem.is_module, gem.parent.as_ref().map(written)),
        // No RBI for the gem: what its installed source opens.
        None => match app.gem_boundary.loaded_source_reopen(&lc.name) {
            Some((_, kind)) => (kind.is_module, kind.parent.as_deref().map(|p| p.trim_start_matches("::").to_string())),
            None => return false,
        },
    };
    if is_module != lc.is_module {
        return false;
    }
    match (&lc.parent, parent) {
        (None, _) => true,
        (Some(own), Some(gems)) => written(own) == gems,
        (Some(_), None) => false,
    }
}

/// Ledger a class-body call we captured but chose not to replay. The
/// emit diagnostic sink is live here (ingest's is not — it runs before
/// any `diagnostics::scope`), which is why the judgment happens at emit
/// rather than where the call was captured.
/// A class-body call that is not replayed because the tree already does
/// what it does elsewhere, so it is not reported as dropped:
/// - `extend ActiveSupport::Concern` in a module. The tree carries no
///   Concern; what it does to an includer, extending it with the
///   module's `ClassMethods` (and its Concern dependencies'), and running
///   its `included` block there, is written at each include
///   (`concern_carrier_extends`, the ingest's concern splices). An
///   `included` / `class_methods` block that is not carried is reported
///   on its own. frozen_record 0.27.4's `FrozenRecord::Compact`.
/// - `autoload :Const, "feature"` for a library Ruby ships (a
///   `rbs/requires.tsv` row: `require feature` defines `Const`). The
///   tree requires the feature in every file naming the constant
///   (`project::require_gaps`), and the module's own lookup of the name
///   then finds the top-level constant, as it does natively once the
///   autoload has loaded it. frozen_record's `Backends::Yaml`
///   `autoload :ERB, 'erb'`.
fn modelled_elsewhere(lc: &LibraryClass, call: &Expr) -> bool {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*call.node else { return false };
    match (method.as_str(), args.as_slice()) {
        ("extend", [only]) => {
            lc.is_module
                && matches!(&*only.node, ExprNode::Const { path }
                    if path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").trim_start_matches("::") == "ActiveSupport::Concern")
        }
        ("autoload", [name, feature]) => {
            let (ExprNode::Lit { value: crate::expr::Literal::Sym { value: name } }, ExprNode::Lit { value: crate::expr::Literal::Str { value: feature } }) =
                (&*name.node, &*feature.node)
            else {
                return false;
            };
            crate::stdlib_rbs::requires().iter().any(|row| row.constant == name.as_str() && row.feature == feature.as_str())
        }
        _ => false,
    }
}

/// The class-body calls the Ruby family drops from `lc` (each reported
/// by `report_dropped_class_body_call`): none where the body replays
/// (a railtie, a foreign base), else every unknown call no lowering
/// writes or answers elsewhere.
pub(crate) fn dropped_class_body_calls<'a>(lc: &'a LibraryClass, app: &App) -> Vec<&'a Expr> {
    if carried_railtie(lc, app) || replays_foreign_class_body(lc, app) {
        return Vec::new();
    }
    let own = modelled_class_body_calls(lc, app);
    let carried_block = |call: &Expr| carried_included_block(lc, call, app);
    // A `delegate_missing_to` is written as the `method_missing` pair
    // placed at it (`ingest::delegate::missing_delegators`), added
    // to the methods above.
    let written_pair = |call: &Expr| {
        app.ruby_declined_delegate_forwarders.contains_key(&lc.name)
            && lc.methods.iter().any(|m| {
                m.receiver == MethodReceiver::Instance && m.name.as_str() == "method_missing" && m.name_span == call.span
            })
    };
    lc.unknown_calls
        .iter()
        .filter(|call| {
            !own.iter().any(|c| std::ptr::eq(*c, *call))
                && !crate::analyze::is_include_refusal(call)
                && !carried_block(call)
                && !modelled_elsewhere(lc, call)
                && !written_pair(call)
                && !is_body_statement(call)
                && !trailing_freeze(lc, app).is_some_and(|f| std::ptr::eq(f, *call))
        })
        .collect()
}

/// Whether `call` is a Concern's `included do` that ingest gave every
/// class it runs on (or reported there): not missing from any of them,
/// and not written in the module's own body.
fn carried_included_block(lc: &LibraryClass, call: &Expr, app: &App) -> bool {
    app.carried_included_blocks.contains(&lc.name)
        && matches!(&*call.node, crate::expr::ExprNode::Send { recv: None, method, block: Some(_), .. }
            if method.as_str() == "included")
}

/// The body's closing `freeze` (`self.freeze`), which Ruby runs after
/// everything above it: Core's component/schema.rb `module BooleanType;
/// freeze; end`. Written as the body's last statement, after every def,
/// constant and nested class the emit places in it. Only where nothing
/// the tree writes later adds to the class: a nested class written in a
/// file of its own, or a reopen, would raise FrozenError where the
/// source's single body did not.
pub(crate) fn trailing_freeze<'a>(lc: &'a LibraryClass, app: &App) -> Option<&'a Expr> {
    let call = lc.unknown_calls.last()?;
    let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { return None };
    let name = lc.name.0.as_str().trim_start_matches("::");
    let nested = format!("{name}::");
    let nests = |id: &ClassId| id.0.as_str().trim_start_matches("::").starts_with(&nested);
    (method.as_str() == "freeze"
        && args.is_empty()
        && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        && !app.library_class_reopens.contains_key(&lc.name)
        && !app.models.iter().any(|m| m.name == lc.name || nests(&m.name))
        && !app.controllers.iter().any(|c| c.name == lc.name || nests(&c.name))
        && !app.library_classes.iter().any(|other| nests(&other.name)))
    .then_some(call)
}

fn report_dropped_class_body_call(lc: &LibraryClass, call: &Expr) {
    let name = crate::lower::model_to_library::dropped_statement_name(call);
    let mut diagnostic = crate::lower::residue_diagnostic(
        "library_class_body",
        &name,
        call.span,
        "not modelled",
        format!(
            "`{}` in the body of `{}` is not modelled and is dropped; only a class \
             extending a base roundhouse does NOT model (a gem's, whose DSL is the \
             class) replays its class body verbatim",
            name,
            lc.name.0.as_str(),
        ),
    );
    // A `define_method` the ingest could not take as the method it
    // defines (`library_class::class_body_define_method`: its block
    // reaches the class body, or takes formals a method cannot) drops a
    // method its callers need: refused, not a warning.
    if let ExprNode::Send { recv: None, method, block: Some(_), .. } = &*call.node
        && method.as_str() == "define_method"
    {
        diagnostic.severity = crate::diagnostic::Severity::Error;
        diagnostic.message = format!(
            "`{name}` in the body of `{}` defines a method from a block that reads the class \
             body (one of its locals, `block_given?`, a bare `super`) or takes formals a method \
             cannot (post, anonymous or numbered parameters); it is not modelled and is dropped",
            lc.name.0.as_str(),
        );
    }
    crate::emit::diagnostics::push(diagnostic);
}

/// Is this name one of the app's own CLASSES (not a module)? Namespace
/// segments that name one must reopen as `class`, not `module`.
/// The file a class belongs in: the OUTERMOST enclosing app class, or
/// the class itself when no enclosing segment is one.
///
/// A nested class used to get a file of its own, which meant every
/// part-file re-opened the outer class and CLOSED it again — and
/// `TracePoint(:end)`, which is how a gem implements "end of class
/// body", fires on each of those closes. A gem that validates there
/// saw a body missing what the parent's own file sets. Naming the same
/// superclass in both places (see `outer_header`) makes the ORDER
/// irrelevant; it cannot make the COUNT of body-ends one.
///
/// Outermost rather than nearest: `A::B::C::D` with both `A::B` and
/// `A::B::C` classes belongs in `A::B`'s file, because that is the
/// body whose end must happen once.
pub(super) fn file_owner(name: &str, app: &App) -> String {
    // A class a load-time conditional declares is written inside that
    // conditional, in its owner's body, wherever that is.
    if let Some(owner) = conditional_owner(name, app) {
        return file_owner(&owner, app);
    }
    // A class declared in a `class << self` (`App::singleton_scoped_classes`)
    // is written inside one in its owner's body, a module's too: a file
    // of its own would have to reopen the singleton class, which no
    // autoload reaches.
    if let Some(owner) = singleton_scope_owner(name, app) {
        return file_owner(&owner, app);
    }
    if app.library_classes.iter().any(|class| class.name.0.as_str() == name
        && matches!(class.origin, Some(LibraryClassOrigin::DataFactory { .. })))
    {
        return file_owner(name.rsplit_once("::").unwrap().0, app);
    }
    let segments: Vec<&str> = name.split("::").collect();
    // Written apart from an enclosing class, it may still share a file
    // with one nested deeper: apps_runtime's database/placeholders.rb
    // writes `class Static < Placeholders` inside `class Placeholders`,
    // apart from database.rb's `class Database`. Given a file of its
    // own, `STATIC = Static.new` in Placeholders' body ran before it.
    let mut apart_from_outer = false;
    for i in 0..segments.len().saturating_sub(1) {
        let prefix = segments[..=i].join("::");
        if owns_a_file(&prefix, app) {
            let mut own = body_files(name, app);
            own.extend(declared_files(name, app));
            // An empty class under app/models has no body span and no
            // `library_class_files` entry: its superclass is where it
            // was written. Core's session/codec.rb `class
            // UnsupportedCheckoutSessionError < DecodingError; end`,
            // written into session.rb's body, read the codec at
            // Session's top, whose table read `Session::CartNext`
            // before session.rb defined it.
            if own.is_empty() {
                own.extend(library_classes_named(name, app).iter().map(|c| c.parent_span.file.0).filter(|f| *f != 0));
            }
            if written_apart(&own, &prefix, app) {
                apart_from_outer = true;
                continue;
            }
            if apart_from_outer {
                return file_owner(&prefix, app);
            }
            // A class with no source spans of its own (the ingest's
            // CurrentAttributes lowering generates every method of
            // `class Current < ActiveSupport::CurrentAttributes`) was
            // written where its nearest enclosing class that keeps a file
            // apart was: Core's `Markets::ShopScope::Cached::Current`, in
            // shop_scope/cached.rb. In the outermost class's file it
            // opened `class Cached` there, which autoloaded cached.rb,
            // whose require of shop_scope.rb was then a circular require.
            if own.is_empty() {
                let apart = (i + 1..segments.len() - 1).rev().map(|j| segments[..=j].join("::")).find(|inner| owns_a_file(inner, app) && file_owner(inner, app) == *inner);
                if let Some(inner) = apart {
                    return inner;
                }
            }
            return prefix;
        }
    }
    name.to_string()
}

/// The class or module whose `class << self` declares `name` or the
/// outermost class enclosing it there (`App::singleton_scoped_classes`),
/// when that owner is a library class with a `class`/`module` body.
fn singleton_scope_owner(name: &str, app: &App) -> Option<String> {
    if app.singleton_scoped_classes.is_empty() {
        return None;
    }
    let segments: Vec<&str> = name.split("::").collect();
    (2..=segments.len()).find_map(|n| {
        let scoped = segments[..n].join("::");
        if !app.singleton_scoped_classes.iter().any(|c| c.0.as_str() == scoped) {
            return None;
        }
        let owner = segments[..n - 1].join("::");
        let hosts = library_classes_named(&owner, app)
            .iter()
            .any(|c| !matches!(c.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. })));
        hosts.then_some(owner)
    })
}

/// Whether a nested class was written in a source file of its own,
/// apart from every file its owner's body came from: Component::Config's
/// `inherited` hook autoloads `Config::Test` from `config/test.rb` beside
/// the file that defined `Config` (`Object.const_source_location`), so
/// the emitted tree keeps that file, at the class's own path, rather
/// than splicing the class into the owner's. Known only when both bodies
/// carry real spans; otherwise the nested class stays in its owner's file.
/// The nested class's files include those that only declare it: Core's
/// `config/test.rb` is `class Config; class Test < Config; end; end`, an
/// empty body the owner's `config.rb` never declares. The owner's are
/// its body's, and each file declaring it that does not declare the
/// nested class: merchant_workflows' `config.rb` is
/// `class Config < ::Component::Config` around Sorbet calls only, an
/// empty body once ingested, and still the owner's file. A file that
/// declares both and adds nothing to the owner only reopens it around
/// the nested class.
///
/// `child`: the nested class's files, its body's and each declaring it.
fn written_apart(child: &crate::det::HashSet<u32>, owner: &str, app: &App) -> bool {
    let mut parent = body_files(owner, app);
    parent.extend(declared_files(owner, app).into_iter().filter(|file| !child.contains(file)));
    !child.is_empty() && !parent.is_empty() && child.is_disjoint(&parent)
}

/// The file stem (`checkouts/one/oversell_protection/inventory_item_allocator`)
/// of a namespace module's constant that the source wrote in a file of
/// its own, named for it the way Zeitwerk maps files to constants:
///
/// ```ruby
/// # oversell_protection/inventory_item_allocator.rb
/// module Checkouts; module One; module OversellProtection
///   InventoryItemAllocator = InventoryItemAllocatorImpl.new(...)
/// ```
///
/// Ingest merges every reopen of the module into one, so the constant
/// arrives among the namespace's own and would be written into the
/// namespace's file. Natively Zeitwerk loads it from its own file when
/// it is first read, and Core's namespace file reads its manager at
/// load (`@manager = ReservationsOversellManager.new`), whose class
/// body reads `InventoryItemAllocator`: in the namespace's file that
/// read required the file already loading, a circular require. So the
/// emitted tree keeps the file, at the constant's own path, and the
/// autoload table maps the constant to it.
///
/// Only for a module (a class's body ends once, in its own file; see
/// `file_owner`) that keeps a file of its own, and only when that
/// source file adds nothing else to the module and the value reads none
/// of the module's other constants, which stay in the namespace's file.
fn constant_file(owner: &str, constant: &str, app: &App) -> Option<String> {
    let classes = library_classes_named(owner, app);
    if classes.is_empty()
        || !classes.iter().all(|c| c.is_module && c.origin.is_none())
        || is_model_named(owner, app)
        || file_owner(owner, app) != owner
        // What the emit adds to a class by name (`extend`s, forwarders)
        // would be written into the constant's file too.
        || app.class_methods_carrier_splices.contains_key(&classes[0].name)
        || app.class_methods_blocks.contains_key(&classes[0].name)
        || app.ruby_declined_delegate_forwarders.contains_key(&classes[0].name)
    {
        return None;
    }
    let qualified = format!("{owner}::{constant}");
    if is_model_or_library_class(&qualified, app) {
        return None;
    }
    let (holder, value) = classes.iter().find_map(|c| {
        c.constants.iter().find(|(n, _)| n.as_str() == constant).map(|(_, v)| (*c, v))
    })?;
    if holder.top_level_constants.iter().any(|t| t.name.as_str() == constant) {
        return None;
    }
    let file = value.span.file.0;
    if file == 0 {
        return None;
    }
    let stem = crate::naming::underscore(&qualified);
    let source = app.sources.get(file as usize - 1)?;
    let source_stem = source.path.strip_suffix(".rb")?;
    if !(source_stem.ends_with(&format!("/{stem}")) || source_stem == stem) {
        return None;
    }
    // Only where an autoloader maps the file to the constant: a Rails
    // autoload root (`app/<x>`, `app/<x>/concerns`) or a Zeitwerk
    // loader's. A gem that `require`s the file loads the constant when
    // it requires it, which the namespace's file, loaded where the
    // require was, already does.
    let dir = relative_source_path(app, source_stem.strip_suffix(stem.as_str())?).trim_matches('/');
    let segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let n = segments.len();
    let autoloaded = (n >= 2 && segments[n - 2] == "app")
        || (n >= 3 && segments[n - 1] == "concerns" && segments[n - 3] == "app")
        || app.zeitwerk_loaders.iter().any(|l| relative_source_path(app, &l.root).trim_matches('/') == dir);
    if !autoloaded {
        return None;
    }
    let mut others: crate::det::HashSet<&str> = crate::det::HashSet::default();
    for c in &classes {
        let from_file = |span: &Span| span.file.0 == file;
        if c.methods.iter().any(|m| from_file(&m.body.span))
            || c.class_ivar_initializers.iter().any(|e| from_file(&e.span))
            || c.unknown_calls.iter().any(|e| from_file(&e.span))
        {
            return None;
        }
        for (n, v) in &c.constants {
            if n.as_str() == constant {
                continue;
            }
            if from_file(&v.span) {
                return None;
            }
            others.insert(n.as_str());
        }
    }
    let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
    walk_const_paths(value, &mut paths);
    let reads_sibling = paths.iter().any(|p| match p.split_last() {
        Some((last, prefix)) => {
            others.contains(last.as_str())
                && (prefix.is_empty() || prefix.join("::").trim_start_matches("::") == owner)
        }
        None => false,
    });
    (!reads_sibling).then_some(stem)
}

/// The emitted file stem (`identity_client/bearer_token_authorization`)
/// of the nested declaration beside which a required source file writes
/// the namespace module `lc`'s `constant`, and whether the source writes
/// the constant above that declaration. identity_client 59.3.0's
/// rails/bearer_token_authorization.rb:
///
/// ```ruby
/// module IdentityClient
///   MARKER_PATTERN = /#{BearerMarkerIdentifier::SUPPORTED_MARKERS.map { … }.join("|")}/
///   module BearerTokenAuthorization
/// ```
///
/// lib/identity_client.rb opens `module IdentityClient`, then requires
/// bearer_marker_identifier.rb before this file. Ingest merges every
/// reopen of the module into one, so the constant was written into the
/// namespace's file, which the entry's opening `module IdentityClient`
/// loads: it read `BearerMarkerIdentifier` before its file loaded
/// (NameError). Requiring the file loads its declarations
/// (`CarriedSources::declarations`), so the constant goes into the
/// first of them when it is written above it, else after the last.
///
/// Only where requiring the file does not load the namespace's own file
/// (another file's program defines the namespace, as the gem's entry
/// does), for a module that keeps a file of its own (as
/// `constant_file`, which places a constant an autoloader maps).
fn nested_file_constant(lc: &LibraryClass, constant: &str, app: &App) -> Option<(String, bool)> {
    let owner = lc.name.0.as_str().trim_start_matches("::");
    let classes = library_classes_named(owner, app);
    if classes.is_empty()
        || !classes.iter().all(|c| c.is_module && c.origin.is_none())
        || is_model_named(owner, app)
        || file_owner(owner, app) != owner
        || app.class_methods_carrier_splices.contains_key(&classes[0].name)
        || app.class_methods_blocks.contains_key(&classes[0].name)
        || app.ruby_declined_delegate_forwarders.contains_key(&classes[0].name)
        || lc.top_level_constants.iter().any(|t| t.name.as_str() == constant)
    {
        return None;
    }
    let value = lc.constants.iter().find(|(n, _)| n.as_str() == constant).map(|(_, v)| v)?;
    let file = value.span.file.0;
    let source = app.sources.get((file as usize).checked_sub(1)?)?;
    let rel = relative_source_path(app, &source.path);
    let carried = carried_sources(app);
    let anchors = carried.declarations.get(rel)?;
    let own = format!("models/{}", super::library_file_stem(owner, app));
    if anchors.contains(&own) {
        return None;
    }
    let prefix = format!("{own}/");
    let nested: Vec<&str> = anchors.iter().filter_map(|a| a.strip_prefix("models/").filter(|_| a.starts_with(&prefix))).collect();
    let (first, last) = (*nested.first()?, *nested.last()?);
    // Where the first of them starts in the file: its earliest span.
    let mut start: Option<u32> = None;
    let names = format!("{owner}::");
    for c in app.library_classes.iter().filter(|c| {
        let name = c.name.0.as_str().trim_start_matches("::");
        name.starts_with(&names) && super::library_file_stem(name, app) == first
    }) {
        let mut see = |span: Span| {
            if span.file.0 == file {
                start = Some(start.map_or(span.start, |s| s.min(span.start)));
            }
        };
        see(c.parent_span);
        c.methods.iter().for_each(|m| see(m.name_span));
        c.constants.iter().for_each(|(_, v)| see(v.span));
        c.unknown_calls.iter().chain(&c.class_ivar_initializers).for_each(|e| see(e.span));
    }
    match start {
        Some(at) if value.span.start < at => Some((first.to_string(), true)),
        _ => Some((last.to_string(), false)),
    }
}

/// The nested declaration (`Metafields::Owners::Registry`) in whose
/// file a namespace module's class-level statement was written, when
/// that is the file an autoloader maps to the declaration and not the
/// namespace's own. Core's metafields/owners/registry.rb:
///
/// ```ruby
/// module Metafields
///   module Owners
///     module Registry … end
///
///     if Rails.application.config.consider_all_requests_local
///       OwnerConfigurationChecks.raise_on_unexpected_empty_access_scopes
/// ```
///
/// Natively the `if` runs when registry.rb loads. Ingest merges every
/// reopen of the module into one, so it was written into the namespace's
/// file and ran at the first `Metafields::Owners::…` reference (another
/// class body's `include Metafields::Owners::AbstractOwner`): the checks
/// loaded the registry early, and its load hook's blocks loaded a model
/// requiring a file still loading, a circular require. The statement is
/// written after the declaration's body in the declaration's file, in a
/// reopening of the namespace, wherever in that file the source wrote it.
///
/// Only for a module that keeps a file of its own (as `constant_file`),
/// and a declaration that keeps its own file (`file_owner`).
fn statement_file(owner: &str, call: &Expr, app: &App) -> Option<String> {
    let file = call.span.file.0;
    if file == 0 {
        return None;
    }
    let classes = library_classes_named(owner, app);
    if classes.is_empty()
        || !classes.iter().all(|c| c.is_module && c.origin.is_none())
        || is_model_named(owner, app)
        || file_owner(owner, app) != owner
    {
        return None;
    }
    let namespace = crate::naming::underscore(owner);
    let source = app.sources.get(file as usize - 1)?;
    let source_stem = source.path.strip_suffix(".rb")?;
    if source_stem == namespace || source_stem.ends_with(&format!("/{namespace}")) {
        return None;
    }
    let at = if source_stem.starts_with(&format!("{namespace}/")) {
        0
    } else {
        source_stem.rfind(&format!("/{namespace}/"))? + 1
    };
    let stem = &source_stem[at..];
    let dir = relative_source_path(app, &source_stem[..at]).trim_matches('/').to_string();
    let segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let n = segments.len();
    let autoloaded = (n >= 2 && segments[n - 2] == "app")
        || (n >= 3 && segments[n - 1] == "concerns" && segments[n - 3] == "app")
        || app.zeitwerk_loaders.iter().any(|l| relative_source_path(app, &l.root).trim_matches('/') == dir);
    if !autoloaded {
        return None;
    }
    let prefix = format!("{owner}::");
    app.library_classes
        .iter()
        .map(|c| c.name.0.as_str().trim_start_matches("::"))
        .filter(|name| name.starts_with(&prefix) && crate::naming::underscore(name) == stem)
        .find(|name| {
            file_owner(name, app) == *name
                && (declared_files(name, app).contains(&file) || body_files(name, app).contains(&file))
        })
        .map(str::to_string)
}

/// The file of a constant `path` names, read from `self_name`, when it
/// is a namespace constant that has a file of its own (`constant_file`):
/// the qualified spelling, or a bare one resolved through the lexical
/// scopes `self_name` sits in, the nearest that defines it.
fn constant_file_for_path(path: &[String], app: &App, self_name: &str) -> Option<String> {
    let (constant, prefix) = path.split_last()?;
    if !constant.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let mut scopes: Vec<String> = Vec::new();
    if !prefix.is_empty() {
        scopes.push(prefix.join("::"));
    }
    let segments: Vec<&str> = if self_name.is_empty() { Vec::new() } else { self_name.split("::").collect() };
    for n in (1..=segments.len()).rev() {
        let scope = segments[..n].join("::");
        scopes.push(if prefix.is_empty() { scope } else { format!("{scope}::{}", prefix.join("::")) });
    }
    for scope in scopes {
        let classes = library_classes_named(&scope, app);
        let defines = is_model_or_library_class(&format!("{scope}::{constant}"), app)
            || classes.iter().any(|c| c.constants.iter().any(|(n, _)| n.as_str() == constant.as_str()));
        if defines {
            return constant_file(&scope, constant, app);
        }
    }
    None
}

/// The `constant_file` stems of `owner`'s constants written in the
/// source file `rel` (relative to the app root): a file program that
/// declares the namespace from that file loads them where it ran them.
fn constant_files_written_in(owner: &str, rel: &str, app: &App) -> Vec<String> {
    let mut out = Vec::new();
    for c in library_classes_named(owner, app) {
        for (name, value) in &c.constants {
            let written_here = value.span.file.0 != 0
                && app
                    .sources
                    .get(value.span.file.0 as usize - 1)
                    .is_some_and(|source| relative_source_path(app, &source.path) == rel);
            if written_here {
                if let Some(stem) = constant_file(owner, name.as_str(), app) {
                    if !out.contains(&stem) {
                        out.push(stem);
                    }
                }
            }
        }
    }
    out
}

/// The namespace module `lc`'s constants that a file program runs, by
/// index, with that program's source file (relative to the app root):
/// those the program writes after a statement of its own. Core's
/// lib/shopify/protobuf/metafield_pb.rb:
///
/// ```ruby
/// pool = ::Google::Protobuf::DescriptorPool.generated_pool
/// pool.add_serialized_file(descriptor_data)
///
/// module Shopify
///   module Protobuf
///     Metafield = ::Google::Protobuf::DescriptorPool.generated_pool.lookup("shopify.protobuf.Metafield").msgclass
/// ```
///
/// Ruby assigns a constant where its file writes it, after the
/// statements above it. Ingest merges every reopen of the module into
/// one, so each `*_pb.rb`'s lookups were written into the namespace's
/// file, which the first program to load it required after adding only
/// its own descriptor: every other lookup answered nil (NoMethodError,
/// `msgclass` for nil). Such a constant is written into its program
/// instead, in a reopening of the module right after the declaration
/// (`program_constants_written_in`).
///
/// Only for a module that keeps a file of its own (as `constant_file`),
/// and not a constant one left in the namespace's file reads at load:
/// that file would read it before its program runs.
fn program_constants(lc: &LibraryClass, app: &App) -> Vec<(usize, String)> {
    use crate::app::ProgramItem;
    let owner = lc.name.0.as_str().trim_start_matches("::");
    if app.file_programs.is_empty() || lc.constants.is_empty() {
        return Vec::new();
    }
    let classes = library_classes_named(owner, app);
    if classes.is_empty()
        || !classes.iter().all(|c| c.is_module && c.origin.is_none())
        || is_model_named(owner, app)
        || file_owner(owner, app) != owner
        || app.class_methods_carrier_splices.contains_key(&classes[0].name)
        || app.class_methods_blocks.contains_key(&classes[0].name)
        || app.ruby_declined_delegate_forwarders.contains_key(&classes[0].name)
    {
        return Vec::new();
    }
    // Each source file whose program runs a statement before declaring
    // the module.
    let mut runs_first: crate::det::HashMap<u32, Option<String>> = crate::det::HashMap::default();
    let mut program_of = |file: u32| -> Option<String> {
        runs_first
            .entry(file)
            .or_insert_with(|| {
                let source = app.sources.get((file as usize).checked_sub(1)?)?;
                let rel = relative_source_path(app, &source.path);
                let program = app.file_programs.iter().find(|p| relative_source_path(app, &p.source) == rel)?;
                let at = program.items.iter().position(|item| {
                    matches!(item, ProgramItem::Declared(names) if names.iter().any(|n| n.0.as_str().trim_start_matches("::") == owner))
                })?;
                program.items[..at]
                    .iter()
                    .any(|item| !matches!(item, ProgramItem::Declared(_) | ProgramItem::Require { .. }))
                    .then(|| rel.to_string())
            })
            .clone()
    };
    let moved: Vec<(usize, String)> = lc
        .constants
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| !lc.top_level_constants.iter().any(|t| t.name.as_str() == n.as_str()))
        .filter(|(_, (n, _))| constant_file(owner, n.as_str(), app).is_none())
        .filter_map(|(j, (_, value))| program_of(value.span.file.0).map(|rel| (j, rel)))
        .collect();
    if moved.is_empty() {
        return moved;
    }
    // A body statement or a constant's own effect the class's file
    // writes (`carries_body_statement`, `gem_source_body_effects`) below
    // a moved constant runs on it, so it must follow it, which the
    // program's reopening cannot keep: the constants stay in the
    // class's file, which the program requires where the declaration
    // stands. paquito's types.rb runs `begin; require "active_support";
    // …; rescue LoadError; end` above `module Paquito`, and below
    // `TYPES = [ … ]` its body runs `begin; require "msgpack/bigint";
    // TYPES << { … class: "Integer" … }; rescue LoadError; end` and
    // `TYPES.freeze`.
    let effects = gem_source_body_effects(lc, app);
    let follows_moved = |call: &Expr| {
        !call.span.is_synthetic()
            && moved.iter().any(|(j, _)| {
                let at = lc.constants[*j].1.span;
                at.file == call.span.file && at.start < call.span.start
            })
    };
    if lc.unknown_calls.iter().any(|c| {
        follows_moved(c)
            && (effects.iter().any(|e| std::ptr::eq(*e, c)) || (is_body_statement(c) && carries_body_statement(lc, c, app)))
    }) {
        return Vec::new();
    }
    let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
    for (j, (_, value)) in lc.constants.iter().enumerate() {
        if !moved.iter().any(|(k, _)| *k == j) {
            walk_const_paths(value, &mut paths);
        }
    }
    let read = |name: &str| {
        paths.iter().any(|p| match p.split_last() {
            Some((last, prefix)) => last == name && (prefix.is_empty() || prefix.join("::").trim_start_matches("::") == owner),
            None => false,
        })
    };
    moved.into_iter().filter(|(j, _)| !read(lc.constants[*j].0.as_str())).collect()
}

/// The namespace module `owner`'s constants the file program of the
/// source `rel` runs (`program_constants`), as a reopening of the
/// module with only them, rendered at the program's `out_path`.
fn program_constants_written_in(owner: &str, rel: &str, app: &App, out_path: &Path) -> Option<String> {
    let classes = library_classes_named(owner, app);
    let first = classes.first()?;
    let written_here = |span: &Span| {
        app.sources
            .get((span.file.0 as usize).wrapping_sub(1))
            .is_some_and(|source| relative_source_path(app, &source.path) == rel)
    };
    if !classes.iter().any(|c| c.constants.iter().any(|(_, v)| written_here(&v.span))) {
        return None;
    }
    let mut constants = Vec::new();
    for c in &classes {
        for (j, from) in program_constants(c, app) {
            if from == rel {
                constants.push(c.constants[j].clone());
            }
        }
    }
    if constants.is_empty() {
        return None;
    }
    let reopen = LibraryClass {
        includes: Vec::new(),
        methods: Vec::new(),
        class_ivar_initializers: Vec::new(),
        nullable_columns: Vec::new(),
        constants,
        unknown_calls: Vec::new(),
        visibility_overrides: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        conditional_definitions: Vec::new(),
        ..(*first).clone()
    };
    Some(emit_library_class_decl(&reopen, app, out_path.to_path_buf()).content)
}

/// Each library class => the ids of the source files that declare it
/// (`App::library_class_files`), a `class X; end` with an empty body
/// included.
fn declaring_files(app: &App) -> HashMap<String, Vec<u32>> {
    let ids: HashMap<&str, u32> =
        app.sources.iter().enumerate().map(|(i, s)| (s.path.as_str(), i as u32 + 1)).collect();
    let mut out: HashMap<String, Vec<u32>> = HashMap::default();
    for (source, names) in &app.library_class_files {
        let Some(&id) = ids.get(source.as_str()) else { continue };
        for name in names {
            let files = out.entry(name.0.as_str().to_string()).or_default();
            if !files.contains(&id) {
                files.push(id);
            }
        }
    }
    out
}

/// The source files that declare a library class, its body or not.
fn declared_files(name: &str, app: &App) -> crate::det::HashSet<u32> {
    match NameIndex::current(app) {
        Some(index) => index.declared_in.get(name).map(|ids| ids.iter().copied().collect()).unwrap_or_default(),
        None => declaring_files(app).remove(name).map(|ids| ids.into_iter().collect()).unwrap_or_default(),
    }
}

/// The source files a library class's own body (methods, constants,
/// class-level calls) was ingested from.
fn body_files(name: &str, app: &App) -> crate::det::HashSet<u32> {
    let mut out = crate::det::HashSet::default();
    let mut add = |span: &crate::span::Span| {
        if span.file.0 != 0 {
            out.insert(span.file.0);
        }
    };
    for c in library_classes_named(name, app) {
        c.methods.iter().filter(|m| !inherited_copy(c, m, app)).for_each(|m| add(&m.body.span));
        c.constants.iter().for_each(|(_, e)| add(&e.span));
        c.class_ivar_initializers.iter().for_each(|e| add(&e.span));
        c.unknown_calls.iter().for_each(|e| add(&e.span));
    }
    out
}

/// Whether `m`, a class-side method of `c`, is a copy of one an
/// ancestor writes (`lower::class_body_new::monomorphize_subclassed`
/// copies an inherited constructing method into each descendant, body
/// span and all): its body was written in the ancestor's file, not in
/// one of `c`'s. Counted as `c`'s, Core's `GraphApi::ObjectType.
/// authorized_new` put object_type.rb among the files of `Home::Feed`
/// and of each card nested under it, so no card was written apart from
/// Feed and all 27 were merged into feed.rb, where `Payment`'s `include
/// IsIconType` read a constant nothing had loaded.
fn inherited_copy(c: &LibraryClass, m: &crate::dialect::MethodDef, app: &App) -> bool {
    if m.receiver != MethodReceiver::Class || m.body.span.file.0 == 0 {
        return false;
    }
    let mut seen: crate::det::HashSet<String> = crate::det::HashSet::default();
    let mut current = c.name.0.as_str().to_string();
    let mut parent = c.parent.clone();
    while let Some(p) = parent {
        let ancestor = lexical_parent(&current, &p, app).0.as_str().trim_start_matches("::").to_string();
        if !seen.insert(ancestor.clone()) {
            return false;
        }
        let classes = library_classes_named(&ancestor, app);
        if classes.is_empty() {
            return false;
        }
        if classes.iter().any(|a| a.methods.iter().any(|am| am.name == m.name && am.receiver == m.receiver && am.body.span == m.body.span)) {
            return true;
        }
        parent = classes.iter().find_map(|a| a.parent.clone());
        current = ancestor;
    }
    false
}

/// A class that can absorb its nested classes: a LIBRARY class, not a
/// model.
///
/// Models are emitted by a different pipeline, so there is nothing for
/// a nested class to be spliced into and it keeps its own file. The
/// emit already knew that; the path resolver did not, and answered
/// "same file, no require needed" for `Account::Joinable` nested in
/// the MODEL `Account` — whose file then referenced a constant nothing
/// had loaded. Both sides ask this now.
fn owns_a_file(name: &str, app: &App) -> bool {
    // A factory class's body is a block, not a `class` header a nested
    // class can be spliced after; those keep their own file and reopen
    // it with its superclass (`class X < Struct`), which Ruby accepts.
    library_classes_named(name, app).iter().any(|c| {
        !c.is_module && !matches!(c.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. }))
    })
}

/// Every constant path the app declares (models, controllers, library
/// classes and modules, test modules) and each namespace enclosing one.
fn app_constant_paths(app: &App) -> std::rc::Rc<crate::det::HashSet<String>> {
    let build = || {
        let mut out: crate::det::HashSet<String> = crate::det::HashSet::default();
        let names = app.models.iter().map(|m| m.name.0.as_str())
            .chain(app.controllers.iter().map(|c| c.name.0.as_str()))
            .chain(app.library_classes.iter().map(|c| c.name.0.as_str()))
            .chain(app.test_modules.iter().map(|t| t.name.0.as_str()));
        for name in names {
            let name = name.trim_start_matches("::");
            let mut end = 0;
            while let Some(at) = name[end..].find("::") {
                out.insert(name[..end + at].to_string());
                end += at + 2;
            }
            out.insert(name.to_string());
        }
        std::rc::Rc::new(out)
    };
    match NameIndex::current(app) {
        Some(index) => index.constant_paths.get_or_init(build).clone(),
        None => build(),
    }
}

/// An app constant `written` as a class header's superclass or a body's
/// mixin must spell it, given the lexical `scopes` (innermost last) Ruby
/// searches first: `::GraphApi::ObjectType` inside `module Channels;
/// module GraphApi`, where a bare `GraphApi` is `Channels::GraphApi`.
/// Core writes the leading `::` (`class ListingAttributeValue <
/// ::GraphApi::ObjectType`); the ingest records the absolute name, and
/// written bare, the class died with `uninitialized constant
/// Channels::GraphApi::ObjectType`. The ingest keeps a mixin's name as
/// written, so a name the innermost shadowing scope does declare in full
/// is that scope's, as Ruby binds it, and stays as written; so does a
/// name the app does not declare.
fn lexically_qualified(written: &str, scopes: &[&str], app: &App) -> String {
    if written.starts_with("::") || scopes.is_empty() {
        return written.to_string();
    }
    let paths = app_constant_paths(app);
    let first = written.split("::").next().unwrap_or(written);
    let Some(scope) = (1..=scopes.len()).rev().map(|i| scopes[..i].join("::")).find(|scope| paths.contains(&format!("{scope}::{first}"))) else {
        return written.to_string();
    };
    if paths.contains(&format!("{scope}::{written}")) || !paths.contains(written) {
        return written.to_string();
    }
    format!("::{written}")
}

/// The helper class of the test file declaring `lc` that is named
/// `qualified`. A test file's helper class nesting another (`class
/// PriceRule; class DiscountCode`) is no app class, but the test file
/// splices `class PriceRule` just above `PriceRule::DiscountCode`, whose
/// outer segment reopens it.
fn test_file_outer_class<'a>(lc: &LibraryClass, qualified: &str, app: &'a App) -> Option<&'a LibraryClass> {
    app.test_modules
        .iter()
        .find(|t| t.inner_classes.iter().any(|c| c.name == lc.name))?
        .inner_classes
        .iter()
        .find(|c| !c.is_module && c.name.0.as_str() == qualified)
}

/// Whether the outer segment `qualified` of `lc`'s name is a class, as
/// the `.rb` header reopens it (`emit_library_class_decl_inner`), so the
/// `.rbs` sidecar declares the same kind: RBS refuses `module Account`
/// beside `class Account < ApplicationRecord` as a duplicated
/// declaration.
pub(super) fn outer_segment_is_class(lc: &LibraryClass, qualified: &str, app: &App) -> bool {
    test_file_outer_class(lc, qualified, app).is_some() || is_app_class(qualified, app)
}

fn is_app_class(name: &str, app: &App) -> bool {
    is_model_named(name, app)
        || is_controller_named(name, app)
        || library_classes_named(name, app).iter().any(|c| !c.is_module)
        || app.test_modules.iter().any(|t| t.name.0.as_str() == name)
}

/// The declared superclass of an app class named by a namespace
/// segment, so a nested file can reopen it with the same parent.
fn outer_class_parent(name: &str, app: &App) -> Option<ClassId> {
    if is_model_named(name, app) {
        if let Some(m) = app.models.iter().find(|m| m.name.0.as_str() == name) {
            return m.parent.clone();
        }
    }
    if is_controller_named(name, app) {
        if let Some(c) = app.controllers.iter().find(|c| c.name.0.as_str() == name) {
            return c.parent.clone();
        }
    }
    if let Some(c) = library_classes_named(name, app).into_iter().find(|c| !c.is_module) {
        return c.parent.clone();
    }
    // A test class nesting another (`PermissionGeneratorTest::
    // ValidateInitialTest`) reopens with the parent its own file
    // declares, after the emit's base rewrite.
    app.test_modules
        .iter()
        .find(|t| t.name.0.as_str() == name)
        .and_then(|t| t.parent.as_ref())
        .map(super::emitted_test_parent)
}

/// The app class a written superclass names, looked up from the child's
/// namespace outward as Ruby does: `PodConnectionConfigProvider` written
/// in `Podding::Database::VmsPodConnectionConfigProvider` is
/// `Podding::Database::PodConnectionConfigProvider`. A name that matches
/// no app class at any level is returned as written (a runtime or gem
/// class).
fn lexical_parent(child: &str, parent: &ClassId, app: &App) -> ClassId {
    let written = parent.0.as_str();
    if written.starts_with("::") {
        return parent.clone();
    }
    let defined = |candidate: &str| is_model_or_library_class(candidate, app);
    let mut scope: Vec<&str> = child.split("::").collect();
    scope.pop();
    while !scope.is_empty() {
        let candidate = format!("{}::{written}", scope.join("::"));
        if defined(&candidate) {
            return ClassId(crate::ident::Symbol::from(candidate.as_str()));
        }
        scope.pop();
    }
    parent.clone()
}

fn require_path_for_parent(parent: &ClassId, app: &App) -> Option<String> {
    let raw = parent.0.as_str();
    if raw == "ActiveRecord::Base" {
        // When the app REOPENS ActiveRecord::Base (lobsters' `q`
        // monkeypatch, emitted as a library class), route the parent
        // require through the reopen file — it requires the runtime
        // itself first, so subclasses see both the framework methods
        // and the app's additions. Without this the reopen dangles:
        // nothing else in the require graph names it.
        if !library_classes_named("ActiveRecord::Base", app).is_empty() {
            return Some("app/models/active_record/base".to_string());
        }
        return Some("runtime/active_record".to_string());
    }
    if raw == "ActionController::Base" || raw == "ActionController::API" {
        return Some("runtime/action_controller".to_string());
    }
    if raw == "ActionMailer::Base" {
        return Some("runtime/action_mailer".to_string());
    }
    if raw == "ActiveJob::Base" {
        return Some("runtime/active_job".to_string());
    }
    // `ApplicationCable::Channel < ActionCable::Channel::Base` and its
    // Connection twin — the two roots of an ingested `app/channels/`
    // tree. Both are a SUPERCLASS reference, which is load-time, so a
    // missing require is not a lazy failure: the file does not define.
    if raw == "ActionCable::Channel::Base" || raw == "ActionCable::Connection::Base" {
        return Some("runtime/action_cable".to_string());
    }
    // `LogScrubbingFormatter < ::Logger::Formatter` — campfire's, and
    // the same load-time rule the two cable roots above state. The
    // leading `::` is part of the spelling an app writes, so both
    // forms resolve.
    if raw == "Logger::Formatter" || raw == "::Logger::Formatter" {
        return Some("runtime/logger".to_string());
    }
    if is_model_or_library_class(raw, app) {
        // underscore: namespaced parents nest (see emit_library_class_decls).
        // A library class nested in another is written in that class's
        // file (`file_owner`): `Top < Outer::Guard` requires `outer`,
        // where no `outer/guard` file exists. A model keeps its own.
        let file = if is_model_named(raw, app) { raw.to_string() } else { file_owner(raw, app) };
        return Some(format!("app/models/{}", crate::naming::underscore(&file)));
    }
    if is_controller_named(raw, app) {
        return Some(format!("app/controllers/{}", crate::naming::underscore(raw)));
    }
    // A SUPERCLASS that is a gem's class — lobsters' `TimeSeries <
    // SVG::Graph::TimeSeries`, campfire's `ApplicationPlatform <
    // PlatformAgent`. Both used to emit no require at all and load only
    // because boot.rb happened to have pulled the gem in first; a body
    // reached from the test harness, which builds its own require
    // chain, saw an undefined superclass and the file did not define.
    // `PlatformAgent` is the same shape and no longer a façade:
    // `runtime/ruby/user_agent.rb` PORTS it, so the anchor is that file
    // rather than the façade host. It needs its own arm precisely
    // because this resolver reads `is_gem_facade_root` and nothing
    // else — dropping the pair from that list to port them took the
    // anchor with it, and campfire's ruby lane went 256/288 to 0/288 on
    // `uninitialized constant PlatformAgent` at load. The spinel lane
    // stayed green throughout: its `main.rb` requires every runtime
    // file eagerly, so the constant happened to exist by the time
    // `application_platform.rb` ran. Exactly the accident the comment
    // above says this arm exists to stop.
    if raw == "PlatformAgent" || raw == "UserAgent" {
        return Some("runtime/user_agent".to_string());
    }
    if let Some(root) = raw.split("::").next() {
        if is_gem_facade_root(root) {
            return Some("runtime/gem_facades".to_string());
        }
    }
    None
}

/// Roots hosted by `runtime/ruby/gem_facades.rb` — the one file that
/// stands in for every stubbed gem, so all their roots anchor there.
/// On a ruby-family tree `project.rs` rewrites that file into the
/// guarded-require block, and the same anchor loads the real gems.
///
/// `OpenTelemetry` is the one name here that is not a stubbed GEM — its
/// façade never stands aside for a real `opentelemetry-api`/`-sdk` (see
/// `runtime/ruby/open_telemetry_facade.rb`) — but it is still hosted off
/// this same anchor file (`gem_facades.rb`'s own `require_relative`, and
/// the per-flavor rewrite's matching one), so a body naming it resolves
/// the same way.
fn is_gem_facade_root(root: &str) -> bool {
    matches!(
        root,
        "Markly"
            | "Nokogiri"
            | "Mail"
            | "ROTP"
            | "BCrypt"
            | "RQRCode"
            | "SVG"
            | "OpenTelemetry"
    )
}

/// Core Ruby classes an app may reopen (monkeypatch) without them being
/// `app/models` files. Kept in sync with `rbs::is_builtin_class_name`.
fn is_core_class_name(name: &str) -> bool {
    matches!(
        name,
        "Integer"
            | "Float"
            | "String"
            | "Symbol"
            | "TrueClass"
            | "FalseClass"
            | "NilClass"
            | "Array"
            | "Hash"
            | "Object"
            | "Numeric"
            | "Comparable"
            | "Enumerable"
            | "Kernel"
    )
}

/// Project-root-anchored require target for a body-referenced constant.
/// `Views::<Plural>` resolves to `app/views/<plural>/_<singular>`; runtime
/// modules resolve to `runtime/<x>`. The caller relpaths the result against
/// the requirer's `out_dir`, so a single mapping serves every output kind.
/// Same-dir siblings (other models, library_classes) drop because Ruby's
/// load path covers them; unknowns drop silently.
/// Whether `nested`, a class with a file of its own, is a subclass of
/// the class `outer` it is nested in, which on the Ruby family loads by
/// its autoload where `outer`'s body reads it (Zeitwerk's order: the
/// outer class is defined by then), not by a require above `outer`'s
/// class: Core's `Markets::Region` body reads `Markets::Region::Country`
/// (`< Markets::Region`), whose file needs region.rb's class first.
fn autoloads_as_nested_subclass(nested: &str, outer: &str, app: &App) -> bool {
    if outer.is_empty() || !super::expr::native_keywords() || !nested.strip_prefix(outer).is_some_and(|rest| rest.starts_with("::")) {
        return false;
    }
    let classes = library_classes_named(nested, app);
    let parents = app
        .models
        .iter()
        .filter(|m| m.name.0.as_str() == nested)
        .filter_map(|m| m.parent.as_ref())
        .chain(classes.iter().filter_map(|lc| lc.parent.as_ref()));
    parents.into_iter().any(|parent| {
        let parent = parent.0.as_str().trim_start_matches("::");
        parent == outer || outer.ends_with(&format!("::{parent}"))
    })
}

fn require_path_for_body_const(
    path: &[String],
    app: &App,
    self_name: &str,
) -> Option<String> {
    // A ROOTED head (`::Broadcasts`, `::Sound::Image`) names the same
    // file as the bare one — the marker says where Ruby should look, not
    // what to load. Strip it before resolving, or rooting a reference
    // silently deletes its require and the constant is undefined at load
    // for a different reason than the one rooting fixed.
    // Ingest marks the root either way: as a `::X` head or as an empty
    // leading segment (`::Logger::Formatter` → ["", "Logger", "Formatter"]).
    let path = match path.split_first() {
        Some((head, rest)) if head.is_empty() => rest,
        _ => path,
    };
    let rooted;
    let path: &[String] = match path.first().and_then(|f| f.strip_prefix("::")) {
        Some(bare) => {
            rooted = std::iter::once(bare.to_string())
                .chain(path.iter().skip(1).cloned())
                .collect::<Vec<_>>();
            &rooted
        }
        None => path,
    };
    let first = path.first()?;
    // A namespace constant written in a file of its own is emitted
    // there (`constant_file`), not in the namespace's file.
    if let Some(stem) = constant_file_for_path(path, app, self_name) {
        return Some(format!("app/models/{stem}"));
    }
    // A nested class of THIS class is still a different file:
    // `Sound::Image` lives at app/models/sound/image.rb even though the
    // reference's first segment is `Sound`. Resolve the full path before
    // the self-reference short-circuit below.
    let joined = path.join("::");
    if joined != self_name && is_model_or_library_class(&joined, app) {
        // The file that DEFINES it, which for a nested class is its
        // parent's — and when that is the file being emitted, there is
        // nothing to require: the class is already in it.
        let owner = file_owner(&joined, app);
        if owner == self_name || (owner == joined && autoloads_as_nested_subclass(&joined, self_name, app)) {
            return None;
        }
        return Some(format!("app/models/{}", crate::naming::underscore(&owner)));
    }
    // A bare reference inside a namespace resolves LEXICALLY first:
    // campfire's `module ContentFilters` builds
    // `TextMessagePresentationFilters = …new(RemoveSoloUnfurledLinkText,
    // …)` at LOAD time, and that name is
    // `ContentFilters::RemoveSoloUnfurledLinkText` — a sibling file that
    // the aggregator requires AFTER this one (`x.rb` sorts before
    // `x/y.rb`). Without the require the constant is uninitialized and
    // the whole app fails to load, so resolve the qualified form before
    // giving up on the bare one.
    let lexical = format!("{self_name}::{joined}");
    if !self_name.is_empty() && is_model_or_library_class(&lexical, app) {
        let owner = file_owner(&lexical, app);
        if owner == self_name {
            return None;
        }
        return Some(format!("app/models/{}", crate::naming::underscore(&owner)));
    }
    // The enclosing namespaces are scopes too: `Change::ADD` inside
    // `module Sales; module Query; module Changes` is `Sales::Change::ADD`
    // when `Sales::Change` is a class, as Ruby's lexical lookup finds it
    // (the innermost scope that has the head segment wins). The file of
    // the longest class prefix of the path under that scope.
    let scopes: Vec<&str> = self_name.match_indices("::").map(|(i, _)| &self_name[..i]).chain(Some(self_name)).collect();
    for scope in scopes.into_iter().rev().filter(|scope| !scope.is_empty()) {
        if !is_model_or_library_class(&format!("{scope}::{first}"), app) {
            continue;
        }
        for n in (1..=path.len()).rev() {
            let prefix = format!("{scope}::{}", path[..n].join("::"));
            if is_model_or_library_class(&prefix, app) {
                let owner = file_owner(&prefix, app);
                if owner == self_name {
                    return None;
                }
                return Some(format!("app/models/{}", crate::naming::underscore(&owner)));
            }
        }
    }
    // A qualified constant (`SecureHeaders::ClearSiteData::CACHE`,
    // `SecureHeaders::PolicyManagement::ALL_DIRECTIVES`) is defined by
    // the file of its longest class prefix: this file when that is this
    // class (or one this file defines), else that class's file. Resolved
    // by its first segment it required the namespace's file, whose body
    // lists the nested classes and so requires their files up front: a
    // cycle native load order does not have (secure_headers.rb requires
    // headers/*, which require nothing back), and Ruby's circular-require
    // warning, which Core's StrictWarning raises.
    for n in (2..=path.len()).rev() {
        let prefix = path[..n].join("::");
        if !self_name.is_empty() && prefix == self_name {
            return None;
        }
        if is_model_or_library_class(&prefix, app) {
            let owner = file_owner(&prefix, app);
            if owner == self_name {
                return None;
            }
            return Some(format!("app/models/{}", crate::naming::underscore(&owner)));
        }
    }
    // Any other constant under this class is in this file. Only after
    // the prefixes: a model's nested module keeps a file of its own
    // (`file_owner`), and Core's `Event` body reads
    // `Verb::CONFIRMED` (`Event::Verb::CONFIRMED`) into a constant as
    // it loads, which raised NameError with no require of event/verb.rb.
    if first == self_name {
        return None;
    }
    // A core Ruby class an app reopens (e.g. `class String` in
    // lib/monkey.rb) lands in `library_classes`, but it is NOT an
    // `app/models/<name>` file — every `String.new` would otherwise emit
    // a dangling `require_relative "app/models/string"`. The reference
    // resolves to the builtin; any monkeypatch is loaded via its own file
    // (lib/), not through this model anchor.
    if is_core_class_name(first) {
        return None;
    }
    if is_model_or_library_class(first, app) {
        return Some(format!("app/models/{}", crate::naming::underscore(first)));
    }
    if is_controller_named(first, app) {
        return Some(format!("app/controllers/{}", crate::naming::underscore(first)));
    }
    // Qualified runtime names, resolved on the FULL path — `ActiveSupport`
    // is a namespace we ship several unrelated pieces of, so anchoring on
    // its root segment would over-require. A fixture loader is the first
    // body to need this: `created_at: <%= 1.hour.ago %>` grounds to
    // `ActiveSupport::Duration.hour(1)` and `test/fixtures/<x>.rb` is
    // reached from the test harness, not from main.rb's require chain.
    if joined == "ActiveSupport::SecurityUtils" {
        return Some("runtime/security_utils".to_string());
    }
    if joined == "ActiveSupport::Duration" {
        return Some("runtime/active_support_duration".to_string());
    }
    match first.as_str() {
        // `Views::*` refs always go through the per-app aggregator at
        // `app/views.rb` (spinel-blog convention; loads all view
        // modules so any `Views::X.method` resolves regardless of
        // which template the method lives in). Per-template requires
        // would be wrong because the same `Views::X` const can host
        // methods from multiple files (`_article.rb`, `index.rb`,
        // `show.rb` all re-open `module Views::Articles`).
        "Views" => Some("app/views".to_string()),
        // Runtime modules under `runtime/`. ViewHelpers still ships
        // hand-written; RouteHelpers is now generated into
        // `app/route_helpers.rb` from `app.routes` so consumers
        // resolve there. Add entries as lowerings introduce new ones;
        // unknown idents silently drop.
        "Broadcasts" => Some("runtime/broadcasts".to_string()),
        // `ActionCable.server.broadcast(stream, payload)` — the
        // low-level publish API, and the two bases `app/channels/*.rb`
        // subclasses. Sibling of Broadcasts, not a replacement: the
        // Turbo Stream family still goes through `Broadcasts.append`.
        "ActionCable" => Some("runtime/action_cable".to_string()),
        "Logger" => Some("runtime/logger".to_string()),
        // `ActionText::Content` / `ActionText::Attachment` — the value
        // half of Action Text. `ActionText::RichText` is NOT here: it
        // is a lowered model and resolves under `app/models/` like any
        // other, which is exactly the split this entry preserves.
        "ActionText" => Some("runtime/action_text".to_string()),
        // `ActiveStorage::Attached` — the value half, what a
        // `has_one_attached` reader returns. `ActiveStorage::Attachment`
        // is NOT here for the same reason `ActionText::RichText` is
        // not: it is a lowered model under `app/models/`.
        "ActiveStorage" => Some("runtime/active_storage".to_string()),
        // `Turbo::Streams::StreamName::ClassMethods`, INCLUDED by a
        // channel that guards its own stream (campfire's
        // `RoomMessagesChannel`). An include runs at class-definition
        // time, so a missing anchor here is a tree that does not boot.
        "Turbo" => Some("runtime/turbo_streams".to_string()),
        "Inflector" => Some("runtime/inflector".to_string()),
        // `IPAddr` — ported into `runtime/ruby/ipaddr.rb` because the
        // strict targets have no stdlib to reach for. Anchored for
        // every target, ruby family included: `ipaddr` is not in
        // `project::BUNDLED`, so nothing inserts a bare
        // `require "ipaddr"` that would reopen this class with the
        // stdlib's own `initialize` and leave the predicates reading a
        // slot nobody filled. One IPAddr per tree: ours on the strict
        // targets, and on the ruby family the stdlib's under the same
        // path with the port's `octets` reopened onto it
        // (`project::IPADDR_STDLIB`) — so runtime code reads octets
        // everywhere and never `to_i`.
        "IPAddr" => Some("runtime/ipaddr".to_string()),
        // `Rack::Utils` — rack's encoding negotiation, ported into
        // `runtime/ruby/rack_utils.rb` for spinel and the rack gem's own
        // on the ruby family (`project::ruby_runtime_files`). Anchored so
        // a file that names it loads the one its tree has.
        "Rack" => Some("runtime/rack_utils".to_string()),
        // `SQLite3::Database` — the gem on the ruby family, the FFI port
        // (`runtime/spinel/sqlite3_database.rb`) on spinel; one require
        // path either way.
        "SQLite3" => Some("runtime/sqlite3_database".to_string()),
        // `Resolv` — Ruby's resolver, ported into
        // `runtime/ruby/resolv.rb` for the targets that have none and
        // swapped for the stdlib on the ruby family. Anchored for the
        // same reason IPAddr is: `resolv` is not in `project::BUNDLED`,
        // so nothing inserts a bare `require "resolv"` of its own.
        "Resolv" => Some("runtime/resolv".to_string()),
        // `Tempfile` — anchored for the same reason IPAddr and Resolv
        // are, and it was MISSING: the port landed in `spinel_files`
        // with no entry here, so nothing in any emitted tree ever
        // required `runtime/tempfile` and the file shipped dead. The
        // interpreted lane hid it — under CRuby something in the bundle
        // loads the stdlib's `tempfile` on its own, so the constant
        // resolved without the anchor — while on spinel the name went
        // into inference unresolved, which is a failure far from its
        // cause rather than a missing require. matz/spinel#4811 added
        // `Tempfile` to spinel's own missing-require table and the
        // diagnostic named it at campfire's
        // `vips_loader_policy_test.rb:163`.
        "Tempfile" => Some("runtime/tempfile".to_string()),
        // `Net::HTTP` — a real client on both lanes (`project::BUNDLED`
        // writes the `require "net/http"`), REOPENED on spinel by
        // `runtime/spinel/net_http.rb` to add the block form of
        // `#request` and the stub seam `lower::webmock` fills. Anchored
        // so every file that names the client loads the reopen; the ruby
        // family's copy of the file is a bare `require "net/http"`.
        "Net" => Some("runtime/net_http".to_string()),
        // `WebPush` — the web-push gem. On spinel, runtime/spinel/
        // web_push.rb: the façade's stub slot plus a port of the gem's
        // delivery over the openssl package. On the ruby family the
        // file is the façade alone, which guarded-requires the real
        // gem. Anchored rather than left to load order: campfire's
        // `WebPush::Notification` names the module without requiring
        // anything, and the port is what defines `WebPush::Request`,
        // the class its initializer prepends the SSRF guard onto.
        "WebPush" => Some("runtime/web_push".to_string()),
        // `Surfguard` — basecamp/surfguard's address policy, ported into
        // `runtime/ruby/surfguard.rb`. Anchored for every target: the
        // gem is a GIT dependency, so no `gem install` and no bare
        // `require` reaches it on the ruby family either. campfire's
        // `RestrictedHTTP::PrivateNetworkGuard` is a two-line shim over
        // it, and `Push::Subscription` names `Surfguard::Unresolvable`
        // in a rescue clause — a NameError there took the whole suite
        // to 0 of 288, because fixtures save through the validation
        // that reaches it.
        "Surfguard" => Some("runtime/surfguard".to_string()),
        // `Zlib` — ported into `runtime/ruby/zlib.rb`. Anchored for
        // every target for the same reason IPAddr is: `zlib` is not in
        // `project::BUNDLED`, so nothing writes a bare `require "zlib"`
        // and the constant would resolve to nothing at all.
        "Zlib" => Some("runtime/zlib".to_string()),
        // `Timeout` — ported into `runtime/ruby/timeout.rb` for spinel,
        // swapped for Ruby's own on the ruby family (`require
        // "timeout"`). Anchored for the same reason Zlib is: on spinel
        // nothing else loads the port, and campfire's unfurl controller
        // names the constant without a require.
        "Timeout" => Some("runtime/timeout".to_string()),
        // `TypeID` — ported into `runtime/ruby/typeid.rb`, swapped for the
        // gem on the ruby family. Anchored so the model that names it
        // (lobsters' Token concern, spliced into 18 models) loads it.
        "TypeID" => Some("runtime/typeid".to_string()),
        // `Concurrent` — concurrent-ruby's pools and barrier, ported into
        // `runtime/spinel/concurrent.rb` over spinel's own threads and
        // swapped for the gem on the ruby family. Anchored for the same
        // reason Zlib is: nothing else writes a require for it.
        "Concurrent" => Some("runtime/concurrent".to_string()),
        // `Mime::Type` — actionpack's registry, ported into
        // `runtime/ruby/mime.rb`. Anchored for every target: the
        // constant is actionpack's, not the stdlib's, so no bare
        // `require` reaches it on the ruby family either.
        "Mime" => Some("runtime/mime".to_string()),
        // `I18n` — the i18n gem's locale accessors, in
        // `runtime/ruby/i18n_locale.rb` (not `i18n.rb`, which would shadow the
        // gem wherever `runtime/ruby` is on the load path). No Rails is
        // loaded on these trees, so nothing else defines the constant.
        "I18n" => Some("runtime/i18n_locale".to_string()),
        // `useragent` + `platform_agent`, PORTED into
        // `runtime/ruby/user_agent.rb` — they were façades that raised
        // until campfire's room page turned out to render all three PWA
        // partials, each of which asks `ApplicationPlatform` what
        // browser it is. Anchored for every target, and it has to be
        // anchored rather than lazily required: campfire's
        // `ApplicationPlatform < PlatformAgent` names its superclass at
        // LOAD time, so a missing anchor is not a lazy failure — the
        // tree does not boot.
        "UserAgent" | "UserAgentToken" | "PlatformAgent" => {
            Some("runtime/user_agent".to_string())
        }
        "ViewHelpers" => Some("runtime/action_view".to_string()),
        "RouteHelpers" => Some("app/route_helpers".to_string()),
        // Gem façades — typed raising stand-ins for write-path-only
        // gem surface (see runtime/ruby/gem_facades.rb). One file
        // hosts every stubbed gem, so all their roots anchor here.
        _ if is_gem_facade_root(first.as_str()) => Some("runtime/gem_facades".to_string()),
        _ => None,
    }
}

/// Compute a `require_relative`-style relative path from `from_dir` to
/// the project-root-anchored `to_anchor`. Both inputs are slash-separated;
/// the result has no `.rb` extension because `require_relative` doesn't
/// need one.
fn relpath(from_dir: &Path, to_anchor: &str) -> String {
    let from_parts: Vec<&str> = from_dir
        .to_str()
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let to_parts: Vec<&str> = to_anchor.split('/').filter(|s| !s.is_empty()).collect();
    // The anchor's LAST segment is a file, never a shared directory:
    // `app/models/content_filters/sanitize_tags.rb` requiring its own
    // namespace module `app/models/content_filters` matched every
    // segment and wrote `require_relative ""` instead of
    // "../content_filters".
    let common = from_parts
        .iter()
        .zip(&to_parts[..to_parts.len().saturating_sub(1)])
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_parts.len() - common;
    let mut parts: Vec<&str> = std::iter::repeat("..").take(ups).collect();
    parts.extend(&to_parts[common..]);
    parts.join("/")
}

/// The tree-root anchor (`app/models/x`, no `.rb`) that a
/// `require_relative` of `rel` names from a file in `from_dir`.
fn anchor_of_relative(from_dir: &Path, rel: &str) -> String {
    let mut parts: Vec<&str> = from_dir.to_str().unwrap_or("").split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// A `require_relative "x"` line, guarded or not (`require_line`), as
/// `(target, rest)`: `rest` is whatever follows the closing quote
/// (` unless defined?(::X)`).
fn split_require_relative(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("require_relative \"")?;
    let end = rest.find('"')?;
    Some((&rest[..end], &rest[end + 1..]))
}

/// The line requiring `rel` from the file `self_anchor` in `out_dir`,
/// for a load-time need of that file: a superclass, an include or
/// extend, a class-body call, a constant's value.
///
/// On the Ruby family, a require that can reach a file still loading is
/// guarded on the constant the required file opens
/// (`require_guard_owners`), because that is how Zeitwerk resolves the
/// reference. A constant that is already defined needs no load, even
/// when its file is still running its body. A constant whose autoload is
/// registered loads when the reference runs. An unguarded require of a
/// file that is still loading is a circular require. Native load order
/// does not have one, and Core's StrictWarning raises on it. A require
/// can reach a loading file in two ways (`waits_on_its_constant`), and
/// Core has a case of each:
/// - `Pricing::DiscountFunctionApi::APIS` lists the four discount API
///   classes, and each of them does `extend Pricing::DiscountFunctionApi`.
/// - payments_api.rb ends in `ActiveSupport.run_load_hooks(:payments_api,
///   …)`. A hook loads `Checkouts::One::ActionFinished`, whose
///   `extend PaymentsApi::PostOffsiteRedirectCallbackInterface` required
///   payments_api.rb back.
///
/// Any other require stays a plain line, which the project's passes then
/// order (the namespace's file first) and route through a file program.
/// So does a constant that is neither defined nor autoloaded, such as a
/// plain gem's, and every require on the strict targets, which splice a
/// `require_relative` where it stands.
///
/// `defined?(::A::B)` loads `A` when `A` still autoloads. Natively the
/// body loads it at the reference, after the class header has defined
/// the requiring class. A namespace that runs load hooks is therefore
/// checked first, without loading it (`hook_namespace_pending`). While
/// its autoload is pending, the body's reference loads it, and the hook
/// it runs finds the requiring class defined.
///
/// A file nested under the required one's path (`markets/region/country`
/// requiring `markets/region`: its superclass, or the class its header
/// reopens) skips the require while that file is loading
/// (`RoundhouseAutoload.loading?`): natively the enclosing class's file
/// is the one loading first, and a nested class's file never requires it
/// back. Core's `Markets::Region` reads `Markets::Region::Country.name`
/// in its body; country.rb's require of region.rb was then a circular
/// require (Ruby's no-op, with the warning StrictWarning raises).
fn require_line(out_dir: &Path, rel: &str, self_anchor: &str, app: &App) -> String {
    if super::expr::native_keywords() {
        let anchor = anchor_of_relative(out_dir, rel);
        let mut condition = Vec::new();
        if let Some(Some(owner)) = require_guard_owners(app).get(&anchor) {
            if waits_on_its_constant(self_anchor, &anchor, owner, app) {
                condition = hook_namespace_pending(owner, app);
                condition.push(format!("defined?(::{owner})"));
            }
        }
        if self_anchor.strip_prefix(anchor.as_str()).is_some_and(|rest| rest.starts_with('/')) {
            let file = anchor.strip_prefix("app/").unwrap_or(&anchor);
            condition.push(format!("(defined?(RoundhouseAutoload) && RoundhouseAutoload.loading?({file:?}))"));
        }
        if !condition.is_empty() {
            return format!("require_relative {rel:?} unless {}", condition.join(" || "));
        }
    }
    format!("require_relative {rel:?}")
}

/// The clauses that are true while a namespace of `owner`, down to the
/// innermost one whose file runs load hooks (`App::class_load_hooks`),
/// still autoloads. None of them loads anything: `Object.autoload?(:A,
/// false)`, then `(defined?(::A) && ::A.autoload?(:B, false))`, and so
/// on. Empty when no namespace of `owner` runs hooks.
fn hook_namespace_pending(owner: &str, app: &App) -> Vec<String> {
    let segments: Vec<&str> = owner.split("::").collect();
    let depth = app
        .class_load_hooks
        .iter()
        .map(|hook| hook.class.trim_start_matches("::"))
        .filter(|class| *class == owner || owner.strip_prefix(class).is_some_and(|rest| rest.starts_with("::")))
        .map(|class| class.split("::").count())
        .max()
        .unwrap_or(0);
    (0..depth)
        .map(|j| match j {
            0 => format!("Object.autoload?(:{}, false)", segments[0]),
            _ => {
                let outer = segments[..j].join("::");
                format!("(defined?(::{outer}) && ::{outer}.autoload?(:{}, false))", segments[j])
            }
        })
        .collect()
}

/// Whether the load-time require from `self_anchor` of `anchor` (the file
/// of `owner`) can reach a file that is still loading (`require_line`):
/// - the two files are in one load-time require cycle
///   (`load_cycle_components`);
/// - `owner`, or a namespace enclosing it, is a class whose file ends in
///   `ActiveSupport.run_load_hooks` (`App::class_load_hooks`). Loading
///   `owner` loads that namespace's file first, and the hooks it runs can
///   load any file, including the one requiring `owner`.
fn waits_on_its_constant(self_anchor: &str, anchor: &str, owner: &str, app: &App) -> bool {
    let cycles = load_cycle_components(app);
    if cycles.get(self_anchor).is_some_and(|c| cycles.get(anchor) == Some(c)) {
        return true;
    }
    app.class_load_hooks.iter().any(|hook| {
        let class = hook.class.trim_start_matches("::");
        owner == class || owner.strip_prefix(class).is_some_and(|rest| rest.starts_with("::"))
    })
}

/// The library class files (`require_guard_owners`) in a load-time
/// require cycle, each mapped to its cycle: the strongly connected
/// components of more than one file, over what each class reads while
/// its file loads. That is its superclass, its includes, its class-body
/// calls, its constants' values, its class-variable initializers (none
/// of them the closures they only store, `walk_load_time_paths`) and
/// the method bodies those run at load (`load_time_methods`). Any
/// other method body is read later. Each read also reads the namespaces
/// it goes through, and the header reopens the enclosing ones
/// (`namespace_prefixed`): both run those namespaces' autoloads.
fn load_cycle_components(app: &App) -> std::rc::Rc<HashMap<String, usize>> {
    let build = || {
        let files = require_guard_owners(app);
        let mut ids: HashMap<String, usize> = HashMap::default();
        let mut anchors: Vec<String> = Vec::new();
        let mut edges: Vec<BTreeSet<usize>> = Vec::new();
        let mut node = |anchor: &str, ids: &mut HashMap<String, usize>, edges: &mut Vec<BTreeSet<usize>>| -> usize {
            if let Some(&id) = ids.get(anchor) {
                return id;
            }
            ids.insert(anchor.to_string(), anchors.len());
            anchors.push(anchor.to_string());
            edges.push(BTreeSet::new());
            anchors.len() - 1
        };
        let split = |id: &ClassId| -> Vec<String> {
            id.0.as_str().trim_start_matches("::").split("::").map(str::to_string).collect()
        };
        for lc in &app.library_classes {
            let name = lc.name.0.as_str();
            let from = format!("app/models/{}", crate::naming::underscore(&file_owner(name, app)));
            if !files.contains_key(&from) {
                continue;
            }
            let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
            let mut later: BTreeSet<Vec<String>> = BTreeSet::new();
            paths.extend(lc.parent.iter().map(split));
            paths.extend(lc.includes.iter().map(split));
            for call in &lc.unknown_calls {
                walk_load_time_paths(call, &mut paths, &mut later);
            }
            for (_, value) in &lc.constants {
                walk_load_time_paths(value, &mut paths, &mut later);
            }
            for init in &lc.class_ivar_initializers {
                walk_load_time_paths(init, &mut paths, &mut later);
            }
            if !lc.constants.is_empty() || !lc.class_ivar_initializers.is_empty() {
                let (_, deferred, initializers_call_self) = partition_deferred_constants(lc, app);
                let ordered = initializers_call_self && ordered_class_state(lc, app);
                let run = load_time_methods(lc, app, &deferred, ordered, initializers_call_self);
                for (i, m) in lc.methods.iter().enumerate() {
                    if run.as_ref().is_none_or(|run| run.contains(&i)) {
                        walk_load_time_paths(&m.body, &mut paths, &mut later);
                    }
                }
            }
            // The header reopens each enclosing namespace (`module Plat;
            // module Artifact` around `module EmptyCheckable`), which
            // runs that namespace's autoload when its file has not run.
            let segments = split(&lc.name);
            paths.extend((1..segments.len()).map(|n| segments[..n].to_vec()));
            for path in namespace_prefixed(&paths) {
                let Some(to) = require_path_for_body_const(&path, app, name) else { continue };
                if to != from && files.contains_key(&to) {
                    let from_id = node(&from, &mut ids, &mut edges);
                    let to_id = node(&to, &mut ids, &mut edges);
                    edges[from_id].insert(to_id);
                }
            }
        }
        // A model's file reads its superclass and what its body runs at
        // load: its `include`s and the other statements carried as
        // written. Core's payments/vault models each include
        // LegacyPaymentInstrument, whose `CLASS_MAPPING` reads them back.
        for model in &app.models {
            let name = model.name.0.as_str();
            let from = format!("app/models/{}", crate::naming::underscore(&file_owner(name, app)));
            if !files.contains_key(&from) {
                continue;
            }
            let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
            let mut later: BTreeSet<Vec<String>> = BTreeSet::new();
            paths.extend(model.parent.iter().map(split));
            for item in &model.body {
                if let crate::dialect::ModelBodyItem::Unknown { expr, .. } | crate::dialect::ModelBodyItem::ClassBody { expr, .. } = item {
                    walk_load_time_paths(expr, &mut paths, &mut later);
                }
            }
            let segments = split(&model.name);
            paths.extend((1..segments.len()).map(|n| segments[..n].to_vec()));
            for path in namespace_prefixed(&paths) {
                let Some(to) = require_path_for_body_const(&path, app, name) else { continue };
                if to != from && files.contains_key(&to) {
                    let from_id = node(&from, &mut ids, &mut edges);
                    let to_id = node(&to, &mut ids, &mut edges);
                    edges[from_id].insert(to_id);
                }
            }
        }
        let edges: Vec<Vec<usize>> = edges.into_iter().map(|e| e.into_iter().collect()).collect();
        let component = strongly_connected(&edges);
        let mut sizes: HashMap<usize, usize> = HashMap::default();
        for c in &component {
            *sizes.entry(*c).or_default() += 1;
        }
        let out: HashMap<String, usize> = ids
            .into_iter()
            .filter(|(_, id)| sizes.get(&component[*id]).is_some_and(|n| *n > 1))
            .map(|(anchor, id)| (anchor, component[id]))
            .collect();
        std::rc::Rc::new(out)
    };
    match NameIndex::current(app) {
        Some(index) => index.load_cycles.get_or_init(build).clone(),
        None => build(),
    }
}

/// Each constant path read at load, with the namespaces it is read
/// through: Ruby resolves `::Caps::Abstract` by looking up `Caps` first,
/// which runs `Caps`'s autoload, so caps.rb loads before caps/abstract.rb
/// and is as much a load-time need of the reading file.
fn namespace_prefixed(paths: &BTreeSet<Vec<String>>) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    for path in paths {
        let skip = path.first().is_some_and(|head| head.is_empty()) as usize;
        for n in (skip + 1)..=path.len() {
            out.insert(path[..n].to_vec());
        }
    }
    out
}

/// Tarjan's strongly connected components of the graph whose node `v`
/// has the out-edges `edges[v]`, iteratively: each node's component.
pub(crate) fn strongly_connected(edges: &[Vec<usize>]) -> Vec<usize> {
    const UNSEEN: usize = usize::MAX;
    let n = edges.len();
    let (mut index, mut low, mut component) = (vec![UNSEEN; n], vec![0; n], vec![UNSEEN; n]);
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let (mut next, mut components) = (0, 0);
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(v, i)) = work.last() {
            if let Some(&w) = edges[v].get(i) {
                work.last_mut().unwrap().1 += 1;
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component[w] = components;
                    if w == v {
                        break;
                    }
                }
                components += 1;
            }
        }
    }
    component
}

/// Framework roots whose classes the runtime (or a loaded gem) defines
/// before any app file runs. An app file under one of them reopens them
/// (or nests under them), so a defined constant there does not mean that
/// file has loaded.
const FRAMEWORK_ROOTS: &[&str] = &[
    "ActiveSupport",
    "ActiveModel",
    "ActiveRecord",
    "ActionController",
    "ActionView",
    "ActionDispatch",
    "ActionMailer",
    "ActionMailbox",
    "ActionCable",
    "ActionText",
    "ActiveJob",
    "ActiveStorage",
    "Rails",
    "Arel",
    "Rack",
    "Zeitwerk",
    "Minitest",
    "T",
];

/// Each app/models anchor of a library class or model, mapped to the
/// class or module its file opens (`file_owner`). The value is None when
/// a load-time require of the file must not wait on that constant
/// (`require_line`):
/// - a file program declares the file, and its loads go through the
///   program (`project::load_declarations_through_their_program`);
/// - the constant is Ruby's, a runtime framework's, or a loaded gem's,
///   so the file is a reopen of a constant defined before it loads;
/// - the class is declared in a load-time conditional;
/// - two owners share the anchor.
///
/// The keys are also the class files the tree writes (`authored_requires`).
fn require_guard_owners(app: &App) -> std::rc::Rc<HashMap<String, Option<String>>> {
    let build = || {
        let declared_by_programs: BTreeSet<String> = app
            .file_programs
            .iter()
            .flat_map(|p| p.items.iter())
            .filter_map(|item| match item {
                crate::app::ProgramItem::Declared(names) => Some(names),
                _ => None,
            })
            .flatten()
            .map(|name| format!("app/models/{}", super::library_file_stem(name.0.as_str(), app)))
            .collect();
        let mut out: HashMap<String, Option<String>> = HashMap::default();
        let names = app.library_classes.iter().map(|lc| lc.name.0.as_str()).chain(app.models.iter().map(|m| m.name.0.as_str()));
        for name in names {
            let owner = file_owner(name, app);
            let anchor = format!("app/models/{}", crate::naming::underscore(&owner));
            let guard = guards_on_owner(&owner, &anchor, &declared_by_programs, app).then(|| owner.clone());
            match out.get(&anchor) {
                Some(seen) if *seen != guard => {
                    out.insert(anchor, None);
                }
                Some(_) => {}
                None => {
                    out.insert(anchor, guard);
                }
            }
        }
        std::rc::Rc::new(out)
    };
    match NameIndex::current(app) {
        Some(index) => index.require_guards.get_or_init(build).clone(),
        None => build(),
    }
}

/// Whether a load-time require of `owner`'s file (`anchor`) can wait on
/// `owner` being defined (`require_guard_owners`).
fn guards_on_owner(owner: &str, anchor: &str, declared_by_programs: &BTreeSet<String>, app: &App) -> bool {
    let root = owner.split("::").next().unwrap_or(owner);
    if declared_by_programs.contains(anchor)
        || crate::gem_boundary::RUBY_CORE.contains(&root)
        || crate::gem_boundary::RUBY_STDLIB.contains(&root)
        || FRAMEWORK_ROOTS.contains(&root)
        || is_core_class_name(root)
        || is_gem_facade_root(root)
        || runtime_reopen_anchor(owner).is_some()
        || conditional_owner(owner, app).is_some()
    {
        return false;
    }
    let id = ClassId(Symbol::from(owner));
    if app.gem_boundary.loaded_reopen(&id).is_some() || app.gem_boundary.loaded_source_reopen(&id).is_some() {
        return false;
    }
    library_classes_named(owner, app).iter().all(|lc| {
        !matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FileConstants)) && !reopens_loaded_gem_class(lc, app)
    })
}

/// Where a source's first top-level `class`/`module` ends.
fn first_declaration_end(source: &crate::span::SourceFile) -> Option<u32> {
    let result = ruby_prism::parse(source.text.as_bytes());
    let root = result.node();
    let program = root.as_program_node()?;
    program
        .statements()
        .body()
        .iter()
        .find(|stmt| stmt.as_class_node().is_some() || stmt.as_module_node().is_some())
        .map(|stmt| stmt.location().end_offset() as u32)
}

/// A source's top-level `require "f"` and `require_relative "f"`
/// statements (one string literal argument), in source order, as
/// `(feature, relative, offset)`.
fn top_level_requires(source: &crate::span::SourceFile) -> Vec<(String, bool, u32)> {
    let result = ruby_prism::parse(source.text.as_bytes());
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let mut out = Vec::new();
    for stmt in program.statements().body().iter() {
        let Some(call) = stmt.as_call_node() else { continue };
        if call.receiver().is_some() || call.block().is_some() {
            continue;
        }
        let relative = match call.name().as_slice() {
            b"require" => false,
            b"require_relative" => true,
            _ => continue,
        };
        let Some(arguments) = call.arguments() else { continue };
        let args: Vec<ruby_prism::Node<'_>> = arguments.arguments().iter().collect();
        let [one] = args.as_slice() else { continue };
        let Some(text) = one.as_string_node() else { continue };
        out.push((String::from_utf8_lossy(text.unescaped()).into_owned(), relative, stmt.location().start_offset() as u32));
    }
    out
}

/// Each class or module whose file carries the top-level requires of an
/// app source (`authored_requires`), mapped to those sources (indices
/// into `app.sources`). Only a source with no file program qualifies,
/// since a program keeps its requires itself (`emit_file_program`).
/// So does a boot initializer or environment file: its holder's
/// `run_initializer` / `run_environment` carries each require where it
/// stands (`RoundhouseAutoload.require_source`), after the statements
/// before it, so the file's requires must not also open the holder's
/// file. The file is that of each class the source writes a body into (`file_owner`).
/// When the source only declares empty classes, it is the file of the
/// innermost one.
///
/// A support-walk source lists its classes in `app.library_class_files`.
/// An app/models-walk source (a concern under app/models/concerns) is
/// not listed there: its classes are the library classes whose bodies
/// it writes.
fn authored_require_sources(app: &App) -> std::rc::Rc<HashMap<String, Vec<usize>>> {
    let build = || {
        let programs: BTreeSet<&str> = app.file_programs.iter().map(|p| p.source.as_str()).collect();
        let holders: BTreeSet<&str> = app
            .initializer_programs
            .iter()
            .map(|p| p.holder.0.as_str())
            .chain(app.environment_programs.iter().filter_map(|p| p.holder.as_ref().map(|h| h.0.as_str())))
            .collect();
        let boot_programs: BTreeSet<usize> = holders
            .iter()
            .flat_map(|holder| body_files(holder, app))
            .filter_map(|file| (file as usize).checked_sub(1))
            .collect();
        let ids: HashMap<&str, usize> = app.sources.iter().enumerate().map(|(i, s)| (s.path.as_str(), i)).collect();
        let mut listed: Vec<(usize, Vec<&str>)> = Vec::new();
        for (source, names) in &app.library_class_files {
            let Some(&i) = ids.get(source.as_str()) else { continue };
            listed.push((i, names.iter().map(|n| n.0.as_str()).collect()));
        }
        let support: BTreeSet<usize> = listed.iter().map(|(i, _)| *i).collect();
        let mut walked: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for lc in &app.library_classes {
            for file in body_files(lc.name.0.as_str(), app) {
                let Some(i) = (file as usize).checked_sub(1) else { continue };
                if i < app.sources.len() && !support.contains(&i) {
                    let names = walked.entry(i).or_default();
                    if !names.contains(&lc.name.0.as_str()) {
                        names.push(lc.name.0.as_str());
                    }
                }
            }
        }
        listed.extend(walked);
        let mut out: HashMap<String, Vec<usize>> = HashMap::default();
        for (i, names) in listed {
            let source = &app.sources[i];
            if programs.contains(source.path.as_str())
                || boot_programs.contains(&i)
                || !source.text.contains("require")
                || top_level_requires(source).is_empty()
            {
                continue;
            }
            let file = i as u32 + 1;
            let mut written: Vec<&str> = names.iter().copied().filter(|n| body_files(n, app).contains(&file)).collect();
            if written.is_empty() {
                let deepest = names.iter().map(|n| n.matches("::").count()).max();
                written = names.iter().copied().filter(|n| Some(n.matches("::").count()) == deepest).collect();
            }
            for name in written {
                let sources = out.entry(file_owner(name, app)).or_default();
                if !sources.contains(&i) {
                    sources.push(i);
                }
            }
        }
        std::rc::Rc::new(out)
    };
    match NameIndex::current(app) {
        Some(index) => index.authored_requires.get_or_init(build).clone(),
        None => build(),
    }
}

/// The lines that open `lc`'s file for the top-level requires its
/// sources write, on the Ruby family. A source the app/models walk
/// ingests has no file program, so nothing else carries them.
///
/// Core's essentials concern model_uuid_columns.rb starts with
/// `require "shopify/model_uuid_enabled_attributes"`. That file is the
/// essentials gem's lib, and nothing autoloads it. The concern reads
/// `::Shopify::ModelUuidEnabledAttributes::BY_MODEL_NAME` only in a
/// method body. A body-only reference is left to app/models.rb, so the
/// emitted file loaded nothing, and the first model to declare a uuid
/// column raised NameError.
///
/// Each require of a carried app file becomes a `require_relative` of the
/// emitted files of what that file declares, in authored order. This is
/// what Ruby loads when it runs the source, and it is how a file program
/// carries a require (`ProgramItem::Require`). An app file the tree does
/// not carry gets a named error. A file that declares nothing beyond
/// the modules it opens has those modules opened. A feature outside the
/// app (a gem's, the stdlib's) is not handled here.
fn authored_requires(lc: &LibraryClass, app: &App, out_dir: &Path, self_anchor: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    if !super::expr::native_keywords() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let by_owner = authored_require_sources(app);
    let Some(sources) = by_owner.get(lc.name.0.as_str()) else { return (Vec::new(), Vec::new(), Vec::new()) };
    let table = carried_sources(app);
    let files = require_guard_owners(app);
    let mut requires: Vec<String> = Vec::new();
    let mut after: Vec<String> = Vec::new();
    let mut trailing: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for &i in sources {
        let source = &app.sources[i];
        let rel = relative_source_path(app, &source.path);
        // A gem file's requires written below its class run after the
        // class is defined (rack-oauth2's `connect_object.rb`: `require
        // 'openid_connect/request_object'` after `class ConnectObject`,
        // a subclass file that reopens the class it requires back).
        let gem_file = app.gem_source_dirs.iter().any(|dir| rel.trim_start_matches('/').starts_with(dir.as_str()));
        let declaration_end = if gem_file { first_declaration_end(source) } else { None };
        for (feature, relative, offset) in top_level_requires(source) {
            let after_declaration = declaration_end.is_some_and(|end| offset >= end);
            let requires = if after_declaration { &mut trailing } else { &mut requires };
            let site = format!("{rel}:{}", source.line_col(offset).0);
            let written = format!("require{} {feature:?}", if relative { "_relative" } else { "" });
            match program_require_source(rel, &feature, relative, &table) {
                Some(Ok((required, anchor))) => {
                    let anchors: Vec<String> = match table.declarations.get(&required) {
                        Some(declared) if declared.is_empty() => Vec::new(),
                        Some(declared) => {
                            let mut anchors: Vec<String> =
                                declared.iter().filter(|a| files.contains_key(&format!("app/{a}"))).cloned().collect();
                            if anchors.is_empty() {
                                anchors.push(anchor);
                            }
                            anchors
                        }
                        None => vec![anchor],
                    };
                    for anchor in anchors {
                        let target = format!("app/{anchor}");
                        if target != self_anchor && seen.insert(target.clone()) {
                            requires.push(format!("require_relative {:?}", relpath(out_dir, &target)));
                        }
                    }
                }
                Some(Err(required)) if table.loads_nothing(&required) => {
                    let text = app.sources.iter().find(|f| relative_source_path(app, &f.path) == required).map(|f| f.text.as_str());
                    let opened = text.and_then(crate::ingest::opened_modules::opened_modules).unwrap_or_default();
                    after.push(format!("# {site}: `{written}` loads {required}, which declares nothing the tree carries beyond the modules it opens"));
                    for path in opened {
                        let open: Vec<String> = path.iter().map(|segment| format!("module {segment}")).collect();
                        after.push(format!("{}; {}", open.join("; "), vec!["end"; path.len()].join("; ")));
                    }
                    // Its own requires still run.
                    for anchor in table.requires_of_uncarried(app, &required, &|a| files.contains_key(&format!("app/{a}"))) {
                        let target = format!("app/{anchor}");
                        if target != self_anchor && seen.insert(target.clone()) {
                            requires.push(format!("require_relative {:?}", relpath(out_dir, &target)));
                        }
                    }
                }
                Some(Err(_)) => {
                    let message = format!(
                        "roundhouse: error[file_require_not_carried]: {site}: `{written}` names an app file the tree does not carry; it was not loaded"
                    );
                    after.push(format!("$stderr.puts {message:?}"));
                }
                None => {}
            }
        }
    }
    (requires, after, trailing)
}

/// The constant references of code that runs as its file loads (a
/// class-body call, a constant's value, a class-variable initializer,
/// a method body that load runs), split into the ones it reads (`now`)
/// and the ones inside a closure it only stores (`later`). `scope :x,
/// -> { Plan.active }` stores the lambda, and Core's Shop::Extension
/// `mark_as_translatable :meta_title, type: proc {
/// StoreLocalization::LocalizableContentTypes::SingleLineTextFieldType }`
/// stores the proc, so those constants are read when the closure is
/// called, not when the body runs; a require of their files at the top
/// of shop.rb closed a circular require Ruby does not have.
///
/// Stored: a closure literal (`-> { }`), the block of `proc`, `lambda`
/// or `Proc.new` (`stores_block`), and the block of `define_method`,
/// which is a method body. Any other block (`included do ... end`,
/// `map { }`) may run at once, so it is walked like the code around it,
/// closures stored inside it included. A closure literal that a call
/// is sent to (`-> { X }.call`) or that is applied (`-> { X }.()`) runs
/// there, so it counts as read.
fn walk_load_time_paths(e: &Expr, now: &mut BTreeSet<Vec<String>>, later: &mut BTreeSet<Vec<String>>) {
    walk_paths(e, now, Some(later));
}

/// Every constant path `e` references (both halves of
/// `walk_load_time_paths`).
pub(super) fn walk_const_paths(e: &Expr, out: &mut BTreeSet<Vec<String>>) {
    walk_paths(e, out, None);
}

/// Whether a call's block is stored rather than run where the call
/// stands (`walk_load_time_paths`): `proc { }`, `lambda { }` and
/// `Proc.new { }` (also on `Kernel`), `define_method(:x) { }`, whose
/// block is a method body, and Sorbet's `T.type_alias { }`, which
/// sorbet-runtime (and the runtime's `SorbetRuntimeCheck.type_alias`,
/// its lowering) evaluates on first use. Core's
/// `Merchandising::Taxonomy::Node::Alias = T.type_alias { T.any(...,
/// Taxonomy::Node::Decorator, ...) }` otherwise made node.rb require
/// node/decorator.rb, whose `ACCEPTED_CLASSES = [Node, ...]` reads the
/// node back as it loads: a circular require.
fn stores_block(recv: &Option<Expr>, method: &str) -> bool {
    let recv_is = |names: &[&str]| {
        recv.as_ref().is_some_and(|r| match &*r.node {
            ExprNode::Const { path } => {
                let segments: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty() && *s != "::").collect();
                matches!(segments.as_slice(), [name] if names.contains(name))
            }
            _ => false,
        })
    };
    match method {
        "proc" | "lambda" => recv.is_none() || recv_is(&["Kernel"]),
        "new" => recv_is(&["Proc"]),
        "define_method" | "define_singleton_method" => true,
        "type_alias" => recv_is(&["T", "SorbetRuntimeCheck"]),
        _ => false,
    }
}

/// `walk_const_paths`, and with `stored` the split of
/// `walk_load_time_paths`: what a stored closure references goes to
/// `stored` instead of `out`.
fn walk_paths(e: &Expr, out: &mut BTreeSet<Vec<String>>, mut stored: Option<&mut BTreeSet<Vec<String>>>) {
    macro_rules! walk {
        ($child:expr) => {
            walk_paths($child, out, stored.as_deref_mut())
        };
    }
    // A block runs with the call unless the call stores it.
    macro_rules! walk_block {
        ($block:expr, $stores:expr) => {
            match stored.as_deref_mut() {
                Some(later) if $stores => walk_paths($block, later, None),
                _ => match &*$block.node {
                    ExprNode::Lambda { body, .. } => walk!(body),
                    _ => walk!($block),
                },
            }
        };
    }
    // A closure something is sent to (`-> { X }.call`, `proc { X }.call`)
    // runs there.
    macro_rules! walk_invoked {
        ($child:expr) => {
            match &*$child.node {
                ExprNode::Lambda { body, .. } => walk!(body),
                ExprNode::Send { recv, method, block: Some(_), .. } if stores_block(recv, method.as_str()) => {
                    walk_paths($child, out, None)
                }
                _ => walk!($child),
            }
        };
    }
    match &*e.node {
        ExprNode::Const { path } => {
            out.insert(path.iter().map(|s| s.as_str().to_string()).collect());
        }
        ExprNode::Send { recv, method, args, block, .. } => {
            if let Some(r) = recv {
                walk_invoked!(r);
            }
            for a in args {
                walk!(a);
            }
            if let Some(b) = block {
                walk_block!(b, stores_block(recv, method.as_str()));
            }
        }
        ExprNode::Apply { fun, args, block } => {
            walk_invoked!(fun);
            for a in args {
                walk!(a);
            }
            if let Some(b) = block {
                walk_block!(b, false);
            }
        }
        ExprNode::Hash { entries, .. } => {
            for (k, v) in entries {
                walk!(k);
                walk!(v);
            }
        }
        ExprNode::Array { elements, .. } => {
            for el in elements {
                walk!(el);
            }
        }
        ExprNode::StringInterp { parts } => {
            for p in parts {
                if let InterpPart::Expr { expr } = p {
                    walk!(expr);
                }
            }
        }
        ExprNode::BoolOp { left, right, .. } => {
            walk!(left);
            walk!(right);
        }
        ExprNode::Let { value, body, .. } => {
            walk!(value);
            walk!(body);
        }
        ExprNode::Lambda { body, .. } => match stored.as_deref_mut() {
            Some(later) => walk_paths(body, later, None),
            None => walk!(body),
        },
        // A type-assertion wrapper is transparent to reference edges —
        // the broadcast lowerings wrap their `Views::…` payload render
        // in `Cast { …, Str }`, and skipping it dropped the model →
        // views require.
        ExprNode::Cast { value, .. } => walk!(value),
        ExprNode::If { cond, then_branch, else_branch } => {
            walk!(cond);
            walk!(then_branch);
            walk!(else_branch);
        }
        ExprNode::Case { scrutinee, arms } => {
            walk!(scrutinee);
            for arm in arms {
                walk!(&arm.body);
            }
        }
        ExprNode::Seq { exprs } => {
            for e in exprs {
                walk!(e);
            }
        }
        ExprNode::Assign { value, .. } => walk!(value),
        ExprNode::Yield { args } => {
            for a in args {
                walk!(a);
            }
        }
        ExprNode::Raise { value } => walk!(value),
        ExprNode::RescueModifier { expr, fallback } => {
            walk!(expr);
            walk!(fallback);
        }
        ExprNode::Return { value } => walk!(value),
        ExprNode::Super { args, block } => {
            for a in args.iter().flatten() {
                walk!(a);
            }
            if let Some(b) = block {
                walk_block!(b, false);
            }
        }
        ExprNode::BeginRescue { body, rescues, else_branch, ensure, .. } => {
            walk!(body);
            for r in rescues {
                walk!(&r.body);
            }
            if let Some(e) = else_branch {
                walk!(e);
            }
            if let Some(e) = ensure {
                walk!(e);
            }
        }
        ExprNode::Next { value: Some(v) } => walk!(v),
        ExprNode::MultiAssign { value, .. } => walk!(value),
        ExprNode::While { cond, body, .. } => {
            walk!(cond);
            walk!(body);
        }
        ExprNode::Range { begin, end, .. } => {
            if let Some(b) = begin {
                walk!(b);
            }
            if let Some(e) = end {
                walk!(e);
            }
        }
        // Leaves and uninteresting nodes pass through.
        _ => {}
    }
}

// has_secure_password synthesis moved to the shared model lowering
// (`lower::secure_password::push_secure_password_methods`) — every
// target's model classes now carry authenticate + the plaintext
// accessors, in the bcrypt gem's own contract shape
// (`BCrypt::Password.create/new`): the CRuby/JRuby trees load the
// real gem (guarded require in the overlay main.rb), and a future
// spinel-bcrypt spin package satisfies the same calls.

fn sp_expr(node: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), node)
}

fn ivar_read(name: &Symbol) -> Expr {
    sp_expr(ExprNode::Ivar { name: name.clone() })
}

// Request-params key normalization moved to the Ruby expr emitter's
// type-directed index hook (`emit::ruby::expr`): a symbol/dynamic key on
// a statically string-keyed hash (`Hash[String, _]`) is coerced to a
// string at the single `[]` emit chokepoint. That gates on the receiver
// *type* rather than a `params` name heuristic, so it covers views and
// helpers (params flows there too) and never touches a genuine
// symbol-keyed `Hash[Symbol, _]` like `StoryRepository#@params`.

// typed_store accessor synthesis moved to the shared model lowering
// (`lower::typed_store::push_typed_store_methods`) — every target's
// model classes now carry the reader/predicate/writer methods; the
// `TypedStore` runtime module (YAML seam) still ships only on the
// CRuby/JRuby overlay trees.

// ── boolean-column cast lowering ─────────────────────────────────────

/// Ruby-family pre-emit pass: boolean-column readers and `<col>?`
/// predicates cast the stored value instead of returning it raw. The
/// CRuby sqlite adapter hydrates boolean columns as the Integers
/// SQLite stores (0/1) — and `0` is TRUTHY in Ruby, so a plain `@col`
/// read makes every `user.is_admin?` guard pass for non-admins.
/// Rewritten body: `@col == true || @col == 1` (handles both a
/// DB-hydrated Integer and an app-assigned true/false; nil/0/false →
/// false). An ordinary nullable getter keeps nil; its `?` predicate
/// still answers false. Strict targets hydrate native booleans and keep
/// the shared synthesized shape. Only plain `@col`-read bodies are rewritten
/// (idempotent; custom bodies win).
pub(crate) fn apply_boolean_lowering(lcs: &mut [LibraryClass], app: &App) {
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else {
            continue;
        };
        let bool_cols: HashMap<Symbol, bool> = table
            .columns
            .iter()
            .filter(|c| matches!(c.col_type, crate::schema::ColumnType::Boolean))
            .map(|c| (c.name.clone(), c.nullable && !c.primary_key))
            .collect();
        if bool_cols.is_empty() {
            continue;
        }
        let Some(lc) = lcs.iter_mut().find(|lc| lc.name == model.name) else {
            continue;
        };
        for m in &mut lc.methods {
            if m.receiver != MethodReceiver::Instance {
                continue;
            }
            let col = Symbol::from(m.name.as_str().trim_end_matches('?'));
            let Some(nullable) = bool_cols.get(&col) else {
                continue;
            };
            if is_plain_ivar_read(&m.body, &col) {
                let cast = boolean_cast_body(&col);
                m.body = if *nullable && !m.name.as_str().ends_with('?') {
                    sp_expr(ExprNode::If {
                        cond: sp_expr(ExprNode::Send {
                            recv: Some(ivar_read(&col)),
                            method: Symbol::from("nil?"),
                            args: vec![],
                            block: None,
                            parenthesized: false,
                        }),
                        then_branch: sp_expr(ExprNode::Lit { value: Literal::Nil }),
                        else_branch: cast,
                    })
                } else {
                    cast
                };
            }
        }
    }
}

/// `@col == true || @col == 1`.
fn boolean_cast_body(col: &Symbol) -> Expr {
    let eq = |rhs: Expr| {
        sp_expr(ExprNode::Send {
            recv: Some(ivar_read(col)),
            method: Symbol::from("=="),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        })
    };
    sp_expr(ExprNode::BoolOp {
        op: crate::expr::BoolOpKind::Or,
        surface: Default::default(),
        left: eq(sp_expr(ExprNode::Lit {
            value: crate::expr::Literal::Bool { value: true },
        })),
        right: eq(sp_expr(ExprNode::Lit {
            value: crate::expr::Literal::Int { value: 1 },
        })),
    })
}

// ─── Runtime-Relation eager loading (issue #27 follow-up) ──────────────
//
// The static arel path already lowers `includes(:assoc)` into inline
// preload statements, but chains that reach the runtime
// `ActiveRecord::Relation` (scope chains, association relations) only
// RECORDED their `includes(...)` specs — `to_a` never executed them, so
// every association read was a lazy per-row query (the lobsters 2x
// query-count gap vs Rails, ~985 excess queries per benchmark pass:
// belongs_to singles ~870, has_many/through ~295).
//
// This pass synthesizes, per model, the statically-dispatched preload
// machinery `Relation#to_a` calls (`@model.preload_associations(records,
// @includes)` — Base supplies a no-op default):
//
//   def self.preload_associations(records, specs)   # spec walker
//   def self._preload_dispatch(records, name, nested)  # case-dispatch
//   def self._preload_batch_<assoc>(records)        # one batched IN load
//   def _preload_<belongs_to>(rec)                  # cache setter
//
// plus a cache guard prepended to each belongs_to reader (mirroring the
// has_many readers' `return @x_cache if @x_loaded` shape; the has_many
// setters/caches already exist from the static-path work).
//
// No method_missing, no send: nested specs (`story: :user`) recurse
// through the case arm's statically-named target class — the same
// case-dispatch shape as dynamic-partial rendering. Bodies are generated
// as Ruby source and parsed back through `runtime_src::parse_methods`
// (templates are fixed; identifiers come from assoc/table names).
//
// Two framework-owned associations batch the same way, under Rails'
// own names: `has_one_attached :<attr>` as `<attr>_attachment` (one
// join over the attachment and blob tables, installing a row-bearing
// `ActiveStorage::Attached` on each record) and `has_rich_text :<attr>`
// as `rich_text_<attr>` (one `IN` over `action_text_rich_texts`,
// installing the record through the owner's load-once setter). The
// `with_attached_*` / `with_rich_text_*` scopes preload these names, so
// campfire's `Message.with_attachment_details` costs the room page two
// queries where it cost eighty.
//
// Known gaps, deliberate: scoped / polymorphic has_one, direct has_many
// with a scope or polymorphic owner, and scope-carrying through-assocs
// (other than a plain `order("...")`) get no batch arm — the dispatch
// falls through and the lazy reader stays correct (just N+1, matching
// Rails, which also lazy-loads what `includes` doesn't name). Unscoped
// non-polymorphic has_one uses PreloadKind::HasOne (first-wins). Assigning
// a belongs_to (`c.story = s`) on a PRELOADED record does not refresh
// the cache (fresh records never have the loaded flag set, so the
// benchmark's build-then-render flows are unaffected).
/// Synthesize runtime batch loaders when includes hints occur, leaving
/// associations whose restrictions cannot be preserved to their lazy readers.
pub(crate) fn apply_preload_lowering(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    // A dynamic query can reach Relation without any named scope (for
    // example, a helper returning a where.not chain). Its includes hint
    // needs the same batch loaders. Apps with no includes remain untouched.
    if !app_mentions_includes(app) {
        return;
    }

    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };

        // belongs_to readers gain the cache guard the has_many readers
        // already carry: `return @user_cache if @user_loaded`.
        for assoc in model.associations() {
            let Association::BelongsTo { name, .. } = assoc else { continue };
            let Some(m) = lc.methods.iter_mut().find(|m| {
                m.name == *name && m.receiver == MethodReceiver::Instance
            }) else {
                continue;
            };
            let old = std::mem::replace(
                &mut m.body,
                Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
            );
            m.body = Expr::new(
                Span::synthetic(),
                ExprNode::Seq { exprs: vec![preload_cache_guard(name), old] },
            );
        }

        let src = preload_methods_source(model, app);
        let methods = crate::runtime_src::parse_methods(&src).unwrap_or_else(|e| {
            panic!("apply_preload_lowering: generated source failed to parse: {e}\n{src}")
        });
        for mut m in methods {
            if lc.methods.iter().any(|existing| {
                existing.name == m.name && existing.receiver == m.receiver
            }) {
                continue; // user-defined names win
            }
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }
    }
}

/// `return @<name>_cache if @<name>_loaded` — the same guard shape the
/// has_many readers carry (`through_reader_body`), so preloaded and lazy
/// reads share one cache contract.
fn preload_cache_guard(name: &Symbol) -> Expr {
    let span = Span::synthetic;
    Expr::new(
        span(),
        ExprNode::If {
            cond: Expr::new(
                span(),
                ExprNode::Ivar { name: Symbol::from(format!("{}_loaded", name.as_str())) },
            ),
            then_branch: Expr::new(
                span(),
                ExprNode::Return {
                    value: Expr::new(
                        span(),
                        ExprNode::Ivar {
                            name: Symbol::from(format!("{}_cache", name.as_str())),
                        },
                    ),
                },
            ),
            else_branch: Expr::new(span(), ExprNode::Lit { value: Literal::Nil }),
        },
    )
}

/// True when any raw app body (controller actions, library-class
/// methods, model scopes/methods) sends `includes`/`preload`/
/// `eager_load` to a receiver, or calls `association(x)`. Over-approximates (a chain the arel pass
/// later consumes statically still counts) — the cost of a false
/// positive is inert synthesized methods, not wrong behavior.
fn app_mentions_includes(app: &App) -> bool {
    use crate::dialect::{ControllerBodyItem, ModelBodyItem};
    let in_expr = expr_mentions_includes;
    app.controllers.iter().any(|c| {
        c.body.iter().any(|item| match item {
            ControllerBodyItem::Action { action, .. } => in_expr(&action.body),
            _ => false,
        })
    }) || app.library_classes.iter().any(|lc| lc.methods.iter().any(|m| in_expr(&m.body)))
        || app.models.iter().any(|m| {
            m.body.iter().any(|item| match item {
                ModelBodyItem::Scope { scope, .. } => in_expr(&scope.body),
                ModelBodyItem::Method { method, .. } => in_expr(&method.body),
                _ => false,
            })
        })
        // `record.association(:x)` reads and seeds the same caches a
        // preload fills, so a belongs_to reader needs the same guard.
        || app_calls_association(app)
}

fn expr_mentions_includes(expr: &Expr) -> bool {
    let mut found = false;
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { recv: Some(_), method, .. } = &*e.node {
            if matches!(method.as_str(), "includes" | "preload" | "eager_load") {
                *found = true;
                return;
            }
        }
        // `Associations::Preloader` (runtime/spinel/active_record_associations.rb) drives the same loaders.
        if matches!(&*e.node, ExprNode::Const { path } if path.len() >= 2 && path[path.len() - 2].as_str() == "Associations" && path[path.len() - 1].as_str() == "Preloader") {
            *found = true;
            return;
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    walk(expr, &mut found);
    found
}

/// One preloadable association, resolved against the app's model set.
enum PreloadKind {
    /// (fk column on the owner, target class, target table)
    BelongsTo { fk: String, target: String, table: String },
    /// (fk column on the target, target class)
    HasMany { fk: String, target: String },
    /// `has_one` — same FK-on-target batch as has_many, installing one
    /// record (or nil) per owner through the single-record preload setter.
    HasOne { fk: String, target: String },
    /// Batched form of the through-reader join:
    /// `SELECT <t>.*, <thr>.<thr_fk> AS __src FROM <t> JOIN <thr> ON
    /// <thr>.<src_fk> = <t>.id WHERE <thr>.<thr_fk> IN (...)`.
    Through { target: String, table: String, join: String, group_col: String, order: Option<String> },
    /// `has_one_attached :<attr>`: one join over the attachment and blob
    /// tables for the whole record set, installing a row-bearing
    /// `ActiveStorage::Attached` on each record (`attr`, owner class).
    /// `variations` is the declared-variant list as Ruby source
    /// (`lower::attached::variations_ruby_source`), the proxy's fourth
    /// constructor argument.
    Attached { attr: String, owner: String, variations: String },
    /// `has_many_attached :<attr>`: one join over the attachment and blob
    /// tables for the whole record set, installing a proxy per record
    /// that answers its rows (`AttachedMany#_preload_rows`) without
    /// asking again.
    AttachedMany { attr: String, owner: String },
    /// `has_rich_text :<attr>`: one `IN` over `action_text_rich_texts`,
    /// installed through the owner's load-once setter.
    RichText { attr: String, owner: String },
    /// `has_markdown :<attr>`: one `IN` over `action_text_markdowns`,
    /// installed through the owner's load-once setter.
    PlainText { attr: String, owner: String },
}

/// Select association shapes whose batch queries preserve the reader's filters,
/// resolving each against the app model registry and its table metadata.
fn preload_targets(model: &crate::dialect::Model, app: &App) -> Vec<(String, PreloadKind)> {
    use crate::dialect::Association;

    let model_exists = |id: &ClassId| app.models.iter().any(|m| &m.name == id);
    let mut out = Vec::new();
    for assoc in model.associations() {
        match assoc {
            Association::BelongsTo { name, target, foreign_key, .. } => {
                let Some(target_model) = app.models.iter().find(|m| &m.name == target) else {
                    continue;
                };
                out.push((
                    name.as_str().to_string(),
                    PreloadKind::BelongsTo {
                        fk: foreign_key.as_str().to_string(),
                        target: target.0.as_str().to_string(),
                        table: target_model.table.0.as_str().to_string(),
                    },
                ));
            }
            // This loader only applies the foreign-key predicate. A scope
            // or polymorphic owner-type restriction must stay on the lazy
            // reader until the batch query can preserve it as well.
            Association::HasMany {
                name, target, foreign_key, through: None,
                scope: None, as_interface: None, ..
            } => {
                if !model_exists(target) {
                    continue;
                }
                out.push((
                    name.as_str().to_string(),
                    PreloadKind::HasMany {
                        fk: foreign_key.as_str().to_string(),
                        target: target.0.as_str().to_string(),
                    },
                ));
            }
            // Same restriction as has_many: FK-only batch. Scoped or
            // polymorphic has_one stays on the lazy reader until the
            // batch can preserve those predicates.
            Association::HasOne {
                name, target, foreign_key,
                scope: None, as_interface: None, ..
            } => {
                if !model_exists(target) {
                    continue;
                }
                out.push((
                    name.as_str().to_string(),
                    PreloadKind::HasOne {
                        fk: foreign_key.as_str().to_string(),
                        target: target.0.as_str().to_string(),
                    },
                ));
            }
            // Through: same two-hop resolution as
            // `apply_through_assoc_lowering`; assoc scopes other than a
            // plain `order("...")` (or none) don't batch — the lazy
            // reader keeps them correct.
            Association::HasMany {
                name, target, through: Some(thr_name), scope, ..
            } => {
                if !model_exists(target) {
                    continue;
                }
                let order = match scope {
                    None => None,
                    Some(s) => match order_literal(s) {
                        Some(o) => Some(o),
                        None => continue,
                    },
                };
                let Some(Association::HasMany {
                    target: thr_target, foreign_key: thr_fk, as_interface, ..
                }) =
                    model.associations().find(|a| {
                        matches!(a, Association::HasMany { name, .. } if name == thr_name)
                    })
                else {
                    continue;
                };
                let Some(thr_model) = app.models.iter().find(|m| &m.name == thr_target) else {
                    continue;
                };
                let Some(Association::BelongsTo { foreign_key: src_fk, .. }) =
                    thr_model.associations().find(|a| {
                        matches!(a, Association::BelongsTo { target: t, .. } if t == target)
                    })
                else {
                    continue;
                };
                let thr_table = model_table_name(&app.models, thr_target);
                let target_table = model_table_name(&app.models, target);
                let owner_type = through_owner_type_predicate(&thr_table, as_interface, &model.name);
                out.push((
                    name.as_str().to_string(),
                    PreloadKind::Through {
                        target: target.0.as_str().to_string(),
                        table: target_table.clone(),
                        join: format!(
                            "INNER JOIN {thr_table} ON {thr_table}.{src_fk} = {target_table}.id{owner_type}"
                        ),
                        group_col: format!("{thr_table}.{thr_fk}"),
                        order: order.map(|o| o.to_string()),
                    },
                ));
            }
            _ => {}
        }
    }
    // The framework-owned pairs, under Rails' own association names.
    for (_span, attr) in crate::lower::attached::attached_attrs(model) {
        out.push((
            crate::lower::attached::attachment_assoc_name(&attr).as_str().to_string(),
            PreloadKind::Attached {
                attr: attr.as_str().to_string(),
                owner: model.name.0.as_str().to_string(),
                variations: crate::lower::attached::variations_ruby_source(model, &attr),
            },
        ));
    }
    for (_span, attr) in crate::lower::attached::many_attached_attrs(model) {
        out.push((
            crate::lower::attached::many_attachments_assoc_name(&attr)
                .as_str()
                .to_string(),
            PreloadKind::AttachedMany {
                attr: attr.as_str().to_string(),
                owner: model.name.0.as_str().to_string(),
            },
        ));
    }
    if model_exists(&crate::lower::rich_text::record_class()) {
        for (_span, attr) in crate::lower::rich_text::rich_text_attrs(model) {
            out.push((
                format!("rich_text_{}", attr.as_str()),
                PreloadKind::RichText {
                    attr: attr.as_str().to_string(),
                    owner: model.name.0.as_str().to_string(),
                },
            ));
        }
    }
    if model_exists(&crate::lower::plain_text_attr::record_class()) {
        for (_span, attr) in crate::lower::plain_text_attr::plain_text_attrs(model) {
            out.push((
                format!("markdown_{}", attr.as_str()),
                PreloadKind::PlainText {
                    attr: attr.as_str().to_string(),
                    owner: model.name.0.as_str().to_string(),
                },
            ));
        }
    }
    out
}

/// Extract the string literal from an assoc-scope lambda body of the
/// exact shape `order("...")` (lobsters `has_many :tags, -> { order
///('tags.is_media desc, tags.tag') }, through: :taggings`).
fn order_literal(scope_body: &Expr) -> Option<&str> {
    let ExprNode::Send { recv: None, method, args, .. } = &*scope_body.node else {
        return None;
    };
    if method.as_str() != "order" || args.len() != 1 {
        return None;
    }
    let ExprNode::Lit { value: Literal::Str { value } } = &*args[0].node else {
        return None;
    };
    Some(value.as_str())
}

/// Generate the per-model preload methods as Ruby source (fed back
/// through `runtime_src::parse_methods`). Templates stay boring on
/// purpose: statement-level assigns and explicit nil-guards round-trip
/// through every walker; no `||=`-on-index, no ternaries.
fn preload_methods_source(model: &crate::dialect::Model, app: &App) -> String {
    let targets = preload_targets(model, app);
    let mut src = String::new();

    // Batch loaders + belongs_to cache setters.
    for (name, kind) in &targets {
        match kind {
            PreloadKind::BelongsTo { fk, target, table } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    v = r.{fk}
    ids << v unless v.nil? || v == 0
  end
  ids.uniq!
  by_id = {{}}
  if ids.length > 0
    {target}._hydrate_all("SELECT " + {target}._columns_sql + " FROM {table} WHERE {table}.id IN (" + Db.escape_int_list(ids) + ")").each do |rec|
      by_id[rec.id] = rec
    end
  end
  records.each do |r|
    r._preload_{name}(by_id[r.{fk}])
  end
  by_id.values
end

def _preload_{name}(rec)
  @{name}_cache = rec
  @{name}_loaded = true
  nil
end
"#
                );
            }
            PreloadKind::HasMany { fk, target } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  loaded = []
  if ids.length > 0
    loaded = ActiveRecord::Relation.new({target}).where({fk}: ids).to_a
  end
  grouped = {{}}
  loaded.each do |rec|
    k = rec.{fk}
    grouped[k] = [] if grouped[k].nil?
    grouped[k] << rec
  end
  records.each do |r|
    r._preload_{name}(grouped[r.id] || [])
  end
  loaded
end
"#
                );
            }
            PreloadKind::HasOne { fk, target } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  by_id = {{}}
  loaded = []
  if ids.length > 0
    loaded = ActiveRecord::Relation.new({target}).where({fk}: ids).to_a
  end
  loaded.each do |rec|
    k = rec.{fk}
    by_id[k] = rec if by_id[k].nil?
  end
  records.each do |r|
    r._preload_{name}(by_id[r.id])
  end
  loaded
end
"#
                );
            }
            PreloadKind::Through { target, table, join, group_col, order } => {
                let order_sql = match order {
                    Some(o) => format!(" ORDER BY {o}"),
                    None => String::new(),
                };
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  grouped = {{}}
  loaded = []
  if ids.length > 0
    rows = ActiveRecord.adapter.select_rows("SELECT {table}.*, {group_col} AS __src FROM {table} {join} WHERE {group_col} IN (" + Db.escape_int_list(ids) + "){order_sql}")
    rows.each do |row|
      rec = {target}.instantiate(row)
      loaded << rec
      k = row["__src"].to_i
      grouped[k] = [] if grouped[k].nil?
      grouped[k] << rec
    end
  end
  records.each do |r|
    r._preload_{name}(grouped[r.id] || [])
  end
  loaded
end
"#
                );
            }
            // One join for every record's attachment row; a record the
            // query did not name gets a proxy that already knows it has
            // nothing (id 0), so its `attached?` is a field read too.
            // The blob columns are `ActiveStorage::Blob.columns`, so
            // this query and the proxy's own cannot disagree about the
            // row shape; two typed hashes (attachment id, blob) rather
            // than one hash of rows so every value keeps its own type.
            PreloadKind::Attached { attr, owner, variations } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  att_ids = {{}}
  blobs = {{}}
  if ids.length > 0
    ActiveRecord.adapter.select_rows("SELECT a.record_id AS record_id, a.id AS attachment_id, " + ActiveStorage::Blob.columns("b") + " FROM active_storage_attachments a JOIN active_storage_blobs b ON b.id = a.blob_id WHERE a.record_type = '{owner}' AND a.name = '{attr}' AND a.record_id IN (" + Db.escape_int_list(ids) + ")").each do |row|
      rid = row["record_id"].to_i
      att_ids[rid] = row["attachment_id"].to_i
      blobs[rid] = ActiveStorage::Blob.from_row(row)
    end
  end
  records.each do |r|
    att = ActiveStorage::Attached.new("{owner}", r.id, "{attr}", {variations})
    found = att_ids[r.id]
    if found.nil?
      att._preload_row(0, nil)
    else
      att._preload_row(found, blobs[r.id])
    end
    r._preload_{name}(att)
  end
  []
end
"#
                );
            }
            PreloadKind::AttachedMany { attr, owner } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  rows_by = {{}}
  if ids.length > 0
    ActiveRecord.adapter.select_rows("SELECT a.record_id AS record_id, a.id AS attachment_id, " + ActiveStorage::Blob.columns("b") + " FROM active_storage_attachments a JOIN active_storage_blobs b ON b.id = a.blob_id WHERE a.record_type = '{owner}' AND a.name = '{attr}' AND a.record_id IN (" + Db.escape_int_list(ids) + ") ORDER BY a.id").each do |row|
      rid = row["record_id"].to_i
      list = rows_by[rid]
      if list.nil?
        list = []
        rows_by[rid] = list
      end
      list.push(ActiveStorage::ManyAttachment.new(row["attachment_id"].to_i, ActiveStorage::Blob.from_row(row)))
    end
  end
  records.each do |r|
    many = ActiveStorage::AttachedMany.new("{owner}", r.id, "{attr}")
    found = rows_by[r.id]
    many._preload_rows(found.nil? ? [] : found)
    r._preload_{name}(many)
  end
  []
end
"#
                );
            }
            // One IN over the rich-text table; a record with no row is
            // told so, which is the state the load-once reader must not
            // re-query.
            PreloadKind::RichText { attr, owner } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  by_id = {{}}
  loaded = []
  if ids.length > 0
    loaded = ActiveRecord::Relation.new(ActionText::RichText).where(record_type: "{owner}", name: "{attr}", record_id: ids).to_a
  end
  loaded.each do |rec|
    by_id[rec.record_id] = rec
  end
  records.each do |r|
    r._preload_{name}(by_id[r.id])
  end
  loaded
end
"#
                );
            }
            PreloadKind::PlainText { attr, owner } => {
                let _ = write!(
                    src,
                    r#"
def self._preload_batch_{name}(records)
  ids = []
  records.each do |r|
    ids << r.id
  end
  by_id = {{}}
  loaded = []
  if ids.length > 0
    loaded = ActiveRecord::Relation.new(ActionText::Markdown).where(record_type: "{owner}", name: "{attr}", record_id: ids).to_a
  end
  loaded.each do |rec|
    by_id[rec.record_id] = rec
  end
  records.each do |r|
    r._preload_{name}(by_id[r.id])
  end
  loaded
end
"#
                );
            }
        }
    }

    // Dispatch: one case arm per preloadable assoc; unknown names fall
    // through silently (lazy readers stay correct — mirrors what Rails
    // does for anything `includes` didn't name).
    src.push_str("\ndef self._preload_dispatch(records, name, nested)\n");
    if targets.is_empty() {
        src.push_str("  nil\nend\n");
    } else {
        src.push_str("  case name\n");
        for (name, kind) in &targets {
            let target = match kind {
                PreloadKind::BelongsTo { target, .. } => Some(target.as_str()),
                PreloadKind::HasMany { target, .. } => Some(target.as_str()),
                PreloadKind::HasOne { target, .. } => Some(target.as_str()),
                PreloadKind::Through { target, .. } => Some(target.as_str()),
                PreloadKind::RichText { .. } => Some("ActionText::RichText"),
                PreloadKind::PlainText { .. } => Some("ActionText::Markdown"),
                // `includes(logo_attachment: :blob)`: the blob is already
                // in the row the loader fetched; there is no model to
                // recurse into. Many installs the proxy only.
                PreloadKind::Attached { .. } | PreloadKind::AttachedMany { .. } => None,
            };
            match target {
                Some(target) => {
                    let _ = write!(
                        src,
                        "  when :{name}\n    loaded = _preload_batch_{name}(records)\n    {target}.preload_associations(loaded, [nested]) unless nested.nil?\n"
                    );
                }
                None => {
                    let _ = write!(src, "  when :{name}\n    _preload_batch_{name}(records)\n");
                }
            }
        }
        src.push_str("  end\n  nil\nend\n");
    }

    // Spec walker — the entry point `Relation#to_a` calls. Specs are
    // Symbols, Hashes (nested: `story: :user`), or Arrays of either.
    src.push_str(
        r#"
def self.preload_associations(records, specs)
  return nil if records.length == 0
  specs.each do |spec|
    if spec.is_a?(Hash)
      spec.each do |name, nested|
        _preload_dispatch(records, name, nested)
      end
    elsif spec.is_a?(Array)
      preload_associations(records, spec)
    elsif !spec.nil?
      _preload_dispatch(records, spec, nil)
    end
  end
  nil
end
"#,
    );

    src
}

/// Monomorphize an app helper on a `raw(...)` argument.
///
/// Rails carries "this string is safe" in the VALUE (SafeBuffer), so a
/// safe label can ride a plain parameter into a helper and out to
/// `link_to`, which then declines to escape it. Nothing survives that
/// boundary here: the emit-time unwrap (`is_html_safe_call`) only sees
/// producers it can name at the call site, and the AOT tree has no
/// safe-string type at all. lobsters' layout hits it once and it shows
/// on every page with a header —
///
///   link_to_different_page raw("#{user}&nbsp;<span class='karma'>…"), settings_path
///
/// rendered `michell_wiegand&amp;nbsp;&lt;span…` on the spinel tree, a
/// +24B diff against Rails on THIRTEEN of the 49 benchmark routes.
///
/// So safety propagates statically instead: a call passing `raw(x)`
/// retargets to a `<name>_raw` clone of the helper in which the safe
/// parameter's escaping consumers are neutralized. This is the same
/// exemption the `link_to(raw(x))` → `link_to_raw(x)` rewrite makes one
/// level down, lifted through one user-defined frame.
///
/// Deliberately narrow. The clone rewrites only what it can prove
/// consumes THAT parameter — `link_to` on it becomes `link_to_raw`, and
/// an `html_escape` of it collapses — and a helper whose safe argument
/// reaches neither is emitted as an ordinary clone rather than guessed
/// at. Widening this is a typed-safe-string project, not a bigger match
/// arm.
pub(crate) fn apply_raw_helper_monomorphization(lcs: &mut [LibraryClass], app: &App) {
    let sites = raw_helper_sites(app);
    if sites.is_empty() {
        return;
    }
    // Definition side: synthesize the `_raw` clone next to the original.
    for lc in lcs.iter_mut() {
        let mut synthesized: Vec<crate::dialect::MethodDef> = Vec::new();
        for m in &lc.methods {
            for (name, idx) in &sites {
                if &m.name != name {
                    continue;
                }
                let Some(param) = m.params.get(*idx) else { continue };
                let mut clone = m.clone();
                clone.name = Symbol::from(format!("{}_raw", name.as_str()));
                let pname = param.name.clone();
                propagate_raw_param(&mut clone.body, &pname);
                synthesized.push(clone);
            }
        }
        lc.methods.extend(synthesized);
    }
    // Call side: `M.helper(raw(x), …)` → `M.helper_raw(x, …)`.
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            rewrite_raw_helper_calls(&mut m.body, &sites);
        }
    }
}

/// `(helper name, index of the argument passed as `raw(...)`)` for every
/// app-helper call site that passes one. Read from `app.views` — i.e.
/// BEFORE lowering, where both the helper call and `raw` are still bare
/// sends — because the two sides of this rewrite are emitted in
/// different LC groups and each needs the same answer.
///
/// Views only: `raw` is a view-layer marker, and a controller reaching
/// for one would be building markup in the wrong place. Extend to
/// controller bodies if a corpus ever does it.
fn raw_helper_sites(app: &App) -> BTreeSet<(Symbol, usize)> {
    let mut out = BTreeSet::new();
    if app.helper_method_index.is_empty() {
        return out;
    }
    let mut scan = |e: &Expr| {
        collect_raw_helper_sites(e, &app.helper_method_index, &mut out);
    };
    for v in &app.views {
        scan(&v.body);
    }
    out
}

fn collect_raw_helper_sites(
    e: &Expr,
    index: &crate::det::HashMap<Symbol, ClassId>,
    out: &mut BTreeSet<(Symbol, usize)>,
) {
    e.node.for_each_child(&mut |c| collect_raw_helper_sites(c, index, out));
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node else {
        return;
    };
    if !index.contains_key(method) {
        return;
    }
    for (i, a) in args.iter().enumerate() {
        let is_raw = matches!(&*a.node,
            ExprNode::Send { recv: None, method: m, args: ra, .. }
                if m.as_str() == "raw" && ra.len() == 1);
        if is_raw {
            out.insert((method.clone(), i));
        }
    }
}

/// Inside the `_raw` clone: the named parameter is already safe, so the
/// escaping it would otherwise receive comes off.
fn propagate_raw_param(body: &mut Expr, pname: &Symbol) {
    body.node.for_each_child_mut(&mut |c| propagate_raw_param(c, pname));
    // `ViewHelpers.link_to(<uses param>, …)` → `link_to_raw`.
    if let ExprNode::Send { recv: Some(r), method, args, .. } = &mut *expr_node_mut(body) {
        if method.as_str() == "link_to"
            && is_view_helpers_const(r)
            && args.first().is_some_and(|a| mentions_var(a, pname))
        {
            *method = Symbol::from("link_to_raw");
            return;
        }
    }
    // `ViewHelpers.html_escape(<param>)` → `<param>`.
    let collapse = match &*body.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "html_escape"
                && args.len() == 1
                && is_view_helpers_const(r)
                && mentions_var(&args[0], pname) =>
        {
            Some(args[0].clone())
        }
        _ => None,
    };
    if let Some(inner) = collapse {
        *body = inner;
    }
}

fn expr_node_mut(e: &mut Expr) -> &mut ExprNode {
    &mut e.node
}

/// Does `e` read `name` (directly, or through a `.to_s` the emitter
/// added)?
fn mentions_var(e: &Expr, name: &Symbol) -> bool {
    match &*e.node {
        ExprNode::Var { name: n, .. } => n == name,
        ExprNode::Send { recv: Some(r), args, .. } if args.is_empty() => mentions_var(r, name),
        _ => false,
    }
}

fn rewrite_raw_helper_calls(e: &mut Expr, sites: &BTreeSet<(Symbol, usize)>) {
    e.node.for_each_child_mut(&mut |c| rewrite_raw_helper_calls(c, sites));
    let ExprNode::Send { recv: Some(_), method, args, block: None, .. } = &mut *e.node else {
        return;
    };
    let Some((_, idx)) = sites.iter().find(|(n, i)| n == method && args.len() > *i) else {
        return;
    };
    let inner = match &*args[*idx].node {
        ExprNode::Send { recv: Some(r2), method: m2, args: a2, .. }
            if m2.as_str() == "raw" && a2.len() == 1 && is_view_helpers_const(r2) =>
        {
            Some(a2[0].clone())
        }
        _ => None,
    };
    if let Some(inner) = inner {
        let i = *idx;
        *method = Symbol::from(format!("{}_raw", method.as_str()));
        args[i] = inner;
    }
}

#[cfg(test)]
mod insert_rel_param_tests {
    use super::*;
    use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, MethodVisibility, Param};
    use crate::expr::{Expr, ExprNode, Literal};
    use crate::ty::ParamKind;

    fn bare_method(params: Vec<Param>, signature: Option<Ty>) -> MethodDef {
        MethodDef {
            visibility: MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from("those_tagged"),
            receiver: MethodReceiver::Class,
            params,
            body: Expr::new(crate::span::Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            signature,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(Symbol::from("Widget")),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        }
    }

    /// The decided fix, at the type level: a rest-param method's `__rel`
    /// is a KEYWORD in `m.params` (a default, not a positional default —
    /// the only legal ordering after a splat), and in the `Ty::Fn`
    /// signature it is `ParamKind::Keyword { required: false }`, typed
    /// as the owning model's `Relation` — NOT `Ty::Untyped` — so a
    /// target whose `.rbs` DOES read the signature (unlike this
    /// fixture's own, which the general stamping pass declines to give
    /// one; see `tests/relation_rest_param_class_method.rs`) writes
    /// `?__rel: ActiveRecord::Relation`, matching every other typed slot
    /// on the method instead of losing its type to the one param this
    /// fix touches.
    #[test]
    fn rest_param_method_gets_rel_as_a_typed_keyword_in_params_and_signature() {
        let owner = ClassId(Symbol::from("Widget"));
        let params = vec![Param::rest(Symbol::from("tags"))];
        let sig = Ty::Fn {
            params: vec![crate::ty::Param {
                name: Symbol::from("tags"),
                ty: Ty::Untyped,
                kind: ParamKind::Rest,
            }],
            block: None,
            ret: Box::new(Ty::Untyped),
            effects: crate::effect::EffectSet::pure(),
        };
        let mut m = bare_method(params, Some(sig));
        let rel_param = Symbol::from("__rel");
        assert!(insert_rel_param(&mut m, &rel_param, &owner));

        let rel = m.params.iter().find(|p| p.name == rel_param).expect("__rel param");
        assert!(rel.keyword, "a keyword, not a positional, after a splat: {rel:?}");
        assert!(!rel.rest);
        assert!(rel.default.is_some(), "optional: carries a default");

        let Some(Ty::Fn { params, .. }) = &m.signature else {
            panic!("the signature must not be dropped");
        };
        let rel_ty_param = params.iter().find(|p| p.name == rel_param).expect("__rel in signature");
        assert_eq!(rel_ty_param.kind, ParamKind::Keyword { required: false });
        assert_eq!(rel_ty_param.ty, Ty::Relation { of: owner });
    }

    /// The untouched case: no rest param, `__rel` stays exactly what it
    /// always was — a positional with a default, inserted before any
    /// keywords, typed `Untyped` in the signature (unchanged from
    /// before this fix).
    #[test]
    fn non_rest_method_keeps_the_untyped_positional_default() {
        let owner = ClassId(Symbol::from("Widget"));
        let params = vec![Param::positional(Symbol::from("user"))];
        let sig = Ty::Fn {
            params: vec![crate::ty::Param {
                name: Symbol::from("user"),
                ty: Ty::Untyped,
                kind: ParamKind::Required,
            }],
            block: None,
            ret: Box::new(Ty::Untyped),
            effects: crate::effect::EffectSet::pure(),
        };
        let mut m = bare_method(params, Some(sig));
        let rel_param = Symbol::from("__rel");
        assert!(insert_rel_param(&mut m, &rel_param, &owner));

        let rel = m.params.iter().find(|p| p.name == rel_param).expect("__rel param");
        assert!(!rel.keyword, "unchanged: still a positional-with-default: {rel:?}");
        assert!(rel.default.is_some());

        let Some(Ty::Fn { params, .. }) = &m.signature else {
            panic!("the signature must not be dropped");
        };
        let rel_ty_param = params.iter().find(|p| p.name == rel_param).expect("__rel in signature");
        assert_eq!(rel_ty_param.kind, ParamKind::Optional);
        assert_eq!(rel_ty_param.ty, Ty::Untyped, "unchanged: still Untyped, not Relation");
    }

    /// `def f(*tags, **opts)`: no keyword may follow `**opts`, so `__rel:`
    /// goes ahead of it, in the params and in the signature.
    #[test]
    fn rest_and_kwrest_method_gets_rel_ahead_of_the_kwrest() {
        let owner = ClassId(Symbol::from("Widget"));
        let kwrest = Param { keyword: true, ..Param::rest(Symbol::from("opts")) };
        let params = vec![Param::rest(Symbol::from("tags")), kwrest];
        let sig = Ty::Fn {
            params: vec![
                crate::ty::Param { name: Symbol::from("tags"), ty: Ty::Untyped, kind: ParamKind::Rest },
                crate::ty::Param { name: Symbol::from("opts"), ty: Ty::Untyped, kind: ParamKind::KeywordRest },
            ],
            block: None,
            ret: Box::new(Ty::Untyped),
            effects: crate::effect::EffectSet::pure(),
        };
        let mut m = bare_method(params, Some(sig));
        let rel_param = Symbol::from("__rel");
        assert!(insert_rel_param(&mut m, &rel_param, &owner));

        let names: Vec<&str> = m.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["tags", "__rel", "opts"]);
        let Some(Ty::Fn { params, .. }) = &m.signature else {
            panic!("the signature must not be dropped");
        };
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["tags", "__rel", "opts"]);
    }
}

#[cfg(test)]
mod spliced_namespace_tests {
    use super::*;

    // A namespace reopen written after a nested declaration lands inside
    // that declaration's file; the reopen's render requires its sibling
    // files and the declaration itself, and a file requiring itself is
    // CRuby's "loading in progress, circular require considered harmful".
    #[test]
    fn a_reopen_spliced_into_a_declarations_file_does_not_require_that_file() {
        let own = "module Outer\n  module Inner\n    module Leaf\n      def self.x = 1\n    end\n  end\nend\n";
        let rendered = "require_relative \"sibling\"\nrequire_relative \"leaf\" unless defined?(::Outer::Inner::Leaf)\nmodule Outer\n  module Inner\n    Leaf.x\n  end\nend\n";
        let spliced = spliced_into_namespace(own, "Outer::Inner", "Outer::Inner::Leaf", rendered, "# marker").expect("splices");
        assert!(spliced.contains("require_relative \"sibling\""), "{spliced}");
        assert!(!spliced.contains("require_relative \"leaf\""), "{spliced}");
        assert!(spliced.contains("Leaf.x"), "{spliced}");
    }
}
