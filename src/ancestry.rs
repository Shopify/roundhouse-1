//! Ruby's ancestors of a class or module, answered in one place.
//!
//! Rubydex linearizes every class and module the sources declare, the
//! runtime's Ruby ports included (`ConstResolver::ancestry`): Ruby's own
//! order, with prepended modules ahead of the class and each included
//! module's own includes after it. That answer comes first. A name it
//! has no answer for (a class a lowering synthesized, or a gem's that
//! only an RBI declares) is linearized here, by the same rules, from the
//! links the caller's registry records (`Links`). A chain Rubydex left
//! partial at a gem's class continues through the registry's links, and
//! a module the registry records that Rubydex did not resolve (a gem's,
//! or one a lowering included) joins the chain after its includer.

use crate::analyze::{ConstResolver, SingletonAncestor};
use crate::ident::ClassId;

/// A registry's own record of each class's superclass and mixins, as
/// written in the IR (already resolved, `ingest::written_names`).
pub(crate) trait Links {
    fn parent(&self, id: &ClassId) -> Option<ClassId>;
    fn includes(&self, id: &ClassId) -> Vec<ClassId>;
    fn extends(&self, id: &ClassId) -> Vec<ClassId>;
}

impl<T: Links + ?Sized> Links for &T {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        (**self).parent(id)
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        (**self).includes(id)
    }
    fn extends(&self, id: &ClassId) -> Vec<ClassId> {
        (**self).extends(id)
    }
}

/// Beyond this many levels a chain is malformed (a cycle the registry
/// records); the walk stops rather than hang.
const DEPTH: usize = 256;

pub(crate) struct Ancestors<'a, L: Links> {
    resolver: Option<&'a ConstResolver>,
    links: L,
    /// Each linearization computed so far; the links do not change while
    /// the accessor lives.
    linearized: std::cell::RefCell<crate::det::HashMap<ClassId, std::rc::Rc<[ClassId]>>>,
    /// Whether the linearization in progress met a cycle.
    cut: std::cell::Cell<bool>,
}

impl<'a, L: Links> Ancestors<'a, L> {
    pub(crate) fn new(resolver: Option<&'a ConstResolver>, links: L) -> Self {
        Self { resolver, links, linearized: Default::default(), cut: Default::default() }
    }

    fn rubydex(&self, id: &ClassId) -> Option<&'a crate::analyze::Ancestry> {
        self.resolver?.ancestry(&root(id))
    }

    /// Ruby's `ancestors` of the class or module `id`, nearest first,
    /// `id` itself included.
    pub(crate) fn of(&self, id: &ClassId) -> Vec<ClassId> {
        self.linearize(&root(id), &mut Vec::new()).to_vec()
    }

    /// `path` holds the names being linearized, so a cycle the registry
    /// records ends at the name met again.
    fn linearize(&self, id: &ClassId, path: &mut Vec<ClassId>) -> std::rc::Rc<[ClassId]> {
        if let Some(chain) = self.linearized.borrow().get(id) {
            return chain.clone();
        }
        if path.contains(id) || path.len() > DEPTH {
            self.cut.set(true);
            return std::rc::Rc::from(vec![id.clone()]);
        }
        let outer_cut = self.cut.replace(false);
        path.push(id.clone());
        let chain: std::rc::Rc<[ClassId]> = self.compute(id, path).into();
        path.pop();
        // A chain cut short by a cycle depends on where the walk entered
        // it, so it is not kept.
        let cut = self.cut.get();
        if !cut {
            self.linearized.borrow_mut().insert(id.clone(), chain.clone());
        }
        self.cut.set(outer_cut || cut);
        chain
    }

    fn compute(&self, id: &ClassId, path: &mut Vec<ClassId>) -> Vec<ClassId> {
        if let Some(ancestry) = self.rubydex(id) {
            let mut chain = ancestry.instance.clone();
            if !ancestry.complete {
                // Continue past the last class Rubydex resolved, through
                // the registry's superclass of it.
                let last = chain.iter().rev().find(|ancestor| self.links.parent(ancestor).is_some()).cloned();
                if let Some(parent) = last.and_then(|last| self.links.parent(&last)).map(|parent| root(&parent))
                    && !chain.contains(&parent)
                {
                    for ancestor in self.linearize(&parent, path).iter() {
                        if !chain.contains(ancestor) {
                            chain.push(ancestor.clone());
                        }
                    }
                }
            }
            // A module the registry records that Rubydex did not resolve
            // (a lowering's include, or a gem's module) goes right after
            // the class or module that includes it, as `include` puts it.
            let mut index = 0;
            while index < chain.len() {
                let owner = chain[index].clone();
                let includes: Vec<ClassId> = self.links.includes(&owner).iter().map(root).collect();
                let mut count = 0;
                for (at, module) in includes.iter().enumerate() {
                    if chain.contains(module) || *module == owner {
                        continue;
                    }
                    // After every module the owner includes later (those
                    // come first in Ruby's order), else right after it.
                    let after = includes[at + 1..]
                        .iter()
                        .filter_map(|later| chain.iter().position(|ancestor| ancestor == later))
                        .max()
                        .map_or(index + 1, |position| position + 1);
                    let linearized: Vec<ClassId> = self
                        .linearize(module, path)
                        .iter()
                        .filter(|ancestor| !chain.contains(ancestor))
                        .cloned()
                        .collect();
                    count += linearized.len();
                    chain.splice(after..after, linearized);
                }
                index += 1 + count;
            }
            return chain;
        }
        let superclass: Vec<ClassId> =
            self.links.parent(id).map(|parent| self.linearize(&root(&parent), path).to_vec()).unwrap_or_default();
        // Each `include` puts the module, with its own ancestors, right
        // after the class: the last one included comes first. A module
        // already on the superclass's chain is not included again.
        let mut modules: Vec<ClassId> = Vec::new();
        for module in self.links.includes(id) {
            let module = root(&module);
            if module == *id {
                continue;
            }
            let added: Vec<ClassId> = self
                .linearize(&module, path)
                .iter()
                .filter(|ancestor| *ancestor != id && !superclass.contains(ancestor) && !modules.contains(ancestor))
                .cloned()
                .collect();
            modules.splice(0..0, added);
        }
        let mut chain = vec![id.clone()];
        chain.extend(modules);
        for ancestor in superclass {
            if !chain.contains(&ancestor) {
                chain.push(ancestor);
            }
        }
        chain
    }

    /// The superclass the class `id` writes, as Ruby binds it: the
    /// registry's record (already resolved, and what a lowering changed
    /// it to), else Rubydex's. A class that writes none has none here,
    /// not Ruby's implicit `Object`.
    pub(crate) fn parent(&self, id: &ClassId) -> Option<ClassId> {
        self.links
            .parent(id)
            .map(|parent| root(&parent))
            .or_else(|| self.rubydex(id).and_then(|ancestry| ancestry.superclass.clone()))
    }

    /// The superclass chain above a record whose written superclass is
    /// `parent`, nearest first: for a caller holding the record itself
    /// (a lowering's working copy), whose parent is the one to follow.
    pub(crate) fn above(&self, parent: Option<&ClassId>) -> Vec<ClassId> {
        parent.map(|parent| self.superclasses(parent)).unwrap_or_default()
    }

    /// `id` and its superclass chain, nearest first.
    pub(crate) fn superclasses(&self, id: &ClassId) -> Vec<ClassId> {
        let mut chain = vec![root(id)];
        while chain.len() <= DEPTH {
            let Some(parent) = self.parent(chain.last().expect("non-empty")) else { break };
            if chain.contains(&parent) {
                break;
            }
            chain.push(parent);
        }
        chain
    }

    /// The modules `id` itself includes, as the IR records them (the
    /// registry's, else Rubydex's).
    pub(crate) fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        let own = self.links.includes(id);
        if !own.is_empty() {
            return own.iter().map(root).collect();
        }
        self.rubydex(id).map(|ancestry| ancestry.includes.clone()).unwrap_or_default()
    }

    /// The modules `id` itself prepends (Rubydex's; the registry records
    /// none).
    pub(crate) fn prepends(&self, id: &ClassId) -> Vec<ClassId> {
        self.rubydex(id).map(|ancestry| ancestry.prepends.clone()).unwrap_or_default()
    }

    /// The modules `id` itself extends.
    pub(crate) fn extends(&self, id: &ClassId) -> Vec<ClassId> {
        let own = self.links.extends(id);
        if !own.is_empty() {
            return own.iter().map(root).collect();
        }
        self.rubydex(id).map(|ancestry| ancestry.extends.clone()).unwrap_or_default()
    }

    /// Where a class method of `id` is looked up, nearest first: for each
    /// class on the superclass chain, its singleton class and then the
    /// modules it extends, each with its own ancestors.
    pub(crate) fn singleton(&self, id: &ClassId) -> Vec<SingletonAncestor> {
        if let Some(chain) = self.rubydex(id).and_then(|ancestry| ancestry.singleton.clone())
            && chain.iter().filter(|entry| matches!(entry, SingletonAncestor::Of(_))).count() == self.superclasses(id).len()
        {
            return chain;
        }
        let mut chain: Vec<SingletonAncestor> = Vec::new();
        for class in self.superclasses(id) {
            chain.push(SingletonAncestor::Of(class.clone()));
            let mut extended: Vec<ClassId> = Vec::new();
            for module in self.extends(&class) {
                let added: Vec<ClassId> =
                    self.of(&module).into_iter().filter(|ancestor| !extended.contains(ancestor)).collect();
                extended.splice(0..0, added);
            }
            for module in extended {
                let entry = SingletonAncestor::Module(module);
                if !chain.contains(&entry) {
                    chain.push(entry);
                }
            }
        }
        chain
    }
}

/// `id` without the leading `::` a rooted include keeps
/// (`ingest::written_names`).
fn root(id: &ClassId) -> ClassId {
    match id.0.as_str().strip_prefix("::") {
        Some(name) => ClassId(crate::ident::Symbol::from(name)),
        None => id.clone(),
    }
}

/// The app's own classes as ingested: models, controllers and library
/// classes, by name (the first of a name wins, as the registries do).
pub(crate) struct AppLinks<'a> {
    classes: crate::det::HashMap<&'a ClassId, AppClass<'a>>,
}

#[derive(Clone, Copy)]
enum AppClass<'a> {
    Library(&'a crate::dialect::LibraryClass),
    Model(&'a crate::dialect::Model),
    Controller(&'a crate::dialect::Controller),
}

impl<'a> AppLinks<'a> {
    pub(crate) fn new(app: &'a crate::App) -> Self {
        Self::with_library(app, &app.library_classes)
    }

    /// `app`'s models and controllers, with `library` standing for its
    /// library classes (a lowering's working set).
    pub(crate) fn with_library(app: &'a crate::App, library: &'a [crate::dialect::LibraryClass]) -> Self {
        let mut classes = crate::det::HashMap::default();
        for lc in library {
            classes.entry(&lc.name).or_insert(AppClass::Library(lc));
        }
        for model in &app.models {
            classes.entry(&model.name).or_insert(AppClass::Model(model));
        }
        for controller in &app.controllers {
            classes.entry(&controller.name).or_insert(AppClass::Controller(controller));
        }
        Self { classes }
    }

    pub(crate) fn defines(&self, id: &ClassId) -> bool {
        self.classes.contains_key(id)
    }
}

impl Links for AppLinks<'_> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        match self.classes.get(id)? {
            AppClass::Library(lc) => lc.parent.clone(),
            AppClass::Model(model) => model.parent.clone(),
            AppClass::Controller(controller) => controller.parent.clone(),
        }
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        match self.classes.get(id) {
            Some(AppClass::Library(lc)) => lc.includes.clone(),
            Some(AppClass::Model(model)) => crate::analyze::model_includes(model),
            Some(AppClass::Controller(controller)) => crate::analyze::controller_includes(controller),
            None => Vec::new(),
        }
    }
    fn extends(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
}

/// A lowering's library classes, by name.
pub(crate) enum LibraryList<'a> {
    Indexed(crate::det::HashMap<&'a ClassId, &'a crate::dialect::LibraryClass>),
    Scan(&'a [crate::dialect::LibraryClass]),
}

impl<'a> LibraryList<'a> {
    /// Indexed once by name (the first of a name wins): for a caller
    /// that asks about many classes.
    pub(crate) fn new(items: &'a [crate::dialect::LibraryClass]) -> Self {
        let mut index = crate::det::HashMap::default();
        for item in items {
            index.entry(&item.name).or_insert(item);
        }
        Self::Indexed(index)
    }

    /// Searched in order: for a caller that asks about one class, where
    /// indexing the list would cost more than the walk.
    pub(crate) fn scan(items: &'a [crate::dialect::LibraryClass]) -> Self {
        Self::Scan(items)
    }

    fn get(&self, id: &ClassId) -> Option<&'a crate::dialect::LibraryClass> {
        match self {
            Self::Indexed(index) => index.get(id).copied(),
            Self::Scan(items) => items.iter().find(|item| item.name == *id),
        }
    }
}

impl Links for LibraryList<'_> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        self.get(id)?.parent.clone()
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        self.get(id).map(|item| item.includes.clone()).unwrap_or_default()
    }
    fn extends(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
}

/// Superclass links a caller computes (`parent` of each name), with no
/// mixins: for a walk over a collection none of the others describes.
pub(crate) struct ParentLinks<F: Fn(&ClassId) -> Option<ClassId>>(pub(crate) F);

impl<F: Fn(&ClassId) -> Option<ClassId>> Links for ParentLinks<F> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        (self.0)(id)
    }
    fn includes(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
    fn extends(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
}

/// A lowering's models, by name.
pub(crate) enum ModelList<'a> {
    Indexed(crate::det::HashMap<&'a ClassId, &'a crate::dialect::Model>),
    Scan(&'a [crate::dialect::Model]),
}

impl<'a> ModelList<'a> {
    /// Indexed once by name (the first of a name wins): for a caller
    /// that asks about many classes.
    pub(crate) fn new(items: &'a [crate::dialect::Model]) -> Self {
        let mut index = crate::det::HashMap::default();
        for item in items {
            index.entry(&item.name).or_insert(item);
        }
        Self::Indexed(index)
    }

    /// Searched in order: for a caller that asks about one class, where
    /// indexing the list would cost more than the walk.
    pub(crate) fn scan(items: &'a [crate::dialect::Model]) -> Self {
        Self::Scan(items)
    }

    fn get(&self, id: &ClassId) -> Option<&'a crate::dialect::Model> {
        match self {
            Self::Indexed(index) => index.get(id).copied(),
            Self::Scan(items) => items.iter().find(|item| item.name == *id),
        }
    }
}

impl Links for ModelList<'_> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        self.get(id)?.parent.clone()
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        self.get(id).map(|item| crate::analyze::model_includes(item)).unwrap_or_default()
    }
    fn extends(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
}

/// A lowering's controllers, by name.
pub(crate) enum ControllerList<'a> {
    Indexed(crate::det::HashMap<&'a ClassId, &'a crate::dialect::Controller>),
    Scan(&'a [crate::dialect::Controller]),
}

impl<'a> ControllerList<'a> {
    /// Indexed once by name (the first of a name wins): for a caller
    /// that asks about many classes.
    pub(crate) fn new(items: &'a [crate::dialect::Controller]) -> Self {
        let mut index = crate::det::HashMap::default();
        for item in items {
            index.entry(&item.name).or_insert(item);
        }
        Self::Indexed(index)
    }

    /// Searched in order: for a caller that asks about one class, where
    /// indexing the list would cost more than the walk.
    pub(crate) fn scan(items: &'a [crate::dialect::Controller]) -> Self {
        Self::Scan(items)
    }

    fn get(&self, id: &ClassId) -> Option<&'a crate::dialect::Controller> {
        match self {
            Self::Indexed(index) => index.get(id).copied(),
            Self::Scan(items) => items.iter().find(|item| item.name == *id),
        }
    }
}

impl Links for ControllerList<'_> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        self.get(id)?.parent.clone()
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        self.get(id).map(|item| crate::analyze::controller_includes(item)).unwrap_or_default()
    }
    fn extends(&self, _id: &ClassId) -> Vec<ClassId> {
        Vec::new()
    }
}

impl<'a> Ancestors<'a, AppLinks<'a>> {
    /// `app`'s classes, from the Rubydex answers ingest prepared and the
    /// app's own records.
    pub(crate) fn for_app(app: &'a crate::App) -> Self {
        Self::new(app.const_resolver.prepared(), AppLinks::new(app))
    }

    /// As `for_app`, with `library` standing for the app's library
    /// classes (a lowering's working set).
    pub(crate) fn for_library(app: &'a crate::App, library: &'a [crate::dialect::LibraryClass]) -> Self {
        Self::new(app.const_resolver.prepared(), AppLinks::with_library(app, library))
    }

    /// Whether the app itself declares `id` (a model, controller or
    /// library class).
    pub(crate) fn app_defines(&self, id: &ClassId) -> bool {
        self.links.defines(&root(id))
    }
}

impl<'a> Ancestors<'a, LibraryList<'a>> {
    /// `library`'s classes, from the Rubydex answers ingest prepared and
    /// a search of `library` (`LibraryList::scan`): for a caller asking
    /// about one class.
    pub(crate) fn for_list(app: &'a crate::App, library: &'a [crate::dialect::LibraryClass]) -> Self {
        Self::new(app.const_resolver.prepared(), LibraryList::scan(library))
    }
}

impl Links for crate::det::HashMap<ClassId, crate::analyze::ClassInfo> {
    fn parent(&self, id: &ClassId) -> Option<ClassId> {
        self.get(id)?.parent.clone()
    }
    fn includes(&self, id: &ClassId) -> Vec<ClassId> {
        self.get(id).map(|info| info.includes.clone()).unwrap_or_default()
    }
    fn extends(&self, id: &ClassId) -> Vec<ClassId> {
        self.get(id).map(|info| info.extends.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::det::HashMap;
    use crate::ident::Symbol;
    use crate::span::SourceFile;

    fn id(name: &str) -> ClassId {
        ClassId(Symbol::from(name))
    }

    fn names(ids: &[ClassId]) -> Vec<&str> {
        ids.iter().map(|id| id.0.as_str()).collect()
    }

    /// A registry with only the links a test gives it.
    #[derive(Default)]
    struct Registry {
        parents: HashMap<ClassId, ClassId>,
        includes: HashMap<ClassId, Vec<ClassId>>,
        extends: HashMap<ClassId, Vec<ClassId>>,
    }

    impl Links for Registry {
        fn parent(&self, id: &ClassId) -> Option<ClassId> {
            self.parents.get(id).cloned()
        }
        fn includes(&self, id: &ClassId) -> Vec<ClassId> {
            self.includes.get(id).cloned().unwrap_or_default()
        }
        fn extends(&self, id: &ClassId) -> Vec<ClassId> {
            self.extends.get(id).cloned().unwrap_or_default()
        }
    }

    const SOURCE: &str = "\
module Loud; end
module Quiet
  include Loud
end
module Tracked; end
module Finder; end
class Speaker
  include Tracked
  extend Finder
end
class Wrapped < Speaker
  prepend Loud
  include Quiet
end
module Outer
  module Mixin; end
  class Inner
    include Mixin
  end
end
";

    fn resolver() -> ConstResolver {
        ConstResolver::from_app_sources(&[SourceFile { path: "app/models/wrapped.rb".into(), text: SOURCE.into() }])
    }

    #[test]
    fn source_classes_and_modules_take_rubydex_s_linearization() {
        let resolver = resolver();
        let registry = Registry::default();
        let ancestors = Ancestors::new(Some(&resolver), &registry);
        // Ruby: Wrapped.ancestors.take(5) => [Loud, Wrapped, Quiet, Speaker, Tracked]
        // (`Loud` is prepended, so `Quiet`'s own include of it is not repeated).
        assert_eq!(names(&ancestors.of(&id("Wrapped"))[..5]), ["Loud", "Wrapped", "Quiet", "Speaker", "Tracked"]);
        assert_eq!(names(&ancestors.of(&id("Quiet"))[..2]), ["Quiet", "Loud"]);
        assert_eq!(names(&ancestors.of(&id("Outer::Inner"))[..2]), ["Outer::Inner", "Outer::Mixin"]);
        assert_eq!(ancestors.parent(&id("Wrapped")), Some(id("Speaker")));
        assert_eq!(names(&ancestors.superclasses(&id("Wrapped"))[..2]), ["Wrapped", "Speaker"]);
        assert_eq!(ancestors.extends(&id("Speaker")), [id("Finder")]);
        let ancestry = resolver.ancestry(&id("Wrapped")).unwrap();
        assert_eq!(ancestry.prepends, [id("Loud")]);
        assert_eq!(ancestry.includes, [id("Quiet")]);
    }

    #[test]
    fn a_class_rubydex_never_saw_is_linearized_from_the_registry() {
        let resolver = resolver();
        let mut registry = Registry::default();
        // A lowering's class under a source class, including a source
        // module and a module of its own.
        registry.parents.insert(id("Synthesized"), id("Wrapped"));
        registry.includes.insert(id("Synthesized"), vec![id("Own"), id("::Outer::Mixin")]);
        registry.includes.insert(id("Own"), vec![id("Loud")]);
        let ancestors = Ancestors::new(Some(&resolver), &registry);
        let chain = ancestors.of(&id("Synthesized"));
        // The last include first; `Loud`, already on the superclass's
        // chain, is not included again.
        assert_eq!(names(&chain[..6]), ["Synthesized", "Outer::Mixin", "Own", "Loud", "Wrapped", "Quiet"]);
        assert_eq!(names(&ancestors.superclasses(&id("Synthesized"))[..3]), ["Synthesized", "Wrapped", "Speaker"]);
    }

    #[test]
    fn a_registry_include_rubydex_never_saw_follows_the_class() {
        let resolver = resolver();
        let mut registry = Registry::default();
        registry.includes.insert(id("Speaker"), vec![id("Tracked"), id("RouteHelpers")]);
        let ancestors = Ancestors::new(Some(&resolver), &registry);
        assert_eq!(names(&ancestors.of(&id("Speaker"))[..3]), ["Speaker", "RouteHelpers", "Tracked"]);
    }

    #[test]
    fn class_methods_are_looked_up_through_each_singleton_and_its_extends() {
        let resolver = resolver();
        let registry = Registry::default();
        let ancestors = Ancestors::new(Some(&resolver), &registry);
        let chain = ancestors.singleton(&id("Wrapped"));
        let expected = [
            SingletonAncestor::Of(id("Wrapped")),
            SingletonAncestor::Of(id("Speaker")),
            SingletonAncestor::Module(id("Finder")),
        ];
        let positions: Vec<usize> = expected.iter().map(|entry| chain.iter().position(|e| e == entry).unwrap_or_else(|| panic!("{entry:?} in {chain:?}"))).collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{chain:?}");
    }

    #[test]
    fn a_registry_cycle_ends_the_walk() {
        let mut registry = Registry::default();
        registry.parents.insert(id("A"), id("B"));
        registry.parents.insert(id("B"), id("A"));
        registry.includes.insert(id("M"), vec![id("N")]);
        registry.includes.insert(id("N"), vec![id("M")]);
        registry.includes.insert(id("A"), vec![id("M")]);
        let ancestors = Ancestors::new(None, &registry);
        assert_eq!(names(&ancestors.superclasses(&id("A"))), ["A", "B"]);
        assert!(ancestors.of(&id("A")).len() <= 4 * DEPTH);
    }
}
