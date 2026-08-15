//! Packages, the module tree from files, and the dependency DAG.
//!
//! A package is a directory with one `lm.package` manifest and one
//! `src/` tree. The file tree under `src/` is the module tree:
//! `src/geometry/shapes.lm` is the module `geometry.shapes` inside its
//! package, and `<package>.geometry.shapes` across packages.
//! `src/main.lm` is special in one way only: its trailing expression
//! is the program entry.
//!
//! The full module path uses the package name from the manifest, not
//! the dependency key. A rename of the key changes the local root name
//! and nothing else, so two packages may name one dependency
//! differently without changing its identity.

use crate::manifest::{parse_manifest, valid_name, Manifest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One module file of one package.
#[derive(Debug, Clone)]
pub struct ModuleFile {
    /// The full path across packages, for example `mathlib.matrix`.
    pub path: String,
    /// The path inside the package, for example `matrix`.
    pub relative: String,
    pub file: PathBuf,
    /// True for `src/main.lm`, which holds the program entry.
    pub is_main: bool,
    /// The `use` roots this module names, in source order.
    pub uses: Vec<Vec<String>>,
}

/// One loaded package.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// The module files, sorted by module path.
    pub modules: Vec<ModuleFile>,
    /// Dependency local name to the package name it provides.
    pub deps: BTreeMap<String, String>,
}

impl Package {
    /// The top-level module names of the package, for the root set.
    pub fn top_level(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .modules
            .iter()
            .map(|m| {
                m.relative
                    .split('.')
                    .next()
                    .expect("a module path has a segment")
                    .to_string()
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// True when the package builds a program.
    pub fn has_main(&self) -> bool {
        self.modules.iter().any(|m| m.is_main)
    }
}

/// One loaded workspace: the root package and its dependency closure.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: String,
    pub packages: BTreeMap<String, Package>,
    /// A topological order: a package follows every package it needs.
    pub order: Vec<String>,
}

impl Workspace {
    pub fn package(&self, name: &str) -> &Package {
        self.packages
            .get(name)
            .expect("every ordered package is loaded")
    }
}

/// Find the package directory that holds one path: the nearest
/// ancestor with an `lm.package` manifest.
pub fn find_package_dir(start: &Path) -> Result<PathBuf, String> {
    let mut at = if start.is_dir() {
        start.to_path_buf()
    } else {
        start
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    loop {
        if at.join("lm.package").is_file() {
            return Ok(at);
        }
        match at.parent() {
            Some(parent) if parent != at => at = parent.to_path_buf(),
            _ => {
                return Err(format!(
                    "error: `{}` is not inside a package; a package needs an \
                     `lm.package` manifest\n",
                    start.display()
                ))
            }
        }
    }
}

/// Load one package and its dependency closure.
pub fn load_workspace(start: &Path) -> Result<Workspace, String> {
    let root_dir = find_package_dir(start)?;
    let mut packages: BTreeMap<String, Package> = BTreeMap::new();
    let mut dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    // An explicit stack keeps the walk off the host stack.
    let mut stack: Vec<(PathBuf, bool, Vec<String>)> = vec![(root_dir.clone(), false, Vec::new())];
    let mut root_name: Option<String> = None;
    while let Some((dir, expanded, path)) = stack.pop() {
        let package = load_package(&dir)?;
        let name = package.name.clone();
        if let Some(known) = dirs.get(&name) {
            if known != &dir {
                return Err(format!(
                    "error: two packages are named `{name}`: `{}` and `{}`; \
                     rename one package in its manifest\n",
                    known.display(),
                    dir.display()
                ));
            }
        }
        if expanded {
            if !order.contains(&name) {
                order.push(name.clone());
                packages.insert(name.clone(), package);
            }
            continue;
        }
        if order.contains(&name) {
            continue;
        }
        if path.contains(&name) {
            return Err(format!(
                "error: the packages form a dependency cycle through `{name}`\n"
            ));
        }
        if root_name.is_none() {
            root_name = Some(name.clone());
        }
        dirs.insert(name.clone(), dir.clone());
        let mut child_path = path.clone();
        child_path.push(name.clone());
        stack.push((dir.clone(), true, path));
        for (_, rel) in &package.manifest.dependencies {
            let dep_dir = dir.join(rel);
            if !dep_dir.join("lm.package").is_file() {
                return Err(format!(
                    "error: the dependency path `{rel}` of `{name}` has no \
                     `lm.package` manifest\n"
                ));
            }
            stack.push((dep_dir, false, child_path.clone()));
        }
    }
    let root = root_name.ok_or_else(|| "error: no package to build\n".to_string())?;
    // Resolve every dependency key to the package name it provides.
    let names: BTreeMap<PathBuf, String> =
        dirs.iter().map(|(n, d)| (d.clone(), n.clone())).collect();
    let keys: Vec<String> = packages.keys().cloned().collect();
    for name in keys {
        let package = packages.get(&name).expect("loaded").clone();
        let mut deps = BTreeMap::new();
        for (key, rel) in &package.manifest.dependencies {
            let dep_dir = normalize(&package.dir.join(rel));
            let provided = names
                .iter()
                .find(|(d, _)| normalize(d) == dep_dir)
                .map(|(_, n)| n.clone())
                .ok_or_else(|| {
                    format!("error: the dependency `{key}` of `{name}` is not loaded\n")
                })?;
            if package.top_level().contains(key) {
                return Err(format!(
                    "error: the dependency `{key}` of `{name}` has the name of a \
                     module of the same package; rename the dependency key in \
                     lm.package\n"
                ));
            }
            deps.insert(key.clone(), provided);
        }
        packages.get_mut(&name).expect("loaded").deps = deps;
    }
    Ok(Workspace {
        root,
        packages,
        order,
    })
}

/// Remove `.` and `..` segments without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Load one package directory: the manifest and the module tree.
pub fn load_package(dir: &Path) -> Result<Package, String> {
    let manifest_path = dir.join("lm.package");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("error: cannot read `{}`: {e}\n", manifest_path.display()))?;
    let manifest = parse_manifest(&text).map_err(|e| {
        format!(
            "error: {}\n",
            e.to_string()
                .replace("lm.package", &manifest_path.display().to_string())
        )
    })?;
    let src = dir.join("src");
    if !src.is_dir() {
        return Err(format!(
            "error: the package `{}` has no `src` directory\n",
            manifest.name
        ));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_modules(&src, &mut Vec::new(), &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "error: the package `{}` has no module in `src`\n",
            manifest.name
        ));
    }
    let mut modules = Vec::new();
    for (relative, file) in files {
        let uses = scan_uses(&file)?;
        modules.push(ModuleFile {
            path: format!("{}.{}", manifest.name, relative),
            is_main: relative == "main",
            relative,
            file,
            uses,
        });
    }
    Ok(Package {
        name: manifest.name.clone(),
        dir: dir.to_path_buf(),
        manifest,
        modules,
        deps: BTreeMap::new(),
    })
}

/// Collect `src/**/*.lm` as module paths. Directory and file names
/// must be valid module names.
fn collect_modules(
    dir: &Path,
    prefix: &mut Vec<String>,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("error: cannot read `{}`: {e}\n", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("error: cannot read `{}`: {e}\n", dir.display()))?;
    entries.sort();
    for entry in entries {
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("error: `{}` has no readable name\n", entry.display()))?
            .to_string();
        if entry.is_dir() {
            if !valid_name(&name) {
                return Err(format!(
                    "error: the directory `{}` is not a module name; use a \
                     lowercase letter, then letters, digits, or underscores\n",
                    entry.display()
                ));
            }
            prefix.push(name);
            collect_modules(&entry, prefix, out)?;
            prefix.pop();
            continue;
        }
        let Some(stem) = name.strip_suffix(".lm") else {
            continue;
        };
        if !valid_name(stem) {
            return Err(format!(
                "error: the file `{}` is not a module name; use a lowercase \
                 letter, then letters, digits, or underscores\n",
                entry.display()
            ));
        }
        let mut segments = prefix.clone();
        segments.push(stem.to_string());
        out.push((segments.join("."), entry));
    }
    Ok(())
}

/// Read the `use` paths of one module. A parse failure surfaces with
/// the ordinary diagnostic text.
fn scan_uses(file: &Path) -> Result<Vec<Vec<String>>, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("error: cannot read `{}`: {e}\n", file.display()))?;
    let source = lm_source::SourceFile::new(file.display().to_string(), text);
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(&source))?;
    Ok(ast.uses.into_iter().map(|u| u.path).collect())
}

/// The build order of the modules of one package: a module follows
/// every module of the same package it names.
pub fn module_order(package: &Package) -> Result<Vec<usize>, String> {
    let index: BTreeMap<&str, usize> = package
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.relative.as_str(), i))
        .collect();
    // The needs of one module: every same-package module its `use`
    // lines name, by the longest matching prefix.
    let needs = |m: &ModuleFile| -> Vec<usize> {
        let mut out = Vec::new();
        for path in &m.uses {
            for take in (1..=path.len()).rev() {
                let candidate = path[..take].join(".");
                if let Some(idx) = index.get(candidate.as_str()) {
                    out.push(*idx);
                    break;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    };
    let mut order: Vec<usize> = Vec::new();
    let mut state = vec![0u8; package.modules.len()];
    let mut stack: Vec<(usize, bool)> = (0..package.modules.len())
        .rev()
        .map(|i| (i, false))
        .collect();
    while let Some((idx, expanded)) = stack.pop() {
        if state[idx] == 2 {
            continue;
        }
        if expanded {
            state[idx] = 2;
            order.push(idx);
            continue;
        }
        if state[idx] == 1 {
            return Err(format!(
                "error: the modules of `{}` form an import cycle through `{}`\n",
                package.name, package.modules[idx].relative
            ));
        }
        state[idx] = 1;
        stack.push((idx, true));
        for need in needs(&package.modules[idx]) {
            if state[need] == 1 {
                return Err(format!(
                    "error: the modules `{}` and `{}` of `{}` import each other\n",
                    package.modules[idx].relative, package.modules[need].relative, package.name
                ));
            }
            if state[need] == 0 {
                stack.push((need, false));
            }
        }
    }
    Ok(order)
}
