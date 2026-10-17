//! Budgets: limits on how many agent runs start and what they cost, nested by scope.
//!
//! The root is the invocation's (`--max-runs`, `--max-usd`, or `limits` in `init.lua`). A
//! scope with `budget = { runs, usd }` adds a child: a run inside it must fit every budget up
//! to the root, and counts against each. With `reserve = true` a scope takes its runs from its
//! parent when it starts, so a fan-out that cannot finish fails before its first run; the runs
//! it does not use go back when it ends.
//!
//! Cost is known only after a run, so a Claude run under a USD limit holds what it may spend
//! from the moment it starts: its own `budget.usd`, else all that is left and not held by
//! another run. Its `--max-budget-usd` is that hold. When it ends it is charged what it
//! reported, and the rest of the hold is freed; a cancelled run frees it all. A run that finds
//! everything left held waits for a hold to come back, so concurrent runs are never given the
//! same money twice. Claude checks its cap after each API call, so a run can still spend a
//! little past its hold; the next run sees that overspend. Codex reports no cost, so only
//! `runs` limits it.

use std::cell::Cell;
use std::rc::Rc;

use mlua::{Lua, Table};
use tokio::sync::Notify;

use crate::error::Error;

pub struct Budget {
    /// Whose limit this is, for messages.
    owner: Owner,
    max_runs: Option<u64>,
    max_usd: Option<f64>,
    runs: Cell<u64>,
    usd: Cell<f64>,
    /// Held by Claude runs that have started and not yet ended.
    held: Cell<f64>,
    /// Told when a hold anywhere under the root comes back.
    freed: Rc<Notify>,
    /// Took its runs from the parent up front; the parent already counts them.
    reserves: bool,
    parent: Option<Rc<Budget>>,
}

#[derive(Clone, Copy)]
enum Owner {
    Invocation,
    Scope,
}

impl Owner {
    fn name(self) -> &'static str {
        match self {
            Owner::Invocation => "this invocation",
            Owner::Scope => "the scope's budget",
        }
    }

    fn runs_hint(self) -> &'static str {
        match self {
            Owner::Invocation => "raise --max-runs or limits.max_runs",
            Owner::Scope => "raise budget.runs on the scope",
        }
    }

    fn usd_hint(self) -> &'static str {
        match self {
            Owner::Invocation => {
                "raise --max-usd or limits.max_usd; Codex runs report no cost and are not counted"
            }
            Owner::Scope => "raise budget.usd on the scope; Codex runs report no cost",
        }
    }
}

impl Budget {
    /// The invocation's budget.
    pub fn root(max_runs: Option<u64>, max_usd: Option<f64>) -> Rc<Self> {
        Rc::new(Self {
            owner: Owner::Invocation,
            max_runs,
            max_usd,
            runs: Cell::new(0),
            usd: Cell::new(0.0),
            held: Cell::new(0.0),
            freed: Rc::new(Notify::new()),
            reserves: false,
            parent: None,
        })
    }

    /// A scope's budget inside `parent`. With `reserve`, its runs are taken from `parent` now,
    /// and raise `budget_exceeded` if `parent` does not have them.
    pub fn child(
        parent: &Rc<Budget>,
        max_runs: Option<u64>,
        max_usd: Option<f64>,
        reserve: bool,
    ) -> Result<Rc<Self>, Error> {
        if reserve {
            let Some(n) = max_runs else {
                return Err(Error::new("bad_option", "reserve = true needs budget.runs")
                    .with_hint("reserve takes the scope's runs from the enclosing budget"));
            };
            if let Some((owner, left)) = parent.runs_left()
                && left < n
            {
                return Err(Error::new(
                    "budget_exceeded",
                    format!(
                        "the scope reserves {n} runs, and {} has {left} left",
                        owner.name()
                    ),
                )
                .with_hint(owner.runs_hint()));
            }
            parent.charge_runs(n as i64);
        }
        Ok(Rc::new(Self {
            owner: Owner::Scope,
            max_runs,
            max_usd,
            runs: Cell::new(0),
            usd: Cell::new(0.0),
            held: Cell::new(0.0),
            freed: parent.freed.clone(),
            reserves: reserve,
            parent: Some(parent.clone()),
        }))
    }

    /// The budgets whose run counts a run here changes: this one and its parents, up to and
    /// including the first that reserved its runs.
    fn run_chain(&self) -> impl Iterator<Item = &Budget> {
        let mut next = Some(self);
        let mut done = false;
        std::iter::from_fn(move || {
            if done {
                return None;
            }
            let b = next?;
            if b.reserves {
                done = true;
            }
            next = b.parent.as_deref();
            Some(b)
        })
    }

    fn all(&self) -> impl Iterator<Item = &Budget> {
        let mut next = Some(self);
        std::iter::from_fn(move || {
            let b = next?;
            next = b.parent.as_deref();
            Some(b)
        })
    }

    /// The fewest runs any budget in the run chain has left, and whose.
    fn runs_left(&self) -> Option<(Owner, u64)> {
        self.run_chain()
            .filter_map(|b| {
                b.max_runs
                    .map(|max| (b.owner, max.saturating_sub(b.runs.get())))
            })
            .min_by_key(|(_, left)| *left)
    }

    fn charge_runs(&self, n: i64) {
        for b in self.run_chain() {
            b.runs.set((b.runs.get() as i64 + n).max(0) as u64);
        }
    }

    /// Admits a run that is about to start, or raises `budget_exceeded` if any budget up to
    /// the root has no runs or money left. A Claude run (`claude`) holds what it may spend:
    /// `own`, capped at the least any budget has left and not held; while that is nothing, it
    /// waits for a hold to come back.
    pub async fn admit(self: &Rc<Self>, claude: bool, own: Option<f64>) -> Result<Hold, Error> {
        loop {
            self.check()?;
            let free = self.usd_free();
            if claude && free.is_some_and(|free| free <= 0.0) {
                self.freed.notified().await;
                continue;
            }
            self.charge_runs(1);
            let usd = free.map(|free| own.map_or(free, |own| own.min(free)));
            let hold = Hold {
                budget: self.clone(),
                usd: if claude { usd.unwrap_or(0.0) } else { 0.0 },
                limit: usd,
            };
            for b in self.all() {
                b.held.set(b.held.get() + hold.usd);
            }
            return Ok(hold);
        }
    }

    /// Counts a run that is about to start, or raises `budget_exceeded` if any budget up to
    /// the root has no runs or money left.
    #[cfg(test)]
    fn start_run(&self) -> Result<(), Error> {
        self.check()?;
        self.charge_runs(1);
        Ok(())
    }

    /// Raises `budget_exceeded` if any budget up to the root has no runs or money left.
    fn check(&self) -> Result<(), Error> {
        for b in self.run_chain() {
            if let Some(max) = b.max_runs
                && b.runs.get() >= max
            {
                return Err(Error::new(
                    "budget_exceeded",
                    format!(
                        "{} has started {} runs; the limit is {max}",
                        b.owner.name(),
                        b.runs.get()
                    ),
                )
                .with_hint(b.owner.runs_hint()));
            }
        }
        for b in self.all() {
            if let Some(max) = b.max_usd
                && b.usd.get() >= max
            {
                return Err(Error::new(
                    "budget_exceeded",
                    format!(
                        "{} has spent ${:.4}; the limit is ${max}",
                        b.owner.name(),
                        b.usd.get()
                    ),
                )
                .with_hint(b.owner.usd_hint()));
            }
        }
        Ok(())
    }

    /// Adds a finished run's cost to every budget up to the root.
    fn spent(&self, usd: f64) {
        for b in self.all() {
            b.usd.set(b.usd.get() + usd);
        }
    }

    /// The least money any budget up to the root has left and not held, if any sets a limit.
    fn usd_free(&self) -> Option<f64> {
        self.all()
            .filter_map(|b| {
                b.max_usd
                    .map(|max| (max - b.usd.get() - b.held.get()).max(0.0))
            })
            .reduce(f64::min)
    }

    /// Gives a reserving scope's unused runs back to its parent, once the scope has ended.
    pub fn release(&self) {
        if !self.reserves {
            return;
        }
        if let (Some(parent), Some(max)) = (&self.parent, self.max_runs) {
            let unused = max.saturating_sub(self.runs.get());
            parent.charge_runs(-(unused as i64));
        }
    }
}

/// A started run's place in its budgets: the money it holds until it ends.
pub struct Hold {
    budget: Rc<Budget>,
    usd: f64,
    limit: Option<f64>,
}

impl Hold {
    /// The most the run may spend, when any budget up to the root sets a USD limit.
    pub fn usd(&self) -> Option<f64> {
        self.limit
    }

    /// Ends the run: charges what it reported, and frees the hold.
    pub fn settle(self, usd: f64) {
        self.budget.spent(usd);
    }
}

impl Drop for Hold {
    /// Frees the hold. A run dropped without settling (cancelled) is charged nothing, as its
    /// cost is unknown.
    fn drop(&mut self) {
        if self.usd == 0.0 {
            return;
        }
        for b in self.budget.all() {
            b.held.set((b.held.get() - self.usd).max(0.0));
        }
        self.budget.freed.notify_waiters();
    }
}

/// The invocation's budget, made on first use from `--max-runs` and `--max-usd`, then
/// `limits.max_runs` and `limits.max_usd` in `init.lua`.
pub(crate) fn invocation(lua: &Lua) -> mlua::Result<Rc<Budget>> {
    if let Some(b) = lua.app_data_ref::<Rc<Budget>>() {
        return Ok(b.clone());
    }
    let (cli_runs, cli_usd) = match lua.app_data_ref::<crate::modules::agent::Overrides>() {
        Some(o) => (o.max_runs, o.max_usd),
        None => (None, None),
    };
    let limits = lua
        .named_registry_value::<Option<Table>>(crate::runtime::SETTINGS)?
        .and_then(|s| s.raw_get::<Option<Table>>("limits").ok().flatten());
    let limit =
        |key: &str| -> Option<f64> { limits.as_ref()?.raw_get::<Option<f64>>(key).ok().flatten() };
    let budget = Budget::root(
        cli_runs.or(limit("max_runs").map(|n| n.max(0.0) as u64)),
        cli_usd.or(limit("max_usd")),
    );
    lua.set_app_data(budget.clone());
    Ok(budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_counts_against_every_budget_up_to_the_root() {
        let root = Budget::root(Some(3), None);
        let scope = Budget::child(&root, Some(2), None, false).unwrap();
        scope.start_run().unwrap();
        scope.start_run().unwrap();
        let err = scope.start_run().unwrap_err();
        assert!(
            err.message
                .starts_with("the scope's budget has started 2 runs"),
            "{}",
            err.message
        );
        root.start_run().unwrap();
        assert_eq!(
            root.start_run().unwrap_err().message,
            "this invocation has started 3 runs; the limit is 3"
        );
    }

    #[test]
    fn reserving_takes_runs_up_front_and_gives_back_the_rest() {
        let root = Budget::root(Some(3), None);
        assert_eq!(
            Budget::child(&root, Some(5), None, true)
                .err()
                .unwrap()
                .message,
            "the scope reserves 5 runs, and this invocation has 3 left"
        );
        let scope = Budget::child(&root, Some(2), None, true).unwrap();
        assert_eq!(root.runs.get(), 2);
        scope.start_run().unwrap();
        assert_eq!(root.runs.get(), 2, "reserved runs are counted once");
        scope.release();
        assert_eq!(root.runs.get(), 1, "the unused run went back");
    }

    #[tokio::test]
    async fn money_is_checked_everywhere_and_the_least_left_caps_a_run() {
        let root = Budget::root(None, Some(1.0));
        let scope = Budget::child(&root, None, Some(0.02), false).unwrap();
        let run = scope.admit(true, None).await.unwrap();
        assert_eq!(run.usd(), Some(0.02));
        run.settle(0.015);
        assert!((scope.usd_free().unwrap() - 0.005).abs() < 1e-9);
        assert!((root.usd.get() - 0.015).abs() < 1e-9);
        scope.admit(true, None).await.unwrap().settle(0.01);
        assert!(
            scope
                .admit(true, None)
                .await
                .err()
                .unwrap()
                .message
                .starts_with("the scope's budget has spent")
        );
    }

    #[tokio::test]
    async fn a_run_waits_for_held_money_and_gets_what_was_not_spent() {
        let root = Budget::root(None, Some(0.02));
        let first = root.admit(true, None).await.unwrap();
        assert_eq!(first.usd(), Some(0.02));
        let (second, ()) = tokio::join!(root.admit(true, None), async {
            tokio::task::yield_now().await;
            first.settle(0.005);
        });
        let second = second.unwrap();
        assert!((second.usd().unwrap() - 0.015).abs() < 1e-9);
        assert_eq!(root.runs.get(), 2);
    }

    #[tokio::test]
    async fn holds_count_in_every_enclosing_budget() {
        let root = Budget::root(None, Some(1.0));
        let scope = Budget::child(&root, None, Some(0.02), false).unwrap();
        let inside = scope.admit(true, None).await.unwrap();
        let capped = root.admit(true, Some(0.1)).await.unwrap();
        assert_eq!(capped.usd(), Some(0.1));
        let outside = root.admit(true, None).await.unwrap();
        assert!((outside.usd().unwrap() - 0.88).abs() < 1e-9);
        drop((inside, capped, outside));
        assert_eq!(root.held.get(), 0.0);
        assert_eq!(scope.held.get(), 0.0);
    }

    #[tokio::test]
    async fn a_cancelled_run_frees_its_hold_and_is_charged_nothing() {
        let root = Budget::root(None, Some(0.02));
        drop(root.admit(true, None).await.unwrap());
        assert_eq!(root.usd.get(), 0.0);
        assert_eq!(root.admit(true, None).await.unwrap().usd(), Some(0.02));
    }

    #[tokio::test]
    async fn codex_runs_hold_nothing_and_never_wait() {
        let root = Budget::root(None, Some(0.02));
        let claude = root.admit(true, None).await.unwrap();
        let codex = root.admit(false, None).await.unwrap();
        assert_eq!(root.held.get(), 0.02);
        drop((claude, codex));
        assert_eq!(root.held.get(), 0.0);
        assert_eq!(
            Budget::root(Some(1), None)
                .admit(true, Some(0.5))
                .await
                .unwrap()
                .usd(),
            None,
            "without a USD limit the run keeps its own cap"
        );
    }
}
