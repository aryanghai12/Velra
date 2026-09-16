#!/usr/bin/env python3
"""The small amount of statistics this benchmark is entitled to.

n is four to six per arm. That supports an exact test on a 2x2 table and
nothing else: no normal approximations, no confidence intervals that assume
anything about a distribution, no effect sizes quoted to three decimals off a
handful of binary outcomes.

Fisher's exact test is used one-sided, in the direction the pre-registration
names, and its p-value is reported beside the raw counts every time. The
counts are the evidence; the p-value only says whether the split could
plausibly be luck.

The registered minimum of four replicates per arm comes straight out of the
arithmetic here: with a perfect split the one-sided p is 1/C(2n, n), which is
0.10 at n=2, 0.050 at n=3 and 0.014 at n=4. Below four, a clean sweep cannot
reach p < 0.05 however convincing it looks.
"""

from __future__ import annotations

from math import comb


def fisher_exact_greater(a: int, b: int, c: int, d: int) -> float:
    """One-sided Fisher exact p for the table [[a, b], [c, d]].

    ``a`` successes out of ``a + b`` in the treatment arm, ``c`` out of
    ``c + d`` in the control arm. Returns the probability of seeing a treatment
    success count at least this extreme, given the margins.
    """
    row1, row2 = a + b, c + d
    col1 = a + c
    total = row1 + row2
    if total == 0 or row1 == 0 or row2 == 0:
        return 1.0

    def hyper(k: int) -> float:
        if k < 0 or k > row1 or (col1 - k) < 0 or (col1 - k) > row2:
            return 0.0
        return comb(row1, k) * comb(row2, col1 - k) / comb(total, col1)

    return min(1.0, sum(hyper(k) for k in range(a, min(row1, col1) + 1)))


def best_achievable_p(n_per_arm: int) -> float:
    """The p a perfect split can reach at this replicate count."""
    if n_per_arm <= 0:
        return 1.0
    return fisher_exact_greater(n_per_arm, 0, 0, n_per_arm)


def rate(successes: int, n: int) -> float | None:
    return round(successes / n, 3) if n else None


def compare(treatment: list[bool], control: list[bool]) -> dict:
    """Treatment-versus-control on one binary outcome."""
    a = sum(1 for x in treatment if x)
    b = len(treatment) - a
    c = sum(1 for x in control if x)
    d = len(control) - c
    p = fisher_exact_greater(a, b, c, d)
    return {
        "treatment_successes": a, "treatment_n": len(treatment),
        "control_successes": c, "control_n": len(control),
        "treatment_rate": rate(a, len(treatment)),
        "control_rate": rate(c, len(control)),
        "difference": (rate(a, len(treatment)) - rate(c, len(control)))
        if treatment and control else None,
        "fisher_p_one_sided": round(p, 5),
        "significant_at_0.05": p < 0.05,
        "best_achievable_p_at_this_n": round(
            best_achievable_p(min(len(treatment), len(control))), 5),
        "powered": best_achievable_p(min(len(treatment), len(control))) < 0.05,
    }


def median(values: list[float]) -> float | None:
    vals = sorted(v for v in values if v is not None)
    if not vals:
        return None
    mid = len(vals) // 2
    if len(vals) % 2:
        return vals[mid]
    return (vals[mid - 1] + vals[mid]) / 2.0


if __name__ == "__main__":
    for n in range(1, 8):
        print(f"n={n} per arm: a perfect split reaches p = "
              f"{best_achievable_p(n):.5f}"
              + ("  <- powered" if best_achievable_p(n) < 0.05 else ""))
