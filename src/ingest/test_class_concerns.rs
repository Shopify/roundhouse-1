//! A Concern's `included do … end` run on a test class that includes it.
//!
//! ```ruby
//! module Factories
//!   extend ActiveSupport::Concern
//!
//!   included do
//!     define_factories { factory(:plan, class: -> { Plan }) }
//!   end
//! end
//!
//! class PlanTest < ActiveSupport::TestCase
//!   include Factories      # `define_factories` runs on PlanTest here
//! end
//! ```
//!
//! `ActiveSupport::Concern#append_features` class_evals the block on the
//! includer where the `include` stands. The emitted module keeps no
//! hook (the tree carries no Concern), and a test class is none of the
//! includers `concern_accessors::splice_into_library_includers` supplies,
//! so the block's statements never ran and the includer found none of
//! what they declare.
//!
//! Here each test class gets the statements of every Concern it includes
//! (a Concern's own includes first, then its block, then what the block
//! includes) as class-body statements, carried as written
//! (`TestModule::body_statements`) and run in the order the includes
//! stand. A Concern a parent test class already includes is skipped, as
//! `append_features` skips a module already in the ancestry. A Concern
//! with a block item this does not carry (a `def`, a `class_attribute`, a
//! non-statement) is left to warn as before.

use crate::App;
use crate::ClassId;
use crate::Symbol;
use crate::det::{HashMap, HashSet};
use crate::dialect::{ModelBodyItem, TestBodyStatement};

/// Run on every test class that includes a Concern with a block. Fills
/// `App::test_carried_included_blocks` with the Concerns every including
/// test class got whole.
pub(super) fn carry(
    app: &mut App,
    sources: &[crate::span::SourceFile],
    carriers: &[super::library_class::ConcernClassMethodSpans],
    framework_shadows: &HashSet<ClassId>,
) {
    if app.concern_model_items.is_empty() || app.test_modules.is_empty() {
        return;
    }
    let top = |id: &ClassId| ClassId(Symbol::from(id.0.as_str().trim_start_matches("::")));
    let mut module_includes: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for lc in &app.library_classes {
        module_includes.entry(lc.name.clone()).or_default().extend(lc.includes.iter().map(top));
    }
    let verified: HashSet<ClassId> =
        super::class_configuration::verified_framework_concerns(carriers, &module_includes, framework_shadows);
    let mut defined: HashSet<String> = HashSet::default();
    for lc in &app.library_classes {
        defined.insert(lc.name.0.as_str().to_string());
        for (name, _) in &lc.constants {
            defined.insert(format!("{}::{}", lc.name.0.as_str(), name.as_str()));
        }
    }
    defined.extend(app.models.iter().map(|m| m.name.0.as_str().to_string()));

    // A block's `include M` modules, in the block's order.
    let block_includes = |concern: &ClassId| -> Vec<ClassId> {
        app.concern_model_items
            .get(concern)
            .into_iter()
            .flatten()
            .flat_map(|item| super::concern_accessors::included_modules(item, concern, &defined))
            .collect()
    };
    // The Concerns whose block runs on a test class with these direct
    // includes, each with the include it hangs off.
    let reach = |includes: &[ClassId], inherited: &HashSet<ClassId>| -> Vec<(usize, ClassId)> {
        fn visit(
            id: ClassId,
            root: usize,
            verified: &HashSet<ClassId>,
            module_includes: &HashMap<ClassId, Vec<ClassId>>,
            block_includes: &dyn Fn(&ClassId) -> Vec<ClassId>,
            inherited: &HashSet<ClassId>,
            seen: &mut HashSet<ClassId>,
            out: &mut Vec<(usize, ClassId)>,
        ) {
            if inherited.contains(&id) || !seen.insert(id.clone()) || !verified.contains(&id) {
                return;
            }
            for dependency in module_includes.get(&id).cloned().into_iter().flatten() {
                visit(dependency, root, verified, module_includes, block_includes, inherited, seen, out);
            }
            out.push((root, id.clone()));
            for included in block_includes(&id) {
                visit(included, root, verified, module_includes, block_includes, inherited, seen, out);
            }
        }
        let mut seen = HashSet::default();
        let mut out = Vec::new();
        for (root, id) in includes.iter().map(top).enumerate() {
            visit(id, root, &verified, &module_includes, &block_includes, inherited, &mut seen, &mut out);
        }
        out
    };

    let by_name: HashMap<&ClassId, usize> = app.test_modules.iter().enumerate().map(|(i, tm)| (&tm.name, i)).collect();
    // What a parent test class's chain already ran.
    let ancestry = |index: usize| -> HashSet<ClassId> {
        let mut inherited = HashSet::default();
        let mut at = app.test_modules[index].parent.as_ref();
        let mut seen = HashSet::default();
        while let Some(parent) = at.take() {
            let Some(&i) = by_name.get(parent) else { break };
            if i == index || !seen.insert(i) {
                break;
            }
            inherited.extend(reach(&app.test_modules[i].includes, &HashSet::default()).into_iter().map(|(_, id)| id));
            at = app.test_modules[i].parent.as_ref();
        }
        inherited
    };

    // Whether a Concern's block is only statements this carries.
    let carriable = |concern: &ClassId| -> bool {
        app.concern_model_items.get(concern).is_some_and(|items| {
            items.iter().all(|item| {
                matches!(item, ModelBodyItem::Unknown { .. }) && !super::concern_accessors::is_candidate(item)
            })
        })
    };

    let mut plans: Vec<(usize, Vec<(usize, ClassId)>)> = Vec::new();
    let mut hosted: HashSet<ClassId> = HashSet::default();
    let mut incomplete: HashSet<ClassId> = HashSet::default();
    for index in 0..app.test_modules.len() {
        let reached = reach(&app.test_modules[index].includes, &ancestry(index));
        let reached: Vec<_> = reached
            .into_iter()
            .filter(|(_, concern)| app.concern_model_items.get(concern).is_some_and(|items| !items.is_empty()))
            .collect();
        for (_, concern) in &reached {
            if carriable(concern) {
                hosted.insert(concern.clone());
            } else {
                incomplete.insert(concern.clone());
            }
        }
        if !reached.is_empty() {
            plans.push((index, reached));
        }
    }

    let mut done: Vec<(usize, Vec<TestBodyStatement>, Vec<(usize, ClassId)>)> = Vec::new();
    for (index, reached) in plans {
        let mut statements = Vec::new();
        let mut includes = Vec::new();
        for (root, concern) in reached {
            if !carriable(&concern) {
                continue;
            }
            for item in app.concern_model_items.get(&concern).into_iter().flatten() {
                let modules = super::concern_accessors::included_modules(item, &concern, &defined);
                if !modules.is_empty() {
                    includes.extend(modules.into_iter().map(|module| (root, module)));
                    continue;
                }
                let class_attributes: &[Symbol] = &[];
                let ModelBodyItem::Unknown { expr, .. } =
                    super::app::rehome_carried(item.clone(), &concern, &app.test_modules[index].name, class_attributes, &defined)
                else {
                    continue;
                };
                let label = sources
                    .get((expr.span.file.0 as usize).wrapping_sub(1))
                    .map(|file| (file.path.clone(), file.line_col(expr.span.start).0));
                statements.push(TestBodyStatement {
                    expr,
                    line: label.as_ref().map_or(0, |(_, line)| *line),
                    after_definitions: false,
                    after_setup_blocks: false,
                    after_teardown_blocks: false,
                    source: label.map(|(path, _)| path),
                });
            }
        }
        done.push((index, statements, includes));
    }

    for (index, statements, includes) in done {
        let tm = &mut app.test_modules[index];
        // Run where the `include` ran, ahead of the class's own statements.
        tm.body_statements.splice(0..0, statements);
        let mut offset = 0;
        let mut last_root = None;
        for (root, module) in includes {
            if tm.includes.iter().any(|own| top(own) == module) {
                continue;
            }
            if last_root != Some(root) {
                offset = 0;
                last_root = Some(root);
            }
            offset += 1;
            let at = (root + offset).min(tm.includes.len());
            tm.includes.insert(at, module);
        }
    }
    let mut hosted: Vec<ClassId> = hosted.into_iter().filter(|id| !incomplete.contains(id)).collect();
    hosted.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    app.test_carried_included_blocks = hosted;
}
