//! A class or module body registering one `autoload` per file of a
//! directory, by a glob over its own source tree:
//!
//! ```ruby
//! module ActiveMerchant
//!   module Billing
//!     load_path = Pathname.new(__FILE__ + '/../../..')
//!     Dir[File.dirname(__FILE__) + '/gateways/**/*.rb'].each do |filename|
//!       gateway_name      = File.basename(filename, '.rb')
//!       gateway_classname = "#{gateway_name}_gateway".camelize
//!       gateway_filename  = Pathname.new(filename).relative_path_from(load_path).sub_ext('')
//!       autoload(gateway_classname, gateway_filename)
//!     end
//!   end
//! end
//! ```
//!
//! (activemerchant 1.137.0's billing/gateways.rb). Its value is the set
//! of autoloads, which depends only on `__FILE__`, the directory listing
//! and pure string/path functions; so it is evaluated at compile time
//! over the compiled tree's own files, and each `autoload :XGateway,
//! "active_merchant/billing/gateways/x"` it registers is one the emitted
//! autoloader registers (`project`'s gem autoloads). Only that closed
//! vocabulary is read: `__FILE__`, string literals and interpolation,
//! `String#+`, `String#camelize`, `File.dirname`/`basename`/`join`,
//! `Pathname.new`, `Pathname#relative_path_from`/`sub_ext`/`to_s`, body
//! locals assigned from those, and `Dir[pattern]`/`Dir.glob(pattern)`
//! with `*`, `?` and `**/`. Anything else is not this shape, and the
//! statement stays the walk's (reported where it is dropped).

use ruby_prism::Node;

use crate::det::HashMap;

use super::util::{class_name_path, constant_id_str, flatten_statements, module_name_path};

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    Path(String),
    Sym(String),
}

impl Value {
    fn text(&self) -> &str {
        match self {
            Value::Str(s) | Value::Path(s) | Value::Sym(s) => s,
        }
    }
}

/// One glob loop of a body: the byte span of its statement and the
/// `(constant, feature)` autoloads it registers.
pub(crate) struct Loop {
    pub(crate) start: usize,
    /// The `(constant, feature)` pairs; the emitted autoloader registers
    /// the constant at the file defining it, so the feature is not read.
    pub(crate) autoloads: Vec<(String, String)>,
}

/// The glob loops among `statements` (one body, in order), evaluated
/// with `__FILE__` = `file`. `listing` is the tree's files; `None`
/// checks the shape only, running the block once on a path the pattern
/// matches.
pub(crate) fn body_loops(statements: &[Node<'_>], file: &str, listing: Option<&[String]>) -> Vec<Loop> {
    let mut locals: HashMap<String, Value> = HashMap::default();
    let mut out = Vec::new();
    for statement in statements {
        if let Some(write) = statement.as_local_variable_write_node() {
            let name = constant_id_str(&write.name()).to_string();
            match eval(&write.value(), file, &locals) {
                Some(value) => {
                    locals.insert(name, value);
                }
                None => {
                    locals.remove(&name);
                }
            }
            continue;
        }
        if let Some(autoloads) = glob_loop(statement, file, &locals, listing) {
            out.push(Loop { start: statement.location().start_offset(), autoloads });
        }
    }
    out
}

/// The full constant names a file's glob loops autoload, each under the
/// class or module whose body registers it.
pub(crate) fn file_autoloads(source: &str, file: &str, listing: &[String]) -> Vec<String> {
    let result = ruby_prism::parse(source.as_bytes());
    let root = result.node();
    let mut out = Vec::new();
    if let Some(program) = root.as_program_node() {
        let statements: Vec<Node<'_>> = program.statements().body().iter().collect();
        declarations(&statements, &[], file, listing, &mut out);
    }
    out
}

/// The absolute directory `__FILE__` stands under while a file's glob
/// requires are evaluated: what a pattern read off `__FILE__` or
/// `__dir__` starts with, and a literal pattern (relative to the
/// process's working directory in Ruby, not to the file) does not.
const TREE_ROOT: &str = "/\u{0}tree";

/// A file's top-level glob requires, each `(statement start, features)`:
///
/// ```ruby
/// Dir[File.join(__dir__, "buyer-risk-ml/**/*.rb")].each { |file| require_relative file }
/// ```
///
/// (buyer-risk-ml 2.51.11's entry file). Ruby requires every file the
/// pattern matches, in `Dir`'s order, as the statement runs; so the
/// statement is those requires, one per matched file of the compiled
/// tree (`listing`), each feature relative to `file`'s directory and
/// without `.rb`, as `require_relative` names it. Read: `Dir[pattern]`
/// or `Dir.glob(pattern)`, optionally `.sort`, then `.each` with a block
/// whose one statement is `require_relative param` or `require param`;
/// the pattern evaluated as the autoload loops' are (`eval`), and only
/// one read off `__FILE__`/`__dir__`: a file's own tree. A loop matching
/// no file of the tree is not one (left to run as written).
pub(crate) fn file_glob_requires(source: &str, file: &str, listing: &[String]) -> Vec<(usize, Vec<String>)> {
    let result = ruby_prism::parse(source.as_bytes());
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    // An ingest path is absolute (the tree's directory joined); a
    // listing path is as the ingest registered it, so is the match.
    let absolute = file.starts_with('/');
    let rooted = format!("{TREE_ROOT}/{}", file.trim_start_matches('/'));
    let dir = dirname(file);
    let mut locals: HashMap<String, Value> = HashMap::default();
    let mut out = Vec::new();
    for statement in program.statements().body().iter() {
        if let Some(write) = statement.as_local_variable_write_node() {
            let name = constant_id_str(&write.name()).to_string();
            match eval(&write.value(), &rooted, &locals) {
                Some(value) => {
                    locals.insert(name, value);
                }
                None => {
                    locals.remove(&name);
                }
            }
            continue;
        }
        let Some(paths) = require_loop(&statement, &rooted, absolute, &locals, listing) else { continue };
        let features: Option<Vec<String>> = paths
            .iter()
            .map(|path| relative_path_from(path, &dir).map(|rel| rel.strip_suffix(".rb").unwrap_or(&rel).to_string()))
            .collect();
        if let Some(features) = features {
            out.push((statement.location().start_offset(), features));
        }
    }
    out
}

/// The tree files one glob-require statement loads, in load order.
fn require_loop(
    statement: &Node<'_>,
    file: &str,
    absolute: bool,
    locals: &HashMap<String, Value>,
    listing: &[String],
) -> Option<Vec<String>> {
    let call = statement.as_call_node()?;
    if constant_id_str(&call.name()) != "each" || call.arguments().is_some() {
        return None;
    }
    let mut glob = call.receiver()?.as_call_node()?;
    let resorted = constant_id_str(&glob.name()) == "sort" && glob.arguments().is_none() && glob.block().is_none();
    if resorted {
        glob = glob.receiver()?.as_call_node()?;
    }
    let on_dir = glob.receiver().is_some_and(|r| r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "Dir"));
    if !on_dir || !matches!(&*constant_id_str(&glob.name()), "[]" | "glob") || glob.block().is_some() {
        return None;
    }
    let args: Vec<Node<'_>> = glob.arguments()?.arguments().iter().collect();
    let [pattern] = args.as_slice() else { return None };
    let Value::Str(pattern) = eval(pattern, file, locals)? else { return None };
    let pattern = cleanpath(&pattern);
    let pattern = pattern.strip_prefix(TREE_ROOT)?;
    let pattern = if absolute { pattern } else { pattern.strip_prefix('/')? };
    let block = call.block()?.as_block_node()?;
    let param = block_param(&block)?;
    let body: Vec<Node<'_>> = block.body().map(flatten_statements).unwrap_or_default();
    let [only] = body.as_slice() else { return None };
    let require = only.as_call_node()?;
    let args: Vec<Node<'_>> = require.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let required = matches!(args.as_slice(), [arg]
        if arg.as_local_variable_read_node().is_some_and(|read| constant_id_str(&read.name()) == param));
    if require.receiver().is_some()
        || require.block().is_some()
        || !matches!(&*constant_id_str(&require.name()), "require" | "require_relative")
        || !required
    {
        return None;
    }
    let mut matched: Vec<String> = listing.iter().filter(|f| f.ends_with(".rb") && glob_matches(pattern, f)).cloned().collect();
    if resorted {
        // `Array#sort` on the paths: by bytes.
        matched.sort();
    } else {
        dir_order(&mut matched);
    }
    (!matched.is_empty()).then_some(matched)
}

/// `Dir.glob`'s order (sorted, Ruby 3.0 on): each directory's entries
/// by bytes, a directory's matches where its name sorts. So `a/y.rb`
/// precedes `a.rb` (`a` < `a.rb`), which a byte sort of whole paths
/// reverses (`/` sorts after `.`).
fn dir_order(paths: &mut [String]) {
    paths.sort_by(|a, b| a.split('/').cmp(b.split('/')));
}

fn declarations(statements: &[Node<'_>], nesting: &[String], file: &str, listing: &[String], out: &mut Vec<String>) {
    for statement in statements {
        let (path, body) = if let Some(module) = statement.as_module_node() {
            (module_name_path(&module), module.body())
        } else if let Some(class) = statement.as_class_node() {
            (class_name_path(&class), class.body())
        } else {
            continue;
        };
        let Some(path) = path else { continue };
        let Some(body) = body else { continue };
        let nesting: Vec<String> = nesting.iter().cloned().chain(path).collect();
        let statements = flatten_statements(body);
        for found in body_loops(&statements, file, Some(listing)) {
            for (name, _) in found.autoloads {
                out.push(format!("{}::{name}", nesting.join("::")));
            }
        }
        declarations(&statements, &nesting, file, listing, out);
    }
}

fn glob_loop(
    statement: &Node<'_>,
    file: &str,
    locals: &HashMap<String, Value>,
    listing: Option<&[String]>,
) -> Option<Vec<(String, String)>> {
    let call = statement.as_call_node()?;
    if constant_id_str(&call.name()) != "each" || call.arguments().is_some() {
        return None;
    }
    let glob = call.receiver()?.as_call_node()?;
    let on_dir = glob.receiver().is_some_and(|r| r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "Dir"));
    let method = constant_id_str(&glob.name());
    if !on_dir || !matches!(&*method, "[]" | "glob") || glob.block().is_some() {
        return None;
    }
    let args: Vec<Node<'_>> = glob.arguments()?.arguments().iter().collect();
    let [pattern] = args.as_slice() else { return None };
    let Value::Str(pattern) = eval(pattern, file, locals)? else { return None };
    let block = call.block()?.as_block_node()?;
    let param = block_param(&block)?;
    let body: Vec<Node<'_>> = block.body().map(flatten_statements).unwrap_or_default();
    let matches: Vec<String> = match listing {
        Some(files) => {
            let mut matched: Vec<String> = files.iter().filter(|f| glob_matches(&pattern, f)).cloned().collect();
            matched.sort();
            matched
        }
        None => vec![sample(&pattern)?],
    };
    let mut autoloads = Vec::new();
    for path in matches {
        let mut scope = locals.clone();
        scope.insert(param.clone(), Value::Str(path));
        let mut registered = false;
        for statement in &body {
            if let Some(write) = statement.as_local_variable_write_node() {
                let value = eval(&write.value(), file, &scope)?;
                scope.insert(constant_id_str(&write.name()).to_string(), value);
                continue;
            }
            autoloads.push(autoload(statement, file, &scope)?);
            registered = true;
        }
        if !registered {
            return None;
        }
    }
    Some(autoloads)
}

fn block_param(block: &ruby_prism::BlockNode<'_>) -> Option<String> {
    let params = block.parameters()?.as_block_parameters_node()?;
    if params.locals().iter().next().is_some() {
        return None;
    }
    let params = params.parameters()?;
    let requireds: Vec<Node<'_>> = params.requireds().iter().collect();
    let [only] = requireds.as_slice() else { return None };
    let plain = params.optionals().iter().next().is_none()
        && params.rest().is_none()
        && params.posts().iter().next().is_none()
        && params.keywords().iter().next().is_none()
        && params.keyword_rest().is_none()
        && params.block().is_none();
    plain.then(|| only.as_required_parameter_node().map(|p| constant_id_str(&p.name()).to_string()))?
}

/// `autoload(name, feature)` on the body's own self.
fn autoload(statement: &Node<'_>, file: &str, scope: &HashMap<String, Value>) -> Option<(String, String)> {
    let call = statement.as_call_node()?;
    if constant_id_str(&call.name()) != "autoload" || call.receiver().is_some_and(|r| r.as_self_node().is_none()) || call.block().is_some() {
        return None;
    }
    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    let [name, feature] = args.as_slice() else { return None };
    let name = match eval(name, file, scope)? {
        Value::Str(s) | Value::Sym(s) => s,
        Value::Path(_) => return None,
    };
    // Module#autoload's own check: a constant name, one segment.
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return None;
    }
    let feature = match eval(feature, file, scope)? {
        Value::Str(s) | Value::Path(s) => s,
        Value::Sym(_) => return None,
    };
    (!feature.is_empty()).then_some((name, feature))
}

fn eval(node: &Node<'_>, file: &str, scope: &HashMap<String, Value>) -> Option<Value> {
    if node.as_source_file_node().is_some() {
        return Some(Value::Str(file.to_string()));
    }
    if let Some(s) = node.as_string_node() {
        return Some(Value::Str(String::from_utf8_lossy(s.unescaped()).into_owned()));
    }
    if let Some(s) = node.as_symbol_node() {
        return Some(Value::Sym(String::from_utf8_lossy(s.unescaped()).into_owned()));
    }
    if let Some(read) = node.as_local_variable_read_node() {
        return scope.get(constant_id_str(&read.name())).cloned();
    }
    if let Some(parens) = node.as_parentheses_node() {
        let body = flatten_statements(parens.body()?);
        let [only] = body.as_slice() else { return None };
        return eval(only, file, scope);
    }
    if let Some(interpolated) = node.as_interpolated_string_node() {
        let mut out = String::new();
        for part in interpolated.parts().iter() {
            if let Some(s) = part.as_string_node() {
                out.push_str(&String::from_utf8_lossy(s.unescaped()));
            } else if let Some(embedded) = part.as_embedded_statements_node() {
                let body = flatten_statements(embedded.statements()?.as_node());
                let [only] = body.as_slice() else { return None };
                out.push_str(eval(only, file, scope)?.text());
            } else {
                return None;
            }
        }
        return Some(Value::Str(out));
    }
    let call = node.as_call_node()?;
    if call.block().is_some() {
        return None;
    }
    let method = constant_id_str(&call.name()).to_string();
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let mut values = Vec::with_capacity(args.len());
    for arg in &args {
        values.push(eval(arg, file, scope)?);
    }
    // `__dir__`: the directory of `__FILE__` (Ruby resolves symlinks
    // first, which a tree path has none of).
    if call.receiver().is_none() && method == "__dir__" && values.is_empty() {
        return Some(Value::Str(dirname(file)));
    }
    let receiver = call.receiver()?;
    let constant = receiver.as_constant_read_node().map(|c| constant_id_str(&c.name()).to_string());
    match (constant.as_deref(), method.as_str(), values.as_slice()) {
        (Some("File"), "dirname", [Value::Str(p) | Value::Path(p)]) => Some(Value::Str(dirname(p))),
        (Some("File"), "basename", [Value::Str(p) | Value::Path(p)]) => Some(Value::Str(basename(p, None))),
        (Some("File"), "basename", [Value::Str(p) | Value::Path(p), Value::Str(ext)]) => Some(Value::Str(basename(p, Some(ext)))),
        (Some("File"), "join", parts) if !parts.is_empty() && parts.iter().all(|v| !matches!(v, Value::Sym(_))) => {
            Some(Value::Str(join(parts.iter().map(Value::text))))
        }
        // `File.expand_path(path, dir)` from an absolute `dir` (one read
        // off `__FILE__`, which is absolute where `file_glob_requires`
        // evaluates): lexical, as Ruby's is with no `~` and no symlink.
        (Some("File"), "expand_path", [Value::Str(p) | Value::Path(p), Value::Str(base) | Value::Path(base)])
            if base.starts_with('/') && !p.starts_with('~') =>
        {
            Some(Value::Str(if p.starts_with('/') { cleanpath(p) } else { cleanpath(&join([base.as_str(), p.as_str()].into_iter())) }))
        }
        (Some("Pathname"), "new", [Value::Str(p) | Value::Path(p)]) => Some(Value::Path(p.clone())),
        (Some(_), _, _) => None,
        (None, _, _) => {
            let recv = eval(&receiver, file, scope)?;
            match (recv, method.as_str(), values.as_slice()) {
                (Value::Str(a), "+", [Value::Str(b)]) => Some(Value::Str(a + b)),
                (Value::Str(s), "camelize", []) => camelize(&s).map(Value::Str),
                (Value::Path(p), "relative_path_from", [Value::Path(base) | Value::Str(base)]) => {
                    relative_path_from(p.as_str(), base).map(Value::Path)
                }
                (Value::Path(p), "sub_ext", [Value::Str(ext)]) => Some(Value::Path(sub_ext(&p, ext))),
                (Value::Path(p) | Value::Str(p), "to_s", []) => Some(Value::Str(p)),
                _ => None,
            }
        }
    }
}

/// ActiveSupport's `String#camelize` on a lower-case `a_b/c_d` name
/// (`crate::naming::camelize_path`, with the app's acronyms). Another
/// spelling is not read.
fn camelize(s: &str) -> Option<String> {
    let plain = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !s.contains("//");
    plain.then(|| crate::naming::camelize_path(s))
}

/// `File.dirname`.
fn dirname(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return if p.starts_with('/') { "/".into() } else { ".".into() };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(i) => {
            let dir = trimmed[..i].trim_end_matches('/');
            if dir.is_empty() { "/".into() } else { dir.into() }
        }
    }
}

/// `File.basename(p[, ext])`.
fn basename(p: &str, ext: Option<&str>) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return if p.is_empty() { String::new() } else { "/".into() };
    }
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    match ext {
        Some(".*") => match base.rfind('.') {
            Some(i) if i > 0 => base[..i].into(),
            _ => base.into(),
        },
        Some(ext) if !ext.is_empty() && base.len() > ext.len() && base.ends_with(ext) => base[..base.len() - ext.len()].into(),
        _ => base.into(),
    }
}

/// `File.join`.
fn join<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (i, part) in parts.enumerate() {
        if i == 0 {
            out.push_str(part);
            continue;
        }
        match (out.ends_with('/'), part.starts_with('/')) {
            (true, true) => out.push_str(&part[1..]),
            (false, false) => {
                out.push('/');
                out.push_str(part);
            }
            _ => out.push_str(part),
        }
    }
    out
}

/// `Pathname#cleanpath`: lexical, `.` and `..` folded.
fn cleanpath(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in p.split('/') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if absolute => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// `Pathname#relative_path_from`; `None` where Ruby raises
/// (`ArgumentError`: one absolute and one relative, or `base` reaching
/// above what `p` shares with it).
fn relative_path_from(p: &str, base: &str) -> Option<String> {
    let (dest, base) = (cleanpath(p), cleanpath(base));
    if dest.starts_with('/') != base.starts_with('/') {
        return None;
    }
    let split = |s: &str| -> Vec<String> {
        s.split('/').filter(|seg| !seg.is_empty() && *seg != ".").map(str::to_string).collect()
    };
    let (dest, base) = (split(&dest), split(&base));
    let common = dest.iter().zip(&base).take_while(|(a, b)| a == b).count();
    if base[common..].iter().any(|seg| seg == "..") {
        return None;
    }
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), base.len() - common).collect();
    parts.extend(dest[common..].iter().cloned());
    Some(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

/// `Pathname#sub_ext`.
fn sub_ext(p: &str, ext: &str) -> String {
    let (dir, base) = match p.rfind('/') {
        Some(i) => (&p[..=i], &p[i + 1..]),
        None => ("", p),
    };
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    format!("{dir}{stem}{ext}")
}

/// One path `pattern` matches, for checking a loop's block: `**/`
/// matches no directory, `*` and `?` one letter. `None` for a pattern
/// this does not read.
fn sample(pattern: &str) -> Option<String> {
    supported(pattern).then(|| pattern.replace("**/", "").replace(['*', '?'], "x"))
}

fn supported(pattern: &str) -> bool {
    !pattern.contains(['{', '[', '\\'])
        && pattern.split('/').all(|seg| seg != "." && seg != ".." && (seg == "**" || !seg.contains("**")))
}

/// `File.fnmatch`-style `Dir.glob` over a file list: `**/` is any number
/// of directories, `*` any run and `?` one character within a segment,
/// neither matching a leading `.`.
fn glob_matches(pattern: &str, path: &str) -> bool {
    if !supported(pattern) {
        return false;
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

fn segments(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            segments(&pattern[1..], path)
                || (path.len() > 1 && !path[0].starts_with('.') && segments(pattern, &path[1..]))
        }
        (Some(p), Some(s)) => segment(p.as_bytes(), s.as_bytes(), true) && segments(&pattern[1..], &path[1..]),
        _ => false,
    }
}

fn segment(p: &[u8], s: &[u8], start: bool) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            if start && s.first() == Some(&b'.') {
                return false;
            }
            segment(&p[1..], s, false) || (!s.is_empty() && segment(p, &s[1..], false))
        }
        (Some(b'?'), Some(c)) => !(start && *c == b'.') && segment(&p[1..], &s[1..], false),
        (Some(a), Some(b)) => a == b && segment(&p[1..], &s[1..], false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_functions_follow_ruby() {
        assert_eq!(dirname("a/b/c.rb"), "a/b");
        assert_eq!(dirname("c.rb"), ".");
        assert_eq!(dirname("/c.rb"), "/");
        assert_eq!(basename("a/b/c.rb", Some(".rb")), "c");
        assert_eq!(basename("a/b/.rb", Some(".rb")), ".rb");
        assert_eq!(cleanpath("v/lib/am/billing/gateways.rb/../../.."), "v/lib");
        assert_eq!(relative_path_from("v/lib/am/billing/gateways/x.rb", "v/lib/am/billing/gateways.rb/../../..").as_deref(), Some("am/billing/gateways/x.rb"));
        assert_eq!(relative_path_from("/a", "b"), None);
        assert_eq!(sub_ext("am/billing/gateways/x.rb", ""), "am/billing/gateways/x");
        assert!(glob_matches("d/gateways/**/*.rb", "d/gateways/x.rb"));
        assert!(glob_matches("d/gateways/**/*.rb", "d/gateways/sub/y.rb"));
        assert!(!glob_matches("d/gateways/**/*.rb", "d/gateways/.hidden.rb"));
        assert!(!glob_matches("d/gateways/*.rb", "d/gateways/sub/y.rb"));
        assert!(!glob_matches("d/gateways/**/*.rb", "d/gateways.rb"));
    }

    #[test]
    fn glob_requires_follow_ruby() {
        let listing: Vec<String> = ["g/lib/x.rb", "g/lib/x/b.rb", "g/lib/x/a.rb", "g/lib/x/a/y.rb", "g/lib/x/ab.rb", "g/lib/x/c.txt", "g/lib/other.rb"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let features = |source: &str| -> Vec<(usize, Vec<String>)> { file_glob_requires(source, "g/lib/x.rb", &listing) };
        let expected = vec!["x/a/y", "x/a", "x/ab", "x/b"];
        let found = features("require \"set\"\nDir[File.join(__dir__, \"x/**/*.rb\")].each { |file| require_relative file }\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 14);
        assert_eq!(found[0].1, expected);
        let found = features("Dir.glob(File.expand_path(\"x/**/*.rb\", File.dirname(__FILE__))).each do |f|\n  require f\nend\n");
        assert_eq!(found[0].1, expected);
        let found = features("dir = File.dirname(__FILE__)\nDir[\"#{dir}/x/*.rb\"].sort.each { |f| require f }\n");
        assert_eq!(found[0].1, vec!["x/a", "x/ab", "x/b"]);
        // `../` out of the file's own directory.
        let up = file_glob_requires("Dir[__dir__ + \"/../lib/x/*.rb\"].each { |f| require f }\n", "g/test/t.rb", &listing);
        assert_eq!(up[0].1, vec!["../lib/x/a", "../lib/x/ab", "../lib/x/b"]);
        // An absolute tree, as the ingest registers it.
        let absolute: Vec<String> = listing.iter().map(|p| format!("/t/{p}")).collect();
        let found = file_glob_requires("Dir[File.join(__dir__, \"x/**/*.rb\")].each { |file| require_relative file }\n", "/t/g/lib/x.rb", &absolute);
        assert_eq!(found[0].1, expected);
        // Not this shape: a cwd-relative literal, another block, no match.
        assert!(features("Dir[\"g/lib/x/*.rb\"].each { |f| require f }\n").is_empty());
        assert!(features("Dir[File.join(__dir__, \"x/*.rb\")].each { |f| load f }\n").is_empty());
        assert!(features("Dir[File.join(__dir__, \"x/*.rb\")].each { |f| puts f; require f }\n").is_empty());
        assert!(features("Dir[File.join(__dir__, \"y/*.rb\")].each { |f| require f }\n").is_empty());
    }
}
