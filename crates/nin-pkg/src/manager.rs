//! [`PackageManager`]: install / remove / update / search / list on top of the
//! registries and the resolver.
//!
//! State: `<root>/installed.toml` (`[[package]]` tables) and
//! `<root>/<name>/<version>/...`. One version per name is active.
//! Installs are staged in `<root>/.staging-*` and moved into place with
//! `rename`; a failure at any point removes the staging directory and rolls
//! back already-moved directories, so nothing is left behind.

use crate::archive::{extract, parse_archive, read_archive, validate_path};
use crate::error::{PkgError, Result};
use crate::manifest::{is_valid_name, Manifest};
use crate::registry::{self, IndexEntry, Registry, Source};
use crate::resolver::{self, Candidate, ResolvedPkg};
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const STATE_FILE: &str = "installed.toml";
static STAGE_N: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPkg {
    pub name: String,
    pub version: Version,
    /// Absolute (root-joined) directory holding the package files.
    pub dir: PathBuf,
    pub entry: Option<String>,
    pub dependencies: BTreeMap<String, VersionReq>,
    /// UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
    pub installed_at: String,
    /// Requested by the user (as opposed to pulled in as a dependency).
    pub explicit: bool,
}

impl InstalledPkg {
    /// Full path of the entry file, if the package declares one.
    pub fn entry_path(&self) -> Option<PathBuf> {
        self.entry.as_ref().map(|e| {
            let mut p = self.dir.clone();
            for c in e.split('/') {
                p.push(c);
            }
            p
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub name: String,
    pub latest: Version,
    pub description: String,
    /// All published versions, highest first.
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveReport {
    pub removed: InstalledPkg,
    /// Auto-installed dependencies removed because nothing needs them any more.
    pub orphans: Vec<InstalledPkg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub name: String,
    pub from: Version,
    pub to: Version,
}

pub struct PackageManager {
    root: PathBuf,
    registries: Vec<Registry>,
}

enum Spec {
    Name(String, VersionReq),
    File(PathBuf),
}

fn parse_spec(spec: &str) -> Result<Spec> {
    let s = spec.trim();
    if s.is_empty() {
        return Err(PkgError::InvalidSpec("empty package spec".into()));
    }
    if s.to_ascii_lowercase().ends_with(".nnpkg") || s.contains('/') || s.contains('\\') {
        return Ok(Spec::File(PathBuf::from(s)));
    }
    let (name, req) = match s.split_once('@') {
        None => (s, VersionReq::STAR),
        Some((n, r)) => {
            let r = r.trim();
            if r.is_empty() {
                return Err(PkgError::InvalidSpec(format!("`{spec}`: empty version after `@`")));
            }
            let req = if let Ok(v) = Version::parse(r) {
                VersionReq::parse(&format!("={v}")).map_err(|e| PkgError::InvalidSpec(e.to_string()))?
            } else {
                VersionReq::parse(r)
                    .map_err(|e| PkgError::InvalidSpec(format!("`{spec}`: bad version requirement `{r}`: {e}")))?
            };
            (n, req)
        }
    };
    if !is_valid_name(name) {
        return Err(PkgError::InvalidSpec(format!("`{name}` is not a valid package name")));
    }
    Ok(Spec::Name(name.to_string(), req))
}

fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_utc(secs)
}

pub(crate) fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Names of auto-installed packages not reachable from any explicit package.
fn compute_orphans(state: &[InstalledPkg]) -> BTreeSet<String> {
    let by: HashMap<&str, &InstalledPkg> = state.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut reach: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = state.iter().filter(|p| p.explicit).map(|p| p.name.as_str()).collect();
    while let Some(n) = stack.pop() {
        if !reach.insert(n) {
            continue;
        }
        if let Some(p) = by.get(n) {
            for d in p.dependencies.keys() {
                if by.contains_key(d.as_str()) {
                    stack.push(d.as_str());
                }
            }
        }
    }
    state
        .iter()
        .filter(|p| !p.explicit && !reach.contains(p.name.as_str()))
        .map(|p| p.name.clone())
        .collect()
}

/// Removes its directory on drop (staging area).
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Catalog {
    by_name: HashMap<String, Vec<(IndexEntry, Source)>>,
}

impl PackageManager {
    pub fn new(root: &Path, registries: Vec<String>) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| PkgError::io(root, e))?;
        Ok(PackageManager {
            root: root.to_path_buf(),
            registries: registries.iter().map(|s| Registry::parse(s)).collect(),
        })
    }

    pub fn default_root() -> PathBuf {
        if cfg!(windows) {
            let base = std::env::var_os("APPDATA")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            base.join("nin").join("packages")
        } else {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            home.join(".local").join("share").join("nin").join("packages")
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ------------------------------------------------------------- state

    fn load_state(&self) -> Result<Vec<InstalledPkg>> {
        let path = self.root.join(STATE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(PkgError::io(&path, e)),
        };
        let serr = |m: String| PkgError::State(m);
        let doc: toml::Table = text.parse().map_err(|e: toml::de::Error| serr(e.to_string()))?;
        let arr = match doc.get("package") {
            None => return Ok(Vec::new()),
            Some(toml::Value::Array(a)) => a,
            Some(_) => return Err(serr("`package` must be an array of tables".into())),
        };
        let mut out: Vec<InstalledPkg> = Vec::new();
        for v in arr {
            let t = match v {
                toml::Value::Table(t) => t,
                _ => return Err(serr("package entry must be a table".into())),
            };
            let gs = |k: &str| -> Result<String> {
                match t.get(k) {
                    Some(toml::Value::String(s)) => Ok(s.clone()),
                    _ => Err(serr(format!("entry missing string `{k}`"))),
                }
            };
            let name = gs("name")?;
            if !is_valid_name(&name) {
                return Err(serr(format!("invalid package name `{name}`")));
            }
            if out.iter().any(|p| p.name == name) {
                return Err(serr(format!("duplicate entry for `{name}`")));
            }
            let version = Version::parse(&gs("version")?).map_err(|e| serr(format!("bad version for `{name}`: {e}")))?;
            let rel = gs("dir")?;
            if validate_path(&rel).is_err() {
                return Err(serr(format!("unsafe dir `{rel}` for `{name}`")));
            }
            let mut dir = self.root.clone();
            for c in rel.split('/') {
                dir.push(c);
            }
            let entry = match t.get("entry") {
                Some(toml::Value::String(e)) => Some(e.clone()),
                None => None,
                Some(_) => return Err(serr(format!("`entry` of `{name}` must be a string"))),
            };
            let mut dependencies = BTreeMap::new();
            if let Some(toml::Value::Table(d)) = t.get("dependencies") {
                for (k, rv) in d {
                    let rs = match rv {
                        toml::Value::String(s) => s,
                        _ => return Err(serr(format!("dependency `{k}` of `{name}` must be a string"))),
                    };
                    let req = VersionReq::parse(rs).map_err(|e| serr(format!("bad requirement `{rs}`: {e}")))?;
                    dependencies.insert(k.clone(), req);
                }
            }
            let installed_at = gs("installed_at").unwrap_or_default();
            let explicit = matches!(t.get("explicit"), Some(toml::Value::Boolean(true)));
            out.push(InstalledPkg { name, version, dir, entry, dependencies, installed_at, explicit });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn save_state(&self, state: &[InstalledPkg]) -> Result<()> {
        let mut arr = Vec::new();
        let mut sorted: Vec<&InstalledPkg> = state.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for p in sorted {
            let mut t = toml::Table::new();
            t.insert("name".into(), p.name.clone().into());
            t.insert("version".into(), p.version.to_string().into());
            let rel = format!("{}/{}", p.name, p.version);
            t.insert("dir".into(), rel.into());
            if let Some(e) = &p.entry {
                t.insert("entry".into(), e.clone().into());
            }
            t.insert("installed_at".into(), p.installed_at.clone().into());
            t.insert("explicit".into(), toml::Value::Boolean(p.explicit));
            let mut deps = toml::Table::new();
            for (k, v) in &p.dependencies {
                deps.insert(k.clone(), v.to_string().into());
            }
            t.insert("dependencies".into(), toml::Value::Table(deps));
            arr.push(toml::Value::Table(t));
        }
        let mut doc = toml::Table::new();
        doc.insert("package".into(), toml::Value::Array(arr));
        let text = toml::to_string(&doc).map_err(|e| PkgError::State(e.to_string()))?;
        registry::write_file_atomic(&self.root.join(STATE_FILE), text.as_bytes())
    }

    // ---------------------------------------------------------- registries

    fn catalog(&self) -> Result<Catalog> {
        let mut by_name: HashMap<String, Vec<(IndexEntry, Source)>> = HashMap::new();
        for r in &self.registries {
            for (e, s) in r.entries()? {
                let list = by_name.entry(e.name.clone()).or_default();
                // first registry wins for a given version
                if !list.iter().any(|(x, _)| x.version == e.version) {
                    list.push((e, s));
                }
            }
        }
        Ok(Catalog { by_name })
    }

    /// Copy a `.nnpkg` into a registry directory and refresh its `index.toml`.
    pub fn publish(&self, nnpkg: &Path, registry_dir: &Path) -> Result<()> {
        registry::publish(nnpkg, registry_dir)
    }

    pub fn search(&self, q: &str) -> Result<Vec<SearchHit>> {
        let cat = self.catalog()?;
        let ql = q.trim().to_lowercase();
        let mut hits: Vec<(u8, SearchHit)> = Vec::new();
        for (name, list) in &cat.by_name {
            let mut sorted: Vec<&(IndexEntry, Source)> = list.iter().collect();
            sorted.sort_by(|a, b| b.0.version.cmp(&a.0.version));
            let latest = match sorted.first() {
                Some(l) => &l.0,
                None => continue,
            };
            let nl = name.to_lowercase();
            let score = if nl == ql {
                0
            } else if nl.starts_with(&ql) {
                1
            } else if nl.contains(&ql) {
                2
            } else if latest.description.to_lowercase().contains(&ql)
                || sorted.iter().any(|e| e.0.description.to_lowercase().contains(&ql))
            {
                3
            } else {
                continue;
            };
            hits.push((
                score,
                SearchHit {
                    name: name.clone(),
                    latest: latest.version.clone(),
                    description: latest.description.clone(),
                    versions: sorted.iter().map(|e| e.0.version.clone()).collect(),
                },
            ));
        }
        hits.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
        Ok(hits.into_iter().map(|h| h.1).collect())
    }

    // ----------------------------------------------------------- resolving

    fn run_resolver(
        &self,
        roots: &[(String, VersionReq)],
        installed: &[InstalledPkg],
        cat: &Catalog,
        archive: Option<&(Manifest, PathBuf)>,
        prefer_installed_roots: bool,
    ) -> Result<Vec<ResolvedPkg>> {
        let root_names: BTreeSet<String> = roots.iter().map(|r| r.0.clone()).collect();
        let provider = |name: &str| -> Result<Vec<Candidate>> {
            if let Some((m, p)) = archive {
                if m.name == name {
                    return Ok(vec![Candidate {
                        version: m.version.clone(),
                        deps: m.dependencies.clone(),
                        source: Source::Archive(p.clone()),
                    }]);
                }
            }
            let mut out: Vec<Candidate> = Vec::new();
            if let Some(list) = cat.by_name.get(name) {
                for (e, s) in list {
                    out.push(Candidate {
                        version: e.version.clone(),
                        deps: e.dependencies.clone(),
                        source: s.clone(),
                    });
                }
            }
            if let Some(inst) = installed.iter().find(|p| p.name == name) {
                let c = Candidate {
                    version: inst.version.clone(),
                    deps: inst.dependencies.clone(),
                    source: Source::Installed,
                };
                match out.iter_mut().find(|x| x.version == inst.version) {
                    Some(x) => *x = c,
                    None => out.push(c),
                }
            }
            // Installed packages (not being replaced) constrain what we may pick.
            let constraints: Vec<(&str, &VersionReq)> = installed
                .iter()
                .filter(|p| p.name != name && !root_names.contains(&p.name))
                .filter_map(|p| p.dependencies.get(name).map(|r| (p.name.as_str(), r)))
                .collect();
            if !constraints.is_empty() && !out.is_empty() {
                let before = out.len();
                out.retain(|c| constraints.iter().all(|(_, r)| r.matches(&c.version)));
                if out.is_empty() && before > 0 {
                    let who: Vec<String> = constraints.iter().map(|(p, r)| format!("{p} requires `{name} {r}`")).collect();
                    return Err(PkgError::Conflict(format!(
                        "no available version of `{name}` is compatible with installed packages ({})",
                        who.join("; ")
                    )));
                }
            }
            Ok(out)
        };
        let prefer = |n: &str| prefer_installed_roots || !root_names.contains(n);
        resolver::resolve(roots, &prefer, &provider)
    }

    /// Resolve `roots` against the registries and installed packages
    /// (installed versions that satisfy a requirement are preferred).
    pub fn resolve(&self, roots: &[(String, VersionReq)]) -> Result<Vec<ResolvedPkg>> {
        let installed = self.load_state()?;
        let cat = self.catalog()?;
        self.run_resolver(roots, &installed, &cat, None, true)
    }

    // ------------------------------------------------------------- install

    pub fn install(&self, spec: &str) -> Result<Vec<InstalledPkg>> {
        let installed = self.load_state()?;
        let (roots, archive, cat) = match parse_spec(spec)? {
            Spec::Name(n, r) => (vec![(n, r)], None, self.catalog()?),
            Spec::File(p) => {
                let a = read_archive(&p)?;
                let m = a.manifest;
                let exact = VersionReq::parse(&format!("={}", m.version)).map_err(|e| PkgError::InvalidSpec(e.to_string()))?;
                let cat = if m.dependencies.is_empty() { Catalog { by_name: HashMap::new() } } else { self.catalog()? };
                (vec![(m.name.clone(), exact)], Some((m, p)), cat)
            }
        };
        let plan = self.run_resolver(&roots, &installed, &cat, archive.as_ref(), false)?;
        let root_names: BTreeSet<String> = roots.iter().map(|r| r.0.clone()).collect();
        let done = self.apply(plan, installed, &root_names)?;
        if done.is_empty() {
            let now = self.load_state()?;
            return Ok(now.into_iter().filter(|p| root_names.contains(&p.name)).collect());
        }
        Ok(done)
    }

    /// Fetch, stage, and atomically commit `plan`; returns the new records.
    fn apply(
        &self,
        plan: Vec<ResolvedPkg>,
        installed: Vec<InstalledPkg>,
        roots: &BTreeSet<String>,
    ) -> Result<Vec<InstalledPkg>> {
        let todo: Vec<&ResolvedPkg> = plan.iter().filter(|p| p.source != Source::Installed).collect();
        let mut state = installed.clone();
        // explicit flag for already-installed roots
        let mut dirty = false;
        for p in state.iter_mut() {
            if roots.contains(&p.name) && !p.explicit && !todo.iter().any(|t| t.name == p.name) {
                p.explicit = true;
                dirty = true;
            }
        }
        if todo.is_empty() {
            if dirty {
                self.save_state(&state)?;
            }
            return Ok(Vec::new());
        }

        let stage_dir = self.root.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            STAGE_N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&stage_dir);
        fs::create_dir_all(&stage_dir).map_err(|e| PkgError::io(&stage_dir, e))?;
        let staging = Staging(stage_dir.clone());

        // Phase 1: download, verify, extract. Nothing outside staging is touched.
        let mut staged: Vec<(usize, Manifest, PathBuf)> = Vec::new();
        for (i, r) in todo.iter().enumerate() {
            let bytes = r.source.fetch()?;
            let a = parse_archive(&bytes)?;
            let m = &a.manifest;
            if m.name != r.name || m.version != r.version {
                return Err(PkgError::Registry(format!(
                    "package file for `{} {}` actually contains `{} {}`",
                    r.name, r.version, m.name, m.version
                )));
            }
            if m.dependencies != r.dependencies {
                return Err(PkgError::Registry(format!(
                    "dependencies of `{} {}` differ from the registry index (stale index.toml?)",
                    r.name, r.version
                )));
            }
            if let Some(e) = &m.entry {
                if !a.files.iter().any(|(p, _)| p == e) {
                    return Err(PkgError::Archive(format!("entry file `{e}` of `{}` is not in the archive", r.name)));
                }
            }
            let dest = stage_dir.join(format!("n{i}"));
            extract(&a, &dest)?;
            staged.push((i, a.manifest, dest));
        }

        // Phase 2: move into place (with rollback).
        let trash = stage_dir.join("trash");
        let mut committed: Vec<(PathBuf, Option<PathBuf>, bool)> = Vec::new(); // target, trashed, created name dir
        let rollback = |committed: &Vec<(PathBuf, Option<PathBuf>, bool)>| {
            for (target, trashed, created_parent) in committed.iter().rev() {
                let _ = fs::remove_dir_all(target);
                if let Some(t) = trashed {
                    let _ = fs::rename(t, target);
                }
                if *created_parent {
                    if let Some(p) = target.parent() {
                        let _ = fs::remove_dir(p);
                    }
                }
            }
        };
        for (i, m, src) in &staged {
            let name_dir = self.root.join(&m.name);
            let target = name_dir.join(m.version.to_string());
            let step = (|| -> Result<(Option<PathBuf>, bool)> {
                let created = !name_dir.exists();
                fs::create_dir_all(&name_dir).map_err(|e| PkgError::io(&name_dir, e))?;
                let mut trashed = None;
                if target.exists() {
                    fs::create_dir_all(&trash).map_err(|e| PkgError::io(&trash, e))?;
                    let t = trash.join(format!("t{i}"));
                    fs::rename(&target, &t).map_err(|e| PkgError::io(&target, e))?;
                    trashed = Some(t);
                }
                if let Err(e) = fs::rename(src, &target) {
                    if let Some(t) = &trashed {
                        let _ = fs::rename(t, &target);
                    }
                    if created {
                        let _ = fs::remove_dir(&name_dir);
                    }
                    return Err(PkgError::io(&target, e));
                }
                Ok((trashed, created))
            })();
            match step {
                Ok((t, c)) => committed.push((target, t, c)),
                Err(e) => {
                    rollback(&committed);
                    return Err(e);
                }
            }
        }

        // Phase 3: record state.
        let now = now_utc();
        let mut records = Vec::new();
        let mut replaced: Vec<InstalledPkg> = Vec::new();
        for (_, m, _) in &staged {
            let old = state.iter().position(|p| p.name == m.name).map(|i| state.remove(i));
            let explicit = roots.contains(&m.name) || old.as_ref().map(|o| o.explicit).unwrap_or(false);
            if let Some(o) = old {
                replaced.push(o);
            }
            let rec = InstalledPkg {
                name: m.name.clone(),
                version: m.version.clone(),
                dir: self.root.join(&m.name).join(m.version.to_string()),
                entry: m.entry.clone(),
                dependencies: m.dependencies.clone(),
                installed_at: now.clone(),
                explicit,
            };
            state.push(rec.clone());
            records.push(rec);
        }
        // drop auto-installed packages that nothing needs any more
        let orphans = compute_orphans(&state);
        let orphan_recs: Vec<InstalledPkg> = state.iter().filter(|p| orphans.contains(&p.name)).cloned().collect();
        state.retain(|p| !orphans.contains(&p.name));
        if let Err(e) = self.save_state(&state) {
            rollback(&committed);
            return Err(e);
        }

        // Cleanup of superseded versions (best effort; state is already committed).
        for o in replaced {
            if !records.iter().any(|r| r.dir == o.dir) {
                let _ = fs::remove_dir_all(&o.dir);
            }
        }
        for o in orphan_recs {
            let _ = fs::remove_dir_all(self.root.join(&o.name));
        }
        drop(staging);
        Ok(records)
    }

    // -------------------------------------------------------------- remove

    pub fn remove(&self, name: &str, force: bool) -> Result<RemoveReport> {
        let mut state = self.load_state()?;
        let idx = state
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| PkgError::NotInstalled(name.to_string()))?;
        let dependents: Vec<String> = state
            .iter()
            .filter(|p| p.name != name && p.dependencies.contains_key(name))
            .map(|p| p.name.clone())
            .collect();
        if !dependents.is_empty() && !force {
            return Err(PkgError::Dependents { name: name.to_string(), dependents });
        }
        let removed = state.remove(idx);
        let orphan_names = compute_orphans(&state);
        let orphans: Vec<InstalledPkg> = state.iter().filter(|p| orphan_names.contains(&p.name)).cloned().collect();
        state.retain(|p| !orphan_names.contains(&p.name));
        self.save_state(&state)?;
        let mut first_err = None;
        for p in std::iter::once(&removed).chain(orphans.iter()) {
            let d = self.root.join(&p.name);
            if let Err(e) = fs::remove_dir_all(&d) {
                if e.kind() != std::io::ErrorKind::NotFound && first_err.is_none() {
                    first_err = Some(PkgError::io(&d, e));
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(RemoveReport { removed, orphans }),
        }
    }

    // -------------------------------------------------------------- update

    pub fn update(&self, name: Option<&str>) -> Result<Vec<UpdateReport>> {
        let installed = self.load_state()?;
        let targets: Vec<&InstalledPkg> = match name {
            Some(n) => vec![installed
                .iter()
                .find(|p| p.name == n)
                .ok_or_else(|| PkgError::NotInstalled(n.to_string()))?],
            None => installed.iter().collect(),
        };
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let mut roots = Vec::new();
        for t in &targets {
            let req = if t.version.pre.is_empty() {
                VersionReq::STAR
            } else {
                VersionReq::parse(&format!(">={}", t.version)).map_err(|e| PkgError::InvalidSpec(e.to_string()))?
            };
            roots.push((t.name.clone(), req));
        }
        let cat = self.catalog()?;
        let plan = self.run_resolver(&roots, &installed, &cat, None, false)?;
        let old: HashMap<String, Version> = installed.iter().map(|p| (p.name.clone(), p.version.clone())).collect();
        // never downgrade
        let plan: Vec<ResolvedPkg> = plan
            .into_iter()
            .filter(|p| old.get(&p.name).map(|v| &p.version >= v).unwrap_or(true))
            .collect();
        let root_names: BTreeSet<String> = roots.iter().map(|r| r.0.clone()).collect();
        let done = self.apply(plan, installed.clone(), &root_names)?;
        let mut reports = Vec::new();
        for r in done {
            if let Some(from) = old.get(&r.name) {
                if *from != r.version {
                    reports.push(UpdateReport { name: r.name, from: from.clone(), to: r.version });
                }
            }
        }
        Ok(reports)
    }

    // ---------------------------------------------------------------- query

    pub fn list(&self) -> Result<Vec<InstalledPkg>> {
        self.load_state()
    }

    pub fn info(&self, name: &str) -> Result<Option<InstalledPkg>> {
        self.find_installed(name)
    }

    pub fn find_installed(&self, name: &str) -> Result<Option<InstalledPkg>> {
        Ok(self.load_state()?.into_iter().find(|p| p.name == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parsing() {
        assert!(matches!(parse_spec("foo").unwrap(), Spec::Name(n, r) if n == "foo" && r == VersionReq::STAR));
        match parse_spec("foo@1.2.3").unwrap() {
            Spec::Name(_, r) => {
                assert!(r.matches(&Version::new(1, 2, 3)));
                assert!(!r.matches(&Version::new(1, 2, 4)));
            }
            _ => panic!(),
        }
        match parse_spec("foo@^1.2").unwrap() {
            Spec::Name(_, r) => assert!(r.matches(&Version::new(1, 9, 0)) && !r.matches(&Version::new(2, 0, 0))),
            _ => panic!(),
        }
        assert!(matches!(parse_spec("x.nnpkg").unwrap(), Spec::File(_)));
        assert!(matches!(parse_spec("./dir/x").unwrap(), Spec::File(_)));
        for bad in ["", "Foo", "foo@", "foo@nope", "@1", "a b"] {
            assert!(matches!(parse_spec(bad), Err(PkgError::InvalidSpec(_))), "{bad}");
        }
    }

    #[test]
    fn utc_format() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn default_root_shape() {
        let r = PackageManager::default_root();
        assert!(r.ends_with(Path::new("nin").join("packages")));
    }
}
