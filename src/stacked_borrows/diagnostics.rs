use smallvec::SmallVec;
use std::fmt;

use rustc_middle::mir::interpret::{alloc_range, AllocId, AllocRange};
use rustc_span::{Span, SpanData};
use rustc_target::abi::Size;

use crate::helpers::CurrentSpan;
use crate::stacked_borrows::{err_sb_ub, AccessKind, GlobalStateInner, Permission};
use crate::*;

use rustc_middle::mir::interpret::InterpError;

#[derive(Clone, Debug)]
pub struct AllocHistory {
    id: AllocId,
    base: (Item, Span),
    creations: smallvec::SmallVec<[Creation; 1]>,
    invalidations: smallvec::SmallVec<[Invalidation; 1]>,
    protectors: smallvec::SmallVec<[Protection; 1]>,
}

#[derive(Clone, Debug)]
struct Creation {
    retag: RetagOp,
    span: Span,
}

impl Creation {
    fn generate_diagnostic(&self) -> (String, SpanData) {
        let tag = self.retag.new_tag;
        if let Some(perm) = self.retag.permission {
            (
                format!(
                    "{tag:?} was created by a {:?} retag at offsets {:?}",
                    perm, self.retag.range,
                ),
                self.span.data(),
            )
        } else {
            assert!(self.retag.range.size == Size::ZERO);
            (
                format!(
                    "{tag:?} would have been created here, but this is a zero-size retag ({:?}) so the tag in question does not exist anywhere",
                    self.retag.range,
                ),
                self.span.data(),
            )
        }
    }
}

#[derive(Clone, Debug)]
struct Invalidation {
    tag: SbTag,
    range: AllocRange,
    span: Span,
    cause: InvalidationCause,
}

#[derive(Clone, Debug)]
enum InvalidationCause {
    Access(AccessKind),
    Retag(Permission, RetagCause),
}

impl Invalidation {
    fn generate_diagnostic(&self) -> (String, SpanData) {
        (
            format!(
                "{:?} was later invalidated at offsets {:?} by a {}",
                self.tag, self.range, self.cause
            ),
            self.span.data(),
        )
    }
}

impl fmt::Display for InvalidationCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidationCause::Access(kind) => write!(f, "{}", kind),
            InvalidationCause::Retag(perm, kind) =>
                if *kind == RetagCause::FnEntry {
                    write!(f, "{:?} FnEntry retag", perm)
                } else {
                    write!(f, "{:?} retag", perm)
                },
        }
    }
}

#[derive(Clone, Debug)]
struct Protection {
    tag: SbTag,
    span: Span,
}

#[derive(Clone)]
pub struct TagHistory {
    pub created: (String, SpanData),
    pub invalidated: Option<(String, SpanData)>,
    pub protected: Option<(String, SpanData)>,
}

pub struct DiagnosticCxBuilder<'span, 'ecx, 'mir, 'tcx> {
    operation: Operation,
    // 'span cannot be merged with any other lifetime since they appear invariantly, under the
    // mutable ref.
    current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
    threads: &'ecx ThreadManager<'mir, 'tcx>,
}

pub struct DiagnosticCx<'span, 'history, 'ecx, 'mir, 'tcx> {
    operation: Operation,
    // 'span and 'history cannot be merged, since when we call `unbuild` we need
    // to return the exact 'span that was used when calling `build`.
    current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
    threads: &'ecx ThreadManager<'mir, 'tcx>,
    history: &'history mut AllocHistory,
    offset: Size,
}

impl<'span, 'ecx, 'mir, 'tcx> DiagnosticCxBuilder<'span, 'ecx, 'mir, 'tcx> {
    pub fn build<'history>(
        self,
        history: &'history mut AllocHistory,
        offset: Size,
    ) -> DiagnosticCx<'span, 'history, 'ecx, 'mir, 'tcx> {
        DiagnosticCx {
            operation: self.operation,
            current_span: self.current_span,
            threads: self.threads,
            history,
            offset,
        }
    }

    pub fn retag(
        current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
        threads: &'ecx ThreadManager<'mir, 'tcx>,
        cause: RetagCause,
        new_tag: SbTag,
        orig_tag: ProvenanceExtra,
        range: AllocRange,
    ) -> Self {
        let operation =
            Operation::Retag(RetagOp { cause, new_tag, orig_tag, range, permission: None });

        DiagnosticCxBuilder { current_span, threads, operation }
    }

    pub fn read(
        current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
        threads: &'ecx ThreadManager<'mir, 'tcx>,
        tag: ProvenanceExtra,
        range: AllocRange,
    ) -> Self {
        let operation = Operation::Access(AccessOp { kind: AccessKind::Read, tag, range });
        DiagnosticCxBuilder { current_span, threads, operation }
    }

    pub fn write(
        current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
        threads: &'ecx ThreadManager<'mir, 'tcx>,
        tag: ProvenanceExtra,
        range: AllocRange,
    ) -> Self {
        let operation = Operation::Access(AccessOp { kind: AccessKind::Write, tag, range });
        DiagnosticCxBuilder { current_span, threads, operation }
    }

    pub fn dealloc(
        current_span: &'span mut CurrentSpan<'ecx, 'mir, 'tcx>,
        threads: &'ecx ThreadManager<'mir, 'tcx>,
        tag: ProvenanceExtra,
    ) -> Self {
        let operation = Operation::Dealloc(DeallocOp { tag });
        DiagnosticCxBuilder { current_span, threads, operation }
    }
}

impl<'span, 'history, 'ecx, 'mir, 'tcx> DiagnosticCx<'span, 'history, 'ecx, 'mir, 'tcx> {
    pub fn unbuild(self) -> DiagnosticCxBuilder<'span, 'ecx, 'mir, 'tcx> {
        DiagnosticCxBuilder {
            operation: self.operation,
            current_span: self.current_span,
            threads: self.threads,
        }
    }
}

#[derive(Debug, Clone)]
enum Operation {
    Retag(RetagOp),
    Access(AccessOp),
    Dealloc(DeallocOp),
}

#[derive(Debug, Clone)]
struct RetagOp {
    cause: RetagCause,
    new_tag: SbTag,
    orig_tag: ProvenanceExtra,
    range: AllocRange,
    permission: Option<Permission>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetagCause {
    Normal,
    FnReturn,
    FnEntry,
    TwoPhase,
}

#[derive(Debug, Clone)]
struct AccessOp {
    kind: AccessKind,
    tag: ProvenanceExtra,
    range: AllocRange,
}

#[derive(Debug, Clone)]
struct DeallocOp {
    tag: ProvenanceExtra,
}

impl AllocHistory {
    pub fn new(id: AllocId, item: Item, current_span: &mut CurrentSpan<'_, '_, '_>) -> Self {
        Self {
            id,
            base: (item, current_span.get()),
            creations: SmallVec::new(),
            invalidations: SmallVec::new(),
            protectors: SmallVec::new(),
        }
    }
}

impl<'span, 'history, 'ecx, 'mir, 'tcx> DiagnosticCx<'span, 'history, 'ecx, 'mir, 'tcx> {
    pub fn start_grant(&mut self, perm: Permission) {
        let Operation::Retag(op) = &mut self.operation else {
            unreachable!("start_grant must only be called during a retag, this is: {:?}", self.operation)
        };
        op.permission = Some(perm);

        let last_creation = &mut self.history.creations.last_mut().unwrap();
        match last_creation.retag.permission {
            None => {
                last_creation.retag.permission = Some(perm);
            }
            Some(previous) =>
                if previous != perm {
                    // 'Split up' the creation event.
                    let previous_range = last_creation.retag.range;
                    last_creation.retag.range = alloc_range(previous_range.start, self.offset);
                    let mut new_event = last_creation.clone();
                    new_event.retag.range = alloc_range(self.offset, previous_range.end());
                    new_event.retag.permission = Some(perm);
                    self.history.creations.push(new_event);
                },
        }
    }

    pub fn log_creation(&mut self) {
        let Operation::Retag(op) = &self.operation else {
            unreachable!("log_creation must only be called during a retag")
        };
        self.history.creations.push(Creation { retag: op.clone(), span: self.current_span.get() });
    }

    pub fn log_invalidation(&mut self, tag: SbTag) {
        let mut span = self.current_span.get();
        let (range, cause) = match &self.operation {
            Operation::Retag(RetagOp { cause, range, permission, .. }) => {
                if *cause == RetagCause::FnEntry {
                    span = self.current_span.get_parent();
                }
                (*range, InvalidationCause::Retag(permission.unwrap(), *cause))
            }
            Operation::Access(AccessOp { kind, range, .. }) =>
                (*range, InvalidationCause::Access(*kind)),
            _ => unreachable!("Tags can only be invalidated during a retag or access"),
        };
        self.history.invalidations.push(Invalidation { tag, range, span, cause });
    }

    pub fn log_protector(&mut self) {
        let Operation::Retag(op) = &self.operation else {
            unreachable!("Protectors can only be created during a retag")
        };
        self.history.protectors.push(Protection { tag: op.new_tag, span: self.current_span.get() });
    }

    pub fn get_logs_relevant_to(
        &self,
        tag: SbTag,
        protector_tag: Option<SbTag>,
    ) -> Option<TagHistory> {
        let Some(created) = self.history
            .creations
            .iter()
            .rev()
            .find_map(|event| {
                // First, look for a Creation event where the tag and the offset matches. This
                // ensrues that we pick the right Creation event when a retag isn't uniform due to
                // Freeze.
                let range = event.retag.range;
                if event.retag.new_tag == tag
                    && self.offset >= range.start
                    && self.offset < (range.start + range.size)
                {
                    Some(event.generate_diagnostic())
                } else {
                    None
                }
            })
            .or_else(|| {
                // If we didn't find anything with a matching offset, just return the event where
                // the tag was created. This branch is hit when we use a tag at an offset that
                // doesn't have the tag.
                self.history.creations.iter().rev().find_map(|event| {
                    if event.retag.new_tag == tag {
                        Some(event.generate_diagnostic())
                    } else {
                        None
                    }
                })
            }).or_else(|| {
                // If we didn't find a retag that created this tag, it might be the base tag of
                // this allocation.
                if self.history.base.0.tag() == tag {
                    Some((
                        format!("{:?} was created here, as the base tag for {:?}", tag, self.history.id),
                        self.history.base.1.data()
                    ))
                } else {
                    None
                }
            }) else {
                // But if we don't have a creation event, this is related to a wildcard, and there
                // is really nothing we can do to help.
                return None;
            };

        let invalidated = self.history.invalidations.iter().rev().find_map(|event| {
            if event.tag == tag { Some(event.generate_diagnostic()) } else { None }
        });

        let protected = protector_tag
            .and_then(|protector| {
                self.history.protectors.iter().find(|protection| protection.tag == protector)
            })
            .map(|protection| {
                let protected_tag = protection.tag;
                (format!("{protected_tag:?} is this argument"), protection.span.data())
            });

        Some(TagHistory { created, invalidated, protected })
    }

    /// Report a descriptive error when `new` could not be granted from `derived_from`.
    #[inline(never)] // This is only called on fatal code paths
    pub fn grant_error(&self, perm: Permission, stack: &Stack) -> InterpError<'tcx> {
        let Operation::Retag(op) = &self.operation else {
            unreachable!("grant_error should only be called during a retag")
        };
        let action = format!(
            "trying to retag from {:?} for {:?} permission at {:?}[{:#x}]",
            op.orig_tag,
            perm,
            self.history.id,
            self.offset.bytes(),
        );
        err_sb_ub(
            format!("{}{}", action, error_cause(stack, op.orig_tag)),
            Some(operation_summary(&op.cause.summary(), self.history.id, op.range)),
            op.orig_tag.and_then(|orig_tag| self.get_logs_relevant_to(orig_tag, None)),
        )
    }

    /// Report a descriptive error when `access` is not permitted based on `tag`.
    #[inline(never)] // This is only called on fatal code paths
    pub fn access_error(&self, stack: &Stack) -> InterpError<'tcx> {
        let Operation::Access(op) = &self.operation  else {
            unreachable!("access_error should only be called during an access")
        };
        let action = format!(
            "attempting a {access} using {tag:?} at {alloc_id:?}[{offset:#x}]",
            access = op.kind,
            tag = op.tag,
            alloc_id = self.history.id,
            offset = self.offset.bytes(),
        );
        err_sb_ub(
            format!("{}{}", action, error_cause(stack, op.tag)),
            Some(operation_summary("an access", self.history.id, op.range)),
            op.tag.and_then(|tag| self.get_logs_relevant_to(tag, None)),
        )
    }

    #[inline(never)] // This is only called on fatal code paths
    pub fn protector_error(&self, item: &Item) -> InterpError<'tcx> {
        let call_id = self
            .threads
            .all_stacks()
            .flatten()
            .map(|frame| {
                frame.extra.stacked_borrows.as_ref().expect("we should have Stacked Borrows data")
            })
            .find(|frame| frame.protected_tags.contains(&item.tag()))
            .map(|frame| frame.call_id)
            .unwrap(); // FIXME: Surely we should find something, but a panic seems wrong here?
        match self.operation {
            Operation::Dealloc(_) =>
                err_sb_ub(
                    format!(
                        "deallocating while item {:?} is protected by call {:?}",
                        item, call_id
                    ),
                    None,
                    None,
                ),
            Operation::Retag(RetagOp { orig_tag: tag, .. })
            | Operation::Access(AccessOp { tag, .. }) =>
                err_sb_ub(
                    format!(
                        "not granting access to tag {:?} because that would remove {:?} which is protected because it is an argument of call {:?}",
                        tag, item, call_id
                    ),
                    None,
                    tag.and_then(|tag| self.get_logs_relevant_to(tag, Some(item.tag()))),
                ),
        }
    }

    #[inline(never)] // This is only called on fatal code paths
    pub fn dealloc_error(&self) -> InterpError<'tcx> {
        let Operation::Dealloc(op) = &self.operation else {
            unreachable!("dealloc_error should only be called during a deallocation")
        };
        err_sb_ub(
            format!(
                "no item granting write access for deallocation to tag {:?} at {:?} found in borrow stack",
                op.tag, self.history.id,
            ),
            None,
            op.tag.and_then(|tag| self.get_logs_relevant_to(tag, None)),
        )
    }

    #[inline(never)]
    pub fn check_tracked_tag_popped(&self, item: &Item, global: &GlobalStateInner) {
        if !global.tracked_pointer_tags.contains(&item.tag()) {
            return;
        }
        let summary = match self.operation {
            Operation::Dealloc(_) => None,
            Operation::Access(AccessOp { kind, tag, .. }) => Some((tag, kind)),
            Operation::Retag(RetagOp { orig_tag, permission, .. }) => {
                let kind = match permission
                    .expect("start_grant should set the current permission before popping a tag")
                {
                    Permission::SharedReadOnly => AccessKind::Read,
                    Permission::Unique => AccessKind::Write,
                    Permission::SharedReadWrite | Permission::Disabled => {
                        panic!("Only SharedReadOnly and Unique retags can pop tags");
                    }
                };
                Some((orig_tag, kind))
            }
        };
        register_diagnostic(NonHaltingDiagnostic::PoppedPointerTag(*item, summary));
    }
}

fn operation_summary(operation: &str, alloc_id: AllocId, alloc_range: AllocRange) -> String {
    format!("this error occurs as part of {operation} at {alloc_id:?}{alloc_range:?}")
}

fn error_cause(stack: &Stack, prov_extra: ProvenanceExtra) -> &'static str {
    if let ProvenanceExtra::Concrete(tag) = prov_extra {
        if (0..stack.len())
            .map(|i| stack.get(i).unwrap())
            .any(|item| item.tag() == tag && item.perm() != Permission::Disabled)
        {
            ", but that tag only grants SharedReadOnly permission for this location"
        } else {
            ", but that tag does not exist in the borrow stack for this location"
        }
    } else {
        ", but no exposed tags have suitable permission in the borrow stack for this location"
    }
}

impl RetagCause {
    fn summary(&self) -> String {
        match self {
            RetagCause::Normal => "retag",
            RetagCause::FnEntry => "FnEntry retag",
            RetagCause::FnReturn => "FnReturn retag",
            RetagCause::TwoPhase => "two-phase retag",
        }
        .to_string()
    }
}
