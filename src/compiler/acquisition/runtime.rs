//! Deterministic command-local scheduling for compiler-acquisition views.

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap};
use std::num::NonZeroUsize;

use crate::compiler::scheduler::{CandidateIx, ViewIx};
use crate::error::{RailError, RailResult};

/// Resident sandbox generations: one per compatibility class a command alternates between.
const MAX_RESIDENT_SANDBOXES: usize = 4;

/// One Cargo process at a time; Cargo owns job parallelism inside it.
///
/// Views of one compatibility class run in turn in one sandbox, so each compiled dependency
/// unit is reused by Cargo's own freshness check instead of being rebuilt in a parallel
/// sandbox. Cargo applies `build.jobs`, `CARGO_BUILD_JOBS`, and an inherited jobserver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExecutionPolicy {
    sandbox_count: NonZeroUsize,
}

impl ExecutionPolicy {
    pub(crate) fn derive(view_count: usize) -> Self {
        Self {
            sandbox_count: NonZeroUsize::new(view_count.clamp(1, MAX_RESIDENT_SANDBOXES))
                .expect("sandbox count is clamped to one"),
        }
    }

    pub(crate) const fn process_slots(self) -> usize {
        1
    }

    pub(crate) const fn sandbox_count(self) -> usize {
        self.sandbox_count.get()
    }

    pub(crate) const fn journal_batch(self) -> usize {
        1
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeViewSpec {
    view: ViewIx,
    ordinal: usize,
    required: bool,
    candidates: Box<[usize]>,
}

impl RuntimeViewSpec {
    pub(crate) fn new(
        view: ViewIx,
        ordinal: usize,
        required: bool,
        candidates: impl IntoIterator<Item = CandidateIx>,
    ) -> Self {
        let candidates = candidates
            .into_iter()
            .map(CandidateIx::offset)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            view,
            ordinal,
            required,
            candidates,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewState {
    Pending,
    Ready,
    Running,
    AwaitingIntegration,
    Complete,
    Skipped,
    Failed,
    Cancelled,
}

impl ViewState {
    const fn terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Skipped | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
struct RuntimeView {
    spec: RuntimeViewSpec,
    state: ViewState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyView {
    required: bool,
    ordinal: usize,
    view: ViewIx,
}

impl Ord for ReadyView {
    fn cmp(&self, other: &Self) -> Ordering {
        self.required
            .cmp(&other.required)
            .then_with(|| other.ordinal.cmp(&self.ordinal))
            .then_with(|| other.view.cmp(&self.view))
    }
}

impl PartialOrd for ReadyView {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Single-writer process-slot and admission state.
pub(crate) struct RuntimeState {
    policy: ExecutionPolicy,
    views: Vec<Option<RuntimeView>>,
    ready: BinaryHeap<ReadyView>,
    running: usize,
    cancelled: bool,
}

impl RuntimeState {
    pub(crate) fn new(
        policy: ExecutionPolicy,
        plan_view_count: usize,
        specs: impl IntoIterator<Item = RuntimeViewSpec>,
    ) -> RailResult<Self> {
        let mut views = vec![None; plan_view_count];
        for spec in specs {
            let offset = spec.view.offset();
            let slot = views
                .get_mut(offset)
                .ok_or_else(|| RailError::message("compiler acquisition runtime view is outside its plan"))?;
            if slot.is_some() {
                return Err(RailError::message("compiler acquisition runtime view is duplicated"));
            }
            *slot = Some(RuntimeView {
                spec,
                state: ViewState::Pending,
            });
        }
        let state = Self {
            policy,
            views,
            ready: BinaryHeap::new(),
            running: 0,
            cancelled: false,
        };
        state.validate()?;
        Ok(state)
    }

    /// Admit required work and only the earliest still-live overlapping conditional frontier.
    pub(crate) fn refresh(&mut self, mut applicable: impl FnMut(ViewIx) -> bool) -> RailResult<Vec<ViewIx>> {
        if self.cancelled {
            return Ok(Vec::new());
        }
        let mut skipped = Vec::new();
        loop {
            let mut changed = false;
            let pending = self
                .views
                .iter()
                .flatten()
                .filter(|view| view.state == ViewState::Pending)
                .map(|view| view.spec.view)
                .collect::<Vec<_>>();
            for index in pending {
                let offset = index.offset();
                let view = self.views[offset]
                    .as_ref()
                    .ok_or_else(|| RailError::message("compiler acquisition runtime view disappeared"))?;
                if !view.spec.required && !applicable(index) {
                    self.views[offset].as_mut().expect("runtime view exists").state = ViewState::Skipped;
                    skipped.push(index);
                    changed = true;
                    continue;
                }
                if !view.spec.required && self.blocked_by_earlier_authority(offset)? {
                    continue;
                }
                let ready = ReadyView {
                    required: view.spec.required,
                    ordinal: view.spec.ordinal,
                    view: index,
                };
                self.views[offset].as_mut().expect("runtime view exists").state = ViewState::Ready;
                self.ready.push(ready);
                changed = true;
            }
            if !changed {
                break;
            }
        }
        self.validate()?;
        Ok(skipped)
    }

    fn blocked_by_earlier_authority(&self, offset: usize) -> RailResult<bool> {
        let current = self.views[offset]
            .as_ref()
            .ok_or_else(|| RailError::message("compiler acquisition conditional view disappeared"))?;
        Ok(self.views.iter().flatten().any(|other| {
            !other.state.terminal()
                && other.spec.view != current.spec.view
                && (other.spec.required || other.spec.ordinal < current.spec.ordinal)
                && candidates_overlap(&current.spec.candidates, &other.spec.candidates)
        }))
    }

    pub(crate) fn start_next(&mut self) -> RailResult<Option<ViewIx>> {
        if self.cancelled || self.running >= self.policy.process_slots() {
            return Ok(None);
        }
        while let Some(ready) = self.ready.pop() {
            let Some(view) = self.views.get_mut(ready.view.offset()).and_then(Option::as_mut) else {
                return Err(RailError::message("compiler acquisition ready view disappeared"));
            };
            if view.state != ViewState::Ready {
                continue;
            }
            view.state = ViewState::Running;
            self.running += 1;
            self.validate()?;
            return Ok(Some(ready.view));
        }
        Ok(None)
    }

    pub(crate) fn complete(&mut self, view: ViewIx) -> RailResult<()> {
        let runtime = self
            .views
            .get_mut(view.offset())
            .and_then(Option::as_mut)
            .ok_or_else(|| RailError::message("compiler acquisition completed view is outside its runtime"))?;
        if runtime.state != ViewState::AwaitingIntegration {
            return Err(RailError::message(
                "compiler acquisition completed view was not awaiting integration",
            ));
        }
        runtime.state = ViewState::Complete;
        self.validate()
    }

    pub(crate) fn executed(&mut self, view: ViewIx) -> RailResult<()> {
        self.finish_running(view, ViewState::AwaitingIntegration)
    }

    pub(crate) fn fail(&mut self, view: ViewIx) -> RailResult<()> {
        self.finish_running(view, ViewState::Failed)?;
        self.cancel_after_failure();
        self.validate()
    }

    pub(crate) fn fail_integration(&mut self, view: ViewIx) -> RailResult<()> {
        let runtime = self
            .views
            .get_mut(view.offset())
            .and_then(Option::as_mut)
            .ok_or_else(|| RailError::message("compiler acquisition failed view is outside its runtime"))?;
        if runtime.state != ViewState::AwaitingIntegration {
            return Err(RailError::message(
                "compiler acquisition integration failure had no completed process",
            ));
        }
        runtime.state = ViewState::Failed;
        self.cancel_after_failure();
        self.validate()
    }

    fn cancel_after_failure(&mut self) {
        self.cancelled = true;
        for view in self.views.iter_mut().flatten() {
            if matches!(
                view.state,
                ViewState::Pending | ViewState::Ready | ViewState::AwaitingIntegration
            ) {
                view.state = ViewState::Cancelled;
            }
        }
        self.ready.clear();
    }

    pub(crate) fn discard_running(&mut self, view: ViewIx) -> RailResult<()> {
        self.finish_running(view, ViewState::Cancelled)
    }

    fn finish_running(&mut self, view: ViewIx, terminal: ViewState) -> RailResult<()> {
        let runtime = self
            .views
            .get_mut(view.offset())
            .and_then(Option::as_mut)
            .ok_or_else(|| RailError::message("compiler acquisition terminal view is outside its runtime"))?;
        if runtime.state != ViewState::Running || !(terminal.terminal() || terminal == ViewState::AwaitingIntegration) {
            return Err(RailError::message(
                "compiler acquisition terminal transition did not own a process slot",
            ));
        }
        runtime.state = terminal;
        self.running = self
            .running
            .checked_sub(1)
            .ok_or_else(|| RailError::message("compiler acquisition process-slot ledger underflowed"))?;
        self.validate()
    }

    pub(crate) const fn running(&self) -> usize {
        self.running
    }

    pub(crate) fn all_terminal(&self) -> bool {
        self.views.iter().flatten().all(|view| view.state.terminal())
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled
    }

    fn validate(&self) -> RailResult<()> {
        let running = self
            .views
            .iter()
            .flatten()
            .filter(|view| view.state == ViewState::Running)
            .count();
        if running != self.running || running > self.policy.process_slots() {
            return Err(RailError::message(
                "compiler acquisition process-slot ledger is inconsistent",
            ));
        }
        let ready = self.ready.iter().map(|ready| ready.view).collect::<BTreeSet<_>>();
        if ready.len() != self.ready.len()
            || self
                .views
                .iter()
                .flatten()
                .any(|view| (view.state == ViewState::Ready) != ready.contains(&view.spec.view))
        {
            return Err(RailError::message("compiler acquisition ready heap is inconsistent"));
        }
        Ok(())
    }
}

fn candidates_overlap(left: &[usize], right: &[usize]) -> bool {
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            Ordering::Less => left_index += 1,
            Ordering::Greater => right_index += 1,
            Ordering::Equal => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{ExecutionPolicy, RuntimeState, RuntimeViewSpec};

    #[test]
    fn one_cargo_process_runs_at_a_time_with_bounded_resident_sandboxes() {
        for (views, sandboxes) in [(0, 1), (1, 1), (3, 3), (32, 4)] {
            let policy = ExecutionPolicy::derive(views);
            assert_eq!(policy.process_slots(), 1);
            assert_eq!(policy.sandbox_count(), sandboxes, "{views} views");
            assert_eq!(policy.journal_batch(), 1);
        }
    }

    #[test]
    fn required_views_precede_disjoint_conditionals_and_respect_process_slots() {
        let policy = ExecutionPolicy::derive(3);
        let specs = [
            RuntimeViewSpec::new(
                super::ViewIx::checked(0).unwrap(),
                0,
                false,
                [0].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
            RuntimeViewSpec::new(
                super::ViewIx::checked(1).unwrap(),
                1,
                true,
                [0].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
            RuntimeViewSpec::new(
                super::ViewIx::checked(2).unwrap(),
                2,
                false,
                [1].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
        ];
        let mut runtime = RuntimeState::new(policy, 3, specs).expect("runtime");
        assert!(runtime.refresh(|_| true).expect("admit").is_empty());

        let required = runtime.start_next().expect("start required").expect("required view");
        assert_eq!(required.offset(), 1);
        assert!(runtime.start_next().expect("bounded start").is_none());
        runtime.executed(required).expect("required execution");
        runtime.complete(required).expect("required integration");

        let independent = runtime
            .start_next()
            .expect("start conditional")
            .expect("independent conditional");
        assert_eq!(independent.offset(), 2);
        runtime.executed(independent).expect("conditional execution");
        runtime.complete(independent).expect("conditional integration");

        assert!(runtime.refresh(|_| true).expect("unblock overlap").is_empty());
        let overlapping = runtime.start_next().expect("start overlap").expect("overlap view");
        assert_eq!(overlapping.offset(), 0);
        assert_eq!(runtime.running(), 1);
        runtime.executed(overlapping).expect("execution");
        runtime.complete(overlapping).expect("integration");
        assert!(runtime.all_terminal());
    }

    #[test]
    fn conditional_frontier_is_admitted_once_and_cancelled_when_false() {
        let policy = ExecutionPolicy::derive(1);
        let specs = [
            RuntimeViewSpec::new(
                super::ViewIx::checked(0).unwrap(),
                0,
                false,
                [7].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
            RuntimeViewSpec::new(
                super::ViewIx::checked(1).unwrap(),
                1,
                false,
                [7].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
        ];
        let mut runtime = RuntimeState::new(policy, 2, specs).expect("runtime");
        runtime.refresh(|_| true).expect("first frontier");
        let first = runtime.start_next().expect("start").expect("first view");
        assert_eq!(first.offset(), 0);
        runtime.executed(first).expect("execution");
        runtime.complete(first).expect("integration");

        let skipped = runtime.refresh(|_| false).expect("false frontier");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].offset(), 1);
        assert!(runtime.all_terminal());
        assert!(runtime.start_next().expect("no speculative work").is_none());
    }

    #[test]
    fn failure_cancels_queued_and_integrated_work_without_leaking_slots() {
        let policy = ExecutionPolicy::derive(2);
        let specs = [
            RuntimeViewSpec::new(
                super::ViewIx::checked(0).unwrap(),
                0,
                true,
                [].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
            RuntimeViewSpec::new(
                super::ViewIx::checked(1).unwrap(),
                1,
                true,
                [].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
            RuntimeViewSpec::new(
                super::ViewIx::checked(2).unwrap(),
                2,
                true,
                [].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
            ),
        ];
        let mut runtime = RuntimeState::new(policy, 3, specs).expect("runtime");
        runtime.refresh(|_| true).expect("admit");
        let first = runtime.start_next().expect("first").expect("first view");
        runtime.executed(first).expect("first executed");
        let second = runtime.start_next().expect("second").expect("second view");
        runtime.fail_integration(first).expect("integration failure");
        assert!(runtime.cancelled());
        runtime.discard_running(second).expect("discard sibling");
        assert_eq!(runtime.running(), 0);
        assert!(runtime.all_terminal());
        assert!(runtime.start_next().expect("cancelled start").is_none());
    }

    #[test]
    fn invalid_terminal_transitions_fail_closed() {
        let policy = ExecutionPolicy::derive(1);
        let spec = RuntimeViewSpec::new(
            super::ViewIx::checked(0).unwrap(),
            0,
            true,
            [].into_iter().map(|index| super::CandidateIx::checked(index).unwrap()),
        );
        let mut runtime = RuntimeState::new(policy, 1, [spec]).expect("runtime");
        let view = crate::compiler::scheduler::ViewIx::checked(0).expect("view");
        assert!(runtime.complete(view).is_err());
        assert!(runtime.executed(view).is_err());
        runtime.refresh(|_| true).expect("admit");
        let view = runtime.start_next().expect("start").expect("view");
        runtime.executed(view).expect("execution");
        assert!(runtime.executed(view).is_err());
        runtime.complete(view).expect("integration");
        assert!(runtime.complete(view).is_err());
    }
}
