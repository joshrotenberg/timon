//! Concurrency tests prove overlap with readiness (one task waits for a flag another sets), not
//! with elapsed time. Every test runs under a timeout so a hang fails instead of stalling CI.

use std::time::Duration;

use timon::{Failure, Runtime};

/// Runs each chunk in turn in one runtime and returns the values of the last.
fn run(chunks: &[&str]) -> Result<Vec<String>, Failure> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let runtime = Runtime::new().unwrap();
        let work = async {
            let mut last = Vec::new();
            for chunk in chunks {
                let values = runtime.eval(chunk, "=eval").await?;
                last = values
                    .iter()
                    .map(|v| timon::value::render(v, false).unwrap())
                    .collect();
            }
            Ok(last)
        };
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .expect("timed out: a task did not finish or was not cancelled")
    })
}

fn eval(code: &str) -> Vec<String> {
    run(&[code]).unwrap_or_else(|f| panic!("{code}\n{f}"))
}

fn fails(code: &str) -> timon::Error {
    run(&[code]).expect_err(code).error
}

#[test]
fn tasks_overlap() {
    let out = eval(
        r#"
        local ready = false
        local a = spawn(function()
          while not ready do sleep(0.001) end
          return "a saw b"
        end)
        local b = spawn(function() ready = true; return "b" end)
        return a:await(), b:await()
        "#,
    );
    assert_eq!(out, ["a saw b", "b"]);
}

#[test]
fn scope_returns_the_body_values_after_its_tasks() {
    let out = eval(
        r#"
        local log = {}
        local v = scope(function(s)
          s:spawn(function() sleep(0.01); log[#log + 1] = "task" end)
          log[#log + 1] = "body"
          return "value"
        end)
        log[#log + 1] = "after"
        return v, table.concat(log, ",")
        "#,
    );
    assert_eq!(out, ["value", "body,task,after"]);
}

#[test]
fn a_failing_task_cancels_its_siblings_and_raises() {
    let out = eval(
        r#"
        local log = {}
        local ok, err = try(scope, function(s)
          s:spawn(function() sleep(10); log[#log + 1] = "slow finished" end)
          s:spawn(function() sleep(0.01); error({ kind = "boom", message = "child failed" }) end)
          sleep(10)
          log[#log + 1] = "body finished"
        end)
        return ok, err.kind, err.message, #log
        "#,
    );
    assert_eq!(out, ["false", "boom", "child failed", "0"]);
}

#[test]
fn a_failing_body_cancels_its_tasks() {
    let out = eval(
        r#"
        local finished = false
        local ok, err = try(scope, function(s)
          s:spawn(function() sleep(10); finished = true end)
          error({ kind = "body_failed", message = "x" })
        end)
        return ok, err.kind, finished
        "#,
    );
    assert_eq!(out, ["false", "body_failed", "false"]);
}

#[test]
fn cancellation_closes_to_be_closed_variables() {
    let out = eval(
        r#"
        local closed = false
        local h
        scope(function(s)
          h = s:spawn(function()
            local guard <close> = setmetatable({}, { __close = function() closed = true end })
            sleep(10)
          end)
          sleep(0.01)
          h:cancel()
        end)
        return closed, h:done()
        "#,
    );
    assert_eq!(out, ["true", "true"]);
}

#[test]
fn await_raises_the_task_error_table() {
    let err = fails(
        r#"
        local h = spawn(error, { kind = "task_kind", message = "from the task", hint = "h" })
        h:await()
        "#,
    );
    assert_eq!(err.kind, "task_kind");
    assert_eq!(err.hint.as_deref(), Some("h"));
}

#[test]
fn awaiting_a_cancelled_task_raises_cancelled() {
    let out = eval(
        r#"
        local h = spawn(sleep, 10)
        h:cancel()
        local ok, err = try(h.await, h)
        return ok, err.kind
        "#,
    );
    assert_eq!(out, ["false", "cancelled"]);
}

#[test]
fn a_script_waits_for_tasks_it_did_not_await() {
    let out = run(&[
        "finished = false\nspawn(function() sleep(0.01); finished = true end)",
        "return finished",
    ])
    .unwrap();
    assert_eq!(out, ["true"]);
}

#[test]
fn a_task_failure_fails_the_script() {
    let err = fails("spawn(function() error({ kind = 'child', message = 'm' }) end)\nsleep(10)");
    assert_eq!(err.kind, "child");
}

#[test]
fn spawning_on_an_ended_scope_is_an_error() {
    let err = fails("local saved\nscope(function(s) saved = s end)\nsaved:spawn(print)");
    assert_eq!(err.kind, "scope_ended");
}

#[test]
fn the_close_form_waits_for_its_tasks() {
    let out = eval(
        r#"
        local results = {}
        do
          local s <close> = scope()
          s:spawn(function() sleep(0.01); results[1] = "done" end)
        end
        return results[1]
        "#,
    );
    assert_eq!(out, ["done"]);
}

#[test]
fn par_map_keeps_order_and_respects_limit() {
    let out = eval(
        r#"
        local active, most = 0, 0
        local out = par.map({ 5, 4, 3, 2, 1 }, function(n)
          active = active + 1
          most = math.max(most, active)
          sleep(n / 1000)
          active = active - 1
          return n * 10
        end, { limit = 2 })
        return out, most
        "#,
    );
    assert_eq!(out, ["[50,40,30,20,10]", "2"]);
}

#[test]
fn par_map_settle_collects_failures() {
    let out = eval(
        r#"
        local out = par.map({ 1, 2, 3 }, function(n)
          if n == 2 then error({ kind = "bad", message = "two" }) end
          return n
        end, { settle = true })
        return out[1].value, out[2].ok, out[2].error.kind, out[3].value
        "#,
    );
    assert_eq!(out, ["1", "false", "bad", "3"]);
}

#[test]
fn par_map_without_settle_raises_the_first_failure() {
    let err = fails(
        "par.map({ 1, 2 }, function(n) if n == 2 then error({ kind = 'bad', message = 'x' }) end; sleep(10) end)",
    );
    assert_eq!(err.kind, "bad");
}

#[test]
fn par_map_checks_its_options() {
    let err = fails("par.map({}, print, { limt = 2 })");
    assert_eq!(err.hint.as_deref(), Some("did you mean \"limit\"?"));
}

#[test]
fn par_all_returns_results_in_argument_order() {
    let out =
        eval("return par.all(function() sleep(0.02); return 'a' end, function() return 'b' end)");
    assert_eq!(out, ["a", "b"]);
}

#[test]
fn par_race_returns_the_first_and_cancels_the_rest() {
    let out = eval(
        r#"
        local slow_finished = false
        local v, i = par.race(
          function() sleep(10); slow_finished = true; return "slow" end,
          function() sleep(0.01); return "fast" end
        )
        return v, i, slow_finished
        "#,
    );
    assert_eq!(out, ["fast", "2", "false"]);
}

#[test]
fn sleep_rejects_bad_durations() {
    assert_eq!(fails("sleep('soon')").kind, "bad_argument");
}
