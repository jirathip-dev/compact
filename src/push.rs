use crate::sh::{json, ok, refused, run_in};
use crate::{map, moves, Plan, Repo};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn remote(url: &str) -> Result<BTreeMap<String, String>> {
    let out = ok("git", &["ls-remote", url])?;
    let row = |l: &str| l.split_once('\t').map(|(v, n)| (n.to_string(), v.to_string()));
    Ok(out.lines().filter_map(row).collect())
}

fn push_all(copy: &Path, url: &str, moves: &[(&str, &str, Option<&str>)]) -> Result<()> {
    let c = copy.to_string_lossy();
    let mut a: Vec<String> = ["-C", &c, "push", "--atomic", "--porcelain", url]
        .map(String::from)
        .into();
    for (name, lease, _) in moves {
        a.push(format!("--force-with-lease={name}:{lease}"));
    }
    a.extend(
        moves
            .iter()
            .map(|(name, _, to)| format!("{}:{name}", to.unwrap_or_default())),
    );
    let o = run_in("git", &a.iter().map(String::as_str).collect::<Vec<_>>(), "")?;
    if o.code != 0 {
        bail!("the atomic push was refused, nothing moved: {}", o.stderr.trim());
    }
    Ok(())
}

/// Every plan ref of one card (`main`: main alone, else all but main) on origin at its old value (`undo`: its new one), else `COMPACT_MOVED`; the refs still to move.
pub fn at(t: &Repo, p: &Plan, undo: bool, main: bool) -> Result<usize> {
    let now = remote(&t.origin()?)?;
    let (mut todo, mut moved) = (0, vec![]);
    for m in p.refs.iter().filter(|m| p.is_main(m) == main) {
        let (from, to) = if undo {
            (m.new.as_deref(), Some(m.old.as_str()))
        } else {
            (Some(m.old.as_str()), m.new.as_deref())
        };
        let is = now.get(&m.name).map(String::as_str);
        if is == from && from != to {
            todo += 1;
        } else if is != to {
            moved.push(m.name.as_str());
        }
    }
    if !moved.is_empty() {
        return refused("COMPACT_MOVED", format!("moved since the plan: {}", moved.join(", ")));
    }
    Ok(todo)
}

fn archive(t: &Repo, p: &Plan) -> Result<()> {
    let view = || json("gh", &["repo", "view", &p.archive, "--json", "visibility"]);
    let v = if let Ok(v) = view() {
        v
    } else {
        ok("gh", &["repo", "create", &p.archive, "--private"])?;
        view()?
    };
    if v["visibility"] != "PRIVATE" {
        return refused(
            "COMPACT_ARCHIVE",
            format!("{} is not private; nothing was pushed", p.archive),
        );
    }
    let (url, copy) = (t.origin()?.replace(&t.repo, &p.archive), t.dir.join("old.git"));
    let c = copy.to_string_lossy();
    let specs = [
        "+refs/heads/*:refs/heads/*",
        "+refs/tags/*:refs/tags/*",
        "+refs/notes/*:refs/notes/*",
        "+refs/pull/*/head:refs/heads/pull/*",
    ];
    ok("git", &[&["-C", &*c, "push", "-q", &url][..], &specs].concat())?;
    let (old, there) = (
        ok("git", &["-C", &c, "for-each-ref", "--format=%(refname)\t%(objectname)"])?,
        remote(&url)?,
    );
    let as_there = |n: &str| match n.strip_prefix("refs/pull/").and_then(|x| x.strip_suffix("/head")) {
        Some(pr) => Some(format!("refs/heads/pull/{pr}")),
        None => (!n.starts_with("refs/pull/")).then(|| n.to_string()),
    };
    let missing: Vec<&str> = (old.lines().filter_map(|l| l.split_once('\t')))
        .filter(|(n, v)| as_there(n).is_some_and(|t| there.get(&t) != Some(&(*v).to_string())))
        .map(|(n, _)| n)
        .collect();
    if !missing.is_empty() {
        return refused(
            "COMPACT_ARCHIVE",
            format!("the archive's read-back misses {}", missing.join(", ")),
        );
    }
    let g = |a: &[&str], input: &str| -> Result<String> {
        let o = run_in("git", &[&["-C", &*c][..], a].concat(), input)?;
        Ok(o.stdout.trim().to_string())
    };
    let blob = |file: &str| -> Result<String> {
        g(
            &["hash-object", "-w", "--stdin"],
            &std::fs::read_to_string(t.dir.join(file))?,
        )
    };
    let tree = format!(
        "100644 blob {}\tcompact-map.tsv\n100644 blob {}\tplan.json\n",
        blob("compact-map.tsv")?,
        blob("plan.json")?
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let who = format!("drover <drover@localhost> {now} +0000");
    let raw = format!(
        "tree {}\nauthor {who}\ncommitter {who}\n\ncompact {}\n",
        g(&["mktree"], &tree)?,
        p.new_tip
    );
    let id = g(&["hash-object", "-t", "commit", "-w", "--stdin"], &raw)?;
    ok(
        "git",
        &[
            "-C",
            &c,
            "push",
            "-q",
            "-f",
            &url,
            &format!("{id}:refs/heads/drover-compact"),
        ],
    )
    .map(drop)
}

fn records(t: &Repo, p: &Plan) -> Result<()> {
    let moved: Vec<serde_json::Value> = (moves(&t.saved()?, &map(&t.dir)?, &p.root).into_iter())
        .map(|(fleet, issue, moved)| serde_json::json!({"fleet": fleet, "issue": issue, "run": p.new_tip, "moved": moved}))
        .collect();
    std::fs::write(t.dir.join("moved.json"), serde_json::to_string_pretty(&moved)?)?;
    Ok(())
}

fn bodies(t: &Repo, p: &Plan) -> Result<()> {
    let m = map(&t.dir)?;
    for pr in p.refs.iter().filter_map(|x| x.pr.filter(|_| x.new.is_some())) {
        let n = pr.to_string();
        let body = json("gh", &["pr", "view", &n, "-R", &t.repo, "--json", "body"])?;
        let body = body["body"].as_str().unwrap_or_default();
        let new = |w: &str| {
            let hex = w.len() >= 7 && w.len() <= 40 && w.bytes().all(|b| b.is_ascii_hexdigit());
            let (_, (to, kind)) = m
                .range(w.to_string()..)
                .next()
                .filter(|(o, _)| hex && o.starts_with(w))?;
            (kind == "same").then(|| to.get(..w.len()).unwrap_or(to).to_string())
        };
        let mut out = String::new();
        let mut word = String::new();
        for ch in body.chars().chain(['\u{0}']) {
            if ch.is_ascii_alphanumeric() {
                word.push(ch);
                continue;
            }
            out += &new(&word).unwrap_or(std::mem::take(&mut word));
            word.clear();
            out.extend(Some(ch).filter(|c| *c != '\u{0}'));
        }
        if out != body {
            ok("gh", &["pr", "edit", &n, "-R", &t.repo, "--body", &out])?;
        }
    }
    Ok(())
}

/// A repo whose origin is at the plan's new values: its `moved.json` written; the line to print.
pub fn finished(t: &Repo) -> Result<Option<String>> {
    let Ok(p) = crate::load(&t.dir) else { return Ok(None) };
    if remote(&t.origin()?)?.get(&format!("refs/heads/{}", p.line)) != Some(&p.new_tip) {
        return Ok(None);
    }
    records(t, &p)?;
    Ok(Some(format!(
        "{} is compacted at {:.12} ({} → {} commits)",
        p.repo, p.new_tip, p.commits[0], p.commits[1]
    )))
}

/// The approved run: archive, one atomic push with a lease on every ref, the records, the open PR bodies.
pub fn push(t: &Repo, p: &Plan) -> Result<()> {
    if at(t, p, false, false)? > 0 {
        archive(t, p)?;
        let mv: Vec<(&str, &str, Option<&str>)> = (p.refs.iter().filter(|m| !p.is_main(m)))
            .map(|m| (m.name.as_str(), m.old.as_str(), m.new.as_deref()))
            .collect();
        push_all(&t.dir.join("old.git"), &t.origin()?, &mv)?;
    }
    finished(t)?;
    bodies(t, p)
}

fn copy(t: &Repo, p: &Plan) -> Result<PathBuf> {
    let copy = t.dir.join("old.git");
    if !copy.join("HEAD").exists() {
        crate::write::mirror(&t.origin()?.replace(&t.repo, &p.archive), &copy)?;
    }
    Ok(copy)
}

/// The approved main card: main alone to its new value (`undo`: its old one), a lease on the value it leaves.
pub fn main(t: &Repo, p: &Plan, undo: bool) -> Result<()> {
    let Some(m) = p.main() else {
        return refused("COMPACT_MOVED", "this plan does not move main");
    };
    let new = m.new.as_deref().unwrap_or_default();
    let (from, to) = if undo {
        (new, m.old.as_str())
    } else {
        (m.old.as_str(), new)
    };
    if at(t, p, undo, true)? > 0 {
        push_all(&copy(t, p)?, &t.origin()?, &[(m.name.as_str(), from, Some(to))])?;
    }
    Ok(())
}

/// The approved undo: every ref but main back to its old value, a lease on its new one, then the `undone` file.
pub fn undo(t: &Repo, p: &Plan) -> Result<()> {
    let copy = copy(t, p)?;
    if at(t, p, true, false)? > 0 {
        let mv: Vec<(&str, &str, Option<&str>)> = (p.refs.iter().filter(|m| !p.is_main(m)))
            .map(|m| {
                (
                    m.name.as_str(),
                    m.new.as_deref().unwrap_or_default(),
                    Some(m.old.as_str()),
                )
            })
            .collect();
        push_all(&copy, &t.origin()?, &mv)?;
    }
    std::fs::write(t.dir.join("undone"), format!("{}\n", p.new_tip))?;
    Ok(())
}
