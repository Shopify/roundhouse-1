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
use crate::dialect::{AccessorKind, LibraryClass, MethodDef, MethodReceiver, MethodVisibility};
use crate::expr::{Expr, ExprNode, InterpPart, LValue, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;
use crate::ty::Ty;

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
    let mut last_of_name: crate::det::HashMap<&str, usize> = crate::det::HashMap::default();
    for (i, lc) in lcs.iter().enumerate() {
        last_of_name.insert(lc.name.0.as_str(), i);
    }
    let mut out: Vec<EmittedFile> = Vec::new();
    let mut at_path: crate::det::HashMap<PathBuf, usize> = crate::det::HashMap::default();
    for (i, lc) in lcs.iter().enumerate().filter(|(_, lc)| owner_in_this_tree(lc).is_none()) {
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
        let without_apart;
        let lc = if apart.is_empty() {
            lc
        } else {
            let mut copy = lc.clone();
            copy.constants = lc
                .constants
                .iter()
                .enumerate()
                .filter(|(j, _)| !apart.iter().any(|(k, _)| k == j))
                .map(|(_, c)| c.clone())
                .collect();
            without_apart = copy;
            &without_apart
        };
        let mut files = emit_library_class_pair(lc, app, out_path);
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
        if last_of_name.get(lc.name.0.as_str()) == Some(&i) {
            if let Some(kids) = children.get(lc.name.0.as_str()) {
                splice_nested(&mut files, lc, kids, app);
            }
        }
        for file in files {
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
    }
    crate::timings::lap("emit library: render");
    out
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

/// `content` with `blocks` placed right after the parent's own header
/// — the `class` line at `header_indent` — or `None` when there is no
/// such line, since anywhere else would be outside the body.
fn spliced_after_header(content: &str, header_indent: &str, blocks: &[String]) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        out.push(line);
        if !inserted
            && line
                .strip_prefix(header_indent)
                .is_some_and(|rest| rest.starts_with("class "))
        {
            out.extend(blocks.iter().map(String::as_str));
            inserted = true;
        }
    }
    inserted.then(|| out.join("\n"))
}

/// Put each nested class's block inside its parent's file, before the
/// parent's own body — the order the source has, and the only one that
/// works when the parent's class body references them.
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
fn splice_nested(
    files: &mut [EmittedFile],
    parent: &LibraryClass,
    kids: &[&LibraryClass],
    app: &App,
) {
    let depth = parent.name.0.as_str().split("::").count();
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
                if waits_for.is_some() {
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
    let mut blocks: Vec<String> = Vec::new();
    let mut sidecar_blocks: Vec<String> = Vec::new();
    let mut hoisted: Vec<String> = Vec::new();
    // The file the blocks run in: a `__dir__` read in a child
    // (`File.join(__dir__, "lookup.sql")`) is anchored on it.
    let parent_rb = files.iter().find(|f| f.path.extension().is_some_and(|e| e == "rb")).map(|f| f.path.clone());
    for kid in kids {
        let stem = crate::naming::underscore(kid.name.0.as_str());
        let rb_path = PathBuf::from(format!("app/models/{stem}.rb"));
        let render = || emit_library_class_decl(kid, app, rb_path.clone());
        let rendered = match &parent_rb {
            Some(parent_rb) => super::expr::with_spliced_into(parent_rb, render),
            None => render(),
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
            let Some(rest) = line.strip_prefix("require_relative ") else { continue };
            let target = rest.trim().trim_matches('"');
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
            hoisted.push(format!("require_relative {rebased:?}"));
        }
        // A plain `require "x"` names a library, not a path, and is
        // carried as written.
        hoisted.extend(
            lines
                .iter()
                .filter(|l| l.starts_with("require \""))
                .map(|l| l.to_string()),
        );
        let body: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| !l.starts_with("require"))
            .collect();
        let Some(block) = unwrapped(&body, depth) else { continue };
        blocks.push(block);
        // The sidecar `emit_library_class_pair` would have written for
        // the child's own file, minus the same wrapper. Its type names
        // were resolved inside the child's enclosing segments, which
        // are the parent's followed by the child's own — the position
        // it is spliced into.
        let sidecar = super::rbs::emit_library_class_rbs(kid, &rb_path);
        let lines: Vec<&str> = sidecar.content.lines().collect();
        if let Some(block) = unwrapped(&lines, depth) {
            sidecar_blocks.push(block);
        }
    }
    if blocks.is_empty() {
        return;
    }
    let own_header_indent = "  ".repeat(depth - 1);
    let Some(rb) = files.iter_mut().find(|f| f.path.extension().is_some_and(|e| e == "rb")) else {
        return;
    };
    let Some(spliced) = spliced_after_header(&rb.content, &own_header_indent, &blocks) else {
        return;
    };
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
    let Some(rbs) = files.iter_mut().find(|f| f.path.extension().is_some_and(|e| e == "rbs"))
    else {
        return;
    };
    if let Some(spliced) = spliced_after_header(&rbs.content, &own_header_indent, &sidecar_blocks) {
        rbs.content = format!("{spliced}\n");
    }
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
        let scopes = crate::lower::rich_text::preload_scopes(model)
            .into_iter()
            .chain(plain_scopes)
            .chain(crate::lower::attached::preload_scopes(model));
        for (n, assoc) in scopes {
            let n = n.as_str().to_string();
            // A declared scope of the same name wins — it has a real
            // body, and shadowing it would drop a filter.
            // A Relation builtin is never delegated, same rule as above.
            if by_name.contains_key(&n) || RELATION_BUILTINS.contains(&n.as_str()) {
                continue;
            }
            preloads.insert(n, assoc.as_str().to_string());
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
    for (name, assoc) in &preloads {
        body.push_str("\n    # Preload scope (Rails' `includes`): the association it names\n");
        body.push_str("    # joins this relation's preload specs, batched at `to_a`.\n");
        writeln!(body, "    def {name}").unwrap();
        writeln!(body, "      preload(:{assoc})\n    end").unwrap();
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
fn insert_rel_param(m: &mut crate::dialect::MethodDef, rel_param: &Symbol) -> bool {
    if m.params.iter().any(|p| p.forwarding) {
        crate::emit::diagnostics::report_unsupported(m.name_span, "ruby", "full argument forwarding",
            "full forwarding cannot use the relation-threading argument ABI");
        return false;
    }
    let insert_at = m.params.iter().position(|p| p.keyword).unwrap_or(m.params.len());
    m.params.insert(
        insert_at,
        crate::dialect::Param::with_default(
            rel_param.clone(),
            crate::lower::model_to_library::relation_new_self(),
        ),
    );
    if let Some(crate::ty::Ty::Fn { params, .. }) = &mut m.signature {
        let at = params
            .iter()
            .position(|p| matches!(p.kind, crate::ty::ParamKind::Keyword { .. }))
            .unwrap_or(params.len());
        params.insert(
            at,
            crate::ty::Param {
                name: rel_param.clone(),
                ty: crate::ty::Ty::Untyped,
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
    // A model-root query needs the same Relation seed even when the app
    // declares no scopes. Otherwise `Widget.order(...)` reaches no method
    // and `Widget.where.not(...)` reaches Base.where with no argument.
    // Keep the whole-app gate consistent with the per-body gate below.
    let mut wants_model_chain = false;
    crate::lower::for_each_hook_body_ref(app, &mut |body| {
        wants_model_chain = wants_model_chain
            || crate::lower::scope_chain::mentions_model_chain_start(body, &models);
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
    let mut wants_assoc_lookup = false;
    crate::lower::for_each_hook_body_ref(app, &mut |body| {
        wants_assoc_lookup = wants_assoc_lookup
            || crate::lower::scope_chain::mentions_assoc_lookup(body, &assocs)
            || crate::lower::scope_chain::mentions_assoc_constructor(body, &assocs)
            || crate::lower::scope_chain::mentions_assoc_alias(body, &assocs);
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
    let regs = crate::lower::scope_chain::Registries {
        scopes: &scopes,
        models: &models,
        assocs: &assocs,
        assoc_class_methods: &assoc_class_methods,
        user_returns: &user_returns,
        unique_keys: &unique_keys,
        track_targets: super::expr::native_keywords() && app_reads_association_state(app),
    };
    drop(_span);
    let _span = crate::timings::begin("scope lowering: bodies");
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
                    if !insert_rel_param(m, &rel_param) { continue; }
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
                if !insert_rel_param(m, &rel_param) { continue; }
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
            let class_self = (m.receiver == MethodReceiver::Class)
                .then(|| if is_model { Some(lc.name.clone()) } else { concern_owner.clone() })
                .flatten();
            // A model's own INSTANCE methods know their self model too —
            // `self.<has_many>.<scope>` there seeds a Relation from the
            // association's foreign key (recent_threads' comment chain).
            let instance_self = (m.receiver == MethodReceiver::Instance)
                .then(|| if is_model { Some(lc.name.clone()) } else { concern_owner.clone() })
                .flatten();
            if crate::lower::scope_chain::mentions_scope(&m.body, &names)
                || crate::lower::scope_chain::mentions_model_chain_start(&m.body, &models)
                || crate::lower::scope_chain::mentions_assoc_constructor(&m.body, &assocs)
                || crate::lower::scope_chain::mentions_assoc_lookup(&m.body, &assocs)
                || crate::lower::scope_chain::mentions_assoc_alias(&m.body, &assocs)
                || crate::lower::scope_chain::mentions_assoc_extension(&m.body, &assocs)
                || crate::lower::scope_chain::mentions_model_insert_all(&m.body, &models)
                || crate::lower::scope_chain::mentions_assoc_class_method(
                    &m.body,
                    &assocs,
                    &names,
                    &assoc_class_methods,
                )
                || (class_self.is_some()
                    && crate::lower::scope_chain::mentions_bare_chain_start(&m.body))
            {
                crate::lower::scope_chain::rewrite_call_site(
                    &mut m.body,
                    &regs,
                    class_self.as_ref(),
                    instance_self.as_ref(),
                );
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
    // belongs_to, load the target on assignment and invalidate an
    // inherited key's writer; the other trees keep the non-polymorphic
    // memoization and their own key writers.
    let native = super::expr::native_keywords();
    let stale_state = native && app_calls_association(app);
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        let mut own = belongs_to(model);
        if !native {
            own.retain(|(_, _, type_column)| type_column.is_none());
        }
        for (name, _, _) in &own {
            memoize_reader(lc, name);
            if native {
                load_on_assign(lc, name, stale_state);
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
/// the CRuby and JRuby trees, and only where the app reads the state
/// (`app_reads_association_state`): elsewhere nothing can observe the
/// difference but a re-query's freshness.
pub(crate) fn apply_collection_memoization(lcs: &mut [LibraryClass], app: &App) {
    use crate::dialect::Association;

    if !super::expr::native_keywords() || !app_reads_association_state(app) {
        return;
    }
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        // (name, :through or HABTM, singular)
        let names: Vec<(Symbol, bool, bool)> = model
            .associations()
            .filter_map(|a| match a {
                Association::HasMany { name, through: None, .. } => Some((name.clone(), false, false)),
                Association::HasOne { name, .. } => Some((name.clone(), false, true)),
                Association::HasMany { name, through: Some(_), .. }
                | Association::HasAndBelongsToMany { name, .. } => Some((name.clone(), true, false)),
                Association::BelongsTo { .. } => None,
            })
            .collect();
        for (name, through, singular) in names {
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
                memoize_query_reader(lc, &name);
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
fn memoize_query_reader(lc: &mut LibraryClass, name: &Symbol) {
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
    let value = exprs.pop().expect("len checked");
    let span = value.span;
    let syn = |n| Expr::new(span, n);
    exprs.push(syn(ExprNode::Assign { target: LValue::Ivar { name: cache.clone() }, value }));
    exprs.push(syn(ExprNode::Assign {
        target: LValue::Ivar { name: loaded },
        value: syn(ExprNode::Lit { value: Literal::Bool { value: true } }),
    }));
    exprs.push(syn(ExprNode::Ivar { name: cache }));
}

/// The app observes association state: it calls `association(x)`, or
/// reads `record.x.loaded?`, which `lower::assoc_loaded` writes as the
/// flat `x_loaded?`.
pub(crate) fn app_reads_association_state(app: &App) -> bool {
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { method, args, .. } = &*e.node {
            if args.is_empty() && method.as_str().ends_with("_loaded?") {
                *found = true;
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    if app_calls_association(app) {
        return true;
    }
    let mut found = false;
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, &mut found));
    for view in &app.views {
        walk(&view.body, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |root| walk(root, &mut found));
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

    if !super::expr::native_keywords() || !app_calls_association(app) {
        return;
    }
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        if lc.methods.iter().any(|m| m.name.as_str() == "association" && m.receiver == MethodReceiver::Instance) {
            continue; // the model's own wins
        }
        let mut arms = String::new();
        for assoc in model.associations() {
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
                    "BelongsToAssociation.new(self, :{}, {:?}, {}, true, {})",
                    name.as_str(),
                    target.0.as_str(),
                    key(foreign_key),
                    primary_key.as_ref().map_or_else(|| ":id".to_string(), key)
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
        let src = if arms.is_empty() {
            format!("def association(name)\n  {otherwise}\nend\n")
        } else {
            format!("def association(name)\n  case name.to_s\n{arms}  else {otherwise}\n  end\nend\n")
        };
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("apply_association_reader: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }
    }
}

/// Some emit-bound body, view or test calls `association(x)`, on a
/// receiver or on self, reads the `association(:x).target` that
/// `lower::assoc_loaded` collapsed onto a reader (`IrHint::
/// AssociationTarget`, which the Ruby emit writes back), or reloads or
/// resets a collection reader (`IrHint::AssociationProxy`, which runs
/// on the association object).
pub(crate) fn app_calls_association(app: &App) -> bool {
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if matches!(
            e.hint,
            Some(crate::expr::IrHint::AssociationTarget | crate::expr::IrHint::AssociationProxy)
        ) {
            *found = true;
            return;
        }
        if let ExprNode::Send { method, args, .. } = &*e.node {
            if method.as_str() == "association" && args.len() == 1 {
                *found = true;
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    let mut found = false;
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, &mut found));
    for view in &app.views {
        walk(&view.body, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |root| walk(root, &mut found));
    found
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

/// `def <fk>=(value); @<fk> = value; end` gains `@<name>_loaded = false`,
/// and with `stale_state`, first records a loaded target going stale:
/// `if @<name>_loaded; @<name>_stale_state = <fk>; @<name>_stale_loaded
/// = true; end`.
fn invalidate_on_fk_write(lc: &mut LibraryClass, name: &Symbol, fk: &Symbol, stale_state: bool) {
    let writer = Symbol::from(format!("{}=", fk.as_str()));
    let Some(m) = lc
        .methods
        .iter_mut()
        .find(|m| m.name == writer && m.receiver == MethodReceiver::Instance)
    else {
        return;
    };
    let span = m.body.span;
    let reset = Expr::new(
        span,
        ExprNode::Assign {
            target: LValue::Ivar { name: Symbol::from(format!("{}_loaded", name.as_str())) },
            value: Expr::new(span, ExprNode::Lit { value: Literal::Bool { value: false } }),
        },
    );
    let mut stmts = match &*m.body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![m.body.clone()],
    };
    if stale_state {
        let n = name.as_str();
        let src = format!(
            "def __stale\n  if @{n}_loaded\n    @{n}_stale_state = {}\n    @{n}_stale_loaded = true\n  end\nend\n",
            fk.as_str()
        );
        let record = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("invalidate_on_fk_write: generated source failed to parse: {e}\n{src}"))
            .pop()
            .expect("one method");
        stmts.insert(0, record.body);
    }
    stmts.push(reset);
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
                    if method.as_str() == "type=" && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                ExprNode::Assign { target: LValue::Attr { name, .. }, value }
                    if name.as_str() == "type" =>
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
                let is_base_new = matches!(&*value.node,
                    ExprNode::Send { recv: Some(r), method, args, .. }
                        if method.as_str() == "new"
                            && args.is_empty()
                            && matches!(&*r.node, ExprNode::Const { path }
                                if const_path_is(path, &base)));
                if !is_base_new {
                    continue;
                }
                *value = sti_dispatch(value.clone(), &type_read, subs, value.span);
                break;
            }
        }
    }
}

fn const_path_is(path: &[Symbol], id: &ClassId) -> bool {
    let joined: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    joined.join("::") == id.0.as_str()
}

/// `case <type_read> when "<Sub>" then <Sub>.new … else <base_new> end`
fn sti_dispatch(base_new: Expr, type_read: &Expr, subs: &[ClassId], span: Span) -> Expr {
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
                    args: vec![],
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
///      a record with no id yet cannot be found again from the foreign
///      key, so something has to hold it until the save;
///   2. `_autosave_<name>` saves it and takes the id, folded into
///      `before_validation`.
///
/// ```ruby
/// def creator=(value)
///   if value.nil?
///     @creator_id = 0
///   else
///     @creator_id = value.id
///     if value.id == 0
///       @creator_cache = value
///       @creator_loaded = true
///     end
///   end
/// end
///
/// def _autosave_creator
///   if @creator_loaded && @creator_id == 0 && !@creator_cache.nil?
///     @creator_cache.save
///     @creator_id = @creator_cache.id
///   end
/// end
/// ```
///
/// A SAVED value deliberately does not populate the cache, so the
/// reader keeps re-querying by foreign key for those and a later direct
/// write to the key cannot be answered from a stale object.
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
        let names: Vec<(Symbol, Symbol)> = model
            .associations()
            .filter_map(|a| match a {
                Association::BelongsTo { name, foreign_key, polymorphic: false, .. } => {
                    Some((name.clone(), foreign_key.clone()))
                }
                _ => None,
            })
            .collect();
        for (name, fk) in names {
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
            lc.methods.push(autosave_method(&lc.name, &name, &fk));
            fold_before_validation(&mut lc.methods, &lc.name, &name);
        }
    }
}

/// `if value.id == 0 then @<name>_cache = value; @<name>_loaded = true end`
fn stash_unsaved(name: &Symbol, span: Span) -> Expr {
    let syn = |n| Expr::new(span, n);
    let value = || syn(ExprNode::Var { id: VarId(0), name: Symbol::from("value") });
    let id_read = syn(ExprNode::Send {
        recv: Some(value()),
        method: Symbol::from("id"),
        args: vec![],
        block: None,
        parenthesized: false,
    });
    syn(ExprNode::If {
        cond: syn(ExprNode::Send {
            recv: Some(id_read),
            method: Symbol::from("=="),
            args: vec![syn(ExprNode::Lit { value: Literal::Int { value: 0 } })],
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
    let fk_zero = syn(ExprNode::Send {
        recv: Some(syn(ExprNode::Ivar { name: fk.clone() })),
        method: Symbol::from("=="),
        args: vec![syn(ExprNode::Lit { value: Literal::Int { value: 0 } })],
        block: None,
        parenthesized: false,
    });
    let cond = and(
        and(
            syn(ExprNode::Ivar { name: Symbol::from(format!("{}_loaded", name.as_str())) }),
            fk_zero,
        ),
        send(send(cache(), "nil?"), "!"),
    );
    let body = syn(ExprNode::If {
        cond,
        then_branch: syn(ExprNode::Seq {
            exprs: vec![
                send(cache(), "save"),
                syn(ExprNode::Assign {
                    target: LValue::Ivar { name: fk.clone() },
                    value: send(cache(), "id"),
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
            let Some((joins, edge_table, edge_fk)) =
                resolve_through_chain(&app.models, model, thr_name, target, 0)
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
            m.body = through_reader_body(name, target, &join_sql, &where_sql, assoc_scope);
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
) -> Option<(Vec<String>, String, Symbol)> {
    use crate::dialect::Association;
    use crate::naming::pluralize_snake;

    if depth > 4 {
        return None;
    }
    // The through association on the owner (`:votes`, `:taggings`, `:tags`).
    let (thr_target, thr_fk, thr_through, as_interface) = model.associations().find_map(|a| match a {
        Association::HasMany { name, target, foreign_key, through, as_interface, .. }
            if name == thr_name =>
        {
            Some((target, foreign_key, through, as_interface))
        }
        _ => None,
    })?;
    // How the join model's rows tie back to the owner: directly by the
    // sibling's fk, or through the sibling's own chain.
    let (back_joins, edge_table, edge_fk) = match thr_through {
        None => (Vec::new(), pluralize_snake(thr_target.0.as_str()), thr_fk.clone()),
        Some(inner) => resolve_through_chain(models, model, inner, thr_target, depth + 1)?,
    };
    let thr_model = models.iter().find(|m| &m.name == thr_target)?;
    let thr_table = pluralize_snake(thr_target.0.as_str());
    let target_table = pluralize_snake(target.0.as_str());
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
    if let Some(src_fk) = thr_model.associations().find_map(|a| match a {
        Association::HasMany { target: t, foreign_key, through: None, .. } if t == target => {
            Some(foreign_key)
        }
        _ => None,
    }) {
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
        resolve_through_chain(models, thr_model, src_through, target, depth + 1)?;
    let mut joins = src_joins;
    joins.push(format!(
        "INNER JOIN {thr_table} ON {thr_table}.id = {src_edge_table}.{src_edge_fk}{owner_type}"
    ));
    joins.extend(back_joins);
    Some((joins, edge_table, edge_fk))
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
    for lc in lcs.iter_mut() {
        // CONTROLLERS in the index provide `helper_method`s to views:
        // only the call-site rewrite applies to them — their methods
        // must NOT flip class-side wholesale (actions are instance
        // methods; the class-side clone for the marked helpers comes
        // from the controller lowering itself).
        let is_helper_module = helper_modules.contains(&lc.name)
            && !app.controllers.iter().any(|c| c.name == lc.name);
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
        let own_instance_side: crate::det::HashSet<Symbol> = lc
            .methods
            .iter()
            .filter(|m| {
                m.receiver == MethodReceiver::Instance
                    || (is_helper_module && !is_singleton_side(m))
            })
            .filter(|m| !renames.contains_key(&(lc.name.clone(), m.name.clone())))
            .map(|m| m.name.clone())
            .collect();
        let own_class_side: crate::det::HashSet<Symbol> = lc
            .methods
            .iter()
            .filter(|m| m.receiver == MethodReceiver::Class)
            .map(|m| m.name.clone())
            .collect();
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
            // A reserved-word local (`class:`) is not assignable, so it
            // keeps the bang call.
            let is_lv = match &*r.node {
                ExprNode::Var { name, .. } => !crate::naming::is_reserved_local(name.as_str()),
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
            if (m == "request" || m == "cookies" || m == "session" || m == "flash"
                || m == "params")
                && args.is_empty()
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
                let (recv, meth) = if m == "request" {
                    (current, Symbol::from("request"))
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

    // Case 2: collapse `Rails.application.routes.url_helpers` to RouteHelpers.
    if is_rails_url_helpers(&expr.node) {
        *expr.node = ExprNode::Const { path: vec![Symbol::from("RouteHelpers")] };
        return;
    }

    // Bare `<x>_url` whose `<x>_path` sibling is generated — the
    // absolute variant grounds to protocol + configured domain + the
    // path helper (same convention as `rewrite_url_helpers_absolute`'s
    // host-kwarg form): `"#{Rails.application.protocol}#{
    // Rails.application.domain}#{RouteHelpers.<x>_path(args)}"`. Lobsters' hats page links
    // `request_hat_url` bare.
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
        if let Some(stem) = method.as_str().strip_suffix("_url") {
            let path_name = Symbol::from(format!("{stem}_path"));
            if route_helpers.contains(&path_name) {
                let span = expr.span;
                let args = args.clone();
                let domain = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(Expr::new(
                            span,
                            ExprNode::Send {
                                recv: Some(Expr::new(
                                    span,
                                    ExprNode::Const { path: vec![Symbol::from("Rails")] },
                                )),
                                method: Symbol::from("application"),
                                args: vec![],
                                block: None,
                                parenthesized: false,
                            },
                        )),
                        method: Symbol::from("domain"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                );
                let path_call = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(Expr::new(
                            span,
                            ExprNode::Const { path: vec![Symbol::from("RouteHelpers")] },
                        )),
                        method: path_name,
                        args,
                        block: None,
                        parenthesized: true,
                    },
                );
                *expr.node = ExprNode::StringInterp {
                    parts: vec![
                        crate::expr::InterpPart::Expr {
                            expr: crate::lower::view_to_library::rails_application_call("protocol"),
                        },
                        crate::expr::InterpPart::Expr { expr: domain },
                        crate::expr::InterpPart::Expr { expr: path_call },
                    ],
                };
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
pub(crate) fn apply_time_current_lowering(lcs: &mut [LibraryClass], app: &App) {
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            crate::lower::time_current::rewrite_time_current(&mut m.body, &app.time_formats);
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
/// order declaration (`lower::duration::apply_duration_rewrites`).
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
fn fixture_cache_ivar(label: &Symbol) -> Symbol {
    Symbol::from(format!("__fx_{}", label.as_str()))
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
    for lc in lcs.iter_mut() {
        for m in &mut lc.methods {
            crate::lower::duration::apply_duration_rewrites(&mut m.body, temporal_predicates);
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
/// The fk 0-sentinel convention stays (belongs_to writers store 0 for
/// nil); readers' `@fk == 0` guards are WIDENED to `@fk.nil? || @fk ==
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
    // authority for "does the inner reading have a target?".
    let class_names: crate::det::HashSet<String> = app
        .models
        .iter()
        .map(|m| m.name.0.as_str().to_string())
        .chain(app.library_classes.iter().map(|c| c.name.0.as_str().to_string()))
        .chain(lcs.iter().map(|c| c.name.0.as_str().to_string()))
        .collect();
    let known = |n: &str| class_names.contains(n);
    for lc in lcs.iter_mut() {
        let name = lc.name.0.as_str();
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
        for m in &mut lc.methods {
            root_shadowed_constants(&mut m.body, &shadowing, &prefixes, &known);
        }
    }
}

fn root_shadowed_constants(
    expr: &mut Expr,
    shadowing: &[String],
    prefixes: &[String],
    known: &dyn Fn(&str) -> bool,
) {
    expr.node
        .for_each_child_mut(&mut |c| root_shadowed_constants(c, shadowing, prefixes, known));
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
    if expr.decisions & crate::expr::RESOLVED_CLASS_REF != 0 {
        if let Some(crate::ty::Ty::Class { id, .. }) = &expr.ty {
            let resolved = id.0.as_str();
            for prefix in prefixes.iter().rev() {
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
    let rbs = super::rbs::emit_library_class_rbs(lc, &out_path);
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
    let rbs = super::rbs::emit_library_class_rbs(lc, &out_path);
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
    let rbs = super::rbs::emit_library_class_rbs(&lc, &out_path);
    vec![rb, rbs]
}

/// Emit only the `.rbs` sidecar for a `LibraryClass`. Used when the
/// `.rb` emit has bespoke post-processing the pair helpers can't
/// model (e.g. test files with autorun shim + preamble).
pub(super) fn emit_rbs_sidecar(lc: &LibraryClass, rb_path: &std::path::Path) -> EmittedFile {
    super::rbs::emit_library_class_rbs(lc, rb_path)
}

/// Emit only the `.rbs` sidecar derived from a `LibraryFunction` group.
/// Companion to `emit_rbs_sidecar` for module-shaped output whose `.rb`
/// emit flows through a bespoke path (e.g. `config/routes.rb`).
pub(super) fn emit_rbs_sidecar_from_funcs(
    funcs: &[crate::dialect::LibraryFunction],
    rb_path: &std::path::Path,
) -> EmittedFile {
    let lc = synthesize_module_lc(funcs);
    super::rbs::emit_library_class_rbs(&lc, rb_path)
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
        for m in &mut copy.methods {
            let Some(f) = full.into_iter().flatten().find(|f| f.name == m.name) else { continue };
            if m.receiver == MethodReceiver::Instance && m.params.is_empty() && m.block_param.is_none() {
                *m = crate::dialect::MethodDef { visibility: m.visibility, ..f.clone() };
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
    }
    let carrier_extends: Vec<&crate::ident::ClassId> = spliced_extends
        .iter()
        .copied()
        .chain(include_extends.iter().flat_map(|(_, carriers)| carriers.iter()))
        .collect();
    if !lc.top_level_constants.is_empty() {
        return emit_with_top_level_constants(lc, app, out_path, synthesized_siblings);
    }
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
    let (eager, deferred) = partition_deferred_constants(lc);
    let load_time_bodies = !deferred.is_empty();

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
        if !load_time && !load_time_bodies && anchor.starts_with("app/models/") {
            return None;
        }
        (anchor != self_anchor).then(|| relpath(&out_dir, &anchor))
    };
    let resolve_all = |e: &Expr, load_time: bool| -> BTreeSet<String> {
        let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
        walk_const_paths(e, &mut paths);
        paths.iter().filter_map(|p| resolve(p, load_time)).collect()
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
        walk_class_body_call_paths(call, &mut now, &mut later);
        body_requires.extend(now.iter().filter_map(|p| resolve(p, true)));
        body_requires.extend(later.iter().filter(|p| !now.contains(*p)).filter_map(|p| resolve(p, false)));
    }
    for &i in &deferred {
        body_requires.extend(resolve_all(&lc.constants[i].1, true));
    }
    // A class-variable initializer runs at load, after the constants.
    for init in &lc.class_ivar_initializers {
        body_requires.extend(resolve_all(init, true));
    }
    for m in &lc.methods {
        body_requires.extend(resolve_all(&m.body, false));
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
    let per_const: Vec<BTreeSet<String>> =
        eager.iter().map(|&i| resolve_all(&lc.constants[i].1, true)).collect();
    let readers = constant_readers(lc, app, &resolve);
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
    for r in &requires {
        writeln!(s, "require_relative {r:?}").unwrap();
    }
    if !requires.is_empty() {
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
    let outer_header = |i: usize, seg: &str| {
        let qualified = segments[..=i].join("::");
        let parent = outer_class_parent(&qualified, app);
        match (is_app_class(&qualified, app), parent) {
            (true, Some(p)) => format!("class {seg} < {}", lexically_qualified(p.0.as_str(), &segments[..i], app)),
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
                    let parent = lexically_qualified(&written, &segments[..depth - 1], app);
                    writeln!(s, "{pad}class {last} < {parent}").unwrap()
                }
                (_, None) => writeln!(s, "{pad}class {last}").unwrap(),
            }
        }
    };
    open_header(&mut s);

    let write_include = |s: &mut String, inc: &ClassId| {
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
    };
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
    let write_extend = |s: &mut String, carrier: &ClassId| {
        writeln!(s, "{body_pad}extend {}", lexically_qualified(carrier.0.as_str(), &segments, app)).unwrap();
    };
    let extends_of = |inc: &ClassId| -> Vec<ClassId> {
        include_extends.iter().filter(|(i, _)| *i == inc).flat_map(|(_, c)| c.iter().cloned()).collect()
    };
    for inc in &lc.includes {
        if positioned.iter().any(|(_, p)| *p == inc) {
            continue;
        }
        write_include(&mut s, inc);
        extends_of(inc).iter().for_each(|carrier| write_extend(&mut s, carrier));
    }
    for carrier in &spliced_extends {
        write_extend(&mut s, carrier);
    }
    for (_, inc) in &positioned {
        extends_of(inc).iter().for_each(|carrier| write_extend(&mut s, carrier));
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
        lc.unknown_calls.iter().filter(|c| !crate::analyze::is_include_refusal(c)).collect()
    } else {
        let own = modelled_class_body_calls(lc, app);
        // A Concern's `included do` that ingest gave every class it runs
        // on (or reported there) is not missing from any of them.
        let carried_block = |call: &Expr| {
            app.carried_included_blocks.contains(&lc.name)
                && matches!(&*call.node, crate::expr::ExprNode::Send { recv: None, method, block: Some(_), .. }
                    if method.as_str() == "included")
        };
        for call in &lc.unknown_calls {
            if !own.iter().any(|c| std::ptr::eq(*c, call))
                && !crate::analyze::is_include_refusal(call)
                && !carried_block(call)
            {
                report_dropped_class_body_call(lc, call);
            }
        }
        own
    };
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
    let first_own = if railtie { None } else { replayed.iter().position(|c| own_runs.runs(c)) };
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
    let render_constants = |s: &mut String, which: &[usize]| {
        for &i in which {
            for call in calls_before.get(&i).into_iter().flatten() {
                for line in super::emit_expr(call).lines() {
                    if line.is_empty() {
                        writeln!(s).unwrap();
                    } else {
                        writeln!(s, "{body_pad}{line}").unwrap();
                    }
                }
            }
            let (cname, value) = &lc.constants[i];
            let mut rendered = super::emit_expr(value);
            if rendered.trim().is_empty() {
                rendered = "nil".to_string();
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
            let pad = body_pad.as_str();
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
            // A stored class-body local stays out of `constants` and
            // `Foo::NAME`, as the local was (ingest `BodyLocal`).
            if cname.as_str().starts_with(crate::ingest::library_class::BODY_LOCAL_CONSTANT_PREFIX) {
                writeln!(s, "{body_pad}private_constant :{}", cname.as_str()).unwrap();
            }
        }
    };
    match &split {
        Some((at, reqs)) => {
            render_constants(&mut s, &eager[..*at]);
            for i in (0..depth).rev() {
                writeln!(s, "{}end", "  ".repeat(i)).unwrap();
            }
            writeln!(s).unwrap();
            for r in reqs {
                writeln!(s, "require_relative {r:?}").unwrap();
            }
            writeln!(s).unwrap();
            open_header(&mut s);
            render_constants(&mut s, &eager[*at..]);
        }
        None => render_constants(&mut s, &eager),
    }
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
    let late_at = lc
        .class_ivar_initializers
        .iter()
        .position(|init| is_self_call(init) || calls_own_class_method(init, &own_class_methods))
        .unwrap_or(lc.class_ivar_initializers.len());
    let (early_inits, late_inits) = lc.class_ivar_initializers.split_at(late_at);
    let mut body_effects: Vec<&Expr> = replayed.iter().copied().chain(early_inits.iter()).collect();
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
    let tail_effects: Vec<&Expr> = if railtie {
        Vec::new()
    } else {
        let own = crate::ingest::library_class::OwnClassCalls::new(&lc.name, &lc.methods);
        let carried = carried_class_macro_calls(lc, app);
        let wraps_own = |e: &Expr| carried.iter().any(|c| std::ptr::eq(*c, e)) && names_own_method(lc, e);
        let at = body_effects.iter().position(|e| own.runs(e) || wraps_own(e)).unwrap_or(body_effects.len());
        body_effects.split_off(at)
    };
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
    for effect in &body_effects {
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
    if !replayed.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }

    // A load-time conditional definition (`ConditionalDefinitions`) is
    // written whole, as the source has it, where its first member stands.
    let mut first = true;
    let mut written = vec![false; lc.conditional_definitions.len()];
    for (index, m) in lc.methods.iter().enumerate() {
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
            write_include(&mut s, inc);
            writeln!(s).unwrap();
        }
        for (_, call) in positioned_prepends.iter().filter(|(at, _)| *at == index) {
            writeln!(s, "{body_pad}{}", super::emit_expr(call)).unwrap();
            writeln!(s).unwrap();
        }
        match conditional {
            Some(at) => write_conditional_definitions(&mut s, lc, &lc.conditional_definitions[at], &body_pad),
            None => write_library_method(&mut s, lc, index, &body_pad),
        }
    }

    for (_, inc) in positioned.iter().filter(|(at, _)| *at >= lc.methods.len()) {
        writeln!(s).unwrap();
        write_include(&mut s, inc);
    }
    for (_, call) in positioned_prepends.iter().filter(|(at, _)| *at >= lc.methods.len()) {
        writeln!(s).unwrap();
        writeln!(s, "{body_pad}{}", super::emit_expr(call)).unwrap();
    }

    if !tail_effects.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for effect in &tail_effects {
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
    if !lc.visibility_overrides.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for o in &lc.visibility_overrides {
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
    }

    // Finite class-side initialization is lowered IR, not replay of a
    // framework DSL. Each statement runs once on this class object; the
    // ones from the first call to a class method this class defines (a
    // Concern macro writing its `class_attribute`) run here, after those
    // methods. Unset subclasses keep their ivar absent.
    if !late_inits.is_empty() && !lc.methods.is_empty() {
        writeln!(s).unwrap();
    }
    for init in late_inits {
        for line in super::emit_expr(init).lines() {
            writeln!(s, "{body_pad}{line}").unwrap();
        }
    }

    if !deferred.is_empty() {
        if !lc.methods.is_empty() {
            writeln!(s).unwrap();
        }
        render_constants(&mut s, &deferred);
    }

    for i in (0..depth).rev() {
        writeln!(s, "{}end", "  ".repeat(i)).unwrap();
    }

    EmittedFile { path: out_path, content: s }
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
    let target = bare(inc);
    let module = app.library_classes.iter().find(|m| m.is_module && bare(&m.name) == target)?;
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

/// `lc.methods[index]`, then its visibility.
fn write_library_method(s: &mut String, lc: &LibraryClass, index: usize, body_pad: &str) {
    let m = &lc.methods[index];
    // An alias copy is written as the native alias, so a `super` in
    // it keeps looking up the original's name, as in the source. The
    // alias takes the original's visibility; a different one on the
    // copy is a later visibility change, written after it.
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
    if lc.method_aliases.iter().any(|a| a.name == m.name && a.receiver == m.receiver)
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
            writeln!(s, "{body_pad}  protected :{}", m.name).unwrap();
            writeln!(s, "{body_pad}end").unwrap();
            None
        }
    };
    if let Some(directive) = directive {
        writeln!(s, "{body_pad}{directive} :{}", m.name).unwrap();
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
        let mut first = true;
        for member in &arm.methods {
            let Some(index) = lc.methods.iter().position(|m| member.is(m)) else { continue };
            if !first {
                writeln!(s).unwrap();
            }
            first = false;
            write_library_method(s, lc, index, &inner_pad);
        }
    }
    writeln!(s, "{body_pad}end").unwrap();
}

/// The method `lc.methods[index]` is a native alias of: it is recorded
/// as an alias copy, and the nearest def of the original's name on the
/// same side before it is the very def it was copied from (same header
/// and body spans; a replacing def has its own). Anything else (an
/// original redefined in place, or a copy whose original a later pass
/// dropped or moved) keeps the copied def.
fn native_alias_original(lc: &LibraryClass, index: usize) -> Option<&MethodDef> {
    let m = &lc.methods[index];
    let alias = lc.method_aliases.iter().find(|a| a.name == m.name && a.receiver == m.receiver)?;
    let original = lc.methods[..index]
        .iter()
        .rev()
        .find(|o| o.name == alias.original && o.receiver == m.receiver)?;
    (original.name_span == m.name_span
        && original.body.span == m.body.span
        && original.params.len() == m.params.len()
        && original.kind == m.kind)
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
        writeln!(s, "require_relative {r:?}").unwrap();
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
        for call in &other.unknown_calls {
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
            for call in &other.unknown_calls {
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
fn library_classes_named<'a>(name: &str, app: &'a App) -> Vec<&'a LibraryClass> {
    match NameIndex::current(app) {
        Some(index) => index
            .library_classes
            .get(name)
            .map(|js| js.iter().map(|&j| &app.library_classes[j]).collect())
            .unwrap_or_default(),
        None => app.library_classes.iter().filter(|lc| lc.name.0.as_str() == name).collect(),
    }
}

/// Whether `name` is a model or a library class of this app.
fn is_model_or_library_class(name: &str, app: &App) -> bool {
    is_model_named(name, app) || !library_classes_named(name, app).is_empty()
}

/// Split a class's constants into the ones that can be initialized
/// before its methods exist and the ones that can't. A constant defers
/// when its initializer dispatches to this class — a bare `new(…)` or a
/// receiverless call naming one of its own methods, or either of those
/// SPELLED WITH THE CLASS NAME (`Sound.new(…)`) — or when it reads a
/// constant that already deferred. A self-dispatch inside a STORED
/// closure doesn't count: that body runs on call, not on load. Returns
/// index lists so both groups keep source order.
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
fn partition_deferred_constants(lc: &LibraryClass) -> (Vec<usize>, Vec<usize>) {
    fn is_own_class(recv: &Option<Expr>, class_name: &str) -> bool {
        let Some(r) = recv else { return false };
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

    let own: crate::det::HashSet<&str> =
        lc.methods.iter().map(|m| m.name.as_str()).collect();
    let mut deferred_names: crate::det::HashSet<String> = crate::det::HashSet::default();
    let (mut eager, mut deferred) = (Vec::new(), Vec::new());
    for (i, (name, value)) in lc.constants.iter().enumerate() {
        if calls_self(value, &own, &deferred_names, lc.name.0.as_str()) {
            deferred_names.insert(name.as_str().to_string());
            written_constants(value, &mut deferred_names);
            deferred.push(i);
        } else {
            eager.push(i);
        }
    }
    (eager, deferred)
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
    if InlineDeclaration::active() || reopens_loaded_gem_class(lc, app) {
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
        if concern && library_classes_named(&carrier, app).iter().any(|c| c.is_module) {
            out.push(ClassId(Symbol::from(carrier.as_str())));
        }
    }
    // Ruby runs a Concern's deferred includes before its own extend, and
    // `include A, B` includes B first; the last extend has precedence.
    out.reverse();
    out
}

/// Whether a module body uses ActiveSupport::Concern's hooks, which
/// `splice_concerns_into_models` applies to each includer.
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
    lc.unknown_calls
        .iter()
        .filter(|c| {
            crate::ingest::sorbet_check::is_mixes_in(c)
                || extensions.iter().any(|e| std::ptr::eq(*e, *c))
                || prepends.iter().any(|(p, _)| std::ptr::eq(*p, *c))
                || claimed(c)
        })
        .collect()
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
    let port_module = |written: &str| -> Option<&'static str> {
        let name = written.trim_start_matches("::");
        if app.library_classes.iter().any(|own| own.name.0.as_str() == name) {
            return None;
        }
        crate::analyze::runtime_reads::ruby_family_runtime_names().get_key_value(name).map(|(k, _)| k.as_str())
    };
    let provided = |c: &LibraryClass| -> Vec<&'static str> {
        c.includes
            .iter()
            .filter_map(|inc| port_module(inc.0.as_str()))
            .filter_map(crate::analyze::runtime_reads::mixed_in_class_methods)
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
                    port_module(&path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::")).is_some()
                }
                _ => false,
            })
    };
    lc.unknown_calls
        .iter()
        .filter(|call| {
            if port_extend(call) {
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
            }
            continue;
        }
        let defines = |c: &LibraryClass| {
            c.methods.iter().any(|m| m.name == *method && m.receiver == MethodReceiver::Instance)
        };
        if extended.iter().any(|m| defines(m)) {
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
fn resolve_app_module<'a>(lc: &LibraryClass, path: &[Symbol], app: &'a App) -> Option<&'a LibraryClass> {
    let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
    let scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    (0..=scope.len()).rev().find_map(|n| {
        let name = if n == 0 { written.clone() } else { format!("{}::{written}", scope[..n].join("::")) };
        app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == name)
    })
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
                    *e = call(None, "warn", vec![lit(message)], true);
                }
                None => {}
            }
            return;
        }
    }
    e.node.for_each_child_mut(&mut |child| rewrite_program_requires(child, app, rel, sources));
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
                    match table.anchors.get(source) {
                        Some(anchor) => {
                            if !anchors.iter().any(|a| matches!(&*a.node, ExprNode::Lit { value: crate::expr::Literal::Str { value } } if value == anchor)) {
                                anchors.push(lit(span, anchor));
                            }
                        }
                        None => missing.push(lit(span, source)),
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
    // A global (`$VERBOSE = true`) is no local.
    locals.retain(|local| !local.starts_with('$'));
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
                    }
                }
                ProgramItem::Statement(expr) => {
                    let mut expr = carried_loads(expr, app, None, sources);
                    rewrite_program_requires(&mut expr, app, rel, sources);
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
                    match program_require_source(rel, feature, *relative, table) {
                        Some(Ok((source, anchor))) => {
                            // A file that only reopens what another
                            // program defines loads nothing of its own.
                            let anchors: Vec<String> = match table.declarations.get(&source) {
                                Some(declared) if declared.is_empty() => Vec::new(),
                                Some(declared) => {
                                    let mut anchors: Vec<String> =
                                        declared.iter().filter(|a| emitted.contains(*a)).cloned().collect();
                                    if anchors.is_empty() {
                                        anchors.push(anchor);
                                    }
                                    anchors
                                }
                                None => vec![anchor],
                            };
                            for anchor in anchors {
                                if loaded.insert(anchor.clone()) {
                                    require_anchor(s, &anchor);
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
                    for lc in classes {
                        let rendered = emit_library_class_decl(lc, app, out_path.to_path_buf());
                        lines(s, pad, &rendered.content);
                    }
                }
                ProgramItem::ClassEval { receiver, method, parts } => {
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
fn report_dropped_class_body_call(lc: &LibraryClass, call: &Expr) {
    let name = crate::lower::model_to_library::dropped_statement_name(call);
    crate::emit::diagnostics::push(crate::lower::residue_diagnostic(
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
    ));
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
    let segments: Vec<&str> = name.split("::").collect();
    for i in 0..segments.len().saturating_sub(1) {
        let prefix = segments[..=i].join("::");
        if owns_a_file(&prefix, app) {
            if written_apart(name, &prefix, app) {
                return name.to_string();
            }
            return prefix;
        }
    }
    name.to_string()
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
/// its body's only, since such a file reopens it without adding to it.
fn written_apart(name: &str, owner: &str, app: &App) -> bool {
    let mut child = body_files(name, app);
    child.extend(declared_files(name, app));
    let parent = body_files(owner, app);
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
        c.methods.iter().for_each(|m| add(&m.body.span));
        c.constants.iter().for_each(|(_, e)| add(&e.span));
        c.class_ivar_initializers.iter().for_each(|e| add(&e.span));
        c.unknown_calls.iter().for_each(|e| add(&e.span));
    }
    out
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
        if owner == self_name {
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
    if first == self_name {
        return None;
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

/// A class-body call's constant references, split into the ones its
/// load reads (`now`) and the ones inside a closure literal it only
/// passes along (`later`): `scope :x, -> { Plan.active }` stores the
/// lambda, so `Plan` is read when the scope runs, not when the body
/// does. A block (`included do ... end`) may run at once, so it counts
/// as `now`, as does everything that is not an argument.
fn walk_class_body_call_paths(e: &Expr, now: &mut BTreeSet<Vec<String>>, later: &mut BTreeSet<Vec<String>>) {
    match &*e.node {
        ExprNode::Lambda { .. } => walk_const_paths(e, later),
        ExprNode::Send { recv, args, block, .. } => {
            if let Some(r) = recv {
                walk_const_paths(r, now);
            }
            for a in args {
                walk_class_body_call_paths(a, now, later);
            }
            if let Some(b) = block {
                walk_const_paths(b, now);
            }
        }
        ExprNode::Hash { entries, .. } => {
            for (k, v) in entries {
                walk_const_paths(k, now);
                walk_class_body_call_paths(v, now, later);
            }
        }
        ExprNode::Array { elements, .. } => {
            for el in elements {
                walk_class_body_call_paths(el, now, later);
            }
        }
        _ => walk_const_paths(e, now),
    }
}

pub(super) fn walk_const_paths(e: &Expr, out: &mut BTreeSet<Vec<String>>) {
    match &*e.node {
        ExprNode::Const { path } => {
            out.insert(path.iter().map(|s| s.as_str().to_string()).collect());
        }
        ExprNode::Send { recv, args, block, .. } => {
            if let Some(r) = recv {
                walk_const_paths(r, out);
            }
            for a in args {
                walk_const_paths(a, out);
            }
            if let Some(b) = block {
                walk_const_paths(b, out);
            }
        }
        ExprNode::Apply { fun, args, block } => {
            walk_const_paths(fun, out);
            for a in args {
                walk_const_paths(a, out);
            }
            if let Some(b) = block {
                walk_const_paths(b, out);
            }
        }
        ExprNode::Hash { entries, .. } => {
            for (k, v) in entries {
                walk_const_paths(k, out);
                walk_const_paths(v, out);
            }
        }
        ExprNode::Array { elements, .. } => {
            for el in elements {
                walk_const_paths(el, out);
            }
        }
        ExprNode::StringInterp { parts } => {
            for p in parts {
                if let InterpPart::Expr { expr } = p {
                    walk_const_paths(expr, out);
                }
            }
        }
        ExprNode::BoolOp { left, right, .. } => {
            walk_const_paths(left, out);
            walk_const_paths(right, out);
        }
        ExprNode::Let { value, body, .. } => {
            walk_const_paths(value, out);
            walk_const_paths(body, out);
        }
        ExprNode::Lambda { body, .. } => walk_const_paths(body, out),
        // A type-assertion wrapper is transparent to reference edges —
        // the broadcast lowerings wrap their `Views::…` payload render
        // in `Cast { …, Str }`, and skipping it dropped the model →
        // views require.
        ExprNode::Cast { value, .. } => walk_const_paths(value, out),
        ExprNode::If { cond, then_branch, else_branch } => {
            walk_const_paths(cond, out);
            walk_const_paths(then_branch, out);
            walk_const_paths(else_branch, out);
        }
        ExprNode::Case { scrutinee, arms } => {
            walk_const_paths(scrutinee, out);
            for arm in arms {
                walk_const_paths(&arm.body, out);
            }
        }
        ExprNode::Seq { exprs } => {
            for e in exprs {
                walk_const_paths(e, out);
            }
        }
        ExprNode::Assign { value, .. } => walk_const_paths(value, out),
        ExprNode::Yield { args } => {
            for a in args {
                walk_const_paths(a, out);
            }
        }
        ExprNode::Raise { value } => walk_const_paths(value, out),
        ExprNode::RescueModifier { expr, fallback } => {
            walk_const_paths(expr, out);
            walk_const_paths(fallback, out);
        }
        ExprNode::Return { value } => walk_const_paths(value, out),
        ExprNode::Super { args, block } => {
            for a in args.iter().flatten() {
                walk_const_paths(a, out);
            }
            if let Some(b) = block {
                walk_const_paths(b, out);
            }
        }
        ExprNode::BeginRescue { body, rescues, else_branch, ensure, .. } => {
            walk_const_paths(body, out);
            for r in rescues {
                walk_const_paths(&r.body, out);
            }
            if let Some(e) = else_branch {
                walk_const_paths(e, out);
            }
            if let Some(e) = ensure {
                walk_const_paths(e, out);
            }
        }
        ExprNode::Next { value: Some(v) } => walk_const_paths(v, out),
        ExprNode::MultiAssign { value, .. } => walk_const_paths(value, out),
        ExprNode::While { cond, body, .. } => {
            walk_const_paths(cond, out);
            walk_const_paths(body, out);
        }
        ExprNode::Range { begin, end, .. } => {
            if let Some(b) = begin {
                walk_const_paths(b, out);
            }
            if let Some(e) = end {
                walk_const_paths(e, out);
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
    Through { target: String, join: String, group_col: String, order: Option<String> },
    /// `has_one_attached :<attr>`: one join over the attachment and blob
    /// tables for the whole record set, installing a row-bearing
    /// `ActiveStorage::Attached` on each record (`attr`, owner class).
    /// `variations` is the declared-variant list as Ruby source
    /// (`lower::attached::variations_ruby_source`), the proxy's fourth
    /// constructor argument.
    Attached { attr: String, owner: String, variations: String },
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
    use crate::naming::pluralize_snake;

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
                let thr_table = pluralize_snake(thr_target.0.as_str());
                let target_table = pluralize_snake(target.0.as_str());
                let owner_type = through_owner_type_predicate(&thr_table, as_interface, &model.name);
                out.push((
                    name.as_str().to_string(),
                    PreloadKind::Through {
                        target: target.0.as_str().to_string(),
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
            PreloadKind::Through { target, join, group_col, order } => {
                let table = crate::naming::pluralize_snake(target.as_str());
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
                // recurse into.
                PreloadKind::Attached { .. } => None,
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
