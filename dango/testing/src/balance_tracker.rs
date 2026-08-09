use {
    crate::TestSuite,
    dango_app::{AppError, Db, Indexer, ProposalPreparer, Vm},
    dango_primitives::{Addressable, Denom, Inner},
    std::{
        cmp::Ordering,
        collections::{BTreeMap, BTreeSet},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceChange {
    Increased(u128),
    Decreased(u128),
    Unchanged,
}

pub struct BalanceTracker<'a, DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm,
    PP: ProposalPreparer,
    ID: Indexer,
{
    pub(crate) suite: &'a mut TestSuite<DB, VM, PP, ID>,
}

impl<DB, VM, PP, ID> BalanceTracker<'_, DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    /// Record the current balance of a list of accounts.
    pub fn record_many<'a, I, A>(&mut self, accounts: I)
    where
        I: IntoIterator<Item = &'a A>,
        A: Addressable + 'a,
    {
        let new_balances = accounts
            .into_iter()
            .map(|addr| (addr.address(), self.suite.query_balances(addr).unwrap()))
            // collect is needed to avoid borrowing issues
            .collect::<BTreeMap<_, _>>();

        self.suite.balances.extend(new_balances);
    }

    /// Record the current balance of a single account.
    pub fn record<A>(&mut self, account: &A)
    where
        A: Addressable,
    {
        let account = account.address();
        let coins = self.suite.query_balances(&account).unwrap();
        self.suite.balances.insert(account, coins);
    }

    /// Refresh all recorded balances.
    pub fn refresh_all(&mut self) {
        // Need to collect the addresses first to avoid borrowing issues
        let addresses: Vec<_> = self.suite.balances.keys().cloned().collect();
        for addr in addresses {
            let coins = self.suite.query_balances(&addr).unwrap();
            self.suite.balances.insert(addr, coins);
        }
    }

    /// Refresh the balance of a single account.
    pub fn refresh<A>(&mut self, account: &A)
    where
        A: Addressable,
    {
        let account = account.address();
        let coins = self.suite.query_balances(&account).unwrap();
        self.suite.balances.insert(account, coins);
    }

    /// Clear all recorded balances.
    pub fn clear(&mut self) {
        self.suite.balances.clear();
    }

    /// Get the changes in balances of an account since the last recorded balances.
    pub fn changes<A>(&self, account: &A) -> BTreeMap<Denom, BalanceChange>
    where
        A: Addressable,
    {
        let account = account.address();
        let old_balances = self.suite.balances.get(&account).unwrap();
        let new_balances = self.suite.query_balances(&account).unwrap();

        old_balances
            .into_iter()
            .chain(&new_balances)
            .map(|coin| coin.denom)
            // Take denoms only once
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|denom| {
                let old_balance = old_balances.amount_of(denom);
                let new_balance = new_balances.amount_of(denom);
                let change = match new_balance.cmp(&old_balance) {
                    Ordering::Greater => BalanceChange::Increased(
                        (new_balance - old_balance).into_inner(),
                    ),
                    Ordering::Less => BalanceChange::Decreased(
                        (old_balance - new_balance).into_inner(),
                    ),
                    Ordering::Equal => BalanceChange::Unchanged,
                };

                (denom.clone(), change)
            })
            .collect()
    }

    /// Assert a list of balance changes for an account.
    pub fn should_change<A>(&self, account: &A, changes: BTreeMap<Denom, BalanceChange>)
    where
        A: Addressable,
    {
        let delta = self.changes(account);

        for (denom, change) in changes {
            // A denom absent from both snapshots (zero balance before and
            // after) has no delta entry; that is an unchanged balance, not
            // an error.
            let diff = delta.get(&denom).unwrap_or(&BalanceChange::Unchanged);
            if change != *diff {
                panic!(
                    "incorrect balance! account: {}, denom: {}, expected: {:?}, actual: {:?}",
                    account.address(),
                    denom,
                    change,
                    diff
                );
            }
        }
    }
}
