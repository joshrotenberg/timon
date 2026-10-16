//! Budgets: limits on how many agent runs start and what they cost, nested by scope.
//!
//! The root is the invocation's (`--max-runs`, `--max-usd`, or `limits` in `init.lua`). A
//! scope with `budget = { runs, usd }` adds a child: a run inside it must fit every budget up
//! to the root, and counts against each. With `reserve = true` a scope takes its runs from its
//! parent when it starts, so a fan-out that cannot finish fails before its first run; the runs
//! it does not use go back when it ends. Cost cannot be reserved, because it is known only
//! after a run. Codex reports no cost, so only `runs` limits it.

use std::cell::Cell;
use std::rc::Rc;

use mlua::{Lua, Table};

use crate::error::Error;

pub struct Budget {
    /// Whose limit this is, for messages.
    owner: Owner,
    max_runs: Option<u64>,
    max_usd: Option<f64>,
    runs: Cell<u64>,
    usd: Cell<f64>,
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

    /// Counts a run that is about to start, or raises `budget_exceeded` if any budget up to
    /// the root has no runs or money left.
    pub fn start_run(&self) -> Result<(), Error> {
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
        self.charge_runs(1);
        Ok(())
    }

    /// Adds a finished run's cost to every budget up to the root.
    pub fn spent(&self, usd: f64) {
        for b in self.all() {
            b.usd.set(b.usd.get() + usd);
        }
    }

    /// The least money any budget up to the root has left, if any sets a limit.
    pub fn usd_left(&self) -> Option<f64> {
        self.all()
            .filter_map(|b| b.max_usd.map(|max| (max - b.usd.get()).max(0.0)))
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

    #[test]
    fn money_is_checked_everywhere_and_the_least_left_caps_a_run() {
        let root = Budget::root(None, Some(1.0));
        let scope = Budget::child(&root, None, Some(0.02), false).unwrap();
        assert_eq!(scope.usd_left(), Some(0.02));
        scope.start_run().unwrap();
        scope.spent(0.015);
        assert!((scope.usd_left().unwrap() - 0.005).abs() < 1e-9);
        assert!((root.usd.get() - 0.015).abs() < 1e-9);
        scope.spent(0.01);
        assert!(
            scope
                .start_run()
                .unwrap_err()
                .message
                .starts_with("the scope's budget has spent")
        );
    }
}
