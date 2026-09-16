"""Benchmark scenarios for the v0.1.1 efficacy experiment.

Each scenario is a self-contained experiment: a fixture generator, a turn
script with a compaction boundary, a set of behavioural checks scored from the
final repository state and the raw session stream, and a canary used by the
per-scenario information-loss control.

A scenario exists to test *one* mechanism the continuation capsule claims to
carry, and to be unanswerable without it:

  s1-dead-end-pair    [DEAD_ENDS]            two eliminated approaches
  s2-hidden-constraint [ROOT_TASK_OBJECTIVE] a constraint stated only in chat
  s3-dynamic-caller   [WORKING_FILES]        a relevant file grep cannot find

See ``registry.py`` for the lookup used by the harness.
"""
