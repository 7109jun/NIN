//! Backtracking dependency resolver (pure: candidates come from a provider).
//!
//! Strategy: depth-first; each package takes the best candidate satisfying every
//! requirement known so far; on failure it backtracks to the next candidate.
//! Candidates are ordered highest-first, except that an installed version is
//! tried first when the caller prefers installed versions.

use crate::error::{PkgError, Result};
use crate::registry::Source;
use semver::{Version, VersionReq};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPkg {
    pub name: String,
    pub version: Version,
    pub source: Source,
    pub dependencies: BTreeMap<String, VersionReq>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub version: Version,
    pub deps: BTreeMap<String, VersionReq>,
    pub source: Source,
}

type Chain = Vec<(String, Version)>;

#[derive(Clone)]
struct Req {
    name: String,
    req: VersionReq,
    chain: Chain,
}

#[derive(Clone)]
struct Chosen {
    version: Version,
    source: Source,
    deps: BTreeMap<String, VersionReq>,
}

const BUDGET: usize = 200_000;

fn describe_chain(chain: &Chain) -> String {
    if chain.is_empty() {
        "the request".to_string()
    } else {
        chain.iter().map(|(n, v)| format!("{n} {v}")).collect::<Vec<_>>().join(" -> ")
    }
}

fn describe_req(r: &Req) -> String {
    format!("`{} {}` required by {}", r.name, r.req, describe_chain(&r.chain))
}

struct Solver<'a> {
    provider: &'a dyn Fn(&str) -> Result<Vec<Candidate>>,
    prefer_installed: &'a dyn Fn(&str) -> bool,
    cache: RefCell<HashMap<String, Vec<Candidate>>>,
}

impl<'a> Solver<'a> {
    fn candidates(&self, name: &str) -> Result<Vec<Candidate>> {
        if let Some(c) = self.cache.borrow().get(name) {
            return Ok(c.clone());
        }
        let mut c = (self.provider)(name)?;
        c.sort_by(|a, b| b.version.cmp(&a.version));
        if (self.prefer_installed)(name) {
            let (mut inst, rest): (Vec<_>, Vec<_>) =
                c.into_iter().partition(|x| x.source == Source::Installed);
            inst.extend(rest);
            c = inst;
        }
        self.cache.borrow_mut().insert(name.to_string(), c.clone());
        Ok(c)
    }

    fn solve(
        &self,
        chosen: BTreeMap<String, Chosen>,
        mut pending: Vec<Req>,
        chosen_reqs: &mut BTreeMap<String, Vec<Req>>,
        budget: &mut usize,
    ) -> Result<BTreeMap<String, Chosen>> {
        loop {
            let r = match pending.pop() {
                Some(r) => r,
                None => return Ok(chosen),
            };
            if let Some(i) = r.chain.iter().position(|(n, _)| *n == r.name) {
                let mut cyc: Vec<String> = r.chain[i..].iter().map(|(n, _)| n.clone()).collect();
                cyc.push(r.name.clone());
                return Err(PkgError::Cycle(cyc));
            }
            if let Some(c) = chosen.get(&r.name) {
                if r.req.matches(&c.version) {
                    chosen_reqs.entry(r.name.clone()).or_default().push(r);
                    continue;
                }
                let first = chosen_reqs
                    .get(&r.name)
                    .and_then(|v| v.first())
                    .map(describe_req)
                    .unwrap_or_else(|| "an earlier requirement".into());
                return Err(PkgError::Conflict(format!(
                    "{} conflicts with {} {} selected earlier ({})",
                    describe_req(&r),
                    r.name,
                    c.version,
                    first
                )));
            }
            let cands = self.candidates(&r.name)?;
            if cands.is_empty() {
                return Err(PkgError::NotFound {
                    name: r.name.clone(),
                    required_by: describe_chain(&r.chain),
                });
            }
            let same: Vec<Req> = pending.iter().filter(|p| p.name == r.name).cloned().collect();
            let viable: Vec<&Candidate> = cands
                .iter()
                .filter(|c| r.req.matches(&c.version) && same.iter().all(|p| p.req.matches(&c.version)))
                .collect();
            if viable.is_empty() {
                let mut msg = String::new();
                msg.push_str(&format!("no version of `{}` satisfies all requirements: ", r.name));
                let mut all = vec![describe_req(&r)];
                all.extend(same.iter().map(describe_req));
                msg.push_str(&all.join("; "));
                let vs: Vec<String> = cands.iter().map(|c| c.version.to_string()).collect();
                msg.push_str(&format!(" (available: {})", vs.join(", ")));
                return Err(PkgError::Conflict(msg));
            }
            let mut first_err: Option<PkgError> = None;
            for c in viable {
                if *budget == 0 {
                    return Err(PkgError::Resolve(format!(
                        "search space too large while resolving `{}`",
                        r.name
                    )));
                }
                *budget -= 1;
                let mut ch2 = chosen.clone();
                ch2.insert(
                    r.name.clone(),
                    Chosen { version: c.version.clone(), source: c.source.clone(), deps: c.deps.clone() },
                );
                let mut p2 = pending.clone();
                let mut chain = r.chain.clone();
                chain.push((r.name.clone(), c.version.clone()));
                for (dn, dr) in c.deps.iter().rev() {
                    p2.push(Req { name: dn.clone(), req: dr.clone(), chain: chain.clone() });
                }
                let mut reqs2 = chosen_reqs.clone();
                reqs2.entry(r.name.clone()).or_default().push(r.clone());
                match self.solve(ch2, p2, &mut reqs2, budget) {
                    Ok(x) => return Ok(x),
                    Err(e @ PkgError::Resolve(_)) => return Err(e),
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
            return Err(first_err.expect("at least one viable candidate"));
        }
    }
}

/// Resolve `roots`. `provider(name)` lists every candidate version of a package.
/// The result is in dependency order (dependencies before dependents).
pub fn resolve(
    roots: &[(String, VersionReq)],
    prefer_installed: &dyn Fn(&str) -> bool,
    provider: &dyn Fn(&str) -> Result<Vec<Candidate>>,
) -> Result<Vec<ResolvedPkg>> {
    let solver = Solver { provider, prefer_installed, cache: RefCell::new(HashMap::new()) };
    let pending: Vec<Req> = roots
        .iter()
        .rev()
        .map(|(n, r)| Req { name: n.clone(), req: r.clone(), chain: Vec::new() })
        .collect();
    let mut budget = BUDGET;
    let chosen = solver.solve(BTreeMap::new(), pending, &mut BTreeMap::new(), &mut budget)?;
    // topological order (post-order DFS)
    let mut out = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut active: Vec<String> = Vec::new();
    fn visit(
        n: &str,
        chosen: &BTreeMap<String, Chosen>,
        done: &mut BTreeSet<String>,
        active: &mut Vec<String>,
        out: &mut Vec<ResolvedPkg>,
    ) -> Result<()> {
        if done.contains(n) {
            return Ok(());
        }
        if let Some(i) = active.iter().position(|a| a == n) {
            let mut c = active[i..].to_vec();
            c.push(n.to_string());
            return Err(PkgError::Cycle(c));
        }
        let c = match chosen.get(n) {
            Some(c) => c,
            None => return Ok(()),
        };
        active.push(n.to_string());
        for d in c.deps.keys() {
            visit(d, chosen, done, active, out)?;
        }
        active.pop();
        done.insert(n.to_string());
        out.push(ResolvedPkg {
            name: n.to_string(),
            version: c.version.clone(),
            source: c.source.clone(),
            dependencies: c.deps.clone(),
        });
        Ok(())
    }
    for n in chosen.keys() {
        visit(n, &chosen, &mut done, &mut active, &mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Uni = HashMap<&'static str, Vec<(&'static str, Vec<(&'static str, &'static str)>, bool)>>;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }
    fn r(s: &str) -> VersionReq {
        VersionReq::parse(s).unwrap()
    }
    fn run(u: &Uni, roots: &[(&str, &str)], prefer: bool) -> Result<Vec<ResolvedPkg>> {
        let roots: Vec<(String, VersionReq)> = roots.iter().map(|(n, q)| (n.to_string(), r(q))).collect();
        let provider = |n: &str| -> Result<Vec<Candidate>> {
            Ok(u.get(n)
                .map(|vs| {
                    vs.iter()
                        .map(|(ver, deps, inst)| Candidate {
                            version: v(ver),
                            deps: deps.iter().map(|(a, b)| (a.to_string(), r(b))).collect(),
                            source: if *inst { Source::Installed } else { Source::Archive(format!("{n}-{ver}").into()) },
                        })
                        .collect()
                })
                .unwrap_or_default())
        };
        resolve(&roots, &|_| prefer, &provider)
    }
    fn pick(res: &[ResolvedPkg], n: &str) -> String {
        res.iter().find(|p| p.name == n).map(|p| p.version.to_string()).unwrap_or_default()
    }

    #[test]
    fn picks_highest_and_orders_deps_first() {
        let mut u = Uni::new();
        u.insert("app", vec![("1.0.0", vec![("lib", "^1")], false)]);
        u.insert("lib", vec![("1.0.0", vec![], false), ("1.4.0", vec![], false), ("2.0.0", vec![], false)]);
        let res = run(&u, &[("app", "*")], false).unwrap();
        assert_eq!(pick(&res, "lib"), "1.4.0");
        let names: Vec<_> = res.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["lib", "app"]);
    }

    #[test]
    fn diamond_shares_one_version() {
        let mut u = Uni::new();
        u.insert("a", vec![("1.0.0", vec![("b", "*"), ("c", "*")], false)]);
        u.insert("b", vec![("1.0.0", vec![("d", ">=1.0.0")], false)]);
        u.insert("c", vec![("1.0.0", vec![("d", "<2.0.0")], false)]);
        u.insert("d", vec![("1.0.0", vec![], false), ("1.5.0", vec![], false), ("2.0.0", vec![], false)]);
        let res = run(&u, &[("a", "*")], false).unwrap();
        assert_eq!(pick(&res, "d"), "1.5.0");
        assert_eq!(res.iter().filter(|p| p.name == "d").count(), 1);
        assert_eq!(res.last().unwrap().name, "a");
    }

    #[test]
    fn conflict_backtracks_to_older_version() {
        // app wants x ^1 and y *; newest y 2.0 needs x ^2 (conflict) -> y 1.0 works
        let mut u = Uni::new();
        u.insert("app", vec![("1.0.0", vec![("x", "^1"), ("y", "*")], false)]);
        u.insert("x", vec![("1.0.0", vec![], false), ("2.0.0", vec![], false)]);
        u.insert("y", vec![("1.0.0", vec![("x", "^1")], false), ("2.0.0", vec![("x", "^2")], false)]);
        let res = run(&u, &[("app", "*")], false).unwrap();
        assert_eq!(pick(&res, "y"), "1.0.0");
        assert_eq!(pick(&res, "x"), "1.0.0");
    }

    #[test]
    fn backtracks_over_parent_versions() {
        // newest app 2.0 needs lib ^2 which does not exist -> falls back to app 1.0
        let mut u = Uni::new();
        u.insert("app", vec![("1.0.0", vec![("lib", "^1")], false), ("2.0.0", vec![("lib", "^2")], false)]);
        u.insert("lib", vec![("1.1.0", vec![], false)]);
        let res = run(&u, &[("app", "*")], false).unwrap();
        assert_eq!(pick(&res, "app"), "1.0.0");
    }

    #[test]
    fn unsatisfiable_conflict_message_has_chain() {
        let mut u = Uni::new();
        u.insert("app", vec![("1.0.0", vec![("a", "*"), ("b", "*")], false)]);
        u.insert("a", vec![("1.0.0", vec![("c", "^1")], false)]);
        u.insert("b", vec![("1.0.0", vec![("c", "^2")], false)]);
        u.insert("c", vec![("1.0.0", vec![], false), ("2.0.0", vec![], false)]);
        let e = run(&u, &[("app", "*")], false).unwrap_err();
        assert!(matches!(e, PkgError::Conflict(_)), "{e}");
        let m = e.to_string();
        assert!(m.contains("app 1.0.0 -> a 1.0.0") || m.contains("app 1.0.0 -> b 1.0.0"), "{m}");
        assert!(m.contains("`c"), "{m}");
    }

    #[test]
    fn cycle_detected() {
        let mut u = Uni::new();
        u.insert("a", vec![("1.0.0", vec![("b", "*")], false)]);
        u.insert("b", vec![("1.0.0", vec![("c", "*")], false)]);
        u.insert("c", vec![("1.0.0", vec![("a", "*")], false)]);
        match run(&u, &[("a", "*")], false).unwrap_err() {
            PkgError::Cycle(c) => {
                assert_eq!(c, vec!["a", "b", "c", "a"]);
            }
            e => panic!("{e}"),
        }
    }

    #[test]
    fn missing_package_reports_requirer() {
        let mut u = Uni::new();
        u.insert("app", vec![("1.0.0", vec![("ghost", "*")], false)]);
        match run(&u, &[("app", "*")], false).unwrap_err() {
            PkgError::NotFound { name, required_by } => {
                assert_eq!(name, "ghost");
                assert!(required_by.contains("app 1.0.0"));
            }
            e => panic!("{e}"),
        }
        assert!(matches!(run(&u, &[("nothing", "*")], false), Err(PkgError::NotFound { .. })));
    }

    #[test]
    fn no_version_matches_lists_available() {
        let mut u = Uni::new();
        u.insert("lib", vec![("1.0.0", vec![], false), ("1.1.0", vec![], false)]);
        let e = run(&u, &[("lib", "^3")], false).unwrap_err().to_string();
        assert!(e.contains("1.0.0") && e.contains("1.1.0") && e.contains("^3"), "{e}");
    }

    #[test]
    fn prefers_installed_when_it_satisfies() {
        let mut u = Uni::new();
        u.insert("lib", vec![("1.0.0", vec![], true), ("1.9.0", vec![], false)]);
        assert_eq!(pick(&run(&u, &[("lib", "^1")], true).unwrap(), "lib"), "1.0.0");
        assert_eq!(pick(&run(&u, &[("lib", "^1")], false).unwrap(), "lib"), "1.9.0");
        // installed version that does not satisfy is ignored even when preferred
        assert_eq!(pick(&run(&u, &[("lib", ">=1.5.0")], true).unwrap(), "lib"), "1.9.0");
        assert_eq!(run(&u, &[("lib", "^1")], true).unwrap()[0].source, Source::Installed);
    }

    #[test]
    fn same_package_requested_twice_must_agree() {
        let mut u = Uni::new();
        u.insert("lib", vec![("1.0.0", vec![], false), ("2.0.0", vec![], false)]);
        let res = run(&u, &[("lib", "*"), ("lib", "^1")], false).unwrap();
        assert_eq!(pick(&res, "lib"), "1.0.0");
        assert!(run(&u, &[("lib", "^1"), ("lib", "^2")], false).is_err());
    }

    #[test]
    fn deep_chain_resolves() {
        let names: Vec<&'static str> = (0..60).map(|i| &*Box::leak(format!("p{i}").into_boxed_str())).collect();
        let mut u = Uni::new();
        for i in 0..60 {
            let deps = if i + 1 < 60 { vec![(names[i + 1], "*")] } else { vec![] };
            u.insert(names[i], vec![("1.0.0", deps, false)]);
        }
        let res = run(&u, &[("p0", "*")], false).unwrap();
        assert_eq!(res.len(), 60);
        assert_eq!(res[0].name, "p59");
    }
}
