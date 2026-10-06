use core::fmt::{self, Write as _};

pub trait Gap: Copy + PartialEq + 'static {
    const ALL: &'static [Self];

    fn name(self) -> &'static str;

    fn is_standing(self) -> bool;

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|g| *g == self)
            .expect("Gap::ALL does not list this variant")
    }
}

pub trait GapCause: fmt::Display + PartialEq {
    type Gap: Gap;

    fn gap(&self) -> Self::Gap;
}

enum GapState<C> {
    Absent,

    Standing,

    Raised(C),
}

pub struct GapLedger<C: GapCause, const N: usize> {
    live: [GapState<C>; N],

    hits: [u64; N],

    revision: u64,
}

impl<C: GapCause, const N: usize> Default for GapLedger<C, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: GapCause, const N: usize> GapLedger<C, N> {
    pub fn new() -> Self {
        const {
            assert!(
                N == C::Gap::ALL.len(),
                "GapLedger width must be Gap::ALL.len()"
            );
        }
        Self {
            live: core::array::from_fn(|i| {
                if C::Gap::ALL[i].is_standing() {
                    GapState::Standing
                } else {
                    GapState::Absent
                }
            }),
            hits: [0; N],
            revision: 0,
        }
    }

    pub fn raise(&mut self, cause: C) {
        let slot = cause.gap().index();
        self.hits[slot] = self.hits[slot].saturating_add(1);
        if !matches!(&self.live[slot], GapState::Raised(live) if *live == cause) {
            self.live[slot] = GapState::Raised(cause);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn clear(&mut self, gap: C::Gap) {
        if !gap.is_standing() && self.is_live(gap) {
            self.live[gap.index()] = GapState::Absent;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Bumps whenever the live set or a cause changes; cheaper to compare
    /// every frame than `write_signature`.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_live(&self, gap: C::Gap) -> bool {
        !matches!(self.live[gap.index()], GapState::Absent)
    }

    pub fn cause(&self, gap: C::Gap) -> Option<&C> {
        match &self.live[gap.index()] {
            GapState::Raised(cause) => Some(cause),
            GapState::Absent | GapState::Standing => None,
        }
    }

    pub fn hits(&self, gap: C::Gap) -> u64 {
        self.hits[gap.index()]
    }

    pub fn live(&self) -> impl Iterator<Item = C::Gap> + '_ {
        C::Gap::ALL.iter().copied().filter(|g| self.is_live(*g))
    }

    pub fn count(&self) -> usize {
        self.live
            .iter()
            .filter(|s| !matches!(s, GapState::Absent))
            .count()
    }

    pub fn write_signature(&self, out: &mut impl fmt::Write) -> fmt::Result {
        for gap in self.live() {
            out.write_str(gap.name())?;
            if let Some(cause) = self.cause(gap) {
                out.write_char('=')?;
                write!(out, "{cause}")?;
            }
            out.write_char(' ')?;
        }
        Ok(())
    }
}

impl<C: GapCause, const N: usize> fmt::Debug for GapLedger<C, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GapLedger[")?;
        self.write_signature(f)?;
        f.write_char(']')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum G {
        A,
    }

    impl Gap for G {
        const ALL: &'static [Self] = &[G::A];
        fn name(self) -> &'static str {
            "a"
        }
        fn is_standing(self) -> bool {
            false
        }
    }

    #[derive(PartialEq)]
    struct C(u8);

    impl fmt::Display for C {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl GapCause for C {
        type Gap = G;
        fn gap(&self) -> G {
            G::A
        }
    }

    #[test]
    fn revision_moves_only_on_state_change() {
        let mut ledger = GapLedger::<C, 1>::new();
        let r0 = ledger.revision();
        ledger.clear(G::A);
        assert_eq!(ledger.revision(), r0);
        ledger.raise(C(1));
        let r1 = ledger.revision();
        assert_ne!(r1, r0);
        ledger.raise(C(1));
        assert_eq!(ledger.revision(), r1);
        assert_eq!(ledger.hits(G::A), 2);
        ledger.raise(C(2));
        let r2 = ledger.revision();
        assert_ne!(r2, r1);
        ledger.clear(G::A);
        assert_ne!(ledger.revision(), r2);
    }
}
