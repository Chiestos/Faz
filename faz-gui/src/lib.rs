// Shared library target for faz-gui: exposes non-GUI modules to the `k` CLI binary.
// GUI-only modules (overlay, termwindow, renderstate, etc.) are not included here.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::assign_op_pattern)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::needless_return)]
#![allow(clippy::redundant_closure)]

#[doc(hidden)]

pub mod thread_util;
