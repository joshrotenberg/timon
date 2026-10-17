//! Tasks (#68): tasks.lua and scripts in both layers, plans, words, metadata and records.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.project().join(".timon")).unwrap();
        std::fs::create_dir_all(s.dir.path().join("logs")).unwrap();
        s
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("home/proj")
    }

    fn user(&self) -> PathBuf {
        self.dir.path().join("timon/config")
    }

    fn write(&self, path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn project_tasks(&self, text: &str) {
        self.write(self.project().join(".timon/tasks.lua"), text);
    }

    fn timon(&self, args: &[&str]) -> Output {
        self.timon_env(args, &[])
    }

    fn timon_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
            .current_dir(self.project())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path().join("home"))
            .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
            .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
            .env("FAKE_LOG_DIR", self.dir.path().join("logs"));
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn trust(&self) {
        let o = self.timon(&["trust"]);
        assert!(o.status.success(), "{}", err(&o));
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.timon(args);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        out(&o)
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.ok(args)).unwrap()
    }

    /// The newest script run.
    fn last_run(&self) -> serde_json::Value {
        let list = self.json(&["runs", "--scripts", "--json"]);
        let id = list[0]["id"].as_str().unwrap().to_owned();
        self.json(&["runs", "show", &id])
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn tasks_come_from_both_layers_and_the_project_replaces_the_user() {
    let s = Sandbox::new();
    s.write(
        s.user().join("tasks.lua"),
        "return { shared = 'echo user', both = { description = 'user version', run = 'echo user' } }",
    );
    s.project_tasks("return { both = { description = 'project version', run = 'echo project' } }");
    s.write(
        s.project().join(".timon/scripts/notes.lua"),
        "-- A project script\nreturn 'notes'",
    );
    let o = s.timon(&["tasks"]);
    assert_eq!(
        out(&o),
        "both    user  user version\nshared  user  echo user\n"
    );
    assert!(
        err(&o).contains("is not trusted, so its tasks are not listed"),
        "{}",
        err(&o)
    );
    let o = s.timon(&["notes"]);
    assert!(err(&o).contains("untrusted: notes is in"), "{}", err(&o));
    assert!(err(&o).contains("timon trust"), "{}", err(&o));

    s.trust();
    assert_eq!(
        s.ok(&["tasks"]),
        "both    project  project version [replaces the user task]\n\
         notes   project  A project script\n\
         shared  user     echo user\n"
    );
    assert_eq!(s.ok(&["both"]), "project\n");
    assert_eq!(s.ok(&["notes"]), "notes\n");
    let list = s.json(&["tasks", "--json"]);
    let notes = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "notes")
        .unwrap();
    assert_eq!(notes["kind"], "script");
    assert_eq!(list[0]["replaces"], "user");
}

#[test]
fn dependencies_run_once_each_depth_first_and_pass_their_results() {
    let s = Sandbox::new();
    s.project_tasks(
        "local order = {}
         local function step(name)
           return function(opts, deps) order[#order + 1] = name; return name end
         end
         return {
           d = { run = step('d') },
           b = { depends = { 'd' }, run = step('b') },
           c = { depends = { 'd' }, run = step('c') },
           a = {
             depends = { 'b', 'c' },
             run = function(opts, deps)
               return table.concat(order, ',') .. ' then a with ' .. deps.b .. deps.c
             end,
           },
           gates = { description = 'Only dependencies', depends = { 'b' } },
         }",
    );
    s.trust();
    assert_eq!(s.ok(&["a"]), "d,b,c then a with bc\n");
    let run = s.last_run();
    assert_eq!(run["request"]["kind"], "task");
    assert_eq!(run["request"]["name"], "a");
    assert_eq!(
        run["request"]["plan"],
        serde_json::json!(["d", "b", "c", "a"])
    );
    let statuses: Vec<(&str, &str)> = run["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        statuses,
        [("d", "ok"), ("b", "ok"), ("c", "ok"), ("a", "ok")]
    );
    assert_eq!(
        s.ok(&["gates"]),
        "",
        "a task with only dependencies returns nothing"
    );
}

#[test]
fn one_job_runs_the_plan_in_declared_order_not_level_by_level() {
    let s = Sandbox::new();
    s.project_tasks(
        "local order = {}
         local function step(name)
           return function() order[#order + 1] = name end
         end
         return {
           d = step('d'),
           b = { depends = { 'd' }, run = step('b') },
           c = step('c'),
           a = { depends = { 'b', 'c' }, run = function() return table.concat(order, ',') end },
           broken = { depends = { 'd' }, run = function() error('broken') end },
           later = function() io.open('later-ran', 'w'):close() end,
           stops = { depends = { 'broken', 'later' }, run = 'true' },
         }",
    );
    s.trust();
    let planned: Vec<String> = s.json(&["--dry-run", "a"])["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(planned, ["d", "b", "c", "a"]);
    assert_eq!(s.ok(&["a"]), "d,b,c\n", "the order the dry run shows");

    let o = s.timon(&["stops"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        !s.project().join("later-ran").exists(),
        "a later sibling does not run after an earlier one fails"
    );
    let names: Vec<String> = s.last_run()["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["d", "broken"]);
}

#[test]
fn the_first_failure_stops_the_plan_with_its_exit_code() {
    let s = Sandbox::new();
    s.project_tasks(
        "return {
           first = 'true',
           lint = { depends = { 'first' }, run = function() error{ kind = 'lint', message = 'two findings', exit = 4 } end },
           release = { depends = { 'lint' }, run = function() print('released') end },
         }",
    );
    s.trust();
    let o = s.timon(&["release"]);
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains("lint: two findings"), "{}", err(&o));
    assert!(!out(&o).contains("released"));
    let run = s.last_run();
    assert_eq!(run["result"]["status"], "failed");
    let tasks = &run["result"]["tasks"];
    assert_eq!(tasks.as_array().unwrap().len(), 2, "{tasks}");
    assert_eq!(tasks[1]["name"], "lint");
    assert_eq!(tasks[1]["status"], "failed");
}

#[test]
fn words_reach_the_task_and_a_command_task_appends_them() {
    let s = Sandbox::new();
    s.project_tasks(
        "return {
           show = function(opts, deps) return table.concat(opts.args, '|') end,
           each = { description = 'printf each word', run = 'printf %s.' },
         }",
    );
    s.trust();
    assert_eq!(s.ok(&["show", "a", "b", "--", "--c", "-d"]), "a|b|--c|-d\n");
    assert_eq!(s.ok(&["each", "--", "x y", "--z"]), "x y.--z.");
    let o = s.timon(&["show", "--c"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("task \"show\": unknown option --c"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("it takes no options; put words that start with - after --"),
        "{}",
        err(&o)
    );
    assert_eq!(s.ok(&["task", "show", "x"]), "x\n");
    assert_eq!(
        s.ok(&["run", "show", "y"]),
        "y\n",
        "run is an alias of task"
    );
}

#[test]
fn timon_commands_win_and_timon_task_reaches_a_task_they_hide() {
    let s = Sandbox::new();
    s.project_tasks(
        "return { runs = { description = 'Mine', run = function() return 'my runs' end } }",
    );
    s.trust();
    assert!(
        s.ok(&["tasks"])
            .contains("[a timon command has this name: timon task runs]")
    );
    assert_eq!(s.json(&["runs", "--json"]), serde_json::json!([]));
    assert_eq!(s.ok(&["task", "runs"]), "my runs\n");
    let o = s.timon(&["rnus"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("did you mean \"runs\"?"), "{}", err(&o));
}

#[test]
fn aliases_groups_private_confirm_and_platforms() {
    let s = Sandbox::new();
    let elsewhere = if cfg!(target_os = "linux") {
        "macos"
    } else {
        "linux"
    };
    s.project_tasks(&format!(
        "return {{
           build = {{ description = 'Build', aliases = {{ 'b' }}, run = function() return 'built' end }},
           fmt = {{ description = 'Format', group = 'gates', run = 'true' }},
           lint = {{ description = 'Lint', group = 'gates', run = 'true' }},
           hidden = {{ description = 'Plumbing', private = true, run = function() return 'ran' end }},
           drop = {{ description = 'Drop it', confirm = 'Drop the table?', run = function() return 'dropped' end }},
           later = {{ description = 'Depends on a confirm', depends = {{ 'drop' }}, run = function() return 'after' end }},
           notary = {{ description = 'Elsewhere', platforms = {{ '{elsewhere}' }}, run = 'true' }},
           uses = {{ depends = {{ 'notary' }}, run = 'true' }},
         }}"
    ));
    s.trust();
    let listing = s.ok(&["tasks"]);
    assert!(
        listing.contains("build   project  Build (also b)\n"),
        "{listing}"
    );
    assert!(
        listing.contains("\ngates\n  fmt     project  Format\n  lint    project  Lint\n"),
        "{listing}"
    );
    assert!(
        listing.contains(&format!("[unavailable: {elsewhere} only]")),
        "{listing}"
    );
    assert!(!listing.contains("hidden"), "{listing}");
    assert!(s.ok(&["tasks", "--all"]).contains("Plumbing [private]"));
    assert_eq!(s.ok(&["b"]), "built\n");
    assert_eq!(s.ok(&["hidden"]), "ran\n");

    for task in ["drop", "later"] {
        let o = s.timon(&[task]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            err(&o).contains("confirm: task \"drop\" asks: Drop the table?"),
            "{}",
            err(&o)
        );
        assert!(
            err(&o).contains(&format!("timon --yes {task}")),
            "{}",
            err(&o)
        );
    }
    assert_eq!(s.ok(&["--yes", "later"]), "after\n");

    let o = s.timon(&["uses"]);
    assert!(
        err(&o).contains(&format!(
            "platform: task \"notary\" runs only on {elsewhere}"
        )),
        "{}",
        err(&o)
    );
}

#[test]
fn a_tasks_file_is_checked_when_it_loads() {
    let s = Sandbox::new();
    s.trust();
    let cases = [
        (
            "return { x = { descripton = 'y', run = 'true' } }",
            "unknown field \"descripton\" in task \"x\"",
        ),
        (
            "return { x = { descripton = 'y', run = 'true' } }",
            "did you mean \"description\"?",
        ),
        (
            "return { x = { description = 'y' } }",
            "task \"x\" has nothing to run",
        ),
        (
            "return { a = { depends = { 'b' } }, b = { depends = { 'a' } } }",
            "dependency cycle: a -> b -> a",
        ),
        (
            "return { a = { depends = { 'bb' } }, b = 'true' }",
            "did you mean \"b\"?",
        ),
        (
            "return { a = { aliases = { 'runs' }, run = 'true' } }",
            "alias \"runs\" of task \"a\" is a timon command",
        ),
        (
            "return { a = { aliases = { 'b' }, run = 'true' }, b = 'true' }",
            "alias \"b\" of task \"a\" is also a task's name",
        ),
        (
            "return { a = { platforms = { 'macOS' }, run = 'true' } }",
            "did you mean \"macos\"?",
        ),
        ("return { ['a b'] = 'true' }", "\"a b\" is not a task name"),
        (
            "return { x = 1 }",
            "task \"x\" must be a table, a command string or a function, got integer",
        ),
        ("return 'x'", "must return a table of tasks, got string"),
        (
            "local v = sh.out('echo hi')\nreturn {}",
            "sh.out cannot run while tasks load",
        ),
        (
            "agent.ask('hi')\nreturn {}",
            "an agent run cannot run while tasks load",
        ),
        (
            "sessions.summarize('x')\nreturn {}",
            "sessions.summarize cannot run while tasks load",
        ),
    ];
    for (source, expected) in cases {
        s.project_tasks(source);
        let o = s.timon(&["tasks"]);
        assert!(!o.status.success(), "{source}");
        assert!(err(&o).contains(expected), "{source}: {}", err(&o));
    }
    assert_eq!(
        std::fs::read_dir(s.dir.path().join("logs"))
            .unwrap()
            .count(),
        0,
        "no agent ran"
    );
    s.write(s.project().join(".timon/tasks.fnl"), "{}");
    let o = s.timon(&["tasks"]);
    assert!(
        err(&o).contains("has both tasks.lua and tasks.fnl"),
        "{}",
        err(&o)
    );
    s.project_tasks("return { notes = 'true' }");
    s.write(s.project().join(".timon/scripts/notes.lua"), "return 1");
    std::fs::remove_file(s.project().join(".timon/tasks.fnl")).unwrap();
    let o = s.timon(&["tasks"]);
    assert!(err(&o).contains("task \"notes\" is also"), "{}", err(&o));
}

#[test]
fn a_fennel_tasks_file() {
    let s = Sandbox::new();
    s.write(
        s.project().join(".timon/tasks.fnl"),
        "{:hi {:description \"From Fennel\"
               :run (fn [opts deps] (.. \"hi \" (or (. opts.args 1) \"you\")))}}",
    );
    s.trust();
    assert_eq!(s.ok(&["tasks"]), "hi  project  From Fennel\n");
    assert_eq!(s.ok(&["hi", "there"]), "hi there\n");
}

#[test]
fn a_script_dependency_runs_inside_the_plan() {
    let s = Sandbox::new();
    s.write(
        s.project().join(".timon/scripts/version.lua"),
        "-- The version\nreturn '1.2.' .. tostring(arg[1])",
    );
    s.project_tasks(
        "return { tag = { depends = { 'version' }, run = function(opts, deps) return 'v' .. deps.version end } }",
    );
    s.trust();
    assert_eq!(s.ok(&["tag"]), "v1.2.nil\n");
    assert_eq!(
        s.last_run()["request"]["plan"],
        serde_json::json!(["version", "tag"])
    );
    assert_eq!(
        s.ok(&["version", "3"]),
        "1.2.3\n",
        "on its own, a script gets its arguments"
    );
}

#[test]
fn a_task_budget_limits_its_agent_runs_and_a_plan_resumes() {
    let s = Sandbox::new();
    s.project_tasks(
        "return {
           plan = function() return agent.ask('plan the change') end,
           review = {
             depends = { 'plan' },
             budget = { runs = 2 },
             run = function(opts, deps)
               agent.ask('write the change')
               return agent.ask('review it, FAIL here the first time')
             end,
           },
           greedy = { budget = { runs = 1 }, run = function() agent.ask('one'); agent.ask('two') end },
         }",
    );
    s.trust();
    let o = s.timon(&["greedy"]);
    assert!(
        err(&o).contains("budget_exceeded: the scope's budget has started 1 runs"),
        "{}",
        err(&o)
    );

    let o = s.timon_env(&["review"], &[("FAKE_FAIL_ON", "FAIL")]);
    assert!(!o.status.success());
    let first = s.last_run();
    assert_eq!(first["result"]["runs"], 3);
    let id = first["result"]["id"].as_str().unwrap().to_owned();
    let o = s.timon(&["--resume", &id, "review"]);
    assert!(o.status.success(), "{}", err(&o));
    let second = s.last_run();
    assert_eq!(second["request"]["resumed_from"], id);
    assert_eq!(second["result"]["cached"], 2, "{}", second["result"]);
}

#[test]
fn the_skill_examples_load_as_tasks_files() {
    let skill = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/timon-tasks/SKILL.md"),
    )
    .unwrap();
    let block = |first: &str| -> String {
        let start = skill.find(first).unwrap();
        let end = skill[start..].find("```").unwrap();
        skill[start..start + end].to_owned()
    };
    let s = Sandbox::new();
    s.project_tasks(&block("-- .timon/tasks.lua"));
    s.trust();
    let all = s.json(&["tasks", "--all", "--json"]);
    let names: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] != "builtin")
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "check", "drop-db", "fmt", "lint", "release", "review", "test"
        ]
    );
    let plan = s.json(&["--dry-run", "release"]);
    assert_eq!(plan["plan"][0]["name"], "release");
    let help = s.ok(&["review", "--help"]);
    assert!(help.contains("-b, --base <string>"), "{help}");
    let listing = s.ok(&["tasks"]);
    assert!(
        listing.contains("check    project  Every gate (also c)"),
        "{listing}"
    );
    assert!(!listing.contains("drop-db"), "{listing}");

    std::fs::remove_file(s.project().join(".timon/tasks.lua")).unwrap();
    s.write(
        s.project().join(".timon/tasks.fnl"),
        &block(";; .timon/tasks.fnl"),
    );
    assert_eq!(s.ok(&["hello", "there"]), "hello there\n");
}

#[test]
fn typed_options_parse_from_flags_and_print_help() {
    let s = Sandbox::new();
    s.project_tasks(
        r#"return {
           review = {
             description = "Review the diff",
             options = {
               base = { type = "string", default = "main", short = "b", doc = "the branch" },
               model = '("sonnet" | "opus")?',
               tags = "string[]",
               verbose = "boolean",
               turns = "integer?",
             },
             run = function(opts, deps) return json.encode(opts) end,
           },
           needs = { options = { id = "integer" }, run = function(opts) return opts.id end },
         }"#,
    );
    s.trust();
    assert_eq!(
        s.ok(&["review"]),
        "{\"args\":[],\"base\":\"main\",\"tags\":[],\"verbose\":false}\n"
    );
    assert_eq!(
        s.ok(&[
            "review",
            "-b",
            "dev",
            "--model=opus",
            "--tags",
            "x",
            "--tags",
            "y",
            "--verbose",
            "--turns",
            "3",
            "w",
            "--",
            "--raw",
        ]),
        "{\"args\":[\"w\",\"--raw\"],\"base\":\"dev\",\"model\":\"opus\",\"tags\":[\"x\",\"y\"],\"turns\":3,\"verbose\":true}\n"
    );
    let help = s.ok(&["review", "--help"]);
    assert!(
        help.starts_with("Review the diff\n\nUsage: timon review [OPTIONS] [WORDS]...\n"),
        "{help}"
    );
    assert!(help.contains("  -b, --base <string>"), "{help}");
    assert!(help.contains("the branch [default: \"main\"]"), "{help}");
    for (args, expected, hint) in [
        (
            &["review", "--modle", "opus"][..],
            "task \"review\": unknown option --modle",
            "did you mean --model?",
        ),
        (
            &["review", "--model", "gpt"],
            "must be one of \"sonnet\", \"opus\", got \"gpt\"",
            "timon review --help",
        ),
        (
            &["review", "--turns", "x"],
            "--turns must be an integer, got \"x\"",
            "",
        ),
        (&["needs"], "task \"needs\": missing --id", "--id <integer>"),
    ] {
        let o = s.timon(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(err(&o).contains(expected), "{args:?}: {}", err(&o));
        assert!(err(&o).contains(hint), "{args:?}: {}", err(&o));
    }
    let list = s.json(&["tasks", "--json"]);
    let review = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "review")
        .unwrap();
    assert_eq!(review["options"][0]["flag"], "--base");
    assert_eq!(review["options"][0]["short"], "-b");
}

#[test]
fn jobs_run_independent_dependencies_together() {
    let s = Sandbox::new();
    s.project_tasks(
        "local log = {}
         local function step(name)
           return function()
             log[#log + 1] = name .. '+'
             sleep('200ms')
             log[#log + 1] = name .. '-'
             return name
           end
         end
         return {
           a = { run = step('a') },
           b = { run = step('b') },
           c = { depends = { 'a' }, run = step('c') },
           all = { depends = { 'a', 'b', 'c' }, run = function() return table.concat(log, ' ') end },
           loud = { depends = { 'x', 'y' }, run = 'true' },
           x = { run = 'echo from-x' },
           y = { run = 'echo from-y' },
         }",
    );
    s.trust();
    assert_eq!(s.ok(&["all"]), "a+ a- b+ b- c+ c-\n");
    assert_eq!(s.ok(&["--jobs", "2", "all"]), "a+ b+ a- b- c+ c-\n");
    let out = s.ok(&["-j", "2", "loud"]);
    assert!(
        out.contains("[x] from-x\n") && out.contains("[y] from-y\n"),
        "{out}"
    );
    let run = s.last_run();
    assert_eq!(run["request"]["jobs"], 2);
}

#[test]
fn a_dry_run_shows_the_plan_and_runs_nothing() {
    let s = Sandbox::new();
    s.project_tasks(
        "return {
           build = { run = 'touch built' },
           ship = {
             depends = { 'build' },
             confirm = 'Ship it?',
             options = { channel = { type = '\"beta\" | \"stable\"', default = 'beta' } },
             run = function() error('ran') end,
           },
         }",
    );
    s.trust();
    let plan = s.json(&["--dry-run", "ship", "--channel", "stable"]);
    assert_eq!(plan["task"], "ship");
    assert_eq!(plan["plan"][0]["name"], "build");
    assert_eq!(plan["plan"][0]["runs"], "touch built");
    assert_eq!(plan["plan"][1]["level"], 2);
    assert_eq!(plan["plan"][1]["runs"], "function");
    assert_eq!(plan["plan"][1]["opts"]["channel"], "stable");
    assert_eq!(plan["plan"][1]["confirm"], "Ship it?");
    assert!(!s.project().join("built").exists());
    assert_eq!(
        s.json(&["runs", "--scripts", "--json"]),
        serde_json::json!([])
    );
}

#[test]
fn task_run_calls_a_task_with_its_options_inside_the_same_run() {
    let s = Sandbox::new();
    s.write(
        s.project().join(".timon/scripts/notes.lua"),
        "-- Notes\nreturn 'notes ' .. tostring(arg[1])",
    );
    s.project_tasks(
        "local count = 0
         return {
           prep = function() count = count + 1; return 'prepared ' .. count end,
           publish = {
             depends = { 'prep' },
             options = { channel = { type = '\"beta\" | \"stable\"', default = 'beta' } },
             run = function(opts, deps) return deps.prep .. ' to ' .. opts.channel end,
           },
           release = {
             depends = { 'prep' },
             run = function(opts, deps)
               local a = task.run('publish', { channel = 'stable' })
               local b = task.run('notes', { args = { '7' } })
               return a .. '; ' .. b
             end,
           },
           typo = function() return task.run('publish', { chanel = 'x' }) end,
         }",
    );
    s.trust();
    assert_eq!(
        s.ok(&["release"]),
        "prepared 1 to stable; notes 7\n",
        "prep ran once, as release's dependency"
    );
    let names: Vec<String> = s.last_run()["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["prep", "publish", "notes", "release"]);
    let o = s.timon(&["typo"]);
    assert!(
        err(&o).contains("unknown option \"chanel\" to task.run(\"publish\")"),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("did you mean \"channel\"?"), "{}", err(&o));
    let o = s.timon(&["eval", "task.run('prep')"]);
    assert!(
        err(&o).contains("task.run works inside a task's run"),
        "{}",
        err(&o)
    );
}

#[test]
fn a_dependency_task_run_has_already_run_does_not_run_again() {
    let s = Sandbox::new();
    s.project_tasks(
        "local n = 0
         return {
           prep = function() sleep('200ms'); n = n + 1; return 'prepared' end,
           extra = { depends = { 'prep' }, run = function() end },
           other = { depends = { 'prep' }, run = function() end },
           first = function() task.run('extra') end,
           all = { depends = { 'first', 'prep' }, run = function(opts, deps) return n .. ' ' .. deps.prep end },
           left = function() task.run('extra') end,
           right = function() task.run('other') end,
           both = { depends = { 'left', 'right', 'prep' }, run = function() return n end },
           again = { depends = { 'prep' }, run = function() task.run('prep'); return n end },
         }",
    );
    s.trust();
    assert_eq!(
        s.ok(&["all"]),
        "1 prepared\n",
        "the nested run completed the queued prep"
    );
    let names: Vec<String> = s.last_run()["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["prep", "extra", "first", "all"]);
    assert_eq!(
        s.ok(&["--jobs", "3", "both"]),
        "1\n",
        "steps sharing prep at once wait for the one running it"
    );
    assert_eq!(s.ok(&["again"]), "2\n", "a task task.run names still runs");
}

#[test]
fn command_tasks_take_words_not_shell_text() {
    let s = Sandbox::new();
    s.project_tasks("return { each = 'printf %s.' }");
    s.trust();
    assert_eq!(s.ok(&["each", "--", "a b", "$HOME", "*"]), "a b.$HOME.*.");
    s.project_tasks("return { tee = 'cargo test | tee log' }");
    let o = s.timon(&["tasks"]);
    assert!(
        err(&o).contains("the command of task \"tee\" uses shell syntax (|)"),
        "{}",
        err(&o)
    );
    s.project_tasks(
        "return { a = { options = { id = 'integer' }, run = 'true' }, b = { depends = { 'a' }, run = 'true' } }",
    );
    let o = s.timon(&["b"]);
    assert!(
        err(&o).contains("task \"a\" is a dependency, so it runs with its defaults"),
        "{}",
        err(&o)
    );
}

#[test]
fn scripts_parse_their_words_with_cli_parse() {
    let s = Sandbox::new();
    s.write(
        s.project().join(".timon/scripts/listen.lua"),
        "-- Listen\nlocal opts, rest = cli.parse(arg, { port = 'integer?', verbose = { type = 'boolean', short = 'v' } })\nreturn json.encode({ port = opts.port, verbose = opts.verbose, rest = rest })",
    );
    s.trust();
    assert_eq!(
        s.ok(&["listen", "--port", "9000", "-v", "x"]),
        "{\"port\":9000,\"rest\":[\"x\"],\"verbose\":true}\n"
    );
    let o = s.timon(&["listen", "--prot", "1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("did you mean --port?"), "{}", err(&o));
}
