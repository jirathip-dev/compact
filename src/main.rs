mod plan;
mod push;
mod sh;
mod write;

use anyhow::{Context, Result};
use clap::Parser;
use plan::{Class, Kind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sh::{json, ok, refused, On, Refused};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A repo's fresh start: one root before a cut, one commit per landed PR after it; archive, undo.
#[derive(Parser, Debug)]
#[command(name = "compact", version)]
struct Opts {
    #[arg(long, help = "The GitHub repo: owner/name")]
    repo: String,
    #[arg(long, help = "The line to compact: the repo's integration branch")]
    line: String,
    #[arg(long, help = "The fleet's clone: its origin is the repo; it is reset after a push")]
    clone: PathBuf,
    #[arg(
        long = "lane",
        help = "A lane folder, `[host:]path` (repeatable): checked clean, reset after a push"
    )]
    lanes: Vec<String>,
    #[arg(
        long = "fleet",
        help = "A drover fleet on the repo (repeatable): no check lane may be working"
    )]
    fleets: Vec<String>,
    #[arg(
        long,
        help = "Drover's saved commit ids, `fleet<TAB>issue<TAB>sha` per line: the records to remap"
    )]
    saved: Option<PathBuf>,
    #[arg(
        long,
        help = "The scratch folder: the bare copy, the plan and the map (default ~/.local/state/compact/<repo>)"
    )]
    dir: Option<PathBuf>,
    #[arg(long, help = "The cut: a date (the last line commit before that midnight) or a sha")]
    cut: Option<String>,
    #[arg(long, help = "Print the plan; push nothing")]
    dry_run: bool,
    #[arg(long, help = "Post the owner's run card in this drover thread")]
    thread: Option<String>,
    #[arg(long, help = "Put the old history back (with --dry-run, --thread or --push)")]
    undo: bool,
    #[arg(
        long,
        help = "The approved card's run: the plan's new tip (with --undo, its old tip)"
    )]
    push: Option<String>,
    #[arg(long, help = "With --push: the approved main card's run, main alone")]
    main: bool,
    #[arg(long, help = "Reset the fleet's clone and lane folders onto origin's new branches")]
    reset: bool,
}

enum Act {
    Cut(String),
    Undo,
    Push { tip: String, undo: bool, main: bool },
    Reset,
}

/// The repo and the folders compact works on, as the arguments name them.
pub struct Repo {
    pub repo: String,
    pub line: String,
    pub clone: PathBuf,
    pub lanes: Vec<(Option<String>, PathBuf)>,
    pub fleets: Vec<String>,
    pub saved: Option<PathBuf>,
    pub dir: PathBuf,
    pub thread: Option<String>,
    base: Vec<String>,
}

impl Repo {
    /// The clone's origin url.
    pub fn origin(&self) -> Result<String> {
        let c = self.clone.to_string_lossy();
        Ok(ok("git", &["-C", &c, "remote", "get-url", "origin"])?
            .trim()
            .to_string())
    }
    fn name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }
    /// The saved commit ids: (fleet, issue, sha) in the file's order.
    pub fn saved(&self) -> Result<Vec<(String, u64, String)>> {
        let Some(f) = &self.saved else { return Ok(vec![]) };
        let text = std::fs::read_to_string(f).with_context(|| format!("--saved {}", f.display()))?;
        let row = |l: &str| {
            let mut w = l.split('\t');
            Some((
                w.next()?.to_string(),
                w.next()?.parse().ok()?,
                w.next()?.trim().to_string(),
            ))
        };
        Ok(text.lines().filter_map(row).collect())
    }
    fn cmd(&self, words: &[&str]) -> String {
        let mut c = self.base.clone();
        c.extend(words.iter().map(ToString::to_string));
        sh::line(&c)
    }
}

/// A ref's move as the plan file keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefMove {
    pub name: String,
    pub old: String,
    pub new: Option<String>,
    pub class: String,
    pub pr: Option<u64>,
}

/// One commit of the new line after the root: old and new id, subject and how many old commits it holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub old: String,
    pub new: String,
    pub subject: String,
    pub held: usize,
}

/// The planned compact of a repo: what the dry run prints, the card pins and the push sends.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub repo: String,
    pub line: String,
    pub cut: String,
    pub root: String,
    pub old_tip: String,
    pub new_tip: String,
    pub archive: String,
    pub commits: [usize; 2],
    pub refs: Vec<RefMove>,
    pub chain: Vec<Step>,
    pub forks: Vec<u64>,
}

const MAIN: &str = "refs/heads/main";

impl Plan {
    /// `m` is `main`, pushed only by its own card (never when main is the line itself).
    pub fn is_main(&self, m: &RefMove) -> bool {
        m.name == MAIN && self.line != "main"
    }
    /// `main`'s move, if the plan moves it.
    pub fn main(&self) -> Option<&RefMove> {
        (self.refs.iter()).find(|m| self.is_main(m) && m.new.as_deref() != Some(m.old.as_str()))
    }
}

fn is_sha(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The map file as rows: old id → (new id, kind).
pub fn map(d: &Path) -> Result<BTreeMap<String, (String, String)>> {
    let text = std::fs::read_to_string(d.join("compact-map.tsv")).context("no compact-map.tsv: run --cut first")?;
    let row = |l: &str| {
        let mut w = l.split('\t').map(String::from);
        Some((w.next()?, (w.next()?, w.next()?)))
    };
    Ok(text.lines().filter_map(row).collect())
}

fn load(d: &Path) -> Result<Plan> {
    let text = std::fs::read_to_string(d.join("plan.json")).context("no plan.json: run --cut first")?;
    Ok(serde_json::from_str(&text)?)
}

fn build(t: &Repo, cut: &str) -> Result<Plan> {
    let d = &t.dir;
    std::fs::create_dir_all(d)?;
    let copy = d.join("old.git");
    write::mirror(&t.origin()?, &copy)?;
    let words = "pr list --state open --limit 500 --json number,headRefName,isCrossRepository -R";
    let prs = json(
        "gh",
        &[&words.split(' ').collect::<Vec<_>>()[..], &[t.repo.as_str()]].concat(),
    )?;
    let prs = prs.as_array().cloned().unwrap_or_default();
    let open = (prs.iter())
        .filter_map(|p| Some((p["number"].as_u64()?, p["headRefName"].as_str()?.to_string())))
        .collect();
    let old = write::read(&copy, open)?;
    let g = |a: &[&str]| ok("git", &[&["-C", &copy.to_string_lossy()][..], a].concat());
    let line = format!("refs/heads/{}", t.line);
    let (sha, label) = if is_sha(cut) {
        let s = g(&["rev-parse", &format!("{cut}^{{commit}}")])?;
        let day = g(&["log", "-1", "--format=%cs", s.trim()])?;
        (s, day)
    } else {
        let before = format!("--before={cut} 00:00");
        (
            g(&["rev-list", "-1", "--first-parent", &before, &line])?,
            cut.to_string(),
        )
    };
    let (sha, label) = (sha.trim().to_string(), label.trim().to_string());
    if sha.is_empty() {
        return refused("BAD_VALUE", format!("no commit on {} before {cut}", t.line));
    }
    let archive = format!("{}-history", t.repo);
    let msg = format!("{}: initial (history before {label} in {archive})\n", t.name());
    let shape = plan::plan(&old, &t.line, &sha, &msg).or_else(|e| refused("BAD_VALUE", e))?;
    let (ids, notes) = write::write(&copy, &shape)?;
    let id = |n: usize| ids.get(n).cloned().unwrap_or_default();
    let pr_of = |name: &str| {
        let b = name.strip_prefix("refs/heads/")?;
        old.open.iter().find(|o| o.1 == b).map(|o| o.0)
    };
    let refs: Vec<RefMove> = (shape.refs.iter())
        .map(|m| RefMove {
            name: m.name.clone(),
            old: m.old.clone(),
            new: match m.class {
                Class::Notes => notes.iter().find(|n| n.0 == m.name).map(|n| n.1.clone()),
                _ => m.to.map(id),
            },
            class: format!("{:?}", m.class),
            pr: pr_of(&m.name),
        })
        .collect();
    if let Some(m) = refs
        .iter()
        .find(|m| m.name == MAIN && m.new.is_none() && t.line != "main")
    {
        return refused(
            "COMPACT_TREE",
            format!("main ({:.12}) would be deleted; nothing was sent", m.old),
        );
    }
    let checked: Vec<&RefMove> = refs.iter().filter(|m| m.class != "Notes" && m.new.is_some()).collect();
    let ends: Vec<&str> = (checked.iter())
        .flat_map(|m| [m.old.as_str(), m.new.as_deref().unwrap_or_default()])
        .collect();
    let trees = write::trees(&copy, &ends)?;
    let bad: Vec<&str> = (checked.iter().zip(trees.chunks(2)))
        .filter(|(_, t)| t.first() != t.get(1))
        .map(|(m, _)| m.name.as_str())
        .collect();
    if !bad.is_empty() {
        return refused(
            "COMPACT_TREE",
            format!("a new tip's tree differs: {}; nothing was sent", bad.join(", ")),
        );
    }
    let mut held = vec![0usize; shape.nodes.len()];
    for (n, _) in shape.place.values().filter(|p| p.1 == Kind::Held) {
        if let Some(h) = held.get_mut(*n) {
            *h += 1;
        }
    }
    let chain = (shape.line.iter().enumerate())
        .map(|(i, o)| Step {
            old: o.clone(),
            new: id(i + 1),
            subject: old
                .commits
                .get(o)
                .map_or("", |c| c.message.lines().next().unwrap_or_default())
                .to_string(),
            held: held.get(i + 1).copied().unwrap_or_default(),
        })
        .collect();
    let old_tip = (old.refs.iter().find(|x| x.0 == line)).map_or(String::new(), |x| x.1.clone());
    let count = g(&["rev-list", "--count", &old_tip])?
        .trim()
        .parse()
        .unwrap_or_default();
    let p = Plan {
        repo: t.repo.clone(),
        line: t.line.clone(),
        cut: sha,
        root: id(0),
        new_tip: id(shape.line.len()),
        old_tip,
        archive,
        commits: [count, shape.line.len() + 1],
        refs,
        chain,
        forks: (prs.iter())
            .filter(|p| p["isCrossRepository"] == true)
            .filter_map(|p| p["number"].as_u64())
            .collect(),
    };
    let mut rows: Vec<String> = (shape.place.iter())
        .map(|(o, (n, k))| format!("{o}\t{}\t{}\n", id(*n), k.word()))
        .collect();
    rows.sort();
    std::fs::write(d.join("compact-map.tsv"), rows.concat())?;
    std::fs::write(d.join("plan.json"), serde_json::to_string_pretty(&p)?)?;
    Ok(p)
}

fn dirty(on: On, wt: &str) -> Option<String> {
    let st = match on.ok("git", &["-C", wt, "status", "--porcelain", "--untracked-files=no"]) {
        Ok(s) => s,
        Err(e) => return Some(format!("unreadable git status ({e:#})")),
    };
    let changed = st.lines().count();
    if changed > 0 {
        return Some(format!("{changed} uncommitted change(s)"));
    }
    match on.ok("git", &["-C", wt, "rev-list", "--count", "HEAD", "--not", "--remotes"]) {
        Ok(n) if n.trim() == "0" => None,
        Ok(n) => Some(format!("{} unpushed commit(s)", n.trim())),
        Err(e) => Some(format!("unreadable unpushed count ({e:#})")),
    }
}

fn lanes(t: &Repo) -> impl Iterator<Item = (On<'_>, &Path)> {
    (t.lanes.iter())
        .map(|(h, p)| (On(h.as_deref()), p.as_path()))
        .filter(|(on, p)| {
            let git = p.join(".git");
            match on.0 {
                None => git.exists(),
                Some(_) => on.ok("test", &["-e", &git.to_string_lossy()]).is_ok(),
            }
        })
}

fn ready(t: &Repo, p: &Plan) -> Vec<String> {
    let mut why = vec![];
    for fleet in &t.fleets {
        match json("drover", &["status", fleet, "--json"]) {
            Ok(v) => {
                for s in v.as_array().into_iter().flatten() {
                    let state = s["state"].as_str().unwrap_or_default();
                    if state.starts_with("review") || state.starts_with("verify") {
                        why.push(format!("#{} has a check lane working ({state})", s["issue"]));
                    }
                }
            }
            Err(e) => why.push(format!("fleet {fleet}: drover status failed ({e:#})")),
        }
    }
    for (on, wt) in lanes(t) {
        if let Some(d) = dirty(on, &wt.to_string_lossy()) {
            let host = on.0.map(|h| format!("{h}:")).unwrap_or_default();
            why.push(format!("{host}{}: {d}", wt.display()));
        }
    }
    for n in &p.forks {
        why.push(format!("PR #{n} comes from a fork"));
    }
    for m in p.refs.iter().filter(|m| m.class == "Detached" && m.pr.is_some()) {
        why.push(format!(
            "PR #{} would be detached: update or close it",
            m.pr.unwrap_or_default()
        ));
    }
    for b in [p.line.as_str(), "main"] {
        let api = format!("repos/{}/branches/{b}/protection", p.repo);
        let v = json("gh", &["api", &api]).unwrap_or(Value::Null);
        if v["allow_force_pushes"]["enabled"] == false {
            why.push(format!("force pushes are not allowed on {b}"));
        }
    }
    why
}

fn unready(t: &Repo, p: &Plan) -> Result<()> {
    let why = ready(t, p);
    if why.is_empty() {
        return Ok(());
    }
    refused("COMPACT_NOT_READY", why.join("; "))
}

/// Each issue's saved ids that now live in the new history: (fleet, issue, its `moved` list).
pub fn moves(
    saved: &[(String, u64, String)],
    m: &BTreeMap<String, (String, String)>,
    root: &str,
) -> Vec<(String, u64, Value)> {
    let mut by: BTreeMap<(String, u64), Vec<String>> = BTreeMap::new();
    for (fleet, i, sha) in saved {
        let ids = by.entry((fleet.clone(), *i)).or_default();
        if !ids.contains(sha) {
            ids.push(sha.clone());
        }
    }
    let mut out = vec![];
    for ((fleet, i), ids) in by {
        let mut moved: Vec<(String, String, Vec<String>)> = vec![];
        let kind = |s: &String| m.get(s).filter(|(to, _)| to != root).map(|(to, k)| (to, k.as_str()));
        let mut ids: Vec<&String> = ids.iter().filter(|s| kind(s).is_some()).collect();
        ids.sort_by_key(|s| kind(s).map(|k| k.1 != "same"));
        for s in ids {
            match kind(s) {
                Some((to, "same")) => moved.push((s.clone(), to.clone(), vec![])),
                Some((to, "held")) => match moved.iter_mut().find(|x| &x.1 == to) {
                    Some(x) => x.2.push(s.clone()),
                    None => moved.push((String::new(), to.clone(), vec![s.clone()])),
                },
                _ => {}
            }
        }
        for x in moved.iter_mut().filter(|x| x.0.is_empty()) {
            x.0 = m
                .iter()
                .find(|(_, v)| v.0 == x.1 && v.1 == "same")
                .map(|(o, _)| o.clone())
                .unwrap_or_default();
        }
        let v: Vec<Value> = moved
            .into_iter()
            .map(|(f, t, h)| json!({"from": f, "to": t, "held": h}))
            .collect();
        if !v.is_empty() {
            out.push((fleet, i, Value::Array(v)));
        }
    }
    out
}

fn report(out: &mut dyn Write, p: &Plan, mv: &[(String, u64, Value)], why: &[String]) -> Result<()> {
    writeln!(out, "compact {} · line {} · cut {:.9}", p.repo, p.line, p.cut)?;
    let [a, b] = p.commits;
    writeln!(out, "commits   {} {a} → {b} (1 root + {})", p.line, b - 1)?;
    writeln!(
        out,
        "root      {:.12} · history before the cut in {}",
        p.root, p.archive
    )?;
    writeln!(out, "chain     {}", p.chain.len())?;
    for s in &p.chain {
        writeln!(
            out,
            "  {:.9} ← {:.9}  {}  holds {} old commits",
            s.new, s.old, s.subject, s.held
        )?;
    }
    let n = |c: &str| p.refs.iter().filter(|m| m.class == c).count();
    let classes = ["Line", "SameTree", "Carried", "Detached", "Merged", "Notes"];
    let each: Vec<String> = classes.iter().map(|c| format!("{c} {}", n(c))).collect();
    writeln!(out, "refs      {}: {}", p.refs.len(), each.join(" · "))?;
    let open: Vec<&RefMove> = p.refs.iter().filter(|m| m.pr.is_some()).collect();
    writeln!(out, "open PRs  {}, each costs one fresh review", open.len())?;
    for m in open {
        let new = m.new.as_deref().unwrap_or("deleted");
        writeln!(
            out,
            "  #{} {}  {:.9} → {new:.9}  {}",
            m.pr.unwrap_or_default(),
            m.name,
            m.old,
            m.class
        )?;
    }
    match p.main() {
        Some(m) => writeln!(
            out,
            "main      {:.9} → {:.9} · its own card",
            m.old,
            m.new.as_deref().unwrap_or_default()
        )?,
        None => writeln!(out, "main      not moved")?,
    }
    writeln!(out, "events    {} issues to remap", mv.len())?;
    for (fleet, i, v) in mv {
        for x in v.as_array().into_iter().flatten() {
            let held = x["held"].as_array().map_or(0, Vec::len);
            let (f, t) = (
                x["from"].as_str().unwrap_or_default(),
                x["to"].as_str().unwrap_or_default(),
            );
            writeln!(out, "  {fleet}#{i}  {f:.9} → {t:.9}, {held} lane heads held")?;
        }
    }
    let checks = if why.is_empty() {
        "ready ✓".to_string()
    } else {
        why.join(" · ")
    };
    writeln!(out, "checks    tip trees equal ✓ · {checks}")?;
    writeln!(out, "new tip   {} · nothing pushed", p.new_tip)?;
    Ok(())
}

const RESET: &str = "cd \"$1\" && git fetch -q --prune --force origin '+refs/heads/*:refs/remotes/origin/*' \
    '+refs/notes/*:refs/notes/*' && b=$(git symbolic-ref -q --short HEAD) && git reset -q --keep \"origin/$b\"";

fn reset(t: &Repo, out: &mut dyn Write) -> Result<()> {
    let dirs = std::iter::once((On::default(), t.clone.as_path())).chain(lanes(t));
    for (on, d) in dirs {
        let host = on.0.map(|h| format!("{h}:")).unwrap_or_default();
        match on.run_in("sh", &["-c", RESET, "sh", &d.to_string_lossy()], "") {
            Ok(o) if o.code == 0 => writeln!(out, "reset {host}{}", d.display())?,
            Ok(o) => writeln!(out, "could not reset {host}{}: {}", d.display(), o.stderr.trim())?,
            Err(e) => writeln!(out, "could not reset {host}{}: {e:#}", d.display())?,
        }
    }
    Ok(())
}

fn running(thread: Option<&str>, want: &str) -> bool {
    let Some(t) = thread else { return false };
    let Ok(v) = json("drover", &["thread", "show", t, "--json"]) else {
        return false;
    };
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let said =
        |p: &str| (items.iter()).any(|i| i["from"] == "system" && i["text"].as_str().is_some_and(|s| s.starts_with(p)));
    (items.iter())
        .filter(|i| i["card"]["cmd"] == want)
        .filter_map(|i| i["card"]["id"].as_str())
        .any(|id| {
            said(&format!("run {id} approved by owner, running in"))
                && !said(&format!("run {id} approved by owner, ran in"))
        })
}

fn card(thread: &str, cmd: &str, what: &str, blast: &str) -> Result<String> {
    let here = std::env::current_dir()?;
    let a = [
        "card",
        "post",
        "--cmd",
        cmd,
        "--why",
        what,
        "--blast",
        blast,
        "--dir",
        &here.to_string_lossy(),
        "--timeout",
        "3600",
        thread,
    ];
    let id = ok("drover", &a)?;
    Ok(format!("run card {} posted: {cmd}", id.trim()))
}

fn run(t: &Repo, act: &Act, dry: bool, out: &mut dyn Write) -> Result<()> {
    let all = format!(
        "force-moves every ref of {} but main (archive first, lease on each)",
        t.repo
    );
    let main_blast = |m: &RefMove, to: &str| {
        format!(
            "force-moves main of {} to {to:.12} (lease on {:.12}); the push may fire the repo's main-merge workflows (a prod deploy, migrations)",
            t.repo, m.old
        )
    };
    let thread = t.thread.as_deref().filter(|_| !dry);
    match act {
        Act::Cut(cut) => {
            if let Some(done) = push::finished(t)? {
                return Ok(writeln!(out, "{done}")?);
            }
            let p = build(t, cut)?;
            let why = ready(t, &p);
            report(out, &p, &moves(&t.saved()?, &map(&t.dir)?, &p.root), &why)?;
            match thread {
                None => Ok(()),
                Some(_) if !why.is_empty() => refused("COMPACT_NOT_READY", why.join("; ")),
                Some(th) => {
                    let cmd = t.cmd(&["--push", &p.new_tip]);
                    writeln!(
                        out,
                        "{}",
                        card(th, &cmd, "compact: archive, then one atomic push", &all)?
                    )?;
                    if let Some(m) = p.main() {
                        let to = m.new.as_deref().unwrap_or_default();
                        let cmd = t.cmd(&["--main", "--push", to]);
                        let what = "compact main, after the line's card";
                        writeln!(out, "{}", card(th, &cmd, what, &main_blast(m, to))?)?;
                    }
                    Ok(())
                }
            }
        }
        Act::Undo => {
            let p = load(&t.dir)?;
            push::at(t, &p, true, false)?;
            push::at(t, &p, true, true)?;
            writeln!(
                out,
                "undo {} · {} refs back to their old values · line {:.9} → {:.9}",
                p.repo,
                p.refs.len(),
                p.new_tip,
                p.old_tip
            )?;
            match thread {
                None => Ok(writeln!(out, "nothing pushed")?),
                Some(th) => {
                    let cmd = t.cmd(&["--undo", "--push", &p.old_tip]);
                    writeln!(out, "{}", card(th, &cmd, "undo the compact", &all)?)?;
                    if let Some(m) = p.main() {
                        let cmd = t.cmd(&["--undo", "--main", "--push", &m.old]);
                        let what = "undo the compact of main";
                        writeln!(out, "{}", card(th, &cmd, what, &main_blast(m, &m.old))?)?;
                    }
                    Ok(())
                }
            }
        }
        Act::Push { tip, undo, main } => {
            let mut words = vec![];
            words.extend(undo.then_some("--undo"));
            words.extend(main.then_some("--main"));
            words.extend(["--push", tip.as_str()]);
            let want = t.cmd(&words);
            if !running(t.thread.as_deref(), &want) {
                return refused(
                    "COMPACT_UNAPPROVED",
                    format!("`{want}` runs only inside its approved run card"),
                );
            }
            let p = load(&t.dir)?;
            let m = p.main();
            let at = match (*main, *undo) {
                (false, false) => Some(p.new_tip.as_str()),
                (false, true) => Some(p.old_tip.as_str()),
                (true, false) => m.and_then(|m| m.new.as_deref()),
                (true, true) => m.map(|m| m.old.as_str()),
            };
            if at != Some(tip.as_str()) {
                return refused(
                    "COMPACT_MOVED",
                    format!("{tip:.12} is not this plan's tip; run --cut again"),
                );
            }
            if *main {
                if !*undo {
                    unready(t, &p)?;
                    if push::finished(t)?.is_none() {
                        return refused(
                            "COMPACT_NOT_READY",
                            format!("{} is not compacted yet: its card runs first", p.line),
                        );
                    }
                }
                push::main(t, &p, *undo)?;
            } else if *undo {
                push::undo(t, &p)?;
            } else {
                unready(t, &p)?;
                push::push(t, &p)?;
            }
            reset(t, out)
        }
        Act::Reset => reset(t, out),
    }
}

fn repo(o: &Opts) -> Result<Repo> {
    if o.repo.split('/').filter(|w| !w.is_empty()).count() != 2 {
        return refused("BAD_VALUE", format!("--repo {} is not owner/name", o.repo));
    }
    let abs = |p: &Path| std::path::absolute(p);
    let clone = abs(&o.clone)?;
    let dir = match &o.dir {
        Some(d) => abs(d)?,
        None => {
            let home = std::env::var_os("HOME").context("no HOME: pass --dir")?;
            PathBuf::from(home).join(".local/state/compact").join(&o.repo)
        }
    };
    let saved = o.saved.as_deref().map(abs).transpose()?;
    let lanes = (o.lanes.iter())
        .map(|l| match l.split_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains('/') => (Some(h.to_string()), PathBuf::from(p)),
            _ => (None, PathBuf::from(l)),
        })
        .collect();
    let mut base: Vec<String> = ["compact", "--repo", &o.repo, "--line", &o.line, "--clone"]
        .map(String::from)
        .into();
    base.push(clone.to_string_lossy().into());
    for l in &o.lanes {
        base.extend(["--lane".into(), l.clone()]);
    }
    for f in &o.fleets {
        base.extend(["--fleet".into(), f.clone()]);
    }
    if let Some(s) = &saved {
        base.extend(["--saved".into(), s.to_string_lossy().into()]);
    }
    base.extend(["--dir".into(), dir.to_string_lossy().into()]);
    if let Some(th) = &o.thread {
        base.extend(["--thread".into(), th.clone()]);
    }
    Ok(Repo {
        repo: o.repo.clone(),
        line: o.line.clone(),
        clone,
        lanes,
        fleets: o.fleets.clone(),
        saved,
        dir,
        thread: o.thread.clone(),
        base,
    })
}

fn main() {
    let o = Opts::parse();
    let act = match (o.cut.clone(), o.push.clone()) {
        _ if o.reset => Act::Reset,
        (_, Some(tip)) => Act::Push {
            tip,
            undo: o.undo,
            main: o.main,
        },
        _ if o.undo => Act::Undo,
        (Some(cut), None) => Act::Cut(cut),
        (None, None) => {
            eprintln!(
                "usage: compact --repo <owner/name> --line <branch> --clone <dir> --cut <date|sha> | --undo | --reset"
            );
            std::process::exit(2);
        }
    };
    let mut out = std::io::stdout();
    let res = repo(&o).and_then(|t| run(&t, &act, o.dry_run, &mut out));
    if let Err(e) = res {
        let code = if e.downcast_ref::<Refused>().is_some() { 3 } else { 1 };
        let _ = out.flush();
        eprintln!("{e:#}");
        std::process::exit(code);
    }
}
