# Velra v0.1.2 — Token-Burn benchmark

Pre-registration `velra-tokenburn` v1.0.0, sha256 `775f18f4775997e7`.

Trials: 8. Matched pairs: 4.

## Verdicts

| pair_id | benchmark | verdict | basis | burden_change_pct | baseline_correct | velra_correct | why |
|---|---|---|---|---|---|---|---|
| A_cold_continuation#q1 | A_cold_continuation | INCONCLUSIVE | CAPTURE_FAILURE | -65.9% | False | True | velra arm: the ledger did not hold every piece of the declared operational state: this is a CAPTURE failure, not a delivery one |
| A_cold_continuation#q2 | A_cold_continuation | INCONCLUSIVE | CAPTURE_FAILURE | -46.5% | True | False | velra arm: the ledger did not hold every piece of the declared operational state: this is a CAPTURE failure, not a delivery one |
| B_clear_survival#q1 | B_clear_survival | INCONCLUSIVE | CAPTURE_FAILURE | +48.8% | True | True | velra arm: the ledger did not hold every piece of the declared operational state: this is a CAPTURE failure, not a delivery one |
| B_clear_survival#q2 | B_clear_survival | INCONCLUSIVE | CAPTURE_FAILURE | -16.8% | True | True | velra arm: the ledger did not hold every piece of the declared operational state: this is a CAPTURE failure, not a delivery one |


## Per-trial rows

| scenario | pair_id | arm | source_session | destination_session | target_context_size | achieved_context_size | cache_condition | input_tokens | cache_read_input_tokens | cache_creation_input_tokens | output_tokens | capsule_tokens | turns | tool_calls | file_reads | first_correct_action | final_correctness | causal_validity | telemetry_source | measurement_status | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A_cold_continuation | A_cold_continuation#q1 | baseline | 980ff0b9-e35d-40bb-ac40-2dec3b278b97 | e37bf69b-5966-4dd3-a7cf-2bcfaf1d4640 | 250000 | unavailable | HIT | 30 | 665245 | 25500 | 12219 | unavailable | 1 | 23 | 11 | 6 | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q1 | velra | 35e68d86-242c-4c25-9561-42a31e57b4d0 | d74ede8c-386d-4009-8ad7-8f58005d0e66 | 250000 | unavailable | HIT | 12 | 219540 | 16119 | 6364 | ~700 | 1 | 5 | 0 | not reached | True | INCOMPLETE | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q2 | baseline | bd31b52d-3ecd-4680-9ee4-c05b6e0e80ae | 49a3683f-dc75-40ff-84e9-1b5d23ce9b32 | 250000 | unavailable | HIT | 26 | 578825 | 29298 | 9451 | unavailable | 1 | 16 | 3 | 5 | True | DEFERRED | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q2 | velra | a44a233f-1ec8-41bb-a074-484de18676d4 | 8b33e6bb-3d5a-41cb-824c-815ea8637b16 | 250000 | unavailable | HIT | 16 | 307906 | 17242 | 6007 | ~710 | 1 | 10 | 4 | not reached | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q1 | baseline | 60ced4f1-392c-45fb-9fa4-62b0b0815f3f | 2ebfe8ee-0a21-41a9-be2b-c399b9e688cc | 250000 | unavailable | HIT | 10 | 170207 | 11895 | 2220 | unavailable | 1 | 5 | 3 | 2 | True | DEFERRED | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q1 | velra | 28ef6dde-e0eb-493c-8ae5-53b3bf7d651b | 708e401f-ebec-4549-bc1d-bad86fe2093c | 250000 | unavailable | HIT | 14 | 256202 | 14762 | 4090 | ~704 | 1 | 9 | 4 | 3 | True | INCOMPLETE | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q2 | baseline | 62972290-62bf-41b1-bb03-dc56fe8d8042 | 80ebe12b-2600-4fa6-a53d-a4e88c28a855 | 250000 | unavailable | HIT | 14 | 258594 | 16344 | 4997 | unavailable | 1 | 13 | 9 | 2 | True | DEFERRED | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q2 | velra | 9f5dbcee-6afc-43d6-8b41-fb8c993068b9 | 73fbdc0a-8944-4fc9-b379-4bbfb95ccf1e | 250000 | unavailable | HIT | 12 | 215649 | 13154 | 2592 | ~667 | 1 | 8 | 3 | 2 | True | INCOMPLETE | structured_usage | measured | (pair-level) |


## Pooled

### A_cold_continuation

* pairs: 2, decided: 0
* verdicts: {'VELRA_WIN': 0, 'BASELINE_WIN': 0, 'TIE': 0, 'INCONCLUSIVE': 2}
* failure classes: {'CAPTURE_FAILURE': 2}
* correct: baseline 1/2, velra 1/2
* total input change: **not measurable** on both arms in any pair of this group

### B_clear_survival

* pairs: 2, decided: 0
* verdicts: {'VELRA_WIN': 0, 'BASELINE_WIN': 0, 'TIE': 0, 'INCONCLUSIVE': 2}
* failure classes: {'CAPTURE_FAILURE': 2}
* correct: baseline 2/2, velra 2/2
* total input change: **not measurable** on both arms in any pair of this group

## How to read the cells

* `unavailable` — the structured data did not carry the field. It is not zero, and it was not recovered from terminal output.
* `inconclusive` — the value depends on something that was not measured.
* `~n` — a proxy. The provenance table in each trial's `analysis.json` says what it stands in for.
* `achieved_context_size` is what the runtime reported. A synthetic fixture's size appears only under `synthetic_context_size` and is never an observation of Claude.
* `cache_condition: UNKNOWN` means the telemetry did not say. It does not mean the cache expired.
