//! Where an ordinary `super` goes once its lookup leaves the app's own
//! declarations, for the Ruby family, which keeps `super(…, **h)` and a
//! zsuper as written and lets the interpreter bind them.
//!
//! As written is only the native binding when the method the lookup
//! reaches has the native argument ABI. Three kinds of code can sit
//! there in a Ruby-family tree:
//!
//! - Ruby core and stdlib (the vendored RBS names, `stdlib_rbs`): the
//!   interpreter's own methods, unless the runtime or the app reopens a
//!   core namespace with a method of that name (`touched`).
//! - A runtime *port* ([`SIGNATURE_PORTS`]): a file whose methods follow
//!   the native source they port, each with a parity test against the
//!   native library. Its method is the native declaration.
//! - Any other runtime file (`runtime/ruby/active_record/base.rb`'s
//!   `save!` takes no arguments where Rails' takes `**options`), and any
//!   name none of these declare (a gem the tree does not carry, a
//!   constant the slice left out): the ABI is unknown, so the caller
//!   refuses.
//!
//! The lookup is Ruby's: a class's prepends (last first), its own
//! methods, its includes (last first), then its superclass. Constants
//! are resolved lexically in the file that writes them; a name that
//! could also mean something nested ([`World::shadowed`]) is unknown.

use std::sync::OnceLock;

use crate::det::{HashMap, HashSet};

/// Runtime files whose methods follow the native source: each says so in
/// its header and has a parity test (`tests/*_runtime.rs`) that runs the
/// same observations against the native library and the port.
/// The bounded `ActiveSupport::Cache::MemoryStore` model the tree carries
/// when app code does not name the cache port.
const MEMORY_STORE_MODEL: &str = "runtime/spinel/active_support_cache_memory.rb";

pub(super) const SIGNATURE_PORTS: &[&str] = &[
    "runtime/spinel/active_support_cache.rb",
    "runtime/spinel/active_support_mem_cache_store.rb",
    "runtime/spinel/active_support_file_store.rb",
    "runtime/spinel/active_support_file_update_checker.rb",
    "runtime/spinel/active_support_benchmark.rb",
    "runtime/spinel/active_support_configuration_file.rb",
    "runtime/spinel/active_support_testing_time_helpers.rb",
    "runtime/spinel/active_support_messages.rb",
    "runtime/spinel/active_model_type.rb",
];

/// What the app declares of an instance method on a namespace outside
/// it: by reopening it, or through a module it mixes in there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reopened {
    /// Nothing: the namespace's own lookup decides.
    No,
    /// A source declaration in the reopening the Ruby family writes with
    /// its source argument contract: the lookup ends there.
    Native,
    /// A mixin, an unverified reopening, or a declaration whose contract
    /// the tree does not keep as written: the ABI is unknown.
    Unknown,
}

/// One step of the lookup through a name outside the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// The lookup ends at a declaration with the native ABI.
    Bound,
    /// A module that does not declare the method: the lookup goes on.
    Continue,
    /// The lookup reaches code whose ABI is unknown.
    Refuse,
}

#[derive(Default)]
struct Namespace {
    is_module: bool,
    /// Declared by some file outside [`SIGNATURE_PORTS`].
    lowered: bool,
    /// `None` written; `Some(None)` written but not resolvable.
    superclass: Option<Option<String>>,
    prepends: Vec<Option<String>>,
    includes: Vec<Option<String>>,
    methods: HashSet<String>,
}

/// A core or stdlib namespace as its RBS declares it.
#[derive(Default)]
struct CoreNamespace {
    is_module: bool,
    superclass: Option<Written>,
    includes: Vec<Written>,
}

pub(super) struct World {
    runtime: HashMap<String, Namespace>,
    /// A runtime constant alias of a namespace, to the full name it names.
    aliases: HashMap<String, Option<String>>,
    stdlib: HashSet<String>,
    core: HashMap<String, CoreNamespace>,
    /// Every proper `::`-suffix of a runtime or stdlib name.
    suffixes: HashSet<String>,
    /// The last names of the mixins the runtime writes that do not
    /// resolve.
    unresolved_mixins: HashSet<String>,
}

pub(super) fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let mut stdlib = HashSet::default();
        let mut core: HashMap<String, CoreNamespace> = HashMap::default();
        for (_, text) in crate::stdlib_rbs::documents() {
            if let Ok(names) = crate::rbs::declared_names(text) {
                stdlib.extend(names.namespaces);
                stdlib.extend(names.aliases.into_iter().map(|alias| alias.full_name));
            }
            if let Ok(signature) = ruby_rbs::node::parse(text) {
                for declaration in signature.declarations().iter() {
                    scan_rbs(&declaration, &mut Vec::new(), &mut core);
                }
            }
        }
        let mut scans = Vec::new();
        for (path, text) in crate::runtime_files::ruby_sources() {
            // The bounded MemoryStore model never ships beside the port
            // above (`project::drop_memory_store_model`); scanning both
            // would read each `MemoryStore` method as defined twice.
            if path == MEMORY_STORE_MODEL {
                continue;
            }
            let mut scan = Scan { port: SIGNATURE_PORTS.contains(&path), ..Scan::default() };
            let parsed = ruby_prism::parse(text.as_bytes());
            ruby_prism::Visit::visit(&mut scan, &parsed.node());
            scans.push(scan);
        }
        World::from_scans(scans, stdlib, core)
    })
}

/// A written RBS type name: `::A::B` keeps its `::`.
fn rbs_name(name: &ruby_rbs::node::TypeNameNode<'_>) -> String {
    let namespace = name.namespace();
    let mut segments: Vec<String> = namespace
        .path()
        .iter()
        .filter_map(|segment| match segment {
            ruby_rbs::node::Node::Symbol(symbol) => Some(symbol.as_str().to_string()),
            _ => None,
        })
        .collect();
    segments.push(name.name().as_str().to_string());
    let written = segments.join("::");
    if namespace.absolute() { format!("::{written}") } else { written }
}

fn scan_rbs(node: &ruby_rbs::node::Node<'_>, cref: &mut Vec<String>, out: &mut HashMap<String, CoreNamespace>) {
    use ruby_rbs::node::Node;
    let (is_module, name, superclass, members) = match node {
        Node::Class(class) => (
            false,
            rbs_name(&class.name()),
            class.super_class().map(|s| rbs_name(&s.name())),
            class.members(),
        ),
        Node::Module(module) => (true, rbs_name(&module.name()), None, module.members()),
        _ => return,
    };
    let full = match (name.strip_prefix("::"), cref.last()) {
        (Some(absolute), _) => absolute.to_string(),
        (None, Some(scope)) => format!("{scope}::{name}"),
        (None, None) => name,
    };
    let written = |text: String, cref: &Vec<String>| Written { text, cref: cref.clone() };
    let superclass = superclass.map(|text| written(text, cref));
    cref.push(full.clone());
    let mut includes = Vec::new();
    for member in members.iter() {
        match &member {
            Node::Include(include) => includes.push(written(rbs_name(&include.name()), cref)),
            Node::Class(_) | Node::Module(_) => scan_rbs(&member, cref, out),
            _ => {}
        }
    }
    cref.pop();
    let entry = out.entry(full).or_default();
    entry.is_module |= is_module;
    if superclass.is_some() {
        entry.superclass = superclass;
    }
    entry.includes.extend(includes);
}

impl World {
    fn from_scans(scans: Vec<Scan>, stdlib: HashSet<String>, core: HashMap<String, CoreNamespace>) -> Self {
        let mut declared: HashSet<String> = stdlib.clone();
        for scan in &scans {
            declared.extend(scan.bodies.iter().map(|body| body.name.clone()));
            declared.extend(scan.aliases.iter().map(|(name, _)| name.clone()));
        }
        let mut suffixes = HashSet::default();
        for name in &declared {
            let mut rest = name.as_str();
            while let Some((_, tail)) = rest.split_once("::") {
                suffixes.insert(tail.to_string());
                rest = tail;
            }
        }
        let unresolved_mixins: HashSet<String> = scans
            .iter()
            .flat_map(|scan| {
                scan.bodies
                    .iter()
                    .flat_map(|body| body.prepends.iter().chain(&body.includes))
                    .chain(scan.mixins.iter().map(|(_, module, _)| module))
            })
            .filter(|written| resolve(written, &declared, &suffixes).is_none())
            .map(|written| written.text.rsplit("::").next().unwrap_or(&written.text).to_string())
            .collect();
        let resolve = |written: &Written| resolve(written, &declared, &suffixes);
        let mut runtime: HashMap<String, Namespace> = HashMap::default();
        for scan in &scans {
            for body in &scan.bodies {
                let entry = runtime.entry(body.name.clone()).or_default();
                entry.is_module |= body.is_module;
                entry.lowered |= !scan.port;
                if let Some(superclass) = &body.superclass {
                    entry.superclass = Some(superclass.as_ref().and_then(&resolve));
                }
                entry.prepends.extend(body.prepends.iter().map(&resolve));
                entry.includes.extend(body.includes.iter().map(&resolve));
                entry.methods.extend(body.methods.iter().cloned());
            }
        }
        // `Target.include(M)` / `Target.prepend(M)` written anywhere.
        for scan in &scans {
            for (target, module, prepend) in &scan.mixins {
                let Some(target) = resolve(target) else { continue };
                let module = resolve(module);
                let entry = runtime.entry(target).or_default();
                entry.lowered |= !scan.port;
                if *prepend {
                    entry.prepends.push(module);
                } else {
                    entry.includes.push(module);
                }
            }
        }
        let mut aliases = HashMap::default();
        for scan in &scans {
            for (name, target) in &scan.aliases {
                // Declared twice, or as a namespace too: not one answer.
                let target = resolve(target).filter(|_| !runtime.contains_key(name) && !aliases.contains_key(name));
                aliases.insert(name.clone(), target);
            }
        }
        World { runtime, aliases, stdlib, core, suffixes, unresolved_mixins }
    }

    /// The written name could resolve lexically to a nested declaration.
    pub(super) fn shadowed(&self, name: &str) -> bool {
        self.suffixes.contains(name)
    }

    /// The runtime or the stdlib declares `name`.
    pub(super) fn declares(&self, name: &str) -> bool {
        self.runtime_declares(name) || self.stdlib.contains(name)
    }

    /// The runtime declares `name` (in any file).
    pub(super) fn runtime_declares(&self, name: &str) -> bool {
        self.runtime.contains_key(name) || self.aliases.contains_key(name)
    }

    pub(super) fn stdlib_declares(&self, name: &str) -> bool {
        self.stdlib.contains(name)
    }

    /// The runtime namespaces that mix `module` in, each with whether it
    /// prepends it. `None` when a mixin the runtime writes that does not
    /// resolve ends in `module`'s last name (it may mean `module`), or
    /// `module` is aliased.
    pub(super) fn includers(&self, module: &str) -> Option<Vec<(&str, bool)>> {
        let last = module.rsplit("::").next().unwrap_or(module);
        if self.aliases.values().flatten().any(|target| target == module) || self.unresolved_mixins.contains(last) {
            return None;
        }
        let mut out = Vec::new();
        for (name, namespace) in &self.runtime {
            for (list, prepend) in [(&namespace.prepends, true), (&namespace.includes, false)] {
                for entry in list {
                    if entry.as_deref() == Some(module) {
                        out.push((name.as_str(), prepend));
                    }
                }
            }
        }
        out.sort_unstable();
        Some(out)
    }

    /// The lookup of instance method `method` in runtime class `target`
    /// from just after the module it mixes in as `module` (`prepend`
    /// says which list): the module's own includes, the earlier mixins
    /// of that list, then (after a prepend) the class's own methods and
    /// includes, then its superclass. The app's own reopening of `target` or mixin into it
    /// is answered as [`World::through`] answers it.
    pub(super) fn after(
        &self,
        target: &str,
        module: &str,
        prepend: bool,
        method: &str,
        app_reopens: &dyn Fn(&str, &str) -> Reopened,
    ) -> Step {
        let Some(namespace) = self.runtime.get(target) else { return Step::Refuse };
        if namespace.lowered || namespace.is_module {
            return Step::Refuse;
        }
        let reopened = app_reopens(target, method);
        if reopened == Reopened::Unknown {
            return Step::Refuse;
        }
        let list = if prepend { &namespace.prepends } else { &namespace.includes };
        let mut positions = list.iter().enumerate().filter(|(_, m)| m.as_deref() == Some(module)).map(|(i, _)| i);
        let (Some(position), None) = (positions.next(), positions.next()) else { return Step::Refuse };
        let other = if prepend { &namespace.includes } else { &namespace.prepends };
        if other.iter().any(|m| m.as_deref() == Some(module)) {
            return Step::Refuse;
        }
        let walk = |modules: &[Option<String>]| -> Step {
            for module in modules.iter().rev() {
                match module.as_deref() {
                    None => return Step::Refuse,
                    Some(module) => match self.step(module, method, false, app_reopens, &mut HashSet::default()) {
                        Step::Continue => {}
                        other => return other,
                    },
                }
            }
            Step::Continue
        };
        // The module's own includes follow it, then the mixins the
        // target wrote before it.
        let own = self.runtime.get(module).map_or(&[][..], |m| m.includes.as_slice());
        for modules in [own, &list[..position]] {
            match walk(modules) {
                Step::Continue => {}
                other => return other,
            }
        }
        if prepend {
            if reopened == Reopened::Native {
                return Step::Bound;
            }
            if namespace.methods.contains(method) {
                return Step::Bound;
            }
            match walk(&namespace.includes) {
                Step::Continue => {}
                other => return other,
            }
        }
        match &namespace.superclass {
            None => self.through("Object", method, true, app_reopens),
            Some(None) => Step::Refuse,
            Some(Some(parent)) => self.through(parent, method, true, app_reopens),
        }
    }

    /// Every instance method namespace `name` declares itself has the
    /// native ABI: it is a core or stdlib namespace, or a port's, and
    /// the runtime prepends nothing to it (which a `super` from a
    /// subclass would reach first).
    pub(super) fn own_methods_native(&self, name: &str) -> bool {
        if self.aliases.contains_key(name) {
            return false;
        }
        match self.runtime.get(name) {
            Some(namespace) => !namespace.lowered && namespace.prepends.is_empty(),
            None => self.stdlib.contains(name),
        }
    }

    /// The core namespaces a lookup through core `name` may visit: its
    /// RBS superclass chain (`Object` when none is written) and the
    /// modules each includes. `None` when one of them does not resolve.
    fn core_ancestry(&self, name: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::default();
        let mut pending = vec![(name.to_string(), true)];
        while let Some((node, as_class)) = pending.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            out.push(node.clone());
            let declared = self.core.get(&node);
            let resolve = |written: &Written| resolve(written, &self.stdlib, &self.suffixes);
            for include in declared.map_or(&[][..], |d| d.includes.as_slice()) {
                pending.push((resolve(include)?, false));
            }
            if !as_class || declared.is_some_and(|d| d.is_module) {
                continue;
            }
            match declared.and_then(|d| d.superclass.as_ref()) {
                Some(superclass) => pending.push((resolve(superclass)?, true)),
                None if node == "BasicObject" => {}
                None => pending.push(("Object".to_string(), true)),
            }
        }
        Some(out)
    }

    /// The runtime declares instance method `method` on namespace `name`:
    /// in its own body, or in a module it prepends or includes there
    /// (followed through the runtime's own modules). A name the runtime
    /// writes but that does not resolve may declare anything; a core or
    /// stdlib module mixed in declares only core methods, whose ABI is
    /// native.
    fn runtime_declares_method<'w>(&'w self, name: &'w str, method: &str, seen: &mut HashSet<&'w str>) -> bool {
        if !seen.insert(name) {
            return false;
        }
        if let Some(target) = self.aliases.get(name) {
            return target.as_deref().is_none_or(|target| self.runtime_declares_method(target, method, seen));
        }
        let Some(namespace) = self.runtime.get(name) else { return false };
        namespace.methods.contains(method)
            || namespace
                .prepends
                .iter()
                .chain(&namespace.includes)
                .any(|module| module.as_deref().is_none_or(|module| self.runtime_declares_method(module, method, seen)))
    }

    /// The lookup of instance method `method` through `name`, entered as a
    /// superclass (`as_superclass`) or as a mixed-in module.
    /// `app_reopens(namespace, method)` answers whether the app declares
    /// `method` on a core namespace it reopens (or mixes into one).
    pub(super) fn through(
        &self,
        name: &str,
        method: &str,
        as_superclass: bool,
        app_reopens: &dyn Fn(&str, &str) -> Reopened,
    ) -> Step {
        self.step(name, method, as_superclass, app_reopens, &mut HashSet::default())
    }

    fn step<'w>(
        &'w self,
        name: &'w str,
        method: &str,
        as_superclass: bool,
        app_reopens: &dyn Fn(&str, &str) -> Reopened,
        seen: &mut HashSet<&'w str>,
    ) -> Step {
        if !seen.insert(name) {
            return Step::Continue;
        }
        if let Some(target) = self.aliases.get(name) {
            return match target {
                Some(target) => self.step(target, method, as_superclass, app_reopens, seen),
                None => Step::Refuse,
            };
        }
        if let Some(namespace) = self.runtime.get(name) {
            if namespace.lowered {
                return Step::Refuse;
            }
            // The app reopening the namespace, or mixing a module into
            // it, with `method`: that declaration is what the lookup may
            // reach first, not the runtime's.
            let reopened = app_reopens(name, method);
            if reopened == Reopened::Unknown {
                return Step::Refuse;
            }
            for module in namespace.prepends.iter().rev() {
                match module.as_deref() {
                    None => return Step::Refuse,
                    Some(module) => match self.step(module, method, false, app_reopens, seen) {
                        Step::Continue => {}
                        other => return other,
                    },
                }
            }
            if reopened == Reopened::Native || namespace.methods.contains(method) {
                return Step::Bound;
            }
            for module in namespace.includes.iter().rev() {
                match module.as_deref() {
                    None => return Step::Refuse,
                    Some(module) => match self.step(module, method, false, app_reopens, seen) {
                        Step::Continue => {}
                        other => return other,
                    },
                }
            }
            if namespace.is_module {
                return if as_superclass { Step::Refuse } else { Step::Continue };
            }
            return match &namespace.superclass {
                None => self.step("Object", method, true, app_reopens, seen),
                Some(None) => Step::Refuse,
                Some(Some(parent)) => self.step(parent, method, true, app_reopens, seen),
            };
        }
        if self.stdlib.contains(name) {
            // A namespace on the way that the runtime or the app reopens
            // with `method` (declared there, or in a module it mixes in)
            // is refused; a reopening that does not touch `method`
            // (runtime/spinel/active_support_tryable.rb's `class Object;
            // include ActiveSupport::Tryable`) leaves the core lookup as is.
            let Some(ancestry) = self.core_ancestry(name) else { return Step::Refuse };
            if ancestry.iter().any(|ancestor| {
                self.runtime_declares_method(ancestor, method, &mut HashSet::default())
                    || app_reopens(ancestor, method) != Reopened::No
            }) {
                return Step::Refuse;
            }
            // A core class's ancestry is core; a core module may or may not
            // declare the method, and either way what follows decides.
            return if as_superclass { Step::Bound } else { Step::Continue };
        }
        Step::Refuse
    }
}

/// A constant as written, with the lexical scopes around it (innermost
/// last), for resolution once every file is read.
#[derive(Clone)]
struct Written {
    text: String,
    cref: Vec<String>,
}

fn resolve(written: &Written, declared: &HashSet<String>, suffixes: &HashSet<String>) -> Option<String> {
    if let Some(absolute) = written.text.strip_prefix("::") {
        return declared.contains(absolute).then(|| absolute.to_string());
    }
    for scope in written.cref.iter().rev() {
        let candidate = format!("{scope}::{}", written.text);
        if declared.contains(&candidate) {
            return Some(candidate);
        }
    }
    // Inside a namespace, a top-level answer is exact only when nothing
    // nested could answer first (a constant of a cref's ancestor).
    if !written.cref.is_empty() && suffixes.contains(&written.text) {
        return None;
    }
    declared.contains(&written.text).then(|| written.text.clone())
}

struct Body {
    name: String,
    is_module: bool,
    superclass: Option<Option<Written>>,
    prepends: Vec<Written>,
    includes: Vec<Written>,
    methods: Vec<String>,
}

#[derive(Default)]
struct Scan {
    port: bool,
    /// Full names of the open namespaces, innermost last.
    cref: Vec<String>,
    /// Index into `bodies` of each open namespace.
    open: Vec<usize>,
    bodies: Vec<Body>,
    /// `(target, module, prepend)` for `Target.include(M)`.
    mixins: Vec<(Written, Written, bool)>,
    /// `Name = Other::Constant`, by the alias's full name.
    aliases: Vec<(String, Written)>,
}

fn text(node: &ruby_prism::Node<'_>) -> String {
    String::from_utf8_lossy(node.location().as_slice()).trim().to_string()
}

fn is_constant(node: &ruby_prism::Node<'_>) -> bool {
    node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some()
}

fn symbol(node: &ruby_prism::Node<'_>) -> Option<String> {
    node.as_symbol_node()
        .map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
        .or_else(|| node.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()))
}

impl Scan {
    fn written(&self, text: String) -> Written {
        Written { text, cref: self.cref.clone() }
    }

    fn enter(&mut self, path: &ruby_prism::Node<'_>, is_module: bool, superclass: Option<Option<Written>>, body: Option<ruby_prism::Node<'_>>) {
        let written = text(path);
        let name = match (written.strip_prefix("::"), self.cref.last()) {
            (Some(absolute), _) => absolute.to_string(),
            (None, Some(scope)) => format!("{scope}::{written}"),
            (None, None) => written,
        };
        self.bodies.push(Body {
            name: name.clone(),
            is_module,
            superclass,
            prepends: Vec::new(),
            includes: Vec::new(),
            methods: Vec::new(),
        });
        self.open.push(self.bodies.len() - 1);
        self.cref.push(name);
        if let Some(body) = body {
            ruby_prism::Visit::visit(self, &body);
        }
        self.cref.pop();
        self.open.pop();
    }

    fn method(&mut self, name: String) {
        if let Some(&open) = self.open.last() {
            self.bodies[open].methods.push(name);
        }
    }
}

impl<'pr> ruby_prism::Visit<'pr> for Scan {
    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        let superclass = node.superclass().map(|s| is_constant(&s).then(|| self.written(text(&s))));
        self.enter(&node.constant_path(), false, superclass, node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        self.enter(&node.constant_path(), true, None, node.body());
    }

    fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}

    fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
        let value = node.value();
        if is_constant(&value) {
            let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
            let full = match self.cref.last() {
                Some(scope) => format!("{scope}::{name}"),
                None => name,
            };
            let target = self.written(text(&value));
            self.aliases.push((full, target));
        }
    }

    fn visit_lambda_node(&mut self, _: &ruby_prism::LambdaNode<'pr>) {}

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if node.receiver().is_none() {
            self.method(String::from_utf8_lossy(node.name().as_slice()).into_owned());
        }
    }

    fn visit_alias_method_node(&mut self, node: &ruby_prism::AliasMethodNode<'pr>) {
        if let Some(name) = symbol(&node.new_name()) {
            self.method(name);
        }
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
        let args: Vec<ruby_prism::Node<'pr>> =
            node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        match (node.receiver(), name.as_str()) {
            (None, "include" | "prepend") => {
                if let Some(&open) = self.open.last() {
                    for arg in &args {
                        let module = self.written(text(arg));
                        let list = if name == "prepend" {
                            &mut self.bodies[open].prepends
                        } else {
                            &mut self.bodies[open].includes
                        };
                        list.push(module);
                    }
                }
                return;
            }
            (Some(receiver), "include" | "prepend") if is_constant(&receiver) => {
                for arg in &args {
                    let target = self.written(text(&receiver));
                    let module = self.written(text(arg));
                    self.mixins.push((target, module, name == "prepend"));
                }
                return;
            }
            (None, "attr_reader" | "attr_writer" | "attr_accessor") => {
                for arg in &args {
                    if let Some(attr) = symbol(arg) {
                        if name != "attr_writer" {
                            self.method(attr.clone());
                        }
                        if name != "attr_reader" {
                            self.method(format!("{attr}="));
                        }
                    }
                }
                return;
            }
            (None, "alias_method" | "define_method") => {
                if let Some(method) = args.first().and_then(symbol) {
                    self.method(method);
                }
                return;
            }
            _ => {}
        }
        // `private def m` and its kin: the arguments, never a block body,
        // which another object may run as a method of its own.
        if node.receiver().is_none() {
            for arg in &args {
                ruby_prism::Visit::visit(self, arg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signature_port_claims_its_native_source() {
        for path in SIGNATURE_PORTS {
            let text = crate::runtime_files::read_to_string(path).expect("port is embedded");
            let header: String = text.lines().take(40).filter(|l| l.starts_with('#')).collect::<Vec<_>>().join("\n");
            assert!(header.contains("Each method follows"), "{path} does not claim its native source");
        }
    }

    #[test]
    fn a_port_method_binds_and_a_lowered_runtime_method_does_not() {
        let world = world();
        let none = |_: &str, _: &str| Reopened::No;
        let initialize_reopened = |namespace: &str, method: &str| {
            if namespace == "Object" && method == "initialize" { Reopened::Native } else { Reopened::No }
        };
        // MemoryStore < Store, prepending Strategy::LocalCache.
        assert_eq!(world.through("ActiveSupport::Cache::MemoryStore", "read_entry", true, &none), Step::Bound);
        assert_eq!(world.through("ActiveSupport::Cache::MemoryStore", "fetch_multi", true, &none), Step::Bound);
        assert_eq!(world.through("ActiveSupport::Cache::MemCacheStore", "delete_entry", true, &none), Step::Bound);
        // runtime/ruby/active_record/base.rb is not a port.
        assert_eq!(world.through("ActiveRecord::Base", "save!", true, &none), Step::Refuse);
        // The runtime's Logger stands in for the stdlib one.
        assert_eq!(world.through("Logger", "add", true, &none), Step::Refuse);
        // Core: the interpreter's own, unless reopened.
        assert_eq!(world.through("StandardError", "initialize", true, &none), Step::Bound);
        assert_eq!(world.through("StandardError", "initialize", true, &initialize_reopened), Step::Refuse);
        assert_eq!(world.through("Comparable", "initialize", false, &none), Step::Continue);
        // runtime/spinel/active_support_tryable.rb reopens Object to
        // include ActiveSupport::Tryable: `try` is the runtime's there,
        // and every other core method stays the interpreter's.
        assert_eq!(world.through("StandardError", "try", true, &none), Step::Refuse);
        assert_eq!(world.through("StandardError", "method_missing", true, &none), Step::Bound);
        // A port's constant alias of its own class.
        assert_eq!(world.through("ActiveRecord::Type::Value", "deserialize", true, &none), Step::Bound);
        // Neither the runtime nor Ruby declares it.
        assert_eq!(world.through("GraphQL::Pagination::Connection", "initialize", true, &none), Step::Refuse);
        // `Digest` inside `module ActiveSupport` is ActiveSupport::Digest.
        assert!(world.shadowed("Digest"));
    }
}
