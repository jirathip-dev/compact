use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

const GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE/gh.log"
case "$1 $2" in
  "pr list") cat "$FAKE/prs.json" ;;
  "repo view") echo '{"visibility": "PRIVATE"}' ;;
  "repo create") ;;
  "pr view") cat "$FAKE/pr-$3.json" ;;
  "pr edit") printf '%s' "$7" > "$FAKE/edit-$3" ;;
  *) exit 1 ;;
esac
"#;

const DROVER: &str = r#"#!/bin/sh
case "$1 $2" in
  "card post")
    shift 2
    while [ $# -gt 1 ]; do
      case "$1" in --cmd) cmd=$2 ;; --blast) blast=$2 ;; esac
      shift 2
    done
    id="r$(( $(ls "$FAKE/cards" | grep -c '\.cmd$') + 1 ))"
    printf '%s' "$cmd" > "$FAKE/cards/$id.cmd"
    printf '%s' "$blast" > "$FAKE/cards/$id.blast"
    echo "$id" ;;
  "thread show") cat "$FAKE/thread.json" ;;
  "status "*) cat "$FAKE/status.json" ;;
  *) exit 1 ;;
esac
"#;

fn tmpdir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn g(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=staging",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    g(dir, &["add", "."]);
    g(dir, &["commit", "-qm", msg]);
    g(dir, &["rev-parse", "HEAD"])
}

const NOTE: &str = "Decided: count twice\nOwner: the meter counts two\n";

struct Fake {
    dir: PathBuf,
    bin: PathBuf,
    args: Vec<String>,
}

impl Fake {
    fn new(dir: &Path, args: Vec<String>) -> Fake {
        let (fake, bin) = (dir.join("fake"), dir.join("bin"));
        std::fs::create_dir_all(fake.join("cards")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        for (name, text) in [("gh", GH), ("drover", DROVER)] {
            let p = bin.join(name);
            std::fs::write(&p, text).unwrap();
            Command::new("chmod").arg("+x").arg(&p).status().unwrap();
        }
        std::fs::write(fake.join("status.json"), "[]").unwrap();
        let f = Fake { dir: fake, bin, args };
        f.thread(&[], &[]);
        f
    }
    fn put(&self, file: &str, text: &str) {
        std::fs::write(self.dir.join(file), text).unwrap();
    }
    fn get(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default()
    }
    fn cards(&self) -> Vec<(String, String, String)> {
        (1..)
            .map(|n| format!("r{n}"))
            .take_while(|id| self.dir.join(format!("cards/{id}.cmd")).exists())
            .map(|id| {
                let (c, b) = (
                    self.get(&format!("cards/{id}.cmd")),
                    self.get(&format!("cards/{id}.blast")),
                );
                (id, c, b)
            })
            .collect()
    }
    fn thread(&self, approved: &[&str], ran: &[&str]) {
        let mut items: Vec<serde_json::Value> = (self.cards().into_iter())
            .map(|(id, cmd, _)| json!({"from": "orchestrator", "card": {"id": id, "cmd": cmd}, "open": false}))
            .collect();
        for id in approved {
            let text = format!("run {id} approved by owner, running in /x…");
            items.push(json!({"from": "system", "text": text}));
        }
        for id in ran {
            let text = format!("run {id} approved by owner, ran in /x: exit 0");
            items.push(json!({"from": "system", "text": text}));
        }
        self.put("thread.json", &json!({"thread": "t", "items": items}).to_string());
    }
    fn run(&self, extra: &[&str]) -> (i32, String) {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap());
        let o = Command::new(env!("CARGO_BIN_EXE_compact"))
            .args(&self.args)
            .args(extra)
            .env("PATH", path)
            .env("FAKE", &self.dir)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        (o.status.code().unwrap_or(-1), out)
    }
}

#[test]
fn compact_keeps_every_tree_needs_a_fresh_review_and_undoes_exactly() {
    let dir = tmpdir("compact-sim");
    let (origin, archive, seed, local, lane) = (
        dir.join("jirathip-dev/sendmeter"),
        dir.join("jirathip-dev/sendmeter-history"),
        dir.join("seed"),
        dir.join("local"),
        dir.join("lane"),
    );
    for b in [&origin, &archive] {
        g(&dir, &["init", "-q", "--bare", b.to_str().unwrap()]);
    }
    g(&dir, &["init", "-q", seed.to_str().unwrap()]);
    g(&seed, &["remote", "add", "origin", origin.to_str().unwrap()]);
    commit(&seed, "a.rs", "one\ntwo\nthree\n", "base");
    g(&seed, &["checkout", "-qb", "old-wip"]);
    commit(&seed, "w.rs", "wip\n", "wip: never merged");
    g(&seed, &["checkout", "-q", "staging"]);
    g(&seed, &["checkout", "-qb", "issue-5"]);
    let c5 = commit(&seed, "a.rs", "ONE\ntwo\nthree\n", "a: old line");
    g(
        &seed,
        &["notes", "--ref", "why", "add", "-m", "Owner: the old note", &c5],
    );
    g(&seed, &["checkout", "-q", "staging"]);
    g(&seed, &["merge", "-q", "--no-ff", "-m", "a: old (#5) (#50)", "issue-5"]);
    let cut = g(&seed, &["rev-parse", "HEAD"]);
    g(&seed, &["checkout", "-qb", "issue-1"]);
    commit(&seed, "a.rs", "ONE\nTWO\nthree\n", "a: two");
    g(&seed, &["checkout", "-q", "staging"]);
    commit(&seed, "b.rs", "b\n", "direct: b");
    g(&seed, &["checkout", "-q", "issue-1"]);
    g(&seed, &["merge", "-q", "--no-edit", "staging"]);
    let l2 = commit(&seed, "c.rs", "c\n", "a: test it");
    g(&seed, &["notes", "--ref", "why", "add", "-m", NOTE.trim(), &l2]);
    g(&seed, &["checkout", "-q", "staging"]);
    g(&seed, &["merge", "-q", "--no-ff", "-m", "a: two (#1) (#11)", "issue-1"]);
    let m1 = g(&seed, &["rev-parse", "HEAD"]);
    g(&seed, &["checkout", "-qb", "main", &format!("{cut}^1")]);
    g(&seed, &["merge", "-q", "--no-ff", "-m", "release", &m1]);
    g(&seed, &["checkout", "-q", "staging"]);
    g(&seed, &["checkout", "-qb", "issue-2"]);
    commit(&seed, "d.rs", "d\n", "d: add");
    g(&seed, &["checkout", "-q", "staging"]);
    commit(&seed, "e.rs", "e\n", "direct: e");
    g(&seed, &["checkout", "-q", "issue-2"]);
    g(&seed, &["merge", "-q", "--no-edit", "staging"]);
    let o2 = g(&seed, &["rev-parse", "HEAD"]);
    let all = [
        "staging",
        "main",
        "issue-1",
        "issue-2",
        "issue-5",
        "old-wip",
        "refs/notes/why",
    ];
    g(&seed, &[&["push", "-q", "origin"][..], &all].concat());
    g(&seed, &["push", "-q", "origin", "issue-2:refs/pull/22/head"]);
    for d in [&local, &lane] {
        g(&dir, &["clone", "-q", origin.to_str().unwrap(), d.to_str().unwrap()]);
    }
    g(&local, &["fetch", "-q", "origin", "refs/notes/why:refs/notes/why"]);
    g(&lane, &["checkout", "-q", "issue-2"]);
    let saved = [
        (5, &c5),
        (5, &cut),
        (5, &c5),
        (1, &l2),
        (1, &l2),
        (1, &m1),
        (1, &l2),
        (2, &o2),
    ];
    let rows: String = saved.iter().map(|(i, s)| format!("sendmeter\t{i}\t{s}\n")).collect();
    std::fs::write(dir.join("saved.tsv"), rows).unwrap();
    let (saved_file, state) = (dir.join("saved.tsv"), dir.join("state/compact"));
    let args = [
        "--repo",
        "jirathip-dev/sendmeter",
        "--line",
        "staging",
        "--clone",
        local.to_str().unwrap(),
        "--lane",
        lane.to_str().unwrap(),
        "--fleet",
        "sendmeter",
        "--saved",
        saved_file.to_str().unwrap(),
        "--dir",
        state.to_str().unwrap(),
    ];
    let fake = Fake::new(&dir, args.map(String::from).into());
    let refs = || g(&dir, &["ls-remote", origin.to_str().unwrap()]);
    let before = refs();
    let undo = before.lines().find(|l| l.ends_with("refs/heads/staging")).unwrap()[..40].to_string();
    let old_tree = g(&local, &["rev-parse", "origin/staging^{tree}"]);
    let main_old = g(&local, &["rev-parse", "origin/main"]);
    let main_tree = g(&local, &["rev-parse", "origin/main^{tree}"]);
    let body = format!("Closes #2\n\n## Evidence (head {o2:.9})");
    let prs = json!([{"number": 22, "headRefName": "issue-2", "isCrossRepository": false}]);
    fake.put("prs.json", &prs.to_string());
    fake.put("pr-22.json", &json!({"body": body}).to_string());

    let (code, out) = fake.run(&["--cut", &cut, "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("commits   staging 9 → 4 (1 root + 3)"), "{out}");
    assert!(out.contains("a: two (#1) (#11)  holds 3 old commits"), "{out}");
    assert!(
        out.contains("#22 refs/heads/issue-2") && out.contains("Carried"),
        "{out}"
    );
    assert!(out.contains("Detached 1 · Merged 2 · Notes 1"), "{out}");
    assert!(
        out.contains("events    2 issues to remap") && out.contains("1 lane heads held"),
        "{out}"
    );
    assert!(out.contains("checks    tip trees equal ✓ · ready ✓"), "{out}");
    assert_eq!(refs(), before, "a dry run pushes nothing");
    let tip = |o: &str| {
        o.lines()
            .find_map(|l| l.strip_prefix("new tip   "))
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .to_string()
    };
    let new = tip(&out);
    let (_, again) = fake.run(&["--cut", &cut, "--dry-run"]);
    assert_eq!(tip(&again), new, "the plan is the same on a second run");

    let moved = out
        .lines()
        .find_map(|l| l.strip_prefix(&format!("main      {main_old:.9} → ")));
    assert!(moved.unwrap().ends_with("· its own card"), "{out}");

    std::fs::write(lane.join("d.rs"), "changed\n").unwrap();
    fake.put("status.json", r#"[{"issue": 7, "state": "review(1)"}]"#);
    let (code, out) = fake.run(&["--cut", &cut, "--thread", "t"]);
    assert_eq!((code, out.contains("COMPACT_NOT_READY")), (3, true), "{out}");
    assert!(
        out.contains("#7 has a check lane working (review(1))") && out.contains("1 uncommitted change(s)"),
        "{out}"
    );
    assert!(fake.cards().is_empty(), "no card while not ready");
    g(&lane, &["checkout", "-q", "d.rs"]);
    fake.put("status.json", "[]");

    let (code, out) = fake.run(&["--push", &new]);
    assert_eq!((code, out.contains("COMPACT_UNAPPROVED")), (3, true), "{out}");
    let (code, out) = fake.run(&["--cut", &cut, "--thread", "t"]);
    assert_eq!(code, 0, "{out}");
    let posted = fake.cards();
    assert_eq!(posted.len(), 2, "two separate cards: {posted:?}");
    let (line, main) = (&posted[0], &posted[1]);
    let base = format!(
        "compact {} --thread t",
        args.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ")
    );
    assert_eq!(line.1, format!("{base} --push {new}"));
    assert!(
        line.2.contains("every ref of jirathip-dev/sendmeter but main"),
        "{line:?}"
    );
    let main_new = main
        .1
        .strip_prefix(&format!("{base} --main --push "))
        .unwrap()
        .to_string();
    assert!(moved.unwrap().starts_with(&main_new[..9]), "{main:?}");
    assert!(
        main.2.contains(&format!("lease on {main_old:.12}")) && main.2.contains("main-merge workflows"),
        "{main:?}"
    );
    let on = |b: &str| g(&dir, &["--git-dir", origin.to_str().unwrap(), "rev-parse", b]);
    let t = ["--thread", "t"];
    fake.thread(&["r1"], &[]);
    let (code, out) = fake.run(&[&t[..], &["--main", "--push", &main_new]].concat());
    assert_eq!(
        (code, out.contains("COMPACT_UNAPPROVED")),
        (3, true),
        "the line's card does not cover main: {out}"
    );
    fake.thread(&["r1", "r2"], &[]);
    let (code, out) = fake.run(&[&t[..], &["--main", "--push", &main_new]].concat());
    assert_eq!((code, out.contains("COMPACT_NOT_READY")), (3, true), "{out}");
    assert_eq!(refs(), before, "main waits for the line's card");
    let (code, out) = fake.run(&[&t[..], &["--push", &new]].concat());
    assert_eq!(code, 0, "{out}");
    assert_eq!(on("staging"), new);
    assert_eq!(on("main"), main_old, "the line's card leaves main alone");
    fake.thread(&["r1", "r2"], &["r1"]);
    let (code, out) = fake.run(&[&t[..], &["--push", &new]].concat());
    assert_eq!(
        (code, out.contains("COMPACT_UNAPPROVED")),
        (3, true),
        "a card that ran is spent: {out}"
    );
    let (code, out) = fake.run(&[&t[..], &["--main", "--push", &main_new]].concat());
    assert_eq!(code, 0, "{out}");
    assert_eq!(on("main"), main_new);
    assert_ne!(main_new, main_old);
    assert_eq!(on("main^{tree}"), main_tree, "main's tip tree is identical");
    assert_eq!(on("main^1"), on("staging~1"), "main's release stands on the new line");
    assert_eq!(on("staging^{tree}"), old_tree, "the tip tree is identical");
    assert_eq!(
        g(
            &dir,
            &["--git-dir", origin.to_str().unwrap(), "rev-list", "--count", "staging"]
        ),
        "4"
    );
    assert!(
        !refs().contains("refs/heads/issue-5"),
        "a fully merged branch is deleted"
    );
    let arc = |b: &str| g(&dir, &["--git-dir", archive.to_str().unwrap(), "rev-parse", b]);
    assert_eq!(arc("staging"), undo, "the archive holds the old tip");
    assert_eq!(arc("pull/22"), o2, "and each PR head");
    assert_eq!(arc("drover-compact:compact-map.tsv").len(), 40);
    let o2_new = on("issue-2");
    assert_eq!(
        on("issue-2^{tree}"),
        g(&seed, &["rev-parse", &format!("{o2}^{{tree}}")])
    );
    assert_eq!(
        fake.get("edit-22"),
        format!("Closes #2\n\n## Evidence (head {o2_new:.9})")
    );
    assert_eq!(g(&local, &["rev-parse", "HEAD"]), new, "the fleet's clone is reset");
    assert_eq!(g(&lane, &["rev-parse", "HEAD"]), o2_new, "and each lane folder");
    let records: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("state/compact/moved.json")).unwrap()).unwrap();
    let issues: Vec<u64> = (records.as_array().unwrap().iter())
        .map(|r| r["issue"].as_u64().unwrap())
        .collect();
    assert_eq!(issues, [1, 2], "the records drover logs: {records}");
    assert_eq!(records[1]["moved"][0], json!({"from": o2, "to": o2_new, "held": []}));
    assert_eq!(records[0]["moved"][0]["held"].as_array().unwrap().len(), 1);

    let (code, out) = fake.run(&["--cut", &cut, "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("jirathip-dev/sendmeter is compacted at"), "{out}");

    let (code, out) = fake.run(&["--undo", "--thread", "t"]);
    assert_eq!(code, 0, "{out}");
    let posted = fake.cards();
    let undo_cmds: Vec<&str> = posted[2..].iter().map(|p| p.1.as_str()).collect();
    assert_eq!(
        undo_cmds,
        [
            format!("{base} --undo --push {undo}"),
            format!("{base} --undo --main --push {main_old}")
        ]
    );
    fake.thread(&["r1", "r2", "r3"], &["r1", "r2"]);
    let (code, out) = fake.run(&[&t[..], &["--undo", "--push", &undo]].concat());
    assert_eq!(code, 0, "{out}");
    assert_eq!(on("main"), main_new, "the line's undo leaves main alone");
    fake.thread(&["r1", "r2", "r3", "r4"], &["r1", "r2", "r3"]);
    let (code, out) = fake.run(&[&t[..], &["--undo", "--main", "--push", &main_old]].concat());
    assert_eq!(code, 0, "{out}");
    assert_eq!(refs(), before, "undo restores the old tip and every ref exactly");
    assert_eq!(
        std::fs::read_to_string(dir.join("state/compact/undone"))
            .unwrap()
            .trim(),
        new
    );
    assert_eq!(g(&local, &["rev-parse", "HEAD"]), undo, "the clone is reset back");
}

#[test]
fn a_cut_date_with_no_commit_before_it_is_refused() {
    let dir = tmpdir("compact-date");
    let (origin, local) = (dir.join("o/r"), dir.join("local"));
    g(&dir, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    g(
        &dir,
        &["clone", "-q", origin.to_str().unwrap(), local.to_str().unwrap()],
    );
    commit(&local, "a", "a\n", "base");
    g(&local, &["push", "-q", "origin", "HEAD:staging"]);
    let state = dir.join("state");
    let args = [
        "--repo",
        "o/r",
        "--line",
        "staging",
        "--clone",
        local.to_str().unwrap(),
        "--dir",
        state.to_str().unwrap(),
    ];
    let fake = Fake::new(&dir, args.map(String::from).into());
    fake.put("prs.json", "[]");
    let (code, out) = fake.run(&["--cut", "1990-01-01", "--dry-run"]);
    assert_eq!(
        (
            code,
            out.contains("REFUSED BAD_VALUE: no commit on staging before 1990-01-01")
        ),
        (3, true),
        "{out}"
    );
    let (code, out) = fake.run(&["--cut", "2090-01-01", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("commits   staging 1 → 1 (1 root + 0)"), "{out}");
}
