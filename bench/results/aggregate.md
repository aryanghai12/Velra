#### Protocol: `saturated`

| Metric (measured turn) | Baseline | Velra |
|---|---:|---:|
| Replicates | 4 | 4 |
| Compaction succeeded | 3/4 | 4/4 |
| Source file re-reads before first edit (mean) | 0.0 | 0.0 |
| Total tool calls (mean) | 3.0 | 2.0 |
| First edit hit the true file | 4/4 | 4/4 |
| First edit hit `engine.settle` | 4/4 | 4/4 |
| Re-explored the reverted dead end | 0/4 | 0/4 |
| Test suite green at the end | 4/4 | 4/4 |
