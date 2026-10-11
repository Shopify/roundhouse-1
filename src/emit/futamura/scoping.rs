//! Which models may carry a `default_scope`.
//!
//! `Model.all` applies a model's default scopes, and the specialized block
//! is built once and cached, so a default scope that reads request state
//! (`default_scope { where(shop: Current.shop) }`) would be frozen into the
//! statement. A chain on such a model stays Rails'. Rails records default
//! scopes per class and inherits them down the superclass chain
//! (`ActiveRecord::Scoping::Default::ClassMethods#default_scopes`,
//! activerecord/lib/active_record/scoping/default.rb), and a module reaches
//! a class through `include`/`prepend`/`extend`, transitively. So a model
//! is judged by its own source, its superclass chain, and every module it
//! pulls in, followed through their sources.
//!
//! The walk reads source text rather than the IR: an `included do` block's
//! `default_scope` call is not modeled. A reference is resolved the way Ruby
//! resolves a constant lexically: through the namespaces enclosing it,
//! innermost first, to the app sources declaring a class or module of that
//! qualified name (names compare without case or underscores, so an app
//! acronym inflection still matches its file). A constant with no app
//! source is a gem's or the framework's, whose
//! scopes Rails reports at run time (`default_scopes`, which the runtime
//! checks before it builds a statement). An include whose argument is not
//! a constant path cannot be followed, so the model stays residue.

use std::collections::{HashMap, HashSet};

use crate::App;
use crate::dialect::Model;

/// A constant reference and the namespaces enclosing it.
#[derive(Clone)]
struct Ref {
    namespace: Vec<String>,
    path: Vec<String>,
    rooted: bool,
}

/// What one source file says about scopes and the modules it pulls in.
#[derive(Clone, Default)]
struct Facts {
    /// A non-comment line mentions `default_scope`.
    default_scope: bool,
    /// An `include`/`prepend`/`extend` whose argument is not a constant path.
    dynamic: bool,
    /// Included modules and superclasses.
    refs: Vec<Ref>,
    /// Normalized qualified names of the classes and modules it declares.
    declares: Vec<String>,
}

pub struct DefaultScopes<'a> {
    app: &'a App,
    /// Normalized qualified constant name -> sources declaring it.
    by_name: HashMap<String, Vec<usize>>,
    facts: Vec<Facts>,
}

/// `ApiClient`, `API_client` and `api_client` are one name here.
fn normalize(segment: &str) -> String {
    segment.chars().filter(|c| *c != '_').map(|c| c.to_ascii_lowercase()).collect()
}

/// Namespaces the framework owns: never an app source, and its own scopes
/// are reported by `default_scopes` at run time.
const FRAMEWORK_ROOTS: [&str; 12] = [
    "ActiveRecord", "ActiveSupport", "ActiveModel", "ActionController", "ActionView", "ActiveJob", "ActiveStorage",
    "ActionText", "ActionMailer", "Rails", "Comparable", "Enumerable",
];

fn is_framework(path: &str) -> bool {
    let root = path.trim_start_matches("::").split("::").next().unwrap_or("");
    FRAMEWORK_ROOTS.contains(&root)
}

fn is_const_path(s: &str) -> bool {
    let s = s.strip_prefix("::").unwrap_or(s);
    !s.is_empty()
        && s.split("::").all(|seg| {
            seg.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn segments(path: &str) -> Vec<String> {
    path.split("::").filter(|s| !s.is_empty()).map(normalize).collect()
}

/// The first whitespace-delimited token of `rest`, minus call parentheses.
fn token(rest: &str) -> &str {
    let rest = rest.trim_start_matches(['(', ' ', '\t']);
    rest.split(|c: char| c.is_whitespace() || c == ')' || c == ',').next().unwrap_or("")
}

fn scan(text: &str) -> Facts {
    let mut facts = Facts::default();
    // Enclosing `class`/`module` declarations by indentation: the nesting
    // of well-formatted Ruby, which is what the namespace is read from.
    let mut stack: Vec<(usize, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let namespace: Vec<String> = stack.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
        if trimmed.contains("default_scope") {
            facts.default_scope = true;
        }
        for keyword in ["include", "prepend", "extend"] {
            let Some(rest) = trimmed.strip_prefix(keyword) else { continue };
            if !rest.starts_with([' ', '\t', '(']) {
                continue;
            }
            // `include A, B` mixes in both; `extend self` is the module's own.
            for arg in rest.split(',') {
                let arg = token(arg);
                if arg == "self" || arg.is_empty() || is_framework(arg) {
                    continue;
                }
                if is_const_path(arg) {
                    facts.refs.push(Ref { namespace: namespace.clone(), path: segments(arg), rooted: arg.starts_with("::") });
                } else {
                    facts.dynamic = true;
                }
            }
        }
        let declaration = trimmed.strip_prefix("class ").map(|r| (r, true)).or_else(|| trimmed.strip_prefix("module ").map(|r| (r, false)));
        if let Some((rest, is_class)) = declaration {
            let name = rest.split(|c: char| c.is_whitespace() || c == '<' || c == ';').next().unwrap_or("");
            if is_const_path(name) {
                let declared = segments(name);
                let mut qualified = if name.starts_with("::") { Vec::new() } else { namespace.clone() };
                qualified.extend(declared.iter().cloned());
                facts.declares.push(qualified.join("::"));
                if is_class
                    && let Some((_, parent)) = rest.split_once('<')
                {
                    let parent = token(parent);
                    if is_const_path(parent) && !is_framework(parent) {
                        facts.refs.push(Ref { namespace: namespace.clone(), path: segments(parent), rooted: parent.starts_with("::") });
                    }
                }
                stack.push((indent, declared));
            }
        }
    }
    facts
}

impl<'a> DefaultScopes<'a> {
    pub fn new(app: &'a App) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut all = Vec::with_capacity(app.sources.len());
        for (i, s) in app.sources.iter().enumerate() {
            let counted = s.path.ends_with(".rb") && !s.path.contains("/test/") && !s.path.starts_with("test/") && !s.path.contains("/spec/");
            let facts = if counted { scan(&s.text) } else { Facts::default() };
            for name in &facts.declares {
                let entry = by_name.entry(name.clone()).or_default();
                if !entry.contains(&i) {
                    entry.push(i);
                }
            }
            all.push(facts);
        }
        DefaultScopes { app, by_name, facts: all }
    }

    /// The sources a reference resolves to: lexical scopes innermost first,
    /// the first that names an app constant.
    fn resolve(&self, r: &Ref) -> &[usize] {
        let depths: Box<dyn Iterator<Item = usize>> =
            if r.rooted { Box::new(std::iter::once(0)) } else { Box::new((0..=r.namespace.len()).rev()) };
        for depth in depths {
            let mut key: Vec<&str> = r.namespace[..depth].iter().map(String::as_str).collect();
            key.extend(r.path.iter().map(String::as_str));
            if let Some(sources) = self.by_name.get(&key.join("::")) {
                return sources;
            }
        }
        &[]
    }

    /// Why `model` may have a default scope, or `None` when nothing it
    /// inherits or includes declares one.
    pub fn why(&self, model: &Model) -> Option<String> {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = Vec::new();
        let own = (model.span.file.0 as usize).wrapping_sub(1);
        if own < self.app.sources.len() {
            stack.push(own);
        }
        let name = segments(model.name.0.as_str());
        stack.extend(self.by_name.get(&name.join("::")).into_iter().flatten().copied());
        if let Some(parent) = &model.parent
            && !is_framework(parent.0.as_str())
        {
            let namespace = name[..name.len().saturating_sub(1)].to_vec();
            let parent = Ref { namespace, path: segments(parent.0.as_str()), rooted: false };
            stack.extend(self.resolve(&parent).iter().copied());
        }
        while let Some(source) = stack.pop() {
            if !seen.insert(source) {
                continue;
            }
            let path = &self.app.sources[source].path;
            let facts = &self.facts[source];
            if facts.default_scope {
                return Some(format!("{path} declares a default_scope"));
            }
            if facts.dynamic {
                return Some(format!("{path} includes a module the specializer cannot resolve"));
            }
            for r in &facts.refs {
                stack.extend(self.resolve(r).iter().copied());
            }
        }
        None
    }
}
