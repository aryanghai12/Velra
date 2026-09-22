#!/usr/bin/env python3
"""Where one Token-Burn run writes, and what it may never write to.

    python bench/tokenburn/run.py --dry-run --results-root <PATH>
    python bench/tokenburn/run.py --live    --results-root <PATH> [--stage formal]

A *run root* holds every mutable artifact a run produces and nothing else::

    <root>/readiness.json     the gate's report for this run
    <root>/run_state.json     which trials ran, and their verdicts
    <root>/trials/<trial>/    one directory per trial (analysis.json included)
    <root>/aggregate.json     the aggregate, verdicts.json and report.md
    <root>/verdicts.json
    <root>/report.md
    <root>/settings-backup/   the user settings snapshot taken before --live
    <root>/quarantine/        trials a --force re-run moved aside
    <root>/.gitignore         raw captures stay out of git, as in the old tree

Relative paths resolve against the current working directory, like any other
command-line path, and are recorded resolved. Only ``<root>/trials`` is ever
read for aggregation, so trials that are not under the root -- the frozen
qualification evidence in particular -- are never discovered or mixed in.

Protected trees
---------------

``bench/results/v0.1.2`` has been frozen evidence since commit 7a09e65 (tag
``tokenburn-qualification-v0.1.2``), and the older result trees before it are
historical record. A run root may not be inside, or contain, any of them, and
no run-scoped write may land in one: the runner raises
:class:`ProtectedEvidenceError` naming the path instead. Without
``--results-root`` the paths keep their original layout -- readiness at
``bench/results/v0.1.2/readiness.json``, everything else under
``bench/results/v0.1.2/tokenburn/`` -- so every reader of that layout still
finds what it expects; but because that layout *is* the frozen evidence now,
a mode that would write there refuses and asks for a results root.
"""

from __future__ import annotations

import dataclasses
import os
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent

#: The frozen v0.1.2 qualification evidence (7a09e65).
FROZEN = BENCH / "results" / "v0.1.2"

#: Every tree no run may write to. The first is the qualification evidence;
#: the rest are the historical record `run.PRESERVED` already names.
PROTECTED: tuple[pathlib.Path, ...] = (
    FROZEN,
    BENCH / "results" / "trials",
    BENCH / "results" / "v0.1.1",
    BENCH / "results" / "v0.1.1_frozen_baseline",
    BENCH / "results" / "superseded",
    REPO_ROOT / "BENCHMARK_REPORT.md",
)

#: Raw captures, as the repository's own .gitignore treats them for the
#: frozen tree. Written into a fresh root so the same files stay untracked.
GITIGNORE = """\
# Written by bench/tokenburn/runroot.py. Raw session captures are regenerated
# by every run and are not evidence; derived measurements are.
trials/*/stream.jsonl
trials/*/source_stream.jsonl
trials/*/transcript.jsonl
trials/*/velra_home/
trials/*/memory_quarantine-*/
quarantine/*/*/stream.jsonl
quarantine/*/*/source_stream.jsonl
quarantine/*/*/transcript.jsonl
quarantine/*/*/velra_home/
"""


class ProtectedEvidenceError(RuntimeError):
    """A run tried to write into frozen or historical benchmark evidence."""


def _norm(path: pathlib.Path) -> str:
    """Comparable form: absolute, resolved, case-folded where the OS is."""
    return os.path.normcase(str(pathlib.Path(path).resolve(strict=False)))


def _within(path: pathlib.Path, ancestor: pathlib.Path) -> bool:
    p, a = _norm(path), _norm(ancestor)
    return p == a or p.startswith(a.rstrip(os.sep) + os.sep)


def protected_hit(path: pathlib.Path) -> pathlib.Path | None:
    """The protected tree ``path`` lies in, if any."""
    for tree in PROTECTED:
        if _within(path, tree):
            return tree
    return None


def assert_writable(path: pathlib.Path, what: str) -> None:
    """Refuse a run-scoped write into protected evidence, naming both."""
    tree = protected_hit(path)
    if tree is not None:
        raise ProtectedEvidenceError(
            f"refusing to write {what} to {path}: it is inside "
            f"{tree}, which is protected benchmark evidence"
            + (" (frozen at 7a09e65, tag tokenburn-qualification-v0.1.2)"
               if tree == FROZEN else "")
            + ". Pass --results-root <PATH> to write a fresh run elsewhere.")


@dataclasses.dataclass(frozen=True)
class RunRoot:
    """Every path one run owns. Build with :func:`default` or :func:`select`."""

    root: pathlib.Path
    readiness: pathlib.Path
    explicit: bool
    #: What the user typed, before resolution (None for the default).
    given: str | None = None

    @property
    def trials(self) -> pathlib.Path:
        return self.root / "trials"

    @property
    def quarantine(self) -> pathlib.Path:
        return self.root / "quarantine"

    @property
    def run_state(self) -> pathlib.Path:
        return self.root / "run_state.json"

    @property
    def settings_backup(self) -> pathlib.Path:
        return self.root / "settings-backup"

    @property
    def aggregate_dir(self) -> pathlib.Path:
        return self.root

    def writable(self) -> bool:
        return protected_hit(self.root) is None and \
            protected_hit(self.readiness) is None

    def assert_writable(self) -> None:
        """Called before a mode writes anything under this root."""
        assert_writable(self.root, "run output")
        assert_writable(self.readiness, "readiness.json")

    def repo_relative(self) -> str | None:
        """The root as a repo-relative POSIX path, when it is inside the repo."""
        if not _within(self.root, REPO_ROOT):
            return None
        rel = os.path.relpath(self.root.resolve(strict=False),
                              REPO_ROOT.resolve(strict=False))
        return rel.replace(os.sep, "/").rstrip("/") + "/"

    def prepare(self) -> None:
        """Create the root and its .gitignore. Refuses protected locations."""
        self.assert_writable()
        self.root.mkdir(parents=True, exist_ok=True)
        ignore = self.root / ".gitignore"
        if not ignore.exists():
            ignore.write_text(GITIGNORE, encoding="utf-8", newline="")

    def to_json(self) -> dict:
        return {
            "root": str(self.root),
            "given": self.given,
            "explicit": self.explicit,
            "readiness": str(self.readiness),
            "trials": str(self.trials),
            "run_state": str(self.run_state),
            "aggregate_dir": str(self.aggregate_dir),
            "settings_backup": str(self.settings_backup),
            "quarantine": str(self.quarantine),
            "writable": self.writable(),
        }


def default() -> RunRoot:
    """The original layout. Read-compatible; writes are refused (frozen)."""
    return RunRoot(root=FROZEN / "tokenburn", readiness=FROZEN / "readiness.json",
                   explicit=False)


def select(value: str | os.PathLike | None,
           cwd: pathlib.Path | None = None) -> RunRoot:
    """The run root for ``--results-root VALUE`` (the default when None).

    Relative values resolve against ``cwd`` (the process working directory
    unless given). A root inside a protected tree, or one that contains a
    protected tree -- ``bench/results`` itself, or the repository root, whose
    ``trials/`` would be historical trials -- is refused here, before any
    phase runs.
    """
    if value is None:
        return default()
    raw = pathlib.Path(os.path.expanduser(os.fspath(value)))
    base = pathlib.Path(cwd) if cwd is not None else pathlib.Path.cwd()
    root = (raw if raw.is_absolute() else base / raw).resolve(strict=False)
    tree = protected_hit(root)
    if tree is not None:
        raise ProtectedEvidenceError(
            f"--results-root {value!r} resolves to {root}, inside protected "
            f"evidence {tree}. Choose a location outside it.")
    for protected in PROTECTED:
        if _within(protected, root):
            raise ProtectedEvidenceError(
                f"--results-root {value!r} resolves to {root}, which contains "
                f"protected evidence {protected}; a run root must not, or its "
                f"trials/ could be historical trials. Choose a new directory.")
    return RunRoot(root=root, readiness=root / "readiness.json",
                   explicit=True, given=os.fspath(value))
