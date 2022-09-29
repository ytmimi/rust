use clippy_utils::diagnostics::{span_lint, span_lint_and_sugg};
use clippy_utils::source::snippet;
use clippy_utils::{is_trait_method, meets_msrv, msrvs};
use rustc_errors::Applicability;
use rustc_hir as hir;
use rustc_lint::LateContext;
use rustc_semver::RustcVersion;
use rustc_span::sym;

use super::FILTER_MAP_NEXT;

pub(super) fn check<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx hir::Expr<'_>,
    recv: &'tcx hir::Expr<'_>,
    arg: &'tcx hir::Expr<'_>,
    msrv: Option<RustcVersion>,
) {
    if is_trait_method(cx, expr, sym::Iterator) {
        if !meets_msrv(msrv, msrvs::ITERATOR_FIND_MAP) {
            return;
        }

        let msg = "called `filter_map(..).next()` on an `Iterator`. This is more succinctly expressed by calling \
                   `.find_map(..)` instead";
        let filter_snippet = snippet(cx, arg.span, "..");
        if filter_snippet.lines().count() <= 1 {
            let iter_snippet = snippet(cx, recv.span, "..");
            span_lint_and_sugg(
                cx,
                FILTER_MAP_NEXT,
                expr.span,
                msg,
                "try this",
                format!("{}.find_map({})", iter_snippet, filter_snippet),
                Applicability::MachineApplicable,
            );
        } else {
            span_lint(cx, FILTER_MAP_NEXT, expr.span, msg);
        }
    }
}
